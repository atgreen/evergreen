//! Global heap-resident symbol table (§1.7, D1.08).
//!
//! This is the single runtime registry for Lisp symbols (bliss-jtc.6). Every
//! interned symbol is a real [`SymbolData`] object on the GC heap carrying its
//! own name/value/function/plist/package cells, so the interpreter, compiler,
//! GC, and image loader can all agree on symbol identity and liveness rather
//! than maintaining parallel name tables and fixnum handles.
//!
//! A program-visible symbol value is `BlissVal::from_symbol_index(idx)` (tag
//! `101`, §1.4) — a 32-bit index into this registry — *not* a direct pointer.
//! The registry maps that index to the symbol's heap object, which is where the
//! cells live. This indirection keeps symbol identity stable across GC and image
//! reload while the object it names is free to carry mutable cells.
//!
//! Interned symbols are **pinned and immortal**: they (and their name strings)
//! are never moved or collected, so the raw object references cached here stay
//! valid for the life of the process. That matches CL semantics — an interned
//! symbol lives as long as its package.
//!
//! ## Staging (bliss-jtc.6)
//!
//! Stage A (this module) makes symbols heap-resident and unifies the reader's
//! name table onto them; the value/function/plist cells are populated but the
//! interpreter still reads global bindings from its own maps. Later stages
//! migrate those bindings into the cells (Stage C), route packages through real
//! `PACKAGE` objects (Stage D), and persist the table in images (Stage F).

use crate::object::{type_id, ObjectHeader, SymbolData};
use crate::value::{BlissVal, NIL, UNBOUND};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;

/// `SymbolData` payload size (everything past the 8-byte header): name, value,
/// function, plist, package (5×8) + flags + tls_index (2×4) = 48 bytes.
const SYMBOL_BODY_SIZE: usize = 48;

/// Uninterned symbol indices start above the top bit so they never collide with
/// the sequentially-assigned interned indices (which count up from 0).
const UNINTERNED_BASE: u32 = 0x8000_0000;

fn header_size() -> usize {
    std::mem::size_of::<ObjectHeader>()
}

/// The single global symbol registry.
///
/// `interned[i]` is the heap object for interned symbol index `i` (dense, so the
/// index doubles as the vector position per §1.4). Uninterned symbols live in a
/// sparse map keyed by their high-range index. Both store the symbol's object as
/// a heap-tagged `BlissVal` (`from_heap_ptr`), from which the cells are reached;
/// the program handles the symbol via `from_symbol_index(idx)` instead.
struct SymbolRegistry {
    interned: Vec<BlissVal>,
    uninterned: HashMap<u32, BlissVal>,
    name_to_index: HashMap<String, u32>,
}

static REGISTRY: RwLock<Option<SymbolRegistry>> = RwLock::new(None);
static UNINTERNED_COUNTER: AtomicU32 = AtomicU32::new(UNINTERNED_BASE);

/// Allocate a pinned `SIMPLE_BASE_STRING` on the GC heap holding `s`.
///
/// Layout `[header | len: u64 | bytes]` (matches the reader's string encoding).
/// Pinned + immortal because a symbol's name must never move out from under the
/// object reference cached in the registry.
fn alloc_pinned_name(s: &str) -> BlissVal {
    let bytes = s.as_bytes();
    let body = crate::gc::alloc_typed(8 + bytes.len(), type_id::SIMPLE_BASE_STRING)
        .expect("OOM allocating symbol name string");
    // SAFETY: `body` points past a freshly written header at `body - header`.
    unsafe {
        *(body as *mut u64) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), body.add(8), bytes.len());
        let header = body.sub(header_size());
        let v = BlissVal::from_heap_ptr(header);
        crate::gc::pin(v);
        v
    }
}

/// Allocate a pinned `SymbolData` object with the given name string and home
/// package, all cells unbound/empty. Returns a heap-tagged reference to it.
fn alloc_pinned_symbol(name: BlissVal, package: BlissVal) -> BlissVal {
    let body = crate::gc::alloc_typed(SYMBOL_BODY_SIZE, type_id::SYMBOL)
        .expect("OOM allocating symbol object");
    // SAFETY: `body` is a fresh SYMBOL body; its header sits at `body - header`,
    // and the header start coincides with `SymbolData`'s first field.
    unsafe {
        let header = body.sub(header_size());
        let sym = header as *mut SymbolData;
        (*sym).name = name;
        (*sym).value = UNBOUND;
        (*sym).function = UNBOUND;
        (*sym).plist = NIL;
        (*sym).package = package;
        (*sym).flags = 0;
        (*sym).tls_index = 0;
        let v = BlissVal::from_heap_ptr(header);
        crate::gc::pin(v);
        v
    }
}

