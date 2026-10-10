// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{heap_test_lock, read_eval_all_env};

fn require_native_windows() {
    assert!(
        enabled(),
        "set EGCL_NATIVE_TRANSFER=1 for the Win64 execution gate"
    );
    assert!(
        egcl_rt::native_transfer::is_supported(),
        "Win64 callable execution gate requires supported mitigation policy; refusal is not execution coverage"
    );
}

#[test]
#[ignore = "requires native Windows with supported mitigation policy"]
fn win64_published_callables_execute_both_entries_and_retire_segments() {
    require_native_windows();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defun win64-entry-fn (x) (+ x 1))
        (defmethod win64-entry-generic ((x t)) (+ x 2))",
        &mut env,
    )
    .unwrap();
    for (source, expected) in [
        ("#'win64-entry-fn", 11),
        ("#'win64-entry-generic", 12),
        ("(let ((n 3)) (lambda (x) (+ n x)))", 13),
        ("#'1+", 11),
    ] {
        egcl_rt::rooted!(function = read_eval_all_env(source, &mut env).unwrap());
        for registers in [false, true] {
            ENTRY_COUNTS.with(|n| n.set((0, 0)));
            assert_eq!(
                invoke_entry(*function, &[EgclVal::from_fixnum(10)], &mut env, registers).unwrap(),
                EgclVal::from_fixnum(expected)
            );
            assert!(
                ENTRY_COUNTS.with(|n| n.get().0) > 0,
                "published entry was bypassed"
            );
            assert!(egcl_rt::native_transfer::current_segment().is_null());
        }
    }
    egcl_rt::rooted!(list = read_eval_all_env("#'list", &mut env).unwrap());
    for count in 0..=5 {
        let args = vec![EgclVal::from_fixnum(7); count];
        egcl_rt::rooted!(result = invoke_entry(*list, &args, &mut env, count <= 3).unwrap());
        let mut tail = *result;
        for _ in 0..count {
            let (head, next) = cp(tail);
            assert_eq!(head, EgclVal::from_fixnum(7));
            tail = next;
        }
        assert_eq!(tail, NIL);
    }
}

#[test]
#[ignore = "requires native Windows with supported mitigation policy"]
fn win64_published_callable_preserves_error_reentry_and_multiple_values() {
    require_native_windows();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        function = read_eval_all_env(
            "(lambda () (let ((events nil))
          (let ((answer (multiple-value-list (catch 'win64-exit
            (unwind-protect
              (funcall (lambda () (throw 'win64-exit (values 7 8))))
              (setq events (cons :cleanup events)))))))
            (values answer events))))",
            &mut env
        )
        .unwrap()
    );
    ENTRY_COUNTS.with(|n| n.set((0, 0)));
    egcl_rt::rooted!(result = invoke(*function, &[], &mut env).unwrap());
    assert_eq!(crate::cli::format_val(*result), "(7 8)");
    assert_eq!(env.mv.len(), 2);
    assert_eq!(crate::cli::format_val(env.mv[1]), "(:CLEANUP)");
    assert!(
        ENTRY_COUNTS.with(|n| n.get().1) > 0,
        "callback did not enter a nested segment"
    );
    egcl_rt::rooted!(missing = read_eval_all_env("'win64-missing-function", &mut env).unwrap());
    egcl_rt::rooted!(error = invoke(*missing, &[], &mut env).unwrap_err());
    assert!(matches!(&*error, EgclError::Signalled(details)
        if crate::cli::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")));
    assert!(egcl_rt::native_transfer::current_segment().is_null());
    assert!(NATIVE_ENV.with(|slot| slot.get()).is_null());
}

#[test]
#[ignore = "requires native Windows with supported mitigation policy"]
fn win64_published_callable_reloads_relocated_roots() {
    require_native_windows();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(prototype = read_eval_all_env("#'list", &mut env).unwrap());
    let (head, identity) = cp(*prototype);
    egcl_rt::rooted!(function = crate::cli::arena_cons(head, identity));
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL));
    let before = (function.to_raw(), argument.to_raw());
    COLLECT_NEXT.with(|flag| flag.set(true));
    egcl_rt::rooted!(result = invoke(*function, &[*argument], &mut env).unwrap());
    assert_ne!(function.to_raw(), before.0, "callable did not relocate");
    assert_ne!(argument.to_raw(), before.1, "argument did not relocate");
    assert_eq!(cp(*result), (*argument, NIL));
}

