//! Macro expansion engine.
//!
//! Expansion runs after reading and before IR construction.
//! Implements the algorithm from spec §4.2 / A4.01.

use std::collections::{HashMap, HashSet};
use std::cell::Cell;
use std::sync::{Arc, LazyLock, RwLock};

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, TAG_CONS, TAG_MASK};

use crate::reader::symbol_name;

// ── Constants ─────────────────────────────────────────────────────

/// Default maximum number of macroexpand-1 iterations per macroexpand call.
/// Catches non-repeating divergent expansions (spec §4.2.13, §4.2.3 Phase 2 step 2.f).
const DEFAULT_MACROEXPAND_LIMIT: usize = 65536;

thread_local! {
    static MACROEXPAND_LIMIT: Cell<usize> = const { Cell::new(DEFAULT_MACROEXPAND_LIMIT) };
}

/// Set the per-thread macroexpand iteration limit.
pub fn set_macroexpand_limit(limit: usize) {
    MACROEXPAND_LIMIT.with(|cell| cell.set(limit));
}

/// Get the current per-thread macroexpand iteration limit.
fn get_macroexpand_limit() -> usize {
    MACROEXPAND_LIMIT.with(|cell| cell.get())
}

// ── Declaration info ──────────────────────────────────────────────

/// Optimization qualities for declaration-information 'optimize' queries.
#[derive(Clone, Debug)]
pub struct OptimizeQualities {
    pub speed: u8,
    pub safety: u8,
    pub debug: u8,
    pub space: u8,
    pub compilation_speed: u8,
}

impl Default for OptimizeQualities {
    fn default() -> Self {
        OptimizeQualities {
            speed: 1,
            safety: 1,
            debug: 1,
            space: 1,
            compilation_speed: 1,
        }
    }
}

/// A declaration entry stored in the environment.
#[derive(Clone, Debug)]
pub enum DeclInfo {
    /// Optimization settings.
    Optimize(OptimizeQualities),
    /// A user-declared declaration name (via DECLARATION proclamation).
    Declaration(u64),
    /// Type declaration for a variable (symbol key, type-spec value).
    Type(u64, BlissVal),
    /// Ignore declaration for a variable.
    Ignore(u64),
    /// Ignorable declaration for a variable.
    Ignorable(u64),
    /// Special (dynamic) declaration.
    Dynamic(u64),
    /// Custom implementation-specific declaration.
    Custom(u64, BlissVal),
}

// ── Environment protocol ───────────────────────────────────────────

