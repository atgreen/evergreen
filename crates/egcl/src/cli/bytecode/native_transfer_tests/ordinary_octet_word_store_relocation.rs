// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exact bounded-loop entry/exit polls must publish every live tagged root.
//! PRE and POST use separate young inputs: a promoted object cannot move twice.

use super::*;
use egcl_compiler::t2::x64_calls::NativeCallOrigin;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    Pre,
    Post,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Collect,
    Interrupt,
}

thread_local! {
    static ARGUMENTS: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static BEFORE: Cell<[u64; 3]> = const { Cell::new([0; 3]) };
    static EXPECTED: Cell<[u8; 8]> = const { Cell::new([0; 8]) };
    static BOUNDARY: Cell<Boundary> = const { Cell::new(Boundary::Pre) };
    static ACTION: Cell<Action> = const { Cell::new(Action::Collect) };
    static OBSERVED: Cell<bool> = const { Cell::new(false) };
    static REQUEST: Cell<u64> = const { Cell::new(0) };
}

const START: usize = 2;
const SIZE: usize = 12;
const SENTINEL: u8 = 0xa5;

fn post_request() {
    egcl_rt::runtime::mark_process_signal_activity();
    REQUEST.with(|request| {
        request.set(
            unsafe { &*egcl_rt::runtime::process_signal_activity_address() }
                .load(std::sync::atomic::Ordering::Acquire),
        )
    });
}

fn arm_after_admission() {
    post_request();
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(Some(observe_boundary)));
    native_transfer_entry::root_publication::OBSERVE_AFTER.with(|hook| hook.set(Some(repost)));
}

fn repost() {
    if !OBSERVED.with(Cell::get) {
        post_request();
    }
}

fn check_bytes(array: EgclVal, complete: bool) {
    for index in 0..SIZE {
        let expected = if complete && (START..START + 8).contains(&index) {
            EXPECTED.with(Cell::get)[index - START]
        } else {
            SENTINEL
        };
        assert_eq!(
            egcl_rt::packed_u8::get(array, index),
            Some(expected),
            "the bounded kernel must preserve prefix/suffix and write all eight bytes"
        );
    }
}

