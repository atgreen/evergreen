//! Macro expansion engine.
//!
//! Expansion runs after reading and before IR construction.
//! Implements the algorithm from spec §4.2 / A4.01.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock, RwLock};

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, TAG_CONS, TAG_MASK};

use crate::reader::{intern_symbol, symbol_name};

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
    ///   Walks the parent chain to find declarations (spec §4.2.9 R4.14).
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
pub type CompilerMacroFn =
    Arc<dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync>;

/// Parsed macro lambda-expression used by `parse_macro`/`enclose`.
#[derive(Clone, Debug)]
pub struct ParsedMacro {
    name: BlissVal,
    lambda_list: BlissVal,
    body: BlissVal,
}

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
static MACRO_FUNCTION_REGISTRY: LazyLock<RwLock<HashMap<u64, Arc<MacroFn>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

static MACRO_FUNCTION_KEY_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Register a macro expander function that can be invoked by the default hook.
/// The `key` is the BlissVal that appears as FunctionInfo::Macro(key).
pub fn register_macro_function(key: BlissVal, func: Arc<MacroFn>) {
    let mut registry = MACRO_FUNCTION_REGISTRY.write().unwrap();
    registry.insert(key.0, func);
}

fn next_registered_macro_key() -> BlissVal {
    BlissVal::from_fixnum(MACRO_FUNCTION_KEY_COUNTER.fetch_add(1, AtomicOrdering::Relaxed) as i64)
}

/// Parse a macro definition into a lambda-expression suitable for `enclose`.
pub fn parse_macro(
    name: BlissVal,
    lambda_list: BlissVal,
    body: BlissVal,
    _env: Option<&Environment>,
) -> Result<ParsedMacro, BlissError> {
    if !name.is_symbol() {
        return Err(BlissError::Internal(
            "PARSE-MACRO: name must be a symbol".into(),
        ));
    }
    Ok(ParsedMacro {
        name,
        lambda_list,
        body,
    })
}

/// Close a parsed macro lambda-expression over the given lexical environment.
pub fn enclose(parsed: ParsedMacro, env: &Environment) -> Result<BlissVal, BlissError> {
    let key = next_registered_macro_key();
    let defining_env = env.clone();
    let _ = parsed.name;
    let lambda_list = parsed.lambda_list;
    let body = parsed.body;
    let func = Arc::new(move |whole_form: BlissVal, call_env: &Environment| {
        expand_local_macro_call(whole_form, call_env, &defining_env, lambda_list, body)
    });
    register_macro_function(key, func);
    Ok(key)
}

