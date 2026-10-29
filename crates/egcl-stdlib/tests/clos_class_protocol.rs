// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclVal, gc, object::type_id, value::NIL};
use egcl_stdlib::clos::*;

fn symbol(name: &str) -> EgclVal {
    EgclVal::from_symbol_index(egcl_rt::symbols::intern(name))
}

fn marker(value: i64) -> EgclVal {
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(value));
        body.cast::<EgclVal>().add(1).write(NIL);
        EgclVal::from_cons_ptr(body)
    }
}

fn raw_wrapper(value: EgclVal) -> EgclVal {
    egcl_rt::rooted!(value = value);
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(*value);
        body.cast::<EgclVal>().add(1).write(NIL);
        EgclVal::from_cons_ptr(body)
    }
}

fn value(list: EgclVal) -> i64 {
    assert!(list.is_cons());
    unsafe { list.as_ptr().cast::<EgclVal>().read().as_fixnum() }
}

#[test]
fn protocol_roots_move_survive_images_and_publish_only_validated_layouts() {
    bootstrap_clos().unwrap();
    let class = symbol("PROTOCOL-CACHE-CLASS");
    let kept = symbol("PROTOCOL-CACHE-KEPT");
    let replacement = symbol("PROTOCOL-CACHE-REPLACEMENT");
    define_class(class, class, &[], &[kept]).unwrap();
    egcl_rt::rooted!(instance = allocate_instance(class).unwrap());
    set_slot_value(*instance, kept, EgclVal::from_fixnum(71)).unwrap();
    let before_layout = instance_layout_key(*instance).unwrap();
    stage_class_with_shared_slots(class, class, &[], &[replacement], &[]).unwrap();
    assert!(!class_protocol_finalized(class));
    assert_eq!(class_layout_key(class), Some(before_layout));
    assert_eq!(instance_layout_key(*instance), Some(before_layout));
    assert_eq!(
        slot_value(*instance, kept).unwrap(),
        EgclVal::from_fixnum(71)
    );
    // Rejected computation cannot publish a new stamp or migrate the instance.
    assert!(
        publish_class_protocol(class, &[class, class], &[replacement], NIL, NIL, NIL, NIL).is_err()
    );
    assert_eq!(instance_layout_key(*instance), Some(before_layout));
    stage_class_with_shared_slots(class, class, &[], &[replacement, kept], &[]).unwrap();
    let cpl = compute_class_precedence_list_default(class).unwrap();
    publish_class_protocol(class, &cpl, &[replacement, kept], NIL, NIL, NIL, NIL).unwrap();
    assert!(class_protocol_finalized(class));
    assert_ne!(class_layout_key(class), Some(before_layout));
    assert_eq!(
        slot_value(*instance, kept).unwrap(),
        EgclVal::from_fixnum(71)
    );

    // Drain all setup/pinned allocations before creating fresh cache values.
    gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(direct = marker(41));
    egcl_rt::rooted!(effective = marker(42));
    egcl_rt::rooted!(defaults = marker(43));
    egcl_rt::rooted!(runtime_defaults = marker(44));
    egcl_rt::rooted!(runtime_slots = marker(45));
    set_class_direct_slot_metaobjects(class, *direct).unwrap();
    publish_class_protocol(
        class,
        &cpl,
        &[replacement, kept],
        *effective,
        *defaults,
        *runtime_defaults,
        *runtime_slots,
    )
    .unwrap();
    let before = [
        direct.to_raw(),
        effective.to_raw(),
        defaults.to_raw(),
        runtime_defaults.to_raw(),
        runtime_slots.to_raw(),
    ];
    gc::collect_t0_minor().unwrap();
    let actual = [
        class_slot_metaobjects(class, true).0,
        class_slot_metaobjects(class, false).0,
        class_protocol_default_initargs(class).unwrap(),
        class_protocol_runtime_defaults(class).unwrap(),
        class_protocol_runtime_slots(class).unwrap(),
    ];
    for (i, item) in actual.into_iter().enumerate() {
        assert_ne!(
            item.to_raw(),
            before[i],
            "cache root {i} must actually relocate"
        );
        assert_eq!(value(item), 41 + i as i64);
    }
    let bytes = serialize_clos_state();
    assert_eq!(restore_clos_state(&bytes, &|raw| raw).unwrap(), bytes.len());
    assert!(class_protocol_finalized(class));
    assert_eq!(class_slot_metaobjects(class, true), (*direct, true));
    assert_eq!(class_slot_metaobjects(class, false), (*effective, true));
    assert_eq!(class_protocol_default_initargs(class), Some(*defaults));
    assert_eq!(
        class_protocol_runtime_defaults(class),
        Some(*runtime_defaults)
    );
    assert_eq!(class_protocol_runtime_slots(class), Some(*runtime_slots));
    assert_eq!(compute_class_precedence_list(class).unwrap(), cpl);

    // Raw introspection is cached in the existing image/root field without
    // publishing protocol runtime slots or changing the already valid layout.
    let raw_class = symbol("RAW-PROTOCOL-CACHE-CLASS");
    define_class(raw_class, raw_class, &[], &[kept]).unwrap();
    egcl_rt::rooted!(raw_instance = allocate_instance(raw_class).unwrap());
    set_slot_value(*raw_instance, kept, EgclVal::from_fixnum(82)).unwrap();
    let layout = class_layout_key(raw_class);
    gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(raw_value = marker(61));
    egcl_rt::rooted!(raw_cache = raw_wrapper(*raw_value));
    assert!(set_class_raw_effective_slot_metaobjects(class, *raw_cache).is_err());
    set_class_raw_effective_slot_metaobjects(raw_class, *raw_cache).unwrap();
    let before_value = *raw_value;
    let before_cache = *raw_cache;
    gc::collect_t0_minor().unwrap();
    assert_ne!(*raw_value, before_value, "raw slot metadata must move");
    assert_ne!(*raw_cache, before_cache, "raw cache wrapper must move");
    assert_eq!(class_slot_metaobjects(raw_class, false), (*raw_value, true));
    assert_eq!(class_protocol_runtime_slots(raw_class), None);
    assert_eq!(class_protocol_default_initargs(raw_class), None);
    assert_eq!(class_layout_key(raw_class), layout);
    let bytes = serialize_clos_state();
    restore_clos_state(&bytes, &|raw| raw).unwrap();
    assert_eq!(class_slot_metaobjects(raw_class, false), (*raw_value, true));
    assert_eq!(class_protocol_runtime_slots(raw_class), None);
    assert_eq!(value(class_slot_metaobjects(raw_class, false).0), 61);
    assert_eq!(
        slot_value(*raw_instance, kept).unwrap(),
        EgclVal::from_fixnum(82)
    );
    define_class(raw_class, raw_class, &[], &[kept]).unwrap();
    assert_eq!(class_slot_metaobjects(raw_class, false), (NIL, false));
    egcl_rt::rooted!(empty_cache = raw_wrapper(NIL));
    set_class_raw_effective_slot_metaobjects(raw_class, *empty_cache).unwrap();
    assert_eq!(class_slot_metaobjects(raw_class, false), (NIL, true));
    let bytes = serialize_clos_state();
    restore_clos_state(&bytes, &|raw| raw).unwrap();
    assert_eq!(class_slot_metaobjects(raw_class, false), (NIL, true));
    assert_eq!(class_protocol_runtime_slots(raw_class), None);
}
