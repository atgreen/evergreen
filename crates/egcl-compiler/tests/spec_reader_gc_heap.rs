// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reader objects live on the shared GC heap and survive collection with their
//! structure and identity intact (bliss-jtc.15).

use std::sync::{Mutex, OnceLock};
use egcl_compiler::read_from_string;
use egcl_rt::value::{TAG_MASK, EgclVal};
use egcl_rt::{Collector, HeapCollector, EgclStack, current_thread};

/// Serialize the GC-forcing tests: they collect the shared per-process heap, so
/// they must not run while another such test holds unrooted young objects.
fn gc_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn car(v: EgclVal) -> EgclVal {
    unsafe { *((v.0 & !TAG_MASK) as *const EgclVal) }
}
fn cdr(v: EgclVal) -> EgclVal {
    unsafe { *(((v.0 & !TAG_MASK) as *const EgclVal).add(1)) }
}

/// A list read by the reader is allocated on the GC heap; after forcing a
/// collection (which evacuates the cons cells) the rooted list still reads back
/// with the same structure and values — every internal cdr link and the root
/// reference were relocated to the moved cells.
#[test]
fn reader_list_survives_gc_and_retains_structure() {
    let _g = gc_lock().lock().unwrap_or_else(|e| e.into_inner());
    let (list, _) = read_from_string("(100 200 300)").expect("read");

    // Root the list head on the current thread's CL stack.
    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(f)[0] = list };

    // Force a minor GC: the cons cells are evacuated and every reference — the
    // frame root and the internal cdr links — must be relocated (jtc.17).
    HeapCollector::new().minor_gc().expect("minor_gc");

    let relocated = unsafe { EgclStack::frame_slots_mut(f)[0] };
    assert_ne!(relocated.0, list.0, "the list head was evacuated (moved)");

    // The relocated list must still read as (100 200 300) — structure preserved.
    assert_eq!(car(relocated).as_fixnum(), 100);
    assert_eq!(car(cdr(relocated)).as_fixnum(), 200);
    assert_eq!(car(cdr(cdr(relocated))).as_fixnum(), 300);
    assert!(cdr(cdr(cdr(relocated))).is_nil(), "list terminates in NIL");

    stack.pop_frame();
}

/// A shared substructure read with #n= / #n# keeps its identity across a GC:
/// both references still point at the *same* (relocated) cons cell.
#[test]
fn reader_shared_structure_retains_identity_across_gc() {
    let _g = gc_lock().lock().unwrap_or_else(|e| e.into_inner());
    // ((#1=(a) . #1#)) — the car and cdr of the inner cons are the same object.
    let (form, _) = read_from_string("(#1=(a) #1#)").expect("read");

    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(f)[0] = form };

    HeapCollector::new().minor_gc().expect("minor_gc");

    let relocated = unsafe { EgclStack::frame_slots_mut(f)[0] };
    // form = (X X) where both elements are the same object #1=(a).
    let first = car(relocated);
    let second = car(cdr(relocated));
    assert_eq!(
        first.0, second.0,
        "shared object identity preserved across GC"
    );
    // And it is a live, relocated cons (tag preserved) reachable through the root.
    assert_eq!(
        first.0 & TAG_MASK,
        egcl_rt::value::TAG_CONS,
        "shared object is a live cons"
    );

    stack.pop_frame();
}
