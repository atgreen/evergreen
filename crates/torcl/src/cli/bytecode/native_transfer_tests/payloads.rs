use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_throw_keeps_moving_tag_identity_through_cleanup() {
    use super::super::native_transfer_entry::{TransferCode, take_native_cleanup_count};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun moving-tag-value () (%force-minor-gc-for-test) (values :kept :extra))
         (defun moving-tag-cleanup () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    torcl_rt::rooted!(params = reader::read_from_string("(tag)").unwrap().0);
    torcl_rt::rooted!(
        forms = reader::read_from_string(
            "((unwind-protect (throw tag (moving-tag-value))
             (moving-tag-cleanup)))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("MOVING-NATIVE-TAG", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("native throw with heap tag");
    let outer = super::super::super::next_control_token("OUTER-TAG");
    let inner = super::super::super::next_control_token("INNER-TAG");
    env.catch_stack
        .push((super::super::super::arena_cons(T, NIL), outer.clone()));
    // The environment is the only root for this second, same-looking tag.
    env.catch_stack
        .push((super::super::super::arena_cons(T, NIL), inner));
    let before = env.catch_stack[1].0.to_raw();
    torcl_rt::rooted!(args = vec![env.catch_stack[0].0]);
    take_native_cleanup_count();
    torcl_rt::rooted!(result = code.run(&args, &mut env));
    assert!(
        matches!(&*result, Err(TorclError::Internal(token)) if token == &outer),
        "{:?}",
        &*result
    );
    assert_eq!(take_native_cleanup_count(), 1);
    assert_ne!(
        env.catch_stack[1].0.to_raw(),
        before,
        "environment-only tag root moved"
    );
    assert_eq!(super::super::super::cp(env.catch_stack[1].0), (T, NIL));
    assert_ne!(env.catch_stack[0].0, env.catch_stack[1].0);
    let value = super::super::super::take_control_mv(&outer, &mut env);
    assert_eq!(super::super::super::val_as_str(value), "KEYWORD:KEPT");
    assert_eq!(env.mv.len(), 2);
    env.catch_stack.clear();
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_reentrant_cleanup_keeps_private_multiple_values() {
    use super::super::native_transfer_entry::{TransferCode, take_native_cleanup_count};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun reentrant-payload-cleanup ()
           (catch :inner
             (unwind-protect (throw :outer (values :wrong :extra))
               (throw :inner :redirect)))
           (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    for (value, count) in [
        ("x", 1),
        ("(values)", 0),
        ("(values x)", 1),
        ("(values x (list x))", 2),
    ] {
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            forms = reader::read_from_string(&format!(
                "((unwind-protect (throw :outer {value}) (reentrant-payload-cleanup)))"
            ))
            .unwrap()
            .0
        );
        let body = Arc::new(
            compile_function(
                "PRIVATE-NATIVE-PAYLOAD",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = TransferCode::compile(body).expect("exceptional native cleanup");
        let token = super::super::super::next_control_token("PRIVATE-PAYLOAD");
        let tag = reader::read_from_string(":outer").unwrap().0;
        env.catch_stack.push((tag, token.clone()));
        torcl_rt::rooted!(
            args = vec![super::super::super::arena_cons(
                TorclVal::from_fixnum(42),
                NIL
            )]
        );
        let before = args[0].to_raw();
        take_native_cleanup_count();
        torcl_rt::rooted!(result = code.run(&args, &mut env));
        assert!(
            matches!(&*result, Err(TorclError::Internal(t)) if t == &token),
            "{:?}",
            &*result
        );
        assert_eq!(take_native_cleanup_count(), 1);
        assert_ne!(
            args[0].to_raw(),
            before,
            "cleanup must move the saved value"
        );
        let primary = super::super::super::take_control_mv(&token, &mut env);
        assert_eq!(primary, if count == 0 { NIL } else { args[0] }, "{value}");
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), count, "{value}");
        if count == 2 {
            assert_eq!(super::super::super::cp(env.mv[1]).0, args[0]);
        }
        env.catch_stack.pop();
    }
}
