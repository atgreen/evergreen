//! Heap-resident interpreted function objects with per-function tiering metadata
//! (bliss-jtc.6.8; D1.17 + FnMeta §4.4.3).
//!
//! A function is a real heap object reachable through a symbol's function cell,
//! carrying its lambda list / body / captured env / name *and* the `FnMeta`
//! tiering substrate (invocation + back-edge counters, active entry point,
//! current tier, flags). This is what a HotSpot-style engine needs a stable
//! object for: profiling counters, entry-point patching on tier change, code-
//! cache ownership, deopt/tracing/debug identity, and precise GC roots — none of
//! which a name→struct map can provide.
//!
//! Function objects are **pinned and identity-stable**: redefinition updates the
//! existing object's fields in place (resetting tiering) rather than replacing
//! the object, because inline caches, tracing, and deopt policy key off object
//! identity.

use crate::object::{type_id, FunctionData, ObjectHeader};
use crate::value::BlissVal;
use core::sync::atomic::{AtomicPtr, Ordering};

/// FnMeta flag: function is queued for T2 optimising compilation.
pub const FLAG_QUEUED_FOR_T2: u16 = 1 << 0;
/// FnMeta flag: a T2 compilation attempt failed.
pub const FLAG_T2_FAILED: u16 = 1 << 1;
/// FnMeta flag: never attempt to compile (unsupported construct).
pub const FLAG_NEVER_COMPILE: u16 = 1 << 2;

fn body_size() -> usize {
    std::mem::size_of::<FunctionData>() - std::mem::size_of::<ObjectHeader>()
}

fn header_size() -> usize {
    std::mem::size_of::<ObjectHeader>()
}

/// True if `v` is an interpreted function heap object created here.
pub fn is_interpreted_function(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    // SAFETY: heap-tagged values point at an ObjectHeader we can read.
    unsafe {
        let header = &*(v.as_ptr() as *const ObjectHeader);
        header.type_id() == type_id::FUNCTION_INTERPRETED
    }
}

/// # Safety
/// `f` must be an interpreted-function object (see `is_interpreted_function`).
unsafe fn data(f: BlissVal) -> *mut FunctionData {
    // SAFETY: caller guarantees `f` is a live interpreted-function object.
    unsafe { f.as_ptr() as *mut FunctionData }
}

/// Allocate a pinned interpreted function object with all cells set and the
/// tiering metadata zeroed (tier 0, no entry, counters 0).
pub fn alloc_interpreted(
    lambda_list: BlissVal,
    body: BlissVal,
    env: BlissVal,
    name: BlissVal,
) -> BlissVal {
    crate::rooted!(lambda_list = lambda_list);
    crate::rooted!(body = body);
    crate::rooted!(env = env);
    crate::rooted!(name = name);
    let body_ptr = crate::gc::alloc_typed(body_size(), type_id::FUNCTION_INTERPRETED)
        .expect("OOM allocating interpreted function object");
    // SAFETY: fresh FUNCTION_INTERPRETED body; header precedes it and its start
    // coincides with FunctionData's first field.
    unsafe {
        let header = body_ptr.sub(header_size());
        let d = header as *mut FunctionData;
        (*d).lambda_list = *lambda_list;
        (*d).body = *body;
        (*d).env = *env;
        (*d).name = *name;
        (*d).invoke_count = 0.into();
        (*d).back_edge_count = 0.into();
        (*d).entry = AtomicPtr::new(std::ptr::null_mut());
        (*d).tier = 0.into();
        (*d).flags = 0.into();
        let v = BlissVal::from_heap_ptr(header);
        crate::gc::pin(v);
        v
    }
}

/// Redefine `f` in place: update its lambda list / body / env and reset tiering
/// (the code changed, so old counters, tier, and entry are invalid). Preserves
/// object identity so existing references, caches, and traces stay valid.
///
/// # Safety
/// `f` must be an interpreted-function object.
pub unsafe fn redefine(f: BlissVal, lambda_list: BlissVal, body: BlissVal, env: BlissVal) {
    // SAFETY: caller guarantees `f` is a live interpreted-function object.
    unsafe {
        let d = data(f);
        (*d).lambda_list = lambda_list;
        (*d).body = body;
        (*d).env = env;
        (*d).invoke_count.store(0, Ordering::Relaxed);
        (*d).back_edge_count.store(0, Ordering::Relaxed);
        (*d).entry.store(std::ptr::null_mut(), Ordering::Release);
        (*d).tier.store(0, Ordering::Release);
        (*d).flags.store(0, Ordering::Release);
    }
}

/// The parsed lambda list cell.
pub fn lambda_list(f: BlissVal) -> BlissVal {
    // SAFETY: interpreted-function object by contract of its callers.
    unsafe { (*data(f)).lambda_list }
}

/// The body cons-tree cell.
pub fn body(f: BlissVal) -> BlissVal {
    unsafe { (*data(f)).body }
}

