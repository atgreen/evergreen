// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap-resident interpreted function objects with per-function tiering metadata
//! (bliss-jtc.6.8; D1.17 + FnMeta §4.4.3).
//!
//! A function is a real heap object reachable through a symbol's function cell,
//! carrying its lambda list / body / captured env / name *and* the `FnMeta`
//! tiering substrate (invocation + back-edge counters, permanent callable entries,
//! current tier, flags). This is what a HotSpot-style engine needs a stable
//! object for: profiling counters, callable dispatch across tier changes, code-
//! cache identity, deopt/tracing/debug identity, and precise GC roots — none of
//! which a name→struct map can provide.
//!
//! Function objects are **pinned and identity-stable**. Ordinary DEFUN installs
//! a fresh object in the symbol's function cell, so saved references retain the
//! previous definition. Tier changes preserve the object's identity. Explicit
//! in-place restoration uses `redefine`; callers must also invalidate compiled
//! dependencies when using that lower-level operation.

use crate::object::{FunctionData, ObjectHeader, type_id};
use crate::value::EgclVal;
use core::sync::atomic::{AtomicPtr, Ordering};

/// Process-lifetime callable dispatchers, independent of the execution-owned
/// code version. The first platform argument register (RDI on SysV x86-64,
/// RCX on Win64) identifies a caller-scanned callable slot; the remaining
/// register/slice arguments use the native call contract.
#[repr(C)]
pub struct NativeCallableEntries {
    pub registers: usize,
    pub slice: usize,
}

static NATIVE_ENTRIES: std::sync::OnceLock<&'static NativeCallableEntries> =
    std::sync::OnceLock::new();

/// Install permanently retained code before publishing callable entries.
/// Repeated initialization must name the same process descriptor.
pub fn install_native_entries(entries: &'static NativeCallableEntries) {
    assert_ne!(entries.registers, 0);
    assert_ne!(entries.slice, 0);
    assert!(std::ptr::eq(
        *NATIVE_ENTRIES.get_or_init(|| entries),
        entries
    ));
}

fn native_entries_pointer() -> *mut NativeCallableEntries {
    NATIVE_ENTRIES
        .get()
        .map_or(std::ptr::null_mut(), |entries| {
            std::ptr::from_ref(*entries).cast_mut()
        })
}

/// FnMeta flag: function is queued for T2 optimising compilation.
pub const FLAG_QUEUED_FOR_T2: u16 = 1 << 0;
/// FnMeta flag: a T2 compilation attempt failed.
pub const FLAG_T2_FAILED: u16 = 1 << 1;
/// FnMeta flag: never attempt to compile (unsupported construct).
pub const FLAG_NEVER_COMPILE: u16 = 1 << 2;
/// Object flag: an instance of a closure template (bliss-gg3i1.7.15.3). Its
/// `env` cell holds the template's prototype function, which owns the shared
/// tiering metadata (counters, tier, FnMeta flags) and, through its definition
/// symbol, the compiled code. Set at allocation and never cleared.
pub const FLAG_CLOSURE_INSTANCE: u16 = 1 << 3;
/// Object flag: the prototype of a closure template, installed in its template
/// symbol's function cell. Set before publication and never cleared.
pub const FLAG_CLOSURE_TEMPLATE: u16 = 1 << 4;
/// Flags owned by the object itself rather than by its metadata owner.
const OBJECT_FLAGS: u16 = FLAG_CLOSURE_INSTANCE | FLAG_CLOSURE_TEMPLATE;

fn body_size() -> usize {
    std::mem::size_of::<FunctionData>() - std::mem::size_of::<ObjectHeader>()
}

fn header_size() -> usize {
    std::mem::size_of::<ObjectHeader>()
}

/// True if `v` is an interpreted function heap object created here.
///
/// A heap TAG is not proof of a readable header. Native entry points and pinned
/// objects outside the managed heap carry pointer tags too, and the collector's
/// own field walk range-checks before reading a header for exactly that reason
/// (`gc::visit_object_references`). Host registry slots surviving a heap that
/// image restore replaced are another source: `--deliver` on a restored image
/// walks those roots and segfaulted here on a pointer into the unmapped old
/// heap. `gc::heap_object_type_id` applies the same lock-free bounds check, so
/// it is safe to call with the world stopped.
pub fn is_interpreted_function(v: EgclVal) -> bool {
    v.is_heap_object() && crate::gc::heap_object_type_id(v) == Some(type_id::FUNCTION_INTERPRETED)
}

