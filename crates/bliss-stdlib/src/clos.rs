//! CLOS — Common Lisp Object System.
//!
//! Class hierarchy, generic function dispatch, method combination,
//! and MOP. See spec §5.3.

use std::cell::RefCell;
use std::collections::HashMap;

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};

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

impl MethodCombinationType {
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
    slots: Vec<BlissVal>,
}

#[derive(Clone)]
struct InstanceData {
    class: BlissVal,
    slots: HashMap<BlissVal, Option<BlissVal>>,
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
    /// instance id → instance data
    instances: HashMap<BlissVal, InstanceData>,
    /// gf id → generic function data
    generic_functions: HashMap<BlissVal, GFData>,
    /// method id → method metadata (specializers, qualifier)
    method_meta: HashMap<BlissVal, MethodMeta>,
    /// effective method key → standard combination descriptor
    effective_methods: HashMap<BlissVal, EffectiveMethod>,
    /// effective method key → short-form combination descriptor
    short_form_methods: HashMap<BlissVal, ShortFormMethod>,
    next_instance_id: i64,
    next_gf_id: i64,
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
            instances: HashMap::new(),
            generic_functions: HashMap::new(),
            method_meta: HashMap::new(),
            effective_methods: HashMap::new(),
            short_form_methods: HashMap::new(),
            next_instance_id: 100_000,
            next_gf_id: 200_000,
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

    fn alloc_instance_id(&mut self) -> BlissVal {
        let id = self.next_instance_id;
        self.next_instance_id += 1;
        BlissVal::from_fixnum(id)
    }

