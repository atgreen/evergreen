// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{BigInt, bigint_from_val};
use egcl_rt::value::EgclVal;
use egcl_stdlib::clos::*;

#[test]
fn class_change_snapshot_keeps_relocated_slot_values() {
    // This binary has one test; set the allocation policy before heap startup.
    unsafe {
        std::env::set_var("EGCL_GC_STRESS", "1");
        std::env::set_var("EGCL_GC_POISON", "1");
    }
    bootstrap_clos().unwrap();
    let class = EgclVal::from_fixnum(1400);
    let slot = EgclVal::from_symbol_index(1401);
    let unbound = EgclVal::from_symbol_index(1402);
    define_class(
        EgclVal::from_symbol_index(1403),
        class,
        &[],
        &[slot, unbound],
    )
    .unwrap();
    egcl_rt::rooted!(instance = allocate_instance(class).unwrap());
    egcl_rt::rooted!(value = BigInt::from_i64(i64::MAX).to_val());
    set_slot_value(*instance, slot, *value).unwrap();
    let before = value.to_raw();
    egcl_rt::rooted!(copy = copy_instance_for_class_change(*instance).unwrap());
    assert_ne!(
        before,
        value.to_raw(),
        "the copied slot value must actually relocate"
    );
    assert_ne!(*instance, *copy);
    assert_eq!(class_of(*copy), class);
    assert_eq!(slot_value(*copy, slot).unwrap(), *value);
    assert!(
        bigint_from_val(slot_value(*copy, slot).unwrap()).unwrap()
            == BigInt::from_i64(i64::MAX)
    );
    assert!(!slot_boundp(*copy, unbound).unwrap());
    set_slot_value(*instance, slot, EgclVal::from_fixnum(7)).unwrap();
    assert_eq!(slot_value(*copy, slot).unwrap(), *value);
}