/// Look up a registered macro function by its BlissVal identity.
fn lookup_macro_function(key: BlissVal) -> Option<Arc<MacroFn>> {
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
fn default_hook(
    expander: BlissVal,
    form: BlissVal,
    env: &Environment,
) -> Result<BlissVal, BlissError> {
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
            Some(FunctionInfo::SpecialOperator)
            | Some(FunctionInfo::Lexical)
            | Some(FunctionInfo::Global) => {
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
    seen.insert(structural_fingerprint(current, 8));

    loop {
        let (expanded, did_expand) = macroexpand_1(current, env)?;
        if !did_expand {
            return Ok((current, ever_expanded));
        }
        ever_expanded = true;
        iteration_count += 1;

        // Check iteration limit (spec §4.2.3 Phase 2 step 2.f, §4.2.13)
        if iteration_count > limit {
            return Err(BlissError::Internal(format!(
                "Macro expansion limit ({}) exceeded",
                limit
            )));
        }

        // Check for circular expansion (R4.16)
        if !seen.insert(structural_fingerprint(expanded, 8)) {
            return Err(BlissError::Internal("circular macro expansion".into()));
        }

        current = expanded;
    }
}

/// Get the name of a symbol, if it is one (and not NIL or T).
fn get_symbol_name(val: BlissVal) -> Option<String> {
    if !val.is_symbol() || val.is_nil() || val.0 == bliss_rt::value::T.0 {
        return None;
    }
    let idx = val.as_symbol_index();
    symbol_name(idx)
}

/// Check if a BlissVal is a symbol with the given name.
fn is_symbol_named(val: BlissVal, name: &str) -> bool {
    match get_symbol_name(val) {
        Some(n) => n == name,
        None => false,
    }
}

/// Intern a CL symbol name and return it as a BlissVal.
fn make_symbol(name: &str) -> BlissVal {
    BlissVal::from_symbol_index(intern_symbol(name))
}

/// Collect cons list elements into a Vec (proper list only).
fn cons_to_vec(form: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut current = form;
    while current.is_cons() {
        result.push(unsafe { cons_car(current) });
        current = unsafe { cons_cdr(current) };
    }
    result
}

/// Build a proper cons list from a slice.
fn vec_to_cons(items: &[BlissVal]) -> BlissVal {
    let mut result = bliss_rt::value::NIL;
    for item in items.iter().rev() {
        result = alloc_cons(*item, result);
    }
    result
}

fn structural_fingerprint(form: BlissVal, depth: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    fingerprint_into(form, depth, &mut hasher);
    hasher.finish()
}

fn fingerprint_into(form: BlissVal, depth: usize, hasher: &mut DefaultHasher) {
    if depth == 0 || !form.is_cons() {
        form.0.hash(hasher);
        return;
    }

    0xC0DEC0DEu64.hash(hasher);
    let car = unsafe { cons_car(form) };
    let cdr = unsafe { cons_cdr(form) };
    fingerprint_into(car, depth - 1, hasher);
    fingerprint_into(cdr, depth - 1, hasher);
}

/// Expand a list of forms, returning a new list.
fn expand_body(forms: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let items = cons_to_vec(forms);
    let mut expanded_items = Vec::with_capacity(items.len());
    let mut changed = false;
    for item in &items {
        let exp = macroexpand_all(*item, env)?;
        if exp != *item {
            changed = true;
        }
        expanded_items.push(exp);
    }
    if !changed {
        Ok(forms)
    } else {
        Ok(vec_to_cons(&expanded_items))
    }
}

/// Check if operator is a lambda expression: (LAMBDA params body...)
fn is_lambda_expression(val: BlissVal) -> bool {
    if !val.is_cons() {
        return false;
    }
    let car = unsafe { cons_car(val) };
    is_symbol_named(car, "LAMBDA")
}

/// Fully expand a form and all its subforms (recursive code-walk).
///
/// Implements the `expand-form` algorithm from spec §4.2.3 Phase 3:
/// 1. Macroexpand the top-level form.
/// 2. If the result is a self-evaluating atom or symbol, return it.
/// 3. If the result is a cons (compound form):
///    a. Check for QUOTE — quoted data is opaque, no sub-form expansion
///    occurs (spec §4.2.7).
///    b. Dispatch to special-form handlers for special operators (spec §4.2.7).
///    c. Handle lambda expressions in operator position (spec §4.2.3 step 4.c).
///    d. Check for compiler macros (spec §4.2.4, R4.12) — if a compiler
///    macro exists for the operator and notinline is NOT declared,
///    invoke it. If it declines (returns form unchanged), fall through.
///    e. Per spec §4.2.3 Phase 3 step 4.d.ii, the operator is NOT
///    recursively code-walked (only arguments are expanded). The
///    operator was already checked for macros by macroexpand above.
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

    // Step 3.b: Special operator dispatch (spec §4.2.7).
    // Special operators have structural subforms (binding names, block names,
    // tag labels) that must NOT be expanded as expressions.
    let is_special = matches!(
        env.function_information(operator),
        Some(FunctionInfo::SpecialOperator)
    );
    if is_special || (operator.is_symbol() && is_known_special_operator(operator)) {
        return expand_special_form(operator, expanded, env);
    }

    // Step 3.c: Lambda expression in operator position (spec §4.2.3 step 4.c).
    // A form like ((lambda (x) x) 42) should have its lambda body expanded.
    if is_lambda_expression(operator) {
        return expand_lambda_call(operator, expanded, env);
    }

    // Step 3.d: Compiler macro check (spec §4.2.4, R4.12).
    // Only applies to function-call forms (not special operators or macros
    // after full macroexpansion).
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

    // Step 3.e: Function call — expand arguments only, not the operator.
    // Per spec §4.2.3 Phase 3 step 4.d.ii.
    expand_function_call_args(operator, expanded, env)
}

/// Expand the arguments of a function call form, leaving the operator untouched.
fn expand_function_call_args(
    operator: BlissVal,
    form: BlissVal,
    env: &Environment,
) -> Result<BlissVal, BlissError> {
    let cdr = unsafe { cons_cdr(form) };
    let expanded_cdr = if cdr.is_cons() {
        walk_cons(cdr, env)?
    } else if !cdr.is_nil() {
        let (expanded_cdr_val, _) = macroexpand(cdr, env)?;
        expanded_cdr_val
    } else {
        cdr
    };

    if expanded_cdr == cdr {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, expanded_cdr))
    }
}

/// Check if a symbol names a known CL special operator.
fn is_known_special_operator(val: BlissVal) -> bool {
    match get_symbol_name(val) {
        Some(name) => matches!(
            name.as_str(),
            "BLOCK"
                | "CATCH"
                | "EVAL-WHEN"
                | "FLET"
                | "FUNCTION"
                | "GO"
                | "IF"
                | "LABELS"
                | "LET"
                | "LET*"
                | "LOAD-TIME-VALUE"
                | "LOCALLY"
                | "MACROLET"
                | "MULTIPLE-VALUE-CALL"
                | "MULTIPLE-VALUE-PROG1"
                | "PROGN"
                | "PROGV"
                | "QUOTE"
                | "RETURN-FROM"
                | "SETQ"
                | "SYMBOL-MACROLET"
                | "TAGBODY"
                | "THE"
                | "THROW"
                | "UNWIND-PROTECT"
        ),
        None => false,
    }
}

