//! Hash-table storage is traced by the runtime GC (bliss-jtc.8).
//!
//! A hash table's entries live in a Rust Vec outside the GC heap; the stdlib
//! registers a GC root scanner so their keys/values are marked and relocated.
//! These tests exercise `full_gc`, so — like the other GC tests in the tree —
//! they serialize on a process-global lock and live in their own binary.

use bliss_rt::object::type_id;
use bliss_rt::value::{BlissVal, NIL};
use bliss_stdlib::{gethash, make_hash_table, set_gethash, MakeHashTableOptions};
use std::sync::{Mutex, OnceLock};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

#[test]
fn value_reachable_only_through_a_hash_table_survives_gc() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let table = make_hash_table(&MakeHashTableOptions::default()).expect("make-hash-table");

    // A heap cons stored ONLY as a hash-table value — no other root.
    let key = BlissVal::from_fixnum(7);
    let marker = BlissVal::from_fixnum(0x00BE_EF00);
    let cons_body = bliss_rt::alloc_typed(16, type_id::CONS).expect("alloc cons");
    // SAFETY: fresh 16-byte cons body [car | cdr].
    let cons = unsafe {
        *(cons_body as *mut BlissVal) = marker;
        *(cons_body as *mut BlissVal).add(1) = NIL;
        BlissVal::from_cons_ptr(cons_body)
    };
    set_gethash(key, table, cons).expect("setf gethash");

    // Churn + collect: without the root scanner the value would be swept (mark)
    // or its slot left dangling (relocate) — the marker would be lost.
    for _ in 0..300 {
        let _ = bliss_rt::alloc_typed(16, type_id::CONS);
    }
    bliss_rt::full_gc().expect("full_gc");

    let (v, present) = gethash(key, table, NIL).expect("gethash");
    assert!(present, "key must still be present after GC");
    // SAFETY: a live cons value points at its body; car is the first word. The
    // slot was relocated to the value's new address by the root scanner.
    let car = unsafe { *(v.as_ptr() as *const BlissVal) };
    assert_eq!(
        car, marker,
        "hash-table value survived GC intact via the external root scanner"
    );
}
