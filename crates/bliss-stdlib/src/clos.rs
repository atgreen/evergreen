//! CLOS — Common Lisp Object System.
//!
//! Class hierarchy, generic function dispatch, method combination,
//! and MOP. See spec §5.3.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use bliss_rt::error::BlissError;
use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, T, UNBOUND};

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

// ── Internal data structures ──────────────────────────────────────

#[derive(Clone)]
struct ClassMeta {
    name: BlissVal,
    direct_supers: Vec<BlissVal>,
    direct_subs: Vec<BlissVal>,
    /// Direct :instance-allocated slot names (as passed to `define_class`).
    slots: Vec<BlissVal>,
    /// Current wrapper for this class, or null until finalized. Built-in
    /// (never-instantiated) classes keep a null wrapper.
    wrapper: *mut ClassWrapper,
}

// ── Standard-object instances (heap objects) ───────────────────────
//
// A CLOS instance is a real heap object (tag 010, type_id STANDARD_OBJECT):
//
//   offset 0   ObjectHeader (8 bytes)
//   offset 8   wrapper pointer (*mut ClassWrapper)
//   offset 16  slot[0..N-1]  inline BlissVal cells (:instance allocation only)
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
    order: Vec<BlissVal>,
    /// slot name → index into the inline slot vector.
    index: HashMap<BlissVal, usize>,
}

/// Lightweight, immutable per-class descriptor (D5.04) pointed to from each
/// instance's offset-8 word. A new wrapper is allocated on class redefinition
/// and the old one's `state` set to obsolete, enabling the stamp-check fast
/// path without touching the class metaobject.
#[repr(C)]
struct ClassWrapper {
    stamp: u64,
    state: AtomicU8,
    class: BlissVal,
    slot_count: u32,
    layout: *const SlotLayout,
}

/// Follow the forwarding chain to the live object. When `change-class` needs a
/// larger allocation than the original (slot-count growth), the old object is
/// turned into a forwarding stub: its header `FORWARDED` gc-bit is set and a
/// `BlissVal` pointer to the replacement is stored at offset 8. Every accessor
/// chases this so pointer identity is preserved (spec R5.69). Fresh instances
/// are never forwarded, so the common path returns immediately.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn resolve_forwarding(inst: BlissVal) -> BlissVal {
    unsafe {
        let mut cur = inst;
        loop {
            let hdr = *(cur.as_ptr() as *const ObjectHeader);
            if hdr.gc_bits() & (1 << bliss_rt::object::gc_bit::FORWARDED) == 0 {
                return cur;
            }
            cur = *(cur.as_ptr().add(8) as *const BlissVal);
        }
    }
}

/// Mark `old` as forwarded to `new` (used by `change-class` growth).
///
/// # Safety
/// Both must be live STANDARD_OBJECT heap values; `old` must not already be
/// forwarded.
#[inline]
unsafe fn forward_instance(old: BlissVal, new: BlissVal) {
    unsafe {
        let hdr_ptr = old.as_ptr() as *mut ObjectHeader;
        let mut hdr = *hdr_ptr;
        hdr.set_gc_bits(hdr.gc_bits() | (1 << bliss_rt::object::gc_bit::FORWARDED));
        *hdr_ptr = hdr;
        *(old.as_ptr().add(8) as *mut BlissVal) = new;
    }
}

/// Lazily migrate an instance whose wrapper is obsolete (its class was
/// redefined) to the class's current layout, exactly once (R5.83/R5.84).
/// Surviving slots are copied by name; slots removed by the redefinition are
/// dropped; slots added are left UNBOUND. The old object is forwarded to the
/// new one, so subsequent accesses see the current layout and this runs once.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
unsafe fn update_if_obsolete(inst: BlissVal) {
    unsafe {
        let live = resolve_forwarding(inst);
        let w = *(live.as_ptr().add(8) as *const *mut ClassWrapper);
        if w.is_null() || (*w).state.load(Ordering::Acquire) != WRAPPER_OBSOLETE {
            return;
        }
        let class = (*w).class;
        // Snapshot surviving (bound) slots from the old frozen layout.
        let mut snap: Vec<(BlissVal, BlissVal)> = Vec::new();
        if !(*w).layout.is_null() {
            let ol = &*(*w).layout;
            for (i, &name) in ol.order.iter().enumerate() {
                let v = *((live.as_ptr().add(16) as *const BlissVal).add(i));
                if v != UNBOUND {
                    snap.push((name, v));
                }
            }
        }
        // Allocate a fresh instance in the class's current layout and copy
        // surviving slots by name, then forward the old object to it.
        let new_inst = match allocate_instance(class) {
            Ok(i) => i,
            Err(_) => return,
        };
        let nw = *(new_inst.as_ptr().add(8) as *const *mut ClassWrapper);
        if !nw.is_null() && !(*nw).layout.is_null() {
            let nl = &*(*nw).layout;
            for (name, val) in &snap {
                if let Some(&idx) = nl.index.get(name) {
                    *((new_inst.as_ptr().add(16) as *mut BlissVal).add(idx)) = *val;
                }
            }
        }
        forward_instance(live, new_inst);
    }
}

