//! Macro expansion engine.
//!
//! Expansion runs after reading and before IR construction.
//! Implements the algorithm from spec §4.2 / A4.01.

use std::collections::{HashMap, HashSet};
use std::cell::Cell;

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, TAG_CONS, TAG_MASK};

// ── Environment protocol ───────────────────────────────────────────

/// Lexical environment for macro expansion (CLtL2 §8.5).
pub struct Environment {
    /// Variable bindings keyed by BlissVal.0 (raw u64).
    variables: HashMap<u64, VariableInfo>,
    /// Function bindings keyed by BlissVal.0 (raw u64).
    functions: HashMap<u64, FunctionInfo>,
}

/// Information about a variable binding.
#[derive(Clone, Debug)]
pub enum VariableInfo {
    /// Lexical variable.
    Lexical,
    /// Special (dynamic) variable.
    Special,
    /// Constant.
    Constant(BlissVal),
    /// Symbol macro.
    SymbolMacro(BlissVal),
}

/// Information about a function binding.
#[derive(Clone, Debug)]
pub enum FunctionInfo {
    /// Lexical function (from FLET/LABELS).
    Lexical,
    /// Global function.
    Global,
    /// Macro.
    Macro(BlissVal),
    /// Special operator.
    SpecialOperator,
}

impl Environment {
    /// Create an empty (null) lexical environment.
    pub fn null() -> Self {
        Environment {
            variables: HashMap::new(),
            functions: HashMap::new(),
        }
    }

    /// Query variable information (CLtL2 `variable-information`).
    pub fn variable_information(&self, name: BlissVal) -> Option<VariableInfo> {
        self.variables.get(&name.0).cloned()
    }

    /// Query function information (CLtL2 `function-information`).
    pub fn function_information(&self, name: BlissVal) -> Option<FunctionInfo> {
        self.functions.get(&name.0).cloned()
    }

    /// Query declaration information (CLtL2 `declaration-information`).
    /// No declaration augmentation method exists, so this always returns None.
    pub fn declaration_information(&self, _decl_name: BlissVal) -> Option<BlissVal> {
        None
    }

    /// Augment this environment with a variable binding.
    /// Returns a new Environment with the binding added; does not mutate self.
    pub fn augment_variable(&self, name: BlissVal, info: VariableInfo) -> Environment {
        let mut variables = self.variables.clone();
        variables.insert(name.0, info);
        Environment {
            variables,
            functions: self.functions.clone(),
        }
    }

    /// Augment this environment with a function binding.
    /// Returns a new Environment with the binding added; does not mutate self.
    pub fn augment_function(&self, name: BlissVal, info: FunctionInfo) -> Environment {
        let mut functions = self.functions.clone();
        functions.insert(name.0, info);
        Environment {
            variables: self.variables.clone(),
            functions,
        }
    }
}

// ── Expansion hook ─────────────────────────────────────────────────

/// The type of `*macroexpand-hook*`.
/// Signature: `(expander form env) -> expanded_form`.
pub type MacroexpandHook = fn(BlissVal, BlissVal, &Environment) -> Result<BlissVal, BlissError>;

/// Default hook: returns the expander (first argument), which is the expansion value.
fn default_hook(expander: BlissVal, _form: BlissVal, _env: &Environment) -> Result<BlissVal, BlissError> {
    Ok(expander)
}

thread_local! {
    static MACROEXPAND_HOOK: Cell<MacroexpandHook> = const { Cell::new(default_hook as MacroexpandHook) };
}

/// Set the macroexpand hook.
pub fn set_macroexpand_hook(hook: MacroexpandHook) {
    MACROEXPAND_HOOK.with(|cell| cell.set(hook));
}

/// Get the current macroexpand hook.
fn get_macroexpand_hook() -> MacroexpandHook {
    MACROEXPAND_HOOK.with(|cell| cell.get())
}

// ── Cons cell helpers ──────────────────────────────────────────────

/// A cons cell is a headerless 16-byte pair: [car: BlissVal, cdr: BlissVal].
/// The cons-tagged pointer (tag 001) points to the first byte of the pair.
#[repr(C)]
struct ConsCell {
    car: BlissVal,
    cdr: BlissVal,
}

/// Extract the CAR of a cons cell.
///
/// # Safety
/// Caller must ensure `val` is a valid cons-tagged `BlissVal` (tag `001`)
/// whose underlying pointer refers to a live, properly-aligned cons cell.
unsafe fn cons_car(val: BlissVal) -> BlissVal {
    debug_assert_eq!(val.0 & TAG_MASK, TAG_CONS, "cons_car called on non-cons");
    let ptr = (val.0 & !TAG_MASK) as *const ConsCell;
    unsafe { (*ptr).car }
}

/// Extract the CDR of a cons cell.
///
/// # Safety
/// Same safety requirements as `cons_car`.
unsafe fn cons_cdr(val: BlissVal) -> BlissVal {
    debug_assert_eq!(val.0 & TAG_MASK, TAG_CONS, "cons_cdr called on non-cons");
    let ptr = (val.0 & !TAG_MASK) as *const ConsCell;
    unsafe { (*ptr).cdr }
}

/// Set the CAR of a cons cell to a new value.
///
/// # Safety
/// Same safety requirements as `cons_car`, plus the cons cell must be mutable.
unsafe fn cons_set_car(cell: BlissVal, new_car: BlissVal) {
    debug_assert_eq!(cell.0 & TAG_MASK, TAG_CONS, "cons_set_car called on non-cons");
    let ptr = (cell.0 & !TAG_MASK) as *mut ConsCell;
    unsafe { (*ptr).car = new_car; }
}

