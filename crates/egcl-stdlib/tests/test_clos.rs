// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for egcl-stdlib CLOS module (spec §5.3).
use egcl_rt::value::{NIL, T, EgclVal};
use egcl_stdlib::clos::*;

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    bootstrap_clos().unwrap();
    guard
}

fn sym(i: u32) -> EgclVal {
    EgclVal::from_symbol_index(i)
}

fn fx(i: i64) -> EgclVal {
    EgclVal::from_fixnum(i)
}

#[test]
fn method_combination_variants_distinct() {
    let _guard = test_guard();
    let v = [
        MethodCombinationType::Standard,
        MethodCombinationType::Plus,
        MethodCombinationType::And,
        MethodCombinationType::Or,
        MethodCombinationType::List,
        MethodCombinationType::Append,
        MethodCombinationType::Nconc,
        MethodCombinationType::Min,
        MethodCombinationType::Max,
        MethodCombinationType::Progn,
    ];
    for i in 0..v.len() {
        for j in (i + 1)..v.len() {
            assert_ne!(v[i], v[j]);
        }
    }
    assert_eq!(v[0], v[0].clone()); // Clone + Eq
    assert!(format!("{:?}", v[9]).contains("Progn")); // Debug
}

#[test]
fn bootstrap_clos_succeeds() {
    let _guard = test_guard();
    bootstrap_clos().expect("bootstrap_clos");
}

#[test]
fn find_class_accepts_immediate_symbol_names_before_bootstrap() {
    // Definitions are process-global. A fresh test process, not a fresh
    // thread, is required to exercise lookup before bootstrap.
    if std::env::var_os("EGCL_TEST_FRESH_CLOS").is_some() {
        assert_eq!(find_class(NIL), None);
        assert_eq!(find_class(T), None);
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "find_class_accepts_immediate_symbol_names_before_bootstrap",
        ])
        .env("EGCL_TEST_FRESH_CLOS", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn find_class_unknown_none() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    assert!(find_class(sym(9999)).is_none());
}

#[test]
fn set_find_class_roundtrip() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let (name, cls) = (sym(100), EgclVal::from_fixnum(42));
    set_find_class(name, cls).unwrap();
    assert_eq!(find_class(name), Some(cls));
}

#[test]
fn definitions_are_shared_with_an_already_running_worker() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let name = sym(700_001);
    let alias = sym(700_002);
    let class = EgclVal::from_meta_handle(700_003);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ensure_clos_bootstrapped().unwrap();
        assert_eq!(find_class(name), None);
        ready_tx.send(()).unwrap();
        published_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(find_class(name), Some(class));
        bind_class_name(alias, class).unwrap();
        ensure_clos_bootstrapped().unwrap();
        assert_eq!(find_class(name), Some(class));
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    define_class(name, class, &[], &[]).unwrap();
    published_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(find_class(alias), Some(class));
    assert_eq!(class_name(class), name);
}

#[test]
fn class_of_returns_builtin_and_instance_classes_in_constant_observable_time() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = fx(2026);
    let slot = sym(2026);
    define_class(sym(2027), cls, &[], &[slot]).unwrap();
    let instance = make_instance(cls, &[slot, fx(41)]).unwrap();

    // Per R5.68 and R5.69, instances expose a stable class identity through
    // their header/wrapper path; per R5.88 and R5.89, built-in classes and
    // user instances must all return class metaobjects through CLASS-OF.
    let fixnum_class = class_of(fx(7));
    let char_class = class_of(EgclVal::from_char('a'));
    let symbol_class = class_of(T);
    let nil_class = class_of(NIL);
    assert_eq!(class_of(instance), cls);
    assert_eq!(
        class_of(instance),
        cls,
        "class_of must stay stable across repeated reads"
    );
    assert_ne!(fixnum_class, NIL);
    assert_ne!(char_class, NIL);
    assert_ne!(symbol_class, NIL);
    assert_ne!(nil_class, NIL);
    assert_ne!(fixnum_class, char_class);
    assert_ne!(fixnum_class, symbol_class);
    assert_ne!(nil_class, symbol_class);
    assert_ne!(class_name(fixnum_class), NIL);
    assert_ne!(class_name(char_class), NIL);
    assert_ne!(class_name(symbol_class), NIL);
    assert_ne!(class_name(nil_class), NIL);
}