// This metadata test intentionally does not require native-transfer capability:
// it asks the Windows unwinder to traverse synthetic frames without executing
// a segment. It is not evidence that the callable execution gate ran.
#[test]
fn win64_published_callable_unwind_metadata_covers_body_and_epilogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    for (address, frame_bytes, epilog_count) in [
        (enter_slice as *const () as u64, 40usize, 1),
        (enter_registers as *const () as u64, 56usize, 1),
        (slice as *const () as u64, 72usize, 2),
    ] {
        unsafe {
            let mut base = 0;
            let entry = RtlLookupFunctionEntry(address, &mut base, std::ptr::null_mut());
            assert!(
                !entry.is_null(),
                "callable shim is missing its SEH function table"
            );
            assert_eq!(base + u64::from((*entry).BeginAddress), address);
            let unwind = (base + u64::from((*entry).Anonymous.UnwindData)) as *const u8;
            let body_offset = usize::from(*unwind.add(1));
            let code = std::slice::from_raw_parts(
                address as *const u8,
                ((*entry).EndAddress - (*entry).BeginAddress) as usize,
            );
            assert_eq!(
                &code[..8],
                &[0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x83, 0xec, frame_bytes as u8]
            );
            assert_eq!(body_offset, 8);
            let epilogs: Vec<_> = code
                .windows(4)
                .enumerate()
                .filter_map(|(offset, bytes)| {
                    (bytes == [0x48, 0x83, 0xc4, frame_bytes as u8]).then_some(offset)
                })
                .collect();
            assert_eq!(epilogs.len(), epilog_count);
            // Probe the body and both sides of each stack adjustment. The
            // transfer epilog ends in JMP [r11], a ModRM mod=00 tail exit that
            // Windows recognizes; the ordinary epilog ends in RET.
            let mut points = vec![(body_offset, 0)];
            for offset in epilogs {
                assert!(
                    code[offset + 4] == 0xc3 || code[offset + 4..].starts_with(&[0x41, 0xff, 0x23])
                );
                points.extend([(offset, 0), (offset + 4, frame_bytes)]);
            }
            let mut aligned_stack = [0u128; 16];
            let stack = aligned_stack.as_mut_ptr().cast::<u64>();
            let return_pc = 0x1234_5678u64;
            *stack.add(frame_bytes / 8) = return_pc;
            let bottom = stack as u64;
            for (offset, adjustment) in points {
                let mut context: CONTEXT = std::mem::zeroed();
                context.Rip = address + offset as u64;
                context.Rsp = bottom + adjustment as u64;
                context.Rbx = 3;
                context.Rbp = 5;
                context.Rsi = 6;
                context.Rdi = 7;
                context.R12 = 12;
                context.R13 = 13;
                context.R14 = 14;
                context.R15 = 15;
                let xmm = &mut context.Anonymous.Anonymous.Xmm6 as *mut M128A;
                for n in 0..10 {
                    (*xmm.add(n)).Low = 600 + n as u64;
                    (*xmm.add(n)).High = 700 + n as i64;
                }
                let mut handler_data = std::ptr::null_mut();
                let mut establisher = 0;
                RtlVirtualUnwind(
                    0,
                    base,
                    context.Rip,
                    entry,
                    &mut context,
                    &mut handler_data,
                    &mut establisher,
                    std::ptr::null_mut(),
                );
                assert_eq!(
                    (context.Rip, context.Rsp),
                    (return_pc, bottom + frame_bytes as u64 + 8),
                    "shim {address:#x}, pc offset {offset}"
                );
                assert_eq!(
                    [
                        context.Rbx,
                        context.Rbp,
                        context.Rsi,
                        context.Rdi,
                        context.R12,
                        context.R13,
                        context.R14,
                        context.R15
                    ],
                    [3, 5, 6, 7, 12, 13, 14, 15]
                );
                let xmm = &context.Anonymous.Anonymous.Xmm6 as *const M128A;
                for n in 0..10 {
                    assert_eq!(
                        ((*xmm.add(n)).Low, (*xmm.add(n)).High),
                        (600 + n as u64, 700 + n as i64)
                    );
                }
            }
        }
    }
}