/// # Safety
/// `f` must be an interpreted-function object (see `is_interpreted_function`).
unsafe fn data(f: EgclVal) -> *mut FunctionData {
    // SAFETY: caller guarantees `f` is a live interpreted-function object.
    unsafe { f.as_ptr() as *mut FunctionData }
}

/// The object holding `f`'s tiering metadata: a closure instance shares its
/// template prototype's, every other function holds its own.
///
/// # Safety
/// `f` must be an interpreted-function object.
#[inline]
unsafe fn meta(f: EgclVal) -> *mut FunctionData {
    // SAFETY: caller guarantees `f` is live. An instance's `env` cell is its
    // pinned prototype, which the instance keeps reachable.
    unsafe {
        let d = data(f);
        if (*d).flags.load(Ordering::Relaxed) & FLAG_CLOSURE_INSTANCE != 0 {
            data((*d).env)
        } else {
            d
        }
    }
}

/// Allocate one pinned FUNCTION_INTERPRETED body, collecting once and retrying
/// when the pinned space is exhausted. The caller must root every value it
/// stores afterwards, including any frame its closure state will own.
fn alloc_function_body() -> *mut u8 {
    crate::gc::alloc_pinned_typed(body_size(), type_id::FUNCTION_INTERPRETED)
        .or_else(|| {
            if std::env::var_os("EGCL_GC_DISABLE").is_some() {
                return None;
            }
            // Collect outside the allocator's heap lock, then retry slots
            // reclaimed from dead functions.
            crate::gc::full_gc().expect("GC failed while allocating interpreted function");
            crate::gc::alloc_pinned_typed(body_size(), type_id::FUNCTION_INTERPRETED)
        })
        .expect("OOM allocating interpreted function object")
}

/// Allocate a pinned interpreted function object with all cells set and the
/// tiering metadata zeroed and the current process's native dispatcher pair.
pub fn alloc_interpreted(
    lambda_list: EgclVal,
    body: EgclVal,
    env: EgclVal,
    name: EgclVal,
) -> EgclVal {
    crate::rooted!(lambda_list = lambda_list);
    crate::rooted!(body = body);
    crate::rooted!(env = env);
    crate::rooted!(name = name);
    crate::rooted!(
        definition = if name
            .symbol_index()
            .is_some_and(crate::symbols::is_uninterned)
        {
            *name
        } else {
            crate::symbols::make_uninterned("FUNCTION-DEFINITION")
        }
    );
    // All fields are rooted above.
    let body_ptr = alloc_function_body();
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
        (*d).native_entries = AtomicPtr::new(native_entries_pointer());
        (*d).tier = 0.into();
        (*d).flags = 0.into();
        (*d).definition_index = definition.as_symbol_index();
        (*d).closure_state = 0.into();
        (*d).entry = 0.into();
        (*d).entry_owner = 0.into();

        EgclVal::from_heap_ptr(header)
    }
}

/// Allocate an instance of the closure template whose prototype is
/// `prototype`: its lambda list, name and definition symbol, the prototype in
/// its `env` cell, and `state` (0 or an evaluator-owned closure state) in its
/// raw state word. No symbol, registry or code-table work.
///
/// The caller must keep alive (root) whatever `state` owns, such as the
/// captured frame, until this returns: the allocation can collect.
pub fn alloc_closure_instance(prototype: EgclVal, state: usize) -> EgclVal {
    debug_assert!(is_interpreted_function(prototype));
    crate::rooted!(prototype = prototype);
    let body_ptr = alloc_function_body();
    // SAFETY: fresh FUNCTION_INTERPRETED body, as in `alloc_interpreted`; the
    // rooted prototype is pinned and live.
    unsafe {
        let header = body_ptr.sub(header_size());
        let d = header as *mut FunctionData;
        let p = data(*prototype);
        (*d).lambda_list = (*p).lambda_list;
        (*d).body = crate::value::NIL;
        (*d).env = *prototype;
        (*d).name = (*p).name;
        (*d).invoke_count = 0.into();
        (*d).back_edge_count = 0.into();
        (*d).native_entries = AtomicPtr::new(native_entries_pointer());
        (*d).tier = 0.into();
        (*d).flags = FLAG_CLOSURE_INSTANCE.into();
        (*d).definition_index = (*p).definition_index;
        (*d).closure_state = state.into();
        (*d).entry = 0.into();
        (*d).entry_owner = 0.into();
        EgclVal::from_heap_ptr(header)
    }
}

