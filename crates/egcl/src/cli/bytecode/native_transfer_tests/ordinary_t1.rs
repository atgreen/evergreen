// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t1_installs_published_calls_without_success_return_poll() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun ordinary-t1-leaf (x) (%ordinary-t2-entry-for-test) (values x 72))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((ordinary-t1-leaf x))")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("ORDINARY-T1-PUBLISHED-CALL");
    let body = Arc::new(
        compile_function(
            "ORDINARY-T1-PUBLISHED-CALL",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    let code = try_promote_to_t1(symbol).expect("ordinary T1 installation");
    let bytes = unsafe { std::slice::from_raw_parts(code.entry, code.code_len) };
    let pending = c2i_transfer_pending as extern "C" fn() -> u64 as usize as u64;
    let decoder =
        iced_x86::Decoder::with_ip(64, bytes, code.entry as u64, iced_x86::DecoderOptions::NONE);
    for instruction in decoder {
        assert!(
            !(0..instruction.op_count()).any(|operand| instruction.op_kind(operand)
                == iced_x86::OpKind::Immediate64
                && instruction.immediate64() == pending),
            "ordinary T1 still loads the successful-return transfer-poll helper at {:#x}",
            instruction.ip()
        );
    }
    assert!(
        matches!(code._storage, NativeCodeStorage::Mapped(_)),
        "ordinary T1 must install the mapped representation"
    );
    assert!(!code.is_t2, "baseline emission must retain its T1 tier");
    assert_eq!(code.transfer_abi_version, MAPPED_TRANSFER_ABI_VERSION);
    assert!(Arc::ptr_eq(code.body.as_ref().unwrap(), &body));
    super::ordinary_t2::expect_entry(&code);
    assert_eq!(
        run_native(&code, symbol, &[EgclVal::from_fixnum(71)], &mut env).unwrap(),
        EgclVal::from_fixnum(71)
    );
    assert_eq!(
        env.mv.as_slice(),
        &[EgclVal::from_fixnum(71), EgclVal::from_fixnum(72)]
    );
    assert!(
        super::ordinary_t2::observed_entry(),
        "installed mapped T1 caller must execute"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t1_keeps_arithmetic_guards_and_mapped_recursion() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let forms = "((ordinary-t1-tick)
        (if (= n 0) (+ x 1) (+ (ordinary-t1-recursive (- n 1) x) 1)))";
    super::super::super::read_eval_all_env(
        &format!(
            "(setq *ordinary-t1-ticks* 0)
           (defun ordinary-t1-tick () (setq *ordinary-t1-ticks* (+ *ordinary-t1-ticks* 1)))
           (defun ordinary-t1-recursive (n x) (progn . {forms}))"
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let symbol = egcl_rt::symbols::intern("ORDINARY-T1-RECURSIVE");
    let body = Arc::new(
        compile_function("ORDINARY-T1-RECURSIVE", *params, *forms, &env, false, false).unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let code = native_for_dispatch(symbol, Some(*function), t1_threshold(), &body)
        .expect("normal dispatch must publish T1");
    assert!(matches!(code._storage, NativeCodeStorage::Mapped(_)));
    assert!(!code.is_t2);
    assert_eq!(egcl_rt::function::tier(*function), 1);
    assert!(
        code.has_deopt,
        "mapped T1 must retain speculative arithmetic guards"
    );
    assert!(
        code.t2_metadata.is_some(),
        "guards need exact retained reconstruction metadata"
    );
    for (x, expected) in [(10, "15"), ((1_i64 << 60) - 1, "1152921504606846980")] {
        super::super::super::read_eval_all_env("(setq *ordinary-t1-ticks* 0)", &mut env).unwrap();
        native_transfer_entry::take_recursive_entries();
        let stack = egcl_rt::current_stack();
        let before = (
            stack.fp(),
            stack.sp(),
            NATIVE_DEPTH.with(|depth| depth.get()),
        );
        egcl_rt::rooted!(
            result = run_native(
                &code,
                symbol,
                &[EgclVal::from_fixnum(4), EgclVal::from_fixnum(x)],
                &mut env
            )
            .unwrap()
        );
        assert_eq!(super::super::super::format_val(*result), expected);
        assert_eq!(
            native_transfer_entry::take_recursive_entries(),
            4,
            "recursive calls must execute mapped frames in the same segment"
        );
        assert_eq!(
            (
                stack.fp(),
                stack.sp(),
                NATIVE_DEPTH.with(|depth| depth.get())
            ),
            before
        );
        assert_eq!(
            super::super::super::read_eval_all_env("*ordinary-t1-ticks*", &mut env).unwrap(),
            EgclVal::from_fixnum(5),
            "guard resumption must not replay earlier effects"
        );
    }
}