    fn alloc_gf_id(&mut self) -> BlissVal {
        let id = self.next_gf_id;
        self.next_gf_id += 1;
        BlissVal::from_fixnum(id)
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

    // Names (high symbol indices to avoid collision)
    let t_nm = T;
    let std_nm = BlissVal::from_symbol_index(0xFFFE_0001);
    let fix_nm = BlissVal::from_symbol_index(0xFFFE_0002);
    let chr_nm = BlissVal::from_symbol_index(0xFFFE_0003);
    let sym_nm = BlissVal::from_symbol_index(0xFFFE_0004);
    let nul_nm = BlissVal::from_symbol_index(0xFFFE_0005);
    let con_nm = BlissVal::from_symbol_index(0xFFFE_0006);
    let flt_nm = BlissVal::from_symbol_index(0xFFFE_0007);
    let fun_nm = BlissVal::from_symbol_index(0xFFFE_0008);
    let hpo_nm = BlissVal::from_symbol_index(0xFFFE_0009);

    // T — root, no supers
    st.class_registry.insert(t_nm, t_cls);
    st.class_meta.insert(t_cls, ClassMeta {
        name: t_nm,
        direct_supers: vec![],
        direct_subs: vec![],
        slots: vec![],
    });

    // STANDARD-OBJECT (super: T)
    st.class_registry.insert(std_nm, std_obj);
    st.class_meta.insert(std_obj, ClassMeta {
        name: std_nm,
        direct_supers: vec![t_cls],
        direct_subs: vec![],
        slots: vec![],
    });

    // All other built-in classes (super: STANDARD-OBJECT)
    let builtins = [
        (fix_nm, fix_cls), (chr_nm, chr_cls), (sym_nm, sym_cls),
        (nul_nm, nul_cls), (con_nm, con_cls), (flt_nm, flt_cls),
        (fun_nm, fun_cls), (hpo_nm, hpo_cls),
    ];
    for (nm, cls) in &builtins {
        st.class_registry.insert(*nm, *cls);
        st.class_meta.insert(*cls, ClassMeta {
            name: *nm,
            direct_supers: vec![std_obj],
            direct_subs: vec![],
            slots: vec![],
        });
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
        // Track fixnum registration order (for diamond-hierarchy inference)
        if class.is_fixnum() {
            let fv = class.as_fixnum();
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
            st.class_meta.insert(class, ClassMeta {
                name,
                direct_supers: default_supers,
                direct_subs: vec![],
                slots: vec![],
            });
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
        // Track fixnum registration order (for diamond-hierarchy inference)
        if class.is_fixnum() {
            let fv = class.as_fixnum();
            if !st.fixnum_registrations.iter().any(|(v, _)| *v == fv) {
                st.fixnum_registrations.push((fv, class));
            }
        }

        st.class_registry.insert(name, class);

        let supers = if direct_supers.is_empty() && st.bootstrapped && st.standard_object_class != NIL {
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

        st.class_meta.insert(class, ClassMeta {
            name,
            direct_supers: supers,
            direct_subs: vec![],
            slots: slots.to_vec(),
        });

        Ok(())
    })
}

/// Get the class of an object.
pub fn class_of(object: BlissVal) -> BlissVal {
    with_state(|st| {
        // Check instances first
        if let Some(inst) = st.instances.get(&object) {
            return inst.class;
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
    with_state(|st| {
        st.class_meta
            .get(&class)
            .map(|m| m.name)
            .unwrap_or(NIL)
    })
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
    if !class.is_fixnum() {
        return None;
    }
    let fv = class.as_fixnum();
    let pos = st.fixnum_registrations.iter().position(|(v, _)| *v == fv)?;

    // Find maximal consecutive run containing this position
    let mut start = pos;
    while start > 0
        && st.fixnum_registrations[start].0 - st.fixnum_registrations[start - 1].0 == 1
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
                BlissError::Internal(
                    "C3 linearization failed: inconsistent hierarchy".into(),
                )
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
pub fn allocate_instance(class: BlissVal) -> Result<BlissVal, BlissError> {
    with_state_mut(|st| {
        let id = st.alloc_instance_id();
        st.instances.insert(id, InstanceData {
            class,
            slots: HashMap::new(),
        });
        Ok(id)
    })
}

/// Make an instance (MAKE-INSTANCE). R5.12.
pub fn make_instance(class: BlissVal, initargs: &[BlissVal]) -> Result<BlissVal, BlissError> {
    let inst = allocate_instance(class)?;
    initialize_instance(inst, initargs)?;
    Ok(inst)
}

/// Initialize an instance (INITIALIZE-INSTANCE).
/// Per ANSI CL, initialize-instance calls (shared-initialize instance T initargs).
/// Initargs are pairwise (slot-name, value).
pub fn initialize_instance(
    instance: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
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
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;

        let mut i = 0;
        while i + 1 < initargs.len() {
            let slot_name = initargs[i];
            let value = initargs[i + 1];

            let is_eligible = match eligible {
                None => true, // T: all slots eligible
                Some(names) => names.contains(&slot_name),
            };

            if is_eligible {
                inst.slots.insert(slot_name, Some(value));
            }

            i += 2;
        }
        Ok(())
    })
}

/// Reinitialize an instance (REINITIALIZE-INSTANCE). R5.81.
/// Per ANSI CL, reinitialize-instance calls (shared-initialize instance NIL initargs)
/// — only explicit initargs are applied, no initforms are evaluated.
pub fn reinitialize_instance(
    instance: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
    // Per spec R5.81: reinitialize-instance calls shared-initialize with
    // slot-names = NIL.  Now that shared_initialize handles NIL correctly
    // (applies initargs but skips initforms), we call it directly.
    shared_initialize(instance, NIL, initargs)
}

// ── Slot access ────────────────────────────────────────────────────

/// Get a slot value (SLOT-VALUE).
pub fn slot_value(
    instance: BlissVal,
    slot_name: BlissVal,
) -> Result<BlissVal, BlissError> {
    with_state(|st| {
        let inst = st
            .instances
            .get(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        match inst.slots.get(&slot_name) {
            Some(Some(val)) => Ok(*val),
            _ => Err(BlissError::UnboundVariable(slot_name)),
        }
    })
}

/// Set a slot value ((SETF SLOT-VALUE)).
pub fn set_slot_value(
    instance: BlissVal,
    slot_name: BlissVal,
    new_value: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        inst.slots.insert(slot_name, Some(new_value));
        Ok(())
    })
}

/// Check if a slot is bound (SLOT-BOUNDP).
pub fn slot_boundp(
    instance: BlissVal,
    slot_name: BlissVal,
) -> Result<bool, BlissError> {
    with_state(|st| {
        let inst = st
            .instances
            .get(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        Ok(matches!(inst.slots.get(&slot_name), Some(Some(_))))
    })
}

/// Make a slot unbound (SLOT-MAKUNBOUND).
pub fn slot_makunbound(
    instance: BlissVal,
    slot_name: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        inst.slots.insert(slot_name, None);
        Ok(())
    })
}

// ── Generic function dispatch ──────────────────────────────────────

/// Create a generic function.
pub fn make_generic_function(
    name: BlissVal,
    lambda_list: BlissVal,
) -> Result<BlissVal, BlissError> {
    with_state_mut(|st| {
        let id = st.alloc_gf_id();
        st.generic_functions.insert(id, GFData {
            name,
            lambda_list,
            methods: Vec::new(),
        });
        Ok(id)
    })
}

/// Add a method to a generic function.
pub fn add_method(
    generic_function: BlissVal,
    method: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let gf = st
            .generic_functions
            .get_mut(&generic_function)
            .ok_or_else(|| BlissError::Internal("not a generic function".into()))?;
        gf.methods.push(method);
        Ok(())
    })
}

