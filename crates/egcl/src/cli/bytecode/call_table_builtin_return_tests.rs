// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn builtin_entry_preserves_errors_contains_panics_and_recovers() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    let symbol = super::super::super::resolve_sym("CHAR-CODE")
        .unwrap()
        .as_symbol_index();
    let cell = resolve(symbol).unwrap();
    let address = Arc::as_ptr(&cell) as u64;
    let state = unsafe { state(address) };
    struct RestoreEnv(*mut Env);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            NATIVE_ENV.with(|slot| slot.set(self.0));
        }
    }
    let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
    let character = EgclVal::from_char('A');
    assert_eq!(cold(state, &[character]).unwrap(), EgclVal::from_fixnum(65));
    assert!(matches!(
        *state.target.borrow(),
        Some(Target::Builtin { .. })
    ));
    let entry = unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
    let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
        unsafe { std::mem::transmute(entry) };
    assert!(!native_error_pending());

    egcl_rt::rooted!(datum = super::super::super::arena_str("not a character"));
    assert_eq!(call(address, 1, datum.0, 0, 0, 0), NIL.0);
    // A later arity failure must not replace the original typed condition.
    assert_eq!(call(address, 0, 0, 0, 0, 0), NIL.0);
    let error = NATIVE_ERROR.with(|slot| slot.take()).unwrap();
    assert!(matches!(error.without_backtrace(),
        EgclError::TypeError { datum: actual, .. } if *actual == *datum));
    assert_eq!(
        call(address, 1, character.0, 0, 0, 0),
        EgclVal::from_fixnum(65).0
    );
    assert!(!native_error_pending());

    assert_eq!(call(address, 0, 0, 0, 0, 0), NIL.0);
    let error = NATIVE_ERROR.with(|slot| slot.take()).unwrap();
    assert!(matches!(
        error.without_backtrace(),
        EgclError::ProgramError(_)
    ));

    // Force the real entry's metadata read to panic. The panic must be caught
    // inside the Rust callback, before it could unwind across extern "C".
    let held = state.target.borrow_mut();
    assert_eq!(call(address, 1, character.0, 0, 0, 0), NIL.0);
    drop(held);
    let error = NATIVE_ERROR.with(|slot| slot.take()).unwrap();
    assert!(
        matches!(error.without_backtrace(), EgclError::Internal(message)
        if message.contains("recovered a panic reached from compiled code")
            && message.contains("borrow"))
    );
    assert_eq!(
        call(address, 1, character.0, 0, 0, 0),
        EgclVal::from_fixnum(65).0
    );
    assert!(!native_error_pending());
}