/// Lexical environment for macro expansion (CLtL2 §8.5).
///
/// Uses a parent chain for nested lexical scopes: each augmentation creates
/// a new frame with only the new bindings, pointing to the previous environment
/// as parent. Lookups walk up the chain (spec §4.2.9 R4.14, §4.2.10).
#[derive(Clone, Debug)]
pub struct Environment {
    /// Parent environment (lexical chain).
    parent: Option<Arc<Environment>>,
    /// Variable bindings keyed by BlissVal.0 (raw u64).
    variables: HashMap<u64, VariableInfo>,
    /// Function bindings keyed by BlissVal.0 (raw u64).
    functions: HashMap<u64, FunctionInfo>,
    /// Active declarations.
    declarations: Vec<DeclInfo>,
    /// Block names in scope (for RETURN-FROM), keyed by BlissVal.0.
    blocks: HashSet<u64>,
    /// Tag names in scope (for GO), keyed by BlissVal.0.
    tags: HashSet<u64>,
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

/// Inline policy for function declarations (used by compiler macro suppression).
#[derive(Clone, Debug, PartialEq)]
pub enum InlinePolicy {
    Unspecified,
    Inline,
    NotInline,
}

impl Environment {
    /// Create an empty (null) lexical environment.
    pub fn null() -> Self {
        Environment {
            parent: None,
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: Vec::new(),
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Query variable information (CLtL2 `variable-information`).
    /// Walks the parent chain to find the binding in the nearest enclosing scope.
    pub fn variable_information(&self, name: BlissVal) -> Option<VariableInfo> {
        if let Some(info) = self.variables.get(&name.0) {
            return Some(info.clone());
        }
        if let Some(ref parent) = self.parent {
            return parent.variable_information(name);
        }
        None
    }

    /// Query function information (CLtL2 `function-information`).
    /// Walks the parent chain to find the binding in the nearest enclosing scope.
    pub fn function_information(&self, name: BlissVal) -> Option<FunctionInfo> {
        if let Some(info) = self.functions.get(&name.0) {
            return Some(info.clone());
        }
        if let Some(ref parent) = self.parent {
            return parent.function_information(name);
        }
        None
    }

    /// Query declaration information (CLtL2 `declaration-information`).
    /// Supports standard queries:
    /// - For 'optimize': returns optimize qualities as a BlissVal encoding.
    /// - For 'declaration': returns list of valid declaration names.
    /// Walks the parent chain to find declarations (spec §4.2.9 R4.14).
    pub fn declaration_information(&self, decl_name: BlissVal) -> Option<BlissVal> {
        // Search this frame's declarations for an optimize entry
        for decl in &self.declarations {
            match decl {
                DeclInfo::Optimize(qualities) => {
                    // Encode optimize qualities as a fixnum packing:
                    // speed in bits 0-2, safety 3-5, debug 6-8, space 9-11, compilation_speed 12-14
                    let packed = (qualities.speed as i64)
                        | ((qualities.safety as i64) << 3)
                        | ((qualities.debug as i64) << 6)
                        | ((qualities.space as i64) << 9)
                        | ((qualities.compilation_speed as i64) << 12);
                    return Some(BlissVal::from_fixnum(packed));
                }
                DeclInfo::Declaration(name_key) => {
                    if *name_key == decl_name.0 {
                        return Some(bliss_rt::value::T);
                    }
                }
                _ => {}
            }
        }
        // Walk parent chain
        if let Some(ref parent) = self.parent {
            return parent.declaration_information(decl_name);
        }
        None
    }

    /// Augment this environment with a variable binding.
    /// Returns a new Environment frame with only the new binding; the current
    /// environment becomes the parent (O(1) per augmentation via parent chain).
    pub fn augment_variable(&self, name: BlissVal, info: VariableInfo) -> Environment {
        let mut variables = HashMap::new();
        variables.insert(name.0, info);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables,
            functions: HashMap::new(),
            declarations: Vec::new(),
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Augment this environment with a function binding.
    /// Returns a new Environment frame with only the new binding; the current
    /// environment becomes the parent.
    pub fn augment_function(&self, name: BlissVal, info: FunctionInfo) -> Environment {
        let mut functions = HashMap::new();
        functions.insert(name.0, info);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions,
            declarations: Vec::new(),
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Augment this environment with declarations.
    /// Returns a new Environment frame with the given declarations.
    pub fn augment_declarations(&self, decls: Vec<DeclInfo>) -> Environment {
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: decls,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Augment with a block name in scope.
    pub fn augment_block(&self, block_name: BlissVal) -> Environment {
        let mut blocks = HashSet::new();
        blocks.insert(block_name.0);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: Vec::new(),
            blocks,
            tags: HashSet::new(),
        }
    }

    /// Augment with a tag name in scope.
    pub fn augment_tag(&self, tag_name: BlissVal) -> Environment {
        let mut tags = HashSet::new();
        tags.insert(tag_name.0);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: Vec::new(),
            blocks: HashSet::new(),
            tags,
        }
    }

    /// General-purpose augment-environment (CLtL2 compatible).
    /// Creates a new environment augmented with given bindings and declarations.
    pub fn augment_environment(
        &self,
        variables: Vec<(BlissVal, VariableInfo)>,
        functions: Vec<(BlissVal, FunctionInfo)>,
        declarations: Vec<DeclInfo>,
    ) -> Environment {
        let mut var_map = HashMap::new();
        for (name, info) in variables {
            var_map.insert(name.0, info);
        }
        let mut fn_map = HashMap::new();
        for (name, info) in functions {
            fn_map.insert(name.0, info);
        }
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: var_map,
            functions: fn_map,
            declarations,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Check if a function has a notinline declaration in this environment.
    pub fn is_notinline(&self, name: BlissVal) -> bool {
        for decl in &self.declarations {
            if let DeclInfo::Custom(key, val) = decl {
                // Convention: notinline stored as Custom with a sentinel
                if *key == name.0 && val.0 == NOTINLINE_SENTINEL {
                    return true;
                }
            }
        }
        if let Some(ref parent) = self.parent {
            return parent.is_notinline(name);
        }
        false
    }

    /// Check if a block name is in scope.
    pub fn has_block(&self, name: BlissVal) -> bool {
        if self.blocks.contains(&name.0) {
            return true;
        }
        if let Some(ref parent) = self.parent {
            return parent.has_block(name);
        }
        false
    }

    /// Check if a tag name is in scope.
    pub fn has_tag(&self, name: BlissVal) -> bool {
        if self.tags.contains(&name.0) {
            return true;
        }
        if let Some(ref parent) = self.parent {
            return parent.has_tag(name);
        }
        false
    }
}

/// Sentinel value used to mark notinline declarations in DeclInfo::Custom.
const NOTINLINE_SENTINEL: u64 = 0xFFFF_FFFF_DEAD_BEEF;

// ── Global macro table ────────────────────────────────────────────

/// Global macro table: maps operator symbol keys to expander BlissVals.
/// Protected by RwLock for concurrent compilation (spec §4.2.12).
static GLOBAL_MACRO_TABLE: LazyLock<RwLock<HashMap<u64, BlissVal>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Global compiler macro table: maps function name keys to compiler macro
/// expander functions. Protected by RwLock (spec §4.2.4, §4.2.12).
/// The expander takes (form, env) and returns either a replacement form
/// or the original form (to decline).
static COMPILER_MACRO_TABLE: LazyLock<RwLock<HashMap<u64, CompilerMacroFn>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Compiler macro function type.
/// Takes (whole_form, env) and returns either a transformed form or the
/// original `whole_form` (pointer-equal) to decline the transformation.
pub type CompilerMacroFn = Arc<dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync>;

/// Register a global macro (DEFMACRO).
pub fn define_global_macro(name: BlissVal, expander: BlissVal) {
    let mut table = GLOBAL_MACRO_TABLE.write().unwrap();
    table.insert(name.0, expander);
}

/// Remove a global macro.
pub fn undefine_global_macro(name: BlissVal) {
    let mut table = GLOBAL_MACRO_TABLE.write().unwrap();
    table.remove(&name.0);
}

/// Look up a global macro by name.
fn lookup_global_macro(name: BlissVal) -> Option<BlissVal> {
    let table = GLOBAL_MACRO_TABLE.read().unwrap();
    table.get(&name.0).copied()
}

/// Register a compiler macro (DEFINE-COMPILER-MACRO).
pub fn define_compiler_macro(name: BlissVal, expander: CompilerMacroFn) {
    let mut table = COMPILER_MACRO_TABLE.write().unwrap();
    table.insert(name.0, expander);
}

/// Remove a compiler macro.
pub fn undefine_compiler_macro(name: BlissVal) {
    let mut table = COMPILER_MACRO_TABLE.write().unwrap();
    table.remove(&name.0);
}

/// Look up a compiler macro by name.
fn lookup_compiler_macro(name: BlissVal) -> Option<CompilerMacroFn> {
    let table = COMPILER_MACRO_TABLE.read().unwrap();
    table.get(&name.0).cloned()
}

// ── Macro function invocation registry ────────────────────────────

/// Registry mapping BlissVal expander identities to callable Rust functions.
/// This enables the default_hook (funcall) to actually invoke macro expanders
/// that are represented as BlissVal handles (spec §4.2.8 R4.15).
static MACRO_FUNCTION_REGISTRY: LazyLock<RwLock<HashMap<u64, Arc<dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a macro expander function that can be invoked by the default hook.
/// The `key` is the BlissVal that appears as FunctionInfo::Macro(key).
pub fn register_macro_function(
    key: BlissVal,
    func: Arc<dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync>,
) {
    let mut registry = MACRO_FUNCTION_REGISTRY.write().unwrap();
    registry.insert(key.0, func);
}

/// Look up a registered macro function by its BlissVal identity.
fn lookup_macro_function(key: BlissVal) -> Option<Arc<dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync>> {
    let registry = MACRO_FUNCTION_REGISTRY.read().unwrap();
    registry.get(&key.0).cloned()
}

// ── Expansion hook ─────────────────────────────────────────────────

/// The type of `*macroexpand-hook*`.
/// Signature: `(expander form env) -> expanded_form`.
pub type MacroexpandHook = fn(BlissVal, BlissVal, &Environment) -> Result<BlissVal, BlissError>;

/// Default hook: implements `funcall` semantics (spec §4.2.8 R4.15).
///
/// For function macros (FunctionInfo::Macro), the expander is a BlissVal handle.
/// The hook looks up the registered Rust-side callable in MACRO_FUNCTION_REGISTRY
/// and invokes it with (form, env). If no callable is registered, it falls back
/// to returning the expander value — this supports symbol macros where the
/// expander IS the expansion value (macroexpand_1 passes (expansion, expansion, env)).
///
/// For full funcall semantics with arbitrary Lisp functions, the runtime must
/// register each macro's expander via `register_macro_function`.
fn default_hook(expander: BlissVal, form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    // Try to look up and invoke the expander as a registered macro function.
    // This implements true funcall semantics: (funcall expander form env).
    if let Some(func) = lookup_macro_function(expander) {
        return func(form, env);
    }
    // Fallback: for symbol macros, macroexpand_1 passes (expansion, expansion, env),
    // so returning the first argument yields the correct expansion value.
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

/// Allocate a new cons cell with the given car and cdr.
/// Returns a cons-tagged BlissVal pointing to the new cell.
///
/// The cell is heap-allocated and leaked (ownership transferred to the GC).
/// In a full implementation, this would use the GC allocator.
fn alloc_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    let cell = Box::new(ConsCell { car, cdr });
    let ptr = Box::into_raw(cell) as *mut u8;
    unsafe { BlissVal::from_cons_ptr(ptr) }
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
/// If the operator is not found in the local environment, consults the
/// global macro table (spec §4.2.3 Phase 1 step 2.e).
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

        // 2.b: Look up operator in local environment
        match env.function_information(operator) {
            Some(FunctionInfo::Macro(expander)) => {
                // 2.c: Found as macro — invoke hook
                let hook = get_macroexpand_hook();
                let result = hook(expander, form, env)?;
                return Ok((result, true));
            }
            Some(FunctionInfo::SpecialOperator) | Some(FunctionInfo::Lexical) | Some(FunctionInfo::Global) => {
                // 2.d: Found as special operator or function — no expansion
                return Ok((form, false));
            }
            None => {
                // 2.e: NOT found in local env — consult global macro table
                if let Some(expander) = lookup_global_macro(operator) {
                    let hook = get_macroexpand_hook();
                    let result = hook(expander, form, env)?;
                    return Ok((result, true));
                }
                // No global macro either — no expansion
            }
        }
    }

    // 3. No expansion
    Ok((form, false))
}

/// Fully expand a form (iterate `macroexpand_1` until no change).
/// Returns `(expanded_form, expanded_p)`.
/// Detects circular expansion by tracking seen forms (R4.16).
/// Enforces *macroexpand-limit* iteration cap (spec §4.2.13, default 65536).
pub fn macroexpand(form: BlissVal, env: &Environment) -> Result<(BlissVal, bool), BlissError> {
    let mut current = form;
    let mut ever_expanded = false;
    let mut seen = HashSet::new();
    let mut iteration_count: usize = 0;
    let limit = get_macroexpand_limit();

    // Insert the original form to detect self-referential expansions
    seen.insert(current.0);

    loop {
        let (expanded, did_expand) = macroexpand_1(current, env)?;
        if !did_expand {
            return Ok((current, ever_expanded));
        }
        ever_expanded = true;
        iteration_count += 1;

        // Check iteration limit (spec §4.2.3 Phase 2 step 2.f, §4.2.13)
        if iteration_count > limit {
            return Err(BlissError::Internal(
                format!("Macro expansion limit ({}) exceeded", limit),
            ));
        }

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
/// 3. If the result is a cons (compound form):
///    a. Check for QUOTE — quoted data is opaque, no sub-form expansion
///       occurs (spec §4.2.7).
///    b. Check for compiler macros (spec §4.2.4, R4.12) — if a compiler
///       macro exists for the operator and notinline is NOT declared,
///       invoke it. If it declines (returns form unchanged), fall through.
///    c. Per spec §4.2.3 Phase 3 step 4.d.ii, the operator is NOT
///       recursively code-walked (only arguments are expanded). The
///       operator was already checked for macros by macroexpand above.
pub fn macroexpand_all(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    // Step 1: Macroexpand the top-level form
    let (expanded, _) = macroexpand(form, env)?;

    // Step 2: If the result is an atom (not a cons), we're done.
    // NIL is an atom (it's the empty list, not a cons cell).
    if !expanded.is_cons() {
        return Ok(expanded);
    }

    // Step 3: The form is a compound (cons cell).
    let operator = unsafe { cons_car(expanded) };

    // Step 3.a: QUOTE suppression (spec §4.2.7).
    // Quoted data is opaque — no sub-form expansion should occur.
    if is_quote_symbol(operator) {
        return Ok(expanded);
    }

    // Step 3.b: Compiler macro check (spec §4.2.4, R4.12).
    // Only applies to function-call forms (not special operators or macros
    // after full macroexpansion).
    if !matches!(env.function_information(operator), Some(FunctionInfo::SpecialOperator)) {
        if !env.is_notinline(operator) {
            if let Some(cm_fn) = lookup_compiler_macro(operator) {
                let cm_result = cm_fn(expanded, env)?;
                // If the compiler macro returns a form that is NOT pointer-equal
                // to the input, it accepted — re-enter expand-form on the result.
                if cm_result.0 != expanded.0 {
                    return macroexpand_all(cm_result, env);
                }
                // Otherwise it declined — fall through to normal expansion.
            }
        }
    }

    // Step 3.c: Recursively walk the arguments (CDR) only, not the operator.
    // Per spec §4.2.3 Phase 3 step 4.d.ii, only the arguments (not the
    // operator) should be recursively expanded for function calls.
    let cdr = unsafe { cons_cdr(expanded) };
    let expanded_cdr = if cdr.is_cons() {
        walk_cons(cdr, env)?
    } else if !cdr.is_nil() {
        // Dotted pair tail — expand as atom
        let (expanded_cdr_val, _) = macroexpand(cdr, env)?;
        expanded_cdr_val
    } else {
        cdr
    };

    // If nothing changed, return the original form to preserve identity.
    if expanded_cdr == cdr {
        return Ok(expanded);
    }

    // Build a new cons cell with the operator unchanged and expanded arguments.
    Ok(alloc_cons(operator, expanded_cdr))
}

/// Check if a BlissVal is the QUOTE symbol.
fn is_quote_symbol(val: BlissVal) -> bool {
    if !val.is_symbol() {
        return false;
    }
    // NIL and T are special symbols that are not QUOTE
    if val.is_nil() || val.0 == bliss_rt::value::T.0 {
        return false;
    }
    let idx = val.as_symbol_index();
    match symbol_name(idx) {
        Some(name) => name == "QUOTE",
        None => false,
    }
}

/// Recursively walk a cons-cell structure, expanding all subforms.
///
/// For each cons cell, expand the CAR (which may be an atom or nested list)
/// and then recurse into the CDR (which is either another cons or NIL for
/// proper lists, or an atom for dotted pairs).
///
/// Returns a new cons structure with expanded values. The original cons cells
/// are NOT mutated — new cells are allocated when any sub-form changes.
/// This avoids corrupting shared cons structures (e.g., quoted data referenced
/// elsewhere, or forms appearing in multiple expansion contexts).
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

    // If nothing changed, return the original cons cell to preserve identity
    // (important for compiler macro decline checks which use pointer equality).
    if expanded_car == car && expanded_cdr == cdr {
        return Ok(form);
    }

    // Build a new cons cell with the expanded values (non-destructive).
    Ok(alloc_cons(expanded_car, expanded_cdr))
}