/// Dispatch to the correct special-form expansion handler (spec §4.2.7).
fn expand_special_form(
    operator: BlissVal,
    form: BlissVal,
    env: &Environment,
) -> Result<BlissVal, BlissError> {
    let name = get_symbol_name(operator);
    let name_str = name.as_deref().unwrap_or("");

    match name_str {
        "QUOTE" => Ok(form),
        "GO" => Ok(form),
        "BLOCK" => expand_block(form, env),
        "RETURN-FROM" => expand_return_from(form, env),
        "TAGBODY" => expand_tagbody(form, env),
        "SETQ" => expand_setq(form, env),
        "THE" => expand_the(form, env),
        "EVAL-WHEN" => expand_eval_when(form, env),
        "FUNCTION" => expand_function_special(form, env),
        "LET" => expand_let(form, env, false),
        "LET*" => expand_let(form, env, true),
        "FLET" => expand_flet(form, env),
        "LABELS" => expand_labels(form, env),
        "LOCALLY" => expand_locally(form, env),
        "MACROLET" => expand_macrolet(form, env),
        "SYMBOL-MACROLET" => expand_symbol_macrolet(form, env),
        // For IF, PROGN, CATCH, THROW, UNWIND-PROTECT, MULTIPLE-VALUE-CALL,
        // MULTIPLE-VALUE-PROG1, PROGV, LOAD-TIME-VALUE — all subforms are
        // expression positions, so the generic walk is correct.
        _ => expand_function_call_args(operator, form, env),
    }
}

// ── Special form handlers ─────────────────────────────────────────

/// Expand BLOCK: (block name body...)
/// Block name is NOT expanded; body forms are expanded.
/// The block name is registered in the environment.
fn expand_block(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let block_name = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    // Augment env with block name
    let new_env = env.augment_block(block_name);
    let expanded_body = expand_body(body, &new_env)?;

    if expanded_body == body {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, alloc_cons(block_name, expanded_body)))
    }
}

/// Expand RETURN-FROM: (return-from name result-form)
/// Block name is NOT expanded; result form is expanded.
fn expand_return_from(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let block_name = unsafe { cons_car(args) };
    let rest = unsafe { cons_cdr(args) };

    if !rest.is_cons() {
        return Ok(form);
    }
    let result_form = unsafe { cons_car(rest) };
    let expanded_result = macroexpand_all(result_form, env)?;

    if expanded_result == result_form {
        Ok(form)
    } else {
        Ok(alloc_cons(
            operator,
            alloc_cons(
                block_name,
                alloc_cons(expanded_result, bliss_rt::value::NIL),
            ),
        ))
    }
}

/// Expand TAGBODY: (tagbody {tag|form}*)
/// Tags (symbols and integers) are NOT expanded. Non-tag forms are expanded.
fn expand_tagbody(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let body = unsafe { cons_cdr(form) };

    // First pass: register all tags in the environment
    let mut new_env = env.clone();
    let items = cons_to_vec(body);
    for item in &items {
        // Tags are symbols or integers (atoms that are not cons)
        if item.is_symbol() && !item.is_nil() {
            new_env = new_env.augment_tag(*item);
        }
    }

    // Second pass: expand non-tag forms
    let mut expanded_items = Vec::with_capacity(items.len());
    let mut changed = false;
    for item in &items {
        if (item.is_symbol() && !item.is_nil()) || item.is_fixnum() {
            // Tags: symbols and integers are NOT expanded
            expanded_items.push(*item);
        } else {
            let exp = macroexpand_all(*item, &new_env)?;
            if exp != *item {
                changed = true;
            }
            expanded_items.push(exp);
        }
    }

    if !changed {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, vec_to_cons(&expanded_items)))
    }
}