/// Set the CDR of a cons cell to a new value.
///
/// # Safety
/// Same safety requirements as `cons_car`, plus the cons cell must be mutable.
unsafe fn cons_set_cdr(cell: BlissVal, new_cdr: BlissVal) {
    debug_assert_eq!(cell.0 & TAG_MASK, TAG_CONS, "cons_set_cdr called on non-cons");
    let ptr = (cell.0 & !TAG_MASK) as *mut ConsCell;
    unsafe { (*ptr).cdr = new_cdr; }
}

// ── Expansion functions ────────────────────────────────────────────

/// Perform one step of macro expansion (CLHS `macroexpand-1`).
/// Returns `(expanded_form, expanded_p)`.
///
/// If form has a SymbolMacro binding in env, invokes the macroexpand hook
/// with (expansion_value, expansion_value, env) and returns (result, true).
///
/// If form is a cons whose car is a symbol with a Macro function binding,
/// invokes the macroexpand hook with (expander, form, env) and returns
/// (result, true).
///
/// Otherwise returns (form, false).
pub fn macroexpand_1(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    // 1. Check if form is a symbol with a symbol-macro binding
    if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(form) {
        let hook = get_macroexpand_hook();
        let result = hook(expansion, expansion, env)?;
        return Ok((result, true));
    }

    // 2. Check if form is a cons with a macro operator
    if form.is_cons() {
        let operator = unsafe { cons_car(form) };
        if let Some(FunctionInfo::Macro(expander)) = env.function_information(operator) {
            let hook = get_macroexpand_hook();
            let result = hook(expander, form, env)?;
            return Ok((result, true));
        }
    }

    // 3. No expansion
    Ok((form, false))
}

/// Fully expand a form (iterate `macroexpand_1` until no change).
/// Returns `(expanded_form, expanded_p)`.
/// Detects circular expansion by tracking seen forms.
pub fn macroexpand(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    let mut current = form;
    let mut ever_expanded = false;
    let mut seen = HashSet::new();

    // Insert the original form to detect self-referential expansions
    seen.insert(current.0);

    loop {
        let (expanded, did_expand) = macroexpand_1(current, env)?;
        if !did_expand {
            return Ok((current, ever_expanded));
        }
        ever_expanded = true;

        // Check for circular expansion (R4.16)
        if !seen.insert(expanded.0) {
            return Err(BlissError::Internal("circular macro expansion".into()));
        }

        current = expanded;
    }
}

/// Fully expand a form and all its subforms (recursive code-walk).
///
/// Implements the `expand-form` algorithm from spec §4.2.3 Phase 3:
/// 1. Macroexpand the top-level form.
/// 2. If the result is a self-evaluating atom or symbol, return it.
/// 3. If the result is a cons (compound form), recursively expand
///    each element in the list (both car and cdr positions), walking
///    through to handle symbol macros in argument positions.
///
/// This does NOT handle special-form-specific walking (e.g., QUOTE
/// suppression, LET binding environment augmentation) — that requires
/// the full code-walker which depends on operator dispatch tables.
/// This function provides the baseline recursive expansion that the
/// full code-walker builds upon.
pub fn macroexpand_all(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    // Step 1: Macroexpand the top-level form
    let (expanded, _) = macroexpand(form, env)?;

    // Step 2: If the result is an atom (not a cons), we're done.
    // NIL is an atom (it's the empty list, not a cons cell).
    if !expanded.is_cons() {
        return Ok(expanded);
    }

    // Step 3: The form is a compound (cons cell). Recursively walk
    // through the list structure, expanding each element.
    //
    // For a proper list (a b c), the structure is:
    //   cons(a, cons(b, cons(c, NIL)))
    // We need to expand each element (a, b, c) and also walk into
    // any nested lists.
    walk_cons(expanded, env)
}

/// Recursively walk a cons-cell structure, expanding all subforms.
///
/// For each cons cell, expand the CAR (which may be an atom or nested list)
/// and then recurse into the CDR (which is either another cons or NIL for
/// proper lists, or an atom for dotted pairs).
///
/// The expansion is done in-place on the existing cons structure when the
/// cons cells are mutable, or by returning transformed values.
fn walk_cons(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    debug_assert!(form.is_cons(), "walk_cons called on non-cons value");

    // Get car and cdr of the current cons cell
    let car = unsafe { cons_car(form) };
    let cdr = unsafe { cons_cdr(form) };

    // Recursively expand the car
    let expanded_car = macroexpand_all(car, env)?;

    // Recursively expand the cdr
    // The cdr is typically another cons (rest of list) or NIL (end of list),
    // but could be any value in a dotted pair.
    let expanded_cdr = if cdr.is_cons() {
        walk_cons(cdr, env)?
    } else {
        // For non-cons cdr (NIL or dotted-pair atom), expand as an atom
        let (expanded_cdr_val, _) = macroexpand(cdr, env)?;
        expanded_cdr_val
    };

    // Update the cons cell in-place with expanded values.
    // This mutates the existing list structure, which is the standard
    // approach for destructive macro expansion (the original form is
    // consumed by the compiler pipeline and not retained).
    if expanded_car != car {
        unsafe { cons_set_car(form, expanded_car); }
    }
    if expanded_cdr != cdr {
        unsafe { cons_set_cdr(form, expanded_cdr); }
    }

    Ok(form)
}
