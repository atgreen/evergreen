// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn compile_cache_body(params: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
    Arc::new(
        compile_function("SEGMENT-CACHE-IDENTITY", *params, *forms, env, false, false).unwrap(),
    )
}

#[test]
fn native_v2_declined_cache_retains_definition_identity() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = compile_cache_body("(&rest x)", &env);
    let key = Arc::as_ptr(&body) as usize;
    assert!(cached_code(&body).is_none());
    assert!(cached_code(&body).is_none());
    assert!(
        Arc::strong_count(&body) > 1 || Arc::weak_count(&body) > 0,
        "a cached decline must keep its address allocated until the entry is removed"
    );
    assert_eq!(
        Arc::strong_count(&body),
        1,
        "a cached decline must not retain bytecode contents or their unrooted constants"
    );
    SEGMENT_CACHE.with(|cache| cache.borrow_mut().remove(&key));
    assert_eq!(Arc::strong_count(&body), 1);
    assert_eq!(Arc::weak_count(&body), 0);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_declined_cache_allows_replacement_definition() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(native_transfer::is_supported());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = compile_cache_body("(&rest x)", &env);
    let declined_key = Arc::as_ptr(&body) as usize;
    assert!(cached_code(&body).is_none());
    drop(body);

    let replacement = compile_cache_body("(x)", &env);
    let replacement_key = Arc::as_ptr(&replacement) as usize;
    assert_ne!(declined_key, replacement_key);
    let code = cached_code(&replacement).expect("replacement must not inherit the old decline");
    assert!(Arc::ptr_eq(&code.body, &replacement));
    assert_eq!(
        code.run(&[EgclVal::from_fixnum(42)], &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
    SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.remove(&declined_key);
        cache.remove(&replacement_key);
    });
}
