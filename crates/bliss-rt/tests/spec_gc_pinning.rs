//! Precise pinning + non-moving large-object policy (bliss-jtc.18): a pinned
//! object keeps its address across collections, references to it from moved
//! objects stay valid, and large objects are never copied.

use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, TAG_CONS, TAG_MASK};
use bliss_rt::{Allocator, BlissStack, Collector, GcConfig, current_thread, init_heap, pin};
use bliss_rt::{HeapAllocator, HeapCollector, full_gc, walk_heap};
use std::sync::{Mutex, OnceLock};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 4 * 1024 * 1024,
        tlab_size: 512,
        region_size: 512, // small regions so distinct objects land in distinct regions
        promotion_threshold: 0,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}

fn make_cons(alloc: &mut HeapAllocator, car: BlissVal, cdr: BlissVal) -> BlissVal {
    let body = alloc
        .alloc_fast(16)
        .or_else(|| alloc.alloc_slow(16).ok())
        .expect("cons alloc");
    unsafe {
        let hdr = body.sub(8) as *mut ObjectHeader;
        (*hdr).0 = ((*hdr).0 & 0x00FF_FFFF_FFFF_FFFF) | ((type_id::CONS as u64) << 56);
        *(body as *mut BlissVal) = car;
        *((body as *mut BlissVal).add(1)) = cdr;
    }
    BlissVal((body as u64) | TAG_CONS)
}

fn car(v: BlissVal) -> BlissVal {
    unsafe { *((v.0 & !TAG_MASK) as *const BlissVal) }
}
fn cdr(v: BlissVal) -> BlissVal {
    unsafe { *(((v.0 & !TAG_MASK) as *const BlissVal).add(1)) }
}
fn fill_region(alloc: &mut HeapAllocator) {
    for _ in 0..40 {
        let _ = make_cons(alloc, BlissVal::from_fixnum(0), NIL);
    }
}

#[test]
fn pinned_object_keeps_its_address_while_others_are_evacuated() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // P (pinned) and Q (not) in separate regions; both rooted.
    let p = make_cons(&mut alloc, BlissVal::from_fixnum(999), NIL);
    pin(p);
    fill_region(&mut alloc);
    let q = make_cons(&mut alloc, BlissVal::from_fixnum(111), NIL);

    let stack = current_thread().stack();
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 2, 0)
        .unwrap();
    unsafe {
        BlissStack::frame_slots_mut(f)[0] = p;
        BlissStack::frame_slots_mut(f)[1] = q;
    }
    let p_addr = p.0 & !TAG_MASK;

    HeapCollector::new().minor_gc().expect("minor_gc");

    let p_after = unsafe { BlissStack::frame_slots_mut(f)[0] };
    let q_after = unsafe { BlissStack::frame_slots_mut(f)[1] };
    assert_eq!(p_after.0 & !TAG_MASK, p_addr, "pinned object was NOT moved");
    assert_eq!(car(p_after).as_fixnum(), 999, "pinned contents intact");
    assert_ne!(
        q_after.0 & !TAG_MASK,
        q.0 & !TAG_MASK,
        "unpinned object WAS moved"
    );
    assert_eq!(
        car(q_after).as_fixnum(),
        111,
        "moved object contents intact"
    );

    stack.pop_frame();
}

#[test]
fn reference_to_a_pinned_object_from_a_moved_object_stays_valid() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // P pinned; O (in a later region) references P via its cdr and will move.
    let p = make_cons(&mut alloc, BlissVal::from_fixnum(555), NIL);
    pin(p);
    let p_addr = p.0 & !TAG_MASK;
    fill_region(&mut alloc);
    let o = make_cons(&mut alloc, BlissVal::from_fixnum(42), p);

    let stack = current_thread().stack();
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { BlissStack::frame_slots_mut(f)[0] = o };

    HeapCollector::new().minor_gc().expect("minor_gc");

    let o_after = unsafe { BlissStack::frame_slots_mut(f)[0] };
    assert_ne!(
        o_after.0 & !TAG_MASK,
        o.0 & !TAG_MASK,
        "referencing object moved"
    );
    // O's cdr still points at the un-moved pinned object, whose contents are intact.
    assert_eq!(
        cdr(o_after).0 & !TAG_MASK,
        p_addr,
        "cdr still points at pinned object"
    );
    assert_eq!(car(cdr(o_after)).as_fixnum(), 555, "pinned referent intact");

    stack.pop_frame();
}

#[test]
fn large_object_is_never_moved_across_repeated_major_collections() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // A large object (bigger than half a region) lands in a dedicated
    // non-moving large-object region.
    let large = alloc.alloc_large(2048).expect("large object");
    unsafe { *(large as *mut u64) = 0x00BA_DA55 };
    let large_addr = large as u64;

    // Churn the heap and force repeated major collections.
    for _ in 0..3 {
        for _ in 0..200 {
            let _ = make_cons(&mut alloc, BlissVal::from_fixnum(0), NIL);
        }
        full_gc().expect("full_gc");
    }

    // The large object is still at its original address, walkable, contents intact.
    assert_eq!(
        unsafe { *(large_addr as *const u64) },
        0x00BA_DA55,
        "large object intact"
    );
    let mut found = false;
    walk_heap(|ptr, _tid, _size| {
        if ptr as u64 == large_addr {
            found = true;
        }
        true
    })
    .expect("walk_heap");
    assert!(found, "large object still walkable at its original address");
}
