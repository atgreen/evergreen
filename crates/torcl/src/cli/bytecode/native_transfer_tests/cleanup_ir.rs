use super::*;
use torcl_compiler::t2::{
    build,
    ir::{AuxData, Opcode},
    verify,
};

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_normal_cleanup_executes_and_preserves_all_values() {
    use super::super::native_transfer_entry::TransferCode;
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(torcl_rt::native_transfer::is_supported());
    // Even a function with no Lisp calls needs a call-capable native frame:
    // these two empty cleanup bodies still save and restore continuations.
    {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            forms = reader::read_from_string("((unwind-protect x (unwind-protect nil nil)))")
                .unwrap()
                .0
        );
        let body = Arc::new(
            compile_function("EMPTY-NATIVE-CLEANUP", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("cleanup-only native frame");
        torcl_rt::rooted!(args = vec![super::super::super::arena_str("empty cleanup answer")]);
        assert_eq!(code.run(&args, &mut env).unwrap(), args[0]);
        assert!(!env.mv_active);
    }
    for result_form in ["x", "(values)", "(values x)", "(values x (list x))"] {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *normal-cleanup-log* nil)
                 (defun normal-protected-value (x) {result_form})
                 (defun normal-cleanup-effect (name x)
                   (%force-minor-gc-for-test)
                   (setq *normal-cleanup-log*
                     (cons (cons name x) *normal-cleanup-log*))
                   (values :discard (list :discard)))"
            ),
            &mut env,
        )
        .unwrap();
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            forms = reader::read_from_string(
                "((unwind-protect
                    (unwind-protect (normal-protected-value x)
                      (normal-cleanup-effect :inner x))
                    (normal-cleanup-effect :outer x)))"
            )
            .unwrap()
            .0
        );
        let body = Arc::new(
            compile_function("NORMAL-NATIVE-CLEANUP", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("normal cleanup must execute natively");
        torcl_rt::rooted!(args = vec![super::super::super::arena_str("kept answer")]);
        let before = args[0].to_raw();
        let frame = torcl_rt::current_stack().fp();
        torcl_rt::rooted!(primary = code.run(&args, &mut env).unwrap());
        assert_ne!(
            args[0].to_raw(),
            before,
            "GC during cleanup relocated the saved answer"
        );
        assert_eq!(torcl_rt::current_stack().fp(), frame);
        if result_form == "(values)" {
            assert_eq!(*primary, NIL);
            assert!(env.mv_active && env.mv.is_empty());
        } else {
            assert_eq!(*primary, args[0]);
            if result_form == "x" {
                assert!(!env.mv_active);
            } else {
                assert!(env.mv_active);
                assert_eq!(env.mv[0], args[0]);
                assert_eq!(
                    env.mv.len(),
                    if result_form.contains("list") { 2 } else { 1 }
                );
                if env.mv.len() == 2 {
                    assert_eq!(super::super::super::cp(env.mv[1]).0, args[0]);
                }
            }
        }
        torcl_rt::rooted!(
            log = super::super::super::read_eval_all_env("*normal-cleanup-log*", &mut env).unwrap()
        );
        torcl_rt::rooted!(entries = super::super::super::list_to_vec(*log));
        assert_eq!(entries.len(), 2, "each cleanup runs exactly once");
        for (index, name) in [":OUTER", ":INNER"].into_iter().enumerate() {
            let expected = reader::read_from_string(name).unwrap().0;
            let (key, value) = super::super::super::cp(entries[index]);
            assert_eq!(key, expected);
            assert_eq!(value, args[0]);
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_transfer_from_running_cleanup_discards_saved_answers_once() {
    use super::super::native_transfer_entry::TransferCode;
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(torcl_rt::native_transfer::is_supported());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *cleanup-replacements* 0 *cleanup-outer-count* 0)
         (defun saved-cleanup-value (x) (values x (list :old)))
         (defun replacing-cleanup (x)
           (%force-minor-gc-for-test)
           (setq *cleanup-replacements* (+ *cleanup-replacements* 1))
           (throw :cleanup-replacement (values x (list :new))))
         (defun surviving-outer-cleanup (x)
           (setq *cleanup-outer-count* (+ *cleanup-outer-count* 1))
           (list x))",
        &mut env,
    )
    .unwrap();
    torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    torcl_rt::rooted!(
        forms = reader::read_from_string(
            "((unwind-protect
                (unwind-protect (saved-cleanup-value x)
                  (unwind-protect (saved-cleanup-value x)
                    (replacing-cleanup x)))
                (surviving-outer-cleanup x)))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function(
            "REPLACING-NATIVE-CLEANUP",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    let code = TransferCode::compile(body).expect("running native cleanup can transfer");
    let token = super::super::super::next_control_token("CLEANUP-REPLACEMENT");
    let tag = reader::read_from_string(":cleanup-replacement").unwrap().0;
    env.catch_stack
        .push((super::super::super::val_as_str(tag), token.clone()));
    torcl_rt::rooted!(args = vec![super::super::super::arena_str("replacement answer")]);
    let before = args[0].to_raw();
    let frame = torcl_rt::current_stack().fp();
    torcl_rt::rooted!(result = code.run(&args, &mut env));
    assert!(
        matches!(&*result, Err(TorclError::Internal(t)) if t == &token),
        "{:?}",
        &*result
    );
    assert_ne!(
        args[0].to_raw(),
        before,
        "GC inside cleanup moved the answer"
    );
    assert_eq!(torcl_rt::current_stack().fp(), frame);
    assert_eq!(
        super::super::super::take_control_mv(&token, &mut env),
        args[0]
    );
    assert!(env.mv_active && env.mv.len() == 2);
    let expected = reader::read_from_string(":new").unwrap().0;
    assert_eq!(super::super::super::cp(env.mv[1]).0, expected);
    for counter in ["*cleanup-replacements*", "*cleanup-outer-count*"] {
        assert_eq!(
            super::super::super::read_eval_all_env(counter, &mut env).unwrap(),
            TorclVal::from_fixnum(1)
        );
    }
    env.catch_stack.pop();
}

#[test]
fn native_v2_normal_cleanup_has_explicit_saved_values_and_checked_continuations() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    for source in [
        "((unwind-protect (values x (list x)) (list x)))",
        "((unwind-protect (unwind-protect (values x) (list x)) (list x)))",
        "((unwind-protect x (unwind-protect (values x) (list x))))",
        "((if x (unwind-protect (list x) (list x)) (unwind-protect (values) (list x))))",
    ] {
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("NORMAL-CLEANUP", *params, *forms, &env, false, false).unwrap(),
        );
        let _body = ActiveBytecodeRoot::new(&body);
        assert!(build::build_from_bytecode(&body).is_err());
        let f = build::build_from_bytecode_for_transfers(&body).expect("normal cleanup SSA");
        verify::verify(&f).unwrap();
        let saves: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| f.inst(i).opcode == Opcode::CleanupSave)
            .collect();
        let restores: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| f.inst(i).opcode == Opcode::CleanupRestore)
            .collect();
        assert!(!saves.is_empty());
        assert_eq!(saves.len(), restores.len());
        let (emitted, _) = torcl_compiler::t2::emit::emit_framed_transfers_with_cleanup(
            &f,
            1,
            body.num_slots(),
            Some((1, 1)),
        )
        .expect("cleanup helpers enable explicit native emission");
        let calls = f
            .block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .filter(|&&i| f.inst(i).opcode == Opcode::Invoke)
            .count();
        assert_eq!(
            emitted.root_sync_sites.len(),
            calls + saves.len() + restores.len(),
            "cleanup helpers need the same call-frame and shadow-root discipline as other calls"
        );
        use torcl_compiler::t2::pass::{Analyses, Pass};
        let mut optimized = f.clone();
        torcl_compiler::t2::opt_dce::Dce.run(&mut optimized, &mut Analyses::new());
        verify::verify(&optimized).unwrap();
        for &inst in saves.iter().chain(&restores) {
            assert!(
                optimized
                    .block_order()
                    .iter()
                    .any(|&b| optimized.block(b).insts.contains(&inst)),
                "cleanup value custody is effectful even when primary is unused"
            );
        }
        let mut bad = f.clone();
        bad.inst_mut(restores[0]).aux = AuxData::None;
        assert!(
            verify::verify(&bad)
                .unwrap_err()
                .iter()
                .any(|error| error.check == "V13 cleanup")
        );
        let mut bad = f.clone();
        bad.inst_mut(saves[0]).opcode = Opcode::ClearMv;
        assert!(
            verify::verify(&bad)
                .unwrap_err()
                .iter()
                .any(|error| error.check == "V13 cleanup")
        );
        if source.contains("(if") {
            let mut bad = f.clone();
            bad.inst_mut(restores[0]).opcode = Opcode::ClearMv;
            bad.inst_mut(restores[0]).aux = AuxData::None;
            assert!(
                verify::verify(&bad)
                    .unwrap_err()
                    .iter()
                    .any(|error| error.check == "V13 cleanup"
                        && error.detail.contains("join disagrees")),
                "one branch cannot arrive with an extra pending cleanup"
            );
        }
        assert!(
            torcl_compiler::t2::emit::emit_framed_transfers(&f, 1, body.num_slots()).is_err(),
            "no emission until native cleanup helpers are wired"
        );
        assert!(torcl_compiler::t2::emit::emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).is_err());
    }
}
