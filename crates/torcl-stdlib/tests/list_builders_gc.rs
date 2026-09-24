//! GC visibility regressions for stdlib list builders.
//!
//! These builders return live Lisp lists. Their cons cells must live on the
//! managed GC heap so the collector can trace car/cdr fields and relocate
//! referenced nursery objects.

use std::sync::{Mutex, OnceLock};
use torcl_rt::gc::{GcConfig, init_heap, walk_heap};
use torcl_rt::object::{ConsCell, type_id};
use torcl_rt::value::{NIL, TorclVal};
use torcl_stdlib::sequences;

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 256,
        region_size: 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

fn init_test_heap() {
    init_heap(&gc_config()).expect("init_heap");
}

fn marker_object(marker: u64) -> TorclVal {
    let body = torcl_rt::alloc_typed(8, type_id::BIGNUM).expect("alloc marker");
    unsafe {
        *(body as *mut u64) = marker;
        TorclVal::from_heap_ptr(body.sub(8))
    }
}

fn leaked_test_list(vals: &[TorclVal]) -> TorclVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell { car: v, cdr: list }));
        list = unsafe { TorclVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };
    }
    list
}

fn heap_contains_bignum_marker(marker: u64) -> bool {
    let mut found = false;
    walk_heap(|body, tid, size| {
        if tid == type_id::BIGNUM && size >= 8 {
            let value = unsafe { *(body as *const u64) };
            found |= value == marker;
        }
        true
    })
    .expect("walk_heap");
    found
}

#[test]
fn sequence_list_result_traces_heap_elements_after_full_gc() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_test_heap();

    let marker = 0x51E0_0001;
    let source = leaked_test_list(&[marker_object(marker)]);

    torcl_rt::rooted!(result = sequences::copy_seq(source).expect("copy-seq"));
    torcl_rt::full_gc().expect("full_gc");

    assert!(result.is_cons(), "copy-seq of a list must return a list");
    assert!(
        heap_contains_bignum_marker(marker),
        "heap element must remain reachable through the sequence result list"
    );
}

#[test]
fn append_copies_heads_and_keeps_shared_tail_and_elements_alive() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_test_heap();

    let tail_marker = 0x51E0_0002;
    let head_marker = 0x51E0_0003;
    torcl_rt::rooted!(
        tail = sequences::copy_seq(leaked_test_list(&[marker_object(tail_marker)])).expect("tail")
    );
    let head = leaked_test_list(&[marker_object(head_marker)]);
    torcl_rt::rooted!(result = sequences::append(&[head, NIL, *tail]).expect("append"));
    assert_ne!(*result, head, "non-final list spine must be copied");
    torcl_rt::full_gc().expect("full_gc");
    let result_cell = unsafe { &*(result.as_ptr() as *const ConsCell) };
    assert_eq!(
        result_cell.cdr, *tail,
        "final tail must be shared unchanged"
    );
    assert!(heap_contains_bignum_marker(head_marker));
    assert!(heap_contains_bignum_marker(tail_marker));

    assert_eq!(sequences::append(&[]).unwrap(), NIL);
    let atom = TorclVal::from_fixnum(42);
    assert_eq!(sequences::append(&[NIL, atom]).unwrap(), atom);
    assert_eq!(sequences::append(&[*tail]).unwrap(), *tail);
    assert!(sequences::append(&[atom, NIL]).is_err());
}