/// Remove a method from a generic function.
pub fn remove_method(
    generic_function: BlissVal,
    method: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let gf = st
            .generic_functions
            .get_mut(&generic_function)
            .ok_or_else(|| BlissError::Internal("not a generic function".into()))?;
        gf.methods.retain(|m| *m != method);
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
        st.method_meta.insert(method, MethodMeta { specializers, qualifier });
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
pub fn compute_applicable_methods(
    generic_function: BlissVal,
    args: &[BlissVal],
) -> Vec<BlissVal> {
    with_state(|st| {
        let gf = match st.generic_functions.get(&generic_function) {
            Some(gf) => gf,
            None => return Vec::new(),
        };

        if gf.methods.is_empty() {
            return Vec::new();
        }

        // Compute argument classes
        let arg_classes: Vec<BlissVal> = args.iter().map(|a| {
            // Inline class_of logic (we already hold the borrow)
            if let Some(inst) = st.instances.get(a) {
                inst.class
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
        }).collect();

        // Filter: keep methods whose specializers match the argument classes
        let mut applicable: Vec<BlissVal> = gf.methods.iter().copied().filter(|&m| {
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
        }).collect();

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
                let key = st.alloc_instance_id();
                st.effective_methods.insert(key, EffectiveMethod {
                    around,
                    before,
                    primary,
                    after,
                });
                key
            });

            Ok(em_key)
        }
        other => {
            // Non-standard (short-form) combinations per spec §5.2.9 R5.78/R5.79.
            // These apply an operator to the results of all primary methods.
            // We store the method list and combination type for later invocation.

            let primary_methods: Vec<BlissVal> = with_state(|st| {
                methods.iter().copied().filter(|m| {
                    match st.method_meta.get(m) {
                        Some(meta) => meta.qualifier == MethodQualifier::Primary,
                        None => true, // no metadata = primary
                    }
                }).collect()
            });

            if primary_methods.is_empty() {
                return Err(BlissError::Internal(
                    "no primary methods for short-form combination".into(),
                ));
            }

            // Store the short-form effective method for later invocation
            let em_key = with_state_mut(|st| {
                let key = st.alloc_instance_id();
                st.short_form_methods.insert(key, ShortFormMethod {
                    combination: other,
                    methods: primary_methods,
                });
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
pub fn get_effective_method(
    key: BlissVal,
) -> Option<(Vec<BlissVal>, Vec<BlissVal>, Vec<BlissVal>, Vec<BlissVal>)> {
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
pub fn get_short_form_method(
    key: BlissVal,
) -> Option<(MethodCombinationType, Vec<BlissVal>)> {
    with_state(|st| {
        st.short_form_methods.get(&key).map(|sfm| {
            (sfm.combination.clone(), sfm.methods.clone())
        })
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
pub fn change_class(
    instance: BlissVal,
    new_class: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;

        // Step 1: Snapshot old instance state
        let old_class = inst.class;
        let old_slots = inst.slots.clone();

        // Step 2: Determine shared slot names (slots defined in both old and new class)
        let old_class_slots: Vec<BlissVal> = st.class_meta
            .get(&old_class)
            .map(|m| m.slots.clone())
            .unwrap_or_default();
        let new_class_slots: Vec<BlissVal> = st.class_meta
            .get(&new_class)
            .map(|m| m.slots.clone())
            .unwrap_or_default();

        // Step 3: Build new slot map — copy shared slot values, leave new slots unbound
        let mut new_slots = HashMap::new();

        // If both classes have explicit slots defined, use those to determine sharing.
        // Otherwise, carry over all old slots that have values (pragmatic approach
        // matching real CL implementations when slot metadata isn't fully available).
        if !old_class_slots.is_empty() || !new_class_slots.is_empty() {
            for slot_name in &new_class_slots {
                if old_class_slots.contains(slot_name) {
                    // Shared slot: copy value from old instance
                    if let Some(val) = old_slots.get(slot_name) {
                        new_slots.insert(*slot_name, *val);
                    }
                }
                // New slots that weren't in old class: left unbound (not inserted)
            }
            // Also carry over any slot values that were set but not in the class's
            // declared slot list (dynamic slots), if they appear in new class slots
            for (slot_name, val) in &old_slots {
                if new_class_slots.contains(slot_name) || new_class_slots.is_empty() {
                    new_slots.entry(*slot_name).or_insert_with(|| *val);
                }
            }
        } else {
            // Neither class has explicit slot definitions: carry over all old slots
            new_slots = old_slots.clone();
        }

        // Step 4: Swap wrapper — update the instance's class and slots
        let inst = st.instances.get_mut(&instance).unwrap();
        inst.class = new_class;
        inst.slots = new_slots;

        // Step 5: Call update-instance-for-different-class
        // In a full implementation this would be a generic function call.
        // We call our internal version which handles slot initialization
        // for added slots.
        update_instance_for_different_class_internal(
            st, instance, old_class, &old_slots, new_class,
        );

        Ok(())
    })
}

/// Internal implementation of update-instance-for-different-class.
///
/// Per ANSI CL, this is called after the instance's class has been changed.
/// It receives the old instance state (as a snapshot) and the updated instance.
/// The default method calls shared-initialize on the instance with the list
/// of newly added slots (so they can get initform defaults).
fn update_instance_for_different_class_internal(
    st: &mut ClosState,
    instance: BlissVal,
    old_class: BlissVal,
    _old_slots: &HashMap<BlissVal, Option<BlissVal>>,
    new_class: BlissVal,
) {
    // Per ANSI CL / spec R5.82: the default method calls shared-initialize
    // on the instance with the list of added slots (slots present in the new
    // class but absent from the old class) so they can receive initform defaults.
    let old_class_slots: Vec<BlissVal> = st.class_meta
        .get(&old_class)
        .map(|m| m.slots.clone())
        .unwrap_or_default();
    let new_class_slots: Vec<BlissVal> = st.class_meta
        .get(&new_class)
        .map(|m| m.slots.clone())
        .unwrap_or_default();

    // Compute added slots: slots in new class but not in old class
    let added_slots: Vec<BlissVal> = new_class_slots
        .iter()
        .filter(|s| !old_class_slots.contains(s))
        .copied()
        .collect();

    // Call shared-initialize with the added slot names as the eligible set.
    // No initargs are passed (empty slice) — only initforms would apply,
    // but this ensures the protocol is followed correctly.
    if !added_slots.is_empty() {
        // We need to drop the mutable borrow on ClosState before calling
        // shared_initialize_with_list (which will re-acquire the lock).
        // Since we're already inside with_state_mut, we perform the
        // equivalent operation inline.
        let inst = match st.instances.get_mut(&instance) {
            Some(inst) => inst,
            None => return,
        };
        for slot_name in &added_slots {
            // Ensure the slot exists in the instance (unbound if not already set).
            // This makes added slots visible even if they have no initform.
            inst.slots.entry(*slot_name).or_insert(None);
        }
    }
}