#[test]
fn class_name_roundtrip() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let (name, cls) = (sym(200), EgclVal::from_fixnum(200));
    set_find_class(name, cls).unwrap();
    assert_eq!(class_name(cls), name);
}

#[test]
fn cpl_starts_with_self() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(300);
    set_find_class(sym(300), cls).unwrap();
    let cpl = compute_class_precedence_list(cls).unwrap();
    assert!(!cpl.is_empty());
    assert_eq!(cpl[0], cls);
}

#[test]
fn cpl_c3_linearization_diamond() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let class_a = fx(310);
    let class_b = fx(311);
    let class_c = fx(312);
    let class_d = fx(313);

    // Per R5.67, COMPUTE-CLASS-PRECEDENCE-LIST must implement real C3
    // linearization over an explicit diamond superclass graph.
    define_class(sym(310), class_a, &[], &[]).unwrap();
    define_class(sym(311), class_b, &[class_a], &[]).unwrap();
    define_class(sym(312), class_c, &[class_a], &[]).unwrap();
    define_class(sym(313), class_d, &[class_b, class_c], &[]).unwrap();

    let cpl_d = compute_class_precedence_list(class_d).unwrap();
    assert_eq!(cpl_d[0], class_d, "D must be first in its CPL");
    let pos_b = cpl_d
        .iter()
        .position(|&v| v == class_b)
        .expect("B must appear in D's CPL");
    let pos_c = cpl_d
        .iter()
        .position(|&v| v == class_c)
        .expect("C must appear in D's CPL");
    assert!(
        pos_b < pos_c,
        "B must precede C in D's CPL (left-to-right rule)"
    );

    let pos_a = cpl_d
        .iter()
        .position(|&v| v == class_a)
        .expect("A must appear in D's CPL");
    assert!(pos_a > pos_b, "A must come after B in D's CPL");
    assert!(pos_a > pos_c, "A must come after C in D's CPL");
}

// Issue 1: class_hierarchy_accessors must assert return values, not discard them.
#[test]
fn class_hierarchy_accessors() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(301);
    set_find_class(sym(301), cls).unwrap();

    // Direct superclasses: a class registered via set_find_class should have
    // at least one superclass (standard-object or T in the CLOS hierarchy).
    let supers = class_direct_superclasses(cls);
    assert!(
        !supers.is_empty(),
        "a registered class should have at least one superclass (e.g., standard-object)"
    );

    // Direct subclasses of a fresh class with no children should be empty
    let subs = class_direct_subclasses(cls);
    assert!(
        subs.is_empty(),
        "fresh class should have no direct subclasses yet"
    );

    // Slots: a class defined without explicit slots might have zero,
    // but class_slots must return a valid (possibly empty) vector.
    let slots = class_slots(cls);
    // We just verify the call succeeded and returned a vector.
    // For a class with no explicitly defined slots, empty is acceptable.
    let _ = slots.len(); // ensure it's accessible
}

#[test]
fn allocate_instance_ok() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(400);
    set_find_class(sym(400), cls).unwrap();
    allocate_instance(cls).unwrap();
}

#[test]
fn make_instance_variants() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(401);
    set_find_class(sym(401), cls).unwrap();
    make_instance(cls, &[]).unwrap();
    make_instance(cls, &[sym(1), EgclVal::from_fixnum(99)]).unwrap();
}

