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

use crate::error::BlissError;
use crate::lock_order::{LockLevel, OrderedRwLock};
use crate::object::{type_id, ObjectHeader, SymbolData};
use crate::value::{BlissVal, NIL, UNBOUND};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

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
    /// Reverse of `name_to_index`: the exact key an interned symbol was created
    /// under, indexed by symbol index. The key uniquely identifies the symbol
    /// (package-qualified where needed), so `intern(index_to_key[i])` round-trips
    /// to `i` — which faithful bytecode serialization relies on to reference the
    /// same symbol across a compile/load boundary (bliss-jtc.23).
    index_to_key: Vec<String>,
}

static REGISTRY: OrderedRwLock<Option<SymbolRegistry>> =
    OrderedRwLock::new(LockLevel::GcWorld, 2, "GC-rooted symbol registry", None);
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
    let roots = crate::gc::ShadowRootScope::new();
    let name = roots.root(name);
    let package = roots.root(package);
    let body = crate::gc::alloc_typed(SYMBOL_BODY_SIZE, type_id::SYMBOL)
        .expect("OOM allocating symbol object");
    // SAFETY: `body` is a fresh SYMBOL body; its header sits at `body - header`,
    // and the header start coincides with `SymbolData`'s first field.
    unsafe {
        let header = body.sub(header_size());
        let sym = header as *mut SymbolData;
        (*sym).name = name.get();
        (*sym).value = UNBOUND;
        (*sym).function = UNBOUND;
        (*sym).plist = NIL;
        (*sym).package = package.get();
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
        index_to_key: Vec::new(),
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
        reg.index_to_key.push(name.to_string());
        idx
    })
}

/// The exact registry key an interned symbol was created under (bliss-jtc.23).
/// `intern(registry_key(idx))` returns `idx`, so this is a faithful,
/// round-trippable identity for cross-process bytecode references. Returns
/// `None` for uninterned symbols (which have no registry key).
pub fn registry_key(idx: u32) -> Option<String> {
    with_registry(|reg| reg?.index_to_key.get(idx as usize).cloned())
}

/// Look up an already-interned symbol index by name without interning it.
/// Returns `None` if no symbol with that name has been interned — what a correct
/// `FIND-SYMBOL` needs (it must not create a symbol).
pub fn find_index(name: &str) -> Option<u32> {
    with_registry(|reg| reg.and_then(|r| r.name_to_index.get(name).copied()))
}

