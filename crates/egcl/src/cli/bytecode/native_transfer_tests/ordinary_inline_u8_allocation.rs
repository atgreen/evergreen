// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Observe allocation at the actual ordinary entry, not an interpreter wrapper.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowFault {
    None,
    Disabled,
    Exhausted,
    Stale,
}

thread_local! {
    static FAULT: Cell<WindowFault> = const { Cell::new(WindowFault::None) };
    static ENTRY_WINDOW: Cell<Option<[u64; 3]>> = const { Cell::new(None) };
    static ENTRY_OBSERVED: Cell<bool> = const { Cell::new(false) };
}

fn window_pointer() -> Option<*mut u64> {
    let layout = egcl_rt::gc::native_alloc_layout()?;
    let thread_pointer: usize;
    // SAFETY: the runtime exports this carrier's fixed native TLAB TLS offset.
    unsafe {
        std::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) thread_pointer,
            options(nostack, readonly, preserves_flags));
    }
    Some(thread_pointer.wrapping_add_signed(layout.tls_offset as isize) as *mut u64)
}

fn window_values() -> Option<[u64; 3]> {
    let window = window_pointer()?;
    // SAFETY: NativeTlab is repr(C) and exports the three u64 offsets.
    Some(unsafe { [window.read(), window.add(1).read(), window.add(2).read()] })
}

fn observe_entry() {
    ENTRY_OBSERVED.with(|observed| observed.set(true));
    native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test();
    if let Some(window) = window_pointer() {
        match FAULT.with(Cell::get) {
            WindowFault::None => {}
            WindowFault::Disabled => unsafe { window.add(1).write(0) },
            WindowFault::Exhausted => unsafe { window.add(1).write(window.read()) },
            WindowFault::Stale => unsafe {
                window.add(2).write(window.add(2).read().wrapping_sub(1));
            },
        }
    }
    ENTRY_WINDOW.with(|entry| entry.set(window_values()));
}

fn run_observed_result(
    code: &Rc<NativeCode>,
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
    fault: WindowFault,
) -> Result<EgclVal, EgclError> {
    // Publish a new runtime window, absorbing any preceding native allocations.
    // No allocating call intervenes between the entry hook and generated code.
    egcl_rt::rooted!(_prime = NIL);
    for _ in 0..32 {
        *_prime = crate::cli::arena_cons(EgclVal::from_fixnum(0), NIL);
        if window_values().is_none_or(|window| window[1].saturating_sub(window[0]) >= 512) {
            break;
        }
    }
    assert!(
        window_values().is_none_or(|window| window[1].saturating_sub(window[0]) >= 512),
        "fixture must reserve enough space before selecting a window fault"
    );
    FAULT.with(|selected| selected.set(fault));
    ENTRY_OBSERVED.with(|observed| observed.set(false));
    let result = native_transfer_entry::with_ordinary_entry_hook_for_test(observe_entry, || {
        run_native(code, symbol, args, env)
    });
    assert!(
        ENTRY_OBSERVED.with(Cell::get),
        "actual ordinary entry must run"
    );
    result
}

fn run_observed(
    code: &Rc<NativeCode>,
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
    fault: WindowFault,
) -> EgclVal {
    run_observed_result(code, symbol, args, env, fault).unwrap()
}

fn check_array(array: EgclVal, length: usize) {
    assert!(egcl_rt::packed_u8::is_vector(array));
    assert_eq!(egcl_rt::packed_u8::len(array), Some(length));
    for index in 0..length {
        assert_eq!(egcl_rt::packed_u8::get(array, index), Some(0));
    }
    assert_eq!(egcl_rt::packed_u8::get(array, length), None);
    let header = unsafe { &*array.as_ptr().cast::<egcl_rt::object::ObjectHeader>() };
    assert_eq!(header.type_id(), egcl_rt::object::type_id::SIMPLE_ARRAY);
    assert_eq!(
        header.hash() as usize,
        egcl_rt::packed_u8::DATA_OFFSET + length,
        "the GC must see the exact body length, not just aligned footprint"
    );
}