/// Expand SETQ: (setq {var value}*)
/// For each pair: if var is a symbol macro, convert to (setf expansion expanded-value).
/// Otherwise, expand value only (var is NOT expanded).
/// Spec §4.2.6, R4.13.
fn expand_setq(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    let items = cons_to_vec(args);

    if items.len() % 2 != 0 {
        return Err(BlissError::Internal(
            "SETQ requires an even number of arguments".into(),
        ));
    }

    let mut result_pairs = Vec::new();
    let mut any_symbol_macro = false;
    let mut changed = false;

    for i in (0..items.len()).step_by(2) {
        let var = items[i];
        let val_form = items[i + 1];

        // Check if var is a symbol macro
        if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(var) {
            // Convert (setq sym val) -> (setf expansion expanded-val)
            any_symbol_macro = true;
            let expanded_val = macroexpand_all(val_form, env)?;
            let setf_sym = make_symbol("SETF");
            let setf_form = alloc_cons(
                setf_sym,
                alloc_cons(expansion, alloc_cons(expanded_val, bliss_rt::value::NIL)),
            );
            // Re-enter expand-form on the setf form
            let expanded_setf = macroexpand_all(setf_form, env)?;
            result_pairs.push((var, val_form, Some(expanded_setf)));
        } else {
            // Normal case: expand value, don't expand var
            let expanded_val = macroexpand_all(val_form, env)?;
            if expanded_val != val_form {
                changed = true;
            }
            result_pairs.push((var, val_form, None));
        }
    }

    if any_symbol_macro {
        // If we have multiple pairs and any had symbol-macro conversion,
        // wrap in PROGN for multiple setf forms, or return single form.
        let mut setf_forms = Vec::new();
        let mut normal_pairs = Vec::new();
        for (var, val_form, setf_result) in &result_pairs {
            if let Some(setf_form) = setf_result {
                // Flush any accumulated normal setq pairs
                if !normal_pairs.is_empty() {
                    let setq_sym = operator;
                    let mut setq_args = Vec::new();
                    for (v, expanded_v) in normal_pairs.drain(..) {
                        setq_args.push(v);
                        setq_args.push(expanded_v);
                    }
                    setf_forms.push(alloc_cons(setq_sym, vec_to_cons(&setq_args)));
                }
                setf_forms.push(*setf_form);
            } else {
                let expanded_val = macroexpand_all(*val_form, env)?;
                normal_pairs.push((*var, expanded_val));
            }
        }
        // Flush remaining normal pairs
        if !normal_pairs.is_empty() {
            let setq_sym = operator;
            let mut setq_args = Vec::new();
            for (v, expanded_v) in normal_pairs.drain(..) {
                setq_args.push(v);
                setq_args.push(expanded_v);
            }
            setf_forms.push(alloc_cons(setq_sym, vec_to_cons(&setq_args)));
        }

        if setf_forms.len() == 1 {
            return Ok(setf_forms.into_iter().next().unwrap());
        } else {
            let progn_sym = make_symbol("PROGN");
            return Ok(alloc_cons(progn_sym, vec_to_cons(&setf_forms)));
        }
    }

    if !changed {
        return Ok(form);
    }

    // Reconstruct with expanded values
    let mut new_args = Vec::with_capacity(items.len());
    for (var, _, _) in &result_pairs {
        new_args.push(*var);
    }
    // Re-expand to get the right values
    let mut final_args = Vec::with_capacity(items.len());
    for i in (0..items.len()).step_by(2) {
        final_args.push(items[i]); // var unchanged
        final_args.push(macroexpand_all(items[i + 1], env)?); // expand value
    }
    Ok(alloc_cons(operator, vec_to_cons(&final_args)))
}

/// Expand THE: (the type-spec value-form)
/// Type specifier is NOT expanded; value form is expanded.
fn expand_the(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let type_spec = unsafe { cons_car(args) };
    let rest = unsafe { cons_cdr(args) };
    if !rest.is_cons() {
        return Ok(form);
    }
    let value_form = unsafe { cons_car(rest) };
    let expanded_value = macroexpand_all(value_form, env)?;

    if expanded_value == value_form {
        Ok(form)
    } else {
        Ok(alloc_cons(
            operator,
            alloc_cons(type_spec, alloc_cons(expanded_value, bliss_rt::value::NIL)),
        ))
    }
}

/// Expand EVAL-WHEN: (eval-when (situation...) body...)
/// Situations list is NOT expanded; body forms are expanded.
fn expand_eval_when(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let situations = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    let expanded_body = expand_body(body, env)?;

    if expanded_body == body {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, alloc_cons(situations, expanded_body)))
    }
}

/// Expand FUNCTION special form: (function name) or (function (lambda ...))
/// If the argument is a lambda expression, expand the lambda body.
/// If it's a function name, no expansion.
fn expand_function_special(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let arg = unsafe { cons_car(args) };

    // Check if the argument is a lambda expression
    if is_lambda_expression(arg) {
        let expanded_lambda = expand_lambda_expression(arg, env)?;
        if expanded_lambda == arg {
            Ok(form)
        } else {
            Ok(alloc_cons(
                operator,
                alloc_cons(expanded_lambda, bliss_rt::value::NIL),
            ))
        }
    } else {
        // (function name) — no expansion
        Ok(form)
    }
}

