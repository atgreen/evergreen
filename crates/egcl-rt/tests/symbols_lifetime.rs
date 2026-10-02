// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::{EgclVal, NIL};

struct RegisteredWeak(Box<egcl_rt::gc::WeakPointer>);

impl RegisteredWeak {
    fn new(value: EgclVal) -> Self {
        let mut pointer = Box::new(egcl_rt::gc::WeakPointer::new(value));
        egcl_rt::gc::register_weak_pointer(&mut pointer);
        Self(pointer)
    }
}

impl Drop for RegisteredWeak {
    fn drop(&mut self) {
        egcl_rt::gc::unregister_weak_pointer(&self.0);
    }
}

#[test]
fn uninterned_symbols_follow_reachability_instead_of_registry_membership() {
    egcl_rt::rooted!(held = egcl_rt::symbols::make_uninterned("HELD"));
    let held_index = held.as_symbol_index();
    let peer = egcl_rt::symbols::make_uninterned("PEER");
    let peer_index = peer.as_symbol_index();
    egcl_rt::symbols::set_symbol_value(held_index, peer);
    egcl_rt::symbols::set_symbol_value(peer_index, *held);
    let dead = egcl_rt::symbols::make_uninterned("DEAD-CYCLE");
    let dead_index = dead.as_symbol_index();
    egcl_rt::symbols::set_symbol_value(dead_index, dead);
    let weak_held = RegisteredWeak::new(*held);
    let weak_dead = RegisteredWeak::new(dead);
    let finalizer = EgclVal::from_fixnum(17);
    egcl_rt::gc::register_deferred_finalizer(
        egcl_rt::gc::finalizer_key(*held).expect("symbol finalizer key"),
        finalizer,
    )
    .unwrap();
    egcl_rt::gc::full_gc().unwrap();
    assert!(egcl_rt::gc::take_deferred_finalizers().is_empty());
    assert_eq!(weak_dead.0.value(), (NIL, true));
    assert_eq!(weak_held.0.value(), (*held, false));
    assert!(
        egcl_rt::symbols::symbol_name(dead_index).is_none(),
        "dead cycle remains registered"
    );
    assert_eq!(
        egcl_rt::symbols::symbol_name(held_index).as_deref(),
        Some("HELD")
    );
    assert_eq!(egcl_rt::symbols::symbol_value(peer_index), Some(*held));
    assert_eq!(
        egcl_rt::symbols::symbol_value(held_index),
        Some(EgclVal::from_symbol_index(peer_index))
    );
    *held = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(weak_held.0.value(), (NIL, true));
    assert_eq!(egcl_rt::gc::take_deferred_finalizers(), vec![finalizer]);
    assert!(egcl_rt::symbols::symbol_name(held_index).is_none());
    assert!(egcl_rt::symbols::symbol_name(peer_index).is_none());
}
