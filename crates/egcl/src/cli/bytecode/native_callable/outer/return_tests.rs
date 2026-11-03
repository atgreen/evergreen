// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{heap_test_lock, read_eval_all_env};

thread_local! {
    static MAPPED_BODY_OBSERVED: Cell<bool> = const { Cell::new(false) };
}

fn observe_mapped_body() {
    MAPPED_BODY_OBSERVED.with(|observed| observed.set(true));
}

struct RestoreBodyObserver(Option<fn()>);
impl Drop for RestoreBodyObserver {
    fn drop(&mut self) {
        crate::cli::bytecode::native_transfer_tests::ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|observer| observer.set(self.0));
    }
}

#[repr(C)]
#[derive(Default, Debug)]
struct Returned {
    primary: u64,
    count: usize,
    payload: *const EgclVal,
    carry: u8,
}

#[repr(C)]
struct Probe {
    invocation: Invocation,
    returned: Returned,
}

// Same registered outer-call geometry as `enter`, with a flag-preserving
// observation immediately after the actual published adapter returns. The
// adapter may allocate: its callable/arguments are rooted by `invoke_probe`.
#[unsafe(naked)]
unsafe extern "C" fn enter_probe() {
    core::arch::naked_asm!(
        "endbr64",
        "mov [rdi + {request}], rdx",
        "mov [rdi + {segment}], rdx",
        "lea rax, [rip + 2f]",
        "mov [rdi + {return_pc}], rax",
        "sub rsp, 8",
        "mov [rsp], rdi",
        "mov [rdi + {call_sp}], rsp",
        "mov r11, rdi",
        "mov rcx, rdi",
        "mov rsi, [r11 + {nargs}]",
        "mov rdx, [r11 + {args}]",
        "mov rdi, [r11 + {function}]",
        "call qword ptr [r11 + {entry}]",
        "2:",
        "mov r11, [rsp]",
        "setc byte ptr [r11 + {carry}]",
        "mov [r11 + {primary}], rax",
        "mov [r11 + {count}], rdx",
        "mov [r11 + {payload}], rcx",
        "add rsp, 8",
        "ret",
        request = const std::mem::offset_of!(NativeCallContext, request),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
        args = const std::mem::offset_of!(NativeCallContext, args),
        function = const std::mem::offset_of!(Invocation, function),
        entry = const std::mem::offset_of!(Invocation, entry),
        segment = const std::mem::offset_of!(Invocation, segment),
        call_sp = const std::mem::offset_of!(Invocation, call_sp),
        return_pc = const std::mem::offset_of!(Invocation, return_pc),
        primary = const std::mem::offset_of!(Probe, returned),
        count = const std::mem::offset_of!(Probe, returned) + std::mem::offset_of!(Returned, count),
        payload = const std::mem::offset_of!(Probe, returned) + std::mem::offset_of!(Returned, payload),
        carry = const std::mem::offset_of!(Probe, returned) + std::mem::offset_of!(Returned, carry),
    );
}

fn invoke_probe(function: EgclVal, env: &mut Env) -> Returned {
    egcl_rt::rooted!(function = function);
    let entries = entries_for(*function).expect("published callable entries");
    let mut probe = Probe {
        invocation: Invocation {
            context: NativeCallContext {
                request: std::ptr::null_mut(),
                capture: egcl_rt::native_transfer::leave_native_segment as *const u8,
                args: std::ptr::null_mut(),
                nargs: 0,
            },
            function: std::ptr::from_mut(&mut *function),
            entry: entries.slice,
            segment: std::ptr::null_mut(),
            call_sp: 0,
            return_pc: 0,
            caller_segment: current_segment() as usize,
            caller_capture: native_transfer_entry::child_capture_for_test(),
            previous_invocation: ACTIVE.with(Cell::get),
        },
        returned: Returned::default(),
    };
    let _active = ActiveInvocation(ACTIVE.with(|active| active.replace(&mut probe.invocation)));
    let primary = unsafe {
        native_transfer_entry::invoke_outer_callable(
            enter_probe as *const u8,
            std::ptr::from_mut(&mut probe).cast(),
            env,
        )
    }
    .expect("successful published call");
    assert_eq!(probe.returned.primary, primary.0);
    probe.returned
}