/// Reach the `SymbolData` for a registry-stored object reference.
///
/// # Safety
/// `obj` must be a heap-tagged reference to a live (pinned) symbol object, i.e.
/// a value previously returned by `alloc_pinned_symbol` and held in the registry.
unsafe fn symbol_data(obj: BlissVal) -> *mut SymbolData {
    // SAFETY: `obj` is a heap-tagged pointer per this function's contract.
    (unsafe { obj.as_ptr() }) as *mut SymbolData
}

fn with_registry_mut<R>(f: impl FnOnce(&mut SymbolRegistry) -> R) -> R {
    let mut guard = REGISTRY.write().expect("symbol registry poisoned");
    let reg = guard.get_or_insert_with(|| SymbolRegistry {
        interned: Vec::new(),
        uninterned: HashMap::new(),
        name_to_index: HashMap::new(),
    });
    f(reg)
}

fn with_registry<R>(f: impl FnOnce(Option<&SymbolRegistry>) -> R) -> R {
    let guard = REGISTRY.read().expect("symbol registry poisoned");
    f(guard.as_ref())
}

/// Intern `name`, returning its symbol index. Idempotent: the same name always
/// yields the same index and the same underlying heap object.
pub fn intern(name: &str) -> u32 {
    // Fast path: already interned. Takes only a read lock.
    if let Some(idx) = find_index(name) {
        return idx;
    }
    // Allocate the object and its name string *outside* the registry lock. A
    // collection triggered by this allocation scans the registry under a read
    // lock (`for_each_root_slot`); holding the write lock across the allocation
    // would deadlock. Both objects are pinned before any further allocation, so
    // a mid-intern collection cannot reclaim them.
    let name_str = alloc_pinned_name(name);
    let sym = alloc_pinned_symbol(name_str, NIL);
    with_registry_mut(|reg| {
        // Re-check under the write lock in case another thread interned `name`
        // while we were allocating; if so, drop our now-orphaned objects.
        if let Some(&idx) = reg.name_to_index.get(name) {
            crate::gc::unpin(sym);
            crate::gc::unpin(name_str);
            return idx;
        }
        let idx = reg.interned.len() as u32;
        reg.interned.push(sym);
        reg.name_to_index.insert(name.to_string(), idx);
        idx
    })
}

/// Look up an already-interned symbol index by name without interning it.
/// Returns `None` if no symbol with that name has been interned — what a correct
/// `FIND-SYMBOL` needs (it must not create a symbol).
pub fn find_index(name: &str) -> Option<u32> {
    with_registry(|reg| reg.and_then(|r| r.name_to_index.get(name).copied()))
}

/// The object reference for a symbol index, if present.
fn object_for_index(reg: &SymbolRegistry, idx: u32) -> Option<BlissVal> {
    if idx >= UNINTERNED_BASE {
        reg.uninterned.get(&idx).copied()
    } else {
        reg.interned.get(idx as usize).copied()
    }
}

/// The name of the symbol at `idx`, read from its heap name cell. `None` if the
/// index is unknown to the registry.
pub fn symbol_name(idx: u32) -> Option<String> {
    with_registry(|reg| {
        let obj = object_for_index(reg?, idx)?;
        // SAFETY: registry objects are pinned live symbols.
        let name = unsafe { (*symbol_data(obj)).name };
        Some(name.as_string())
    })
}

/// Create a fresh uninterned symbol with the given name. Each call yields a
/// distinct symbol (a unique high-range index) with a real heap object, but the
/// name is not added to the intern map, so `intern`/`find_index` never return it.
pub fn make_uninterned(name: &str) -> BlissVal {
    let idx = UNINTERNED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name_str = alloc_pinned_name(name);
    let sym = alloc_pinned_symbol(name_str, NIL);
    with_registry_mut(|reg| {
        reg.uninterned.insert(idx, sym);
    });
    BlissVal::from_symbol_index(idx)
}