/// Expand a lambda expression: (lambda (params...) body...)
/// Parameters are NOT expanded (they are binding names).
/// Body is expanded in an env augmented with param bindings.
fn expand_lambda_expression(lambda: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let lambda_sym = unsafe { cons_car(lambda) }; // LAMBDA
    let rest = unsafe { cons_cdr(lambda) };
    if !rest.is_cons() {
        return Ok(lambda);
    }
    let params = unsafe { cons_car(rest) }; // parameter list
    let body = unsafe { cons_cdr(rest) }; // body forms

    // Augment environment with parameter bindings (shadow any symbol macros)
    let mut new_env = env.clone();
    let param_list = cons_to_vec(params);
    for param in &param_list {
        if param.is_symbol() && !param.is_nil() {
            // Skip lambda list keywords (&optional, &rest, &key, &body, &allow-other-keys, &aux, &whole, &environment)
            if let Some(name) = get_symbol_name(*param) {
                if name.starts_with('&') {
                    continue;
                }
            }
            new_env = new_env.augment_variable(*param, VariableInfo::Lexical);
        }
    }

    let expanded_body = expand_body(body, &new_env)?;

    if expanded_body == body {
        Ok(lambda)
    } else {
        Ok(alloc_cons(lambda_sym, alloc_cons(params, expanded_body)))
    }
}

/// Expand a lambda call: ((lambda (params...) body...) arg1 arg2 ...)
/// Expand the lambda body AND the arguments.
fn expand_lambda_call(
    operator: BlissVal,
    form: BlissVal,
    env: &Environment,
) -> Result<BlissVal, BlissError> {
    let args = unsafe { cons_cdr(form) };

    // Expand the lambda expression
    let expanded_lambda = expand_lambda_expression(operator, env)?;

    // Expand the arguments
    let expanded_args = if args.is_cons() {
        walk_cons(args, env)?
    } else {
        args
    };

    if expanded_lambda == operator && expanded_args == args {
        Ok(form)
    } else {
        Ok(alloc_cons(expanded_lambda, expanded_args))
    }
}

/// Expand LET or LET*: (let/let* ((var init)...) decl* body*)
/// Binding variable names are NOT expanded. Init forms are expanded.
/// Body is expanded in an env augmented with the new bindings (shadowing symbol macros).
/// For LET, all init-forms are expanded in the outer env.
/// For LET*, each init-form is expanded in an env augmented by prior bindings.
fn expand_let(form: BlissVal, env: &Environment, sequential: bool) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let bindings_list = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    let bindings = cons_to_vec(bindings_list);

    // Expand init-forms and collect variable names
    let mut expanded_bindings = Vec::with_capacity(bindings.len());
    let mut bindings_changed = false;
    let mut current_env = env.clone();

    for binding in &bindings {
        if binding.is_cons() {
            // (var init-form) pair
            let var = unsafe { cons_car(*binding) };
            let init_rest = unsafe { cons_cdr(*binding) };
            let init_form = if init_rest.is_cons() {
                unsafe { cons_car(init_rest) }
            } else {
                bliss_rt::value::NIL
            };

            // For LET*, expand in the progressively-augmented env
            // For LET, expand in the outer env
            let expand_env = if sequential { &current_env } else { env };
            let expanded_init = macroexpand_all(init_form, expand_env)?;

            if expanded_init != init_form {
                bindings_changed = true;
            }
            // Reconstruct binding: (var expanded-init)
            expanded_bindings.push(alloc_cons(
                var,
                alloc_cons(expanded_init, bliss_rt::value::NIL),
            ));

            // For LET*, augment env after each binding
            if sequential {
                current_env = current_env.augment_variable(var, VariableInfo::Lexical);
            }
        } else {
            // Bare symbol — (let (x) ...) means (let ((x nil)) ...)
            expanded_bindings.push(*binding);
        }
    }

    // Build augmented env for the body
    let mut body_env = if sequential { current_env } else { env.clone() };
    if !sequential {
        for binding in &bindings {
            let var = if binding.is_cons() {
                unsafe { cons_car(*binding) }
            } else {
                *binding
            };
            if var.is_symbol() && !var.is_nil() {
                body_env = body_env.augment_variable(var, VariableInfo::Lexical);
            }
        }
    }

    let expanded_body = expand_body(body, &body_env)?;

    if !bindings_changed && expanded_body == body {
        Ok(form)
    } else {
        let new_bindings = vec_to_cons(&expanded_bindings);
        Ok(alloc_cons(
            operator,
            alloc_cons(new_bindings, expanded_body),
        ))
    }
}

