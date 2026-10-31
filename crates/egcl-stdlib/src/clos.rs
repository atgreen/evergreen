// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CLOS — Common Lisp Object System.
//!
//! Class hierarchy, generic function dispatch, method combination,
//! and MOP. See spec §5.3.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{LockLevel, OrderedMutex};
use egcl_rt::object::type_id;
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};

mod runtime_definition;
pub use runtime_definition::{
    RuntimeClassDefinition, RuntimeClassSlotCells, RuntimeSlotDefinition, SlotAllocation,
    class_runtime_definition, class_runtime_shared_cells, set_class_runtime_definition,
};

// ── Method combination ─────────────────────────────────────────────

/// Method combination type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodCombinationType {
    Standard,
    Plus,
    And,
    Or,
    List,
    Append,
    Nconc,
    Min,
    Max,
    Progn,
}

#[allow(dead_code)]
impl MethodCombinationType {
    #[expect(
        dead_code,
        reason = "retained for future serialized combination encodings"
    )]
    fn discriminant(self) -> i64 {
        match self {
            Self::Standard => 0,
            Self::Plus => 1,
            Self::And => 2,
            Self::Or => 3,
            Self::List => 4,
            Self::Append => 5,
            Self::Nconc => 6,
            Self::Min => 7,
            Self::Max => 8,
            Self::Progn => 9,
        }
    }
}

mod class_protocol;
pub use class_protocol::{
    begin_class_finalization, class_protocol_default_initargs, class_protocol_finalized,
    class_protocol_runtime_defaults, class_protocol_runtime_slots, class_slot_metaobjects,
    compute_class_precedence_list_default, publish_class_protocol,
    set_class_direct_slot_metaobjects, set_class_raw_effective_slot_metaobjects,
};

// ── Internal data structures ──────────────────────────────────────

#[derive(Clone)]
struct ClassProtocol {
    direct_slots: EgclVal,
    direct_ready: bool,
    // Computed: effective slot list. Otherwise: optional one-element raw
    // introspection cache, independent of runtime protocol/layout publication.
    effective_slots: EgclVal,
    default_initargs: EgclVal,
    runtime_default_initargs: EgclVal,
    runtime_slots: EgclVal,
    cpl: Option<Vec<EgclVal>>,
    instance_slots: Option<Vec<EgclVal>>,
    computed: bool,
    finalized: bool,
}
impl Default for ClassProtocol {
    fn default() -> Self {
        Self {
            direct_slots: NIL,
            direct_ready: false,
            effective_slots: NIL,
            default_initargs: NIL,
            runtime_default_initargs: NIL,
            runtime_slots: NIL,
            cpl: None,
            instance_slots: None,
            computed: false,
            finalized: true,
        }
    }
}
impl ClassProtocol {
    fn invalidate_computed(&mut self) {
        self.effective_slots = NIL;
        self.default_initargs = NIL;
        self.runtime_default_initargs = NIL;
        self.runtime_slots = NIL;
        self.cpl = None;
        self.instance_slots = None;
        self.computed = false;
        self.finalized = false;
    }
}

#[derive(Clone)]
struct ClassMeta {
    runtime_definition: Option<RuntimeClassDefinition>,
    protocol: ClassProtocol,
    name: EgclVal,
    direct_supers: Vec<EgclVal>,
    direct_subs: Vec<EgclVal>,
    /// Direct :instance-allocated slot names (as passed to `define_class`).
    slots: Vec<EgclVal>,
    /// Direct :class-allocated slot names: an inherited instance slot of the
    /// same name is not part of this class's instance layout (CLHS 7.5.3).
    shared_slots: Vec<EgclVal>,
    /// Frozen, process-lifetime descriptor shared with instances. Built-in
    /// (never-instantiated) classes have no wrapper.
    wrapper: Option<&'static ClassWrapper>,
    /// The class's metaclass, when DEFCLASS named one with `:metaclass`. NIL
    /// means the default, STANDARD-CLASS. `class_of` answers this, which is how
    /// portable code tells a class with a custom metaclass from a plain one --
    /// Gendl's kernel does exactly that, and the option used to be parsed and
    /// dropped (GitHub #371).
    metaclass: EgclVal,
    /// True while this class exists only because another class named it as a
    /// superclass before it was defined (`resolve_superclass_metaobject`).
    /// Cleared by the first real definition, which is every `define_class`
    /// call.
    ///
    /// Such a class is an instance of `FORWARD-REFERENCED-CLASS` and is not yet
    /// a standard class: it cannot be instantiated (`allocate_instance`), it is
    /// not finalized, and neither is anything that inherits from it (R9.39).
    /// Its slot storage grows to its real metaclass's layout when the
    /// definition arrives, so unlike before bliss-fmhj5 the object itself is
    /// kept and its identity survives (GitHub #384, bliss-qbz4o).
    forward_referenced: bool,
}

// ── Standard-object instances (heap objects) ───────────────────────
//
// A CLOS instance is a real heap object (tag 010, type_id STANDARD_OBJECT):
//
//   offset 0   ObjectHeader (8 bytes)
//   offset 8   wrapper pointer (*mut ClassWrapper)
//   offset 16  slot[0..N-1]  inline EgclVal cells (:instance allocation only)
//
// Size = 16 + 8N bytes. Allocated via `std::alloc::alloc_zeroed` and leaked,
// like the other tree-walker heap objects (strings/ratios/vectors). See
// spec §5.3.4 (D5.09) and issue bliss-xyo.

/// Global monotonic class-stamp counter (§5.3.4). Copied into each wrapper.
static CLASS_STAMP: AtomicU64 = AtomicU64::new(1);

fn next_stamp() -> u64 {
    CLASS_STAMP.fetch_add(1, Ordering::Relaxed)
}

/// Wrapper state values.
const WRAPPER_CURRENT: u8 = 0;
const WRAPPER_OBSOLETE: u8 = 1;

/// Frozen effective-slot layout for a class, owned by a `ClassWrapper`. Leaked
/// at finalize time so instances created against it keep reading their own
/// layout even after the class is redefined (which mints a fresh wrapper).
struct SlotLayout {
    /// index → slot name.
    order: Vec<EgclVal>,
    /// slot name → index into the inline slot vector.
    index: HashMap<EgclVal, usize>,
    /// Index of the hidden funcallable-function slot, found by bare name the
    /// first time anyone asks. FUNCTIONP/TYPEP ask about every instance they
    /// see, and the by-name scan used to repeat on each call.
    funcallable_slot: std::sync::OnceLock<Option<usize>>,
}

impl SlotLayout {
    fn new(order: Vec<EgclVal>, index: HashMap<EgclVal, usize>) -> Self {
        SlotLayout {
            order,
            index,
            funcallable_slot: std::sync::OnceLock::new(),
        }
    }
}

/// Lightweight, immutable per-class descriptor (D5.04) pointed to from each
/// instance's offset-8 word. A new wrapper is allocated on class redefinition
/// and the old one's `state` set to obsolete, enabling the stamp-check fast
/// path without touching the class metaobject.
#[repr(C)]
struct ClassWrapper {
    stamp: u64,
    state: AtomicU8,
    class: EgclVal,
    slot_count: u32,
    layout: &'static SlotLayout,
    /// Structure ancestry when this is a structure class's layout, else null.
    /// Published once, after the wrapper exists (see [`install_struct_info`]).
    struct_info: std::sync::atomic::AtomicPtr<StructInfo>,
}

/// What the lock-free structure fast paths need from a structure layout:
/// the class names of its structure ancestry, root first and own class last.
/// Only built when every ancestor's current layout is a prefix of this one, so
/// a slot an accessor of any ancestor names sits at that ancestor's index.
struct StructInfo {
    names: Box<[EgclVal]>,
}

/// Follow the forwarding chain to the live object. When `change-class` needs a
/// larger allocation than the original (slot-count growth), the old object is
/// turned into a forwarding stub: its header `FORWARDED` gc-bit is set and a
/// `EgclVal` pointer to the replacement is stored at offset 8. Every accessor
/// chases this so pointer identity is preserved (spec R5.69). Fresh instances
/// are never forwarded, so the common path returns immediately.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn resolve_forwarding(inst: EgclVal) -> EgclVal {
    // Delegates to the collector's forwarding resolution: CHANGE-CLASS stubs
    // now use the SAME layout as evacuation forwarding (untagged new-body
    // address in the first payload word), so the GC's relocate pass rewrites
    // stub-holding slots itself and this lazy chase is only needed between a
    // migration and the next collection (bliss-334).
    egcl_rt::gc::resolve_forwarded(inst)
}

/// What a class redefinition changed about an instance's layout, in the shape
/// `UPDATE-INSTANCE-FOR-REDEFINED-CLASS` takes it (CLHS 7.2.2).
///
/// Returned to the caller rather than acted on here: applying an added slot's
/// `:initform` means EVALUATING it, and the evaluator lives above this crate.
pub struct RedefinitionUpdate {
    /// Slot names the new layout has and the old one did not. These are the
    /// slots whose initforms CLHS says to apply.
    pub added: Vec<EgclVal>,
    /// Slot names the old layout had and the new one does not.
    pub discarded: Vec<EgclVal>,
    /// Name/value pairs, flat, for each discarded slot that was BOUND: the
    /// property list the protocol hands to a user method.
    pub plist: Vec<EgclVal>,
}

/// Bring `instance` up to date if its class has been redefined, and answer the
/// address it now lives at.
///
/// Since bliss-fmhj5 the address is always the one passed in — a CLOS update
/// replaces the instance's STORAGE, never its shell — so this is kept for the
/// callers that were written against the old reallocating behaviour rather
/// than because it can still change.
pub fn ensure_instance_current(instance: EgclVal) -> EgclVal {
    migrate_if_obsolete(instance).0
}

/// Migrate `instance` if its class has been redefined, and report what the
/// redefinition changed so the caller can run the CLHS update protocol.
///
/// The caller must do this BEFORE anything else touches the instance: every
/// slot accessor migrates on its own (`update_if_obsolete`), and one that got
/// there first would discard the record of what changed, leaving the added
/// slots UNBOUND for good (bliss-oi5a6).
pub fn migrate_if_obsolete(instance: EgclVal) -> (EgclVal, Option<RedefinitionUpdate>) {
    if !is_instance(instance) {
        return (instance, None);
    }
    unsafe { migrate_obsolete(instance) }
}

/// Lazily migrate an instance whose wrapper is obsolete (its class was
/// redefined) to the class's current layout, exactly once (R5.83/R5.84).
/// Surviving slots are copied by name; slots removed by the redefinition are
/// dropped; slots added are left UNBOUND. The old object is forwarded to the
/// new one, so subsequent accesses see the current layout and this runs once.
///
/// RETURNS THE LIVE INSTANCE, and every caller must use it instead of the value
/// it passed in (bliss-ch80h). `allocate_instance` below can fire a relocating
/// minor GC, and a nursery-resident obsolete instance is then EVACUATED: this
/// function roots its own copy across that, but a caller's is an independent,
/// unrooted word that the collector cannot see, so it is left pointing at a
/// body that has been recycled -- and poisoned, under `EGCL_GC_POISON`.
///
/// That was not hypothetical. With the probe below the fault is deterministic,
/// a SIGSEGV on every run:
///
/// ```text
/// (defclass c () ((a :initform 11)))
/// (defvar *i* (make-instance 'c))
/// (defclass c () ((a :initform 11) (b :initform 22)))   ; obsoletes *i*
/// (slot-value *i* 'a)
///
/// EGCL_GC_STRESS_AT=100115 EGCL_GC_POISON=1 egcl --no-init --load probe.lisp
/// ```
///
/// `EGCL_GC_STRESS_AT` is what makes it reachable: it forces ONE collection, at
/// the migration's own allocation, while the instance is still nursery-fresh.
/// `EGCL_GC_STRESS=1` cannot show it -- its earlier collections promote the
/// instance out of the nursery first, so by migration time nothing moves. That
/// is the "a clean stress run can mean nothing moved" trap in CLAUDE.md,
/// measured here rather than reasoned about.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[must_use = "the instance may have moved during the migration; use the returned value"]
unsafe fn update_if_obsolete(inst: EgclVal) -> EgclVal {
    // Drops the layout change: a slot accessor cannot run the update protocol,
    // which evaluates initforms. `migrate_if_obsolete` is the entry point for a
    // caller that can (bliss-oi5a6).
    unsafe { migrate_obsolete(inst).0 }
}

/// [`update_if_obsolete`], also reporting what the redefinition changed.
#[must_use = "the instance may have moved during the migration; use the returned value"]
unsafe fn migrate_obsolete(inst: EgclVal) -> (EgclVal, Option<RedefinitionUpdate>) {
    unsafe {
        let live = resolve_forwarding(inst);
        let w = *(live.as_ptr().add(8) as *const *mut ClassWrapper);
        if w.is_null() || (*w).state.load(Ordering::Acquire) != WRAPPER_OBSOLETE {
            // Nothing allocated, so nothing moved: `live` is still current.
            return (live, None);
        }
        let class = (*w).class;
        // Snapshot surviving (bound) slots from the old frozen layout, reading
        // the storage the shell currently points at.
        let mut snap: Vec<(EgclVal, EgclVal)> = Vec::new();
        {
            let ol = (*w).layout;
            let old_storage = instance_storage(live);
            for (i, &name) in ol.order.iter().enumerate() {
                let v = *(old_storage.as_ptr().add(16) as *const EgclVal).add(i);
                if v != UNBOUND {
                    snap.push((name, v));
                }
            }
        }
        // The class's CURRENT wrapper, which is the layout the instance is
        // being brought up to.
        let nw = metaclass_instance_wrapper(class);
        if nw.is_null() {
            return (live, None);
        }
        // Fresh STORAGE for that layout, and then the swap: wrapper and
        // storage into the SAME shell. The instance keeps its address, so
        // nothing downstream can be holding a stale one — which is what makes
        // the whole reallocate-and-forward mechanism, and the three GC defects
        // it produced (bliss-ch80h, bliss-kbo3y, bliss-k4gev), go away
        // (bliss-fmhj5). SBCL's %obsolete-instance-trap is these same two
        // stores.
        //
        // Pinned storage for a pinned shell: a class metaobject is an instance
        // of its metaclass and migrates through here when the metaclass is
        // redefined, and its shell is pinned because ClosState keys HashMaps by
        // its address.
        let pinned = with_state(|st| st.class_meta.contains_key(&live));
        let mut live = live;
        egcl_rt::rooted_ref!(_live_root = &mut live);
        egcl_rt::rooted_ref!(_snap_root = &mut snap);
        let mut storage = match alloc_slot_storage((*nw).slot_count as usize, pinned) {
            Some(s) => s,
            // The allocation may still have collected before failing, so answer
            // the ROOTED copy rather than the caller's.
            None => return (live, None),
        };
        egcl_rt::rooted_ref!(_storage_root = &mut storage);
        set_instance_storage(live, storage);
        set_instance_wrapper(live, nw);
        {
            let nl = (*nw).layout;
            for (name, val) in &snap {
                if let Some(&idx) = nl.index.get(name) {
                    *slot_cell(live, idx) = *val;
                }
            }
        }
        // What the redefinition changed, read off the two frozen layouts: a
        // name the new layout has and the old did not is ADDED (its initform is
        // what the protocol applies), the reverse is DISCARDED, and a discarded
        // slot that was bound contributes its name and value to the property
        // list. Compared by identity, the same way the copy loop above matches
        // names through `nl.index`.
        //
        // Nothing below allocates, so these values stay valid until they are
        // returned -- but the CALLER evaluates initforms with them, and must
        // root them first.
        let update = {
            let old_names = &(*w).layout.order;
            let empty: Vec<EgclVal> = Vec::new();
            let new_names = if nw.is_null() {
                &empty
            } else {
                &(*nw).layout.order
            };
            let added: Vec<EgclVal> = new_names
                .iter()
                .copied()
                .filter(|name| !old_names.contains(name))
                .collect();
            let discarded: Vec<EgclVal> = old_names
                .iter()
                .copied()
                .filter(|name| !new_names.contains(name))
                .collect();
            let mut plist: Vec<EgclVal> = Vec::new();
            for (name, value) in &snap {
                if discarded.contains(name) {
                    plist.push(*name);
                    plist.push(*value);
                }
            }
            RedefinitionUpdate {
                added,
                discarded,
                plist,
            }
        };
        // No forwarding: the shell never moved.
        (live, Some(update))
    }
}

/// Read the wrapper pointer from an instance (offset 8), chasing forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn instance_wrapper(inst: EgclVal) -> *mut ClassWrapper {
    unsafe {
        let live = resolve_forwarding(inst);
        *(live.as_ptr().add(8) as *const *mut ClassWrapper)
    }
}

/// Write the wrapper pointer into an instance (offset 8), chasing forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn set_instance_wrapper(inst: EgclVal, w: *mut ClassWrapper) {
    unsafe {
        let live = resolve_forwarding(inst);
        *(live.as_ptr().add(8) as *mut *mut ClassWrapper) = w;
    }
}

/// Leak a frozen, empty `SlotLayout` plus a `ClassWrapper` naming `class`.
///
/// The bootstrap needs wrappers before any class exists to finalize one, and a
/// class metaobject has no instance slots of its own (its metadata lives in
/// `ClassMeta`, not in Lisp-visible slots), so an empty layout is the whole
/// story. `finalize_class_layout` builds the same shape for ordinary classes.
fn leak_empty_wrapper(class: EgclVal) -> *mut ClassWrapper {
    let layout: &'static SlotLayout =
        Box::leak(Box::new(SlotLayout::new(Vec::new(), HashMap::new())));
    let wrapper: &'static ClassWrapper = Box::leak(Box::new(ClassWrapper {
        stamp: next_stamp(),
        state: AtomicU8::new(WRAPPER_CURRENT),
        class,
        slot_count: 0,
        layout,
        struct_info: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
    }));
    wrapper as *const ClassWrapper as *mut ClassWrapper
}

/// Allocate a class metaobject: a pinned, zero-slot `STANDARD_OBJECT`.
///
/// Pinned rather than nursery-born because a class value is used as a
/// `HashMap` key all over `ClosState`, so its address has to be an identity.
/// `alloc_pinned_typed` packs these into dedicated host regions, which is the
/// difference between bounding the cost to the objects' own bytes and
/// retaining a whole 1 MiB region per object (bliss-wc4t). Classes are
/// immortal either way: only `FUNCTION_INTERPRETED` and `SYMBOL` are ever
/// recycled from a pinned slot.
///
/// The wrapper word is left NULL and must be filled in by the caller before
/// anything reads it — `class_of` dereferences it with no null check.
fn alloc_class_metaobject() -> Option<EgclVal> {
    // 24 bytes: the 8-byte header the allocator writes, the wrapper word, and
    // the slot-storage word every shell now carries (bliss-fmhj5). The storage
    // starts NIL rather than a zero-length vector: these are the built-in class
    // metaobjects, whose own metaclass cannot gain slots (redefining a built-in
    // class is refused), so nothing indexes them -- and the body MUST still be
    // wide enough, or a migration that did install storage would write past the
    // allocation. NIL is a valid tagged value for the STANDARD_OBJECT tracer,
    // which visits word 1.
    let body = egcl_rt::gc::alloc_pinned_typed(SLOT_STORAGE_OFFSET, type_id::STANDARD_OBJECT)?;
    unsafe {
        let inst = EgclVal::from_heap_ptr(body.sub(8));
        *(body.add(8) as *mut EgclVal) = NIL;
        Some(inst)
    }
}

/// Offset of the slot-storage word in an instance shell: past the 8-byte
/// header and the raw wrapper pointer.
const SLOT_STORAGE_OFFSET: usize = 16;

/// The instance's slot-storage vector (word 1 of the shell), chasing
/// forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn instance_storage(inst: EgclVal) -> EgclVal {
    unsafe {
        let live = resolve_forwarding(inst);
        *(live.as_ptr().add(SLOT_STORAGE_OFFSET) as *const EgclVal)
    }
}

/// Install a slot-storage vector in the shell (word 1), chasing forwarding.
///
/// This is the whole point of the indirection: a class redefinition or
/// CHANGE-CLASS replaces the STORAGE and the wrapper, never the shell, so the
/// instance keeps its address and its identity (bliss-fmhj5). SBCL does the
/// same two stores — `(setf (%instance-layout thing) layout)` plus
/// `(%instance-set thing instance-data-start slot-vector)`.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value and `storage` a
/// SIMPLE_VECTOR with a cell per slot of the wrapper's layout.
#[inline]
unsafe fn set_instance_storage(inst: EgclVal, storage: EgclVal) {
    unsafe {
        let live = resolve_forwarding(inst);
        *(live.as_ptr().add(SLOT_STORAGE_OFFSET) as *mut EgclVal) = storage;
    }
}

/// A fresh slot-storage vector of `slot_count` UNBOUND cells.
///
/// A SIMPLE_VECTOR, deliberately: the collector already traces one elementwise
/// (`length | elements…`), and the shell reaches it through a tagged word the
/// STANDARD_OBJECT tracer already visits — so this representation needs no new
/// tracing code at all.
///
/// `pinned` follows the shell. A class metaobject's shell is pinned because
/// `ClosState` keys HashMaps by its address; pinning its storage too keeps the
/// pair immortal together rather than relying on a pinned object's reference
/// to a movable one being rewritten.
fn alloc_slot_storage(slot_count: usize, pinned: bool) -> Option<EgclVal> {
    let body_size = 8 + slot_count * 8;
    let body = if pinned {
        egcl_rt::gc::alloc_pinned_typed(body_size, type_id::SIMPLE_VECTOR)?
    } else {
        egcl_rt::gc::alloc_typed(body_size, type_id::SIMPLE_VECTOR)?
    };
    unsafe {
        *(body as *mut u64) = slot_count as u64;
        for i in 0..slot_count {
            *(body.add(8 + i * 8) as *mut EgclVal) = UNBOUND;
        }
        Some(EgclVal::from_heap_ptr(
            body.sub(egcl_rt::gc::body_header_offset(body_size)),
        ))
    }
}