/// Visit every GC-root reference slot held by the registry's symbols — each
/// symbol's name/value/function/plist/package cell (bliss-jtc.6 Stage C). The
/// collector calls this so symbol cells act as roots: a heap object reachable
/// only through a global symbol is marked (major GC) and, if it moves, the cell
/// is relocated to follow it (both GCs).
///
/// Symbols are pinned, so the yielded slot addresses are stable for the duration
/// of a collection. Only a read lock is held; this must never run while the
/// caller holds the registry write lock — and it never does, because interning
/// releases the lock before any allocation that could trigger a collection.
pub(crate) fn for_each_root_slot(mut f: impl FnMut(*mut BlissVal)) {
    let guard = REGISTRY.read().expect("symbol registry poisoned");
    let Some(reg) = guard.as_ref() else {
        return;
    };
    let mut visit = |obj: BlissVal| {
        // SAFETY: registry objects are pinned live symbols; each ref cell is a
        // writable BlissVal at a fixed offset within the object.
        unsafe {
            let sym = symbol_data(obj);
            f(&raw mut (*sym).name);
            f(&raw mut (*sym).value);
            f(&raw mut (*sym).function);
            f(&raw mut (*sym).plist);
            f(&raw mut (*sym).package);
        }
    };
    for &obj in &reg.interned {
        visit(obj);
    }
    for &obj in reg.uninterned.values() {
        visit(obj);
    }
}

// ── Cell accessors (used by later staging; symbols carry their own cells) ────

/// Read one of a symbol's cells by field, or `None` if the index is unknown.
fn read_cell(idx: u32, get: impl FnOnce(&SymbolData) -> BlissVal) -> Option<BlissVal> {
    with_registry(|reg| {
        let obj = object_for_index(reg?, idx)?;
        // SAFETY: registry objects are pinned live symbols.
        Some(get(unsafe { &*symbol_data(obj) }))
    })
}

/// Write one of a symbol's cells by field. No-op if the index is unknown.
fn write_cell(idx: u32, set: impl FnOnce(&mut SymbolData)) {
    with_registry(|reg| {
        if let Some(reg) = reg {
            if let Some(obj) = object_for_index(reg, idx) {
                // SAFETY: registry objects are pinned live symbols.
                set(unsafe { &mut *symbol_data(obj) });
            }
        }
    });
}

/// The global value cell (`UNBOUND` if unbound).
pub fn symbol_value(idx: u32) -> Option<BlissVal> {
    read_cell(idx, |s| s.value)
}

/// Set the global value cell.
pub fn set_symbol_value(idx: u32, value: BlissVal) {
    write_cell(idx, |s| s.value = value);
}

/// The global function cell (`UNBOUND` if undefined).
pub fn symbol_function(idx: u32) -> Option<BlissVal> {
    read_cell(idx, |s| s.function)
}

/// Set the global function cell.
pub fn set_symbol_function(idx: u32, function: BlissVal) {
    write_cell(idx, |s| s.function = function);
}

/// The property list (`NIL` or a cons).
pub fn symbol_plist(idx: u32) -> Option<BlissVal> {
    read_cell(idx, |s| s.plist)
}

/// Set the property list.
pub fn set_symbol_plist(idx: u32, plist: BlissVal) {
    write_cell(idx, |s| s.plist = plist);
}

/// The home package (`NIL` for uninterned).
pub fn symbol_package(idx: u32) -> Option<BlissVal> {
    read_cell(idx, |s| s.package)
}

/// Set the home package.
pub fn set_symbol_package(idx: u32, package: BlissVal) {
    write_cell(idx, |s| s.package = package);
}

#[cfg(test)]
mod tests {
    //! bliss-jtc.6 Stage A: symbols are heap-resident with their own cells and
    //! interning is unified through this one registry. Names are unique per test
    //! so the process-global registry (shared across the test binary) can't cause
    //! cross-test collisions, and assertions check relationships rather than
    //! absolute indices (other tests may have interned first).
    use super::*;
    use crate::object::type_id;

    #[test]
    fn intern_is_idempotent_and_names_round_trip_through_the_heap_cell() {
        let a = intern("STAGE-A-ALPHA");
        let a2 = intern("STAGE-A-ALPHA");
        let b = intern("STAGE-A-BETA");
        assert_eq!(a, a2, "same name must intern to the same index");
        assert_ne!(a, b, "distinct names must get distinct indices");
        // The name is read back out of the symbol's heap name cell, not a side map.
        assert_eq!(symbol_name(a).as_deref(), Some("STAGE-A-ALPHA"));
        assert_eq!(symbol_name(b).as_deref(), Some("STAGE-A-BETA"));
    }

