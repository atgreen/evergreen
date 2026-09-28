use super::*;
use torcl_compiler::t2::emit::TransferCallRequest;
use torcl_rt::native_transfer::{NativeExit, NativeOutcome};
use torcl_rt::{Collector, HeapCollector};
mod cleanup_ir;
mod compiled;
mod fallback;
mod fibers;
mod payloads;

struct NativeEnvGuard(*mut Env);
impl NativeEnvGuard {
    fn enter(env: &mut Env) -> Self {
        assert!(!native_error_pending());
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnvGuard {
    fn drop(&mut self) {
        NATIVE_ERROR.with(|slot| {
            slot.take();
        });
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}

unsafe fn invoke_bridge(symbol: u32, args: *mut TorclVal, nargs: usize) -> NativeOutcome {
    let mut request = TransferCallRequest {
        symbol: u64::from(symbol),
        nargs,
        args,
        activation: args,
    };
    let mut outcome = NativeOutcome {
        value: NIL,
        exit: NativeExit::Deopt,
    };
    unsafe {
        c2i_call_legacy_v2(
            (&mut request as *mut TransferCallRequest).cast(),
            &mut outcome,
        );
    }
    outcome
}

#[test]
fn native_v2_bridge_preserves_real_lisp_values_errors_and_wide_calls() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(defun v2-zero () (values))
         (defun v2-many (x) (values x (list :second)))
         (defun v2-wide (a b c d e f g h) (list a b c d e f g h))
         (defun v2-error (x) (car x))",
        &mut env,
    )
    .unwrap();
    let zero = torcl_rt::symbols::intern("V2-ZERO");
    let many = torcl_rt::symbols::intern("V2-MANY");
    let wide = torcl_rt::symbols::intern("V2-WIDE");
    let error = torcl_rt::symbols::intern("V2-ERROR");
    let _native = NativeEnvGuard::enter(&mut env);
    let outcome = unsafe { invoke_bridge(zero, std::ptr::null_mut(), 0) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    assert_eq!(outcome.value, NIL);
    assert!(env.mv_active && env.mv.is_empty());

    torcl_rt::rooted!(args = vec![super::super::arena_cons(TorclVal::from_fixnum(19), NIL)]);
    let before = args[0].to_raw();
    let outcome = unsafe { invoke_bridge(many, args.as_mut_ptr(), args.len()) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    torcl_rt::rooted!(primary = outcome.value);
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(*primary, args[0]);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], *primary);
    assert!(env.mv[1].is_cons());
    assert_ne!(
        args[0].to_raw(),
        before,
        "real Lisp bridge preserved a relocated argument"
    );

    torcl_rt::rooted!(wide_args = (0..8).map(TorclVal::from_fixnum).collect::<Vec<_>>());
    let outcome = unsafe { invoke_bridge(wide, wide_args.as_mut_ptr(), wide_args.len()) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    torcl_rt::rooted!(list = outcome.value);
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(super::super::list_to_vec(*list), *wide_args);

    let mut error_args = [TorclVal::from_fixnum(7)];
    let outcome = unsafe { invoke_bridge(error, error_args.as_mut_ptr(), error_args.len()) };
    assert_eq!(outcome.exit, NativeExit::Transfer);
    assert!(native_error_pending());
    assert!(NATIVE_ERROR.with(|slot| slot.take()).is_some());
    assert!(!native_error_pending());
}

#[test]
fn native_v2_bridge_preserves_throw_multiple_values_and_first_error() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(setq *v2-count* 0)
         (defun v2-side-effect () (setq *v2-count* (+ *v2-count* 1)))
         (defun v2-throw (x) (throw :v2-tag (values x (list :secondary))))",
        &mut env,
    )
    .unwrap();
    let throwing = torcl_rt::symbols::intern("V2-THROW");
    let side_effect = torcl_rt::symbols::intern("V2-SIDE-EFFECT");
    let tag = reader::read_from_string(":v2-tag").unwrap().0;
    let token = super::super::next_control_token("V2-CATCH");
    env.catch_stack.push((tag, token.clone()));
    let _native = NativeEnvGuard::enter(&mut env);
    torcl_rt::rooted!(args = vec![super::super::arena_cons(TorclVal::from_fixnum(23), NIL)]);
    let outcome = unsafe { invoke_bridge(throwing, args.as_mut_ptr(), args.len()) };
    assert_eq!(outcome.exit, NativeExit::Transfer);
    env.clear_mv();
    HeapCollector::new().minor_gc().unwrap();
    let error = NATIVE_ERROR.with(|slot| slot.take()).unwrap();
    assert!(matches!(error, TorclError::Internal(ref message) if message == &token));
    let primary = super::super::take_control_mv(&token, &mut env);
    assert_eq!(primary, args[0]);
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], primary);
    assert!(env.mv[1].is_cons());
    env.catch_stack.pop();

    NATIVE_ERROR.with(|slot| slot.set_first(TorclError::Interrupt));
    assert_eq!(
        unsafe { invoke_bridge(side_effect, std::ptr::null_mut(), 0) }.exit,
        NativeExit::Transfer
    );
    assert!(matches!(
        NATIVE_ERROR.with(|slot| slot.take()),
        Some(TorclError::Interrupt)
    ));
    let value = super::super::read_eval_all_env("*v2-count*", &mut env).unwrap();
    assert_eq!(value, TorclVal::from_fixnum(0));
}