/// All interned symbols as `(index, name)` pairs, in index order — one lock
/// acquisition for callers that enumerate the whole table (e.g. package
/// enumeration of COMMON-LISP / COMMON-LISP-USER / KEYWORD). Replaces probing a
/// fixed `0..4096` range with `symbol_name` per index, which both truncated at
/// 4096 symbols and took a lock per probe (bliss-gq5.5). The name is the
/// registry key the symbol was interned under (identical to `symbol_name`).
pub fn interned_names() -> Vec<(u32, String)> {
    with_registry(|reg| {
        reg.map(|r| {
            r.index_to_key
                .iter()
                .enumerate()
                .map(|(i, name)| (i as u32, name.clone()))
                .collect()
        })
        .unwrap_or_default()
    })
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
/// of a collection. Takes the registry *write* lock (exclusive): the collector
/// both reads cells (marking) and rewrites them (relocation), so it must exclude
/// concurrent `symbol_value`/`set_symbol_value` on other threads that would
/// otherwise race on the same cell word. This never runs while the caller holds
/// the registry lock — interning releases it before any allocation that could
/// trigger a collection.
pub(crate) fn for_each_root_slot(mut f: impl FnMut(*mut BlissVal)) {
    let guard = REGISTRY.write().expect("symbol registry poisoned");
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

// ── Image serialization (bliss-jtc.6 Stage F) ───────────────────────────────

/// Serialize the interned symbol table's identity: the interned names in index
/// order, length-prefixed. Re-interning them in order on restore reconstructs
/// the same index→symbol mapping, so a `from_symbol_index(i)` saved in an image
/// still names the same symbol after reload.
///
/// This persists symbol *identity*, not the value/function/plist cell contents —
/// those reference arbitrary heap objects and belong to whole-heap image
/// serialization. Uninterned (gensym) symbols are process-local and skipped.
pub fn serialize() -> Vec<u8> {
    let mut buf = Vec::new();
    with_registry(|reg| {
        let names: Vec<String> = match reg {
            Some(r) => r
                .interned
                .iter()
                .map(|&obj| {
                    // SAFETY: registry entries are pinned live symbols.
                    unsafe { (*symbol_data(obj)).name }.as_string()
                })
                .collect(),
            None => Vec::new(),
        };
        buf.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for name in &names {
            buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
            buf.extend_from_slice(name.as_bytes());
        }
    });
    buf
}

/// Restore an interned symbol table serialized by [`serialize`]. Intended for a
/// fresh runtime (image load): the current interned table is reset, then the
/// saved names are re-interned in order so indices match the saved image. The
/// previously-interned objects (if any) are dropped from the registry (they are
/// pinned/immortal, so this leaks them — acceptable for a one-shot image load).
pub fn restore(data: &[u8]) -> Result<(), BlissError> {
    let names = decode_names(data)?;
    with_registry_mut(|reg| {
        reg.interned.clear();
        reg.uninterned.clear();
        reg.name_to_index.clear();
        reg.index_to_key.clear();
    });
    for name in names {
        intern(&name);
    }
    Ok(())
}

/// Decode a `[u32 count][u32 len, bytes]*` blob into names.
fn decode_names(data: &[u8]) -> Result<Vec<String>, BlissError> {
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8], BlissError> {
        let end = pos
            .checked_add(n)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| BlissError::Internal("truncated symbol-table image section".into()))?;
        let slice = &data[*pos..end];
        *pos = end;
        Ok(slice)
    };
    let count = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
        let bytes = take(&mut pos, len)?;
        names.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(names)
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
///
/// Takes the registry *write* lock even though it only mutates a cell (not the
/// registry structure): a cell write must be mutually exclusive with the GC's
/// cell scan/relocation (`for_each_root_slot`, also write-locked) and with cell
/// reads, or a concurrent collection could observe a torn pointer.
fn write_cell(idx: u32, set: impl FnOnce(&mut SymbolData)) {
    with_registry_mut(|reg| {
        if let Some(obj) = object_for_index(reg, idx) {
            // SAFETY: registry objects are pinned live symbols.
            set(unsafe { &mut *symbol_data(obj) });
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

/// Diagnostic: the raw address of a symbol's pinned `SymbolData` object, or
/// `None` for an unknown index. The `function` cell lives at offset 24. Used to
/// place a hardware watchpoint on a symbol's function/value cell when chasing
/// heap corruption (bliss-6b2).
pub fn symbol_object_ptr(idx: u32) -> Option<usize> {
    with_registry(|reg| object_for_index(reg?, idx).map(|o| unsafe { o.as_ptr() } as usize))
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

/// Visit every interned symbol whose global function cell is bound, as
/// `(index, name, function)`. Used to enumerate globally-defined functions
/// (e.g. for image dump) now that they live in the function cell rather than an
/// interpreter-side name map (bliss-jtc.6.8).
pub fn for_each_bound_function(mut f: impl FnMut(u32, String, BlissVal)) {
    // Snapshot under the registry lock, then invoke arbitrary caller code only
    // after releasing it. Image serialization formats function bodies in the
    // callback, which legitimately asks the registry for symbol names again.
    let bound = with_registry(|reg| {
        reg.map(|r| {
            r.interned
                .iter()
                .enumerate()
                .filter_map(|(idx, &obj)| {
                    // SAFETY: registry entries are pinned live symbols.
                    let (func, name) = unsafe {
                        let d = symbol_data(obj);
                        ((*d).function, (*d).name)
                    };
                    (func != UNBOUND).then(|| (idx as u32, name.as_string(), func))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    });
    for (idx, name, function) in bound {
        f(idx, name, function);
    }
}

#[cfg(test)]
mod tests {
    //! bliss-jtc.6 Stage A: symbols are heap-resident with their own cells and
    //! interning is unified through this one registry. Names are unique per test
    //! so the process-global registry (shared across the test binary) can't cause
    //! cross-test collisions, and assertions check relationships rather than
    //! absolute indices (other tests may have interned first).
    //!
    //! Interning allocates on the shared GC heap, and concurrent allocation from
    //! multiple threads is not yet safe (no safepoints), so — like every other
    //! heap-touching test in this crate — these serialize on a process-global
    //! lock rather than running in parallel.
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn heap_test_lock() -> &'static Mutex<()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
    }

    /// bliss-jtc.23: registry_key(intern(name)) == name, and interning the key
    /// round-trips to the same index — for distinct package-qualified keys too.
    #[test]
    fn registry_key_round_trips() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        for name in [
            "JTC23-ALPHA",
            "PKGX23::SHARED",
            "PKGY23::SHARED",
            "KEYWORD::JTC23KW",
        ] {
            let idx = intern(name);
            assert_eq!(
                registry_key(idx).as_deref(),
                Some(name),
                "key must match intern name"
            );
            assert_eq!(
                intern(&registry_key(idx).unwrap()),
                idx,
                "key must round-trip to index"
            );
        }
        // Distinct keys with the same bare name are distinct symbols.
        assert_ne!(intern("PKGX23::SHARED"), intern("PKGY23::SHARED"));
    }

    #[test]
    fn intern_is_idempotent_and_names_round_trip_through_the_heap_cell() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
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
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(find_index("STAGE-A-NEVER-INTERNED"), None);
        let idx = intern("STAGE-A-FINDABLE");
        assert_eq!(find_index("STAGE-A-FINDABLE"), Some(idx));
    }

    #[test]
    fn uninterned_symbols_are_distinct_and_unfindable() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
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
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let idx = intern("STAGE-A-CELLS");
        assert_eq!(symbol_value(idx), Some(UNBOUND));
        assert_eq!(symbol_function(idx), Some(UNBOUND));
        assert_eq!(symbol_plist(idx), Some(NIL));

        set_symbol_value(idx, BlissVal::from_fixnum(42));
        set_symbol_plist(idx, intern_as_value("STAGE-A-CELLS"));
        assert_eq!(symbol_value(idx), Some(BlissVal::from_fixnum(42)));
        assert_eq!(symbol_plist(idx), Some(intern_as_value("STAGE-A-CELLS")));
    }

    #[test]
    fn bound_function_callbacks_run_outside_the_registry_lock() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let idx = intern("STAGE-A-BOUND-FUNCTION-SNAPSHOT");
        set_symbol_function(idx, BlissVal::from_fixnum(17));
        let mut saw_probe = false;
        for_each_bound_function(|seen_idx, _, _| {
            // Re-entering a registry reader is expected callback behaviour (the
            // image writer does this while formatting function bodies).
            let _ = symbol_name(seen_idx);
            saw_probe |= seen_idx == idx;
        });
        assert!(saw_probe);
        set_symbol_function(idx, UNBOUND);
    }

    /// The symbol value that flows through the program (tag `101` + index).
    fn intern_as_value(name: &str) -> BlissVal {
        BlissVal::from_symbol_index(intern(name))
    }

    // GC-interaction tests (they call `full_gc`, which is unsafe to run
    // concurrently with allocation from another thread — no safepoints yet) live
    // in the serialized integration binary `tests/symbols_gc_roots.rs`, matching
    // the pattern of the other GC tests, so they can't race the parallel unit
    // tests in this process.
}