/// The captured lexical environment cell (NIL for a global defun).
pub fn env(f: BlissVal) -> BlissVal {
    unsafe { (*data(f)).env }
}

/// The function name cell.
pub fn name(f: BlissVal) -> BlissVal {
    unsafe { (*data(f)).name }
}

/// Record an invocation (T0/T1 prologue): atomically bump and return the new
/// invocation count. Lock-free (R4.53).
pub fn record_invocation(f: BlissVal) -> u32 {
    unsafe { (*data(f)).invoke_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// The current invocation count.
pub fn invoke_count(f: BlissVal) -> u32 {
    unsafe { (*data(f)).invoke_count.load(Ordering::Relaxed) }
}

/// Record a loop back-edge; returns the new count.
pub fn record_back_edge(f: BlissVal) -> u32 {
    unsafe { (*data(f)).back_edge_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// Add a sampled batch of back-edges. Native tiers poll only periodically, so
/// charging the batch keeps the shared hotness counter representative without
/// a runtime call on every loop iteration.
pub fn record_back_edges(f: BlissVal, count: u32) -> u32 {
    unsafe {
        (*data(f))
            .back_edge_count
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count)
    }
}

/// The current loop back-edge count — the hot-loop profiling signal read by the
/// tier scheduler alongside `invoke_count` (bliss-jtc.10).
pub fn back_edge_count(f: BlissVal) -> u32 {
    unsafe { (*data(f)).back_edge_count.load(Ordering::Relaxed) }
}

/// Reset the loop back-edge count to zero. Used as OSR-deopt backoff (bliss-izt.2):
/// after a speculating OSR loop deoptimizes back to T0, clearing the counter means
/// the loop must run another full threshold of interpreted iterations before it is
/// re-promoted, bounding OSR-enter/deopt thrash on a loop that keeps overflowing.
pub fn reset_back_edge_count(f: BlissVal) {
    unsafe { (*data(f)).back_edge_count.store(0, Ordering::Relaxed) }
}

/// The current tier (0/1/2).
pub fn tier(f: BlissVal) -> u8 {
    unsafe { (*data(f)).tier.load(Ordering::Acquire) }
}

/// Set the current tier.
pub fn set_tier(f: BlissVal, tier: u8) {
    unsafe { (*data(f)).tier.store(tier, Ordering::Release) }
}

/// The active native entry point (null at T0).
pub fn entry(f: BlissVal) -> *mut u8 {
    unsafe { (*data(f)).entry.load(Ordering::Acquire) }
}

/// Install the active native entry point (on tier change).
pub fn set_entry(f: BlissVal, ptr: *mut u8) {
    unsafe { (*data(f)).entry.store(ptr, Ordering::Release) }
}

/// The FnMeta flags word.
pub fn flags(f: BlissVal) -> u16 {
    unsafe { (*data(f)).flags.load(Ordering::Acquire) }
}

/// Set FnMeta flag bits.
pub fn set_flags(f: BlissVal, bits: u16) {
    unsafe {
        (*data(f)).flags.fetch_or(bits, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::NIL;
    use std::sync::{Mutex, OnceLock};

    fn heap_test_lock() -> &'static Mutex<()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn interpreted_function_carries_cells_and_zeroed_fnmeta() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let ll = BlissVal::from_fixnum(1);
        let bdy = BlissVal::from_fixnum(2);
        let nm = BlissVal::from_symbol_index(bliss_rt_name());
        let f = alloc_interpreted(ll, bdy, NIL, nm);

        assert!(is_interpreted_function(f));
        assert_eq!(lambda_list(f), ll);
        assert_eq!(body(f), bdy);
        assert_eq!(env(f), NIL);
        assert_eq!(name(f), nm);
        assert_eq!(tier(f), 0);
        assert_eq!(invoke_count(f), 0);
        assert!(entry(f).is_null());
    }

    fn bliss_rt_name() -> u32 {
        crate::symbols::intern("FN-OBJ-TEST")
    }

    #[test]
    fn invocation_counter_and_redefinition_reset() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let f = alloc_interpreted(NIL, BlissVal::from_fixnum(1), NIL, NIL);
        assert_eq!(record_invocation(f), 1);
        assert_eq!(record_invocation(f), 2);
        set_tier(f, 1);

        // Redefinition keeps identity but resets tiering.
        let new_body = BlissVal::from_fixnum(9);
        unsafe { redefine(f, NIL, new_body, NIL) };
        assert_eq!(body(f), new_body);
        assert_eq!(invoke_count(f), 0);
        assert_eq!(tier(f), 0);
    }

    #[test]
    fn function_object_is_not_a_symbol_or_fixnum() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let f = alloc_interpreted(NIL, NIL, NIL, NIL);
        assert!(!f.is_fixnum());
        assert!(!f.is_symbol());
        assert!(f.is_heap_object());
    }
}
