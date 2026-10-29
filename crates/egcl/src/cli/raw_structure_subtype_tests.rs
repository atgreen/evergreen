// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn setup(env: &mut Env) {
    read_eval_all_env(
        "(defstruct (raw-proof-parent (:constructor nil)))
         (defstruct (raw-proof-child (:constructor nil) (:include raw-proof-parent)))
         (defstruct (raw-proof-sibling (:constructor nil) (:include raw-proof-parent)))",
        env,
    )
    .unwrap();
}

fn read(source: &str, env: &mut Env) -> EgclVal {
    read_from_string_in_env(source, env).unwrap().0
}

#[test]
fn raw_structure_boolean_proofs_respect_inclusion_and_ancestor_overlap() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    setup(&mut env);
    let child = read("raw-proof-child", &mut env);
    let parent = read("raw-proof-parent", &mut env);
    for (left, source, expected) in [
        (
            child,
            "(and raw-proof-parent atom structure-object t)",
            (true, true),
        ),
        (
            child,
            "(or raw-proof-sibling raw-proof-parent)",
            (true, true),
        ),
        (
            child,
            "(and raw-proof-parent (not raw-proof-sibling))",
            (true, true),
        ),
        (child, "(and)", (true, true)),
        (child, "(or)", (false, false)),
        (parent, "(not raw-proof-child)", (false, false)),
        (child, "(not raw-proof-parent)", (false, false)),
    ] {
        egcl_rt::rooted!(right = read(source, &mut env));
        assert_eq!(
            raw_structure_boolean_subtypep(left, *right),
            Some(expected),
            "{source}"
        );
    }
    egcl_rt::rooted!(class = egcl_stdlib::find_class(child).unwrap());
    egcl_rt::rooted!(right = read("(and raw-proof-parent)", &mut env));
    assert_eq!(
        raw_structure_boolean_subtypep(*class, *right),
        Some((true, true))
    );
}

#[test]
fn raw_structure_boolean_syntax_cycles_and_budgets_remain_unknown() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    setup(&mut env);
    let child = read("raw-proof-child", &mut env);
    for source in [
        "(and raw-proof-parent . raw-proof-parent)",
        "(not)",
        "(not raw-proof-parent raw-proof-child)",
        "(or t 999999999)",
        "(or t (unsupported-type raw-proof-parent))",
        // Validation must inspect all operands before accepting the true arm.
        "(or raw-proof-parent (not raw-proof-parent raw-proof-child))",
    ] {
        egcl_rt::rooted!(right = read(source, &mut env));
        assert_eq!(
            raw_structure_boolean_subtypep(child, *right),
            Some((false, false)),
            "{source}"
        );
    }
    let and = resolve_sym("AND").unwrap();
    let not = resolve_sym("NOT").unwrap();
    egcl_rt::rooted!(spine = arena_cons(parent_symbol(&mut env), NIL));
    unsafe {
        let cell = spine.as_ptr() as *mut ConsCell;
        egcl_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).cdr), *spine);
    }
    egcl_rt::rooted!(cyclic_spine = arena_cons(and, *spine));
    assert_eq!(
        raw_structure_boolean_subtypep(child, *cyclic_spine),
        Some((false, false))
    );
    egcl_rt::rooted!(operand = arena_cons(NIL, NIL));
    egcl_rt::rooted!(cyclic_tree = arena_cons(not, *operand));
    unsafe {
        let cell = operand.as_ptr() as *mut ConsCell;
        egcl_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).car), *cyclic_tree);
    }
    assert_eq!(
        raw_structure_boolean_subtypep(child, *cyclic_tree),
        Some((false, false))
    );
    egcl_rt::rooted!(deep = child);
    for _ in 0..40 {
        egcl_rt::rooted!(tail = arena_cons(*deep, NIL));
        *deep = arena_cons(not, *tail);
    }
    assert_eq!(
        raw_structure_boolean_subtypep(child, *deep),
        Some((false, false))
    );
    egcl_rt::rooted!(wide = NIL);
    for _ in 0..257 {
        *wide = arena_cons(T, *wide);
    }
    egcl_rt::rooted!(over_budget = arena_cons(and, *wide));
    assert_eq!(
        raw_structure_boolean_subtypep(child, *over_budget),
        Some((false, false))
    );
}

fn parent_symbol(env: &mut Env) -> EgclVal {
    read("raw-proof-parent", env)
}

#[test]
fn raw_structure_proofs_require_canonical_operators_and_exact_class_identity() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    setup(&mut env);
    let child = read("raw-proof-child", &mut env);
    egcl_rt::rooted!(right = read("(and raw-proof-parent)", &mut env));
    egcl_rt::rooted!(lookalike = egcl_rt::symbols::make_uninterned(&sym_name(child)));
    assert_eq!(
        raw_structure_boolean_subtypep(*lookalike, *right),
        None,
        "legacy FIND-CLASS name fallback must not certify an unrelated symbol"
    );
    egcl_rt::rooted!(foreign_and = egcl_rt::symbols::make_uninterned("AND"));
    egcl_rt::rooted!(tail = arena_cons(child, NIL));
    egcl_rt::rooted!(foreign_form = arena_cons(*foreign_and, *tail));
    assert_eq!(raw_structure_boolean_subtypep(child, *foreign_form), None);
    let integer = resolve_sym("INTEGER").unwrap();
    assert_eq!(raw_structure_boolean_subtypep(integer, *right), None);
    assert_eq!(raw_structure_boolean_subtypep(child, integer), None);
}
