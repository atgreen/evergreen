// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn restored_named_views_share_the_exact_class_owned_cells() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defclass owner-core-unit () ((shared :allocation :class)))",
        &mut env,
    )
    .unwrap();
    let name = resolve_sym("OWNER-CORE-UNIT").unwrap();
    egcl_rt::rooted!(class = egcl_stdlib::find_class(name).unwrap());
    let previous_classes = SAVE_CLASSES.with(|cell| cell.replace(Some(env.classes)));
    let previous_methods = SAVE_METHODS.with(|cell| cell.replace(Some(env.methods)));
    let previous_generics = SAVE_GENERICS.with(|cell| cell.replace(Some(env.generics)));
    let bytes = host_serialize_registries();
    SAVE_CLASSES.with(|cell| *cell.borrow_mut() = previous_classes);
    SAVE_METHODS.with(|cell| *cell.borrow_mut() = previous_methods);
    SAVE_GENERICS.with(|cell| *cell.borrow_mut() = previous_generics);
    host_restore_registries(&bytes).unwrap();
    drain_pending_host_generics(&env);
    let cells = egcl_stdlib::clos::class_runtime_shared_cells(*class).unwrap();
    {
        let views = env.classes.borrow();
        let view = &views["OWNER-CORE-UNIT"];
        assert_eq!(view.owner, *class);
        assert!(
            Arc::ptr_eq(&cells, &view.class_slot_values),
            "a restored source view must not keep a separately deserialized cell map"
        );
    }
    cells
        .lock()
        .unwrap()
        .insert("SHARED".into(), Some(EgclVal::from_fixnum(71)));
    assert_eq!(
        env.classes.borrow()["OWNER-CORE-UNIT"]
            .class_slot_values
            .lock()
            .unwrap()["SHARED"],
        Some(EgclVal::from_fixnum(71))
    );

    let marker = bytes
        .windows(4)
        .position(|window| window == b"CLS3")
        .unwrap();
    let mut legacy = bytes;
    legacy[marker..marker + 4].copy_from_slice(b"CLS2");
    assert!(
        host_restore_registries(&legacy)
            .unwrap_err()
            .to_string()
            .contains("obsolete name-owned class definitions")
    );
}

const RELOCATION_CHILD: &str = "EGCL_CLASS_DEFINITION_OWNER_RELOCATION_CHILD";
const RELOCATION_FILTER: &str =
    "cli::class_definition_owner_tests::class_owned_metadata_relocation_child";

#[test]
fn class_owned_metadata_remains_rooted_without_any_named_source_view() {
    for poison in [false, true] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                RELOCATION_FILTER,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(RELOCATION_CHILD, "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
            "EGCL_GC_DISABLE",
        ] {
            command.env_remove(name);
        }
        if poison {
            command.env("EGCL_GC_POISON", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "poison={poison}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("CLASS-OWNER-METADATA-RELOCATED"),
            "{stdout}\n{stderr}"
        );
    }
}

#[test]
#[ignore = "fresh-process child driven by the relocation parent"]
fn class_owned_metadata_relocation_child() {
    if std::env::var_os(RELOCATION_CHILD).is_none() {
        return;
    }
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defclass owner-moving-unit () ((shared :allocation :class :initarg :shared)))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(
        class = egcl_stdlib::find_class(resolve_sym("OWNER-MOVING-UNIT").unwrap()).unwrap()
    );
    let list = resolve_sym("LIST").unwrap();
    let option = resolve_sym(":OWNER-PAYLOAD").unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    // No interning/compilation after this fresh allocation can pin its region.
    egcl_rt::rooted!(tail = arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(payload = arena_cons(list, *tail));
    let before = payload.to_raw();
    egcl_rt::rooted!(definition = egcl_stdlib::clos::class_runtime_definition(*class).unwrap());
    definition.slots[0].initform = Some(*payload);
    definition.slots[0].source_initform = Some(*payload);
    definition.slots[0].mop_options = vec![option, *payload];
    definition.default_initargs = vec![("SHARED".into(), *payload)];
    egcl_stdlib::clos::set_class_runtime_definition(*class, definition.clone()).unwrap();
    runtime_write_class_slot(&env, *class, "SHARED", Some(*payload));
    drop(definition);
    // The separately rooted original must move, and every independent copy in
    // ClassMeta must be rewritten. A named view must not mask a missing trace.
    env.classes
        .borrow_mut()
        .retain(|_, view| view.owner != *class);
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(
        payload.to_raw(),
        before,
        "the metadata payload must actually move"
    );
    let definition = egcl_stdlib::clos::class_runtime_definition(*class).unwrap();
    assert_eq!(
        definition.slots[0].initform.unwrap().to_raw(),
        payload.to_raw()
    );
    assert_eq!(
        definition.slots[0].source_initform.unwrap().to_raw(),
        payload.to_raw()
    );
    assert_eq!(
        definition.slots[0].mop_options[1].to_raw(),
        payload.to_raw()
    );
    assert_eq!(definition.default_initargs[0].1.to_raw(), payload.to_raw());
    assert_eq!(
        definition.class_slot_values.lock().unwrap()["SHARED"]
            .unwrap()
            .to_raw(),
        payload.to_raw()
    );
    assert_eq!(cp(cp(*payload).1).0, EgclVal::from_fixnum(71));
    println!("CLASS-OWNER-METADATA-RELOCATED");
}