fn assert_tuple(returned: Returned, env: &Env, expected: &[i64]) {
    let primary = expected
        .first()
        .copied()
        .map(EgclVal::from_fixnum)
        .unwrap_or(NIL);
    assert_eq!(returned.primary, primary.0, "{returned:?}");
    assert_eq!(
        returned.carry,
        u8::from(expected.len() != 1),
        "{returned:?}"
    );
    if expected.len() == 1 {
        assert!(!env.mv_active);
        assert!(
            env.mv.is_empty(),
            "single return retains no duplicate tuple roots"
        );
    } else {
        assert_eq!(returned.count, expected.len());
        assert!(env.mv_active);
        assert_eq!(
            env.mv,
            expected
                .iter()
                .copied()
                .map(EgclVal::from_fixnum)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            returned.payload,
            if expected.is_empty() {
                std::ptr::null()
            } else {
                env.mv.as_ptr()
            }
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_return_registers_after_real_mapped_t2_execution() {
    use crate::cli::bytecode::native_transfer_tests::ordinary_t2;
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // The observation hook is intentionally a tree-walker-only test form, not
    // a compiler builtin. Reach it through a real named function, as the other
    // mapped-entry probes do, so the compiled body retains an admissible call.
    read_eval_all_env(
        "(defun return-abi-observe-mapped () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    for (name, values, expected) in [
        ("RETURN-ABI-MAPPED-ZERO", "(values)", vec![]),
        ("RETURN-ABI-MAPPED-ONE", "(values 42)", vec![42]),
        (
            "RETURN-ABI-MAPPED-MANY",
            "(values 42 43 44)",
            vec![42, 43, 44],
        ),
    ] {
        read_eval_all_env(
            &format!("(defun {name} () (return-abi-observe-mapped) {values})"),
            &mut env,
        )
        .unwrap();
        let (_, code) = ordinary_t2::install(
            name,
            "()",
            &format!("((return-abi-observe-mapped) {values})"),
            &mut env,
        );
        assert!(code.is_t2 && matches!(code._storage, NativeCodeStorage::Mapped(_)));
        egcl_rt::rooted!(function = read_eval_all_env(&format!("#'{name}"), &mut env).unwrap());
        for stale in [vec![], vec![70], vec![70, 71, 72]] {
            env.set_mv(stale.into_iter().map(EgclVal::from_fixnum).collect());
            ordinary_t2::expect_entry(&code);
            MAPPED_BODY_OBSERVED.with(|observed| observed.set(false));
            let _observer = RestoreBodyObserver(
                ordinary_t2::RECURSIVE_LEAF_OBSERVER
                    .with(|observer| observer.replace(Some(observe_mapped_body))),
            );
            let returned = invoke_probe(*function, &mut env);
            assert!(
                MAPPED_BODY_OBSERVED.with(Cell::get),
                "the installed mapped body must actually execute"
            );
            assert_tuple(returned, &env, &expected);
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_return_registers_after_callable_fallback_and_unwind() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defmethod return-abi-generic () (values 42 43))", &mut env).unwrap();
    for (source, expected) in [
        ("(lambda () (values))", vec![]),
        ("(lambda () (values 42))", vec![42]),
        ("(lambda () (values 42 43 44))", vec![42, 43, 44]),
        ("#'return-abi-generic", vec![42, 43]),
        ("(let ((x 42)) (lambda () (values x 43)))", vec![42, 43]),
        (
            "(lambda () (catch 'return-abi-exit (unwind-protect (throw 'return-abi-exit (values 42 43)) (values 90 91 92))))",
            vec![42, 43],
        ),
    ] {
        egcl_rt::rooted!(function = read_eval_all_env(source, &mut env).unwrap());
        env.set_mv(vec![EgclVal::from_fixnum(70), EgclVal::from_fixnum(71)]);
        ENTRY_COUNTS.with(|counts| counts.set((0, 0)));
        let returned = invoke_probe(*function, &mut env);
        assert!(
            ENTRY_COUNTS.with(|counts| counts.get().0) > 0,
            "published adapter must execute: {source}"
        );
        assert_tuple(returned, &env, &expected);
    }
}

thread_local! {
    static OWNED_RECORD_PROBE_RAN: Cell<bool> = const { Cell::new(false) };
    static OWNED_RECORD_PARENT: Cell<usize> = const { Cell::new(0) };
}

/// Mint the same verified outer record geometry as invoke_probe, but retain
/// the enclosing mapped CAPTURE deliberately. Normal public outer entry clears
/// CAPTURE already; this adversarial fixture makes the provenance gate itself
/// necessary instead of passing merely because no parent is visible.
fn invoke_named_outer_probe(symbol: u32, args: &[EgclVal]) -> Returned {
    use std::sync::atomic::Ordering;
    egcl_rt::rooted!(args = args.to_vec());
    let cell = crate::cli::bytecode::call_table::resolve(symbol).unwrap();
    let mut probe = Probe {
        invocation: Invocation {
            context: NativeCallContext {
                request: std::ptr::null_mut(),
                capture: egcl_rt::native_transfer::leave_native_segment as *const u8,
                args: args.as_mut_ptr(),
                nargs: args.len(),
            },
            // enter_probe transports this opaque first ABI word. A named cell
            // entry consumes a CallCell address, not a callable-word pointer.
            // The Arc above retains it; neither Rust nor GC dereferences this
            // field as an EgclVal in the named-adapter fixture.
            function: Arc::as_ptr(&cell).cast_mut().cast(),
            entry: unsafe { &*cell.entry_address(true) }.load(Ordering::Acquire),
            segment: std::ptr::null_mut(),
            call_sp: 0,
            return_pc: 0,
            caller_segment: current_segment() as usize,
            caller_capture: native_transfer_entry::child_capture_for_test(),
            previous_invocation: ACTIVE.with(Cell::get),
        },
        returned: Returned::default(),
    };
    assert!(
        probe.invocation.caller_capture.is_some(),
        "a real mapped parent remains live"
    );
    let _active = ActiveInvocation(ACTIVE.with(|active| active.replace(&mut probe.invocation)));
    // The enclosing real mapped helper publication and rooted args remain
    // live throughout; only the test bypasses outer entry's CAPTURE clearing.
    let outcome = unsafe {
        egcl_rt::native_transfer::invoke_native_segment(
            enter_probe as *const u8,
            std::ptr::from_mut(&mut probe).cast(),
            egcl_rt::current_stack(),
        )
    }
    .expect("registered outer test segment");
    assert_eq!(outcome.exit, egcl_rt::native_transfer::NativeExit::Returned);
    assert_eq!(outcome.value.to_raw(), probe.returned.primary);
    probe.returned
}

thread_local! {
    static OWNED_CHILD_ANCHOR: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    static OWNED_CHILD_BODY: Cell<bool> = const { Cell::new(false) };
}
fn observe_owned_child_body() {
    let stack = egcl_rt::current_stack();
    // The marker itself is a T0 helper and owns one legitimate helper frame.
    // Inspect its immediate predecessor, the native child's real VM anchor.
    let expected = OWNED_CHILD_ANCHOR.with(Cell::get);
    if expected.0 == 0 {
        return;
    }
    native_transfer_entry::rust_ordinary_root::assert_anchor_for_test(expected.0);
    assert_ne!(stack.fp() as usize, 0);
    OWNED_CHILD_BODY.with(|slot| slot.set(true));
}

fn exercise_owned_records_below_mapped_parent(prior_entries: usize) {
    use crate::cli::bytecode::native_transfer_entry::{self as native, mapped_ordinary};
    let child = crate::cli::bytecode::symbol_index_of("OWNED-RECORD-CHILD").unwrap();
    let funcall = crate::cli::bytecode::symbol_index_of("FUNCALL").unwrap();
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(child).unwrap());
    assert!(
        prior_entries > 0,
        "the actual generated first call established an eligible reusable parent lane"
    );
    for symbol in [child, funcall] {
        egcl_rt::rooted!(
            args = if symbol == child {
                vec![EgclVal::from_fixnum(41)]
            } else {
                vec![*function, EgclVal::from_fixnum(41)]
            }
        );
        native::start_activation_counts_for_test(child);
        native::take_lean_root_entries_for_test();
        let stack = egcl_rt::current_stack();
        let before = (stack.fp() as usize, stack.sp() as usize);
        OWNED_CHILD_ANCHOR.with(|slot| slot.set(before));
        OWNED_CHILD_BODY.with(|slot| slot.set(false));
        native::rust_ordinary_root::take_entries_for_test();
        let returned = invoke_named_outer_probe(symbol, &args);
        assert!(
            OWNED_CHILD_BODY.with(Cell::get),
            "actual owner-checked child body executes"
        );
        assert_eq!((stack.fp() as usize, stack.sp() as usize), before);
        let enabled = native::rust_ordinary_root::enabled_for_test();
        assert_eq!(
            native::rust_ordinary_root::take_entries_for_test(),
            usize::from(enabled)
        );
        assert_eq!(returned.primary, EgclVal::from_fixnum(42).to_raw());
        assert_eq!(returned.carry, 0);
        assert_eq!(
            native::take_lean_root_entries_for_test(),
            usize::from(!enabled),
            "the independent Rust root replaces the old lean managed root only when enabled"
        );
        assert_eq!(
            native::take_activation_counts_for_test(),
            if enabled { (0, 0) } else { (1, 1) },
            "Rust-owned entry avoids callee preparation without borrowing a mapped lane"
        );
        assert_eq!(
            mapped_ordinary::take_entries_for_test(),
            0,
            "an older CAPTURE never grants this outer record generated ingress"
        );
    }
    OWNED_CHILD_ANCHOR.with(|slot| slot.set((0, 0)));
    OWNED_RECORD_PROBE_RAN.with(|slot| slot.set(true));
}

struct RestoreOwnedProfile(Option<fn()>);
impl Drop for RestoreOwnedProfile {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.0));
    }
}
fn observe_live_owned_record_parent() {
    use crate::cli::bytecode::native_transfer_entry::{self as native, mapped_ordinary};
    if OWNED_RECORD_PROBE_RAN.with(Cell::get) {
        return;
    }
    let parent = OWNED_RECORD_PARENT.with(Cell::get);
    if parent == 0 || native::child_capture_for_test() != Some((parent, false)) {
        return;
    }
    if native::root_publication::active_anchor_for_test().map(|(_, owner)| owner) != Some(parent) {
        return;
    }
    let entries = mapped_ordinary::take_entries_for_test();
    if entries == 0 {
        return;
    }
    // Run before entering the helper adapter that deliberately clears CAPTURE.
    // Avoid recursive observations while the adversarial records execute.
    let _restore = RestoreOwnedProfile(
        native::root_publication::PROFILE_OBSERVE.with(|slot| slot.replace(None)),
    );
    exercise_owned_records_below_mapped_parent(entries);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn owned_outer_records_cannot_borrow_an_older_published_mapped_parent_lane() {
    use crate::cli::bytecode::native_transfer_tests::ordinary_t2;
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defun owned-child-observe () (%ordinary-t2-entry-for-test))
         (defun owned-record-child (x) (owned-child-observe) (+ x 1))
         (defun owned-record-parent (f &key (seed (1+ 0)) &aux (unused 0))
           (funcall f 1) (funcall f 2))",
        &mut env,
    )
    .unwrap();
    let (child, child_code) = ordinary_t2::install(
        "OWNED-RECORD-CHILD",
        "(x)",
        "((owned-child-observe) (+ x 1))",
        &mut env,
    );

    let (parent, code) = ordinary_t2::install(
        "OWNED-RECORD-PARENT",
        "(f &key (seed (1+ 0)) &aux (unused 0))",
        "((funcall f 1) (funcall f 2))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("actual mapped parent");
    };
    assert!(mapped.ordinary_entry.is_none());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(child).unwrap());
    ordinary_t2::expect_entry(&child_code);
    // Warm both named slots before installing the observation hook.
    struct RestoreEnv(*mut Env);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            NATIVE_ENV.with(|slot| slot.set(self.0));
        }
    }
    let saved = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
    crate::cli::bytecode::call_table::ordinary_call(child, &[EgclVal::from_fixnum(1)]).unwrap();
    crate::cli::bytecode::call_table::ordinary_call(
        crate::cli::bytecode::symbol_index_of("FUNCALL").unwrap(),
        &[*function, EgclVal::from_fixnum(1)],
    )
    .unwrap();
    drop(saved);
    native_transfer_entry::mapped_ordinary::take_entries_for_test();
    let _pins = ordinary_t2::RecursiveFixturePins::new(&["OWNED-CHILD-OBSERVE"]);
    ordinary_t2::expect_entry(&child_code);
    let _body_observer = RestoreBodyObserver(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|slot| slot.replace(Some(observe_owned_child_body))),
    );
    OWNED_RECORD_PROBE_RAN.with(|slot| slot.set(false));
    OWNED_RECORD_PARENT.with(|slot| slot.set(Rc::as_ptr(mapped) as usize));
    struct ResetCounts;
    impl Drop for ResetCounts {
        fn drop(&mut self) {
            native_transfer_entry::take_activation_counts_for_test();
            OWNED_RECORD_PARENT.with(|slot| slot.set(0));
        }
    }
    let _counts = ResetCounts;
    let _observer = RestoreOwnedProfile(
        native_transfer_entry::root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(observe_live_owned_record_parent))),
    );
    assert_eq!(
        crate::cli::bytecode::run_native(&code, parent, &[*function], &mut env).unwrap(),
        EgclVal::from_fixnum(3)
    );
    assert!(OWNED_RECORD_PROBE_RAN.with(Cell::get));
}