    #[test]
    fn find_index_never_interns() {
        assert_eq!(find_index("STAGE-A-NEVER-INTERNED"), None);
        let idx = intern("STAGE-A-FINDABLE");
        assert_eq!(find_index("STAGE-A-FINDABLE"), Some(idx));
    }

    #[test]
    fn uninterned_symbols_are_distinct_and_unfindable() {
        let g1 = make_uninterned("G");
        let g2 = make_uninterned("G");
        assert_ne!(g1, g2, "each make_uninterned yields a fresh symbol");
        assert_eq!(g1.as_symbol_index() & UNINTERNED_BASE, UNINTERNED_BASE);
        // Same printed name, but not interned: find_index must not see them.
        assert_eq!(find_index("G"), None);
        assert_eq!(symbol_name(g1.as_symbol_index()).as_deref(), Some("G"));
    }

    #[test]
    fn cells_start_unbound_and_are_mutable() {
        let idx = intern("STAGE-A-CELLS");
        assert_eq!(symbol_value(idx), Some(UNBOUND));
        assert_eq!(symbol_function(idx), Some(UNBOUND));
        assert_eq!(symbol_plist(idx), Some(NIL));

        set_symbol_value(idx, BlissVal::from_fixnum(42));
        set_symbol_plist(idx, intern_as_value("STAGE-A-CELLS"));
        assert_eq!(symbol_value(idx), Some(BlissVal::from_fixnum(42)));
        assert_eq!(symbol_plist(idx), Some(intern_as_value("STAGE-A-CELLS")));
    }

    /// The symbol value that flows through the program (tag `101` + index).
    fn intern_as_value(name: &str) -> BlissVal {
        BlissVal::from_symbol_index(intern(name))
    }

    #[test]
    fn interned_symbol_object_lives_on_the_gc_heap() {
        intern("STAGE-A-ON-HEAP");
        let mut saw_symbol = false;
        crate::walk_heap(|_p, tid, _s| {
            if tid == type_id::SYMBOL {
                saw_symbol = true;
            }
            true
        })
        .expect("walk_heap");
        assert!(saw_symbol, "interned symbols must be SYMBOL objects on the GC heap");
    }

    #[test]
    fn value_reachable_only_through_a_symbol_cell_survives_gc() {
        // A cons stored ONLY in a symbol's value cell — no other root anywhere.
        let idx = intern("STAGE-C-CELL-ROOT");
        let marker = BlissVal::from_fixnum(0x00C0_FFEE);
        let cons_body = crate::gc::alloc_typed(16, type_id::CONS).expect("alloc cons");
        // SAFETY: fresh 16-byte cons body [car | cdr].
        let cons = unsafe {
            *(cons_body as *mut BlissVal) = marker;
            *(cons_body as *mut BlissVal).add(1) = NIL;
            BlissVal::from_cons_ptr(cons_body)
        };
        set_symbol_value(idx, cons);

        // Push it through several generations: heavy churn + repeated collection
        // promotes the cons to old-gen and drives major evacuation/sweep. Without
        // the symbol cell acting as a root it would be swept (mark) or its cell
        // left dangling (relocate); either way the marker would be lost.
        for cycle in 0..3 {
            for _ in 0..300 {
                let _ = crate::gc::alloc_typed(16, type_id::CONS);
            }
            crate::gc::full_gc().unwrap_or_else(|_| panic!("full_gc cycle {cycle}"));
            let cell = symbol_value(idx).expect("value cell present");
            // SAFETY: a live cons value points at its body; car is the first word.
            let car = unsafe { *(cell.as_ptr() as *const BlissVal) };
            assert_eq!(
                car, marker,
                "cons reachable only via the symbol cell survived GC cycle {cycle} intact"
            );
        }
    }

    #[test]
    fn pinned_symbols_survive_a_full_gc_with_cells_intact() {
        let idx = intern("STAGE-A-SURVIVOR");
        set_symbol_value(idx, BlissVal::from_fixnum(7));
        // Churn + collect: pinned symbols and their name strings must persist.
        for _ in 0..200 {
            let _ = crate::gc::alloc_typed(16, type_id::CONS);
        }
        crate::gc::full_gc().expect("full_gc");
        assert_eq!(symbol_name(idx).as_deref(), Some("STAGE-A-SURVIVOR"));
        assert_eq!(symbol_value(idx), Some(BlissVal::from_fixnum(7)));
    }
}