#[test]
fn name_reuse_cannot_reuse_another_class_accessor_cache_entry() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defclass owner-cache-unit () ((left :accessor owner-cache-reader)))",
        &mut env,
    )
    .unwrap();
    let name = resolve_sym("OWNER-CACHE-UNIT").unwrap();
    let accessor = resolve_sym("OWNER-CACHE-READER").unwrap();
    let left = resolve_sym("LEFT").unwrap();
    let right = resolve_sym("RIGHT").unwrap();
    egcl_rt::rooted!(old_class = egcl_stdlib::find_class(name).unwrap());
    egcl_rt::rooted!(old = egcl_stdlib::allocate_instance(*old_class).unwrap());
    assert_eq!(
        accessor_slot_symbol_cached(&env, *old, accessor),
        Some(left)
    );
    egcl_stdlib::clos::bind_class_name(name, NIL).unwrap();
    read_eval_all_env(
        "(defclass owner-cache-unit () ((right :accessor owner-cache-reader)))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(new_class = egcl_stdlib::find_class(name).unwrap());
    egcl_rt::rooted!(new = egcl_stdlib::allocate_instance(*new_class).unwrap());
    assert_ne!(*old_class, *new_class);
    assert_eq!(class_name_symbol(*old_class), None);
    assert_eq!(
        accessor_slot_symbol_cached(&env, *new, accessor),
        Some(right)
    );
    assert_eq!(accessor_slot_symbol_cached(&env, *old, accessor), None);
    assert_eq!(
        accessor_slot_name_in_class_chain(&env, *old, &sym_name(accessor)),
        Some("LEFT".into())
    );
    assert_eq!(
        accessor_slot_symbol_cached(&env, *new, accessor),
        Some(right)
    );
}

#[test]
fn unchanged_primary_name_does_not_invalidate_native_calls() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defclass unchanged-primary-name () ())", &mut env).unwrap();
    egcl_rt::rooted!(
        class = egcl_stdlib::find_class(resolve_sym("UNCHANGED-PRIMARY-NAME").unwrap()).unwrap()
    );
    egcl_rt::rooted!(name = egcl_stdlib::class_name(*class));
    let operator = "EGCL-INTERNAL:%SET-CLASS-NAME";
    let before = bytecode::direct_call_gen();
    assert_eq!(
        evaluated_builtins::call(operator, &[*name, *class], &mut env)
            .unwrap()
            .unwrap(),
        *name
    );
    assert_eq!(bytecode::direct_call_gen(), before);
    assert_eq!(
        evaluated_builtins::call(operator, &[NIL, *class], &mut env)
            .unwrap()
            .unwrap(),
        NIL
    );
    let changed = bytecode::direct_call_gen();
    assert_ne!(changed, before);
    assert_eq!(
        evaluated_builtins::call(operator, &[NIL, *class], &mut env)
            .unwrap()
            .unwrap(),
        NIL
    );
    assert_eq!(bytecode::direct_call_gen(), changed);
}