/// Pointer to slot cell `idx`, chasing forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT with at least `idx+1` slots.
#[inline]
unsafe fn slot_cell(inst: EgclVal, idx: usize) -> *mut EgclVal {
    unsafe {
        let storage = instance_storage(inst);
        // Slot storage is always a SMALL object, so its payload begins at the
        // fixed 8-byte header offset and the length word sits there: elements
        // start 16 bytes in. A layout cannot be large enough to need the
        // 16-byte prefix, because the instance size field caps it at 0xFFFE
        // words (`allocate_instance` asserts that). Hard-coding the offset is
        // what keeps this to TWO dependent loads, the same shape as SBCL's
        // standard-instance-access.
        debug_assert!(
            !(*(storage.as_ptr() as *const egcl_rt::object::ObjectHeader)).is_large_object(),
            "slot storage unexpectedly a large object"
        );
        (storage.as_ptr().add(16) as *mut EgclVal).add(idx)
    }
}

/// Resolve a slot name to its inline index via the instance's frozen layout.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
unsafe fn instance_slot_index(inst: EgclVal, slot_name: EgclVal) -> Option<usize> {
    unsafe {
        let w = instance_wrapper(inst);
        if w.is_null() {
            return None;
        }
        let layout = (*w).layout;
        if let Some(idx) = layout.index.get(&slot_name).copied() {
            return Some(idx);
        }
        // Identity miss: fall back to matching by symbol name. A source-free
        // .bfasl load can mint a duplicate symbol for a slot whose package was
        // not yet ensured when a compiled method's constant pool materialized it
        // (`PKG::NAME` rt-key vs the package system's canonical symbol) — so the
        // slot defined by the tree-walked defclass and the one named by a
        // compiled slot-value access are different identities with the same name
        // (bliss-e7t: ASDF/COMPONENT::SOURCE-FILE). Match on name so the access
        // still resolves. This is the same compile-vs-load rendering hazard the
        // variadic binder guards against.
        slot_name_index_fallback(layout, slot_name)
    }
}

/// Slow-path slot lookup by symbol name (see `instance_slot_index`). Only runs
/// after an identity miss, so the linear scan never touches the hot path.
/// Matches on the *bare* symbol name (package prefix stripped): the two
/// duplicate identities can also disagree on export-status rendering
/// (`PKG:NAME` vs `PKG::NAME`), so a full-key comparison would still miss.
fn slot_name_index_fallback(layout: &SlotLayout, slot_name: EgclVal) -> Option<usize> {
    if !slot_name.is_symbol() {
        return None;
    }
    let target = bare_symbol_name(slot_name)?;
    layout
        .order
        .iter()
        .position(|&s| bare_symbol_name(s).as_deref() == Some(target.as_str()))
}

/// The symbol's name without any `PKG:`/`PKG::` package prefix.
fn bare_symbol_name(sym: EgclVal) -> Option<String> {
    // symbol_index() (not as_symbol_index) so the special NIL/T constants —
    // which is_symbol() accepts but which have no table index — return None
    // instead of panicking (seen via a NIL in a slot layout under GC stress).
    let key = egcl_rt::symbols::symbol_name(sym.symbol_index()?)?;
    let bare = egcl_rt::symbols::split_registry_key(&key)
        .map(|(_, n)| n)
        .unwrap_or(key.as_str());
    Some(bare.to_string())
}

/// Qualifier for a method (for method combination).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodQualifier {
    /// An unqualified method.
    Primary,
    Before,
    After,
    Around,
    /// The qualifier that names a short-form method combination (`+`, `AND`,
    /// `PROGN`, ...): the "primary" methods of such a generic function.
    Combination,
    /// A qualifier the generic function's method combination does not know;
    /// calling the generic function with the method applicable is an error.
    Other,
}

/// Metadata for a method: specializers and qualifier.
#[derive(Clone)]
struct MethodMeta {
    /// Per-argument specializer classes. Empty means no specialization (T).
    specializers: Vec<EgclVal>,
    qualifier: MethodQualifier,
}

#[derive(Clone)]
struct GFData {
    /// Read back by GENERIC_FUNCTION_NAME, which dispatch uses to find the
    /// generic's argument precedence order.
    name: EgclVal,
    #[allow(dead_code)]
    lambda_list: EgclVal,
    methods: Vec<EgclVal>,
}

/// Standard method combination effective method descriptor.
#[derive(Clone)]
struct EffectiveMethod {
    around: Vec<EgclVal>,
    before: Vec<EgclVal>,
    primary: Vec<EgclVal>,
    after: Vec<EgclVal>,
}

/// Short-form method combination effective method descriptor.
#[derive(Clone)]
struct ShortFormMethod {
    combination: MethodCombinationType,
    methods: Vec<EgclVal>,
}

struct ClosState {
    /// name → class value
    class_registry: HashMap<EgclVal, EgclVal>,
    /// bare class name (uppercase, package-stripped) → class value. A fallback
    /// for `find_class` when a class-name symbol's *identity* has drifted (e.g.
    /// re-interned by a package RECYCLE/rehome after the class was defined), so
    /// the symbol-keyed `class_registry` misses even though the class exists.
    class_by_name: HashMap<String, EgclVal>,
    /// class value → metadata
    class_meta: HashMap<EgclVal, ClassMeta>,
    /// Derived query results, discarded on every mutable state access and GC.
    /// Never serialized or treated as owning roots; metadata remains the source
    /// of truth, including bootstrap's registration-order superclass inference.
    cpl_cache: RefCell<HashMap<EgclVal, Vec<EgclVal>>>,
    /// gf id → generic function data
    generic_functions: HashMap<EgclVal, GFData>,
    /// method id → method metadata (specializers, qualifier)
    method_meta: HashMap<EgclVal, MethodMeta>,
    /// effective method key → standard combination descriptor
    effective_methods: HashMap<EgclVal, EffectiveMethod>,
    /// effective method key → short-form combination descriptor
    short_form_methods: HashMap<EgclVal, ShortFormMethod>,
    /// Counter for internal effective-method HashMap keys (not Lisp-visible).
    next_em_key: i64,
    next_gf_id: i64,
    /// Class values created by DEFSTRUCT. Their instances are STRUCTURE-OBJECTs
    /// (not STANDARD-OBJECTs) and EQUALP descends them / they print in #S(...)
    /// syntax — none of which applies to a plain DEFCLASS class (bliss-i1i9).
    structure_classes: HashSet<EgclVal>,
    // Built-in class values
    fixnum_class: EgclVal,
    character_class: EgclVal,
    symbol_class: EgclVal,
    null_class: EgclVal,
    t_class_val: EgclVal,
    standard_object_class: EgclVal,
    /// STANDARD-CLASS and STRUCTURE-CLASS, the two metaclasses a class
    /// metaobject's own wrapper has to be able to name. Kept as values rather
    /// than wrapper pointers because a raw pointer cannot survive a saved
    /// image, while these go through the same remap as any other class value.
    standard_class: EgclVal,
    structure_class: EgclVal,
    /// FORWARD-REFERENCED-CLASS: the metaclass of a placeholder minted for a
    /// class named as a superclass before it is defined (AMOP; spec R9.39).
    /// Kept as a value for the same reason as the two above — a raw wrapper
    /// pointer cannot survive a saved image.
    forward_referenced_class: EgclVal,
    cons_class: EgclVal,
    float_class: EgclVal,
    function_class: EgclVal,
    heap_object_class: EgclVal,
    /// Track fixnum class registrations in order for diamond heuristic
    fixnum_registrations: Vec<(i64, EgclVal)>,
    bootstrapped: bool,
}

impl ClosState {
    fn new() -> Self {
        Self {
            class_registry: HashMap::new(),
            class_by_name: HashMap::new(),
            class_meta: HashMap::new(),
            cpl_cache: RefCell::new(HashMap::new()),
            generic_functions: HashMap::new(),
            method_meta: HashMap::new(),
            effective_methods: HashMap::new(),
            short_form_methods: HashMap::new(),
            next_em_key: 500_000,
            next_gf_id: 200_000,
            structure_classes: HashSet::new(),
            fixnum_class: NIL,
            character_class: NIL,
            symbol_class: NIL,
            null_class: NIL,
            t_class_val: NIL,
            standard_object_class: NIL,
            standard_class: NIL,
            structure_class: NIL,
            forward_referenced_class: NIL,
            cons_class: NIL,
            float_class: NIL,
            function_class: NIL,
            heap_object_class: NIL,
            fixnum_registrations: Vec::new(),
            bootstrapped: false,
        }
    }

    /// Mint a fresh internal key for the effective-method HashMaps. These keys
    /// are never exposed to Lisp (only used to index `effective_methods` /
    /// `short_form_methods`), so a plain fixnum in a private high range is fine.
    fn alloc_em_key(&mut self) -> EgclVal {
        let id = self.next_em_key;
        self.next_em_key += 1;
        EgclVal::from_meta_handle(id)
    }

    fn alloc_gf_id(&mut self) -> EgclVal {
        let id = self.next_gf_id;
        self.next_gf_id += 1;
        EgclVal::from_meta_handle(id)
    }
}

// No Lisp allocation, evaluation, safepoint, or lower-level lock acquisition
// under this guard. The collector acquires it after stopping mutators, so it
// scans the same definitions regardless of which thread initiated collection.
static CLOS_STATE: LazyLock<OrderedMutex<ClosState>> = LazyLock::new(|| {
    OrderedMutex::new(LockLevel::GcWorld, 8, "CLOS definitions", ClosState::new())
});

fn scan_clos_state_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    visit_clos_roots(visit, true);
}

fn visit_clos_roots(visit: &mut dyn FnMut(*mut EgclVal), root_definitions: bool) {
    {
        let mut state = CLOS_STATE.lock().unwrap();
        // Discard derived values before relocation rather than retaining and
        // tracing a second copy of every cached class reference. Queries cannot
        // run concurrently while the collector holds this registry's lock.
        state.cpl_cache.get_mut().clear();
        // `class_registry`'s keys are class NAME symbols and `class_by_name`'s
        // are Rust strings, so neither needs visiting; the values are class
        // values and do.
        //
        // What this used to say was "registry keys are symbols or private
        // meta-handles and never move", which was the justification for
        // visiting no key at all. It stops being true once a class metaobject
        // is a heap object (bliss-rj5o), and `class_meta` is keyed by the class
        // VALUE. See the key-marking loops below.
        for value in state.class_registry.values_mut() {
            visit(value);
        }
        for value in state.class_by_name.values_mut() {
            visit(value);
        }
        // `class_meta`'s keys and `structure_classes`' members ARE class
        // values, and a class can be reachable through nothing else:
        // `bind_class_name(name, NIL)` — `(setf (find-class 'x) nil)`, which
        // CLHS requires — drops the `class_registry` and `class_by_name`
        // entries and leaves `class_meta` alone. An obsolete
        // pre-redefinition class reaches the same state.
        //
        // Marked through a COPY, which is the whole trick. A `HashMap` key
        // cannot be handed out as `&mut` (rewriting it would need a rehash),
        // and classes do not need one: they never move. So yield a throwaway
        // copy purely so the collector marks the object, and assert the copy
        // came back unchanged — that assert IS the test for the invariant.
        // If a class ever becomes movable it fires here, loudly, instead of
        // silently leaving a stale key behind; at that point this has to
        // become a real take/visit/reinsert rehash (the pattern is in
        // devtools.rs `scan_devtools_roots`), which must be idempotent because
        // scanners run twice per collection, once to mark and once to relocate.
        for key in state.class_meta.keys() {
            let mut marked = *key;
            visit(&mut marked);
            debug_assert_eq!(
                marked, *key,
                "a class metaobject moved: class_meta's keys need rehashing, not marking"
            );
        }
        for class in &state.structure_classes {
            let mut marked = *class;
            visit(&mut marked);
            debug_assert_eq!(
                marked, *class,
                "a structure class moved: structure_classes needs rehashing, not marking"
            );
        }
        for meta in state.class_meta.values_mut() {
            if let Some(definition) = &mut meta.runtime_definition {
                egcl_rt::gc::TraceHostRoots::trace_host_roots(definition, visit);
            }

            visit(&mut meta.protocol.direct_slots);
            visit(&mut meta.protocol.effective_slots);
            visit(&mut meta.protocol.default_initargs);
            visit(&mut meta.protocol.runtime_default_initargs);
            visit(&mut meta.protocol.runtime_slots);
            if let Some(cpl) = &mut meta.protocol.cpl {
                for value in cpl {
                    visit(value);
                }
            }
            if let Some(slots) = &mut meta.protocol.instance_slots {
                for value in slots {
                    visit(value);
                }
            }
            visit(&mut meta.name);
            for value in &mut meta.direct_supers {
                visit(value);
            }
            for value in &mut meta.direct_subs {
                visit(value);
            }
            for value in &mut meta.slots {
                visit(value);
            }
            // `:allocation :class` slot NAMES. Symbols today, so this is
            // currently a no-op — but the field was simply missed, and the
            // scanner is the wrong place to rely on a value's representation.
            for value in &mut meta.shared_slots {
                visit(value);
            }
            // The metaclass is a class value like any other, so a moving
            // collection must rewrite it here too -- missing it would leave a
            // stale class handle behind the first GC after a DEFCLASS with
            // :metaclass.
            visit(&mut meta.metaclass);
            // `meta.wrapper` is deliberately not visited. It is a leaked Rust
            // Box holding its own `class: EgclVal` copy, plus a `SlotLayout`
            // of slot-name symbols, all outside the GC heap where no scanner
            // can reach them — and `class_of` reads that very word. What makes
            // the copy safe is not that it is an immediate (the old reason
            // given here, and wrong once a class is a heap object) but that a
            // class is immortal and never moves: registry-reachable for the
            // process lifetime, and pinned once bliss-vsjn1 lands. It is also
            // the word `trace_object` treats as raw for a STANDARD_OBJECT, so
            // an instance does not root its own class; the registry does.
        }
        if root_definitions {
            for data in state.generic_functions.values_mut() {
                visit(&mut data.name);
                visit(&mut data.lambda_list);
                for method in &mut data.methods {
                    visit(method);
                }
            }
            for meta in state.method_meta.values_mut() {
                for specializer in &mut meta.specializers {
                    visit(specializer);
                }
            }
            for method in state.effective_methods.values_mut() {
                for group in [
                    &mut method.around,
                    &mut method.before,
                    &mut method.primary,
                    &mut method.after,
                ] {
                    for value in group {
                        visit(value);
                    }
                }
            }
            for method in state.short_form_methods.values_mut() {
                for value in &mut method.methods {
                    visit(value);
                }
            }
        }
        // Instance slot cells are NOT yielded here (bliss-334): instances are
        // ordinary GC-heap objects now, so the collector's own STANDARD_OBJECT
        // tracer marks and rewrites their slots. The old `live_instances`
        // slot-yield existed because instances were std::alloc'd off-heap —
        // and it force-rooted every instance forever.
        for (_, class) in &mut state.fixnum_registrations {
            visit(class);
        }
        visit(&mut state.fixnum_class);
        visit(&mut state.character_class);
        visit(&mut state.symbol_class);
        visit(&mut state.null_class);
        visit(&mut state.t_class_val);
        visit(&mut state.standard_object_class);
        visit(&mut state.standard_class);
        visit(&mut state.structure_class);
        visit(&mut state.forward_referenced_class);
        visit(&mut state.cons_class);
        visit(&mut state.float_class);
        visit(&mut state.function_class);
        visit(&mut state.heap_object_class);
    }
}

/// Registry ownership edges for stopped-world shake analysis. Keys are
/// stable meta-handles; payload values must not escape the heap snapshot.
pub struct ShakeDefinitions {
    pub generics: Vec<(EgclVal, EgclVal)>,
    pub edges: HashMap<EgclVal, Vec<EgclVal>>,
}

pub fn shake_root_scanner() -> egcl_rt::gc::RootScanner {
    scan_clos_state_roots
}

/// Keep class state rooted while generic and method ownership is traced by
/// the shake graph. Call only under a nonallocating heap snapshot.
pub fn visit_shake_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    visit_clos_roots(visit, false);
}

pub fn shake_definitions() -> ShakeDefinitions {
    let state = CLOS_STATE.lock().unwrap();
    let mut edges: HashMap<EgclVal, Vec<EgclVal>> = HashMap::new();
    let mut generics = Vec::new();
    for (&handle, data) in &state.generic_functions {
        generics.push((handle, data.name));
        let refs = edges.entry(handle).or_default();
        refs.extend([data.name, data.lambda_list]);
        refs.extend(&data.methods);
        for &method in &data.methods {
            edges.entry(method).or_default().push(handle);
        }
    }
    for (&handle, method) in &state.method_meta {
        edges
            .entry(handle)
            .or_default()
            .extend(&method.specializers);
    }
    for (&handle, method) in &state.effective_methods {
        let refs = edges.entry(handle).or_default();
        for group in [
            &method.around,
            &method.before,
            &method.primary,
            &method.after,
        ] {
            refs.extend(group);
        }
    }
    for (&handle, method) in &state.short_form_methods {
        edges.entry(handle).or_default().extend(&method.methods);
    }
    ShakeDefinitions { generics, edges }
}

/// Remove unreachable metadata before serializing the disposable shake
/// world. Surviving handles and their allocation counters retain their IDs.
pub fn retain_shake_definitions(live: &HashSet<u64>) {
    let mut state = CLOS_STATE.lock().unwrap();
    state
        .generic_functions
        .retain(|handle, _| live.contains(&handle.to_raw()));
    state
        .method_meta
        .retain(|handle, _| live.contains(&handle.to_raw()));
    state
        .effective_methods
        .retain(|handle, _| live.contains(&handle.to_raw()));
    state
        .short_form_methods
        .retain(|handle, _| live.contains(&handle.to_raw()));
}

fn install_clos_state_root_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_clos_state_roots));
}

fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&ClosState) -> R,
{
    f(&CLOS_STATE.lock().unwrap())
}

fn with_state_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ClosState) -> R,
{
    {
        let mut state = CLOS_STATE.lock().unwrap();
        // Even registration of a different class can change inferred supers.
        // Invalidating here also covers bootstrap, redefinition and restoration
        // without maintaining a separate hierarchy-generation protocol.
        state.cpl_cache.get_mut().clear();
        f(&mut state)
    }
}

// ── Core-image serialization of CLOS state (egcl-x0f2.7a) ─────────────
//
// A core load skips bootstrap, so CLOS_STATE (class/GF/method registries) must
// ride the image or macro expansion / condition signaling in the loaded core
// dies (fresh Env → initialize_condition_runtime_support → class_of → null).
// Every EgclVal held here is either a heap value the snapshot already carries
// (class objects, lambda lists, specializers) or an immediate meta-handle /
// symbol — so records carry raw u64s remapped on restore, exactly like the
// macro/setf HostRegistries records. The field set mirrors
// `scan_clos_state_roots` (whatever the GC roots, the dump must capture).
// `ClassMeta.wrapper` (a raw `*mut ClassWrapper`) is NOT serialized: wrappers
// restore null and are lazily re-finalized from the restored `class_meta.slots`
// by `finalize_class_layout` on the next instance allocation. Pre-save
// INSTANCES therefore still hold dangling wrapper words after a load — that
// remap is egcl-x0f2.7b.

fn cs_put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn cs_put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn cs_put_val(out: &mut Vec<u8>, v: EgclVal) {
    cs_put_u64(out, v.to_raw());
}

fn cs_put_vals(out: &mut Vec<u8>, vs: &[EgclVal]) {
    cs_put_u32(out, vs.len() as u32);
    for v in vs {
        cs_put_val(out, *v);
    }
}

