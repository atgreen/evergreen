//! Symbol registry × GC interaction (bliss-jtc.6 Stage A/C).
//!
//! These exercise `full_gc`, which is stop-the-world but *not* safe to run
//! concurrently with allocation from another thread (there are no safepoints
//! yet). Like the other GC integration tests in this crate, every test here
//! takes a process-global lock so they run one at a time and no parallel test in
//! this binary allocates underneath a collection.

use std::sync::{Mutex, OnceLock};
use torcl_rt::object::type_id;
use torcl_rt::value::{NIL, TorclVal};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

#[test]
fn interned_symbol_object_lives_on_the_gc_heap() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    torcl_rt::symbols::intern("GC-ROOTS-ON-HEAP");
    let mut saw_symbol = false;
    torcl_rt::walk_heap(|_p, tid, _s| {
        if tid == type_id::SYMBOL {
            saw_symbol = true;
        }
        true
    })
    .expect("walk_heap");
    assert!(
        saw_symbol,
        "interned symbols must be SYMBOL objects on the GC heap"
    );
}

#[test]
fn value_reachable_only_through_a_symbol_cell_survives_gc() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    // A cons stored ONLY in a symbol's value cell — no other root anywhere.
    let idx = torcl_rt::symbols::intern("GC-ROOTS-CELL-ROOT");
    let marker = TorclVal::from_fixnum(0x00C0_FFEE);
    let cons_body = torcl_rt::alloc_typed(16, type_id::CONS).expect("alloc cons");
    // SAFETY: fresh 16-byte cons body [car | cdr].
    let cons = unsafe {
        *(cons_body as *mut TorclVal) = marker;
        *(cons_body as *mut TorclVal).add(1) = NIL;
        TorclVal::from_cons_ptr(cons_body)
    };
    torcl_rt::symbols::set_symbol_value(idx, cons);

    // Push it through several generations: heavy churn + repeated collection
    // promotes the cons to old-gen and drives major evacuation/sweep. Without the
    // symbol cell acting as a root it would be swept (mark) or its cell left
    // dangling (relocate); either way the marker would be lost.
    for cycle in 0..3 {
        for _ in 0..300 {
            let _ = torcl_rt::alloc_typed(16, type_id::CONS);
        }
        torcl_rt::full_gc().unwrap_or_else(|_| panic!("full_gc cycle {cycle}"));
        let cell = torcl_rt::symbols::symbol_value(idx).expect("value cell present");
        // SAFETY: a live cons value points at its body; car is the first word.
        let car = unsafe { *(cell.as_ptr() as *const TorclVal) };
        assert_eq!(
            car, marker,
            "cons reachable only via the symbol cell survived GC cycle {cycle} intact"
        );
    }
}

#[test]
fn pinned_symbols_survive_a_full_gc_with_cells_intact() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let idx = torcl_rt::symbols::intern("GC-ROOTS-SURVIVOR");
    torcl_rt::symbols::set_symbol_value(idx, TorclVal::from_fixnum(7));
    // Churn + collect: pinned symbols and their name strings must persist.
    for _ in 0..200 {
        let _ = torcl_rt::alloc_typed(16, type_id::CONS);
    }
    torcl_rt::full_gc().expect("full_gc");
    assert_eq!(
        torcl_rt::symbols::symbol_name(idx).as_deref(),
        Some("GC-ROOTS-SURVIVOR")
    );
    assert_eq!(
        torcl_rt::symbols::symbol_value(idx),
        Some(TorclVal::from_fixnum(7))
    );
}
