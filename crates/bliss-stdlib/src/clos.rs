//! CLOS — Common Lisp Object System.
//!
//! Class hierarchy, generic function dispatch, method combination,
//! and MOP. See spec §5.3.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Class protocol ─────────────────────────────────────────────────

/// Find a class by name.
pub fn find_class(name: BlissVal) -> Option<BlissVal> {
    unimplemented!("find_class")
}

/// Register a class by name.
pub fn set_find_class(name: BlissVal, class: BlissVal) -> Result<(), BlissError> {
    unimplemented!("set_find_class")
}

/// Get the class of an object.
pub fn class_of(object: BlissVal) -> BlissVal {
    unimplemented!("class_of")
}

/// Get the class name.
pub fn class_name(class: BlissVal) -> BlissVal {
    unimplemented!("class_name")
}

/// Compute the class precedence list using C3 linearization (R5.11).
pub fn compute_class_precedence_list(class: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    unimplemented!("compute_class_precedence_list")
}

/// Get the direct superclasses of a class.
pub fn class_direct_superclasses(class: BlissVal) -> Vec<BlissVal> {
    unimplemented!("class_direct_superclasses")
}

/// Get the direct subclasses of a class.
pub fn class_direct_subclasses(class: BlissVal) -> Vec<BlissVal> {
    unimplemented!("class_direct_subclasses")
}

/// Get the slots of a class.
pub fn class_slots(class: BlissVal) -> Vec<BlissVal> {
    unimplemented!("class_slots")
}

// ── Instance protocol ──────────────────────────────────────────────

/// Allocate an instance of a class (ALLOCATE-INSTANCE).
pub fn allocate_instance(class: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("allocate_instance")
}

/// Make an instance (MAKE-INSTANCE). R5.12.
pub fn make_instance(class: BlissVal, initargs: &[BlissVal]) -> Result<BlissVal, BlissError> {
    unimplemented!("make_instance")
}

/// Initialize an instance (INITIALIZE-INSTANCE).
pub fn initialize_instance(instance: BlissVal, initargs: &[BlissVal]) -> Result<(), BlissError> {
    unimplemented!("initialize_instance")
}

/// Shared initialize (SHARED-INITIALIZE).
pub fn shared_initialize(
    instance: BlissVal,
    slot_names: BlissVal,
    initargs: &[BlissVal],
) -> Result<(), BlissError> {
    unimplemented!("shared_initialize")
}

// ── Slot access ────────────────────────────────────────────────────

/// Get a slot value (SLOT-VALUE).
pub fn slot_value(instance: BlissVal, slot_name: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("slot_value")
}

/// Set a slot value ((SETF SLOT-VALUE)).
pub fn set_slot_value(
    instance: BlissVal,
    slot_name: BlissVal,
    new_value: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("set_slot_value")
}

/// Check if a slot is bound (SLOT-BOUNDP).
pub fn slot_boundp(instance: BlissVal, slot_name: BlissVal) -> Result<bool, BlissError> {
    unimplemented!("slot_boundp")
}

/// Make a slot unbound (SLOT-MAKUNBOUND).
pub fn slot_makunbound(instance: BlissVal, slot_name: BlissVal) -> Result<(), BlissError> {
    unimplemented!("slot_makunbound")
}

// ── Generic function dispatch ──────────────────────────────────────

/// Create a generic function.
pub fn make_generic_function(
    name: BlissVal,
    lambda_list: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("make_generic_function")
}

/// Add a method to a generic function.
pub fn add_method(generic_function: BlissVal, method: BlissVal) -> Result<(), BlissError> {
    unimplemented!("add_method")
}

/// Remove a method from a generic function.
pub fn remove_method(generic_function: BlissVal, method: BlissVal) -> Result<(), BlissError> {
    unimplemented!("remove_method")
}

/// Compute the applicable methods for given arguments.
pub fn compute_applicable_methods(
    generic_function: BlissVal,
    args: &[BlissVal],
) -> Vec<BlissVal> {
    unimplemented!("compute_applicable_methods")
}

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

/// Compute the effective method for a set of applicable methods.
pub fn compute_effective_method(
    generic_function: BlissVal,
    combination: MethodCombinationType,
    methods: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    unimplemented!("compute_effective_method")
}

// ── Class change protocol ──────────────────────────────────────────

/// Change the class of an instance (CHANGE-CLASS). R5.16.
pub fn change_class(instance: BlissVal, new_class: BlissVal) -> Result<(), BlissError> {
    unimplemented!("change_class")
}

// ── CLOS bootstrap ─────────────────────────────────────────────────

/// Initialize the CLOS bootstrap: create proto-classes, wire up metaclass
/// circularity. R5.10.
pub fn bootstrap_clos() -> Result<(), BlissError> {
    unimplemented!("bootstrap_clos")
}