/// Expand FLET: (flet ((name (params) fn-body...) ...) body...)
/// Function names are NOT expanded. Function bodies are expanded in the outer env.
/// Body is expanded in an env augmented with the function bindings.
fn expand_flet(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let fn_defs = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    let defs = cons_to_vec(fn_defs);
    let mut expanded_defs = Vec::with_capacity(defs.len());
    let mut defs_changed = false;

    // FLET: function bodies are expanded in the OUTER env (not the augmented one)
    for def in &defs {
        if !def.is_cons() {
            expanded_defs.push(*def);
            continue;
        }
        let fn_name = unsafe { cons_car(*def) };
        let fn_rest = unsafe { cons_cdr(*def) };
        if !fn_rest.is_cons() {
            expanded_defs.push(*def);
            continue;
        }
        let params = unsafe { cons_car(fn_rest) };
        let fn_body = unsafe { cons_cdr(fn_rest) };

        // Augment env with params for expanding the function body
        let mut fn_env = env.clone();
        let param_list = cons_to_vec(params);
        for param in &param_list {
            if param.is_symbol() && !param.is_nil() {
                if let Some(name) = get_symbol_name(*param) {
                    if name.starts_with('&') {
                        continue;
                    }
                }
                fn_env = fn_env.augment_variable(*param, VariableInfo::Lexical);
            }
        }

        let expanded_fn_body = expand_body(fn_body, &fn_env)?;
        if expanded_fn_body != fn_body {
            defs_changed = true;
        }
        expanded_defs.push(alloc_cons(fn_name, alloc_cons(params, expanded_fn_body)));
    }

    // Augment env with function names for the body
    let mut body_env = env.clone();
    for def in &defs {
        if def.is_cons() {
            let fn_name = unsafe { cons_car(*def) };
            body_env = body_env.augment_function(fn_name, FunctionInfo::Lexical);
        }
    }

    let expanded_body = expand_body(body, &body_env)?;

    if !defs_changed && expanded_body == body {
        Ok(form)
    } else {
        let new_defs = vec_to_cons(&expanded_defs);
        Ok(alloc_cons(operator, alloc_cons(new_defs, expanded_body)))
    }
}

/// Expand LABELS: (labels ((name (params) fn-body...) ...) body...)
/// Like FLET, but function bodies ARE expanded in the augmented env
/// (functions are visible in their own bodies — recursive).
fn expand_labels(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let fn_defs = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    let defs = cons_to_vec(fn_defs);

    // LABELS: first augment env with ALL function names (recursive visibility)
    let mut augmented_env = env.clone();
    for def in &defs {
        if def.is_cons() {
            let fn_name = unsafe { cons_car(*def) };
            augmented_env = augmented_env.augment_function(fn_name, FunctionInfo::Lexical);
        }
    }

    // Now expand function bodies in the augmented env
    let mut expanded_defs = Vec::with_capacity(defs.len());
    let mut defs_changed = false;
    for def in &defs {
        if !def.is_cons() {
            expanded_defs.push(*def);
            continue;
        }
        let fn_name = unsafe { cons_car(*def) };
        let fn_rest = unsafe { cons_cdr(*def) };
        if !fn_rest.is_cons() {
            expanded_defs.push(*def);
            continue;
        }
        let params = unsafe { cons_car(fn_rest) };
        let fn_body = unsafe { cons_cdr(fn_rest) };

        // Augment with params
        let mut fn_env = augmented_env.clone();
        let param_list = cons_to_vec(params);
        for param in &param_list {
            if param.is_symbol() && !param.is_nil() {
                if let Some(name) = get_symbol_name(*param) {
                    if name.starts_with('&') {
                        continue;
                    }
                }
                fn_env = fn_env.augment_variable(*param, VariableInfo::Lexical);
            }
        }

        let expanded_fn_body = expand_body(fn_body, &fn_env)?;
        if expanded_fn_body != fn_body {
            defs_changed = true;
        }
        expanded_defs.push(alloc_cons(fn_name, alloc_cons(params, expanded_fn_body)));
    }

    let expanded_body = expand_body(body, &augmented_env)?;

    if !defs_changed && expanded_body == body {
        Ok(form)
    } else {
        let new_defs = vec_to_cons(&expanded_defs);
        Ok(alloc_cons(operator, alloc_cons(new_defs, expanded_body)))
    }
}

/// Expand LOCALLY: (locally decl* body*)
/// Declarations are processed but NOT expanded. Body is expanded.
fn expand_locally(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let operator = unsafe { cons_car(form) };
    let body = unsafe { cons_cdr(form) };

    // Skip declarations (forms starting with DECLARE), expand the rest
    let items = cons_to_vec(body);
    let mut decls = Vec::new();
    let mut body_forms = Vec::new();
    let mut in_decls = true;
    for item in &items {
        if in_decls && item.is_cons() {
            let car = unsafe { cons_car(*item) };
            if is_symbol_named(car, "DECLARE") {
                decls.push(*item);
                continue;
            }
        }
        in_decls = false;
        body_forms.push(*item);
    }

    let mut expanded_body_forms = Vec::with_capacity(body_forms.len());
    let mut changed = false;
    for bf in &body_forms {
        let exp = macroexpand_all(*bf, env)?;
        if exp != *bf {
            changed = true;
        }
        expanded_body_forms.push(exp);
    }

    if !changed {
        Ok(form)
    } else {
        let mut all_items = decls;
        all_items.extend(expanded_body_forms);
        Ok(alloc_cons(operator, vec_to_cons(&all_items)))
    }
}