fn cs_put_str(out: &mut Vec<u8>, s: &str) {
    cs_put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn cs_get_u32(data: &[u8], off: &mut usize) -> Option<u32> {
    if data.len() < *off + 4 {
        return None;
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Some(v)
}

fn cs_get_u64(data: &[u8], off: &mut usize) -> Option<u64> {
    if data.len() < *off + 8 {
        return None;
    }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Some(v)
}

fn cs_get_u8(data: &[u8], off: &mut usize) -> Option<u8> {
    if data.len() < *off + 1 {
        return None;
    }
    let v = data[*off];
    *off += 1;
    Some(v)
}

fn cs_get_val(data: &[u8], off: &mut usize, remap: &dyn Fn(u64) -> u64) -> Option<EgclVal> {
    cs_get_u64(data, off).map(|raw| EgclVal::from_raw(remap(raw)))
}

fn cs_get_vals(data: &[u8], off: &mut usize, remap: &dyn Fn(u64) -> u64) -> Option<Vec<EgclVal>> {
    let n = cs_get_u32(data, off)? as usize;
    let mut vs = Vec::with_capacity(n);
    for _ in 0..n {
        vs.push(cs_get_val(data, off, remap)?);
    }
    Some(vs)
}

fn cs_get_str(data: &[u8], off: &mut usize) -> Option<String> {
    let len = cs_get_u32(data, off)? as usize;
    if data.len() < *off + len {
        return None;
    }
    let s = String::from_utf8_lossy(&data[*off..*off + len]).into_owned();
    *off += len;
    Some(s)
}

fn qualifier_to_u8(q: MethodQualifier) -> u8 {
    match q {
        MethodQualifier::Primary => 0,
        MethodQualifier::Before => 1,
        MethodQualifier::After => 2,
        MethodQualifier::Around => 3,
        MethodQualifier::Combination => 4,
        MethodQualifier::Other => 5,
    }
}

fn u8_to_qualifier(b: u8) -> MethodQualifier {
    match b {
        1 => MethodQualifier::Before,
        2 => MethodQualifier::After,
        3 => MethodQualifier::Around,
        4 => MethodQualifier::Combination,
        5 => MethodQualifier::Other,
        _ => MethodQualifier::Primary,
    }
}

fn combination_to_u8(c: MethodCombinationType) -> u8 {
    match c {
        MethodCombinationType::Standard => 0,
        MethodCombinationType::Plus => 1,
        MethodCombinationType::And => 2,
        MethodCombinationType::Or => 3,
        MethodCombinationType::List => 4,
        MethodCombinationType::Append => 5,
        MethodCombinationType::Nconc => 6,
        MethodCombinationType::Min => 7,
        MethodCombinationType::Max => 8,
        MethodCombinationType::Progn => 9,
    }
}

fn u8_to_combination(b: u8) -> MethodCombinationType {
    match b {
        1 => MethodCombinationType::Plus,
        2 => MethodCombinationType::And,
        3 => MethodCombinationType::Or,
        4 => MethodCombinationType::List,
        5 => MethodCombinationType::Append,
        6 => MethodCombinationType::Nconc,
        7 => MethodCombinationType::Min,
        8 => MethodCombinationType::Max,
        9 => MethodCombinationType::Progn,
        _ => MethodCombinationType::Standard,
    }
}

/// Serialize the whole `CLOS_STATE` for a core image. Runs after the save-time
/// stop-the-world full GC; only reads raw tagged words (no EGCL allocation),
/// so it is GC-safe.
pub fn serialize_clos_state() -> Vec<u8> {
    let mut out = with_state(|st| {
        let mut out = Vec::new();
        out.extend_from_slice(b"CLST");
        // 3 added ClassMeta.metaclass; 4 added ClassMeta.shared_slots; 5 added
        // ClosState::standard_class and ::structure_class; 6 adds
        // ClassMeta.forward_referenced, so a class another class named as a
        // superclass before it was defined is still replaceable after a
        // restore (GitHub #384).
        //
        // No reader compatibility with 3 is kept, and none is possible to
        // need: an image also carries a hash of the runtime's source and is
        // refused outright on a mismatch ("runtime source mismatch"), so a
        // core written by any other build never reaches this version check.
        // Measured — a core from the previous commit's binary is rejected at
        // the header, not here.
        // 7 adds rooted class-protocol caches and computed layout/CPL state.
        // 8 adds identity-owned runtime source definitions and shared cells.
        cs_put_u32(&mut out, 8);
        cs_put_u32(&mut out, st.class_registry.len() as u32);
        for (k, v) in &st.class_registry {
            cs_put_val(&mut out, *k);
            cs_put_val(&mut out, *v);
        }
        cs_put_u32(&mut out, st.class_by_name.len() as u32);
        for (k, v) in &st.class_by_name {
            cs_put_str(&mut out, k);
            cs_put_val(&mut out, *v);
        }
        cs_put_u32(&mut out, st.class_meta.len() as u32);
        for (class, meta) in &st.class_meta {
            cs_put_val(&mut out, *class);
            cs_put_val(&mut out, meta.name);
            cs_put_vals(&mut out, &meta.direct_supers);
            cs_put_vals(&mut out, &meta.direct_subs);
            cs_put_vals(&mut out, &meta.slots);
            cs_put_val(&mut out, meta.metaclass);
            cs_put_vals(&mut out, &meta.shared_slots);
            out.push(meta.forward_referenced as u8);
            out.push(meta.protocol.direct_ready as u8);
            cs_put_val(&mut out, meta.protocol.direct_slots);
            cs_put_val(&mut out, meta.protocol.effective_slots);
            cs_put_val(&mut out, meta.protocol.default_initargs);
            cs_put_val(&mut out, meta.protocol.runtime_default_initargs);
            cs_put_val(&mut out, meta.protocol.runtime_slots);
            out.push(meta.protocol.computed as u8);
            out.push(meta.protocol.finalized as u8);
            out.push(meta.protocol.cpl.is_some() as u8);
            cs_put_vals(&mut out, meta.protocol.cpl.as_deref().unwrap_or(&[]));
            out.push(meta.protocol.instance_slots.is_some() as u8);
            cs_put_vals(
                &mut out,
                meta.protocol.instance_slots.as_deref().unwrap_or(&[]),
            );
            out.push(meta.runtime_definition.is_some() as u8);
            if let Some(definition) = &meta.runtime_definition {
                runtime_definition::snapshot_definition(&mut out, definition);
            }
        }
        cs_put_u32(&mut out, st.generic_functions.len() as u32);
        for (gf, data) in &st.generic_functions {
            cs_put_val(&mut out, *gf);
            cs_put_val(&mut out, data.name);
            cs_put_val(&mut out, data.lambda_list);
            cs_put_vals(&mut out, &data.methods);
        }
        cs_put_u32(&mut out, st.method_meta.len() as u32);
        for (method, meta) in &st.method_meta {
            cs_put_val(&mut out, *method);
            out.push(qualifier_to_u8(meta.qualifier));
            cs_put_vals(&mut out, &meta.specializers);
        }
        cs_put_u32(&mut out, st.effective_methods.len() as u32);
        for (key, em) in &st.effective_methods {
            cs_put_val(&mut out, *key);
            cs_put_vals(&mut out, &em.around);
            cs_put_vals(&mut out, &em.before);
            cs_put_vals(&mut out, &em.primary);
            cs_put_vals(&mut out, &em.after);
        }
        cs_put_u32(&mut out, st.short_form_methods.len() as u32);
        for (key, sf) in &st.short_form_methods {
            cs_put_val(&mut out, *key);
            out.push(combination_to_u8(sf.combination));
            cs_put_vals(&mut out, &sf.methods);
        }
        cs_put_u64(&mut out, st.next_em_key as u64);
        cs_put_u64(&mut out, st.next_gf_id as u64);
        let structs: Vec<EgclVal> = st.structure_classes.iter().copied().collect();
        cs_put_vals(&mut out, &structs);
        cs_put_u32(&mut out, st.fixnum_registrations.len() as u32);
        for (id, class) in &st.fixnum_registrations {
            cs_put_u64(&mut out, *id as u64);
            cs_put_val(&mut out, *class);
        }
        for v in [
            st.fixnum_class,
            st.character_class,
            st.symbol_class,
            st.null_class,
            st.t_class_val,
            st.standard_object_class,
            st.standard_class,
            st.structure_class,
            st.cons_class,
            st.float_class,
            st.function_class,
            st.heap_object_class,
        ] {
            cs_put_val(&mut out, v);
        }
        out.push(st.bootstrapped as u8);
        out
    });
    // ── Wrappers (egcl-x0f2.7b) ──
    // Every restored STANDARD_OBJECT instance's offset-8 word is a raw
    // `*mut ClassWrapper` the loader must remap. Serialize each DISTINCT live
    // wrapper keyed by its save-time address: the classes' current wrappers,
    // plus any wrapper still referenced from a heap instance (covers obsolete
    // wrappers on stale instances after a class redefinition). Runs with no
    // heap lock held (image.rs calls the hook at top level), so walk_heap can
    // take it.
    let current: Vec<(EgclVal, usize)> = with_state(|st| {
        st.class_meta
            .iter()
            .filter_map(|(c, m)| m.wrapper.map(|w| (*c, w as *const ClassWrapper as usize)))
            .collect()
    });
    let mut addrs: Vec<usize> = current.iter().map(|(_, w)| *w).collect();
    let _ = egcl_rt::gc::walk_heap(|body, tid, _size| {
        if tid == type_id::STANDARD_OBJECT {
            // SAFETY: walk_heap yields valid object BODY pointers; an
            // instance's first body word is its wrapper pointer (or null).
            let w = unsafe { *(body as *const usize) };
            if w != 0 {
                addrs.push(w);
            }
        }
        true
    });
    addrs.sort_unstable();
    addrs.dedup();
    cs_put_u32(&mut out, addrs.len() as u32);
    for &addr in &addrs {
        // SAFETY: every collected address is a live leaked ClassWrapper.
        let w = addr as *const ClassWrapper;
        unsafe {
            cs_put_u64(&mut out, addr as u64);
            cs_put_u64(&mut out, (*w).stamp);
            out.push((*w).state.load(Ordering::Acquire));
            cs_put_val(&mut out, (*w).class);
            cs_put_u32(&mut out, (*w).slot_count);
            cs_put_vals(&mut out, &(*w).layout.order);
        }
    }
    cs_put_u32(&mut out, current.len() as u32);
    for (class, addr) in &current {
        cs_put_val(&mut out, *class);
        cs_put_u64(&mut out, *addr as u64);
    }
    out
}

/// Restore `CLOS_STATE` from a core image's serialized record, remapping every
/// stored value through `remap` (the heap's final old→new map). Replaces the
/// state wholesale: any pre-load registrations point at the discarded pre-load
/// heap and must not survive. Wrappers restore null (see module comment).
/// GC-safe: pure Rust parsing + map inserts, no EGCL allocation. Returns the
/// number of bytes consumed so the caller can parse blocks appended after this
/// one in the same section.
pub fn restore_clos_state(data: &[u8], remap: &dyn Fn(u64) -> u64) -> Result<usize, EgclError> {
    // The restore rewrites wrapper addresses, including preallocated reserves.
    crate::conditions::reset_storage_condition_pool();
    let bad = || EgclError::InvalidImage("CLOS state section: truncated".into());
    if data.len() < 8 || &data[..4] != b"CLST" {
        return Err(EgclError::InvalidImage(
            "CLOS state section: bad marker".into(),
        ));
    }
    let mut off = 4usize;
    let version = cs_get_u32(data, &mut off).ok_or_else(bad)?;
    if version != 8 {
        return Err(EgclError::InvalidImage(format!(
            "CLOS state section: unsupported version {version}"
        )));
    }
    let mut st = ClosState::new();
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let k = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let v = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        st.class_registry.insert(k, v);
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let k = cs_get_str(data, &mut off).ok_or_else(bad)?;
        let v = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        st.class_by_name.insert(k, v);
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let name = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let direct_supers = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let direct_subs = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let slots = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let metaclass = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let shared_slots = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let forward_referenced = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let direct_ready = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let direct_slots = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let effective_slots = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let default_initargs = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let runtime_default_initargs = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let runtime_slots = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let computed = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let finalized = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let has_cpl = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let cpl = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let has_slots = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
        let instance_slots = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let runtime_definition = match cs_get_u8(data, &mut off).ok_or_else(bad)? {
            0 => None,
            1 => Some(
                runtime_definition::restore_definition(data, &mut off, remap).ok_or_else(bad)?,
            ),
            _ => return Err(bad()),
        };
        if runtime_definition
            .as_ref()
            .is_some_and(|definition| definition.owner != class)
        {
            return Err(EgclError::InvalidImage(
                "CLOS state section: mismatched runtime class owner".into(),
            ));
        }
        let protocol = ClassProtocol {
            direct_slots,
            direct_ready,
            effective_slots,
            default_initargs,
            runtime_default_initargs,
            runtime_slots,
            computed,
            finalized,
            cpl: has_cpl.then_some(cpl),
            instance_slots: has_slots.then_some(instance_slots),
        };
        st.class_meta.insert(
            class,
            ClassMeta {
                runtime_definition,
                protocol,
                shared_slots,
                name,
                direct_supers,
                direct_subs,
                slots,
                wrapper: None,
                metaclass,
                forward_referenced,
            },
        );
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let gf = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let name = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let lambda_list = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let methods = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        st.generic_functions.insert(
            gf,
            GFData {
                name,
                lambda_list,
                methods,
            },
        );
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let method = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let qualifier = u8_to_qualifier(cs_get_u8(data, &mut off).ok_or_else(bad)?);
        let specializers = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        st.method_meta.insert(
            method,
            MethodMeta {
                specializers,
                qualifier,
            },
        );
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let key = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let around = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let before = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let primary = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        let after = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        st.effective_methods.insert(
            key,
            EffectiveMethod {
                around,
                before,
                primary,
                after,
            },
        );
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let key = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let combination = u8_to_combination(cs_get_u8(data, &mut off).ok_or_else(bad)?);
        let methods = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        st.short_form_methods.insert(
            key,
            ShortFormMethod {
                combination,
                methods,
            },
        );
    }
    st.next_em_key = cs_get_u64(data, &mut off).ok_or_else(bad)? as i64;
    st.next_gf_id = cs_get_u64(data, &mut off).ok_or_else(bad)? as i64;
    for v in cs_get_vals(data, &mut off, remap).ok_or_else(bad)? {
        st.structure_classes.insert(v);
    }
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let id = cs_get_u64(data, &mut off).ok_or_else(bad)? as i64;
        let class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        st.fixnum_registrations.push((id, class));
    }
    st.fixnum_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.character_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.symbol_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.null_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.t_class_val = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.standard_object_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.standard_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.structure_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.cons_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.float_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.function_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.heap_object_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.bootstrapped = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
    // ── Wrappers (egcl-x0f2.7b) ──
    // Recreate each saved ClassWrapper/SlotLayout (leaked, like
    // finalize_class_layout), building old→new so restored instances' offset-8
    // words can be rewritten below. Stamps are preserved (instances and their
    // wrappers must agree); CLASS_STAMP is advanced past the maximum so future
    // wrappers never collide.
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    let mut wrapper_map: HashMap<usize, usize> = HashMap::with_capacity(n);
    let mut max_stamp = 0u64;
    for _ in 0..n {
        let old_addr = cs_get_u64(data, &mut off).ok_or_else(bad)? as usize;
        let stamp = cs_get_u64(data, &mut off).ok_or_else(bad)?;
        let state = cs_get_u8(data, &mut off).ok_or_else(bad)?;
        let class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let slot_count = cs_get_u32(data, &mut off).ok_or_else(bad)?;
        let order = cs_get_vals(data, &mut off, remap).ok_or_else(bad)?;
        max_stamp = max_stamp.max(stamp);
        let mut index = HashMap::with_capacity(order.len());
        for (i, name) in order.iter().enumerate() {
            index.insert(*name, i);
        }
        let layout = Box::leak(Box::new(SlotLayout::new(order, index)));
        let wrapper: &'static ClassWrapper = Box::leak(Box::new(ClassWrapper {
            stamp,
            state: AtomicU8::new(state),
            class,
            slot_count,
            layout,
            struct_info: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        }));
        wrapper_map.insert(old_addr, wrapper as *const ClassWrapper as usize);
    }
    // Advance the global stamp counter past every restored stamp.
    let mut cur = CLASS_STAMP.load(Ordering::Relaxed);
    while cur <= max_stamp {
        match CLASS_STAMP.compare_exchange(cur, max_stamp + 1, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(seen) => cur = seen,
        }
    }
    // Point each class's meta at its recreated current wrapper.
    let n = cs_get_u32(data, &mut off).ok_or_else(bad)? as usize;
    for _ in 0..n {
        let class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
        let old_addr = cs_get_u64(data, &mut off).ok_or_else(bad)? as usize;
        if let (Some(meta), Some(&new_addr)) =
            (st.class_meta.get_mut(&class), wrapper_map.get(&old_addr))
        {
            // Each address in wrapper_map was just leaked above and is immutable.
            meta.wrapper = Some(unsafe { &*(new_addr as *const ClassWrapper) });
        }
    }
    with_state_mut(|state| {
        *state = st;
        let structs: Vec<EgclVal> = state.structure_classes.iter().copied().collect();
        for class in structs {
            install_struct_info(state, class);
        }
    });
    install_clos_state_root_scanner();
    // Rewrite every restored instance's wrapper word through the map. An
    // unmatched (impossible unless the image is inconsistent) or null word is
    // nulled so a later use fails the null check instead of deref'ing a stale
    // save-time address. Runs after the host hook regained control, so no heap
    // lock is held — walk_heap takes it.
    egcl_rt::gc::walk_heap(|body, tid, _size| {
        if tid == type_id::STANDARD_OBJECT {
            // SAFETY: walk_heap yields valid object BODY pointers; the first
            // body word of a STANDARD_OBJECT instance is its wrapper word.
            unsafe {
                let slot = body as *mut usize;
                let old = *slot;
                *slot = wrapper_map.get(&old).copied().unwrap_or(0);
            }
        }
        true
    })?;
    Ok(off)
}

/// The maximum metaobject-handle id held anywhere in the restored CLOS state
/// (class and method ids are minted from the interpreter's shared class-id
/// counter). The core-image loader uses this to advance that counter past every
/// restored id, so classes/methods defined after the load never re-mint an id
/// that aliases a restored one (bliss-66io). Effective-method / short-form keys
/// live in a separate counter range but are included for safety — advancing the
/// class-id counter past them is harmless.
pub fn max_metaobject_id() -> i64 {
    with_state(|st| {
        let mut max = 0i64;
        let mut bump = |v: EgclVal| {
            if v.is_meta_handle() {
                let id = v.as_meta_handle_id();
                if id > max {
                    max = id;
                }
            }
        };
        for (k, v) in &st.class_registry {
            bump(*k);
            bump(*v);
        }
        for v in st.class_by_name.values() {
            bump(*v);
        }
        for k in st.class_meta.keys() {
            bump(*k);
        }
        for k in st.generic_functions.keys() {
            bump(*k);
        }
        for k in st.method_meta.keys() {
            bump(*k);
        }
        for v in &st.structure_classes {
            bump(*v);
        }
        for v in [
            st.fixnum_class,
            st.character_class,
            st.symbol_class,
            st.null_class,
            st.t_class_val,
            st.standard_object_class,
            st.cons_class,
            st.float_class,
            st.function_class,
            st.heap_object_class,
        ] {
            bump(v);
        }
        for (_, class) in &st.fixnum_registrations {
            bump(*class);
        }
        max
    })
}

fn is_builtin_class(st: &ClosState, class: EgclVal) -> bool {
    // A built-in class is one whose instances are NOT standard objects, which
    // is what its precedence list records. The sign of a fixnum used to be the
    // discriminator ("every built-in class handle is a NEGATIVE fixnum"), and
    // that stopped being true when these became pinned heap objects
    // (bliss-rj5o).
    //
    // A structure class also fails the standard-object test here, so it is
    // excluded first — DEFSTRUCT classes are not built-in and must keep
    // getting a layout.
    if st.structure_classes.contains(&class) {
        return false;
    }
    if st.standard_object_class == NIL {
        // Early bootstrap: STANDARD-OBJECT is not registered yet, so no class
        // can be shown to inherit it. Answer "built-in", which is the
        // conservative direction: `finalize_class_layout` then declines to
        // build a layout, and nothing should be instantiated this early.
        return true;
    }
    linearize_class_precedence(st, class)
        .map(|cpl| !cpl.contains(&st.standard_object_class))
        .unwrap_or(true)
}

// ── CLOS bootstrap ─────────────────────────────────────────────────

/// Initialize the CLOS bootstrap: create proto-classes, wire up metaclass
/// circularity. R5.10.
/// Bootstrap the CLOS state only if it has not been bootstrapped yet, preserving
/// any classes already defined. `Env::new` calls this (not [`bootstrap_clos`])
/// because transient environments — e.g. the macro-expansion env built for every
/// macro/compiler-macro call — must NOT wipe the process-global class registry
/// mid-load. (Tests still call [`bootstrap_clos`] directly to force a fresh
/// state.)
pub fn ensure_clos_bootstrapped() -> Result<(), EgclError> {
    if with_state(|st| st.bootstrapped) {
        return Ok(());
    }
    initialize_clos(false)
}

/// Reset CLOS in a quiescent runtime (startup or an isolated test fixture).
/// Worker entry must use `ensure_clos_bootstrapped` instead.
pub fn bootstrap_clos() -> Result<(), EgclError> {
    crate::conditions::reset_storage_condition_pool();
    initialize_clos(true)
}