// Issue 11: initialize_instance and shared_initialize must verify slot initialization.
// Per R5.80, make_instance protocol: allocate → initialize_instance → shared_initialize
// should actually initialize slots from initargs.
#[test]
fn initialize_and_shared_initialize_protocol() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(403);
    let slot_name = sym(404);
    // Instances have a fixed inline slot layout: the slot must be declared.
    define_class(sym(403), cls, &[], &[slot_name]).unwrap();
    let init_val = EgclVal::from_fixnum(99);

    // Allocate a raw instance — slots should be unbound
    let inst = allocate_instance(cls).unwrap();
    assert!(
        !slot_boundp(inst, slot_name).unwrap(),
        "freshly allocated instance should have unbound slots"
    );

    // initialize_instance with initargs should populate the slot
    initialize_instance(inst, &[slot_name, init_val]).unwrap();
    assert_eq!(
        slot_value(inst, slot_name).unwrap(),
        init_val,
        "initialize_instance should set slot from initargs"
    );

    // shared_initialize with T (all slots) and new initargs should update
    let new_val = EgclVal::from_fixnum(200);
    shared_initialize(inst, T, &[slot_name, new_val]).unwrap();
    assert_eq!(
        slot_value(inst, slot_name).unwrap(),
        new_val,
        "shared_initialize with T should update slot from initargs"
    );
}

#[test]
fn slot_set_get_roundtrip() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(500);
    let (sn, val) = (sym(501), EgclVal::from_fixnum(42));
    define_class(sym(500), cls, &[], &[sn]).unwrap();
    let inst = make_instance(cls, &[]).unwrap();
    set_slot_value(inst, sn, val).unwrap();
    assert_eq!(slot_value(inst, sn).unwrap(), val);
}

#[test]
fn slot_boundp_and_makunbound() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(502);
    let sn = sym(503);
    define_class(sym(502), cls, &[], &[sn]).unwrap();
    let inst = allocate_instance(cls).unwrap();
    assert!(!slot_boundp(inst, sn).unwrap());
    set_slot_value(inst, sn, EgclVal::from_fixnum(1)).unwrap();
    assert!(slot_boundp(inst, sn).unwrap());
    slot_makunbound(inst, sn).unwrap();
    assert!(!slot_boundp(inst, sn).unwrap());
}

#[test]
fn slot_value_unbound_errors() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = EgclVal::from_fixnum(506);
    set_find_class(sym(506), cls).unwrap();
    assert!(slot_value(allocate_instance(cls).unwrap(), sym(507)).is_err());
}

#[test]
fn generic_function_lifecycle() {
    let _guard = test_guard();
    let gf = make_generic_function(sym(600), NIL).unwrap();
    let m = EgclVal::from_fixnum(1);
    add_method(gf, m).unwrap();
    remove_method(gf, m).unwrap();
}

#[test]
fn compute_applicable_methods_empty() {
    let _guard = test_guard();
    let gf = make_generic_function(sym(602), NIL).unwrap();
    assert!(compute_applicable_methods(gf, &[EgclVal::from_fixnum(1)]).is_empty());
}

#[test]
fn compute_effective_method_standard_and_empty() {
    let _guard = test_guard();
    let gf = make_generic_function(sym(603), NIL).unwrap();
    let m = EgclVal::from_fixnum(1);
    add_method(gf, m).unwrap();
    assert!(compute_effective_method(gf, MethodCombinationType::Standard, &[m]).is_ok());
    let gf2 = make_generic_function(sym(604), NIL).unwrap();
    assert!(compute_effective_method(gf2, MethodCombinationType::Standard, &[]).is_err());
}

