//! CLOS — Common Lisp Object System.
//!
//! Class hierarchy, generic function dispatch, method combination,
//! and MOP. See spec §5.3.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use torcl_rt::error::TorclError;
use torcl_rt::lock_order::{LockLevel, OrderedMutex};
use torcl_rt::object::type_id;
use torcl_rt::value::{NIL, T, TorclVal, UNBOUND};

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
    name: TorclVal,
    direct_supers: Vec<TorclVal>,
    direct_subs: Vec<TorclVal>,
    /// Direct :instance-allocated slot names (as passed to `define_class`).
    slots: Vec<TorclVal>,
    /// Frozen, process-lifetime descriptor shared with instances. Built-in
    /// (never-instantiated) classes have no wrapper.
    wrapper: Option<&'static ClassWrapper>,
}

// ── Standard-object instances (heap objects) ───────────────────────
//
// A CLOS instance is a real heap object (tag 010, type_id STANDARD_OBJECT):
//
//   offset 0   ObjectHeader (8 bytes)
//   offset 8   wrapper pointer (*mut ClassWrapper)
//   offset 16  slot[0..N-1]  inline TorclVal cells (:instance allocation only)
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
    order: Vec<TorclVal>,
    /// slot name → index into the inline slot vector.
    index: HashMap<TorclVal, usize>,
}

/// Lightweight, immutable per-class descriptor (D5.04) pointed to from each
/// instance's offset-8 word. A new wrapper is allocated on class redefinition
/// and the old one's `state` set to obsolete, enabling the stamp-check fast
/// path without touching the class metaobject.
#[repr(C)]
struct ClassWrapper {
    stamp: u64,
    state: AtomicU8,
    class: TorclVal,
    slot_count: u32,
    layout: &'static SlotLayout,
}

/// Follow the forwarding chain to the live object. When `change-class` needs a
/// larger allocation than the original (slot-count growth), the old object is
/// turned into a forwarding stub: its header `FORWARDED` gc-bit is set and a
/// `TorclVal` pointer to the replacement is stored at offset 8. Every accessor
/// chases this so pointer identity is preserved (spec R5.69). Fresh instances
/// are never forwarded, so the common path returns immediately.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
#[inline]
unsafe fn resolve_forwarding(inst: TorclVal) -> TorclVal {
    // Delegates to the collector's forwarding resolution: CHANGE-CLASS stubs
    // now use the SAME layout as evacuation forwarding (untagged new-body
    // address in the first payload word), so the GC's relocate pass rewrites
    // stub-holding slots itself and this lazy chase is only needed between a
    // migration and the next collection (bliss-334).
    torcl_rt::gc::resolve_forwarded(inst)
}

/// Mark `old` as forwarded to `new` (used by `change-class` growth).
///
/// # Safety
/// Both must be live STANDARD_OBJECT heap values; `old` must not already be
/// forwarded.
#[inline]
unsafe fn forward_instance(old: TorclVal, new: TorclVal) {
    // The collector's forwarding layout — NOT a tagged TorclVal in the payload
    // word. relocate_slot reads that word as an untagged body address; the old
    // CLOS-private tagged scheme made it compute a wild pointer whenever a GC
    // relocation pass encountered a slot still holding the stub (bliss-334).
    unsafe { torcl_rt::gc::forward_object_to(old, new) };
}