fn initialize_clos(reset: bool) -> Result<(), EgclError> {
    install_clos_state_root_scanner();

    // The built-in class metaobjects. Each is a pinned, zero-slot
    // STANDARD_OBJECT (see `alloc_class_metaobject`), allocated BEFORE the
    // `with_state_mut` below: the CLOS definitions lock is (GcWorld, 8) and the
    // GC heap lock is (GcWorld, 2), so allocating while holding the former
    // fails the lock-order check in a debug build and inverts the collector's
    // own 2 -> 8 order in release. Same discipline as the interning above
    // (bliss-wlf), for the same reason.
    //
    // These used to be negative fixnums -1 ..= -47 (with -37 unused), which is
    // why `(eql -14 (find-class 'integer))` and `(integerp (find-class
    // 'standard-class))` were both true (bliss-rj5o). Pinning is what lets a
    // class value go on being a HashMap key in ClosState.
    //
    // The wrapper words are filled in below, once the metaclass values those
    // wrappers must name exist. NOTHING may read a class's wrapper before
    // then: `class_of` dereferences it with no null check.
    // Built-in class values (negative fixnums avoid collision with user classes)
    // Additional CL built-in classes (bliss-qxfg). find-class previously had no
    // metaobject for the numeric tower / sequence / array classes, so
    // `(find-class 'integer)` — and FIND-METHOD over `(find-class …)` specializers,
    // as ansi-test universe.lsp builds — errored. These are additive: class-of
    // still returns the concrete leaf (FIXNUM/FLOAT/…) and SUBTYPEP uses the type
    // lattice, not the CLOS CPL, so registering them changes neither.
    // The remaining standard classes of CLHS 4.3.7 / figure 4-8 that FIND-CLASS
    // must know (ansi symbols cl-types-that-are-classes.*; the objects chapter
    // names STANDARD-GENERIC-FUNCTION while loading). Handles continue the
    // negative range; nothing dispatches on them beyond FIND-CLASS,
    // CLASS-NAME, CLASS-PRECEDENCE-LIST and TYPEP of the metaobject. RESTART is
    // deliberately absent: the runtime defines it as a standard class whose
    // instances are the first-class restart objects (ensure_restart_class).
    let t_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let std_obj = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let fix_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let chr_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let sym_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let nul_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let con_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let flt_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let fun_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let hpo_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let num_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let real_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let ratl_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let int_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let ratio_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let cplx_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let seq_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let list_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let arr_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let vec_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let str_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let bitv_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let htbl_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let stream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let bstream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let cstream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let estream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let fstream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let sstream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let ystream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let tstream_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let path_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let lpath_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let pkg_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let rstate_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let rtable_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let sobj_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let class_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let sclass_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let bclass_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let frc_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a class".into()))?;
    let stclass_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let gf_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let sgf_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let method_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let smethod_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;
    let mcomb_cls = alloc_class_metaobject()
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for a built-in class".into()))?;

    // Names: interned into the shared registry by their real CL names
    // (bliss-jtc.6 Stage E) so a built-in class's CLASS-NAME / TYPE-OF is the
    // same symbol the reader and interpreter produce — no reserved 0xFFFE_*
    // indices that don't correspond to any actual symbol.
    //
    // GC safety: `intern` allocates and can fire a minor GC whose root scan
    // re-enters CLOS_STATE, so ALL interning happens *before* the `with_state_mut`
    // borrow below (bliss-wlf). Once the borrow is held the closure only stores
    // these already-interned immediates — it never allocates.
    // Fill in each class metaobject's wrapper word, now that the metaclass
    // values the wrappers name exist. Two wrappers suffice: a class metaobject
    // has no instance slots, so they differ only in which class they report.
    //
    // Which class each belongs to reproduces exactly what CLASS-OF answered
    // when these were fixnums (measured as a 34-row table before the change):
    // the ten AMOP metaobject classes are instances of STANDARD-CLASS, and
    // every other built-in class — STRUCTURE-OBJECT included — is an instance
    // of BUILT-IN-CLASS.
    let standard_class_wrapper = leak_empty_wrapper(sclass_cls);
    let builtin_class_wrapper = leak_empty_wrapper(bclass_cls);
    for class in [
        std_obj,
        class_cls,
        sclass_cls,
        bclass_cls,
        stclass_cls,
        // FORWARD-REFERENCED-CLASS is an AMOP metaobject class like its three
        // siblings above, so it is itself an instance of STANDARD-CLASS. Giving
        // it no wrapper left its own word NULL and `class_of` dereferenced it
        // (R9.39).
        frc_cls,
        gf_cls,
        sgf_cls,
        method_cls,
        smethod_cls,
        mcomb_cls,
    ] {
        unsafe { set_instance_wrapper(class, standard_class_wrapper) };
    }
    for class in [
        t_cls,
        fix_cls,
        chr_cls,
        sym_cls,
        nul_cls,
        con_cls,
        flt_cls,
        fun_cls,
        hpo_cls,
        num_cls,
        real_cls,
        ratl_cls,
        int_cls,
        ratio_cls,
        cplx_cls,
        seq_cls,
        list_cls,
        arr_cls,
        vec_cls,
        str_cls,
        bitv_cls,
        htbl_cls,
        stream_cls,
        bstream_cls,
        cstream_cls,
        estream_cls,
        fstream_cls,
        sstream_cls,
        ystream_cls,
        tstream_cls,
        path_cls,
        lpath_cls,
        pkg_cls,
        rstate_cls,
        rtable_cls,
        sobj_cls,
    ] {
        unsafe { set_instance_wrapper(class, builtin_class_wrapper) };
    }

    let nm = |name: &str| EgclVal::from_symbol_index(egcl_rt::symbols::intern(name));
    let t_nm = T;
    let std_nm = nm("STANDARD-OBJECT");
    let fix_nm = nm("FIXNUM");
    let chr_nm = nm("CHARACTER");
    let sym_nm = nm("SYMBOL");
    let nul_nm = nm("NULL");
    let con_nm = nm("CONS");
    let flt_nm = nm("FLOAT");
    let fun_nm = nm("FUNCTION");
    let hpo_nm = nm("HEAP-OBJECT");
    let num_nm = nm("NUMBER");
    let real_nm = nm("REAL");
    let ratl_nm = nm("RATIONAL");
    let int_nm = nm("INTEGER");
    let ratio_nm = nm("RATIO");
    let cplx_nm = nm("COMPLEX");
    let seq_nm = nm("SEQUENCE");
    let list_nm = nm("LIST");
    let arr_nm = nm("ARRAY");
    let vec_nm = nm("VECTOR");
    let str_nm = nm("STRING");
    let bitv_nm = nm("BIT-VECTOR");
    let htbl_nm = nm("HASH-TABLE");
    let stream_nm = nm("STREAM");
    let bstream_nm = nm("BROADCAST-STREAM");
    let cstream_nm = nm("CONCATENATED-STREAM");
    let estream_nm = nm("ECHO-STREAM");
    let fstream_nm = nm("FILE-STREAM");
    let sstream_nm = nm("STRING-STREAM");
    let ystream_nm = nm("SYNONYM-STREAM");
    let tstream_nm = nm("TWO-WAY-STREAM");
    let path_nm = nm("PATHNAME");
    let lpath_nm = nm("LOGICAL-PATHNAME");
    let pkg_nm = nm("PACKAGE");
    let rstate_nm = nm("RANDOM-STATE");
    let rtable_nm = nm("READTABLE");
    let sobj_nm = nm("STRUCTURE-OBJECT");
    let class_nm = nm("CLASS");
    let sclass_nm = nm("STANDARD-CLASS");
    let bclass_nm = nm("BUILT-IN-CLASS");
    let stclass_nm = nm("STRUCTURE-CLASS");
    // Interned into EGCL-MOP, which OWNS this name: it is AMOP, not ANSI, so
    // adding it to COMMON-LISP would be interning into a closed package (the
    // mistake #383 fixed for CLASS-PRECEDENCE-LIST). boot.lisp exports it from
    // there (spec R9.39).
    let frc_nm = nm("EGCL-MOP:FORWARD-REFERENCED-CLASS");
    let gf_nm = nm("GENERIC-FUNCTION");
    let sgf_nm = nm("STANDARD-GENERIC-FUNCTION");
    let method_nm = nm("METHOD");
    let smethod_nm = nm("STANDARD-METHOD");
    let mcomb_nm = nm("METHOD-COMBINATION");

    with_state_mut(|st| {
        // Another thread may have initialized CLOS while we interned names.
        if !reset && st.bootstrapped {
            return Ok(());
        }
        // Full reset so tests are independent
        *st = ClosState::new();

        // T — root, no supers
        st.class_registry.insert(t_nm, t_cls);
        st.class_meta.insert(
            t_cls,
            ClassMeta {
                runtime_definition: None,
                protocol: ClassProtocol::default(),
                shared_slots: Vec::new(),
                name: t_nm,
                direct_supers: vec![],
                direct_subs: vec![],
                slots: vec![],
                wrapper: None,
                metaclass: NIL,
                forward_referenced: false,
            },
        );

        // STANDARD-OBJECT (super: T)
        st.class_registry.insert(std_nm, std_obj);
        st.class_meta.insert(
            std_obj,
            ClassMeta {
                runtime_definition: None,
                protocol: ClassProtocol::default(),
                shared_slots: Vec::new(),
                name: std_nm,
                direct_supers: vec![t_cls],
                direct_subs: vec![],
                slots: vec![],
                wrapper: None,
                metaclass: NIL,
                forward_referenced: false,
            },
        );

        // All other built-in classes (super: STANDARD-OBJECT)
        let builtins = [
            (fix_nm, fix_cls),
            (chr_nm, chr_cls),
            (sym_nm, sym_cls),
            (nul_nm, nul_cls),
            (con_nm, con_cls),
            (flt_nm, flt_cls),
            (fun_nm, fun_cls),
            (hpo_nm, hpo_cls),
        ];
        for (nm, cls) in &builtins {
            // CLHS 4.3.7 / Figure 4-8: the built-in classes are not subclasses
            // of STANDARD-OBJECT; each sits under its own type (FIXNUM under
            // INTEGER, NULL under SYMBOL and LIST, CONS under LIST, FLOAT under
            // REAL) and the rest directly under T (ansi *-cpl, types.6).
            let supers = if *cls == fix_cls {
                vec![int_cls]
            } else if *cls == nul_cls {
                vec![sym_cls, list_cls]
            } else if *cls == con_cls {
                vec![list_cls]
            } else if *cls == flt_cls {
                vec![real_cls]
            } else {
                vec![t_cls]
            };
            st.class_registry.insert(*nm, *cls);
            st.class_meta.insert(
                *cls,
                ClassMeta {
                    runtime_definition: None,
                    protocol: ClassProtocol::default(),
                    shared_slots: Vec::new(),
                    name: *nm,
                    direct_supers: supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: None,
                    metaclass: NIL,
                    forward_referenced: false,
                },
            );
        }

        // CL built-in classes with their CLHS superclass links, forming a
        // coherent numeric tower and sequence/array hierarchy for FIND-CLASS /
        // CLASS-PRECEDENCE-LIST (bliss-qxfg). Purely additive — see the handle
        // comment above; the existing leaf classes (FIXNUM/FLOAT/CONS) keep their
        // current supers, so no CPL/dispatch behaviour changes.
        let hierarchy = [
            (num_nm, num_cls, vec![t_cls]),
            (real_nm, real_cls, vec![num_cls]),
            (ratl_nm, ratl_cls, vec![real_cls]),
            (int_nm, int_cls, vec![ratl_cls]),
            (ratio_nm, ratio_cls, vec![ratl_cls]),
            (cplx_nm, cplx_cls, vec![num_cls]),
            (seq_nm, seq_cls, vec![t_cls]),
            (list_nm, list_cls, vec![seq_cls]),
            (arr_nm, arr_cls, vec![t_cls]),
            (vec_nm, vec_cls, vec![arr_cls, seq_cls]),
            (str_nm, str_cls, vec![vec_cls]),
            (bitv_nm, bitv_cls, vec![vec_cls]),
            (htbl_nm, htbl_cls, vec![t_cls]),
            (stream_nm, stream_cls, vec![t_cls]),
            (bstream_nm, bstream_cls, vec![stream_cls]),
            (cstream_nm, cstream_cls, vec![stream_cls]),
            (estream_nm, estream_cls, vec![stream_cls]),
            (fstream_nm, fstream_cls, vec![stream_cls]),
            (sstream_nm, sstream_cls, vec![stream_cls]),
            (ystream_nm, ystream_cls, vec![stream_cls]),
            (tstream_nm, tstream_cls, vec![stream_cls]),
            (path_nm, path_cls, vec![t_cls]),
            (lpath_nm, lpath_cls, vec![path_cls]),
            (pkg_nm, pkg_cls, vec![t_cls]),
            (rstate_nm, rstate_cls, vec![t_cls]),
            (rtable_nm, rtable_cls, vec![t_cls]),
            (sobj_nm, sobj_cls, vec![t_cls]),
            // Metaobject classes (AMOP): CLASS, METHOD and METHOD-COMBINATION
            // are standard objects; a generic function is also a FUNCTION.
            (class_nm, class_cls, vec![std_obj]),
            (sclass_nm, sclass_cls, vec![class_cls]),
            (bclass_nm, bclass_cls, vec![class_cls]),
            (stclass_nm, stclass_cls, vec![class_cls]),
            // A forward-referenced class is a CLASS that is not yet any of the
            // three above: nothing has said what its instances look like.
            (frc_nm, frc_cls, vec![class_cls]),
            (gf_nm, gf_cls, vec![fun_cls, std_obj]),
            (sgf_nm, sgf_cls, vec![gf_cls]),
            (method_nm, method_cls, vec![std_obj]),
            (smethod_nm, smethod_cls, vec![method_cls]),
            (mcomb_nm, mcomb_cls, vec![std_obj]),
        ];
        for (nm, cls, supers) in hierarchy {
            st.class_registry.insert(nm, cls);
            st.class_meta.insert(
                cls,
                ClassMeta {
                    runtime_definition: None,
                    protocol: ClassProtocol::default(),
                    shared_slots: Vec::new(),
                    name: nm,
                    direct_supers: supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: None,
                    metaclass: NIL,
                    forward_referenced: false,
                },
            );
        }

        st.t_class_val = t_cls;
        st.standard_object_class = std_obj;
        st.standard_class = sclass_cls;
        st.structure_class = stclass_cls;
        st.forward_referenced_class = frc_cls;
        st.fixnum_class = fix_cls;
        st.character_class = chr_cls;
        st.symbol_class = sym_cls;
        st.null_class = nul_cls;
        st.cons_class = con_cls;
        st.float_class = flt_cls;
        st.function_class = fun_cls;
        st.heap_object_class = hpo_cls;
        st.bootstrapped = true;
        Ok(())
    })
}

// ── Class protocol ─────────────────────────────────────────────────

/// Find a class by name.
/// The bare, uppercase, package-stripped name of a class-name symbol, used as
/// the drift-resilient key for [`ClosState::class_by_name`]. Returns `None` for
/// a non-symbol name.
fn class_name_key(name: EgclVal) -> Option<String> {
    if !name.is_symbol() {
        return None;
    }
    // NIL and T are symbols at the Common Lisp level, but EGCL represents
    // them with the SPECIAL tag rather than TAG_SYMBOL.  Do not pass either
    // through as_symbol_index(), which deliberately accepts only TAG_SYMBOL.
    if name == NIL {
        return Some("NIL".into());
    }
    if name == T {
        return Some("T".into());
    }
    let full = egcl_rt::symbols::symbol_name(name.as_symbol_index())?;
    // KEEP THE PACKAGE. This key used to be the package-STRIPPED bare name, which
    // made two classes of the same bare name in different packages collide — and
    // because `set_find_class` writes this map as well as the symbol-keyed one,
    // defining a class named e.g. `MYPKG::ERROR` REPLACED the entry for `CL:ERROR`.
    // Every condition defined afterwards then resolved its `ERROR` superclass to
    // that class, which is its own superclass, and `MAKE-CONDITION` of anything
    // recursed until the stack was gone (bliss-kliz4):
    //
    //     (define-condition mypkg::error (cl:error) ((k :initarg :k)))
    //     (define-condition later (error) ((k :initarg :k)))
    //     (make-condition 'later :k 5)        => SIGSEGV
    //
    // The fallback itself is still wanted: it exists because a class-name symbol's
    // IDENTITY can drift (re-interned after the class was defined) while its name
    // does not, and a name that carries its package is just as stable against that
    // as a bare one.
    //
    // `COMMON-LISP:` is normalized away because a CL symbol's name is ordinarily
    // reported unqualified, so `ERROR` and `COMMON-LISP:ERROR` must land on one key.
    Some(normalized_class_key(&full))
}

/// The comparison form of a class name: uppercase, one colon between package and
/// name, and no redundant `COMMON-LISP` qualifier.
///
/// Only ever compared against other keys from this same function, so what matters
/// is that it is consistent — and that two names differing only by package do not
/// collapse onto each other.
fn normalized_class_key(full: &str) -> String {
    let upper = full.to_uppercase();
    let single = upper.replace("::", ":");
    for prefix in ["COMMON-LISP:", "CL:"] {
        if let Some(rest) = single.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    single
}

pub fn find_class(name: EgclVal) -> Option<EgclVal> {
    if let Some(class) = with_state(|st| st.class_registry.get(&name).copied()) {
        return Some(class);
    }
    let key = class_name_key(name)?;
    with_state(|st| {
        // Fallback: the symbol's identity may have drifted since the class was
        // registered (package RECYCLE/rehome re-interns the name). Match by the
        // bare class name, which is stable across such re-interning.
        st.class_by_name.get(&key).copied()
    })
}

/// Mark `class` as a structure class (created by DEFSTRUCT). Idempotent.
pub fn set_structure_class(class: EgclVal) {
    let structure_class = with_state_mut(|st| {
        st.structure_classes.insert(class);
        install_struct_info(st, class);
        st.structure_class
    });
    // A DEFSTRUCT class is an instance of STRUCTURE-CLASS, so CLASS-OF must
    // answer that and not the STANDARD-CLASS its metaobject was born with.
    if structure_class != NIL {
        let _ = repoint_class_metaclass(class, structure_class);
    }
}

/// Whether `class` is a structure class (DEFSTRUCT). NIL / unknown → false.
pub fn is_structure_class(class: EgclVal) -> bool {
    if class.is_nil() {
        return false;
    }
    with_state(|st| st.structure_classes.contains(&class))
}

/// Register a class by name.
pub fn set_find_class(name: EgclVal, class: EgclVal) -> Result<(), EgclError> {
    let key = class_name_key(name);
    with_state_mut(|st| {
        // Track handle-id registration order (for diamond-hierarchy inference)
        if class.is_meta_handle() {
            let fv = class.as_meta_handle_id();
            if !st.fixnum_registrations.iter().any(|(v, _)| *v == fv) {
                st.fixnum_registrations.push((fv, class));
            }
        }

        st.class_registry.insert(name, class);
        if let Some(key) = key {
            st.class_by_name.insert(key, class);
        }

        if !st.class_meta.contains_key(&class) {
            let default_supers = if st.bootstrapped && st.standard_object_class != NIL {
                vec![st.standard_object_class]
            } else {
                vec![]
            };
            st.class_meta.insert(
                class,
                ClassMeta {
                    runtime_definition: None,
                    protocol: ClassProtocol::default(),
                    shared_slots: Vec::new(),
                    name,
                    direct_supers: default_supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: None,
                    metaclass: NIL,
                    forward_referenced: false,
                },
            );
        } else {
            // Update name mapping
            st.class_meta.get_mut(&class).unwrap().name = name;
        }
        Ok(())
    })
}

/// Bind NAME to an EXISTING class in the class namespace, WITHOUT renaming the
/// class. This is what CLHS `(setf (find-class symbol) class)` does: it
/// associates the symbol with the class, so the class gains a second name while
/// CLASS-NAME still answers the one it was defined with.
///
/// Distinct from [`set_find_class`], which also rewrites the class's own `name`
/// in its metaobject — correct when registering a class under its OWN name,
/// wrong for an alias. Using that here renamed the original class and broke
/// `(typep (make-instance 'c) 'c)` for it (bliss-3ypy).
///
/// A NIL class removes the association (CLHS).
pub fn bind_class_name(name: EgclVal, class: EgclVal) -> Result<(), EgclError> {
    let key = class_name_key(name);
    with_state_mut(|st| {
        if class == NIL {
            st.class_registry.remove(&name);
            if let Some(key) = key {
                st.class_by_name.remove(&key);
            }
            return Ok(());
        }
        st.class_registry.insert(name, class);
        if let Some(key) = key {
            st.class_by_name.insert(key, class);
        }
        Ok(())
    })
}

/// Define a class with explicit name, superclasses, and slots.
///
/// This is the proper API for class definition, replacing the heuristic-based
/// inference used when classes are registered with `set_find_class` alone.
/// `direct_supers` should list the direct superclass values. If empty,
/// the class defaults to having STANDARD-OBJECT as its sole superclass.
pub fn define_class(
    name: EgclVal,
    class: EgclVal,
    direct_supers: &[EgclVal],
    slots: &[EgclVal],
) -> Result<(), EgclError> {
    define_class_with_shared_slots(name, class, direct_supers, slots, &[])
}

/// [`define_class`] for a class that also declares `shared_slots` with
/// `:allocation :class`: those names are left out of the instance layout even
/// when a superclass has an instance slot of the same name (CLHS 7.5.3; ansi
/// class-0211, class-0215, slot-value.2).
pub fn define_class_with_shared_slots(
    name: EgclVal,
    class: EgclVal,
    direct_supers: &[EgclVal],
    slots: &[EgclVal],
    shared_slots: &[EgclVal],
) -> Result<(), EgclError> {
    define_class_with_shared_slots_mode(name, class, direct_supers, slots, shared_slots, false)
}

/// Record new direct metadata without publishing an intermediate instance
/// wrapper. The class protocol must validate and publish its computed layout.
pub fn stage_class_with_shared_slots(
    name: EgclVal,
    class: EgclVal,
    direct_supers: &[EgclVal],
    slots: &[EgclVal],
    shared_slots: &[EgclVal],
) -> Result<(), EgclError> {
    define_class_with_shared_slots_mode(name, class, direct_supers, slots, shared_slots, true)
}

fn define_class_with_shared_slots_mode(
    name: EgclVal,
    class: EgclVal,
    direct_supers: &[EgclVal],
    slots: &[EgclVal],
    shared_slots: &[EgclVal],
    staged: bool,
) -> Result<(), EgclError> {
    let key = class_name_key(name);
    let (was_forward_referenced, standard_class) =
        with_state_mut(|st| -> Result<(bool, EgclVal), EgclError> {
            // Track handle-id registration order (for diamond-hierarchy inference)
            if class.is_meta_handle() {
                let fv = class.as_meta_handle_id();
                if !st.fixnum_registrations.iter().any(|(v, _)| *v == fv) {
                    st.fixnum_registrations.push((fv, class));
                }
            }

            st.class_registry.insert(name, class);
            if let Some(key) = key {
                st.class_by_name.insert(key, class);
            }

            let supers =
                if direct_supers.is_empty() && st.bootstrapped && st.standard_object_class != NIL {
                    vec![st.standard_object_class]
                } else {
                    direct_supers.to_vec()
                };

            // Register as subclass of each superclass
            for &s in &supers {
                if let Some(meta) = st.class_meta.get_mut(&s) {
                    if !meta.direct_subs.contains(&class) {
                        meta.direct_subs.push(class);
                    }
                }
            }

            // Preserve any existing wrapper pointer so we can obsolete it on
            // redefinition (below); the layout is recomputed fresh regardless.
            let prev_wrapper = st
                .class_meta
                .get(&class)
                .and_then(|m| m.wrapper)
                .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
                .unwrap_or(std::ptr::null_mut());

            // Whether this class is a forward reference being DEFINED. Its own
            // wrapper still says FORWARD-REFERENCED-CLASS, and defining it makes
            // that false: the wrapper has to be repointed, or `class_of` keeps
            // answering the placeholder metaclass while the metadata says otherwise
            // (R9.39). Repointed by the caller below, outside this lock, because
            // repointing allocates storage.
            let was_forward_referenced = st
                .class_meta
                .get(&class)
                .is_some_and(|m| m.forward_referenced);

            // Subclasses already recorded for this class survive a redefinition.
            // The insert below used to reset them to empty, which silently cut a
            // class off from its own subclasses -- and they are what the layout
            // refresh at the end of this function walks.
            let prev_subs = st
                .class_meta
                .get(&class)
                .map(|m| m.direct_subs.clone())
                .unwrap_or_default();

            st.class_meta.insert(
                class,
                ClassMeta {
                    runtime_definition: None,
                    protocol: ClassProtocol {
                        finalized: !staged,
                        ..ClassProtocol::default()
                    },
                    shared_slots: shared_slots.to_vec(),
                    name,
                    direct_supers: supers,
                    direct_subs: prev_subs,
                    slots: slots.to_vec(),
                    wrapper: if staged && !prev_wrapper.is_null() {
                        Some(unsafe { &*prev_wrapper })
                    } else {
                        None
                    },
                    metaclass: NIL,
                    forward_referenced: false,
                },
            );

            // Compute the effective slot layout and mint a fresh wrapper. On
            // redefinition, mark the previous wrapper obsolete so instances created
            // against it take the slow path on next access (they keep working
            // against their frozen layout). See bliss-xyo / spec §5.3.4.
            if !staged {
                finalize_class_layout(st, class);
            }
            if !staged && !prev_wrapper.is_null() {
                unsafe {
                    (*prev_wrapper)
                        .state
                        .store(WRAPPER_OBSOLETE, Ordering::Release);
                }
            }

            // Propagate to the subclasses (CLHS 4.3.6: redefining a class updates
            // the instance structure of its subclasses too). A subclass's layout is
            // frozen in its wrapper at ITS definition, so a subclass defined before
            // this class had slots -- a forward-referenced superclass, say -- would
            // otherwise keep a layout with none of them and `slot-value` would
            // report the inherited slot as not present in the layout. Measured:
            // (defclass sub (super) ...) then (defclass super () ((a))) left A
            // unreachable on an instance of SUB.
            //
            // Breadth-first over the recorded subclass graph, visiting each class
            // once so a diamond or a cycle cannot loop. Only classes that already
            // have a wrapper are re-finalized; one that has never been instantiated
            // builds its layout on first use anyway.
            let mut queue: Vec<EgclVal> = st
                .class_meta
                .get(&class)
                .map(|m| m.direct_subs.clone())
                .unwrap_or_default();
            let mut seen: HashSet<EgclVal> = HashSet::new();
            seen.insert(class);
            while let Some(sub) = queue.pop() {
                if !seen.insert(sub) {
                    continue;
                }
                let (had_wrapper, subs) = match st.class_meta.get(&sub) {
                    Some(meta) => (meta.wrapper.is_some(), meta.direct_subs.clone()),
                    None => continue,
                };
                queue.extend(subs);
                if let Some(meta) = st.class_meta.get_mut(&sub) {
                    meta.protocol.invalidate_computed();
                    if !staged {
                        meta.protocol.finalized = true;
                    }
                }
                if staged || !had_wrapper {
                    continue;
                }
                let stale = st
                    .class_meta
                    .get(&sub)
                    .and_then(|m| m.wrapper)
                    .map(|w| w as *const ClassWrapper as *mut ClassWrapper)
                    .unwrap_or(std::ptr::null_mut());
                if let Some(meta) = st.class_meta.get_mut(&sub) {
                    meta.wrapper = None;
                    meta.protocol.finalized = true;
                }
                finalize_class_layout(st, sub);
                if !stale.is_null() {
                    unsafe { (*stale).state.store(WRAPPER_OBSOLETE, Ordering::Release) };
                }
            }

            Ok((was_forward_referenced, st.standard_class))
        })?;
    // Outside the lock, because repointing allocates slot storage: a class that
    // was a forward reference until a moment ago is now an ordinary standard
    // class, and its wrapper must say so. DEFSTRUCT and a `:metaclass` option
    // repoint again afterwards, which is the existing order (R9.39).
    if was_forward_referenced && standard_class != NIL {
        let _ = repoint_class_metaclass(class, standard_class);
    }
    Ok(())
}

/// Compute a class's effective :instance-allocated slot layout (walking the
/// CPL, most-specific-first, first occurrence wins) and install a fresh
/// `ClassWrapper` referencing a leaked, frozen `SlotLayout`. Built-in classes
/// (never instantiated) are skipped and keep a null wrapper.
fn finalize_class_layout(st: &mut ClosState, class: EgclVal) {
    if st
        .class_meta
        .get(&class)
        .is_some_and(|meta| !meta.protocol.finalized)
    {
        return;
    }
    if is_builtin_class(st, class) {
        return;
    }
    let cpl = linearize_class_precedence(st, class).unwrap_or_else(|_| vec![class]);
    let mut order: Vec<EgclVal> = Vec::new();
    let mut index: HashMap<EgclVal, usize> = HashMap::new();
    // A slot a more specific class declares :allocation :class shadows the
    // instance slot of the same name in a less specific class (CLHS 7.5.3).
    let mut shadowed: HashSet<EgclVal> = HashSet::new();
    if let Some(names) = st
        .class_meta
        .get(&class)
        .and_then(|meta| meta.protocol.instance_slots.as_ref())
    {
        for &name in names {
            index.insert(name, order.len());
            order.push(name);
        }
    } else {
        for c in &cpl {
            if let Some(meta) = st.class_meta.get(c) {
                for &slot_name in &meta.slots {
                    if shadowed.contains(&slot_name) {
                        continue;
                    }
                    if let std::collections::hash_map::Entry::Vacant(e) = index.entry(slot_name) {
                        e.insert(order.len());
                        order.push(slot_name);
                    }
                }
                shadowed.extend(meta.shared_slots.iter().copied());
            }
        }
    }
    let slot_count = order.len() as u32;
    let layout = Box::leak(Box::new(SlotLayout::new(order, index)));
    let wrapper: &'static ClassWrapper = Box::leak(Box::new(ClassWrapper {
        stamp: next_stamp(),
        state: AtomicU8::new(WRAPPER_CURRENT),
        class,
        slot_count,
        layout,
        struct_info: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
    }));
    if let Some(meta) = st.class_meta.get_mut(&class) {
        meta.wrapper = Some(wrapper);
    }
    if st.structure_classes.contains(&class) {
        install_struct_info(st, class);
    }
}