#[test]
fn ordinary_inline_u8_window_and_fallback_contract() {
    const CHILD: &str = "EGCL_TEST_INLINE_U8_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::inline_u8_allocation::ordinary_inline_u8_window_and_fallback_contract";
    let Ok(mode) = std::env::var(CHILD) else {
        for mode in ["on", "off", "global-off"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(CHILD, mode)
                .env_remove("EGCL_DISABLE_INLINE_U8_ALLOCATION")
                .env_remove("EGCL_INLINE_ALLOC")
                .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env_remove("EGCL_GC_VERIFY")
                .env("EGCL_GC_POISON", "1");
            if mode == "off" {
                command.env("EGCL_DISABLE_INLINE_U8_ALLOCATION", "1");
            }
            if mode == "global-off" {
                command.env("EGCL_INLINE_ALLOC", "0");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "mode={mode}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("INLINE-U8-WINDOW-CHECKS"));
        }
        return;
    };
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install(
        "INLINE-U8-WINDOW-TEST",
        "(n descriptor)",
        "((make-array n :element-type descriptor))",
        &mut env,
    );
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(mapped.ordinary_entry.is_some());
    let unsigned_byte = EgclVal::from_symbol_index(egcl_rt::symbols::intern("UNSIGNED-BYTE"));
    egcl_rt::rooted!(tail = crate::cli::arena_cons(EgclVal::from_fixnum(8), NIL));
    egcl_rt::rooted!(descriptor = crate::cli::arena_cons(unsigned_byte, *tail));
    egcl_rt::rooted!(args = vec![NIL, *descriptor]);
    for length in [0usize, 1, 7, 8, 136, 256, 257] {
        args[0] = EgclVal::from_fixnum(length as i64);
        egcl_rt::rooted!(array = run_observed(&code, symbol, &args, &mut env, WindowFault::None));
        let entries =
            native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test();
        let fast = mode == "on" && length <= 256;
        assert_eq!(entries, usize::from(!fast), "mode={mode} length={length}");
        check_array(*array, length);
        if fast {
            let before = ENTRY_WINDOW.with(Cell::get).unwrap();
            let after = window_values().unwrap();
            let footprint = (8 + egcl_rt::packed_u8::DATA_OFFSET + length + 15) & !15usize;
            assert_eq!(after[0] - before[0], footprint as u64);
            assert_eq!(unsafe { array.as_ptr() } as usize, before[0] as usize);
            assert_eq!(
                before[1..],
                after[1..],
                "fast allocation only advances the cursor"
            );
        }
    }
    if mode != "global-off" {
        for fault in [
            WindowFault::Disabled,
            WindowFault::Exhausted,
            WindowFault::Stale,
        ] {
            args[0] = EgclVal::from_fixnum(8);
            egcl_rt::rooted!(array = run_observed(&code, symbol, &args, &mut env, fault));
            assert_eq!(
                native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
                1,
                "{fault:?} must take the original collecting adapter"
            );
            check_array(*array, 8);
        }
    }
    // A proper singleton dimension list uses the same guarded fast allocator.
    egcl_rt::rooted!(dimension = crate::cli::arena_cons(EgclVal::from_fixnum(3), NIL));
    args[0] = *dimension;
    egcl_rt::rooted!(array = run_observed(&code, symbol, &args, &mut env, WindowFault::None));
    assert_eq!(
        native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
        usize::from(mode != "on")
    );
    check_array(*array, 3);
    if mode == "on" {
        assert_eq!(
            unsafe { array.as_ptr() } as u64,
            ENTRY_WINDOW.with(Cell::get).unwrap()[0]
        );
    }
    // Aliasing the literal cons does not make its element-type contents fixed.
    unsafe {
        tail.as_ptr()
            .cast::<EgclVal>()
            .write(EgclVal::from_fixnum(16))
    };
    args[0] = EgclVal::from_fixnum(3);
    {
        // This deliberately minimal Env has no boot MAKE-ARRAY function.
        // Unsupported types must reach that public fallback and preserve its
        // missing-function error; full public type semantics run in CLI tests.
        let error = run_observed_result(&code, symbol, &args, &mut env, WindowFault::None)
            .expect_err("width mutation must leave the packed-byte allocator");
        assert!(format!("{error:?}").contains("FUNCTION"));
        assert_eq!(
            native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
            1
        );
    }
    unsafe {
        tail.as_ptr()
            .cast::<EgclVal>()
            .write(EgclVal::from_fixnum(8))
    };
    egcl_rt::rooted!(array = run_observed(&code, symbol, &args, &mut env, WindowFault::None));
    check_array(*array, 3);
    assert_eq!(
        native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
        usize::from(mode != "on")
    );
    let (list_symbol, list_code) = install(
        "INLINE-U8-LIST-PRODUCER",
        "(n descriptor)",
        "((make-array (list n) :element-type descriptor))",
        &mut env,
    );
    assert_mapped_t2(&list_code);
    args[0] = EgclVal::from_fixnum(136);
    egcl_rt::rooted!(
        list_array = run_observed(&list_code, list_symbol, &args, &mut env, WindowFault::None)
    );
    check_array(*list_array, 136);
    assert_eq!(
        native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
        usize::from(mode != "on")
    );
    assert_eq!(
        mapped.deopt_count(),
        0,
        "fallback must not replay through deoptimization"
    );
    println!("INLINE-U8-WINDOW-CHECKS {mode}");
}

thread_local! {
    static GC_ARGUMENTS: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static GC_OBSERVED: Cell<bool> = const { Cell::new(false) };
    static GC_EXPECT_FAST: Cell<bool> = const { Cell::new(false) };
    static GC_ENTRY_MARKER: Cell<u64> = const { Cell::new(0) };
}

fn observe_gc_entry() {
    observe_entry();
    let arguments = GC_ARGUMENTS.with(Cell::get);
    assert!(!arguments.is_null());
    GC_ENTRY_MARKER.with(|marker| marker.set(unsafe { (*arguments.add(3)).to_raw() }));
    if std::env::var_os("EGCL_GC_STRESS").is_some() {
        egcl_rt::gc::stress_arm_after_init();
    }
}

fn collect_after_inline_result_is_published() {
    if GC_OBSERVED.with(Cell::get) {
        return;
    }
    let arguments = GC_ARGUMENTS.with(Cell::get);
    assert!(!arguments.is_null());
    let carrier = unsafe { *arguments.add(2) };
    let array = egcl_stdlib::sequences::svref(carrier, 0).unwrap();
    if !egcl_rt::packed_u8::is_vector(array) {
        return;
    }
    let array_before = array.to_raw();
    let marker_before = GC_ENTRY_MARKER.with(Cell::get);
    if GC_EXPECT_FAST.with(Cell::get) {
        assert_eq!(
            unsafe { array.as_ptr() } as u64,
            ENTRY_WINDOW.with(Cell::get).unwrap()[0],
            "the live result must originate from the native window"
        );
        assert_eq!(
            native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
            0,
            "the native allocation itself must bypass the adapter"
        );
    }
    GC_OBSERVED.with(|observed| observed.set(true));
    // The carrier and caller's homes own the array. No Rust-local root is
    // added for it; after moving, read through authoritative roots again.
    egcl_rt::collect_t0_minor().unwrap();
    let carrier = unsafe { *arguments.add(2) };
    let moved = egcl_stdlib::sequences::svref(carrier, 0).unwrap();
    assert_ne!(
        moved.to_raw(),
        array_before,
        "the allocated result must actually move"
    );
    assert_ne!(
        unsafe { (*arguments.add(3)).to_raw() },
        marker_before,
        "the live caller argument must actually move too"
    );
}

#[test]
fn ordinary_inline_u8_result_and_live_argument_survive_moving_gc() {
    const CHILD: &str = "EGCL_TEST_INLINE_U8_MOVING_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::inline_u8_allocation::ordinary_inline_u8_result_and_live_argument_survive_moving_gc";
    let Ok(mode) = std::env::var(CHILD) else {
        for mode in [
            "normal8",
            "normal-list136",
            "stress8",
            "stress-list8",
            "stress136",
            "stress-list136",
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(CHILD, mode)
                .env_remove("EGCL_DISABLE_INLINE_U8_ALLOCATION")
                .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
                .env_remove("EGCL_INLINE_ALLOC")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env_remove("EGCL_GC_STRESS_SKIP")
                .env("EGCL_GC_POISON", "1");
            if mode.starts_with("stress") {
                command
                    .env("EGCL_GC_STRESS", "1")
                    .env("EGCL_GC_STRESS_AFTER_INIT", "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{mode}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("INLINE-U8-RESULT-MOVED"));
        }
        return;
    };
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun inline-u8-force-collector () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "INLINE-U8-LIVE-RESULT-TEST",
        "(n descriptor record marker)",
        "((let ((array (make-array n :element-type descriptor)))
            (setf (svref record 0) array)
            (inline-u8-force-collector)
            (cons array marker)))",
        &mut env,
    );
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(mapped.ordinary_entry.is_some());
    let unsigned_byte = EgclVal::from_symbol_index(egcl_rt::symbols::intern("UNSIGNED-BYTE"));
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(tail = crate::cli::arena_cons(EgclVal::from_fixnum(8), NIL));
    egcl_rt::rooted!(descriptor = crate::cli::arena_cons(unsigned_byte, *tail));
    egcl_rt::rooted!(record = egcl_stdlib::sequences::build_simple_vector(&[NIL]));
    egcl_rt::rooted!(marker = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    let length = if mode.ends_with("136") { 136 } else { 8 };
    egcl_rt::rooted!(dimensions = crate::cli::arena_cons(EgclVal::from_fixnum(length), NIL));
    egcl_rt::rooted!(
        args = vec![
            if mode.contains("list") {
                *dimensions
            } else {
                EgclVal::from_fixnum(length)
            },
            *descriptor,
            *record,
            *marker
        ]
    );
    egcl_rt::rooted!(_prime = crate::cli::arena_cons(EgclVal::from_fixnum(0), NIL));
    FAULT.with(|fault| fault.set(WindowFault::None));
    GC_ARGUMENTS.with(|arguments| arguments.set(args.as_ptr()));
    GC_OBSERVED.with(|observed| observed.set(false));
    ENTRY_OBSERVED.with(|observed| observed.set(false));
    GC_EXPECT_FAST.with(|fast| fast.set(!mode.starts_with("stress") || length == 8));
    struct Restore(Option<fn()>);
    impl Drop for Restore {
        fn drop(&mut self) {
            RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
            GC_ARGUMENTS.with(|arguments| arguments.set(std::ptr::null()));
        }
    }
    let _restore = Restore(
        RECURSIVE_LEAF_OBSERVER
            .with(|hook| hook.replace(Some(collect_after_inline_result_is_published))),
    );
    expect_entry(&code);
    egcl_rt::rooted!(
        result = native_transfer_entry::with_ordinary_entry_hook_for_test(observe_gc_entry, || {
            run_native(&code, symbol, &args, &mut env)
        })
        .unwrap()
    );
    assert!(
        ENTRY_OBSERVED.with(Cell::get),
        "actual ordinary entry hook must run"
    );
    assert!(
        GC_OBSERVED.with(Cell::get),
        "collect at an actual published native call"
    );
    let pair = crate::cli::cp(*result);
    egcl_rt::rooted!(array = pair.0);
    egcl_rt::rooted!(returned_marker = pair.1);
    check_array(*array, length as usize);
    assert_eq!(
        *array,
        egcl_stdlib::sequences::svref(args[2], 0).unwrap(),
        "the caller's array home must reload its relocated value"
    );
    assert_eq!(
        *returned_marker, args[3],
        "the caller's marker home must reload too"
    );
    assert_eq!(crate::cli::cp(*returned_marker).0, EgclVal::from_fixnum(71));
    assert_eq!(
        native_transfer_entry::ordinary_primitives::take_array_runtime_entries_for_test(),
        usize::from(mode.starts_with("stress") && length == 136)
    );
    assert_eq!(mapped.deopt_count(), 0);
    println!("INLINE-U8-RESULT-MOVED {mode}");
}
