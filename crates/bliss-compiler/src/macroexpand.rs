//! Macro expansion engine.
//!
//! Expansion runs after reading and before IR construction.
//! Implements the algorithm from spec §4.2 / A4.01.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

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

static MACROEXPAND_HOOK: Mutex<MacroexpandHook> = Mutex::new(default_hook as MacroexpandHook);

/// Set the macroexpand hook.
pub fn set_macroexpand_hook(hook: MacroexpandHook) {
    let mut guard = MACROEXPAND_HOOK.lock().unwrap();
    *guard = hook;
}

/// Get the current macroexpand hook.
fn get_macroexpand_hook() -> MacroexpandHook {
    let guard = MACROEXPAND_HOOK.lock().unwrap();
    *guard
}

// ── Expansion functions ────────────────────────────────────────────

/// Perform one step of macro expansion (CLHS `macroexpand-1`).
/// Returns `(expanded_form, expanded_p)`.
///
/// If form has a SymbolMacro binding in env, invokes the macroexpand hook
/// with (expansion_value, expansion_value, env) and returns (result, true).
/// Otherwise returns (form, false).
pub fn macroexpand_1(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    // Check if form has a symbol-macro binding in the environment
    if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(form) {
        let hook = get_macroexpand_hook();
        let result = hook(expansion, expansion, env)?;
        return Ok((result, true));
    }

    // No expansion
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

        // Check for circular expansion
        if !seen.insert(expanded.0) {
            return Err(BlissError::Internal("circular macro expansion".into()));
        }

        current = expanded;
    }
}

/// Fully expand a form and all its subforms (recursive walk).
/// For atomic forms, calls macroexpand and returns the result.
pub fn macroexpand_all(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let (expanded, _) = macroexpand(form, env)?;
    Ok(expanded)
}