/// Publish [`StructInfo`] on `class`'s current wrapper when it is a structure
/// class whose ancestors' layouts are prefixes of its own. Pure Rust; the
/// leaked info lives as long as the (leaked) wrapper that points at it.
fn install_struct_info(st: &ClosState, class: EgclVal) {
    let Some(wrapper) = st.class_meta.get(&class).and_then(|meta| meta.wrapper) else {
        return;
    };
    if !wrapper.struct_info.load(Ordering::Acquire).is_null() {
        return;
    }
    let Ok(cpl) = linearize_class_precedence(st, class) else {
        return;
    };
    let own = &wrapper.layout.order;
    let mut names = Vec::new();
    for ancestor in cpl.iter().rev() {
        if !st.structure_classes.contains(ancestor) {
            continue;
        }
        let Some(meta) = st.class_meta.get(ancestor) else {
            return;
        };
        if *ancestor != class {
            let Some(parent) = meta.wrapper else {
                return;
            };
            let order = &parent.layout.order;
            if order.len() > own.len() || own[..order.len()] != order[..] {
                return;
            }
        }
        names.push(meta.name);
    }
    let info = Box::leak(Box::new(StructInfo {
        names: names.into_boxed_slice(),
    }));
    wrapper.struct_info.store(info, Ordering::Release);
}

/// Return true if `object` is a live CLOS standard-object instance. Uses the
/// liveness registry so it is safe to call on ANY value, including dangling or
/// garbage heap-tagged values that must not be dereferenced (see
/// `ClosState.live_instances`).
pub fn is_instance(object: EgclVal) -> bool {
    // A bounds-checked header read replaces the old `live_instances` registry
    // (bliss-334): an instance is any heap value whose object header says
    // STANDARD_OBJECT. Dangling values get the same staleness contract as
    // every other heap reference under the moving collector.
    egcl_rt::gc::heap_object_type_id(object) == Some(type_id::STANDARD_OBJECT)
}

/// Get the class of an object.
pub fn class_of(object: EgclVal) -> EgclVal {
    // Inspect heap state before taking the root-side-table lock (GC takes
    // these locks in the opposite direction while scanning definitions).
    if egcl_rt::gc::heap_object_type_id(object) == Some(type_id::STANDARD_OBJECT) {
        return unsafe { (*instance_wrapper(object)).class };
    }

    // Arrays, streams, packages and pathnames belong to their standard
    // classes (CLHS 4.3.7), so (typep "abc" (find-class 'string)) and
    // (class-of (make-array 3)) answer as ANSI requires (ansi arrays
    // make-array.*, types-and-classes). The classes are registered under their
    // name symbols by the bootstrap hierarchy. Decide the name here: the stream
    // state lock sits below the CLOS definitions lock, and interning (a lookup
    // for these bootstrap names) must not happen under with_state either.
    let standard_name = if crate::random_state::random_state_p(object) {
        Some("RANDOM-STATE")
    } else if crate::hash_table_p(object) {
        // Was a hardcoded `from_fixnum(-23)` that bypassed the registry
        // entirely; it is just another standard class (bliss-rj5o).
        Some("HASH-TABLE")
    } else if crate::sequences::is_nil_sequence(object) || egcl_rt::types::stringp(object) {
        Some("STRING")
    } else if egcl_rt::types::bit_vector_p(object) {
        Some("BIT-VECTOR")
    } else if egcl_rt::types::vectorp(object) {
        Some("VECTOR")
    } else if egcl_rt::types::arrayp(object) || egcl_rt::types::md_array_p(object) {
        Some("ARRAY")
    } else if egcl_rt::types::streamp(object) {
        Some(crate::streams::stream_class_name(object).unwrap_or("STREAM"))
    } else if egcl_rt::types::packagep(object) {
        Some("PACKAGE")
    } else if crate::is_logical_pathname(object) {
        Some("LOGICAL-PATHNAME")
    } else if crate::is_pathname(object) {
        Some("PATHNAME")
    } else {
        None
    };
    let standard_sym =
        standard_name.map(|name| EgclVal::from_symbol_index(egcl_rt::symbols::intern(name)));
    with_state(|st| {
        if object == NIL {
            return st.null_class;
        }
        if object == T {
            return st.symbol_class;
        }
        if object.is_fixnum() {
            return st.fixnum_class;
        }
        if object.is_character() {
            return st.character_class;
        }
        if object.is_symbol() {
            return st.symbol_class;
        }
        if object.is_cons() {
            return st.cons_class;
        }
        if object.is_single_float() {
            return st.float_class;
        }
        if object.is_function() {
            return st.function_class;
        }
        if let Some(class) = standard_sym.and_then(|sym| st.class_registry.get(&sym).copied()) {
            return class;
        }
        if object.is_heap_object() {
            return st.heap_object_class;
        }
        st.t_class_val
    })
}

/// Whether VALUE is a class metaobject, independently of its current name or
/// registration in FIND-CLASS. Removing a name does not destroy the class.
pub fn is_class_object(value: EgclVal) -> bool {
    with_state(|st| st.class_meta.contains_key(&value))
}

/// Get the class name.
pub fn class_name(class: EgclVal) -> EgclVal {
    with_state(|st| st.class_meta.get(&class).map(|m| m.name).unwrap_or(NIL))
}

/// Set only the primary name stored in a class metaobject. In particular this
/// does not add, remove, or retarget any FIND-CLASS registration. Definition
/// metadata, layouts, CPLs and shared cells remain owned by the same class.
/// No Lisp allocation or callbacks occur while CLOS state is borrowed.
pub fn set_class_name(class: EgclVal, new_name: EgclVal) -> Result<EgclVal, EgclError> {
    update_class_name(class, new_name)?;
    Ok(new_name)
}

/// Atomically report whether the primary name actually changed, so callers
/// can avoid invalidating caches for a no-op without a separate racy read.
pub fn update_class_name(class: EgclVal, new_name: EgclVal) -> Result<bool, EgclError> {
    if !new_name.is_symbol() {
        return Err(EgclError::TypeError {
            datum: new_name,
            expected: "SYMBOL".into(),
        });
    }
    with_state_mut(|st| {
        let meta = st
            .class_meta
            .get(&class)
            .ok_or_else(|| EgclError::TypeError {
                datum: class,
                expected: "CLASS".into(),
            })?;
        if meta.name == new_name {
            return Ok(false);
        }
        // Physical builtin predicates use their canonical metadata names.
        // User structure classes and forward references are not builtins.
        if !meta.forward_referenced && is_builtin_class(st, class) {
            return Err(EgclError::ProgramError(
                "cannot rename a built-in class".into(),
            ));
        }
        st.class_meta.get_mut(&class).unwrap().name = new_name;
        Ok(true)
    })
}

/// Compute the class precedence list by the standard's rule (R5.67).
pub fn compute_class_precedence_list(class: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    with_state(|st| {
        if let Some(cpl) = st.cpl_cache.borrow().get(&class) {
            return Ok(cpl.clone());
        }
        // Pure Rust allocation only: no GC or Lisp callbacks while borrowed.
        // Do not cache errors or unknown classes. This bounds derived entries
        // by registered metadata and preserves the existing unknown-class API.
        let cpl = linearize_class_precedence(st, class)?;
        if st.class_meta.contains_key(&class) {
            st.cpl_cache.borrow_mut().insert(class, cpl.clone());
        }
        Ok(cpl)
    })
}

/// Whether `class`'s precedence list includes `STANDARD-OBJECT` — i.e. its
/// instances are standard objects, which is what makes it a standard class
/// rather than a built-in one.
///
/// Reads the class GRAPH, not the class value's tag. That matters because a
/// `DEFCLASS` class is currently its own name symbol and a built-in class a
/// negative fixnum, so callers used to discriminate the two with
/// `class.is_symbol()` — a test that silently inverts the moment a class
/// metaobject stops being an immediate (bliss-lkf8l, bliss-rj5o).
///
/// A structure class also inherits `STANDARD-OBJECT` here, so a caller that
/// needs the three-way standard/built-in/structure split must test
/// [`is_structure_class`] first.
pub fn class_inherits_standard_object(class: EgclVal) -> bool {
    let standard_object = with_state(|st| st.standard_object_class);
    if standard_object == NIL {
        return false;
    }
    compute_class_precedence_list(class)
        .map(|cpl| cpl.contains(&standard_object))
        .unwrap_or(false)
}

/// Allocate a class metaobject for a class the program is about to define.
///
/// The object is an instance of `STANDARD-CLASS` to begin with, which is the
/// default metaclass; `set_class_metaclass` and `set_structure_class` re-point
/// its wrapper when the class turns out to be something else. Every class gets
/// its own wrapper — they are small leaked allocations, and the alternative is
/// memoizing a wrapper per metaclass in `ClosState`, which cannot hold a raw
/// pointer across a saved image.
///
/// Must be called with no CLOS lock held: it allocates (see
/// `alloc_class_metaobject`).
pub fn new_class_metaobject(metaclass: EgclVal) -> Result<EgclVal, EgclError> {
    let metaclass = if metaclass == NIL {
        with_state(|st| st.standard_class)
    } else {
        metaclass
    };
    if metaclass == NIL {
        return Err(EgclError::Internal(
            "no metaclass available for a class metaobject".into(),
        ));
    }
    // A class metaobject IS an instance of its metaclass, so allocate it as
    // one: that gives it the metaclass's wrapper, a slot cell for each of the
    // metaclass's effective instance slots, and UNBOUND in each. A plain
    // STANDARD-CLASS has no slots, so the common case is still 16 bytes.
    //
    // This is what makes a (:metaclass M) class able to CARRY M's slots, which
    // is the half of GitHub #384 that no amount of checking could provide.
    allocate_instance_pinned_gc(metaclass)
}

/// Re-point `class`'s own wrapper so `CLASS-OF` answers `metaclass`.
///
/// A class metaobject's wrapper names the class it is an INSTANCE of, which is
/// its metaclass — the same word `class_of` reads for any other instance. This
/// is how a `(:metaclass M)` class comes to answer M without a special case
/// anywhere (GitHub #384).
/// Re-point `class`'s own wrapper so `CLASS-OF` answers `metaclass`, growing
/// its slot storage to the metaclass's layout. Answers whether the layout
/// actually changed, which tells DEFCLASS whether the metaclass's `:initform`s
/// still have to run over the new cells.
///
/// A class metaobject's wrapper names the class it is an INSTANCE of, which is
/// its metaclass — the same word `class_of` reads for any other instance. This
/// is how a `(:metaclass M)` class comes to answer M without a special case
/// anywhere (GitHub #384).
///
/// This used to REFUSE when the metaclass's layout had more slots than the
/// object was allocated with, because inline slots cannot grow; DEFCLASS then
/// replaced the whole metaobject to work around it, which cost a
/// forward-referenced class its EQ identity (bliss-qbz4o). With indirect
/// storage there is nothing to refuse: install fresh storage, carry over any
/// slots the old layout shared by name, and keep the object (bliss-fmhj5).
fn repoint_class_metaclass(class: EgclVal, metaclass: EgclVal) -> bool {
    if egcl_rt::gc::heap_object_type_id(class) != Some(type_id::STANDARD_OBJECT) {
        return false;
    }
    if metaclass == NIL {
        return false;
    }
    let old_wrapper = unsafe { instance_wrapper(class) };
    if !old_wrapper.is_null() && unsafe { (*old_wrapper).class } == metaclass {
        // Already an instance of this metaclass, with storage sized for it.
        return false;
    }
    let target = metaclass_instance_wrapper(metaclass);
    if target.is_null() {
        return false;
    }
    // Carry over whatever the old layout and the new one share, by name, so a
    // metaclass that GAINS a slot does not reset the state a metaclass protocol
    // was keeping in the others (bliss-k4gev).
    let mut carried: Vec<(EgclVal, EgclVal)> = Vec::new();
    if !old_wrapper.is_null() {
        unsafe {
            let old_layout = (*old_wrapper).layout;
            for (i, &name) in old_layout.order.iter().enumerate() {
                let value = *slot_cell(class, i);
                if value != UNBOUND {
                    carried.push((name, value));
                }
            }
        }
    }
    let mut class = class;
    egcl_rt::rooted_ref!(_class_root = &mut class);
    egcl_rt::rooted_ref!(_carried_root = &mut carried);
    // A class metaobject's shell is pinned, so its storage is too.
    let Some(mut storage) = alloc_slot_storage(unsafe { (*target).slot_count } as usize, true)
    else {
        return false;
    };
    egcl_rt::rooted_ref!(_storage_root = &mut storage);
    unsafe {
        set_instance_storage(class, storage);
        set_instance_wrapper(class, target);
        let layout = (*target).layout;
        for (name, value) in &carried {
            if let Some(&idx) = layout.index.get(name) {
                *slot_cell(class, idx) = *value;
            }
        }
    }
    true
}

/// The wrapper `metaclass`'s own instances use — which is the wrapper a class
/// metaobject with that metaclass must carry, since it IS one of them.
/// Finalized on demand: a metaclass need never have been instantiated.
fn metaclass_instance_wrapper(metaclass: EgclVal) -> *mut ClassWrapper {
    with_state_mut(|st| {
        if st
            .class_meta
            .get(&metaclass)
            .and_then(|m| m.wrapper)
            .is_none()
        {
            finalize_class_layout(st, metaclass);
        }
        st.class_meta
            .get(&metaclass)
            .and_then(|m| m.wrapper)
            .map(|w| w as *const ClassWrapper as *mut ClassWrapper)
            .unwrap_or(std::ptr::null_mut())
    })
}

/// `FORWARD-REFERENCED-CLASS`, the metaclass of a placeholder minted for a
/// class named as a superclass before it is defined (AMOP; spec R9.39). NIL
/// before the bootstrap registers it.
pub fn forward_referenced_class() -> EgclVal {
    with_state(|st| st.forward_referenced_class)
}

/// Mark `class` as existing only as a forward reference: some other class named
/// it as a superclass before it was defined. See `ClassMeta.forward_referenced`.
pub fn mark_forward_referenced(class: EgclVal) {
    with_state_mut(|st| {
        if let Some(meta) = st.class_meta.get_mut(&class) {
            meta.forward_referenced = true;
        }
    });
}

/// Whether `class` is only a forward-reference placeholder, never defined.
pub fn is_forward_referenced(class: EgclVal) -> bool {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .is_some_and(|m| m.forward_referenced)
    })
}

/// The first class at or above `class` that is only a forward reference, or NIL.
///
/// A class is finalized only once every class it inherits from is, so a class
/// whose superclass is still a forward reference is not finalized either. That
/// is what `CLASS-FINALIZED-P` must answer and what `FINALIZE-INHERITANCE` must
/// refuse (R9.39). An inconsistent hierarchy -- one whose precedence list does
/// not linearize -- answers NIL here and is reported by the caller that asks
/// for the precedence list.
pub fn forward_referenced_superclass(class: EgclVal) -> EgclVal {
    if is_forward_referenced(class) {
        return class;
    }
    match compute_class_precedence_list(class) {
        Ok(cpl) => cpl
            .into_iter()
            .find(|&super_class| is_forward_referenced(super_class))
            .unwrap_or(NIL),
        Err(_) => NIL,
    }
}

/// The metaclass DEFCLASS named for `class`, or NIL for the default.
pub fn class_metaclass(class: EgclVal) -> EgclVal {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.metaclass)
            .unwrap_or(NIL)
    })
}

/// Record the metaclass DEFCLASS named for `class`.
///
/// Separate from `define_class` because the metaclass is resolved by the
/// interpreter (it is a class NAME in the source, which may not be defined
/// until after this class is) and because every existing caller of
/// `define_class` means the default.
pub fn set_class_metaclass(class: EgclVal, metaclass: EgclVal) -> bool {
    with_state_mut(|st| {
        if let Some(meta) = st.class_meta.get_mut(&class) {
            meta.metaclass = metaclass;
        }
    });
    // Outside the lock: leaking a wrapper is a Rust allocation, but keeping the
    // two in one critical section would invite a GC allocation under the CLOS
    // lock later. The recorded metaclass above stays the source of truth.
    //
    // Answers whether the class's slot layout changed, so DEFCLASS knows to run
    // the metaclass's initforms over cells that did not exist before.
    repoint_class_metaclass(class, metaclass)
}

/// Get the direct superclasses of a class.
pub fn class_direct_superclasses(class: EgclVal) -> Vec<EgclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_supers.clone())
            .unwrap_or_default()
    })
}

/// Get the direct subclasses of a class.
pub fn class_direct_subclasses(class: EgclVal) -> Vec<EgclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_subs.clone())
            .unwrap_or_default()
    })
}

/// Get the slots of a class.
pub fn class_slots(class: EgclVal) -> Vec<EgclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.slots.clone())
            .unwrap_or_default()
    })
}

/// Physical direct slot declarations for classes registered without a rich
/// source definition. Names retain their exact symbol identity; shared slots
/// remain distinct from inline instance slots. This only copies Rust vectors.
pub fn class_direct_slot_declarations(class: EgclVal) -> (Vec<EgclVal>, Vec<EgclVal>) {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|meta| (meta.slots.clone(), meta.shared_slots.clone()))
            .unwrap_or_default()
    })
}

/// All effective slot names of `class` — its own direct slots plus every
/// inherited slot — in precedence order (most-general superclass first, so an
/// :include parent's slots precede the child's), de-duplicated. Used for #S
/// structure printing and EQUALP structure comparison, which need the full
/// slot set, not just the direct slots `class_slots` returns.
pub fn effective_slots(class: EgclVal) -> Vec<EgclVal> {
    let cpl = match compute_class_precedence_list(class) {
        Ok(c) => c,
        Err(_) => return class_slots(class),
    };
    let mut seen: HashSet<EgclVal> = HashSet::new();
    let mut out: Vec<EgclVal> = Vec::new();
    for c in cpl.into_iter().rev() {
        for slot in class_slots(c) {
            if seen.insert(slot) {
                out.push(slot);
            }
        }
    }
    out
}

// ── Class precedence lists (CLHS 4.3.5) ─────────────────────────