/// Mark `f` as the prototype of a closure template.
///
/// # Safety
/// `f` must be an interpreted-function object not yet published to another
/// thread.
pub unsafe fn mark_closure_template(f: EgclVal) {
    unsafe {
        (*data(f))
            .flags
            .fetch_or(FLAG_CLOSURE_TEMPLATE, Ordering::AcqRel);
    }
}

/// Whether `f` is an instance of a closure template.
pub fn is_closure_instance(f: EgclVal) -> bool {
    is_interpreted_function(f) && own_flags(f) & FLAG_CLOSURE_INSTANCE != 0
}

/// Whether `f` is the prototype of a closure template.
pub fn is_closure_template(f: EgclVal) -> bool {
    is_interpreted_function(f) && own_flags(f) & FLAG_CLOSURE_TEMPLATE != 0
}

/// The template prototype of a closure instance.
pub fn closure_prototype(f: EgclVal) -> Option<EgclVal> {
    is_closure_instance(f).then(|| env(f))
}

/// The flags stored in `f` itself, without delegation to a prototype.
fn own_flags(f: EgclVal) -> u16 {
    unsafe { (*data(f)).flags.load(Ordering::Acquire) }
}

/// The raw closure-state word: 0, or a value the evaluator stored with
/// [`set_closure_state`]. The collector never traces it.
///
/// # Safety
/// `f` must be an interpreted-function object.
pub unsafe fn closure_state(f: EgclVal) -> usize {
    unsafe { (*data(f)).closure_state.load(Ordering::Acquire) }
}

/// Replace the raw closure-state word, returning the previous one. The
/// evaluator owns both values; a nonzero word left in a dead object is passed
/// to the collector's closure-state release hook (see
/// `gc::set_closure_state_release_fn`).
///
/// # Safety
/// `f` must be an interpreted-function object.
pub unsafe fn set_closure_state(f: EgclVal, state: usize) -> usize {
    unsafe { (*data(f)).closure_state.swap(state, Ordering::AcqRel) }
}

/// Redefine `f` in place: update its lambda list / body / env and reset tiering
/// (the code changed, so old counters, tier, and entry are invalid). Preserves
/// object identity so existing references, caches, and traces stay valid.
///
/// # Safety
/// `f` must be an interpreted-function object.
pub unsafe fn redefine(f: EgclVal, lambda_list: EgclVal, body: EgclVal, env: EgclVal) {
    // SAFETY: caller guarantees `f` is a live interpreted-function object.
    unsafe {
        let d = data(f);
        (*d).lambda_list = lambda_list;
        (*d).body = body;
        (*d).env = env;
        (*d).invoke_count.store(0, Ordering::Relaxed);
        (*d).back_edge_count.store(0, Ordering::Relaxed);
        (*d).native_entries
            .store(native_entries_pointer(), Ordering::Release);
        (*d).tier.store(0, Ordering::Release);
        (*d).flags.fetch_and(OBJECT_FLAGS, Ordering::AcqRel);
        // The old body's code must not be entered through this object.
        (*d).entry_owner.store(0, Ordering::Release);
        (*d).entry.store(0, Ordering::Release);
    }
}

/// Byte offset of the native entry word from a tagged function pointer.
pub const ENTRY_TAGGED_OFFSET: i32 =
    core::mem::offset_of!(FunctionData, entry) as i32 - crate::value::TAG_HEAP_OBJECT as i32;
/// Byte offset of the entry owner key from a tagged function pointer.
pub const ENTRY_OWNER_TAGGED_OFFSET: i32 =
    core::mem::offset_of!(FunctionData, entry_owner) as i32 - crate::value::TAG_HEAP_OBJECT as i32;
/// Byte offset of the header's type-id byte from a tagged heap pointer.
pub const TYPE_ID_TAGGED_OFFSET: i32 = 7 - crate::value::TAG_HEAP_OBJECT as i32;