/// Expand MACROLET: (macrolet ((name lambda-list macro-body...) ...) body...)
/// Install local macro definitions into a new environment.
/// Expand body in the augmented env. Strip MACROLET from output (spec §4.2.7).
fn expand_macrolet(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let macro_defs = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    // Install macro definitions in a new environment frame
    let mut augmented_env = env.clone();
    let defs = cons_to_vec(macro_defs);
    for def in &defs {
        if !def.is_cons() {
            continue;
        }
        let macro_name = unsafe { cons_car(*def) };
        let key = make_local_macrolet_expander(*def, env.clone())?;
        augmented_env = augmented_env.augment_function(macro_name, FunctionInfo::Macro(key));
    }

    // Expand body in augmented env
    let expanded_body = expand_body(body, &augmented_env)?;

    // Strip MACROLET wrapper: output as (LOCALLY expanded-body...) or
    // if single body form, just return it.
    let body_items = cons_to_vec(expanded_body);
    if body_items.len() == 1 {
        Ok(body_items[0])
    } else {
        let progn_sym = make_symbol("PROGN");
        Ok(alloc_cons(progn_sym, expanded_body))
    }
}

/// Expand SYMBOL-MACROLET: (symbol-macrolet ((sym expansion)...) body...)
/// Install symbol-macro bindings in the environment.
/// Expand body in the augmented env. Strip SYMBOL-MACROLET from output (spec §4.2.7).
fn expand_symbol_macrolet(form: BlissVal, env: &Environment) -> Result<BlissVal, BlissError> {
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let bindings_list = unsafe { cons_car(args) };
    let body = unsafe { cons_cdr(args) };

    // Install symbol-macro bindings
    let mut augmented_env = env.clone();
    let bindings = cons_to_vec(bindings_list);
    for binding in &bindings {
        if !binding.is_cons() {
            continue;
        }
        let sym = unsafe { cons_car(*binding) };
        let expansion_rest = unsafe { cons_cdr(*binding) };
        let expansion = if expansion_rest.is_cons() {
            unsafe { cons_car(expansion_rest) }
        } else {
            bliss_rt::value::NIL
        };

        // Validate: symbol-macrolet of a special variable is an error
        if let Some(VariableInfo::Special) = env.variable_information(sym) {
            return Err(BlissError::Internal(
                "SYMBOL-MACROLET: cannot define symbol macro for special variable".into(),
            ));
        }

        augmented_env = augmented_env.augment_variable(sym, VariableInfo::SymbolMacro(expansion));
    }

    // Expand body in augmented env
    let expanded_body = expand_body(body, &augmented_env)?;

    // Strip SYMBOL-MACROLET wrapper from output
    let body_items = cons_to_vec(expanded_body);
    if body_items.len() == 1 {
        Ok(body_items[0])
    } else {
        let progn_sym = make_symbol("PROGN");
        Ok(alloc_cons(progn_sym, expanded_body))
    }
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
type MacroFn = dyn Fn(BlissVal, &Environment) -> Result<BlissVal, BlissError> + Send + Sync;

fn make_local_macrolet_expander(
    def: BlissVal,
    defining_env: Environment,
) -> Result<BlissVal, BlissError> {
    let name = unsafe { cons_car(def) };
    let rest = unsafe { cons_cdr(def) };
    if !rest.is_cons() {
        return Err(BlissError::Internal(
            "MACROLET: malformed local macro definition".into(),
        ));
    }
    let params = unsafe { cons_car(rest) };
    let body = unsafe { cons_cdr(rest) };
    let parsed = parse_macro(name, params, body, Some(&defining_env))?;
    enclose(parsed, &defining_env)
}

fn expand_local_macro_call(
    whole_form: BlissVal,
    call_env: &Environment,
    defining_env: &Environment,
    params: BlissVal,
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    let arg_forms = if whole_form.is_cons() {
        unsafe { cons_cdr(whole_form) }
    } else {
        bliss_rt::value::NIL
    };
    let bindings = bind_macrolet_lambda_list(params, arg_forms)?;
    let expansion_env = defining_env.augment_environment(bindings, Vec::new(), Vec::new());
    eval_local_macro_body(body, &expansion_env, call_env)
}

fn bind_macrolet_lambda_list(
    params: BlissVal,
    args: BlissVal,
) -> Result<Vec<(BlissVal, VariableInfo)>, BlissError> {
    let params_vec = cons_to_vec(params);
    let args_vec = cons_to_vec(args);
    let mut bindings = Vec::new();
    let mut arg_i = 0usize;
    let mut rest_target: Option<BlissVal> = None;
    let mut optional_mode = false;

    let mut i = 0usize;
    while i < params_vec.len() {
        let param = params_vec[i];
        if let Some(name) = get_symbol_name(param) {
            match name.as_str() {
                "&OPTIONAL" => {
                    optional_mode = true;
                    i += 1;
                    continue;
                }
                "&REST" | "&BODY" => {
                    if i + 1 >= params_vec.len() {
                        return Err(BlissError::Internal(
                            "MACROLET: &REST requires a parameter".into(),
                        ));
                    }
                    rest_target = Some(params_vec[i + 1]);
                    break;
                }
                _ if name.starts_with('&') => {
                    return Err(BlissError::Internal(format!(
                        "MACROLET: unsupported lambda-list keyword {}",
                        name
                    )));
                }
                _ => {}
            }
        }

        let value = if arg_i < args_vec.len() {
            let arg = args_vec[arg_i];
            arg_i += 1;
            arg
        } else if optional_mode {
            bliss_rt::value::NIL
        } else {
            return Err(BlissError::Internal(
                "MACROLET: too few arguments for local macro".into(),
            ));
        };
        bindings.push((param, VariableInfo::Constant(value)));
        i += 1;
    }

    if let Some(rest) = rest_target {
        bindings.push((
            rest,
            VariableInfo::Constant(vec_to_cons(&args_vec[arg_i..])),
        ));
    } else if arg_i != args_vec.len() {
        return Err(BlissError::Internal(
            "MACROLET: too many arguments for local macro".into(),
        ));
    }

    Ok(bindings)
}

fn eval_local_macro_body(
    body: BlissVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<BlissVal, BlissError> {
    let forms = cons_to_vec(body);
    let mut result = bliss_rt::value::NIL;
    for form in forms {
        result = eval_local_macro_form(form, env, call_env)?;
    }
    Ok(result)
}

fn eval_local_macro_form(
    form: BlissVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<BlissVal, BlissError> {
    if form.is_symbol() {
        if let Some(info) = env.variable_information(form) {
            return match info {
                VariableInfo::Constant(v) | VariableInfo::SymbolMacro(v) => Ok(v),
                VariableInfo::Lexical | VariableInfo::Special => Ok(form),
            };
        }
        return Ok(form);
    }
    if !form.is_cons() {
        return Ok(form);
    }

    let operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    let op_name = get_symbol_name(operator).unwrap_or_default();
    match op_name.as_str() {
        "QUOTE" => Ok(if args.is_cons() {
            unsafe { cons_car(args) }
        } else {
            bliss_rt::value::NIL
        }),
        "LIST" => {
            let mut out = Vec::new();
            for item in cons_to_vec(args) {
                out.push(eval_local_macro_form(item, env, call_env)?);
            }
            Ok(vec_to_cons(&out))
        }
        "CONS" => {
            let items = cons_to_vec(args);
            if items.len() != 2 {
                return Err(BlissError::Internal("MACROLET: CONS expects 2 args".into()));
            }
            Ok(alloc_cons(
                eval_local_macro_form(items[0], env, call_env)?,
                eval_local_macro_form(items[1], env, call_env)?,
            ))
        }
        "APPEND" => eval_local_macro_append(args, env, call_env),
        "PROGN" => eval_local_macro_body(args, env, call_env),
        "BLISS::QUASIQUOTE" => expand_local_quasiquote(
            if args.is_cons() {
                unsafe { cons_car(args) }
            } else {
                bliss_rt::value::NIL
            },
            env,
            call_env,
        ),
        _ => {
            let (expanded, did_expand) = macroexpand_1(form, call_env)?;
            if did_expand {
                eval_local_macro_form(expanded, env, call_env)
            } else {
                Ok(form)
            }
        }
    }
}

fn eval_local_macro_append(
    args: BlissVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<BlissVal, BlissError> {
    let mut result = bliss_rt::value::NIL;
    let parts = cons_to_vec(args);
    for part in parts.into_iter().rev() {
        let mut items = cons_to_vec(eval_local_macro_form(part, env, call_env)?);
        while let Some(item) = items.pop() {
            result = alloc_cons(item, result);
        }
    }
    Ok(result)
}

fn expand_local_quasiquote(
    form: BlissVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<BlissVal, BlissError> {
    if !form.is_cons() {
        return Ok(form);
    }

    let operator = unsafe { cons_car(form) };
    if is_symbol_named(operator, "BLISS::UNQUOTE") {
        let args = unsafe { cons_cdr(form) };
        return Ok(if args.is_cons() {
            eval_local_macro_form(unsafe { cons_car(args) }, env, call_env)?
        } else {
            bliss_rt::value::NIL
        });
    }

    let mut out = Vec::new();
    let mut cursor = form;
    while cursor.is_cons() {
        let item = unsafe { cons_car(cursor) };
        if item.is_cons() && is_symbol_named(unsafe { cons_car(item) }, "BLISS::UNQUOTE-SPLICING") {
            let splice_args = unsafe { cons_cdr(item) };
            let splice_form = if splice_args.is_cons() {
                unsafe { cons_car(splice_args) }
            } else {
                bliss_rt::value::NIL
            };
            out.extend(cons_to_vec(eval_local_macro_form(
                splice_form,
                env,
                call_env,
            )?));
        } else {
            out.push(expand_local_quasiquote(item, env, call_env)?);
        }
        cursor = unsafe { cons_cdr(cursor) };
    }
    Ok(vec_to_cons(&out))
}