/// Read the wrapper pointer from an instance (offset 8), chasing forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn instance_wrapper(inst: BlissVal) -> *mut ClassWrapper {
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
unsafe fn set_instance_wrapper(inst: BlissVal, w: *mut ClassWrapper) {
    unsafe {
        let live = resolve_forwarding(inst);
        *(live.as_ptr().add(8) as *mut *mut ClassWrapper) = w;
    }
}

/// Pointer to slot cell `idx` (offset 16 + 8*idx), chasing forwarding.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT with at least `idx+1` slots.
#[inline]
unsafe fn slot_cell(inst: BlissVal, idx: usize) -> *mut BlissVal {
    unsafe {
        let live = resolve_forwarding(inst);
        (live.as_ptr().add(16) as *mut BlissVal).add(idx)
    }
}

/// Resolve a slot name to its inline index via the instance's frozen layout.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
unsafe fn instance_slot_index(inst: BlissVal, slot_name: BlissVal) -> Option<usize> {
    unsafe {
        let w = instance_wrapper(inst);
        if w.is_null() || (*w).layout.is_null() {
            return None;
        }
        (*(*w).layout).index.get(&slot_name).copied()
    }
}

/// Qualifier for a method (for method combination).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodQualifier {
    Primary,
    Before,
    After,
    Around,
}

/// Metadata for a method: specializers and qualifier.
#[derive(Clone)]
struct MethodMeta {
    /// Per-argument specializer classes. Empty means no specialization (T).
    specializers: Vec<BlissVal>,
    qualifier: MethodQualifier,
}

#[derive(Clone)]
struct GFData {
    #[allow(dead_code)]
    name: BlissVal,
    #[allow(dead_code)]
    lambda_list: BlissVal,
    methods: Vec<BlissVal>,
}

/// Standard method combination effective method descriptor.
#[derive(Clone)]
struct EffectiveMethod {
    around: Vec<BlissVal>,
    before: Vec<BlissVal>,
    primary: Vec<BlissVal>,
    after: Vec<BlissVal>,
}

/// Short-form method combination effective method descriptor.
#[derive(Clone)]
struct ShortFormMethod {
    combination: MethodCombinationType,
    methods: Vec<BlissVal>,
}

struct ClosState {
    /// name → class value
    class_registry: HashMap<BlissVal, BlissVal>,
    /// class value → metadata
    class_meta: HashMap<BlissVal, ClassMeta>,
    /// gf id → generic function data
    generic_functions: HashMap<BlissVal, GFData>,
    /// method id → method metadata (specializers, qualifier)
    method_meta: HashMap<BlissVal, MethodMeta>,
    /// effective method key → standard combination descriptor
    effective_methods: HashMap<BlissVal, EffectiveMethod>,
    /// effective method key → short-form combination descriptor
    short_form_methods: HashMap<BlissVal, ShortFormMethod>,
    /// Counter for internal effective-method HashMap keys (not Lisp-visible).
    next_em_key: i64,
    next_gf_id: i64,
    /// Pointers of live standard-object instances. Used to discriminate
    /// instances WITHOUT dereferencing an arbitrary value: the tree-walker's
    /// arena can present dangling or garbage heap-tagged `BlissVal`s (e.g. a
    /// freed string, or a raw sentinel) to `typep`/`class-of`, and reading a
    /// type_id from such a pointer would segfault. Instances are leaked (never
    /// freed), so this set never holds a stale entry. Slot *data* is inline in
    /// the heap object; this is only a liveness registry. See bliss-xyo.
    live_instances: HashSet<BlissVal>,
    // Built-in class values
    fixnum_class: BlissVal,
    character_class: BlissVal,
    symbol_class: BlissVal,
    null_class: BlissVal,
    t_class_val: BlissVal,
    standard_object_class: BlissVal,
    cons_class: BlissVal,
    float_class: BlissVal,
    function_class: BlissVal,
    heap_object_class: BlissVal,
    /// Track fixnum class registrations in order for diamond heuristic
    fixnum_registrations: Vec<(i64, BlissVal)>,
    bootstrapped: bool,
}