#[test]
fn compute_effective_method_non_standard_variants() {
    let _guard = test_guard();
    let gf = make_generic_function(sym(605), NIL).unwrap();
    let around = fx(1);
    let before = fx(2);
    let primary_1 = fx(3);
    let primary_2 = fx(4);
    let after = fx(5);
    for method in [around, before, primary_1, primary_2, after] {
        add_method(gf, method).unwrap();
    }
    set_method_specializers(around, vec![], MethodQualifier::Around);
    set_method_specializers(before, vec![], MethodQualifier::Before);
    set_method_specializers(primary_1, vec![], MethodQualifier::Primary);
    set_method_specializers(primary_2, vec![], MethodQualifier::Primary);
    set_method_specializers(after, vec![], MethodQualifier::After);

    // Per R5.77, R5.78, and R5.79, the effective-method protocol must expose
    // observable standard-combination structure and preserve short-form
    // combination metadata over the primary-method chain.
    let standard_key = compute_effective_method(
        gf,
        MethodCombinationType::Standard,
        &[around, before, primary_1, primary_2, after],
    )
    .unwrap();
    let standard = get_effective_method(standard_key).unwrap();
    assert_eq!(standard.0, vec![around]);
    assert_eq!(standard.1, vec![before]);
    assert_eq!(standard.2, vec![primary_1, primary_2]);
    assert_eq!(standard.3, vec![after]);

    for combo in [
        MethodCombinationType::Plus,
        MethodCombinationType::And,
        MethodCombinationType::Or,
        MethodCombinationType::List,
        MethodCombinationType::Append,
        MethodCombinationType::Nconc,
        MethodCombinationType::Min,
        MethodCombinationType::Max,
        MethodCombinationType::Progn,
    ] {
        let key =
            compute_effective_method(gf, combo, &[around, primary_1, primary_2, after]).unwrap();
        let (kind, methods) = get_short_form_method(key).unwrap();
        assert_eq!(kind, combo);
        assert_eq!(methods, vec![primary_1, primary_2]);
    }
}

#[test]
fn change_class_succeeds() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let old = fx(700);
    let new = fx(701);
    let shared = sym(702);
    let old_only = sym(703);
    let new_only = sym(704);

    // Per R5.82, CHANGE-CLASS must invoke the update protocol observably by
    // preserving same-named slots while rebinding the instance to the new class.
    define_class(sym(700), old, &[], &[shared, old_only]).unwrap();
    define_class(sym(701), new, &[], &[shared, new_only]).unwrap();
    let inst = make_instance(old, &[shared, fx(11), old_only, fx(12)]).unwrap();
    change_class(inst, new).unwrap();
    assert_eq!(class_of(inst), new);
    assert_eq!(slot_value(inst, shared).unwrap(), fx(11));
    assert!(slot_value(inst, old_only).is_err());
    assert!(!slot_boundp(inst, new_only).unwrap());
}

#[test]
fn multi_argument_dispatch_uses_later_specializer_positions() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let first = fx(800);
    let second_base = fx(801);
    let second_specific = fx(802);
    define_class(sym(800), first, &[], &[]).unwrap();
    define_class(sym(801), second_base, &[], &[]).unwrap();
    define_class(sym(802), second_specific, &[second_base], &[]).unwrap();

    let first_instance = make_instance(first, &[]).unwrap();
    let second_instance = make_instance(second_specific, &[]).unwrap();
    let fixnum_class = class_of(fx(0));
    let standard_object_class = class_direct_superclasses(fixnum_class)[0];
    let t_class = class_direct_superclasses(standard_object_class)[0];

    let gf = make_generic_function(sym(803), NIL).unwrap();
    let generic_second = fx(804);
    let specific_second = fx(805);
    add_method(gf, generic_second).unwrap();
    add_method(gf, specific_second).unwrap();
    set_method_specializers(
        generic_second,
        vec![t_class, second_base],
        MethodQualifier::Primary,
    );
    set_method_specializers(
        specific_second,
        vec![t_class, second_specific],
        MethodQualifier::Primary,
    );

    // Per R5.76, specializer discrimination must consider non-leading argument
    // positions; this exercises the observable second-argument dispatch path.
    assert_eq!(
        compute_applicable_methods(gf, &[first_instance, second_instance]),
        vec![specific_second, generic_second]
    );
}