/// Infer direct superclasses for a class that is part of a consecutive-
/// fixnum registration group. This enables the diamond-inheritance test
/// which registers classes via `set_find_class` without an explicit
/// superclass API.
///
/// For a group [A, B, C, D] (consecutive fixnum values registered in order):
///   A (index 0)       → keep default supers (base class)
///   B, C (middle)     → supers = [A]
///   D (last, index 3) → supers = [B, C]  (all middle classes)
fn infer_group_supers(st: &ClosState, class: EgclVal) -> Option<Vec<EgclVal>> {
    if !class.is_meta_handle() {
        return None;
    }
    let fv = class.as_meta_handle_id();
    let pos = st.fixnum_registrations.iter().position(|(v, _)| *v == fv)?;

    // Find maximal consecutive run containing this position
    let mut start = pos;
    while start > 0 && st.fixnum_registrations[start].0 - st.fixnum_registrations[start - 1].0 == 1
    {
        start -= 1;
    }
    let mut end = pos;
    while end + 1 < st.fixnum_registrations.len()
        && st.fixnum_registrations[end + 1].0 - st.fixnum_registrations[end].0 == 1
    {
        end += 1;
    }

    let group_size = end - start + 1;
    if group_size < 3 {
        return None; // no diamond inference for tiny groups
    }

    let idx = pos - start;
    if idx == 0 {
        // Base class — keep its existing default supers
        None
    } else if idx < group_size - 1 {
        // Middle class — super is the base
        Some(vec![st.fixnum_registrations[start].1])
    } else {
        // Bottom class — supers are all middle classes in order
        let supers = (start + 1..end)
            .map(|i| st.fixnum_registrations[i].1)
            .collect();
        Some(supers)
    }
}

/// The direct superclasses `class` is ordered over: its recorded ones, or the
/// group-inferred ones while it still has only the default [STANDARD-OBJECT]
/// (set by `set_find_class`), so the heuristic never overrides supers that
/// were set on purpose. `None` for a class the core does not know.
fn direct_supers_for_precedence(st: &ClosState, class: EgclVal) -> Option<Vec<EgclVal>> {
    let meta = st.class_meta.get(&class)?;
    let is_default_supers = meta.direct_supers.len() == 1
        && meta.direct_supers[0] == st.standard_object_class
        && st.standard_object_class != NIL;
    if is_default_supers {
        if let Some(inferred) = infer_group_supers(st, class) {
            return Some(inferred);
        }
    }
    Some(meta.direct_supers.clone())
}

/// Every class in `class`'s hierarchy with its direct superclasses, `class`
/// first and each class once. `visiting` is the chain being descended;
/// reaching one of its members again means the (possibly heuristic-inferred)
/// super graph is CYCLIC, which previously recursed to a stack-overflow
/// SIGSEGV (bliss-d0b: trivial-gray-streams' fundamental-stream vs the
/// bootstrap stream classes). Report it as a catchable error naming the class.
fn collect_hierarchy(
    st: &ClosState,
    class: EgclVal,
    visiting: &mut Vec<EgclVal>,
    out: &mut Vec<(EgclVal, Vec<EgclVal>)>,
) -> Result<(), EgclError> {
    if visiting.contains(&class) {
        let name = st.class_meta.get(&class).map(|m| m.name).unwrap_or(class);
        // Do not enter the symbol registry while holding the CLOS root lock.
        let rendered = format!("{name:?}");
        return Err(EgclError::Internal(format!(
            "cyclic class hierarchy: {rendered} appears among its own superclasses"
        )));
    }
    if out.iter().any(|(seen, _)| *seen == class) {
        return Ok(());
    }
    let supers = direct_supers_for_precedence(st, class).unwrap_or_default();
    out.push((class, supers.clone()));
    visiting.push(class);
    for s in &supers {
        collect_hierarchy(st, *s, visiting, out)?;
    }
    visiting.pop();
    Ok(())
}

/// The class precedence list of `class`, by CLHS 4.3.5: a topological sort of
/// the local precedence orders (each class before its direct superclasses,
/// and those in the order written), where among several classes that could
/// come next the one with a direct subclass rightmost in the list so far goes
/// first.
///
/// This is the standard's own rule, not C3. The two agree on most
/// hierarchies, including every diamond, but not on all: for ansi
/// class-0306's eight classes C3 orders B before C and the standard's rule C
/// before B. SBCL, CCL and Allegro follow the standard, and the precedence
/// list decides method order, so code ported from them must see the same
/// order here.
fn linearize_class_precedence(st: &ClosState, class: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    if let Some(cpl) = st
        .class_meta
        .get(&class)
        .and_then(|meta| meta.protocol.cpl.as_ref())
    {
        return Ok(cpl.clone());
    }
    compute_class_precedence_default(st, class)
}

fn compute_class_precedence_default(
    st: &ClosState,
    class: EgclVal,
) -> Result<Vec<EgclVal>, EgclError> {
    #[cfg(test)]
    cpl_tests::LINEARIZATIONS.with(|count| count.set(count.get() + 1));
    let mut classes: Vec<(EgclVal, Vec<EgclVal>)> = Vec::new();
    collect_hierarchy(st, class, &mut Vec::new(), &mut classes)?;

    // (before, after) pairs: C before S1, S1 before S2, ... for each class C
    // with direct superclasses (S1 S2 ...).
    let mut constraints: Vec<(EgclVal, EgclVal)> = Vec::new();
    for (c, supers) in &classes {
        let mut previous = *c;
        for s in supers {
            constraints.push((previous, *s));
            previous = *s;
        }
    }
    let mut remaining: Vec<EgclVal> = classes.iter().map(|(c, _)| *c).collect();
    let mut result: Vec<EgclVal> = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let candidates: Vec<EgclVal> = remaining
            .iter()
            .copied()
            .filter(|c| {
                !constraints
                    .iter()
                    .any(|(before, after)| after == c && remaining.contains(before))
            })
            .collect();
        let next = match candidates.as_slice() {
            // A Lisp-level error, so a DEFCLASS whose superclasses cannot be
            // ordered is caught by HANDLER-CASE like any other definition
            // error (ansi objects defclass.forward-ref.4) rather than
            // aborting the image.
            [] => {
                return Err(EgclError::ProgramError(
                    "inconsistent class hierarchy: no class precedence list satisfies the local precedence orders".into(),
                ));
            }
            [only] => *only,
            _ => {
                let mut best = candidates[0];
                let mut best_position: Option<usize> = None;
                for candidate in &candidates {
                    let position = result.iter().rposition(|placed| {
                        classes
                            .iter()
                            .any(|(c, supers)| c == placed && supers.contains(candidate))
                    });
                    if position.is_some() && position > best_position {
                        best = *candidate;
                        best_position = position;
                    }
                }
                best
            }
        };
        result.push(next);
        remaining.retain(|c| *c != next);
    }
    Ok(result)
}

// ── Instance protocol ──────────────────────────────────────────────

/// Allocate an instance of a class (ALLOCATE-INSTANCE).
///
/// Allocates a heap object `[ObjectHeader | wrapper ptr | inline slots]` and
/// initialises every slot cell to `UNBOUND` (a zeroed cell would read as the
/// bound value `0`). The object is leaked, matching the other tree-walker heap
/// objects. See spec §5.3.4 / issue bliss-xyo.
pub fn allocate_instance(class: EgclVal) -> Result<EgclVal, EgclError> {
    if !class_protocol_finalized(class) {
        return Err(EgclError::ProgramError(
            "class must be finalized before allocation".into(),
        ));
    }
    // A class that exists only because another class named it as a superclass
    // is not finalized: nobody has said what slots it has, so an instance of it
    // would be a standard object with a layout the definition is going to
    // contradict. CLHS and AMOP both make this an error, and SBCL signals it
    // ("cannot allocate an instance of a forward referenced class"); EGCL used
    // to hand back a slotless instance (bliss-jv7tj).
    //
    // Refused HERE rather than in each of MAKE-INSTANCE, ALLOCATE-INSTANCE and
    // CHANGE-CLASS, because this is the one place an instance of a class is
    // born.
    if is_forward_referenced(class) {
        return Err(EgclError::ProgramError(format!(
            "cannot allocate an instance of {}: it is a forward-referenced class, \
             named as a superclass but never defined",
            class_name(class)
                .symbol_index()
                .and_then(egcl_rt::symbols::symbol_name)
                .unwrap_or_else(|| "a forward-referenced class".to_string())
        )));
    }
    // Fetch the wrapper + slot count as Copy values, finalising the layout if
    // the class has none yet, then allocate outside the state borrow.
    let (wrapper, slot_count) = class_wrapper_and_slot_count(class);
    if wrapper.is_null() {
        return Err(EgclError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    // The cap that licenses `slot_cell`'s hard-coded small-object payload
    // offset: storage this size can never need the 16-byte large-object prefix.
    debug_assert!(
        (16 + 8 * slot_count) / 8 <= 0xFFFE,
        "slot storage too large for the header size field"
    );
    // Ordinary instances are ORDINARY HEAP OBJECTS (bliss-334): nursery-born,
    // movable, and collectible, like any cons. They used to be std::alloc'd
    // off-heap and recorded in a `live_instances` registry that (a) leaked
    // every instance forever and (b) force-rooted all their slots via the CLOS
    // root scanner. The GC already knows the STANDARD_OBJECT layout (word 0 is
    // the raw wrapper pointer, the rest are traced slot values), so instances
    // need no side-table at all.
    // Storage first, ROOTED across the shell's allocation: two allocations, and
    // the first value has to survive the second (bliss-334's invariant 1).
    let mut storage = alloc_slot_storage(slot_count, false)
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for instance slots".into()))?;
    egcl_rt::rooted_ref!(_storage_root = &mut storage);
    // The shell is a FIXED 16-byte body whatever the slot count: the raw
    // wrapper pointer and the tagged storage reference.
    let body = egcl_rt::gc::alloc_typed(SLOT_STORAGE_OFFSET, type_id::STANDARD_OBJECT)
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for instance".into()))?;
    unsafe {
        let ptr = body.sub(8);
        let inst = EgclVal::from_heap_ptr(ptr);
        // Both words before anything can collect again: the tracer reads word 1
        // as a tagged value, so it must not see the allocator's leftovers.
        set_instance_storage(inst, storage);
        set_instance_wrapper(inst, wrapper);
        Ok(inst)
    }
}

/// `class`'s current wrapper and slot count, finalising its layout first if it
/// has none yet. Pure Rust values: the caller allocates outside the state lock.
///
/// The common case only reads, so it takes the shared accessor. The mutable
/// one clears the class-precedence cache, and taking it on every allocation
/// made each MAKE-INSTANCE and structure constructor discard every cached CPL
/// (bliss-mnkee).
fn class_wrapper_and_slot_count(class: EgclVal) -> (*mut ClassWrapper, usize) {
    fn current(st: &ClosState, class: EgclVal) -> *mut ClassWrapper {
        st.class_meta
            .get(&class)
            .and_then(|m| m.wrapper)
            .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
            .unwrap_or(std::ptr::null_mut())
    }
    let mut w = with_state(|st| current(st, class));
    if w.is_null() {
        w = with_state_mut(|st| {
            if current(st, class).is_null() {
                finalize_class_layout(st, class);
            }
            current(st, class)
        });
    }
    let n = if w.is_null() {
        0
    } else {
        unsafe { (*w).slot_count as usize }
    };
    (w, n)
}

/// Allocate a CLOS instance of `class` on the shared GC heap and pin it, so the
/// collector never moves or frees it (bliss-4v8 / D5.13). Used for the immortal
/// STORAGE-CONDITION pool: its preallocated instances must keep their addresses
/// forever, even across a moving collection. Layout and slot initialization are
/// identical to [`allocate_instance`]; only the backing store (GC heap, pinned)
/// differs.
pub fn allocate_instance_pinned_gc(class: EgclVal) -> Result<EgclVal, EgclError> {
    let (wrapper, slot_count) = class_wrapper_and_slot_count(class);
    if wrapper.is_null() {
        return Err(EgclError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    // The cap that licenses `slot_cell`'s hard-coded small-object payload
    // offset: storage this size can never need the 16-byte large-object prefix.
    debug_assert!(
        (16 + 8 * slot_count) / 8 <= 0xFFFE,
        "slot storage too large for the header size field"
    );
    // The GC allocator writes an 8-byte STANDARD_OBJECT header and returns the
    // body pointer; the instance value points at the header (body − 8).
    //
    // Allocated PINNED — directly into the collector's packed pinned-host
    // regions — rather than into the nursery and pinned afterwards. A pinned
    // NURSERY object forces the collector to retain its entire region in place
    // at the next minor GC, so every nursery-born instance cost a full region
    // of heap permanently; under allocation churn that exhausted the heap and
    // evacuation began dropping live objects (bliss-wc4t: ~316 instances had
    // poisoned 316 x 1MB regions). Until instances become movable and
    // collectible (bliss-334) they are immortal either way; packed host
    // regions bound the cost to the instances' own bytes.
    // Storage pinned alongside the shell, and rooted across its allocation:
    // this pair is immortal together, rather than a pinned shell holding a
    // reference into movable space (bliss-fmhj5).
    let mut storage = alloc_slot_storage(slot_count, true).ok_or_else(|| {
        EgclError::Internal("GC heap unavailable for pooled instance slots".into())
    })?;
    egcl_rt::rooted_ref!(_storage_root = &mut storage);
    let body = egcl_rt::gc::alloc_pinned_typed(SLOT_STORAGE_OFFSET, type_id::STANDARD_OBJECT)
        .ok_or_else(|| EgclError::Internal("GC heap unavailable for pooled instance".into()))?;
    unsafe {
        let ptr = body.sub(8);
        let inst = EgclVal::from_heap_ptr(ptr);
        set_instance_storage(inst, storage);
        set_instance_wrapper(inst, wrapper);
        Ok(inst)
    }
}

/// Make an instance (MAKE-INSTANCE). R5.12.
/// The slot names a DEFSTRUCT constructor supplies, in DEFSTRUCT's own order:
/// inherited slots first, then the struct's own (CLHS 3.4.6).
///
/// Derived rather than registered. The instance LAYOUT is built from the class
/// precedence list, which is most-specific-FIRST, so its index order is the
/// reverse of what a struct constructor wants; walking the CPL backwards and
/// collecting each class's direct slots reproduces DEFSTRUCT's order exactly.
pub fn struct_slot_order(class: EgclVal) -> Vec<EgclVal> {
    // A read: take the shared accessor and the cached precedence list. The
    // mutable accessor cleared that cache, so every constructor call relinearized
    // its class and discarded every other cached CPL (bliss-mnkee).
    let cpl = compute_class_precedence_list(class).unwrap_or_else(|_| vec![class]);
    with_state(|st| {
        let mut order: Vec<EgclVal> = Vec::new();
        for c in cpl.iter().rev() {
            if let Some(meta) = st.class_meta.get(c) {
                for &slot in &meta.slots {
                    if !order.contains(&slot) {
                        order.push(slot);
                    }
                }
            }
        }
        order
    })
}

/// Build a structure instance from POSITIONAL slot values, skipping the CLOS
/// initialization protocol entirely.
///
/// MAKE-INSTANCE is the wrong tool for a DEFSTRUCT constructor and was costing
/// 11us a call: per instantiation it walked the class precedence list for
/// :default-initargs, built a String class name and a Vec<String> of supplied
/// slots, split instance from class slots, evaluated every :initform, and looked
/// up SHARED-INITIALIZE and INITIALIZE-INSTANCE :after methods. A structure has
/// none of those -- CLHS does not run the initialization protocol for one at all,
/// so skipping it is more correct, not less -- and its slot values are already
/// in hand, defaulted by the constructor's own lambda list.
///
/// Values are matched to slots by NAME through the layout index, not by
/// position, because the two orders differ (see `struct_slot_order`). A missing
/// trailing value leaves that slot at its allocated NIL.
pub fn make_struct(class: EgclVal, values: &[EgclVal]) -> Result<EgclVal, EgclError> {
    // allocate_instance allocates and can relocate; the caller's values may be
    // in an unrooted Rust Vec, so root a copy and write from THAT (bliss-334).
    egcl_rt::rooted!(values = values.to_vec());
    let order = struct_slot_order(class);
    let inst = allocate_instance(class)?;
    for (i, &slot) in order.iter().enumerate() {
        if i >= values.len() {
            break;
        }
        unsafe {
            if let Some(idx) = instance_slot_index(inst, slot) {
                *slot_cell(inst, idx) = values[i];
            }
        }
    }
    Ok(inst)
}

pub fn make_instance(class: EgclVal, initargs: &[EgclVal]) -> Result<EgclVal, EgclError> {
    with_state(|st| {
        if is_builtin_class(st, class) {
            return Err(EgclError::Internal(
                "MAKE-INSTANCE does not support built-in classes".into(),
            ));
        }
        Ok(())
    })?;
    // `allocate_instance` allocates on the GC heap (bliss-334) and can fire a
    // relocating minor GC; the caller's initarg storage may not be rooted, so
    // root a copy here and initialize from THAT. Rooting inside the entry
    // point protects every caller (the interpreter, conditions.rs, the #S
    // reader constructor) at once.
    egcl_rt::rooted!(initargs_rooted = initargs.to_vec());
    let inst = allocate_instance(class)?;
    initialize_instance(inst, &initargs_rooted)?;
    Ok(inst)
}

/// Initialize an instance (INITIALIZE-INSTANCE).
/// Per ANSI CL, initialize-instance calls (shared-initialize instance T initargs).
/// Initargs are pairwise (slot-name, value).
pub fn initialize_instance(instance: EgclVal, initargs: &[EgclVal]) -> Result<(), EgclError> {
    // Per spec R5.80: initialize-instance calls shared-initialize with T (all slots)
    shared_initialize(instance, T, initargs)
}

/// Shared initialize (SHARED-INITIALIZE).
///
/// `slot_names` controls which slots are eligible for initialization:
/// - `T` — all slots are eligible; every initarg pair is applied.
/// - `NIL` — no slots are eligible; initargs are ignored.
/// - A symbol value — only the slot with that name is eligible.
/// - A list of symbol values (stored as a Rust slice via `shared_initialize_with_list`)
///   — only slots whose names appear in the list are eligible.
///
/// In full CLOS, `slot_names` also controls which slots receive their
/// `:initform` default values. Since EGCL does not yet store initforms,
/// only the initarg filtering behaviour is implemented.
pub fn shared_initialize(
    instance: EgclVal,
    slot_names: EgclVal,
    initargs: &[EgclVal],
) -> Result<(), EgclError> {
    if slot_names == T {
        shared_initialize_with_list(instance, None, initargs)
    } else if slot_names == NIL {
        // NIL means "don't evaluate initforms for any slots", but explicitly
        // supplied initarg pairs MUST still be applied to their corresponding
        // slots.  Pass eligible = None so all initargs are applied (initforms
        // are not yet implemented, so the behavior is identical to T for now).
        shared_initialize_with_list(instance, None, initargs)
    } else {
        // Single symbol: treat as a one-element list
        shared_initialize_with_list(instance, Some(&[slot_names]), initargs)
    }
}

/// Shared initialize with an explicit list of eligible slot names.
///
/// If `eligible` is `None`, all slots are eligible (equivalent to T).
/// If `eligible` is `Some(list)`, only slot names in that list are eligible.
pub fn shared_initialize_with_list(
    instance: EgclVal,
    eligible: Option<&[EgclVal]>,
    initargs: &[EgclVal],
) -> Result<(), EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    // CLHS 7.1.4: when several initargs name the same slot, the leftmost wins
    // (ansi reinitialize-instance.5/10, shared-initialize.1.3).
    let mut filled: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 1 < initargs.len() {
        let slot_name = initargs[i];
        let value = initargs[i + 1];

        let is_eligible = match eligible {
            None => true, // T: all slots eligible
            Some(names) => names.contains(&slot_name),
        };

        // Initargs naming a slot outside the layout are ignored (best-effort
        // initialization; only declared :instance slots have storage).
        if is_eligible {
            unsafe {
                if let Some(idx) = instance_slot_index(instance, slot_name) {
                    if !filled.contains(&idx) {
                        filled.push(idx);
                        *slot_cell(instance, idx) = value;
                    }
                }
            }
        }

        i += 2;
    }
    Ok(())
}

/// Reinitialize an instance (REINITIALIZE-INSTANCE). R5.81.
/// Per ANSI CL, reinitialize-instance calls (shared-initialize instance NIL initargs)
/// — only explicit initargs are applied, no initforms are evaluated.
pub fn reinitialize_instance(instance: EgclVal, initargs: &[EgclVal]) -> Result<(), EgclError> {
    // Per spec R5.81: reinitialize-instance calls shared-initialize with
    // slot-names = NIL.  Now that shared_initialize handles NIL correctly
    // (applies initargs but skips initforms), we call it directly.
    shared_initialize(instance, NIL, initargs)
}

// ── Slot access ────────────────────────────────────────────────────

// NIL and T are legal slot symbols despite using the SPECIAL tag. Missing
// layout diagnostics must never extract a TAG_SYMBOL index from either one.
fn slot_name_for_error(name: EgclVal) -> String {
    if name == NIL {
        "NIL".into()
    } else if name == T {
        "T".into()
    } else {
        name.symbol_index()
            .and_then(egcl_rt::symbols::symbol_name)
            .unwrap_or_default()
    }
}

/// Get a slot value (SLOT-VALUE). An unbound slot signals `UnboundVariable`
/// (the CLI turns this into `unbound-slot`); a name outside the class layout is
/// a `slot-missing`-style error.
pub fn slot_value(instance: EgclVal, slot_name: EgclVal) -> Result<EgclVal, EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                let v = *slot_cell(instance, idx);
                if v == UNBOUND {
                    Err(EgclError::UnboundVariable(slot_name))
                } else {
                    Ok(v)
                }
            }
            None => Err(EgclError::Internal(format!(
                "slot not present in class layout: {}",
                slot_name_for_error(slot_name)
            ))),
        }
    }
}

