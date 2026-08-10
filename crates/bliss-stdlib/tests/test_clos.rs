//! Tests for bliss-stdlib CLOS module (spec §5.3).
use bliss_stdlib::clos::*;
use bliss_rt::value::{BlissVal, NIL, T};

fn sym(i: u32) -> BlissVal { BlissVal::from_symbol_index(i) }

#[test]
fn method_combination_variants_distinct() {
    let v = [
        MethodCombinationType::Standard, MethodCombinationType::Plus,
        MethodCombinationType::And,      MethodCombinationType::Or,
        MethodCombinationType::List,     MethodCombinationType::Append,
        MethodCombinationType::Nconc,    MethodCombinationType::Min,
        MethodCombinationType::Max,      MethodCombinationType::Progn,
    ];
    for i in 0..v.len() {
        for j in (i + 1)..v.len() { assert_ne!(v[i], v[j]); }
    }
    assert_eq!(v[0], v[0].clone()); // Clone + Eq
    assert!(format!("{:?}", v[9]).contains("Progn")); // Debug
}

#[test]
fn bootstrap_clos_succeeds() {
    bootstrap_clos().expect("bootstrap_clos");
}

#[test]
fn find_class_unknown_none() {
    bootstrap_clos().unwrap();
    assert!(find_class(sym(9999)).is_none());
}

#[test]
fn set_find_class_roundtrip() {
    bootstrap_clos().unwrap();
    let (name, cls) = (sym(100), BlissVal::from_fixnum(42));
    set_find_class(name, cls).unwrap();
    assert_eq!(find_class(name), Some(cls));
}

#[test]
fn class_of_various() {
    bootstrap_clos().unwrap();
    let _ = class_of(BlissVal::from_fixnum(7));
    let _ = class_of(NIL);
}

#[test]
fn class_name_roundtrip() {
    bootstrap_clos().unwrap();
    let (name, cls) = (sym(200), BlissVal::from_fixnum(200));
    set_find_class(name, cls).unwrap();
    assert_eq!(class_name(cls), name);
}

#[test]
fn cpl_starts_with_self() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(300);
    set_find_class(sym(300), cls).unwrap();
    let cpl = compute_class_precedence_list(cls).unwrap();
    assert!(!cpl.is_empty());
    assert_eq!(cpl[0], cls);
}

#[test]
fn class_hierarchy_accessors() {
    bootstrap_clos().unwrap();
    let c = BlissVal::from_fixnum(301);
    let _ = class_direct_superclasses(c);
    let _ = class_direct_subclasses(c);
    let _ = class_slots(c);
}

#[test]
fn allocate_instance_ok() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(400);
    set_find_class(sym(400), cls).unwrap();
    allocate_instance(cls).unwrap();
}

#[test]
fn make_instance_variants() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(401);
    set_find_class(sym(401), cls).unwrap();
    make_instance(cls, &[]).unwrap();
    make_instance(cls, &[sym(1), BlissVal::from_fixnum(99)]).unwrap();
}

#[test]
fn initialize_and_shared_initialize() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(403);
    set_find_class(sym(403), cls).unwrap();
    let inst = allocate_instance(cls).unwrap();
    initialize_instance(inst, &[]).unwrap();
    shared_initialize(inst, T, &[]).unwrap();
}

#[test]
fn slot_set_get_roundtrip() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(500);
    set_find_class(sym(500), cls).unwrap();
    let inst = make_instance(cls, &[]).unwrap();
    let (sn, val) = (sym(501), BlissVal::from_fixnum(42));
    set_slot_value(inst, sn, val).unwrap();
    assert_eq!(slot_value(inst, sn).unwrap(), val);
}

#[test]
fn slot_boundp_and_makunbound() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(502);
    set_find_class(sym(502), cls).unwrap();
    let inst = allocate_instance(cls).unwrap();
    let sn = sym(503);
    assert!(!slot_boundp(inst, sn).unwrap());
    set_slot_value(inst, sn, BlissVal::from_fixnum(1)).unwrap();
    assert!(slot_boundp(inst, sn).unwrap());
    slot_makunbound(inst, sn).unwrap();
    assert!(!slot_boundp(inst, sn).unwrap());
}

#[test]
fn slot_value_unbound_errors() {
    bootstrap_clos().unwrap();
    let cls = BlissVal::from_fixnum(506);
    set_find_class(sym(506), cls).unwrap();
    assert!(slot_value(allocate_instance(cls).unwrap(), sym(507)).is_err());
}

#[test]
fn generic_function_lifecycle() {
    let gf = make_generic_function(sym(600), NIL).unwrap();
    let m = BlissVal::from_fixnum(1);
    add_method(gf, m).unwrap();
    remove_method(gf, m).unwrap();
}

#[test]
fn compute_applicable_methods_empty() {
    let gf = make_generic_function(sym(602), NIL).unwrap();
    assert!(compute_applicable_methods(gf, &[BlissVal::from_fixnum(1)]).is_empty());
}

#[test]
fn compute_effective_method_standard_and_empty() {
    let gf = make_generic_function(sym(603), NIL).unwrap();
    let m = BlissVal::from_fixnum(1);
    add_method(gf, m).unwrap();
    assert!(compute_effective_method(gf, MethodCombinationType::Standard, &[m]).is_ok());
    let gf2 = make_generic_function(sym(604), NIL).unwrap();
    assert!(compute_effective_method(gf2, MethodCombinationType::Standard, &[]).is_err());
}

#[test]
fn change_class_succeeds() {
    bootstrap_clos().unwrap();
    let old = BlissVal::from_fixnum(700);
    set_find_class(sym(700), old).unwrap();
    let inst = make_instance(old, &[]).unwrap();
    let new = BlissVal::from_fixnum(701);
    set_find_class(sym(701), new).unwrap();
    change_class(inst, new).unwrap();
}