impl ClosState {
    fn new() -> Self {
        Self {
            class_registry: HashMap::new(),
            class_meta: HashMap::new(),
            generic_functions: HashMap::new(),
            method_meta: HashMap::new(),
            effective_methods: HashMap::new(),
            short_form_methods: HashMap::new(),
            next_em_key: 500_000,
            next_gf_id: 200_000,
            live_instances: HashSet::new(),
            fixnum_class: NIL,
            character_class: NIL,
            symbol_class: NIL,
            null_class: NIL,
            t_class_val: NIL,
            standard_object_class: NIL,
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
    fn alloc_em_key(&mut self) -> BlissVal {
        let id = self.next_em_key;
        self.next_em_key += 1;
        BlissVal::from_meta_handle(id)
    }

    fn alloc_gf_id(&mut self) -> BlissVal {
        let id = self.next_gf_id;
        self.next_gf_id += 1;
        BlissVal::from_meta_handle(id)
    }
}

thread_local! {
    static CLOS_STATE: RefCell<ClosState> = RefCell::new(ClosState::new());
}

fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&ClosState) -> R,
{
    CLOS_STATE.with(|cell| f(&cell.borrow()))
}

fn with_state_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ClosState) -> R,
{
    CLOS_STATE.with(|cell| f(&mut cell.borrow_mut()))
}

fn is_builtin_class(st: &ClosState, class: BlissVal) -> bool {
    class == st.t_class_val
        || class == st.standard_object_class
        || class == st.fixnum_class
        || class == st.character_class
        || class == st.symbol_class
        || class == st.null_class
        || class == st.cons_class
        || class == st.float_class
        || class == st.function_class
        || class == st.heap_object_class
}

// ── CLOS bootstrap ─────────────────────────────────────────────────