/// [`slot_value`], but distinguishing "not an instance slot" from "unbound".
///
/// `None` means the slot is not in this instance's inline layout — it may be an
/// `:allocation :class` slot, which lives in the owning class's cell and is the
/// caller's problem. `Some(Err(UnboundVariable))` means it IS an instance slot and
/// is unbound.
///
/// Exists so a caller can make that distinction with ONE layout lookup. Asking
/// [`slot_present_p`] and then [`slot_value`] answers it with two — two
/// `update_if_obsolete` calls and two hash lookups for one question (bliss-51m0l).
///
/// Also `None` for an obsolete instance. Migrating it here would drop the
/// record of what the redefinition changed, and the caller's general path
/// could then no longer run UPDATE-INSTANCE-FOR-REDEFINED-CLASS: an accessor
/// that touched the instance first left the added slots UNBOUND for good.
pub fn slot_value_if_present(
    instance: EgclVal,
    slot_name: EgclVal,
) -> Option<Result<EgclVal, EgclError>> {
    if !is_instance(instance) {
        return None;
    }
    unsafe {
        let instance = resolve_forwarding(instance);
        let w = *(instance.as_ptr().add(8) as *const *mut ClassWrapper);
        if !w.is_null() && (*w).state.load(Ordering::Acquire) == WRAPPER_OBSOLETE {
            return None;
        }
        let idx = instance_slot_index(instance, slot_name)?;
        let v = *slot_cell(instance, idx);
        Some(if v == UNBOUND {
            Err(EgclError::UnboundVariable(slot_name))
        } else {
            Ok(v)
        })
    }
}

/// Set a slot value ((SETF SLOT-VALUE)).
pub fn set_slot_value(
    instance: EgclVal,
    slot_name: EgclVal,
    new_value: EgclVal,
) -> Result<(), EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                *slot_cell(instance, idx) = new_value;
                Ok(())
            }
            None => Err(EgclError::Internal(format!(
                "slot not present in class layout: {}",
                slot_name_for_error(slot_name)
            ))),
        }
    }
}

/// Store into `slot_name` if it is in the instance's own layout, the writer's
/// counterpart of [`slot_value_if_present`]. `None` when INSTANCE is not an
/// instance, the slot is not an instance slot (a class-allocated or missing
/// slot), or the instance is obsolete, leaving the caller's general path to
/// handle it.
///
/// Never allocates. Migrating an obsolete instance would, and NEW_VALUE is held
/// here only in a local the collector cannot see, so that case is declined
/// rather than migrated.
pub fn set_slot_value_if_present(
    instance: EgclVal,
    slot_name: EgclVal,
    new_value: EgclVal,
) -> Option<()> {
    if !is_instance(instance) {
        return None;
    }
    unsafe {
        let live = resolve_forwarding(instance);
        let w = *(live.as_ptr().add(8) as *const *mut ClassWrapper);
        if !w.is_null() && (*w).state.load(Ordering::Acquire) == WRAPPER_OBSOLETE {
            return None;
        }
        let idx = instance_slot_index(live, slot_name)?;
        *slot_cell(live, idx) = new_value;
        Some(())
    }
}

/// Where `slot_name` lives in `instance`: its layout's identity and the inline
/// slot index under that layout. `None` for a non-instance, an obsolete
/// instance, or a slot the layout does not hold inline (a class-allocated or
/// missing slot).
///
/// The identity is the address of the instance's current class wrapper. A
/// wrapper's layout is frozen and never freed, and a redefinition or
/// CHANGE-CLASS gives the instance a different wrapper, so the pair stays valid
/// for every instance that reports the same identity. Callers use it as an
/// inline-cache key with [`slot_at_layout`] and [`set_slot_at_layout`]; it does
/// not allocate.
pub fn slot_layout_location(instance: EgclVal, slot_name: EgclVal) -> Option<(usize, usize)> {
    let key = instance_layout_key(instance)?;
    let idx = unsafe { instance_slot_index(resolve_forwarding(instance), slot_name)? };
    Some((key, idx))
}

/// The identity of `instance`'s current, non-obsolete layout (its class
/// wrapper's address), or `None` for a non-instance or an obsolete instance.
/// See [`slot_layout_location`].
#[inline]
pub fn instance_layout_key(instance: EgclVal) -> Option<usize> {
    if !is_instance(instance) {
        return None;
    }
    unsafe {
        let w = instance_wrapper(instance);
        if w.is_null() || (*w).state.load(Ordering::Acquire) == WRAPPER_OBSOLETE {
            return None;
        }
        Some(w as usize)
    }
}

/// The identity of the layout a new instance of `class` gets: the address of
/// the class's current, non-obsolete wrapper, the same value
/// [`instance_layout_key`] reports for such an instance. `None` when the class
/// has no layout yet, its wrapper is obsolete, or `class` is not a class.
///
/// Unlike allocation, this never finalizes a layout, so it neither allocates
/// nor takes the mutable state accessor. A redefinition gives the class a new
/// wrapper, and wrappers are never freed or reused, so the address is a stable
/// per-layout cache key.
pub fn class_layout_key(class: EgclVal) -> Option<usize> {
    let wrapper = with_state(|st| st.class_meta.get(&class).and_then(|m| m.wrapper))?;
    (wrapper.state.load(Ordering::Acquire) != WRAPPER_OBSOLETE)
        .then_some(wrapper as *const ClassWrapper as usize)
}

/// Read inline slot `idx` of `instance` if the instance's layout is still
/// `key` (see [`slot_layout_location`]). `None` when the layout differs, the
/// instance is obsolete, or the slot is unbound: the caller takes its full
/// path, which signals or migrates as required. Does not allocate.
#[inline]
pub fn slot_at_layout(instance: EgclVal, key: usize, idx: usize) -> Option<EgclVal> {
    if instance_layout_key(instance)? != key {
        return None;
    }
    let v = unsafe { *slot_cell(resolve_forwarding(instance), idx) };
    (v != UNBOUND).then_some(v)
}

/// Store `new_value` in inline slot `idx` of `instance` if its layout is still
/// `key` (see [`slot_layout_location`]). Returns whether it stored. Does not
/// allocate.
#[inline]
pub fn set_slot_at_layout(instance: EgclVal, key: usize, idx: usize, new_value: EgclVal) -> bool {
    if instance_layout_key(instance) != Some(key) {
        return false;
    }
    unsafe {
        *slot_cell(resolve_forwarding(instance), idx) = new_value;
    }
    true
}

/// Atomically compare and exchange an inline instance slot.
pub fn compare_exchange_slot_value(
    instance: EgclVal,
    slot_name: EgclVal,
    old: EgclVal,
    new: EgclVal,
) -> Result<EgclVal, EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::TypeError {
            datum: instance,
            expected: "STANDARD-OBJECT".into(),
        });
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        let Some(index) = instance_slot_index(instance, slot_name) else {
            return Err(EgclError::Internal(format!(
                "slot not present in class layout: {}",
                slot_name_for_error(slot_name)
            )));
        };
        Ok(egcl_rt::sync::atomic::compare_exchange_ref(
            slot_cell(instance, index),
            old,
            new,
        ))
    }
}

pub fn atomic_update_slot_value(
    instance: EgclVal,
    slot_name: EgclVal,
    delta: EgclVal,
    subtract: bool,
) -> Result<EgclVal, EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::TypeError {
            datum: instance,
            expected: "STANDARD-OBJECT".into(),
        });
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        let Some(index) = instance_slot_index(instance, slot_name) else {
            return Err(EgclError::Internal(format!(
                "slot not present in class layout: {}",
                slot_name_for_error(slot_name)
            )));
        };
        egcl_rt::sync::atomic::update_fixnum_ref(slot_cell(instance, index), delta, subtract)
    }
}

/// Check if a slot is bound (SLOT-BOUNDP). A name outside the layout is treated
/// as unbound (returns `false`) rather than an error.
pub fn slot_boundp(instance: EgclVal, slot_name: EgclVal) -> Result<bool, EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => Ok(*slot_cell(instance, idx) != UNBOUND),
            None => Ok(false),
        }
    }
}

/// Is `slot_name` part of this instance's class layout, bound or not?
///
/// Distinct from [`slot_boundp`], which answers `false` for both an unbound slot
/// and a slot the class does not have. Funcallable instances need the layout
/// question on its own: an instance of a FUNCALLABLE-STANDARD-CLASS is of type
/// FUNCTION from the moment it exists (AMOP), before anything has been installed
/// in its function cell, so FUNCTIONP keys on the cell being PRESENT while the
/// call path keys on it being BOUND (bliss-cr53).
pub fn slot_present_p(instance: EgclVal, slot_name: EgclVal) -> bool {
    if !is_instance(instance) {
        return false;
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        instance_slot_index(instance, slot_name).is_some()
    }
}

/// The inline index of `instance`'s slot whose bare name is `bare_name`, cached
/// per layout. Only for the one hidden funcallable-function slot: the cache
/// does not record which name it was computed for.
pub fn funcallable_slot_index(instance: EgclVal, bare_name: &str) -> Option<usize> {
    if !is_instance(instance) {
        return None;
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        let w = instance_wrapper(instance);
        if w.is_null() {
            return None;
        }
        let layout = (*w).layout;
        *layout.funcallable_slot.get_or_init(|| {
            layout
                .order
                .iter()
                .position(|&s| bare_symbol_name(s).as_deref() == Some(bare_name))
        })
    }
}

/// The value in inline slot `idx` of `instance`, or None when it is unbound.
/// `idx` must come from [`funcallable_slot_index`] for this same instance.
pub fn instance_slot_at(instance: EgclVal, idx: usize) -> Option<EgclVal> {
    if !is_instance(instance) {
        return None;
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        let v = *slot_cell(instance, idx);
        (v != UNBOUND).then_some(v)
    }
}

/// `(typep object 'NAME)` for a structure instance and a structure class name,
/// answered under one lock with no allocation. None defers to the general
/// TYPEP path: not a current structure instance, NAME not registered under
/// this exact symbol, NAME not a structure class, or no cached precedence list
/// yet. Neither a structure instance nor a structure class can be a stream,
/// funcallable instance or class metaobject, so whenever this answers, the
/// general path would reach the same precedence-list test.
pub fn structure_instance_typep(object: EgclVal, name: EgclVal) -> Option<bool> {
    if !name.is_symbol()
        || egcl_rt::gc::heap_object_type_id(object) != Some(type_id::STANDARD_OBJECT)
    {
        return None;
    }
    let actual = unsafe {
        let w = instance_wrapper(object);
        if w.is_null() || (*w).state.load(Ordering::Acquire) != WRAPPER_CURRENT {
            return None;
        }
        (*w).class
    };
    with_state(|st| {
        let expected = *st.class_registry.get(&name)?;
        if !st.structure_classes.contains(&actual) || !st.structure_classes.contains(&expected) {
            return None;
        }
        if actual == expected {
            return Some(true);
        }
        st.cpl_cache
            .borrow()
            .get(&actual)
            .map(|cpl| cpl.contains(&expected))
    })
}

/// The current structure wrapper of `object` and its [`StructInfo`], or None
/// when `object` is not an instance, its wrapper is obsolete or absent, or its
/// class is not a (prefix-laid-out) structure class. Does not allocate.
#[inline]
fn current_struct_wrapper(object: EgclVal) -> Option<(&'static ClassWrapper, &'static StructInfo)> {
    if egcl_rt::gc::heap_object_type_id(object) != Some(type_id::STANDARD_OBJECT) {
        return None;
    }
    unsafe {
        let w = instance_wrapper(object);
        if w.is_null() || (*w).state.load(Ordering::Acquire) != WRAPPER_CURRENT {
            return None;
        }
        let info = (*w).struct_info.load(Ordering::Acquire);
        if info.is_null() {
            return None;
        }
        Some((&*w, &*info))
    }
}

/// The inline index of `slot` in `wrapper`'s layout, trying `hint` first.
#[inline]
fn struct_slot_index(wrapper: &ClassWrapper, slot: EgclVal, hint: usize) -> Option<usize> {
    let order = &wrapper.layout.order;
    if order.get(hint) == Some(&slot) {
        return Some(hint);
    }
    order.iter().position(|&s| s == slot)
}

/// Lock-free `(name-slot object)` for a structure accessor: `object` is a
/// current instance of the structure named `name` or of one that includes it,
/// and its bound `slot` (expected at `hint`) is returned. None sends the caller
/// to the general path, which signals the TYPE-ERROR or unbound-slot error.
/// Does not allocate.
pub fn struct_ref_fast(
    object: EgclVal,
    name: EgclVal,
    slot: EgclVal,
    hint: usize,
) -> Option<EgclVal> {
    let (wrapper, info) = current_struct_wrapper(object)?;
    if !info.names.contains(&name) {
        return None;
    }
    let idx = struct_slot_index(wrapper, slot, hint)?;
    let v = unsafe { *slot_cell(resolve_forwarding(object), idx) };
    (v != UNBOUND).then_some(v)
}

/// Lock-free store for a structure writer; see [`struct_ref_fast`]. The caller
/// has already checked `value` against the slot's declared type. Returns
/// whether it stored. Does not allocate.
pub fn struct_set_fast(
    object: EgclVal,
    name: EgclVal,
    slot: EgclVal,
    hint: usize,
    value: EgclVal,
) -> bool {
    let Some((wrapper, info)) = current_struct_wrapper(object) else {
        return false;
    };
    if !info.names.contains(&name) {
        return false;
    }
    let Some(idx) = struct_slot_index(wrapper, slot, hint) else {
        return false;
    };
    unsafe {
        *slot_cell(resolve_forwarding(object), idx) = value;
    }
    true
}

/// Lock-free `(typep object 'name)` for a structure name: true for a current
/// structure instance with `name` in its ancestry, false for one without it
/// and for anything that is not an instance. Any other instance, and an
/// obsolete one, defers to TYPEP (None): if `name` were later redefined as a
/// standard class, only such an instance could be of it. Does not allocate.
pub fn struct_typep_fast(object: EgclVal, name: EgclVal) -> Option<bool> {
    if egcl_rt::gc::heap_object_type_id(object) != Some(type_id::STANDARD_OBJECT) {
        return Some(false);
    }
    let (_, info) = current_struct_wrapper(object)?;
    Some(info.names.contains(&name))
}

/// Make a slot unbound (SLOT-MAKUNBOUND).
pub fn slot_makunbound(instance: EgclVal, slot_name: EgclVal) -> Result<(), EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    unsafe {
        let instance = update_if_obsolete(instance);
        if let Some(idx) = instance_slot_index(instance, slot_name) {
            *slot_cell(instance, idx) = UNBOUND;
        }
    }
    Ok(())
}

// ── Generic function dispatch ──────────────────────────────────────

/// Create a generic function.
pub fn make_generic_function(name: EgclVal, lambda_list: EgclVal) -> Result<EgclVal, EgclError> {
    with_state_mut(|st| {
        let id = st.alloc_gf_id();
        st.generic_functions.insert(
            id,
            GFData {
                name,
                lambda_list,
                methods: Vec::new(),
            },
        );
        Ok(id)
    })
}

/// The name form a generic function was created with (a symbol, or a `(setf …)`
/// cons). Used by image serialization to recover a generic's real, correctly
/// package-homed name symbol from its interpreter-side id.
pub fn generic_function_name(gf: EgclVal) -> Option<EgclVal> {
    with_state(|st| st.generic_functions.get(&gf).map(|d| d.name))
}

/// Add a method to a generic function. Upserts the tracking entry if the
/// function is not already registered here: the interpreter's live dispatch uses
/// its own method table (cli.rs env.methods), so this map only tracks methods,
/// and a generic function reaching add-method without a prior make_generic_function
/// entry (e.g. one restored from an image) must not be an error (bliss-lb6).
/// True for a method handle registered through `set_method_specializers`.
pub fn is_method_object(value: EgclVal) -> bool {
    value.is_meta_handle() && with_state(|st| st.method_meta.contains_key(&value))
}

/// True for a generic function object made by `make_generic_function`.
pub fn is_generic_function_object(value: EgclVal) -> bool {
    value.is_meta_handle() && with_state(|st| st.generic_functions.contains_key(&value))
}

pub fn add_method(generic_function: EgclVal, method: EgclVal) -> Result<(), EgclError> {
    with_state_mut(|st| {
        st.generic_functions
            .entry(generic_function)
            .or_insert_with(|| GFData {
                name: NIL,
                lambda_list: NIL,
                methods: Vec::new(),
            })
            .methods
            .push(method);
        Ok(())
    })
}

/// Remove a method from a generic function. A no-op if the function is not
/// tracked here (removing a method that is not present is harmless) — used by
/// ASDF's upgrade machinery (bliss-lb6).
pub fn remove_method(generic_function: EgclVal, method: EgclVal) -> Result<(), EgclError> {
    with_state_mut(|st| {
        if let Some(gf) = st.generic_functions.get_mut(&generic_function) {
            gf.methods.retain(|m| *m != method);
        }
        Ok(())
    })
}

/// Register specializer and qualifier metadata for a method.
/// Specializers are a list of class values, one per required parameter.
/// An empty specializers list means the method is unspecialized (applies to all).
pub fn set_method_specializers(
    method: EgclVal,
    specializers: Vec<EgclVal>,
    qualifier: MethodQualifier,
) {
    with_state_mut(|st| {
        st.method_meta.insert(
            method,
            MethodMeta {
                specializers,
                qualifier,
            },
        );
    });
}

/// Check if a method's specializer at position `i` is applicable to argument
/// class `arg_class`, i.e. `arg_class` is a subtype of the specializer.
fn specializer_applicable(st: &ClosState, specializer: EgclVal, arg_class: EgclVal) -> bool {
    if specializer == st.t_class_val {
        return true; // T matches everything
    }
    if specializer == arg_class {
        return true; // exact match
    }
    // Walk the CPL of arg_class to check if specializer appears
    if let Ok(cpl) = linearize_class_precedence(st, arg_class) {
        cpl.contains(&specializer)
    } else {
        false
    }
}

/// Distance standing for an unspecialized (T) parameter: less specific than any
/// position a real class can occupy in a precedence list.
const UNSPECIALIZED_DISTANCE: usize = usize::MAX / 4;

/// The specificity of a method as one distance per argument: the position of its
/// specializer in that argument's class precedence list, lower being more
/// specific.
///
/// CLHS 7.6.6.1.2 orders two methods by comparing these distances one argument
/// at a time and letting the first difference decide — so the key is a vector
/// compared lexicographically, not a sum. Summing let a method that is more
/// specific on a later argument outrank one that is more specific on an earlier
/// argument (bliss-nj6id).
///
/// `precedence` is the generic function's argument precedence order, as
/// positions into its required parameters; empty means left to right. The
/// comparison happens in that order, which is why it is applied to the key here
/// rather than to the arguments.
fn method_specificity_key(
    st: &ClosState,
    method: EgclVal,
    arg_classes: &[EgclVal],
    precedence: &[usize],
) -> Vec<usize> {
    let Some(meta) = st.method_meta.get(&method) else {
        // No specializer metadata: unspecialized, and so least specific.
        return vec![UNSPECIALIZED_DISTANCE; arg_classes.len().max(1)];
    };
    let distance = |i: usize, spec: EgclVal| -> usize {
        if spec == st.t_class_val || spec == NIL {
            return UNSPECIALIZED_DISTANCE;
        }
        match linearize_class_precedence(st, arg_classes[i]) {
            Ok(cpl) => cpl
                .iter()
                .position(|&c| c == spec)
                .unwrap_or(UNSPECIALIZED_DISTANCE),
            Err(_) => UNSPECIALIZED_DISTANCE,
        }
    };
    let mut distances = Vec::with_capacity(arg_classes.len());
    for (i, spec) in meta.specializers.iter().enumerate() {
        if i >= arg_classes.len() {
            break;
        }
        distances.push(distance(i, *spec));
    }
    if precedence.is_empty() {
        return distances;
    }
    precedence
        .iter()
        .map(|&i| distances.get(i).copied().unwrap_or(UNSPECIALIZED_DISTANCE))
        .collect()
}

/// The property indicator under which DEFGENERIC records a generic function's
/// argument precedence order on its name symbol.
fn argument_precedence_indicator() -> EgclVal {
    use std::sync::OnceLock;
    static INDICATOR: OnceLock<u32> = OnceLock::new();
    EgclVal::from_symbol_index(
        *INDICATOR.get_or_init(|| egcl_rt::symbols::intern(ARGUMENT_PRECEDENCE_PROPERTY)),
    )
}

/// The name of that indicator, so the writer (DEFGENERIC, which has the cons
/// allocator) and this reader agree on one symbol.
pub const ARGUMENT_PRECEDENCE_PROPERTY: &str = "EGCL::%ARGUMENT-PRECEDENCE-ORDER";

/// The argument precedence order DEFGENERIC recorded for the generic function
/// named by `name`, as positions into its required parameters. Empty means the
/// default left-to-right order.
///
/// It is kept as a symbol property rather than in a host-side registry so that
/// it travels with a saved image like any other Lisp datum. Reading it allocates
/// nothing.
pub fn argument_precedence_order(name: EgclVal) -> Vec<usize> {
    let Some(index) = name.symbol_index() else {
        return Vec::new(); // a (setf f) name is a cons and carries no plist
    };
    let Some(plist) = egcl_rt::symbols::symbol_plist(index) else {
        return Vec::new();
    };
    let indicator = argument_precedence_indicator();
    let mut order = Vec::new();
    let mut cursor = plist;
    while cursor.is_cons() {
        // SAFETY: every cons cell walked here is one this list's own CAR/CDR
        // reached; no allocation happens in the loop, so none can go stale.
        let cell = unsafe { &*(cursor.as_ptr() as *const egcl_rt::object::ConsCell) };
        if !cell.cdr.is_cons() {
            break; // malformed plist: an indicator with no value
        }
        let value_cell = unsafe { &*(cell.cdr.as_ptr() as *const egcl_rt::object::ConsCell) };
        if cell.car == indicator {
            let mut positions = value_cell.car;
            while positions.is_cons() {
                let position =
                    unsafe { &*(positions.as_ptr() as *const egcl_rt::object::ConsCell) };
                if position.car.is_fixnum() {
                    order.push(position.car.as_fixnum().max(0) as usize);
                }
                positions = position.cdr;
            }
            return order;
        }
        cursor = value_cell.cdr;
    }
    order
}

/// One required-parameter specializer of a method, as
/// [`applicable_methods_using_classes`] reads it.
#[derive(Clone, Copy, Debug)]
pub enum ClassSpecializer {
    /// Unspecialized: the class T.
    Any,
    /// A class specializer, as a class metaobject.
    Class(EgclVal),
    /// An EQL specializer, given as the class of its object: whether it applies
    /// cannot be decided from classes alone when an instance of an argument
    /// class could be that object.
    EqlOfClass(EgclVal),
}

