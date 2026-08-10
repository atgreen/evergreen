//! Macro expansion engine.
//!
//! Expansion runs after reading and before IR construction.
//! Implements the algorithm from spec §4.2 / A4.01.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Environment protocol ───────────────────────────────────────────

/// Lexical environment for macro expansion (CLtL2 §8.5).
pub struct Environment {
    _private: (),
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
        unimplemented!("Environment::null")
    }

    /// Query variable information (CLtL2 `variable-information`).
    pub fn variable_information(&self, name: BlissVal) -> Option<VariableInfo> {
        unimplemented!("Environment::variable_information")
    }

    /// Query function information (CLtL2 `function-information`).
    pub fn function_information(&self, name: BlissVal) -> Option<FunctionInfo> {
        unimplemented!("Environment::function_information")
    }

    /// Query declaration information (CLtL2 `declaration-information`).
    pub fn declaration_information(&self, decl_name: BlissVal) -> Option<BlissVal> {
        unimplemented!("Environment::declaration_information")
    }

    /// Augment this environment with a variable binding.
    pub fn augment_variable(&self, name: BlissVal, info: VariableInfo) -> Environment {
        unimplemented!("Environment::augment_variable")
    }

    /// Augment this environment with a function binding.
    pub fn augment_function(&self, name: BlissVal, info: FunctionInfo) -> Environment {
        unimplemented!("Environment::augment_function")
    }
}

// ── Expansion functions ────────────────────────────────────────────

/// Perform one step of macro expansion (CLHS `macroexpand-1`).
/// Returns `(expanded_form, expanded_p)`.
pub fn macroexpand_1(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    unimplemented!("macroexpand_1")
}

/// Fully expand a form (iterate `macroexpand_1` until no change).
/// Returns `(expanded_form, expanded_p)`.
pub fn macroexpand(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    unimplemented!("macroexpand")
}

/// Fully expand a form and all its subforms (recursive walk).
pub fn macroexpand_all(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    unimplemented!("macroexpand_all")
}

// ── Expansion hook ─────────────────────────────────────────────────

/// The type of `*macroexpand-hook*`.
/// Signature: `(expander form env) -> expanded_form`.
pub type MacroexpandHook = fn(BlissVal, BlissVal, &Environment) -> Result<BlissVal, BlissError>;

/// Set the macroexpand hook.
pub fn set_macroexpand_hook(hook: MacroexpandHook) {
    unimplemented!("set_macroexpand_hook")
}