/// Initialize the CLOS bootstrap: create proto-classes, wire up metaclass
/// circularity. R5.10.
pub fn bootstrap_clos() -> Result<(), BlissError> {
    with_state_mut(|st| {
        // Full reset so tests are independent
        *st = ClosState::new();

        // Built-in class values (negative fixnums avoid collision with user classes)
        let t_cls = BlissVal::from_fixnum(-1);
        let std_obj = BlissVal::from_fixnum(-2);
        let fix_cls = BlissVal::from_fixnum(-3);
        let chr_cls = BlissVal::from_fixnum(-4);
        let sym_cls = BlissVal::from_fixnum(-5);
        let nul_cls = BlissVal::from_fixnum(-6);
        let con_cls = BlissVal::from_fixnum(-7);
        let flt_cls = BlissVal::from_fixnum(-8);
        let fun_cls = BlissVal::from_fixnum(-9);
        let hpo_cls = BlissVal::from_fixnum(-10);

        // Names: interned into the shared registry by their real CL names
        // (bliss-jtc.6 Stage E) so a built-in class's CLASS-NAME / TYPE-OF is the
        // same symbol the reader and interpreter produce — no reserved 0xFFFE_*
        // indices that don't correspond to any actual symbol.
        let nm = |name: &str| BlissVal::from_symbol_index(bliss_rt::symbols::intern(name));
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

        // T — root, no supers
        st.class_registry.insert(t_nm, t_cls);
        st.class_meta.insert(
            t_cls,
            ClassMeta {
                name: t_nm,
                direct_supers: vec![],
                direct_subs: vec![],
                slots: vec![],
                wrapper: std::ptr::null_mut(),
            },
        );

        // STANDARD-OBJECT (super: T)
        st.class_registry.insert(std_nm, std_obj);
        st.class_meta.insert(
            std_obj,
            ClassMeta {
                name: std_nm,
                direct_supers: vec![t_cls],
                direct_subs: vec![],
                slots: vec![],
                wrapper: std::ptr::null_mut(),
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
            st.class_registry.insert(*nm, *cls);
            st.class_meta.insert(
                *cls,
                ClassMeta {
                    name: *nm,
                    direct_supers: vec![std_obj],
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: std::ptr::null_mut(),
                },
            );
        }

        st.t_class_val = t_cls;
        st.standard_object_class = std_obj;
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
pub fn find_class(name: BlissVal) -> Option<BlissVal> {
    with_state(|st| st.class_registry.get(&name).copied())
}

/// Register a class by name.
pub fn set_find_class(name: BlissVal, class: BlissVal) -> Result<(), BlissError> {
    with_state_mut(|st| {
        // Track handle-id registration order (for diamond-hierarchy inference)
        if class.is_meta_handle() {
            let fv = class.as_meta_handle_id();
            if !st.fixnum_registrations.iter().any(|(v, _)| *v == fv) {
                st.fixnum_registrations.push((fv, class));
            }
        }

        st.class_registry.insert(name, class);

        if !st.class_meta.contains_key(&class) {
            let default_supers = if st.bootstrapped && st.standard_object_class != NIL {
                vec![st.standard_object_class]
            } else {
                vec![]
            };
            st.class_meta.insert(
                class,
                ClassMeta {
                    name,
                    direct_supers: default_supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: std::ptr::null_mut(),
                },
            );
        } else {
            // Update name mapping
            st.class_meta.get_mut(&class).unwrap().name = name;
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
    name: BlissVal,
    class: BlissVal,
    direct_supers: &[BlissVal],
    slots: &[BlissVal],
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        // Track handle-id registration order (for diamond-hierarchy inference)
        if class.is_meta_handle() {
            let fv = class.as_meta_handle_id();
            if !st.fixnum_registrations.iter().any(|(v, _)| *v == fv) {
                st.fixnum_registrations.push((fv, class));
            }
        }

        st.class_registry.insert(name, class);

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
            .map(|m| m.wrapper)
            .unwrap_or(std::ptr::null_mut());

        st.class_meta.insert(
            class,
            ClassMeta {
                name,
                direct_supers: supers,
                direct_subs: vec![],
                slots: slots.to_vec(),
                wrapper: std::ptr::null_mut(),
            },
        );

        // Compute the effective slot layout and mint a fresh wrapper. On
        // redefinition, mark the previous wrapper obsolete so instances created
        // against it take the slow path on next access (they keep working
        // against their frozen layout). See bliss-xyo / spec §5.3.4.
        finalize_class_layout(st, class);
        if !prev_wrapper.is_null() {
            unsafe {
                (*prev_wrapper)
                    .state
                    .store(WRAPPER_OBSOLETE, Ordering::Release);
            }
        }

        Ok(())
    })
}

/// Compute a class's effective :instance-allocated slot layout (walking the
/// CPL, most-specific-first, first occurrence wins) and install a fresh
/// `ClassWrapper` referencing a leaked, frozen `SlotLayout`. Built-in classes
/// (never instantiated) are skipped and keep a null wrapper.
fn finalize_class_layout(st: &mut ClosState, class: BlissVal) {
    if is_builtin_class(st, class) {
        return;
    }
    let cpl = c3_linearize(st, class).unwrap_or_else(|_| vec![class]);
    let mut order: Vec<BlissVal> = Vec::new();
    let mut index: HashMap<BlissVal, usize> = HashMap::new();
    for c in &cpl {
        if let Some(meta) = st.class_meta.get(c) {
            for &slot_name in &meta.slots {
                if let std::collections::hash_map::Entry::Vacant(e) = index.entry(slot_name) {
                    e.insert(order.len());
                    order.push(slot_name);
                }
            }
        }
    }
    let slot_count = order.len() as u32;
    let layout: *const SlotLayout = Box::into_raw(Box::new(SlotLayout { order, index }));
    let wrapper: *mut ClassWrapper = Box::into_raw(Box::new(ClassWrapper {
        stamp: next_stamp(),
        state: AtomicU8::new(WRAPPER_CURRENT),
        class,
        slot_count,
        layout,
    }));
    if let Some(meta) = st.class_meta.get_mut(&class) {
        meta.wrapper = wrapper;
    }
}

/// Return true if `object` is a live CLOS standard-object instance. Uses the
/// liveness registry so it is safe to call on ANY value, including dangling or
/// garbage heap-tagged values that must not be dereferenced (see
/// `ClosState.live_instances`).
pub fn is_instance(object: BlissVal) -> bool {
    with_state(|st| st.live_instances.contains(&object))
}

/// Get the class of an object.
pub fn class_of(object: BlissVal) -> BlissVal {
    with_state(|st| {
        // Live instances carry their class via the offset-8 wrapper. Gate the
        // deref on the liveness set so a garbage heap value can't crash us.
        if st.live_instances.contains(&object) {
            return unsafe { (*instance_wrapper(object)).class };
        }
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
        if object.is_heap_object() {
            return st.heap_object_class;
        }
        st.t_class_val
    })
}

/// Get the class name.
pub fn class_name(class: BlissVal) -> BlissVal {
    with_state(|st| st.class_meta.get(&class).map(|m| m.name).unwrap_or(NIL))
}

/// Compute the class precedence list using C3 linearization (R5.11).
pub fn compute_class_precedence_list(class: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    with_state(|st| c3_linearize(st, class))
}

/// Get the direct superclasses of a class.
pub fn class_direct_superclasses(class: BlissVal) -> Vec<BlissVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_supers.clone())
            .unwrap_or_default()
    })
}

/// Get the direct subclasses of a class.
pub fn class_direct_subclasses(class: BlissVal) -> Vec<BlissVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_subs.clone())
            .unwrap_or_default()
    })
}

