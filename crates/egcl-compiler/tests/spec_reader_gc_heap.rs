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

/// Reading a character from a user-defined stream may collect. In particular,
/// a dotted tail and a complex imaginary part remain live while the parser
/// requests the whitespace/closing delimiter that follows them.
#[test]
fn streaming_reader_roots_values_across_character_callbacks() {
    use egcl_compiler::reader::{ReaderMacroKind, ReaderStream, read_from_stream};
    use egcl_rt::{error::EgclError, object::ComplexData};

    struct CollectingInput {
        chars: Vec<char>,
        position: usize,
        collections: usize,
    }
    impl ReaderStream for CollectingInput {
        fn read_char(&mut self) -> Result<Option<char>, EgclError> {
            HeapCollector::new()
                .minor_gc()
                .expect("callback collection");
            self.collections += 1;
            let ch = self.chars.get(self.position).copied();
            if ch.is_some() {
                self.position += 1;
            }
            Ok(ch)
        }
        fn unread_char(&mut self, ch: char) -> Result<(), EgclError> {
            self.position -= 1;
            assert_eq!(self.chars[self.position], ch);
            Ok(())
        }
        fn invoke_macro(
            &mut self,
            _: EgclVal,
            _: char,
            _: ReaderMacroKind,
        ) -> Result<Vec<EgclVal>, EgclError> {
            panic!("no custom macros in this test")
        }
    }
    let _g = gc_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _thread = current_thread();
    let mut input = CollectingInput {
        chars: "((1 . (2 3) ) #c(1.25d0 2.5d0 ))".chars().collect(),
        position: 0,
        collections: 0,
    };
    egcl_rt::rooted!(form = read_from_stream(&mut input, 10, false, false).unwrap());
    assert_eq!(input.collections, input.chars.len());
    let list = car(*form);
    assert_eq!(car(list).as_fixnum(), 1);
    assert_eq!(car(cdr(list)).as_fixnum(), 2);
    assert_eq!(car(cdr(cdr(list))).as_fixnum(), 3);
    assert!(cdr(cdr(cdr(list))).is_nil());
    let complex = car(cdr(*form));
    let parts = unsafe { &*(complex.as_ptr() as *const ComplexData) };
    assert_eq!(parts.realpart.as_double_float(), 1.25);
    assert_eq!(parts.imagpart.as_double_float(), 2.5);
}