/// Claim `f`'s entry word for the execution `owner` and point it at `entry`.
/// Returns false, changing nothing, when another execution owns it.
///
/// Protocol (bliss-gg3i1.7.15.10.1). Generated callers load `entry` and THEN
/// compare `entry_owner` with their own key, entering only on a match. A
/// claim is made by the owning execution itself: the owner key is set before
/// the entry, so its own later loads see the entry. A release (from any
/// thread) clears the owner before the entry, so a caller that loads a
/// cleared entry also sees a cleared owner. Writers serialize under the call
/// table lock. A non-owner never enters, so code is entered only by the
/// execution that retains it.
///
/// # Safety
/// `f` must be a live interpreted-function object, `owner` the calling
/// execution's nonzero code-owner key, and `entry` an ordinary entry that
/// execution retains while this claim stands.
pub unsafe fn claim_entry(f: EgclVal, owner: u64, entry: usize) -> bool {
    debug_assert_ne!(owner, 0);
    let d = unsafe { data(f) };
    let current = unsafe { &(*d).entry_owner };
    if current
        .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
        && current.load(Ordering::Acquire) != owner
    {
        return false;
    }
    unsafe { (*d).entry.store(entry, Ordering::Release) };
    true
}

/// Release `owner`'s claim on `f`'s entry word, if it still holds one.
///
/// # Safety
/// `f` must be a live interpreted-function object.
pub unsafe fn release_entry(f: EgclVal, owner: u64) {
    if owner == 0 {
        return;
    }
    let d = unsafe { data(f) };
    if unsafe { &(*d).entry_owner }
        .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        unsafe { (*d).entry.store(0, Ordering::Release) };
    }
}

/// The current `(entry, entry_owner)` words of `f` (diagnostics and tests).
pub fn entry_word(f: EgclVal) -> Option<(usize, u64)> {
    is_interpreted_function(f).then(|| unsafe {
        let d = data(f);
        (
            (*d).entry.load(Ordering::Acquire),
            (*d).entry_owner.load(Ordering::Acquire),
        )
    })
}

/// The parsed lambda list cell.
pub fn lambda_list(f: EgclVal) -> EgclVal {
    // SAFETY: interpreted-function object by contract of its callers.
    unsafe { (*data(f)).lambda_list }
}

/// The body cons-tree cell.
pub fn body(f: EgclVal) -> EgclVal {
    unsafe { (*data(f)).body }
}

/// The captured lexical environment cell (NIL for a global defun).
pub fn env(f: EgclVal) -> EgclVal {
    unsafe { (*data(f)).env }
}

/// The function name cell.
pub fn name(f: EgclVal) -> EgclVal {
    unsafe { (*data(f)).name }
}

/// Immutable identity of the semantic definition, independent of its binding.
pub fn definition_index(f: EgclVal) -> u32 {
    unsafe { (*data(f)).definition_index }
}

/// Address of this exact interpreted definition's 32-bit atomic call counter.
/// Returns `None` for other value kinds. Interpreted function objects are pinned,
/// so the address is stable across collections, but the caller must retain a
/// traced root for `f` throughout any later pointer use. Installing a replacement
/// function does not redirect this pointer to the replacement's counter.
/// Generated increments must be atomic read-modify-write operations with at
/// least Relaxed ordering, exactly once per successfully admitted invocation.
pub fn invocation_counter_address(f: EgclVal) -> Option<*const core::sync::atomic::AtomicU32> {
    if !is_interpreted_function(f) {
        return None;
    }
    // SAFETY: the kind check established a live pinned FunctionData object;
    // taking its field address neither reads the counter nor allocates.
    Some(unsafe { std::ptr::addr_of!((*meta(f)).invoke_count) })
}