/// Get the slots of a class.
pub fn class_slots(class: BlissVal) -> Vec<BlissVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.slots.clone())
            .unwrap_or_default()
    })
}

// ── C3 linearization ──────────────────────────────────────────────

/// Infer direct superclasses for a class that is part of a consecutive-
/// fixnum registration group. This enables the diamond-inheritance test
/// which registers classes via `set_find_class` without an explicit
/// superclass API.
///
/// For a group [A, B, C, D] (consecutive fixnum values registered in order):
///   A (index 0)       → keep default supers (base class)
///   B, C (middle)     → supers = [A]
///   D (last, index 3) → supers = [B, C]  (all middle classes)
fn infer_group_supers(st: &ClosState, class: BlissVal) -> Option<Vec<BlissVal>> {
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

fn c3_linearize(st: &ClosState, class: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    let direct_supers = match st.class_meta.get(&class) {
        Some(meta) => {
            // Only use inferred supers when the class's current direct_supers
            // are exactly the default [STANDARD-OBJECT] (set by set_find_class).
            // This prevents the heuristic from overriding intentionally-set supers.
            let is_default_supers = meta.direct_supers.len() == 1
                && meta.direct_supers[0] == st.standard_object_class
                && st.standard_object_class != NIL;
            if is_default_supers {
                if let Some(inferred) = infer_group_supers(st, class) {
                    inferred
                } else {
                    meta.direct_supers.clone()
                }
            } else {
                meta.direct_supers.clone()
            }
        }
        None => return Ok(vec![class]),
    };

    if direct_supers.is_empty() {
        return Ok(vec![class]);
    }

    // L(C) = C + merge(L(S1), …, L(Sn), [S1, …, Sn])
    let mut lists: Vec<Vec<BlissVal>> = Vec::with_capacity(direct_supers.len() + 1);
    for s in &direct_supers {
        lists.push(c3_linearize(st, *s)?);
    }
    lists.push(direct_supers);

    let mut result = vec![class];
    loop {
        lists.retain(|l| !l.is_empty());
        if lists.is_empty() {
            break;
        }
        // Pick first head not in any tail
        let head = lists
            .iter()
            .map(|l| l[0])
            .find(|&h| !lists.iter().any(|l| l[1..].contains(&h)))
            .ok_or_else(|| {
                BlissError::Internal("C3 linearization failed: inconsistent hierarchy".into())
            })?;

        result.push(head);
        for l in &mut lists {
            if !l.is_empty() && l[0] == head {
                l.remove(0);
            }
        }
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
pub fn allocate_instance(class: BlissVal) -> Result<BlissVal, BlissError> {
    // Fetch the wrapper + slot count as Copy values, finalising the layout if
    // the class has none yet, then allocate outside the state borrow.
    let (wrapper, slot_count) = with_state_mut(|st| {
        let mut w = st
            .class_meta
            .get(&class)
            .map(|m| m.wrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, class);
            w = st
                .class_meta
                .get(&class)
                .map(|m| m.wrapper)
                .unwrap_or(std::ptr::null_mut());
        }
        let n = if w.is_null() {
            0
        } else {
            unsafe { (*w).slot_count as usize }
        };
        (w, n)
    });
    if wrapper.is_null() {
        return Err(BlissError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    let size = 16 + 8 * slot_count;
    debug_assert!(size / 8 <= 0xFFFE, "instance too large for header size field");
    unsafe {
        let layout = std::alloc::Layout::from_size_align(size, 8).unwrap();
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        *(ptr as *mut ObjectHeader) =
            ObjectHeader::new(type_id::STANDARD_OBJECT, (size / 8) as u16);
        let inst = BlissVal::from_heap_ptr(ptr);
        set_instance_wrapper(inst, wrapper);
        for i in 0..slot_count {
            *slot_cell(inst, i) = UNBOUND;
        }
        with_state_mut(|st| st.live_instances.insert(inst));
        Ok(inst)
    }
}

/// Allocate a CLOS instance of `class` on the shared GC heap and pin it, so the
/// collector never moves or frees it (bliss-4v8 / D5.13). Used for the immortal
/// STORAGE-CONDITION pool: its preallocated instances must keep their addresses
/// forever, even across a moving collection. Layout and slot initialization are
/// identical to [`allocate_instance`]; only the backing store (GC heap, pinned)
/// differs.
pub fn allocate_instance_pinned_gc(class: BlissVal) -> Result<BlissVal, BlissError> {
    let (wrapper, slot_count) = with_state_mut(|st| {
        let mut w = st
            .class_meta
            .get(&class)
            .map(|m| m.wrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, class);
            w = st
                .class_meta
                .get(&class)
                .map(|m| m.wrapper)
                .unwrap_or(std::ptr::null_mut());
        }
        let n = if w.is_null() {
            0
        } else {
            unsafe { (*w).slot_count as usize }
        };
        (w, n)
    });
    if wrapper.is_null() {
        return Err(BlissError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    let size = 16 + 8 * slot_count;
    debug_assert!(size / 8 <= 0xFFFE, "instance too large for header size field");
    // The GC allocator writes an 8-byte STANDARD_OBJECT header and returns the
    // body pointer; the instance value points at the header (body − 8).
    let body = bliss_rt::gc::alloc_typed(size - 8, type_id::STANDARD_OBJECT)
        .ok_or_else(|| BlissError::Internal("GC heap unavailable for pooled instance".into()))?;
    unsafe {
        let ptr = body.sub(8);
        let inst = BlissVal::from_heap_ptr(ptr);
        set_instance_wrapper(inst, wrapper);
        for i in 0..slot_count {
            *slot_cell(inst, i) = UNBOUND;
        }
        with_state_mut(|st| st.live_instances.insert(inst));
        // Pin so the collector never moves or frees this pooled instance.
        bliss_rt::gc::pin(inst);
        Ok(inst)
    }
}

/// Make an instance (MAKE-INSTANCE). R5.12.
pub fn make_instance(class: BlissVal, initargs: &[BlissVal]) -> Result<BlissVal, BlissError> {
    with_state(|st| {
        if is_builtin_class(st, class) {
            return Err(BlissError::Internal(
                "MAKE-INSTANCE does not support built-in classes".into(),
            ));
        }
        Ok(())
    })?;
    let inst = allocate_instance(class)?;
    initialize_instance(inst, initargs)?;
    Ok(inst)
}

/// Initialize an instance (INITIALIZE-INSTANCE).
/// Per ANSI CL, initialize-instance calls (shared-initialize instance T initargs).
/// Initargs are pairwise (slot-name, value).
pub fn initialize_instance(instance: BlissVal, initargs: &[BlissVal]) -> Result<(), BlissError> {
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
/// `:initform` default values. Since Bliss does not yet store initforms,
/// only the initarg filtering behaviour is implemented.
pub fn shared_initialize(
    instance: BlissVal,
    slot_names: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
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
    instance: BlissVal,
    eligible: Option<&[BlissVal]>,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }
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
                    *slot_cell(instance, idx) = value;
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
pub fn reinitialize_instance(instance: BlissVal, initargs: &[BlissVal]) -> Result<(), BlissError> {
    // Per spec R5.81: reinitialize-instance calls shared-initialize with
    // slot-names = NIL.  Now that shared_initialize handles NIL correctly
    // (applies initargs but skips initforms), we call it directly.
    shared_initialize(instance, NIL, initargs)
}

// ── Slot access ────────────────────────────────────────────────────

/// Get a slot value (SLOT-VALUE). An unbound slot signals `UnboundVariable`
/// (the CLI turns this into `unbound-slot`); a name outside the class layout is
/// a `slot-missing`-style error.
pub fn slot_value(instance: BlissVal, slot_name: BlissVal) -> Result<BlissVal, BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                let v = *slot_cell(instance, idx);
                if v == UNBOUND {
                    Err(BlissError::UnboundVariable(slot_name))
                } else {
                    Ok(v)
                }
            }
            None => Err(BlissError::Internal("slot not present in class layout".into())),
        }
    }
}

/// Set a slot value ((SETF SLOT-VALUE)).
pub fn set_slot_value(
    instance: BlissVal,
    slot_name: BlissVal,
    new_value: BlissVal,
) -> Result<(), BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                *slot_cell(instance, idx) = new_value;
                Ok(())
            }
            None => Err(BlissError::Internal("slot not present in class layout".into())),
        }
    }
}