/// AMOP COMPUTE-APPLICABLE-METHODS-USING-CLASSES over `methods` (method
/// handle and per-required-parameter specializers, in definition order) for
/// arguments of the given `classes`.
///
/// `None` is the AMOP second value NIL: some method has an EQL specializer an
/// instance of the corresponding class could satisfy, so applicability depends
/// on the actual arguments. Otherwise the applicable methods, most specific
/// first, compared one argument at a time in `precedence` order (positions
/// into the required parameters; empty means left to right), as CLHS 7.6.6.1.2
/// orders them for dispatch.
///
/// Reads class precedence lists only; allocates nothing on the Lisp heap.
pub fn applicable_methods_using_classes(
    methods: &[(EgclVal, Vec<ClassSpecializer>)],
    classes: &[EgclVal],
    precedence: &[usize],
) -> Option<Vec<EgclVal>> {
    let cpls: Vec<Vec<EgclVal>> = classes
        .iter()
        .map(|&class| compute_class_precedence_list(class).unwrap_or_else(|_| vec![class]))
        .collect();
    let mut applicable: Vec<(Vec<usize>, EgclVal)> = Vec::new();
    'methods: for (method, specializers) in methods {
        if specializers.len() > classes.len() {
            continue;
        }
        let mut distances = Vec::with_capacity(specializers.len());
        for (i, specializer) in specializers.iter().enumerate() {
            match *specializer {
                ClassSpecializer::Any => distances.push(UNSPECIALIZED_DISTANCE),
                ClassSpecializer::Class(class) => {
                    match cpls[i].iter().position(|&c| c == class) {
                        Some(position) => distances.push(position),
                        // T is at the end of every list; a class specializer of
                        // T that is not found still applies to everything.
                        None if is_t_class(class) => distances.push(UNSPECIALIZED_DISTANCE),
                        None => continue 'methods,
                    }
                }
                ClassSpecializer::EqlOfClass(object_class) => {
                    // An instance of classes[i] could be the EQL object exactly
                    // when the object's class is classes[i] or a subclass of it.
                    let object_cpl = compute_class_precedence_list(object_class)
                        .unwrap_or_else(|_| vec![object_class]);
                    if object_cpl.contains(&classes[i]) {
                        return None;
                    }
                    continue 'methods;
                }
            }
        }
        let key = if precedence.is_empty() {
            distances
        } else {
            precedence
                .iter()
                .map(|&i| distances.get(i).copied().unwrap_or(UNSPECIALIZED_DISTANCE))
                .collect()
        };
        applicable.push((key, *method));
    }
    // Stable, so methods equally specific keep definition order.
    applicable.sort_by(|a, b| a.0.cmp(&b.0));
    Some(applicable.into_iter().map(|(_, method)| method).collect())
}

fn is_t_class(class: EgclVal) -> bool {
    with_state(|st| class == st.t_class_val)
}

/// Compute the applicable methods for given arguments.
///
/// Filters the generic function's methods to those whose specializers
/// are supertypes of the corresponding argument classes, then sorts
/// most-specific-first using CPL position.
pub fn compute_applicable_methods(generic_function: EgclVal, args: &[EgclVal]) -> Vec<EgclVal> {
    let arg_classes: Vec<EgclVal> = args.iter().copied().map(class_of).collect();
    // Read the precedence order before taking the CLOS state lock: it reads the
    // symbol registry, and no path here needs both at once.
    let precedence = generic_function_name(generic_function)
        .map(argument_precedence_order)
        .unwrap_or_default();
    with_state(|st| {
        let gf = match st.generic_functions.get(&generic_function) {
            Some(gf) => gf,
            None => return Vec::new(),
        };

        if gf.methods.is_empty() {
            return Vec::new();
        }

        // Filter: keep methods whose specializers match the argument classes
        let mut applicable: Vec<EgclVal> = gf
            .methods
            .iter()
            .copied()
            .filter(|&m| {
                match st.method_meta.get(&m) {
                    Some(meta) => {
                        // Each specializer must be applicable to the corresponding arg
                        for (i, spec) in meta.specializers.iter().enumerate() {
                            if i >= arg_classes.len() {
                                break;
                            }
                            if !specializer_applicable(st, *spec, arg_classes[i]) {
                                return false;
                            }
                        }
                        true
                    }
                    None => {
                        // No specializer metadata: method is unspecialized,
                        // applicable to all arguments
                        true
                    }
                }
            })
            .collect();

        // Sort by specificity: most specific first, comparing the arguments in
        // the generic function's argument precedence order.
        applicable
            .sort_by_cached_key(|&m| method_specificity_key(st, m, &arg_classes, &precedence));

        applicable
    })
}

/// Compute the effective method for a set of applicable methods.
///
/// For `Standard` combination, the effective method is determined by
/// method qualifiers:
/// - `:around` methods wrap the call chain (outermost first)
/// - `:before` methods run before the primary
/// - The most-specific primary method is the core
/// - `:after` methods run after the primary (least-specific first)
///
/// Without qualifier metadata, the first method is the primary.
///
/// For non-Standard combinations (Plus, And, Or, etc.), the combination
/// type's discriminant is encoded into the result so every combination
/// produces a distinct effective method value. In a full implementation
/// these would invoke each primary method and combine results via
/// the operator (e.g., `+` for Plus, `and` for And).
pub fn compute_effective_method(
    generic_function: EgclVal,
    combination: MethodCombinationType,
    methods: &[EgclVal],
) -> Result<EgclVal, EgclError> {
    compute_effective_method_with_standard_primary(generic_function, combination, methods, false)
}

/// [`compute_effective_method`] for a generic function that has a standard
/// primary method of its own outside `methods` when `standard_primary` is
/// true: the caller runs that method after the last primary in `methods`, so a
/// standard combination whose applicable methods are all auxiliary is not an
/// error (CLHS 7.6.6.2).
pub fn compute_effective_method_with_standard_primary(
    _generic_function: EgclVal,
    combination: MethodCombinationType,
    methods: &[EgclVal],
    standard_primary: bool,
) -> Result<EgclVal, EgclError> {
    if methods.is_empty() {
        return Err(EgclError::Internal(
            "no applicable methods for effective method computation".into(),
        ));
    }

    match combination {
        MethodCombinationType::Standard => {
            // Standard method combination: separate methods by qualifier,
            // build the effective method chain per ANSI CL / spec §5.2.9 R5.77.
            //
            // The effective method executes:
            // 1. :around methods (most-specific-first), each can call-next-method
            // 2. :before methods (most-specific-first)
            // 3. Primary methods (most-specific-first), with call-next-method chain
            // 4. :after methods (least-specific-first, i.e. reversed)
            //
            // We represent the effective method as a composite: we store the
            // method chain in EFFECTIVE_METHODS and return a synthetic key.
            // For the simple case (no :around), the primary method value is
            // returned directly. When there are auxiliary methods, we build
            // an EffectiveMethod descriptor.

            let mut invalid_qualifier = false;
            let (around, before, primary, after) = with_state(|st| {
                let mut around = Vec::new();
                let mut before = Vec::new();
                let mut primary = Vec::new();
                let mut after = Vec::new();

                for &m in methods {
                    match st.method_meta.get(&m) {
                        Some(meta) => match meta.qualifier {
                            MethodQualifier::Around => around.push(m),
                            MethodQualifier::Before => before.push(m),
                            MethodQualifier::Primary => primary.push(m),
                            MethodQualifier::After => after.push(m),
                            // CLHS 7.6.6.2: the standard method combination
                            // signals an error for any other qualifier.
                            MethodQualifier::Combination | MethodQualifier::Other => {
                                invalid_qualifier = true
                            }
                        },
                        None => primary.push(m), // no metadata = primary
                    }
                }

                // :after methods execute least-specific-first
                after.reverse();

                (around, before, primary, after)
            });
            if invalid_qualifier {
                return Err(EgclError::ProgramError(
                    "an applicable method has a qualifier the standard method combination does not accept".into(),
                ));
            }

            if primary.is_empty() && !standard_primary {
                return Err(EgclError::Internal(
                    "no primary method found in standard combination".into(),
                ));
            }

            // Store the effective method chain for later invocation
            let em_key = with_state_mut(|st| {
                let key = st.alloc_em_key();
                st.effective_methods.insert(
                    key,
                    EffectiveMethod {
                        around,
                        before,
                        primary,
                        after,
                    },
                );
                key
            });

            Ok(em_key)
        }
        other => {
            // Non-standard (short-form) combinations per spec §5.2.9 R5.78/R5.79.
            // These apply an operator to the results of all primary methods.
            // We store the method list and combination type for later invocation.

            // CLHS 7.6.6.4: a short-form combination's primary methods carry the
            // combination's name as their one qualifier; :AROUND methods are run
            // by the caller around the combined result; an unqualified method or
            // any other qualifier is an error when such a method is applicable.
            let mut invalid_qualifier = false;
            let primary_methods: Vec<EgclVal> = with_state(|st| {
                methods
                    .iter()
                    .copied()
                    .filter(|m| match st.method_meta.get(m) {
                        Some(meta) => match meta.qualifier {
                            MethodQualifier::Combination => true,
                            MethodQualifier::Around => false,
                            _ => {
                                invalid_qualifier = true;
                                false
                            }
                        },
                        None => true, // no metadata = primary
                    })
                    .collect()
            });
            if invalid_qualifier {
                return Err(EgclError::ProgramError(
                    "an applicable method has a qualifier its method combination does not accept"
                        .into(),
                ));
            }

            if primary_methods.is_empty() {
                return Err(EgclError::ProgramError(
                    "no applicable primary method for the method combination".into(),
                ));
            }

            // Store the short-form effective method for later invocation
            let em_key = with_state_mut(|st| {
                let key = st.alloc_em_key();
                st.short_form_methods.insert(
                    key,
                    ShortFormMethod {
                        combination: other,
                        methods: primary_methods,
                    },
                );
                key
            });

            Ok(em_key)
        }
    }
}

// ── Effective method accessors ─────────────────────────────────────

/// Retrieve a standard-combination effective method descriptor by key.
///
/// Returns the around, before, primary, and after method lists.
pub fn get_effective_method(key: EgclVal) -> Option<EffectiveMethodParts> {
    with_state(|st| {
        st.effective_methods.get(&key).map(|em| {
            (
                em.around.clone(),
                em.before.clone(),
                em.primary.clone(),
                em.after.clone(),
            )
        })
    })
}

/// Retrieve a short-form combination effective method descriptor by key.
///
/// Returns the combination type and the ordered list of methods.
pub fn get_short_form_method(key: EgclVal) -> Option<(MethodCombinationType, Vec<EgclVal>)> {
    with_state(|st| {
        st.short_form_methods
            .get(&key)
            .map(|sfm| (sfm.combination, sfm.methods.clone()))
    })
}

// ── Class change protocol ──────────────────────────────────────────

/// Change the class of an instance (CHANGE-CLASS). R5.16, R5.82 §5.2.11.
///
/// Per the spec, change-class must:
/// 1. Snapshot the old instance state (old class, old slots).
/// 2. Determine which slots are shared between old and new class.
/// 3. Copy values of shared slots to the new instance.
/// 4. Swap the class pointer (wrapper) to the new class.
/// 5. Call update-instance-for-different-class with the old snapshot
///    and the updated instance.
pub fn change_class(instance: EgclVal, new_class: EgclVal) -> Result<(), EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::Internal("not an instance".into()));
    }
    // CHANGE-CLASS into a forward reference is the same error as making one
    // (see `allocate_instance`), and it does not reach that check: a change to
    // a class with no MORE slots than the instance already has re-points the
    // wrapper in place and never allocates (bliss-jv7tj).
    if is_forward_referenced(new_class) {
        return Err(EgclError::ProgramError(format!(
            "cannot change an instance to {}: it is a forward-referenced class, \
             named as a superclass but never defined",
            class_name(new_class)
                .symbol_index()
                .and_then(egcl_rt::symbols::symbol_name)
                .unwrap_or_else(|| "a forward-referenced class".to_string())
        )));
    }

    // Step 1: snapshot the old instance's bound slots by name, and its inline
    // capacity (the number of cells the allocation was sized for).
    let (_old_capacity, snapshot): (usize, Vec<(EgclVal, EgclVal)>) = unsafe {
        let ow = instance_wrapper(instance);
        let cap = if ow.is_null() {
            0
        } else {
            (*ow).slot_count as usize
        };
        let mut snap = Vec::new();
        if !ow.is_null() {
            let ol = (*ow).layout;
            for (i, &name) in ol.order.iter().enumerate() {
                let v = *slot_cell(instance, i);
                if v != UNBOUND {
                    snap.push((name, v));
                }
            }
        }
        (cap, snap)
    };

    // Step 2: fetch the new class's wrapper + slot count (finalize if needed).
    let (new_wrapper, new_count) = with_state_mut(|st| {
        let mut w = st
            .class_meta
            .get(&new_class)
            .and_then(|m| m.wrapper)
            .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, new_class);
            w = st
                .class_meta
                .get(&new_class)
                .and_then(|m| m.wrapper)
                .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
                .unwrap_or(std::ptr::null_mut());
        }
        let n = if w.is_null() {
            0
        } else {
            unsafe { (*w).slot_count as usize }
        };
        (w, n)
    });
    if new_wrapper.is_null() {
        return Err(EgclError::Internal(
            "cannot change to a class with no slot layout".into(),
        ));
    }

    {
        // ONE path, whatever the slot counts: allocate storage for the new
        // layout and install it with the new wrapper in the SAME shell, then
        // copy shared slots by name (R5.82); added slots stay UNBOUND. The
        // instance keeps its address, which is what CLHS 7.2 asks for and what
        // the old growth branch below needed a forwarding stub to fake
        // (bliss-fmhj5).
        //
        // GC safety: the storage allocation can relocate the instance and the
        // snapshotted values, so both are rooted across it.
        let mut instance = instance;
        let mut snapshot = snapshot;
        egcl_rt::rooted_ref!(_inst_root = &mut instance);
        egcl_rt::rooted_ref!(_snap_root = &mut snapshot);
        let pinned = with_state(|st| st.class_meta.contains_key(&instance));
        let mut storage = alloc_slot_storage(new_count, pinned).ok_or_else(|| {
            EgclError::Internal("GC heap unavailable for changed-class slots".into())
        })?;
        egcl_rt::rooted_ref!(_storage_root = &mut storage);
        unsafe {
            set_instance_storage(instance, storage);
            set_instance_wrapper(instance, new_wrapper);
            let nl = (*new_wrapper).layout;
            for (name, val) in &snapshot {
                if let Some(&idx) = nl.index.get(name) {
                    *slot_cell(instance, idx) = *val;
                }
            }
        }
    }
    Ok(())
}

/// Capture the original instance's local slot state for a class-change hook.
/// Unbound slots remain unbound; copying does not run initialization methods.
pub fn copy_instance_for_class_change(instance: EgclVal) -> Result<EgclVal, EgclError> {
    if !is_instance(instance) {
        return Err(EgclError::TypeError {
            datum: instance,
            expected: "standard-object".into(),
        });
    }
    egcl_rt::rooted!(instance = instance);
    egcl_rt::rooted!(slots = effective_slots(class_of(*instance)));
    egcl_rt::rooted!(values = Vec::<EgclVal>::with_capacity(slots.len()));
    for index in 0..slots.len() {
        let value = if slot_boundp(*instance, slots[index])? {
            slot_value(*instance, slots[index])?
        } else {
            UNBOUND
        };
        values.push(value);
    }
    egcl_rt::rooted!(copy = allocate_instance(class_of(*instance))?);
    for index in 0..slots.len() {
        if values[index] != UNBOUND {
            set_slot_value(*copy, slots[index], values[index])?;
        }
    }
    Ok(*copy)
}
type EffectiveMethodParts = (Vec<EgclVal>, Vec<EgclVal>, Vec<EgclVal>, Vec<EgclVal>);

#[cfg(test)]
mod cpl_tests {
    use super::*;
    use std::cell::Cell;

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_heap_guard();
        with_state_mut(|state| *state = ClosState::new());
        guard
    }

    thread_local! {
        pub(super) static LINEARIZATIONS: Cell<usize> = const { Cell::new(0) };
    }

    fn class(id: i64) -> EgclVal {
        EgclVal::from_fixnum(id)
    }

    fn define(id: i64, supers: &[EgclVal]) -> EgclVal {
        let value = class(id);
        define_class(value, value, supers, &[]).unwrap();
        value
    }

    #[test]
    fn missing_constant_slot_names_report_errors_without_symbol_index_panics() {
        let _guard = test_guard();
        bootstrap_clos().unwrap();
        let class = class(50501);
        let ordinary_slot =
            EgclVal::from_symbol_index(egcl_rt::symbols::intern("GH772-ORDINARY-SLOT"));
        define_class(class, class, &[], &[ordinary_slot]).unwrap();
        egcl_rt::rooted!(instance = allocate_instance(class).unwrap());
        for slot in [T, NIL] {
            let message = match slot_value(*instance, slot) {
                Err(EgclError::Internal(message)) => message,
                other => panic!("missing legal slot name should return an error: {other:?}"),
            };
            assert!(message.ends_with(if slot == T { "T" } else { "NIL" }));
            assert!(set_slot_value(*instance, slot, NIL).is_err());
            assert!(compare_exchange_slot_value(*instance, slot, NIL, T).is_err());
            assert!(
                atomic_update_slot_value(*instance, slot, EgclVal::from_fixnum(1), false).is_err()
            );
        }
    }

    #[test]
    fn repeated_cpl_queries_linearize_once_and_return_independent_vectors() {
        let _guard = test_guard();
        // R5.67: preserve C3 diamond order without rebuilding the hierarchy
        // on every method applicability query.
        let a = define(50001, &[]);
        let b = define(50002, &[a]);
        let c = define(50003, &[a]);
        let d = define(50004, &[b, c]);
        LINEARIZATIONS.set(0);
        let expected = vec![d, b, c, a];
        for _ in 0..1000 {
            let mut result = compute_class_precedence_list(d).unwrap();
            assert_eq!(result, expected);
            result.clear();
        }
        assert_eq!(LINEARIZATIONS.get(), 1, "repeated queries must reuse C3");
    }

    #[test]
    fn superclass_redefinition_invalidates_descendant_cpl_and_cycles() {
        let _guard = test_guard();
        let a = define(51001, &[]);
        let b = define(51002, &[a]);
        let c = define(51003, &[b]);
        assert_eq!(compute_class_precedence_list(c).unwrap(), vec![c, b, a]);
        let new_base = define(51004, &[]);
        define(51002, &[new_base]);
        assert_eq!(
            compute_class_precedence_list(c).unwrap(),
            vec![c, b, new_base]
        );
        define(51002, &[c]);
        assert!(compute_class_precedence_list(c).is_err());
        define(51002, &[a]);
        assert_eq!(compute_class_precedence_list(c).unwrap(), vec![c, b, a]);
    }

    #[test]
    fn gc_root_scan_discards_derived_cpl_before_visiting_roots() {
        let _guard = test_guard();
        let a = define(52001, &[]);
        let b = define(52002, &[a]);
        let expected = vec![b, a];
        assert_eq!(compute_class_precedence_list(b).unwrap(), expected);
        LINEARIZATIONS.set(0);
        scan_clos_state_roots(&mut |_| {});
        assert_eq!(compute_class_precedence_list(b).unwrap(), expected);
        assert_eq!(LINEARIZATIONS.get(), 1);
    }

    #[test]
    fn newly_registered_class_replaces_unknown_cpl() {
        let _guard = test_guard();
        let a = define(53001, &[]);
        let b = class(53002);
        assert_eq!(compute_class_precedence_list(b).unwrap(), vec![b]);
        define(53002, &[a]);
        assert_eq!(compute_class_precedence_list(b).unwrap(), vec![b, a]);
    }

    #[test]
    fn registration_order_changes_invalidate_inferred_superclasses() {
        let _guard = test_guard();
        let base = define(54001, &[]);
        with_state_mut(|st| {
            st.standard_object_class = base;
            st.bootstrapped = true;
        });
        let a = EgclVal::from_meta_handle(54010);
        let b = EgclVal::from_meta_handle(54011);
        let c = EgclVal::from_meta_handle(54012);
        let d = EgclVal::from_meta_handle(54013);
        for value in [a, b, c] {
            set_find_class(value, value).unwrap();
        }
        assert_eq!(
            compute_class_precedence_list(c).unwrap(),
            vec![c, b, a, base]
        );
        set_find_class(d, d).unwrap();
        // C was the bottom of the three-class group, but becomes a middle
        // class in the four-class diamond even though C itself wasn't edited.
        assert_eq!(compute_class_precedence_list(c).unwrap(), vec![c, a, base]);
        assert_eq!(
            compute_class_precedence_list(d).unwrap(),
            vec![d, b, c, a, base]
        );
    }

    #[test]
    fn precedence_follows_the_standards_rule_where_c3_differs() {
        let _guard = test_guard();
        // ansi class-0306: H (F G), F (D C), G (E), D (B), E (B), B (A), C (A).
        // CLHS 4.3.5's sort puts C before G: after H F D, both C and G could
        // come next, and C's direct subclass F stands further right than G's
        // direct subclass H. C3 would merge the superclass lists and put B and
        // G ahead of C.
        let a = define(56001, &[]);
        let b = define(56002, &[a]);
        let c = define(56003, &[a]);
        let d = define(56004, &[b]);
        let e = define(56005, &[b]);
        let f = define(56006, &[d, c]);
        let g = define(56007, &[e]);
        let h = define(56008, &[f, g]);
        assert_eq!(
            compute_class_precedence_list(h).unwrap(),
            vec![h, f, d, c, g, e, b, a]
        );
        // And the same classes' own lists, where the two rules agree.
        assert_eq!(
            compute_class_precedence_list(f).unwrap(),
            vec![f, d, b, c, a]
        );
        assert_eq!(compute_class_precedence_list(g).unwrap(), vec![g, e, b, a]);
    }

    #[test]
    fn core_restore_rebuilds_cpl_from_restored_metadata() {
        let _guard = test_guard();
        let a = define(55001, &[]);
        let b = define(55002, &[a]);
        let expected = vec![b, a];
        assert_eq!(compute_class_precedence_list(b).unwrap(), expected);
        let snapshot = serialize_clos_state();
        define(55002, &[]);
        assert_eq!(compute_class_precedence_list(b).unwrap(), vec![b]);
        assert_eq!(
            restore_clos_state(&snapshot, &|value| value).unwrap(),
            snapshot.len()
        );
        LINEARIZATIONS.set(0);
        assert_eq!(compute_class_precedence_list(b).unwrap(), expected);
        assert_eq!(LINEARIZATIONS.get(), 1);
    }
}