/// Lazily migrate an instance whose wrapper is obsolete (its class was
/// redefined) to the class's current layout, exactly once (R5.83/R5.84).
/// Surviving slots are copied by name; slots removed by the redefinition are
/// dropped; slots added are left UNBOUND. The old object is forwarded to the
/// new one, so subsequent accesses see the current layout and this runs once.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
unsafe fn update_if_obsolete(inst: TorclVal) {
    unsafe {
        let live = resolve_forwarding(inst);
        let w = *(live.as_ptr().add(8) as *const *mut ClassWrapper);
        if w.is_null() || (*w).state.load(Ordering::Acquire) != WRAPPER_OBSOLETE {
            return;
        }
        let class = (*w).class;
        // Snapshot surviving (bound) slots from the old frozen layout.
        let mut snap: Vec<(TorclVal, TorclVal)> = Vec::new();
        {
            let ol = (*w).layout;
            for (i, &name) in ol.order.iter().enumerate() {
                let v = *((live.as_ptr().add(16) as *const TorclVal).add(i));
                if v != UNBOUND {
                    snap.push((name, v));
                }
            }
        }
        // Allocate a fresh instance in the class's current layout and copy
        // surviving slots by name, then forward the old object to it.
        //
        // GC safety (bliss-334): allocate_instance can fire a relocating minor
        // GC, so both the OLD instance and the snapshotted slot values must be
        // rooted across it — an unrooted `live` would forward a stale address,
        // and unrooted snap values would be copied stale into the new layout.
        let mut live = live;
        torcl_rt::rooted_ref!(_live_root = &mut live);
        torcl_rt::rooted_ref!(_snap_root = &mut snap);
        let new_inst = match allocate_instance(class) {
            Ok(i) => i,
            Err(_) => return,
        };
        let nw = *(new_inst.as_ptr().add(8) as *const *mut ClassWrapper);
        if !nw.is_null() {
            let nl = (*nw).layout;
            for (name, val) in &snap {
                if let Some(&idx) = nl.index.get(name) {
                    *((new_inst.as_ptr().add(16) as *mut TorclVal).add(idx)) = *val;
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
unsafe fn instance_wrapper(inst: TorclVal) -> *mut ClassWrapper {
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
unsafe fn set_instance_wrapper(inst: TorclVal, w: *mut ClassWrapper) {
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
unsafe fn slot_cell(inst: TorclVal, idx: usize) -> *mut TorclVal {
    unsafe {
        let live = resolve_forwarding(inst);
        (live.as_ptr().add(16) as *mut TorclVal).add(idx)
    }
}

/// Resolve a slot name to its inline index via the instance's frozen layout.
///
/// # Safety
/// `inst` must be a live STANDARD_OBJECT heap value.
unsafe fn instance_slot_index(inst: TorclVal, slot_name: TorclVal) -> Option<usize> {
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
fn slot_name_index_fallback(layout: &SlotLayout, slot_name: TorclVal) -> Option<usize> {
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
fn bare_symbol_name(sym: TorclVal) -> Option<String> {
    // symbol_index() (not as_symbol_index) so the special NIL/T constants —
    // which is_symbol() accepts but which have no table index — return None
    // instead of panicking (seen via a NIL in a slot layout under GC stress).
    let key = torcl_rt::symbols::symbol_name(sym.symbol_index()?)?;
    let bare = torcl_rt::symbols::split_registry_key(&key)
        .map(|(_, n)| n)
        .unwrap_or(key.as_str());
    Some(bare.to_string())
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
    specializers: Vec<TorclVal>,
    qualifier: MethodQualifier,
}

#[derive(Clone)]
struct GFData {
    /// Read back by GENERIC_FUNCTION_NAME, which dispatch uses to find the
    /// generic's argument precedence order.
    name: TorclVal,
    #[allow(dead_code)]
    lambda_list: TorclVal,
    methods: Vec<TorclVal>,
}

/// Standard method combination effective method descriptor.
#[derive(Clone)]
struct EffectiveMethod {
    around: Vec<TorclVal>,
    before: Vec<TorclVal>,
    primary: Vec<TorclVal>,
    after: Vec<TorclVal>,
}

/// Short-form method combination effective method descriptor.
#[derive(Clone)]
struct ShortFormMethod {
    combination: MethodCombinationType,
    methods: Vec<TorclVal>,
}

struct ClosState {
    /// name → class value
    class_registry: HashMap<TorclVal, TorclVal>,
    /// bare class name (uppercase, package-stripped) → class value. A fallback
    /// for `find_class` when a class-name symbol's *identity* has drifted (e.g.
    /// re-interned by a package RECYCLE/rehome after the class was defined), so
    /// the symbol-keyed `class_registry` misses even though the class exists.
    class_by_name: HashMap<String, TorclVal>,
    /// class value → metadata
    class_meta: HashMap<TorclVal, ClassMeta>,
    /// Derived query results, discarded on every mutable state access and GC.
    /// Never serialized or treated as owning roots; metadata remains the source
    /// of truth, including bootstrap's registration-order superclass inference.
    cpl_cache: RefCell<HashMap<TorclVal, Vec<TorclVal>>>,
    /// gf id → generic function data
    generic_functions: HashMap<TorclVal, GFData>,
    /// method id → method metadata (specializers, qualifier)
    method_meta: HashMap<TorclVal, MethodMeta>,
    /// effective method key → standard combination descriptor
    effective_methods: HashMap<TorclVal, EffectiveMethod>,
    /// effective method key → short-form combination descriptor
    short_form_methods: HashMap<TorclVal, ShortFormMethod>,
    /// Counter for internal effective-method HashMap keys (not Lisp-visible).
    next_em_key: i64,
    next_gf_id: i64,
    /// Class values created by DEFSTRUCT. Their instances are STRUCTURE-OBJECTs
    /// (not STANDARD-OBJECTs) and EQUALP descends them / they print in #S(...)
    /// syntax — none of which applies to a plain DEFCLASS class (bliss-i1i9).
    structure_classes: HashSet<TorclVal>,
    // Built-in class values
    fixnum_class: TorclVal,
    character_class: TorclVal,
    symbol_class: TorclVal,
    null_class: TorclVal,
    t_class_val: TorclVal,
    standard_object_class: TorclVal,
    cons_class: TorclVal,
    float_class: TorclVal,
    function_class: TorclVal,
    heap_object_class: TorclVal,
    /// Track fixnum class registrations in order for diamond heuristic
    fixnum_registrations: Vec<(i64, TorclVal)>,
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
    fn alloc_em_key(&mut self) -> TorclVal {
        let id = self.next_em_key;
        self.next_em_key += 1;
        TorclVal::from_meta_handle(id)
    }

    fn alloc_gf_id(&mut self) -> TorclVal {
        let id = self.next_gf_id;
        self.next_gf_id += 1;
        TorclVal::from_meta_handle(id)
    }
}

// No Lisp allocation, evaluation, safepoint, or lower-level lock acquisition
// under this guard. The collector acquires it after stopping mutators, so it
// scans the same definitions regardless of which thread initiated collection.
static CLOS_STATE: LazyLock<OrderedMutex<ClosState>> = LazyLock::new(|| {
    OrderedMutex::new(LockLevel::GcWorld, 8, "CLOS definitions", ClosState::new())
});

fn scan_clos_state_roots(visit: &mut dyn FnMut(*mut TorclVal)) {
    visit_clos_roots(visit, true);
}

fn visit_clos_roots(visit: &mut dyn FnMut(*mut TorclVal), root_definitions: bool) {
    {
        let mut state = CLOS_STATE.lock().unwrap();
        // Discard derived values before relocation rather than retaining and
        // tracing a second copy of every cached class reference. Queries cannot
        // run concurrently while the collector holds this registry's lock.
        state.cpl_cache.get_mut().clear();
        // Registry keys are symbols or private meta-handles and never move.
        // Payloads can include reader-built lambda lists and other heap values.
        for value in state.class_registry.values_mut() {
            visit(value);
        }
        for value in state.class_by_name.values_mut() {
            visit(value);
        }
        for meta in state.class_meta.values_mut() {
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
            // Wrapper class ids and slot-layout names are the same immediate
            // meta-handles/symbols represented above, so they need no rewrite.
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
        visit(&mut state.cons_class);
        visit(&mut state.float_class);
        visit(&mut state.function_class);
        visit(&mut state.heap_object_class);
    }
}

/// Registry ownership edges for stopped-world delivery analysis. Keys are
/// stable meta-handles; payload values must not escape the heap snapshot.
pub struct DeliveryDefinitions {
    pub generics: Vec<(TorclVal, TorclVal)>,
    pub edges: HashMap<TorclVal, Vec<TorclVal>>,
}

pub fn delivery_root_scanner() -> torcl_rt::gc::RootScanner {
    scan_clos_state_roots
}

/// Keep class state rooted while generic and method ownership is traced by
/// the delivery graph. Call only under a nonallocating heap snapshot.
pub fn visit_delivery_roots(visit: &mut dyn FnMut(*mut TorclVal)) {
    visit_clos_roots(visit, false);
}

pub fn delivery_definitions() -> DeliveryDefinitions {
    let state = CLOS_STATE.lock().unwrap();
    let mut edges: HashMap<TorclVal, Vec<TorclVal>> = HashMap::new();
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
    DeliveryDefinitions { generics, edges }
}

/// Remove unreachable metadata before serializing the disposable delivery
/// world. Surviving handles and their allocation counters retain their IDs.
pub fn retain_delivery_definitions(live: &HashSet<u64>) {
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
    INSTALL.call_once(|| torcl_rt::gc::register_root_scanner(scan_clos_state_roots));
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

// ── Core-image serialization of CLOS state (torcl-x0f2.7a) ─────────────
//
// A core load skips bootstrap, so CLOS_STATE (class/GF/method registries) must
// ride the image or macro expansion / condition signaling in the loaded core
// dies (fresh Env → initialize_condition_runtime_support → class_of → null).
// Every TorclVal held here is either a heap value the snapshot already carries
// (class objects, lambda lists, specializers) or an immediate meta-handle /
// symbol — so records carry raw u64s remapped on restore, exactly like the
// macro/setf HostRegistries records. The field set mirrors
// `scan_clos_state_roots` (whatever the GC roots, the dump must capture).
// `ClassMeta.wrapper` (a raw `*mut ClassWrapper`) is NOT serialized: wrappers
// restore null and are lazily re-finalized from the restored `class_meta.slots`
// by `finalize_class_layout` on the next instance allocation. Pre-save
// INSTANCES therefore still hold dangling wrapper words after a load — that
// remap is torcl-x0f2.7b.

fn cs_put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn cs_put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn cs_put_val(out: &mut Vec<u8>, v: TorclVal) {
    cs_put_u64(out, v.to_raw());
}

fn cs_put_vals(out: &mut Vec<u8>, vs: &[TorclVal]) {
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

fn cs_get_val(data: &[u8], off: &mut usize, remap: &dyn Fn(u64) -> u64) -> Option<TorclVal> {
    cs_get_u64(data, off).map(|raw| TorclVal::from_raw(remap(raw)))
}

fn cs_get_vals(data: &[u8], off: &mut usize, remap: &dyn Fn(u64) -> u64) -> Option<Vec<TorclVal>> {
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
    }
}

fn u8_to_qualifier(b: u8) -> MethodQualifier {
    match b {
        1 => MethodQualifier::Before,
        2 => MethodQualifier::After,
        3 => MethodQualifier::Around,
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
/// stop-the-world full GC; only reads raw tagged words (no TorCL allocation),
/// so it is GC-safe.
pub fn serialize_clos_state() -> Vec<u8> {
    let mut out = with_state(|st| {
        let mut out = Vec::new();
        out.extend_from_slice(b"CLST");
        cs_put_u32(&mut out, 2); // format version
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
        let structs: Vec<TorclVal> = st.structure_classes.iter().copied().collect();
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
    // ── Wrappers (torcl-x0f2.7b) ──
    // Every restored STANDARD_OBJECT instance's offset-8 word is a raw
    // `*mut ClassWrapper` the loader must remap. Serialize each DISTINCT live
    // wrapper keyed by its save-time address: the classes' current wrappers,
    // plus any wrapper still referenced from a heap instance (covers obsolete
    // wrappers on stale instances after a class redefinition). Runs with no
    // heap lock held (image.rs calls the hook at top level), so walk_heap can
    // take it.
    let current: Vec<(TorclVal, usize)> = with_state(|st| {
        st.class_meta
            .iter()
            .filter_map(|(c, m)| m.wrapper.map(|w| (*c, w as *const ClassWrapper as usize)))
            .collect()
    });
    let mut addrs: Vec<usize> = current.iter().map(|(_, w)| *w).collect();
    let _ = torcl_rt::gc::walk_heap(|body, tid, _size| {
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
/// GC-safe: pure Rust parsing + map inserts, no TorCL allocation. Returns the
/// number of bytes consumed so the caller can parse blocks appended after this
/// one in the same section.
pub fn restore_clos_state(data: &[u8], remap: &dyn Fn(u64) -> u64) -> Result<usize, TorclError> {
    // The restore rewrites wrapper addresses, including preallocated reserves.
    crate::conditions::reset_storage_condition_pool();
    let bad = || TorclError::InvalidImage("CLOS state section: truncated".into());
    if data.len() < 8 || &data[..4] != b"CLST" {
        return Err(TorclError::InvalidImage(
            "CLOS state section: bad marker".into(),
        ));
    }
    let mut off = 4usize;
    let version = cs_get_u32(data, &mut off).ok_or_else(bad)?;
    if version != 2 {
        return Err(TorclError::InvalidImage(format!(
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
        st.class_meta.insert(
            class,
            ClassMeta {
                name,
                direct_supers,
                direct_subs,
                slots,
                wrapper: None,
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
    st.cons_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.float_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.function_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.heap_object_class = cs_get_val(data, &mut off, remap).ok_or_else(bad)?;
    st.bootstrapped = cs_get_u8(data, &mut off).ok_or_else(bad)? != 0;
    // ── Wrappers (torcl-x0f2.7b) ──
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
        let layout = Box::leak(Box::new(SlotLayout { order, index }));
        let wrapper: &'static ClassWrapper = Box::leak(Box::new(ClassWrapper {
            stamp,
            state: AtomicU8::new(state),
            class,
            slot_count,
            layout,
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
    with_state_mut(|state| *state = st);
    install_clos_state_root_scanner();
    // Rewrite every restored instance's wrapper word through the map. An
    // unmatched (impossible unless the image is inconsistent) or null word is
    // nulled so a later use fails the null check instead of deref'ing a stale
    // save-time address. Runs after the host hook regained control, so no heap
    // lock is held — walk_heap takes it.
    torcl_rt::gc::walk_heap(|body, tid, _size| {
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
        let mut bump = |v: TorclVal| {
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

fn is_builtin_class(st: &ClosState, class: TorclVal) -> bool {
    // Every built-in class handle is a NEGATIVE fixnum (user classes get positive
    // ids from next_stdlib_class_id, base 300_000), so the sign is the reliable
    // discriminator and covers the numeric-tower / sequence / array classes added
    // in bliss-qxfg without enumerating each. The explicit field checks below stay
    // as documentation of the core classes.
    if class.is_fixnum() && class.as_fixnum() < 0 {
        return true;
    }
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
/// Bootstrap the CLOS state only if it has not been bootstrapped yet, preserving
/// any classes already defined. `Env::new` calls this (not [`bootstrap_clos`])
/// because transient environments — e.g. the macro-expansion env built for every
/// macro/compiler-macro call — must NOT wipe the process-global class registry
/// mid-load. (Tests still call [`bootstrap_clos`] directly to force a fresh
/// state.)
pub fn ensure_clos_bootstrapped() -> Result<(), TorclError> {
    if with_state(|st| st.bootstrapped) {
        return Ok(());
    }
    initialize_clos(false)
}

/// Reset CLOS in a quiescent runtime (startup or an isolated test fixture).
/// Worker entry must use `ensure_clos_bootstrapped` instead.
pub fn bootstrap_clos() -> Result<(), TorclError> {
    crate::conditions::reset_storage_condition_pool();
    initialize_clos(true)
}

fn initialize_clos(reset: bool) -> Result<(), TorclError> {
    install_clos_state_root_scanner();

    // Built-in class values (negative fixnums avoid collision with user classes)
    let t_cls = TorclVal::from_fixnum(-1);
    let std_obj = TorclVal::from_fixnum(-2);
    let fix_cls = TorclVal::from_fixnum(-3);
    let chr_cls = TorclVal::from_fixnum(-4);
    let sym_cls = TorclVal::from_fixnum(-5);
    let nul_cls = TorclVal::from_fixnum(-6);
    let con_cls = TorclVal::from_fixnum(-7);
    let flt_cls = TorclVal::from_fixnum(-8);
    let fun_cls = TorclVal::from_fixnum(-9);
    let hpo_cls = TorclVal::from_fixnum(-10);
    // Additional CL built-in classes (bliss-qxfg). find-class previously had no
    // metaobject for the numeric tower / sequence / array classes, so
    // `(find-class 'integer)` — and FIND-METHOD over `(find-class …)` specializers,
    // as ansi-test universe.lsp builds — errored. These are additive: class-of
    // still returns the concrete leaf (FIXNUM/FLOAT/…) and SUBTYPEP uses the type
    // lattice, not the CLOS CPL, so registering them changes neither.
    let num_cls = TorclVal::from_fixnum(-11);
    let real_cls = TorclVal::from_fixnum(-12);
    let ratl_cls = TorclVal::from_fixnum(-13);
    let int_cls = TorclVal::from_fixnum(-14);
    let ratio_cls = TorclVal::from_fixnum(-15);
    let cplx_cls = TorclVal::from_fixnum(-16);
    let seq_cls = TorclVal::from_fixnum(-17);
    let list_cls = TorclVal::from_fixnum(-18);
    let arr_cls = TorclVal::from_fixnum(-19);
    let vec_cls = TorclVal::from_fixnum(-20);
    let str_cls = TorclVal::from_fixnum(-21);
    let bitv_cls = TorclVal::from_fixnum(-22);
    let htbl_cls = TorclVal::from_fixnum(-23);

    // Names: interned into the shared registry by their real CL names
    // (bliss-jtc.6 Stage E) so a built-in class's CLASS-NAME / TYPE-OF is the
    // same symbol the reader and interpreter produce — no reserved 0xFFFE_*
    // indices that don't correspond to any actual symbol.
    //
    // GC safety: `intern` allocates and can fire a minor GC whose root scan
    // re-enters CLOS_STATE, so ALL interning happens *before* the `with_state_mut`
    // borrow below (bliss-wlf). Once the borrow is held the closure only stores
    // these already-interned immediates — it never allocates.
    let nm = |name: &str| TorclVal::from_symbol_index(torcl_rt::symbols::intern(name));
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
                name: t_nm,
                direct_supers: vec![],
                direct_subs: vec![],
                slots: vec![],
                wrapper: None,
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
                wrapper: None,
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
                    wrapper: None,
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
        ];
        for (nm, cls, supers) in hierarchy {
            st.class_registry.insert(nm, cls);
            st.class_meta.insert(
                cls,
                ClassMeta {
                    name: nm,
                    direct_supers: supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: None,
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
/// The bare, uppercase, package-stripped name of a class-name symbol, used as
/// the drift-resilient key for [`ClosState::class_by_name`]. Returns `None` for
/// a non-symbol name.
fn class_name_key(name: TorclVal) -> Option<String> {
    if !name.is_symbol() {
        return None;
    }
    // NIL and T are symbols at the Common Lisp level, but TorCL represents
    // them with the SPECIAL tag rather than TAG_SYMBOL.  Do not pass either
    // through as_symbol_index(), which deliberately accepts only TAG_SYMBOL.
    if name == NIL {
        return Some("NIL".into());
    }
    if name == T {
        return Some("T".into());
    }
    let full = torcl_rt::symbols::symbol_name(name.as_symbol_index())?;
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

pub fn find_class(name: TorclVal) -> Option<TorclVal> {
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
pub fn set_structure_class(class: TorclVal) {
    with_state_mut(|st| {
        st.structure_classes.insert(class);
    });
}

/// Whether `class` is a structure class (DEFSTRUCT). NIL / unknown → false.
pub fn is_structure_class(class: TorclVal) -> bool {
    if class.is_nil() {
        return false;
    }
    with_state(|st| st.structure_classes.contains(&class))
}

/// Register a class by name.
pub fn set_find_class(name: TorclVal, class: TorclVal) -> Result<(), TorclError> {
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
                    name,
                    direct_supers: default_supers,
                    direct_subs: vec![],
                    slots: vec![],
                    wrapper: None,
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
pub fn bind_class_name(name: TorclVal, class: TorclVal) -> Result<(), TorclError> {
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
    name: TorclVal,
    class: TorclVal,
    direct_supers: &[TorclVal],
    slots: &[TorclVal],
) -> Result<(), TorclError> {
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

        st.class_meta.insert(
            class,
            ClassMeta {
                name,
                direct_supers: supers,
                direct_subs: vec![],
                slots: slots.to_vec(),
                wrapper: None,
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
fn finalize_class_layout(st: &mut ClosState, class: TorclVal) {
    if is_builtin_class(st, class) {
        return;
    }
    let cpl = c3_linearize(st, class).unwrap_or_else(|_| vec![class]);
    let mut order: Vec<TorclVal> = Vec::new();
    let mut index: HashMap<TorclVal, usize> = HashMap::new();
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
    let layout = Box::leak(Box::new(SlotLayout { order, index }));
    let wrapper: &'static ClassWrapper = Box::leak(Box::new(ClassWrapper {
        stamp: next_stamp(),
        state: AtomicU8::new(WRAPPER_CURRENT),
        class,
        slot_count,
        layout,
    }));
    if let Some(meta) = st.class_meta.get_mut(&class) {
        meta.wrapper = Some(wrapper);
    }
}

/// Return true if `object` is a live CLOS standard-object instance. Uses the
/// liveness registry so it is safe to call on ANY value, including dangling or
/// garbage heap-tagged values that must not be dereferenced (see
/// `ClosState.live_instances`).
pub fn is_instance(object: TorclVal) -> bool {
    // A bounds-checked header read replaces the old `live_instances` registry
    // (bliss-334): an instance is any heap value whose object header says
    // STANDARD_OBJECT. Dangling values get the same staleness contract as
    // every other heap reference under the moving collector.
    torcl_rt::gc::heap_object_type_id(object) == Some(type_id::STANDARD_OBJECT)
}

/// Get the class of an object.
pub fn class_of(object: TorclVal) -> TorclVal {
    // Inspect heap state before taking the root-side-table lock (GC takes
    // these locks in the opposite direction while scanning definitions).
    if torcl_rt::gc::heap_object_type_id(object) == Some(type_id::STANDARD_OBJECT) {
        return unsafe { (*instance_wrapper(object)).class };
    }
    if crate::hash_table_p(object) {
        return TorclVal::from_fixnum(-23);
    }
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
        if object.is_heap_object() {
            return st.heap_object_class;
        }
        st.t_class_val
    })
}

/// Get the class name.
pub fn class_name(class: TorclVal) -> TorclVal {
    with_state(|st| st.class_meta.get(&class).map(|m| m.name).unwrap_or(NIL))
}

/// Compute the class precedence list using C3 linearization (R5.67).
pub fn compute_class_precedence_list(class: TorclVal) -> Result<Vec<TorclVal>, TorclError> {
    with_state(|st| {
        if let Some(cpl) = st.cpl_cache.borrow().get(&class) {
            return Ok(cpl.clone());
        }
        // Pure Rust allocation only: no GC or Lisp callbacks while borrowed.
        // Do not cache errors or unknown classes. This bounds derived entries
        // by registered metadata and preserves the existing unknown-class API.
        let cpl = c3_linearize(st, class)?;
        if st.class_meta.contains_key(&class) {
            st.cpl_cache.borrow_mut().insert(class, cpl.clone());
        }
        Ok(cpl)
    })
}

/// Get the direct superclasses of a class.
pub fn class_direct_superclasses(class: TorclVal) -> Vec<TorclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_supers.clone())
            .unwrap_or_default()
    })
}

/// Get the direct subclasses of a class.
pub fn class_direct_subclasses(class: TorclVal) -> Vec<TorclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.direct_subs.clone())
            .unwrap_or_default()
    })
}

/// Get the slots of a class.
pub fn class_slots(class: TorclVal) -> Vec<TorclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.slots.clone())
            .unwrap_or_default()
    })
}

/// All effective slot names of `class` — its own direct slots plus every
/// inherited slot — in precedence order (most-general superclass first, so an
/// :include parent's slots precede the child's), de-duplicated. Used for #S
/// structure printing and EQUALP structure comparison, which need the full
/// slot set, not just the direct slots `class_slots` returns.
pub fn effective_slots(class: TorclVal) -> Vec<TorclVal> {
    let cpl = match compute_class_precedence_list(class) {
        Ok(c) => c,
        Err(_) => return class_slots(class),
    };
    let mut seen: HashSet<TorclVal> = HashSet::new();
    let mut out: Vec<TorclVal> = Vec::new();
    for c in cpl.into_iter().rev() {
        for slot in class_slots(c) {
            if seen.insert(slot) {
                out.push(slot);
            }
        }
    }
    out
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
fn infer_group_supers(st: &ClosState, class: TorclVal) -> Option<Vec<TorclVal>> {
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

fn c3_linearize(st: &ClosState, class: TorclVal) -> Result<Vec<TorclVal>, TorclError> {
    #[cfg(test)]
    cpl_tests::LINEARIZATIONS.with(|count| count.set(count.get() + 1));
    c3_linearize_guarded(st, class, &mut Vec::new())
}

/// `visiting` is the chain of classes currently being linearized; re-entering
/// one means the (possibly heuristic-inferred) super graph is CYCLIC, which
/// previously recursed to a stack-overflow SIGSEGV (bliss-d0b:
/// trivial-gray-streams' fundamental-stream vs the bootstrap stream classes).
/// Report it as a catchable error naming the class instead.
fn c3_linearize_guarded(
    st: &ClosState,
    class: TorclVal,
    visiting: &mut Vec<TorclVal>,
) -> Result<Vec<TorclVal>, TorclError> {
    if visiting.contains(&class) {
        let name = st.class_meta.get(&class).map(|m| m.name).unwrap_or(class);
        // Do not enter the symbol registry while holding the CLOS root lock.
        let rendered = format!("{name:?}");
        return Err(TorclError::Internal(format!(
            "cyclic class hierarchy: {rendered} appears among its own superclasses"
        )));
    }
    visiting.push(class);
    let result = c3_linearize_inner(st, class, visiting);
    visiting.pop();
    result
}

fn c3_linearize_inner(
    st: &ClosState,
    class: TorclVal,
    visiting: &mut Vec<TorclVal>,
) -> Result<Vec<TorclVal>, TorclError> {
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
    let mut lists: Vec<Vec<TorclVal>> = Vec::with_capacity(direct_supers.len() + 1);
    for s in &direct_supers {
        lists.push(c3_linearize_guarded(st, *s, visiting)?);
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
                TorclError::Internal("C3 linearization failed: inconsistent hierarchy".into())
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
pub fn allocate_instance(class: TorclVal) -> Result<TorclVal, TorclError> {
    // Fetch the wrapper + slot count as Copy values, finalising the layout if
    // the class has none yet, then allocate outside the state borrow.
    let (wrapper, slot_count) = with_state_mut(|st| {
        let mut w = st
            .class_meta
            .get(&class)
            .and_then(|m| m.wrapper)
            .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, class);
            w = st
                .class_meta
                .get(&class)
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
    if wrapper.is_null() {
        return Err(TorclError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    let size = 16 + 8 * slot_count;
    debug_assert!(
        size / 8 <= 0xFFFE,
        "instance too large for header size field"
    );
    // Ordinary instances are ORDINARY HEAP OBJECTS (bliss-334): nursery-born,
    // movable, and collectible, like any cons. They used to be std::alloc'd
    // off-heap and recorded in a `live_instances` registry that (a) leaked
    // every instance forever and (b) force-rooted all their slots via the CLOS
    // root scanner. The GC already knows the STANDARD_OBJECT layout (word 0 is
    // the raw wrapper pointer, the rest are traced slot values), so instances
    // need no side-table at all.
    let body = torcl_rt::gc::alloc_typed(size - 8, type_id::STANDARD_OBJECT)
        .ok_or_else(|| TorclError::Internal("GC heap unavailable for instance".into()))?;
    unsafe {
        let ptr = body.sub(8);
        let inst = TorclVal::from_heap_ptr(ptr);
        set_instance_wrapper(inst, wrapper);
        for i in 0..slot_count {
            *slot_cell(inst, i) = UNBOUND;
        }
        Ok(inst)
    }
}

/// Allocate a CLOS instance of `class` on the shared GC heap and pin it, so the
/// collector never moves or frees it (bliss-4v8 / D5.13). Used for the immortal
/// STORAGE-CONDITION pool: its preallocated instances must keep their addresses
/// forever, even across a moving collection. Layout and slot initialization are
/// identical to [`allocate_instance`]; only the backing store (GC heap, pinned)
/// differs.
pub fn allocate_instance_pinned_gc(class: TorclVal) -> Result<TorclVal, TorclError> {
    let (wrapper, slot_count) = with_state_mut(|st| {
        let mut w = st
            .class_meta
            .get(&class)
            .and_then(|m| m.wrapper)
            .map(|wrapper| wrapper as *const ClassWrapper as *mut ClassWrapper)
            .unwrap_or(std::ptr::null_mut());
        if w.is_null() {
            finalize_class_layout(st, class);
            w = st
                .class_meta
                .get(&class)
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
    if wrapper.is_null() {
        return Err(TorclError::Internal(
            "cannot allocate an instance of a class with no slot layout".into(),
        ));
    }
    let size = 16 + 8 * slot_count;
    debug_assert!(
        size / 8 <= 0xFFFE,
        "instance too large for header size field"
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
    let body = torcl_rt::gc::alloc_pinned_typed(size - 8, type_id::STANDARD_OBJECT)
        .ok_or_else(|| TorclError::Internal("GC heap unavailable for pooled instance".into()))?;
    unsafe {
        let ptr = body.sub(8);
        let inst = TorclVal::from_heap_ptr(ptr);
        set_instance_wrapper(inst, wrapper);
        for i in 0..slot_count {
            *slot_cell(inst, i) = UNBOUND;
        }
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
pub fn struct_slot_order(class: TorclVal) -> Vec<TorclVal> {
    with_state_mut(|st| {
        let cpl = c3_linearize(st, class).unwrap_or_else(|_| vec![class]);
        let mut order: Vec<TorclVal> = Vec::new();
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
pub fn make_struct(class: TorclVal, values: &[TorclVal]) -> Result<TorclVal, TorclError> {
    // allocate_instance allocates and can relocate; the caller's values may be
    // in an unrooted Rust Vec, so root a copy and write from THAT (bliss-334).
    torcl_rt::rooted!(values = values.to_vec());
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

pub fn make_instance(class: TorclVal, initargs: &[TorclVal]) -> Result<TorclVal, TorclError> {
    with_state(|st| {
        if is_builtin_class(st, class) {
            return Err(TorclError::Internal(
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
    torcl_rt::rooted!(initargs_rooted = initargs.to_vec());
    let inst = allocate_instance(class)?;
    initialize_instance(inst, &initargs_rooted)?;
    Ok(inst)
}

/// Initialize an instance (INITIALIZE-INSTANCE).
/// Per ANSI CL, initialize-instance calls (shared-initialize instance T initargs).
/// Initargs are pairwise (slot-name, value).
pub fn initialize_instance(instance: TorclVal, initargs: &[TorclVal]) -> Result<(), TorclError> {
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
/// `:initform` default values. Since TorCL does not yet store initforms,
/// only the initarg filtering behaviour is implemented.
pub fn shared_initialize(
    instance: TorclVal,
    slot_names: TorclVal,
    initargs: &[TorclVal],
) -> Result<(), TorclError> {
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
    instance: TorclVal,
    eligible: Option<&[TorclVal]>,
    initargs: &[TorclVal],
) -> Result<(), TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
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
pub fn reinitialize_instance(instance: TorclVal, initargs: &[TorclVal]) -> Result<(), TorclError> {
    // Per spec R5.81: reinitialize-instance calls shared-initialize with
    // slot-names = NIL.  Now that shared_initialize handles NIL correctly
    // (applies initargs but skips initforms), we call it directly.
    shared_initialize(instance, NIL, initargs)
}

// ── Slot access ────────────────────────────────────────────────────

/// Get a slot value (SLOT-VALUE). An unbound slot signals `UnboundVariable`
/// (the CLI turns this into `unbound-slot`); a name outside the class layout is
/// a `slot-missing`-style error.
pub fn slot_value(instance: TorclVal, slot_name: TorclVal) -> Result<TorclVal, TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                let v = *slot_cell(instance, idx);
                if v == UNBOUND {
                    Err(TorclError::UnboundVariable(slot_name))
                } else {
                    Ok(v)
                }
            }
            None => Err(TorclError::Internal(format!(
                "slot not present in class layout: {}",
                torcl_rt::symbols::symbol_name(slot_name.as_symbol_index()).unwrap_or_default()
            ))),
        }
    }
}

/// Set a slot value ((SETF SLOT-VALUE)).
pub fn set_slot_value(
    instance: TorclVal,
    slot_name: TorclVal,
    new_value: TorclVal,
) -> Result<(), TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
        match instance_slot_index(instance, slot_name) {
            Some(idx) => {
                *slot_cell(instance, idx) = new_value;
                Ok(())
            }
            None => Err(TorclError::Internal(format!(
                "slot not present in class layout: {}",
                torcl_rt::symbols::symbol_name(slot_name.as_symbol_index()).unwrap_or_default()
            ))),
        }
    }
}

/// Check if a slot is bound (SLOT-BOUNDP). A name outside the layout is treated
/// as unbound (returns `false`) rather than an error.
pub fn slot_boundp(instance: TorclVal, slot_name: TorclVal) -> Result<bool, TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
    }
    unsafe {
        update_if_obsolete(instance);
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
pub fn slot_present_p(instance: TorclVal, slot_name: TorclVal) -> bool {
    if !is_instance(instance) {
        return false;
    }
    unsafe {
        update_if_obsolete(instance);
        instance_slot_index(instance, slot_name).is_some()
    }
}

/// Make a slot unbound (SLOT-MAKUNBOUND).
pub fn slot_makunbound(instance: TorclVal, slot_name: TorclVal) -> Result<(), TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
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
    name: TorclVal,
    lambda_list: TorclVal,
) -> Result<TorclVal, TorclError> {
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
pub fn generic_function_name(gf: TorclVal) -> Option<TorclVal> {
    with_state(|st| st.generic_functions.get(&gf).map(|d| d.name))
}

/// Add a method to a generic function. Upserts the tracking entry if the
/// function is not already registered here: the interpreter's live dispatch uses
/// its own method table (cli.rs env.methods), so this map only tracks methods,
/// and a generic function reaching add-method without a prior make_generic_function
/// entry (e.g. one restored from an image) must not be an error (bliss-lb6).
pub fn add_method(generic_function: TorclVal, method: TorclVal) -> Result<(), TorclError> {
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
pub fn remove_method(generic_function: TorclVal, method: TorclVal) -> Result<(), TorclError> {
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
    method: TorclVal,
    specializers: Vec<TorclVal>,
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
fn specializer_applicable(st: &ClosState, specializer: TorclVal, arg_class: TorclVal) -> bool {
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
    method: TorclVal,
    arg_classes: &[TorclVal],
    precedence: &[usize],
) -> Vec<usize> {
    let Some(meta) = st.method_meta.get(&method) else {
        // No specializer metadata: unspecialized, and so least specific.
        return vec![UNSPECIALIZED_DISTANCE; arg_classes.len().max(1)];
    };
    let distance = |i: usize, spec: TorclVal| -> usize {
        if spec == st.t_class_val || spec == NIL {
            return UNSPECIALIZED_DISTANCE;
        }
        match c3_linearize(st, arg_classes[i]) {
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
fn argument_precedence_indicator() -> TorclVal {
    use std::sync::OnceLock;
    static INDICATOR: OnceLock<u32> = OnceLock::new();
    TorclVal::from_symbol_index(
        *INDICATOR.get_or_init(|| torcl_rt::symbols::intern(ARGUMENT_PRECEDENCE_PROPERTY)),
    )
}

/// The name of that indicator, so the writer (DEFGENERIC, which has the cons
/// allocator) and this reader agree on one symbol.
pub const ARGUMENT_PRECEDENCE_PROPERTY: &str = "TORCL::%ARGUMENT-PRECEDENCE-ORDER";

/// The argument precedence order DEFGENERIC recorded for the generic function
/// named by `name`, as positions into its required parameters. Empty means the
/// default left-to-right order.
///
/// It is kept as a symbol property rather than in a host-side registry so that
/// it travels with a saved image like any other Lisp datum. Reading it allocates
/// nothing.
pub fn argument_precedence_order(name: TorclVal) -> Vec<usize> {
    let Some(index) = name.symbol_index() else {
        return Vec::new(); // a (setf f) name is a cons and carries no plist
    };
    let Some(plist) = torcl_rt::symbols::symbol_plist(index) else {
        return Vec::new();
    };
    let indicator = argument_precedence_indicator();
    let mut order = Vec::new();
    let mut cursor = plist;
    while cursor.is_cons() {
        // SAFETY: every cons cell walked here is one this list's own CAR/CDR
        // reached; no allocation happens in the loop, so none can go stale.
        let cell = unsafe { &*(cursor.as_ptr() as *const torcl_rt::object::ConsCell) };
        if !cell.cdr.is_cons() {
            break; // malformed plist: an indicator with no value
        }
        let value_cell = unsafe { &*(cell.cdr.as_ptr() as *const torcl_rt::object::ConsCell) };
        if cell.car == indicator {
            let mut positions = value_cell.car;
            while positions.is_cons() {
                let position =
                    unsafe { &*(positions.as_ptr() as *const torcl_rt::object::ConsCell) };
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

/// Compute the applicable methods for given arguments.
///
/// Filters the generic function's methods to those whose specializers
/// are supertypes of the corresponding argument classes, then sorts
/// most-specific-first using CPL position.
pub fn compute_applicable_methods(generic_function: TorclVal, args: &[TorclVal]) -> Vec<TorclVal> {
    let arg_classes: Vec<TorclVal> = args.iter().copied().map(class_of).collect();
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
        let mut applicable: Vec<TorclVal> = gf
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
    _generic_function: TorclVal,
    combination: MethodCombinationType,
    methods: &[TorclVal],
) -> Result<TorclVal, TorclError> {
    if methods.is_empty() {
        return Err(TorclError::Internal(
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
                return Err(TorclError::Internal(
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

            let primary_methods: Vec<TorclVal> = with_state(|st| {
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
                return Err(TorclError::Internal(
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
pub fn get_effective_method(key: TorclVal) -> Option<EffectiveMethodParts> {
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
pub fn get_short_form_method(key: TorclVal) -> Option<(MethodCombinationType, Vec<TorclVal>)> {
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
pub fn change_class(instance: TorclVal, new_class: TorclVal) -> Result<(), TorclError> {
    if !is_instance(instance) {
        return Err(TorclError::Internal("not an instance".into()));
    }

    // Step 1: snapshot the old instance's bound slots by name, and its inline
    // capacity (the number of cells the allocation was sized for).
    let (old_capacity, snapshot): (usize, Vec<(TorclVal, TorclVal)>) = unsafe {
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
        return Err(TorclError::Internal(
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
            let nl = (*new_wrapper).layout;
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
        //
        // GC safety (bliss-334): the allocation can relocate both the old
        // instance and the snapshotted slot values; root them across it.
        let mut instance = instance;
        let mut snapshot = snapshot;
        torcl_rt::rooted_ref!(_inst_root = &mut instance);
        torcl_rt::rooted_ref!(_snap_root = &mut snapshot);
        let new_inst = allocate_instance(new_class)?;
        unsafe {
            let nl = (*new_wrapper).layout;
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
type EffectiveMethodParts = (Vec<TorclVal>, Vec<TorclVal>, Vec<TorclVal>, Vec<TorclVal>);

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

    fn class(id: i64) -> TorclVal {
        TorclVal::from_fixnum(id)
    }

    fn define(id: i64, supers: &[TorclVal]) -> TorclVal {
        let value = class(id);
        define_class(value, value, supers, &[]).unwrap();
        value
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
        let a = TorclVal::from_meta_handle(54010);
        let b = TorclVal::from_meta_handle(54011);
        let c = TorclVal::from_meta_handle(54012);
        let d = TorclVal::from_meta_handle(54013);
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
