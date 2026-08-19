//! bliss-jtc.7f: GC finalizer-registry keys and weak-pointer referents must
//! survive object evacuation. A live object that is promoted/evacuated by a
//! collection must NOT have its finalizer fired (its key is forwarded to the new
//! address) and its weak pointers must be updated rather than broken; only when
//! the object actually dies does its finalizer fire (exactly once) and its weak
//! pointers break.
//!
//! Mirrors SBCL's post-mark `scan_finalizers` / `smash_weak_pointers` discipline.
//!
//! The probe is held through an external root for the first collection, then the
//! root is cleared before the second collection. This exercises both forwarding
//! and death without relying on unreachable nursery garbage being promoted.

use bliss_rt::gc::{
    alloc_typed, init_heap, register_finalizer, register_root_scanner, register_weak_pointer,
    set_finalizer_dispatch, walk_heap, Collector, GcConfig, HeapCollector, WeakPointer,
};
use bliss_rt::value::{BlissVal, NIL};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, Once, OnceLock};

/// Serialize every test in this file: they all mutate the process-global heap.
fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

static FIRED: AtomicUsize = AtomicUsize::new(0);
static LAST_OBJECT: AtomicUsize = AtomicUsize::new(0);
static ROOT: Mutex<BlissVal> = Mutex::new(NIL);
static INSTALL_ROOT_SCANNER: Once = Once::new();

fn scan_test_root(visit: &mut dyn FnMut(*mut BlissVal)) {
    let mut root = ROOT.lock().unwrap_or_else(|e| e.into_inner());
    visit(&mut *root);
}

fn set_test_root(value: BlissVal) {
    INSTALL_ROOT_SCANNER.call_once(|| register_root_scanner(scan_test_root));
    *ROOT.lock().unwrap_or_else(|e| e.into_inner()) = value;
}

fn dispatch(_finalizer: BlissVal, object: BlissVal) {
    FIRED.fetch_add(1, Ordering::SeqCst);
    LAST_OBJECT.store(object.to_raw() as usize, Ordering::SeqCst);
}

fn cfg() -> GcConfig {
    GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 512,
        region_size: 4096,
        promotion_threshold: 0,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}

const PROBE_TID: u8 = 0x60;

/// Body address of the single live PROBE_TID object, or None if collected.
fn find_probe() -> Option<usize> {
    let mut found = None;
    walk_heap(|ptr, type_id, _size| {
        if type_id == PROBE_TID {
            found = Some(ptr as usize);
        }
        true
    })
    .expect("walk_heap");
    found
}

#[test]
fn finalizer_survives_evacuation_and_fires_once_on_death() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&cfg()).expect("init_heap");
    set_finalizer_dispatch(dispatch);
    FIRED.store(0, Ordering::SeqCst);

    let body = alloc_typed(16, PROBE_TID).expect("alloc_typed");
    let orig_key = BlissVal::from_raw(body as u64);
    register_finalizer(orig_key, BlissVal::from_fixnum(7)).expect("register_finalizer");
    set_test_root(unsafe { BlissVal::from_heap_ptr(body.sub(8)) });

    // Collection #1: minor GC promotes (evacuates) the object out of the nursery.
    // Its finalizer MUST NOT fire — it survived — and the registry key must be
    // forwarded to the new address.
    let mut collector = HeapCollector::new();
    collector.minor_gc().expect("minor_gc");

    assert_eq!(
        FIRED.load(Ordering::SeqCst),
        0,
        "finalizer must NOT fire for an object that survived (was promoted)"
    );
    let new_addr = find_probe().expect("promoted object must still be live");
    assert_ne!(
        new_addr, body as usize,
        "object should have been evacuated to a new address"
    );

    // Collection #2: major GC. Clear the only strong root; the object is now dead and its
    // finalizer fires exactly once, keyed on the forwarded (new) address.
    set_test_root(NIL);
    collector.major_gc().expect("major_gc");

    assert_eq!(
        FIRED.load(Ordering::SeqCst),
        1,
        "finalizer must fire exactly once when the survivor finally dies"
    );
    assert_eq!(
        LAST_OBJECT.load(Ordering::SeqCst),
        new_addr,
        "finalizer must fire keyed on the forwarded address, not the stale one"
    );
    assert!(find_probe().is_none(), "object must be collected after death");

    // Collection #3: no re-fire (the entry was removed on firing).
    collector.major_gc().expect("major_gc #3");
    assert_eq!(
        FIRED.load(Ordering::SeqCst),
        1,
        "finalizer must not re-fire on a later collection"
    );
}

#[test]
fn weak_pointer_forwards_across_evacuation_then_breaks_on_death() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&cfg()).expect("init_heap");

    let body = alloc_typed(16, PROBE_TID).expect("alloc_typed");
    let orig = BlissVal::from_raw(body as u64);
    let mut weak = WeakPointer::new(orig);
    register_weak_pointer(&mut weak);
    set_test_root(unsafe { BlissVal::from_heap_ptr(body.sub(8)) });

    // Collection #1: the object is promoted (survives). The weak pointer must be
    // updated to the new address, NOT broken.
    let mut collector = HeapCollector::new();
    collector.minor_gc().expect("minor_gc");

    let new_addr = find_probe().expect("promoted object must still be live");
    let (val, broken) = weak.value();
    assert!(!broken, "weak pointer to a survivor must not break");
    assert_eq!(
        val.to_raw() as usize,
        new_addr,
        "weak pointer must be forwarded to the object's new address"
    );

    // Collection #2: remove the strong root; the object is now dead → the weak pointer breaks.
    set_test_root(NIL);
    collector.major_gc().expect("major_gc");
    let (_, broken_after) = weak.value();
    assert!(broken_after, "weak pointer must break once the object dies");
}