/// Record an invocation (T0/T1 prologue): atomically bump and return the new
/// invocation count. Lock-free (R4.53).
pub fn record_invocation(f: EgclVal) -> u32 {
    unsafe { (*meta(f)).invoke_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// The current invocation count.
pub fn invoke_count(f: EgclVal) -> u32 {
    unsafe { (*meta(f)).invoke_count.load(Ordering::Relaxed) }
}

/// Record a loop back-edge; returns the new count.
pub fn record_back_edge(f: EgclVal) -> u32 {
    unsafe { (*meta(f)).back_edge_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// Add a sampled batch of back-edges. Native tiers poll only periodically, so
/// charging the batch keeps the shared hotness counter representative without
/// a runtime call on every loop iteration.
pub fn record_back_edges(f: EgclVal, count: u32) -> u32 {
    unsafe {
        (*meta(f))
            .back_edge_count
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count)
    }
}

/// The current loop back-edge count — the hot-loop profiling signal read by the
/// tier scheduler alongside `invoke_count` (bliss-jtc.10).
pub fn back_edge_count(f: EgclVal) -> u32 {
    unsafe { (*meta(f)).back_edge_count.load(Ordering::Relaxed) }
}

/// Stable address of the pinned callable's loop counter. The caller must
/// retain a traced root, as for `invocation_counter_address`.
pub fn back_edge_counter_address(f: EgclVal) -> Option<*const core::sync::atomic::AtomicU32> {
    is_interpreted_function(f).then(|| unsafe { std::ptr::addr_of!((*meta(f)).back_edge_count) })
}

/// Reset the loop back-edge count to zero. Used as OSR-deopt backoff (bliss-izt.2):
/// after a speculating OSR loop deoptimizes back to T0, clearing the counter means
/// the loop must run another full threshold of interpreted iterations before it is
/// re-promoted, bounding OSR-enter/deopt thrash on a loop that keeps overflowing.
pub fn reset_back_edge_count(f: EgclVal) {
    unsafe { (*meta(f)).back_edge_count.store(0, Ordering::Relaxed) }
}

/// The current tier (0/1/2).
pub fn tier(f: EgclVal) -> u8 {
    unsafe { (*meta(f)).tier.load(Ordering::Acquire) }
}

/// Set the current tier.
pub fn set_tier(f: EgclVal, tier: u8) {
    unsafe { (*meta(f)).tier.store(tier, Ordering::Release) }
}

/// Publish this process's callable contract, repairing functions allocated
/// before installation or restored from an image. Never dereference the saved
/// pointer: older images may contain a raw tier-body address in this word.
/// Actual code/version selection remains owned by the current execution.
pub fn native_entries(f: EgclVal) -> Option<&'static NativeCallableEntries> {
    let entries = *NATIVE_ENTRIES.get()?;
    let pointer = std::ptr::from_ref(entries).cast_mut();
    let stored = unsafe { &(*data(f)).native_entries };
    if stored.load(Ordering::Acquire) != pointer {
        stored.store(pointer, Ordering::Release);
    }
    Some(entries)
}

/// The FnMeta flags word.
pub fn flags(f: EgclVal) -> u16 {
    unsafe { (*meta(f)).flags.load(Ordering::Acquire) & !OBJECT_FLAGS }
}

/// Set FnMeta flag bits.
pub fn set_flags(f: EgclVal, bits: u16) {
    unsafe {
        (*meta(f))
            .flags
            .fetch_or(bits & !OBJECT_FLAGS, Ordering::AcqRel);
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
    fn native_publication_repairs_late_and_restored_function_metadata() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        crate::rooted!(function = alloc_interpreted(NIL, NIL, NIL, NIL));
        assert!(native_entries(*function).is_none());
        static ENTRIES: NativeCallableEntries = NativeCallableEntries {
            registers: 0x1000,
            slice: 0x2000,
        };
        install_native_entries(&ENTRIES);
        assert!(std::ptr::eq(native_entries(*function).unwrap(), &ENTRIES));
        // An image can contain an address from another process, including the
        // old raw tier-body address. Never dereference it during repair.
        unsafe {
            (*data(*function)).native_entries.store(
                std::ptr::dangling_mut::<NativeCallableEntries>(),
                Ordering::Release,
            );
        }
        assert!(std::ptr::eq(native_entries(*function).unwrap(), &ENTRIES));
        unsafe { redefine(*function, NIL, EgclVal::from_fixnum(17), NIL) };
        assert!(std::ptr::eq(native_entries(*function).unwrap(), &ENTRIES));
        let fresh = alloc_interpreted(NIL, NIL, NIL, NIL);
        assert!(std::ptr::eq(native_entries(fresh).unwrap(), &ENTRIES));
        assert_eq!(std::mem::size_of::<FunctionData>(), 88);
        assert_eq!(std::mem::offset_of!(FunctionData, native_entries), 48);
        assert_eq!(std::mem::offset_of!(FunctionData, definition_index), 60);
        assert_eq!(std::mem::offset_of!(FunctionData, closure_state), 64);
        assert_eq!(std::mem::offset_of!(FunctionData, entry), 72);
        assert_eq!(std::mem::offset_of!(FunctionData, entry_owner), 80);
        assert_eq!(ENTRY_TAGGED_OFFSET, 70);
        assert_eq!(ENTRY_OWNER_TAGGED_OFFSET, 78);
    }

    #[test]
    fn entry_claims_are_exclusive_and_images_store_them_as_zero() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        crate::gc::ensure_heap_initialized();
        crate::rooted!(function = alloc_interpreted(NIL, NIL, NIL, NIL));
        assert_eq!(entry_word(*function), Some((0, 0)));
        unsafe {
            assert!(claim_entry(*function, 7, 0x1234));
            assert!(!claim_entry(*function, 8, 0x5678), "one owner at a time");
            assert!(claim_entry(*function, 7, 0x2345), "the owner may move it");
            release_entry(*function, 8);
            assert_eq!(
                entry_word(*function),
                Some((0x2345, 7)),
                "not 8's to release"
            );
        }
        let body = unsafe { function.as_ptr().add(header_size()) } as u64;
        let entry = core::mem::offset_of!(FunctionData, entry) - header_size();
        // Per-object serialization.
        let saved = crate::gc::serialize_heap_objects();
        let mut offset = 0;
        let mut found = false;
        while offset < saved.len() {
            let address = u64::from_le_bytes(saved[offset..offset + 8].try_into().unwrap());
            let size =
                u32::from_le_bytes(saved[offset + 9..offset + 13].try_into().unwrap()) as usize;
            if address == body {
                let data = &saved[offset + 13..offset + 13 + size];
                assert_eq!(&data[entry..entry + 16], &[0; 16]);
                found = true;
            }
            offset += 13 + size;
        }
        assert!(found, "the function object was serialized");
        // Region snapshots.
        let snapshot =
            crate::gc::with_heap_snapshot(|| unsafe { crate::gc::snapshot_heap_regions(false) })
                .unwrap()
                .unwrap();
        let region = snapshot
            .regions
            .iter()
            .find(|region| {
                (region.saved_start as u64) <= body
                    && body < (region.saved_start + region.bytes.len()) as u64
            })
            .expect("the function's region was saved");
        let at = body as usize - region.saved_start + entry;
        assert_eq!(&region.bytes[at..at + 16], &[0; 16]);
        // The live object keeps its claim.
        assert_eq!(entry_word(*function), Some((0x2345, 7)));
        unsafe { release_entry(*function, 7) };
        assert_eq!(entry_word(*function), Some((0, 0)));
    }

    #[test]
    fn interpreted_function_carries_cells_and_zeroed_fnmeta() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let ll = EgclVal::from_fixnum(1);
        let bdy = EgclVal::from_fixnum(2);
        let nm = EgclVal::from_symbol_index(egcl_rt_name());
        let f = alloc_interpreted(ll, bdy, NIL, nm);

        assert!(is_interpreted_function(f));
        assert_eq!(lambda_list(f), ll);
        assert_eq!(body(f), bdy);
        assert_eq!(env(f), NIL);
        assert_eq!(name(f), nm);
        assert_eq!(tier(f), 0);
        assert_eq!(invoke_count(f), 0);
        assert_eq!(native_entries(f).is_some(), NATIVE_ENTRIES.get().is_some());
    }

    fn egcl_rt_name() -> u32 {
        crate::symbols::intern("FN-OBJ-TEST")
    }

    #[test]
    fn invocation_counter_address_retains_exact_definition_across_gc() {
        use crate::Collector;
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert!(invocation_counter_address(NIL).is_none());
        assert!(invocation_counter_address(EgclVal::from_fixnum(1)).is_none());
        crate::rooted!(original = alloc_interpreted(NIL, EgclVal::from_fixnum(1), NIL, NIL));
        let address = invocation_counter_address(*original).unwrap();
        crate::rooted!(replacement = alloc_interpreted(NIL, EgclVal::from_fixnum(2), NIL, NIL));
        assert_ne!(address, invocation_counter_address(*replacement).unwrap());
        crate::HeapCollector::new().minor_gc().unwrap();
        assert_eq!(invocation_counter_address(*original), Some(address));
        // The root owns the pinned object throughout this generated-code-like
        // atomic access. A different definition must not receive the count.
        unsafe { &*address }.fetch_add(1, Ordering::Relaxed);
        assert_eq!(invoke_count(*original), 1);
        assert_eq!(invoke_count(*replacement), 0);
    }

    #[test]
    fn invocation_counter_and_redefinition_reset() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let f = alloc_interpreted(NIL, EgclVal::from_fixnum(1), NIL, NIL);
        assert_eq!(record_invocation(f), 1);
        assert_eq!(record_invocation(f), 2);
        set_tier(f, 1);

        // Redefinition keeps identity but resets tiering.
        let new_body = EgclVal::from_fixnum(9);
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