fn observe_boundary() {
    let origin = native_transfer_entry::root_publication::native_poll_origin_for_test();
    let wanted = matches!(
        (BOUNDARY.with(Cell::get), origin),
        (Boundary::Pre, Some(NativeCallOrigin::BoundedLoopEntry(_)))
            | (Boundary::Post, Some(NativeCallOrigin::BoundedLoopExit(_)))
    );
    if !wanted {
        return;
    }
    assert!(
        !OBSERVED.with(Cell::get),
        "visit exactly one selected boundary"
    );
    assert!(
        egcl_rt::runtime::current_seen_signal_activity()
            .is_none_or(|seen| seen < REQUEST.with(Cell::get)),
        "the request must reach the generated boundary before runtime drain"
    );
    let pointer = ARGUMENTS.with(Cell::get);
    assert!(!pointer.is_null());
    // The pointer addresses a rooted Vec's stable allocation. These values
    // are only borrowed until the collection, then read again from the roots.
    let before = BEFORE.with(Cell::get);
    assert_eq!(unsafe { (*pointer).to_raw() }, before[0]);
    assert_eq!(unsafe { (*pointer.add(2)).to_raw() }, before[1]);
    assert_eq!(unsafe { (*pointer.add(3)).to_raw() }, before[2]);
    for index in 0..4 {
        assert_eq!(
            egcl_stdlib::sequences::svref(unsafe { *pointer.add(3) }, index).unwrap(),
            NIL,
            "the carrier must stay untouched until after the completed exit poll"
        );
    }
    check_bytes(
        unsafe { *pointer },
        BOUNDARY.with(Cell::get) == Boundary::Post,
    );
    match ACTION.with(Cell::get) {
        Action::Collect => {
            let heap_integer = unsafe { (*pointer.add(2)).is_heap_object() };
            egcl_rt::collect_t0_minor().unwrap();
            assert_ne!(
                unsafe { (*pointer).to_raw() },
                before[0],
                "the young array must really relocate at the selected boundary"
            );
            if heap_integer {
                assert_ne!(
                    unsafe { (*pointer.add(2)).to_raw() },
                    before[1],
                    "the original one-limb integer is live through both polls"
                );
            }
            assert_ne!(
                unsafe { (*pointer.add(3)).to_raw() },
                before[2],
                "the preallocated general carrier must really relocate too"
            );
            check_bytes(
                unsafe { *pointer },
                BOUNDARY.with(Cell::get) == Boundary::Post,
            );
        }
        Action::Interrupt => native_transfer_entry::request_interrupt_for_test(),
    }
    OBSERVED.with(|observed| observed.set(true));
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    native_transfer_entry::root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_octet_word_store_polls_relocate_roots_and_deliver_interrupts() {
    const CHILD: &str = "EGCL_TEST_OCTET_WORD_ROOTS_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::octet_word_store_relocation::ordinary_octet_word_store_polls_relocate_roots_and_deliver_interrupts";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_DISABLE_OCTET_WORD_STORES")
            .output()
            .expect("run isolated octet word store root fixture");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child failed: {}\n{stdout}\n{stderr}",
            output.status
        );
        assert!(
            stdout.contains("OCTET-WORD-ROOTS-OK"),
            "fixture did not run: {stdout}\n{stderr}"
        );
        assert!(
            stderr.contains("OCTET-WORD-ROOTS: octet word stores: 1"),
            "the guarded word-store fusion must actually be emitted: {stderr}"
        );
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // The minimal Env skips boot.lisp. Install its actual byte-specifier
    // definitions so opportunistic bytecode compilation can establish these
    // canonical operators as functions, exactly as in a booted runtime.
    crate::cli::read_eval_all_env(
        "(defun byte (size position) (cons size position))
         (defun byte-size (bytespec) (car bytespec))
         (defun byte-position (bytespec) (cdr bytespec))
         (defun ldb (bytespec integer)
           (logand (ash integer (- (byte-position bytespec)))
                   (1- (ash 1 (byte-size bytespec)))))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "OCTET-WORD-ROOTS",
        "(array start value carrier)",
        "((declare (type fixnum start) (type integer value) (type simple-vector carrier))
          (let ((end (+ start 8)) (index start) (lane value))
            (tagbody loop
              (if (>= index end) (go done))
              (setf (aref array index) (ldb (byte 8 0) lane))
              (setq lane (ash lane -8))
              (setq index (1+ index))
              (go loop) done)
            (setf (svref carrier 0) array)
            (setf (svref carrier 1) lane)
            (setf (svref carrier 2) index)
            (setf (svref carrier 3) value)
            array))",
        &mut env,
    );
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "fixture must enter the ordinary native ABI"
    );
    struct ClearHooks;
    impl Drop for ClearHooks {
        fn drop(&mut self) {
            native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
            native_transfer_entry::root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
            ARGUMENTS.with(|pointer| pointer.set(std::ptr::null()));
        }
    }
    for boundary in [Boundary::Pre, Boundary::Post] {
        for action in [Action::Collect, Action::Interrupt] {
            for (sign, magnitude, boxed) in [
                (1, 4097u64, false),
                (-1, 4097, false),
                (1, 0xfedc_ba98_7654_3210, true),
                (-1, 0xfedc_ba98_7654_3210, true),
            ] {
                egcl_rt::collect_t0_minor().unwrap();
                egcl_rt::rooted!(
                    carrier = egcl_stdlib::sequences::build_filled_simple_vector(4, NIL)
                );
                egcl_rt::rooted!(array = egcl_rt::packed_u8::make(SIZE, SENTINEL).unwrap());
                egcl_rt::rooted!(
                    integer = if boxed {
                        egcl_rt::bignum::alloc_bignum_cli(sign, &[magnitude])
                    } else {
                        EgclVal::from_fixnum(i64::from(sign) * magnitude as i64)
                    }
                );
                egcl_rt::rooted!(
                    args = vec![
                        *array,
                        EgclVal::from_fixnum(START as i64),
                        *integer,
                        *carrier
                    ]
                );
                let bits = if sign < 0 {
                    magnitude.wrapping_neg()
                } else {
                    magnitude
                };
                EXPECTED.with(|expected| expected.set(bits.to_le_bytes()));
                BEFORE.with(|before| {
                    before.set([args[0].to_raw(), args[2].to_raw(), args[3].to_raw()])
                });
                ARGUMENTS.with(|pointer| pointer.set(args.as_ptr()));
                BOUNDARY.with(|which| which.set(boundary));
                ACTION.with(|which| which.set(action));
                OBSERVED.with(|observed| observed.set(false));
                let _clear = ClearHooks;
                let deopts = mapped.deopt_count();
                native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
                native_transfer_entry::ordinary_calls::take_named_entries_for_test();
                native_transfer_entry::reset_poll_paths_for_test();
                let outcome = native_transfer_entry::with_ordinary_entry_hook_for_test(
                    arm_after_admission,
                    || run_native(&code, symbol, &args, &mut env),
                );
                assert!(
                    OBSERVED.with(Cell::get),
                    "{boundary:?}/{action:?} must reach its exact generated poll"
                );
                assert_eq!(mapped.deopt_count(), deopts);
                assert_eq!(
                    native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
                    0
                );
                assert_eq!(
                    native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
                    0,
                    "the kernel and carrier stores must stay native"
                );
                assert!(native_transfer_entry::take_poll_paths_for_test().1 > 0);
                if action == Action::Interrupt {
                    let condition = match &outcome {
                        Err(EgclError::Signalled(details)) => details.condition,
                        _ => panic!(
                            "{boundary:?}/{action:?} sign={sign} magnitude={magnitude:#x} boxed={boxed}: expected signalled interrupt; outcome={outcome:?}"
                        ),
                    };
                    egcl_rt::rooted!(condition = condition);
                    let diagnostic = format!(
                        "{boundary:?}/{action:?} sign={sign} magnitude={magnitude:#x} boxed={boxed}: outcome={outcome:?}"
                    );
                    let hierarchy = crate::cli::condition_type_hierarchy_names(*condition)
                        .expect("the signalled interrupt must be a condition instance");
                    assert_eq!(
                        hierarchy.first().map(String::as_str),
                        Some("INTERRUPT-CONDITION"),
                        "{diagnostic}: unexpected condition class"
                    );
                    check_bytes(args[0], boundary == Boundary::Post);
                    for index in 0..4 {
                        assert_eq!(
                            egcl_stdlib::sequences::svref(args[3], index).unwrap(),
                            NIL,
                            "interrupt delivery must not replay or execute the later carrier stores"
                        );
                    }
                } else {
                    egcl_rt::rooted!(result = outcome.unwrap());
                    assert_eq!(
                        *result, args[0],
                        "the primary return must reload the moved array"
                    );
                    egcl_rt::rooted!(
                        returned_array = egcl_stdlib::sequences::svref(args[3], 0).unwrap()
                    );
                    egcl_rt::rooted!(
                        returned_integer = egcl_stdlib::sequences::svref(args[3], 3).unwrap()
                    );
                    assert_eq!(*returned_array, args[0]);
                    assert_eq!(
                        egcl_stdlib::sequences::svref(args[3], 1).unwrap(),
                        EgclVal::from_fixnum(if sign < 0 { -1 } else { 0 })
                    );
                    assert_eq!(
                        egcl_stdlib::sequences::svref(args[3], 2).unwrap(),
                        EgclVal::from_fixnum((START + 8) as i64)
                    );
                    assert_eq!(
                        *returned_integer, args[2],
                        "the original moved integer must remain live"
                    );
                    check_bytes(*result, true);
                }
            }
        }
    }
    println!("OCTET-WORD-ROOTS-OK");
}