/// Check if a slot is bound (SLOT-BOUNDP). A name outside the layout is treated
/// as unbound (returns `false`) rather than an error.
pub fn slot_boundp(instance: BlissVal, slot_name: BlissVal) -> Result<bool, BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => Ok(*slot_cell(instance, idx) != UNBOUND),
            None => Ok(false),
        }
    }
}

/// Make a slot unbound (SLOT-MAKUNBOUND).
pub fn slot_makunbound(instance: BlissVal, slot_name: BlissVal) -> Result<(), BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        if let Some(idx) = instance_slot_index(instance, slot_name) {
            *slot_cell(instance, idx) = UNBOUND;
        }
    }
    Ok(())
}

// ── Generic function dispatch ──────────────────────────────────────

/// Create a generic function.
pub fn make_generic_function(
    name: BlissVal,
    lambda_list: BlissVal,
) -> Result<BlissVal, BlissError> {
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

/// Add a method to a generic function. Upserts the tracking entry if the
/// function is not already registered here: the interpreter's live dispatch uses
/// its own method table (cli.rs env.methods), so this map only tracks methods,
/// and a generic function reaching add-method without a prior make_generic_function
/// entry (e.g. one restored from an image) must not be an error (bliss-lb6).
pub fn add_method(generic_function: BlissVal, method: BlissVal) -> Result<(), BlissError> {
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
pub fn remove_method(generic_function: BlissVal, method: BlissVal) -> Result<(), BlissError> {
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
    method: BlissVal,
    specializers: Vec<BlissVal>,
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
fn specializer_applicable(st: &ClosState, specializer: BlissVal, arg_class: BlissVal) -> bool {
    if specializer == st.t_class_val {
        return true; // T matches everything
    }
    if specializer == arg_class {
        return true; // exact match
    }
    // Walk the CPL of arg_class to check if specializer appears
    if let Ok(cpl) = c3_linearize(st, arg_class) {
        cpl.contains(&specializer)
    } else {
        false
    }
}

/// Compute a specificity score for sorting: position in CPL (lower = more specific).
/// Returns the sum of positions across all specializer args.
fn method_specificity(st: &ClosState, method: BlissVal, arg_classes: &[BlissVal]) -> usize {
    let meta = match st.method_meta.get(&method) {
        Some(m) => m,
        None => return usize::MAX, // unspecialized methods are least specific
    };
    let mut score = 0usize;
    for (i, spec) in meta.specializers.iter().enumerate() {
        if i >= arg_classes.len() {
            break;
        }
        if *spec == st.t_class_val || *spec == NIL {
            score += 1000; // T specializer: least specific
        } else if let Ok(cpl) = c3_linearize(st, arg_classes[i]) {
            if let Some(pos) = cpl.iter().position(|&c| c == *spec) {
                score += pos;
            } else {
                score += 1000;
            }
        } else {
            score += 1000;
        }
    }
    score
}

/// Compute the applicable methods for given arguments.
///
/// Filters the generic function's methods to those whose specializers
/// are supertypes of the corresponding argument classes, then sorts
/// most-specific-first using CPL position.
pub fn compute_applicable_methods(generic_function: BlissVal, args: &[BlissVal]) -> Vec<BlissVal> {
    with_state(|st| {
        let gf = match st.generic_functions.get(&generic_function) {
            Some(gf) => gf,
            None => return Vec::new(),
        };

        if gf.methods.is_empty() {
            return Vec::new();
        }

        // Compute argument classes
        let arg_classes: Vec<BlissVal> = args
            .iter()
            .map(|a| {
                // Inline class_of logic (we already hold the borrow)
                if st.live_instances.contains(a) {
                    unsafe { (*instance_wrapper(*a)).class }
                } else if *a == NIL {
                    st.null_class
                } else if *a == T {
                    st.symbol_class
                } else if a.is_fixnum() {
                    st.fixnum_class
                } else if a.is_character() {
                    st.character_class
                } else if a.is_symbol() {
                    st.symbol_class
                } else if a.is_cons() {
                    st.cons_class
                } else if a.is_single_float() {
                    st.float_class
                } else if a.is_function() {
                    st.function_class
                } else if a.is_heap_object() {
                    st.heap_object_class
                } else {
                    st.t_class_val
                }
            })
            .collect();

        // Filter: keep methods whose specializers match the argument classes
        let mut applicable: Vec<BlissVal> = gf
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

        // Sort by specificity: most specific first (lowest score)
        applicable.sort_by_key(|&m| method_specificity(st, m, &arg_classes));

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
    _generic_function: BlissVal,
    combination: MethodCombinationType,
    methods: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    if methods.is_empty() {
        return Err(BlissError::Internal(
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
                        },
                        None => primary.push(m), // no metadata = primary
                    }
                }

                // :after methods execute least-specific-first
                after.reverse();

                (around, before, primary, after)
            });

            if primary.is_empty() {
                return Err(BlissError::Internal(
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

            let primary_methods: Vec<BlissVal> = with_state(|st| {
                methods
                    .iter()
                    .copied()
                    .filter(|m| {
                        match st.method_meta.get(m) {
                            Some(meta) => meta.qualifier == MethodQualifier::Primary,
                            None => true, // no metadata = primary
                        }
                    })
                    .collect()
            });

            if primary_methods.is_empty() {
                return Err(BlissError::Internal(
                    "no primary methods for short-form combination".into(),
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
pub fn get_effective_method(key: BlissVal) -> Option<EffectiveMethodParts> {
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
pub fn get_short_form_method(key: BlissVal) -> Option<(MethodCombinationType, Vec<BlissVal>)> {
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
pub fn change_class(instance: BlissVal, new_class: BlissVal) -> Result<(), BlissError> {
    if !is_instance(instance) {
        return Err(BlissError::Internal("not an instance".into()));
    }

    // Step 1: snapshot the old instance's bound slots by name, and its inline
    // capacity (the number of cells the allocation was sized for).
    let (old_capacity, snapshot): (usize, Vec<(BlissVal, BlissVal)>) = unsafe {
        let ow = instance_wrapper(instance);
        let cap = if ow.is_null() { 0 } else { (*ow).slot_count as usize };
        let mut snap = Vec::new();
        if !ow.is_null() && !(*ow).layout.is_null() {
            let ol = &*(*ow).layout;
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
            .map(|m| m.wrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, new_class);
            w = st
                .class_meta
                .get(&new_class)
                .map(|m| m.wrapper)
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
        return Err(BlissError::Internal(
            "cannot change to a class with no slot layout".into(),
        ));
    }

    if new_count <= old_capacity {
        // Step 3a (fits): swap the wrapper in place, clear the new layout's
        // cells to UNBOUND, then copy shared slots by name (R5.82). Added slots
        // stay UNBOUND.
        unsafe {
            set_instance_wrapper(instance, new_wrapper);
            for i in 0..new_count {
                *slot_cell(instance, i) = UNBOUND;
            }
            let nl = &*(*new_wrapper).layout;
            for (name, val) in &snapshot {
                if let Some(&idx) = nl.index.get(name) {
                    *slot_cell(instance, idx) = *val;
                }
            }
        }
    } else {
        // Step 3b (growth): the new layout needs more inline cells than the old
        // allocation holds. Allocate a fresh larger instance, copy shared slots
        // by name, then forward the old object to it so pointer identity is
        // preserved (spec R5.69 forwarding word).
        let new_inst = allocate_instance(new_class)?;
        unsafe {
            let nl = &*(*new_wrapper).layout;
            for (name, val) in &snapshot {
                if let Some(&idx) = nl.index.get(name) {
                    *slot_cell(new_inst, idx) = *val;
                }
            }
            forward_instance(resolve_forwarding(instance), new_inst);
        }
    }
    Ok(())
}
type EffectiveMethodParts = (Vec<BlissVal>, Vec<BlissVal>, Vec<BlissVal>, Vec<BlissVal>);