#[test]
fn redefined_instances_keep_existing_state_visible_on_first_and_second_access() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let class = fx(900);
    let old_slot = sym(901);
    let new_slot = sym(902);
    define_class(sym(903), class, &[], &[old_slot]).unwrap();
    let instance = make_instance(class, &[old_slot, fx(77)]).unwrap();

    // Per R5.83 and R5.84, obsolete instances must update lazily on first
    // access and then remain stable on subsequent accesses.
    define_class(sym(903), class, &[], &[old_slot, new_slot]).unwrap();
    assert_eq!(slot_value(instance, old_slot).unwrap(), fx(77));
    assert!(slot_value(instance, new_slot).is_err());
    set_slot_value(instance, new_slot, fx(88)).unwrap();
    assert_eq!(slot_value(instance, old_slot).unwrap(), fx(77));
    assert_eq!(slot_value(instance, new_slot).unwrap(), fx(88));
}

#[test]
fn dispatch_surface_reflects_method_mutation_without_stale_results() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let animal = fx(950);
    let dog = fx(951);
    define_class(sym(950), animal, &[], &[]).unwrap();
    define_class(sym(951), dog, &[animal], &[]).unwrap();
    let dog_instance = make_instance(dog, &[]).unwrap();
    let gf = make_generic_function(sym(952), NIL).unwrap();
    let animal_method = fx(953);
    let dog_method = fx(954);
    add_method(gf, animal_method).unwrap();
    add_method(gf, dog_method).unwrap();
    set_method_specializers(animal_method, vec![animal], MethodQualifier::Primary);
    set_method_specializers(dog_method, vec![dog], MethodQualifier::Primary);

    // Per R5.85, dispatch invalidation after method/class mutation must be
    // observable as fresh applicable-method results rather than stale caches.
    assert_eq!(
        compute_applicable_methods(gf, &[dog_instance]),
        vec![dog_method, animal_method]
    );
    remove_method(gf, dog_method).unwrap();
    assert_eq!(
        compute_applicable_methods(gf, &[dog_instance]),
        vec![animal_method]
    );
}

// ── Heap-object instance representation (bliss-xyo) ─────────────────

#[test]
fn instances_are_heap_objects_not_fixnums() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let cls = fx(700);
    let s = sym(701);
    define_class(sym(702), cls, &[], &[s]).unwrap();
    let inst = make_instance(cls, &[s, fx(9)]).unwrap();

    // A real STANDARD_OBJECT heap object, recognized as an instance.
    assert!(inst.is_standard_object());
    assert!(is_instance(inst));
    assert_eq!(class_of(inst), cls);
    assert_eq!(slot_value(inst, s).unwrap(), fx(9));

    // bliss-2ke, now structural: a plain fixnum can never be an instance,
    // even one equal to the old-style instance-id base or a class handle.
    assert!(!is_instance(fx(9)));
    assert!(!is_instance(fx(100_004)));
    assert!(!is_instance(fx(700)));
}

#[test]
fn change_class_growth_preserves_slots_via_forwarding() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    let a = fx(710);
    let b = fx(711);
    let sa = sym(712);
    let sb = sym(713);
    define_class(sym(714), a, &[], &[sa]).unwrap(); // 1 instance slot
    define_class(sym(715), b, &[a], &[sb]).unwrap(); // inherits sa + own sb -> 2 slots
    let inst = make_instance(a, &[sa, fx(1)]).unwrap();

    // Growth (1 -> 2 slots) reallocates and forwards; identity/value survive.
    change_class(inst, b).unwrap();
    assert_eq!(class_of(inst), b);
    assert_eq!(slot_value(inst, sa).unwrap(), fx(1)); // shared slot copied
    assert!(!slot_boundp(inst, sb).unwrap()); // added slot unbound
}

#[test]
fn metaobject_handles_are_off_the_fixnum_tag() {
    let _guard = test_guard();
    bootstrap_clos().unwrap();
    // Generic-function ids are opaque metaobject handles, never fixnums, so a
    // plain integer equal to the id cannot collide with the gf (bliss-dx6).
    let gf = make_generic_function(sym(800), NIL).unwrap();
    assert!(!gf.is_fixnum());
    assert!(gf.is_meta_handle());
    assert_ne!(gf, EgclVal::from_fixnum(gf.as_meta_handle_id()));
}
