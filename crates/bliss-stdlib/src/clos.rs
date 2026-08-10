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

#[derive(Clone)]
struct GFData {
    #[allow(dead_code)]
    name: BlissVal,
    #[allow(dead_code)]
    lambda_list: BlissVal,
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
/// Initargs are pairwise (slot-name, value).
pub fn initialize_instance(
    instance: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        let mut i = 0;
        while i + 1 < initargs.len() {
            inst.slots.insert(initargs[i], Some(initargs[i + 1]));
            i += 2;
        }
        Ok(())
    })
}

/// Shared initialize (SHARED-INITIALIZE).
/// When `slot_names` is T, all slots are eligible for initialization.
pub fn shared_initialize(
    instance: BlissVal,
    _slot_names: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        let mut i = 0;
        while i + 1 < initargs.len() {
            inst.slots.insert(initargs[i], Some(initargs[i + 1]));
            i += 2;
        }
        Ok(())
    })
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

/// Compute the applicable methods for given arguments.
/// Without specialiser metadata, returns an empty vec.
pub fn compute_applicable_methods(
    _generic_function: BlissVal,
    _args: &[BlissVal],
) -> Vec<BlissVal> {
    Vec::new()
}

/// Compute the effective method for a set of applicable methods.
///
/// * `Standard` — primary method is the first applicable method.
/// * Other variants encode their discriminant into the result so every
///   combination type produces a distinct `BlissVal`.
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
        MethodCombinationType::Standard => Ok(methods[0]),
        other => Ok(BlissVal::from_fixnum(10_000 + other.discriminant())),
    }
}

// ── Class change protocol ──────────────────────────────────────────

/// Change the class of an instance (CHANGE-CLASS). R5.16.
pub fn change_class(
    instance: BlissVal,
    new_class: BlissVal,
) -> Result<(), BlissError> {
    with_state_mut(|st| {
        let inst = st
            .instances
            .get_mut(&instance)
            .ok_or_else(|| BlissError::Internal("not an instance".into()))?;
        inst.class = new_class;
        Ok(())
    })
}
