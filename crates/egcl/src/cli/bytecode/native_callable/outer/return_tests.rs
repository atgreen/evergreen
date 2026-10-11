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
            &format!("(defun {name} () (%ordinary-t2-entry-for-test) {values})"),
            &mut env,
        )
        .unwrap();
        let (_, code) = ordinary_t2::install(
            name,
            "()",
            &format!("((%ordinary-t2-entry-for-test) {values})"),
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
