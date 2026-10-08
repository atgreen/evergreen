// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Macro expansion: the compile-time lexical [`Environment`], the global,
//! compiler-macro and local macro tables, `macroexpand-1` / `macroexpand` /
//! `macroexpand-all`, and the special-operator code walker that
//! `macroexpand-all` is built on.
//!
//! Expansion runs after reading and before bytecode compilation or
//! evaluation. The evaluator in `crates/egcl` and the bytecode compiler both
//! call in here; the T2 compiler never sees unexpanded code.
//!
//! # Environment
//!
//! [`Environment`] is a chain of frames linked by `Arc` parents. Each frame
//! holds variable bindings ([`VariableInfo`]: lexical, special, constant,
//! symbol-macro), function bindings ([`FunctionInfo`]: lexical, global, macro,
//! special operator), declarations ([`DeclInfo`], including
//! [`OptimizeQualities`] and [`InlinePolicy`]), and the block and tag names in
//! scope. `augment_*` return a child frame; `variable_information`,
//! `function_information` and `declaration_information` walk the chain. The
//! environment holds `EgclVal`s that can move under GC, so it implements
//! `TraceHostRoots` and callers keep it rooted with `rooted_ref!`. The cached
//! global-macro base frame is marked `no_gc_roots` so tracing stops there
//! instead of cloning a shared `Arc` on every collection (bliss-htff).
//!
//! # Macro tables
//!
//! * The global macro table maps a symbol to an expander handle. Expanders
//!   written in Rust are registered as closures in the macro-function
//!   registry under a fresh handle (`register_macro_function`); Lisp-defined
//!   expanders are handles the host evaluator understands.
//! * Compiler macros are a separate table of Rust functions
//!   (`define_compiler_macro`); `compiler_macroexpand_1` applies one unless
//!   the operator is declared NOTINLINE in the environment.
//! * `MACROLET` definitions are parsed into [`ParsedMacro`]s and `enclose`d
//!   with their defining environment; their bodies run through a host
//!   `LocalMacroEvaluator` when one is installed, otherwise through the small
//!   built-in evaluator that understands the lambda-list binding, quasiquote,
//!   and the handful of operators macro bodies typically use.
//!
//! # Expansion
//!
//! * [`macroexpand_1`] — one step. A symbol with a symbol-macro binding
//!   expands through the hook like a macro call does. A compound form whose
//!   operator has a local macro binding, else a global macro definition, is
//!   handed to the current `*macroexpand-hook*` as `(expander form env)`.
//!   The default hook looks the expander up in the registry and calls it;
//!   for symbol macros the expander is the expansion value and the form is the
//!   original symbol. `*PACKAGE*` is snapshotted and
//!   restored around every expansion because an expander running in another
//!   package otherwise leaks its package into the caller's reads
//!   (bliss-cpm9).
//! * [`macroexpand`] — repeats `macroexpand_1` until no change, with two
//!   guards: a per-thread iteration limit (`set_macroexpand_limit`, default
//!   65 536) for divergent expansions and a hash of seen forms for circular
//!   ones.
//! * [`macroexpand_all`] — the code walker. Expands the head, then: quoted
//!   data is left alone; quasiquote templates are walked depth-aware so only
//!   unquoted sub-forms are expanded; special operators go to their
//!   `expand_*` handler (COND, BLOCK/RETURN-FROM, TAGBODY, SETQ/SETF and
//!   places, THE, EVAL-WHEN, FUNCTION, LAMBDA, LET/LET*, FLET/LABELS,
//!   LOCALLY, MACROLET/SYMBOL-MACROLET, MULTIPLE-VALUE-SETQ), which augment
//!   the environment for their bodies and strip the binding forms whose job
//!   is done; a lambda expression in operator position is expanded in place;
//!   compiler macros are tried on ordinary calls; argument forms are walked
//!   but the operator position of an ordinary call is not.
//!
//! All conses the walker builds are allocated on the GC heap through the
//! same path the reader uses and rooted across allocation, so rebuilt forms
//! survive a moving collection (bliss-noh).
//!
//! # Hooks the runtime installs
//!
//! `set_macroexpand_hook` (`*macroexpand-hook*`), `set_local_macro_evaluator`
//! (full Lisp evaluation of MACROLET bodies), and the macro-function
//! registry are per-thread or global state the `egcl` evaluator configures
//! at startup.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock};

use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{LockLevel, OrderedRwLock};
use egcl_rt::value::{TAG_CONS, TAG_MASK, EgclVal};

use crate::reader::{intern_symbol, symbol_name};

// ── Constants ─────────────────────────────────────────────────────

/// Default maximum number of macroexpand-1 iterations per macroexpand call.
/// Catches non-repeating divergent expansions.
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
    Type(u64, EgclVal),
    /// Ignore declaration for a variable.
    Ignore(u64),
    /// Ignorable declaration for a variable.
    Ignorable(u64),
    /// Special (dynamic) declaration.
    Dynamic(u64),
    /// Custom implementation-specific declaration.
    Custom(u64, EgclVal),
}

// ── Environment protocol ───────────────────────────────────────────

/// Lexical environment for macro expansion.
///
/// Uses a parent chain for nested lexical scopes: each augmentation creates
/// a new frame with only the new bindings, pointing to the previous environment
/// as parent. Lookups walk up the chain.
#[derive(Clone, Debug)]
pub struct Environment {
    /// Parent environment (lexical chain).
    parent: Option<Arc<Environment>>,
    /// Variable bindings keyed by EgclVal.0 (raw u64).
    variables: HashMap<u64, VariableInfo>,
    /// Function bindings keyed by EgclVal.0 (raw u64).
    functions: HashMap<u64, FunctionInfo>,
    /// Active declarations.
    declarations: Vec<DeclInfo>,
    /// True when this level and everything above it need neither relocation
    /// nor symbol liveness marking, so `visit_gc_roots` may stop here.
    ///
    /// Set only for the cached global-macro base, whose function map holds
    /// immediate macro handles (`from_macro_handle`) and immortal symbol keys.
    /// It matters because descending
    /// into a parent uses `Arc::make_mut`, which CLONES a shared Arc: without
    /// this, sharing the base would make every GC trace copy it, which is worse
    /// than the per-expansion clone it replaces (bliss-htff).
    no_gc_roots: bool,
    /// Block names in scope (for RETURN-FROM), keyed by EgclVal.0.
    blocks: HashSet<u64>,
    /// Tag names in scope (for GO), keyed by EgclVal.0.
    tags: HashSet<u64>,
}

/// Rooting support (bliss-yab): lets `rooted_ref!` keep a transient compiler
/// `Environment` scanned in place, replacing `ExpansionEnvGuard`'s registry.
impl egcl_rt::gc::TraceHostRoots for Environment {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.visit_gc_roots(visit);
    }
}

/// Information about a variable binding.
#[derive(Clone, Debug)]
pub enum VariableInfo {
    /// Lexical variable.
    Lexical,
    /// Special (dynamic) variable.
    Special,
    /// Constant.
    Constant(EgclVal),
    /// Symbol macro.
    SymbolMacro(EgclVal),
}

/// Information about a function binding.
#[derive(Clone, Debug)]
pub enum FunctionInfo {
    /// Lexical function (from FLET/LABELS).
    Lexical,
    /// Global function.
    Global,
    /// Macro.
    Macro(EgclVal),
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
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Yield every value retained by this compiler environment.
    ///
    /// Symbol keys are immediate, but still keep their backing objects alive.
    /// Binding/declaration payloads can also point into the moving heap.
    /// `Arc::make_mut` safely gives this stored environment its own parent chain
    /// when another environment shares a frame.
    pub fn visit_gc_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.visit_own_gc_roots(visit);
        // Iterative, like every other parent walk here: the chain is one frame
        // per augmentation and a deep one would otherwise overflow the control
        // stack DURING A COLLECTION (bliss-ump7).
        //
        // `Arc::make_mut` clones a SHARED parent, so descending into the shared
        // global-macro base would copy it on every collection. That base holds
        // only immediate handles, so there is nothing to visit.
        let mut frame: &mut Environment = self;
        loop {
            let descend = match frame.parent {
                Some(ref parent) => !parent.no_gc_roots,
                None => false,
            };
            if !descend {
                return;
            }
            let parent = frame.parent.as_mut().expect("checked above");
            frame = Arc::make_mut(parent);
            frame.visit_own_gc_roots(visit);
        }
    }

    /// Visit this frame's own reference slots, without descending.
    fn visit_own_gc_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for &key in self
            .variables
            .keys()
            .chain(self.functions.keys())
            .chain(self.blocks.iter())
            .chain(self.tags.iter())
        {
            visit(&mut EgclVal(key));
        }
        for info in self.variables.values_mut() {
            match info {
                VariableInfo::Constant(value) | VariableInfo::SymbolMacro(value) => visit(value),
                VariableInfo::Lexical | VariableInfo::Special => {}
            }
        }
        for info in self.functions.values_mut() {
            if let FunctionInfo::Macro(value) = info {
                visit(value);
            }
        }
        for declaration in &mut self.declarations {
            match declaration {
                DeclInfo::Declaration(key)
                | DeclInfo::Type(key, _)
                | DeclInfo::Ignore(key)
                | DeclInfo::Ignorable(key)
                | DeclInfo::Dynamic(key)
                | DeclInfo::Custom(key, _) => {
                    visit(&mut EgclVal(*key));
                }
                DeclInfo::Optimize(_) => {}
            }
            match declaration {
                DeclInfo::Type(_, value) | DeclInfo::Custom(_, value) => visit(value),
                DeclInfo::Optimize(_)
                | DeclInfo::Declaration(_)
                | DeclInfo::Ignore(_)
                | DeclInfo::Ignorable(_)
                | DeclInfo::Dynamic(_) => {}
            }
        }
    }

    /// Query variable information (CLtL2 `variable-information`).
    /// Walks the parent chain to find the binding in the nearest enclosing scope.
    ///
    /// Every parent walk in this impl is ITERATIVE. The chain is one frame per
    /// augmentation, so a code walker that augments per binding builds a very
    /// long one: Serapeum's LOCAL did, and the recursive walk exhausted the
    /// control stack while its file was compiled (bliss-ump7).
    pub fn variable_information(&self, name: EgclVal) -> Option<VariableInfo> {
        let mut frame = self;
        loop {
            if let Some(info) = frame.variables.get(&name.0) {
                return Some(info.clone());
            }
            match frame.parent {
                Some(ref parent) => frame = parent,
                None => return None,
            }
        }
    }

    /// Query function information (CLtL2 `function-information`).
    /// Walks the parent chain to find the binding in the nearest enclosing scope.
    pub fn function_information(&self, name: EgclVal) -> Option<FunctionInfo> {
        let mut frame = self;
        loop {
            if let Some(info) = frame.functions.get(&name.0) {
                return Some(info.clone());
            }
            match frame.parent {
                Some(ref parent) => frame = parent,
                None => return None,
            }
        }
    }

    /// Query declaration information (CLtL2 `declaration-information`).
    /// Supports standard queries:
    /// - For 'optimize': returns optimize qualities as a EgclVal encoding.
    /// - For 'declaration': returns list of valid declaration names.
    ///   Walks the parent chain to find declarations.
    pub fn declaration_information(&self, decl_name: EgclVal) -> Option<EgclVal> {
        let mut frame = self;
        loop {
            if let Some(found) = frame.declaration_information_here(decl_name) {
                return Some(found);
            }
            match frame.parent {
                Some(ref parent) => frame = parent,
                None => return None,
            }
        }
    }

    /// The `declaration_information` answer from THIS frame alone.
    fn declaration_information_here(&self, decl_name: EgclVal) -> Option<EgclVal> {
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
                    return Some(EgclVal::from_fixnum(packed));
                }
                DeclInfo::Declaration(name_key) => {
                    if *name_key == decl_name.0 {
                        return Some(egcl_rt::value::T);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Return the nearest raw declaration specifier named `decl_name`.
    ///
    /// This is the lossless bridge used by implementation-level CLtL2
    /// adapters: a declaration handler needs the complete source specifier,
    /// not the compiler's normalized representation.  Search entries in
    /// reverse source order within a frame, then walk outward, so the nearest
    /// lexical declaration has precedence.
    pub fn declaration_specifier(&self, decl_name: EgclVal) -> Option<EgclVal> {
        let mut frame = self;
        loop {
            for decl in frame.declarations.iter().rev() {
                if let DeclInfo::Custom(name_key, specifier) = decl
                    && *name_key == decl_name.0
                {
                    return Some(*specifier);
                }
            }
            match frame.parent {
                Some(ref parent) => frame = parent,
                None => return None,
            }
        }
    }

    /// Every raw declaration specifier named `decl_name` in force here,
    /// OUTERMOST first. `declaration_specifier` answers the innermost one, which
    /// is what a shadowing declaration needs; a *cumulative* declaration such as
    /// OPTIMIZE needs them all, so its reader can merge outward-to-inward.
    pub fn declaration_specifiers(&self, decl_name: EgclVal) -> Vec<EgclVal> {
        // Collect innermost-first, then reverse: the caller wants outermost-first.
        let mut inward = Vec::new();
        let mut frame = self;
        loop {
            let start = inward.len();
            frame.declaration_specifiers_here(decl_name, &mut inward);
            inward[start..].reverse();
            match frame.parent {
                Some(ref parent) => frame = parent,
                None => break,
            }
        }
        inward.reverse();
        inward
    }

    /// The specifiers named `decl_name` in THIS frame, in source order.
    fn declaration_specifiers_here(&self, decl_name: EgclVal, found: &mut Vec<EgclVal>) {
        for decl in self.declarations.iter() {
            if let DeclInfo::Custom(name_key, specifier) = decl
                && *name_key == decl_name.0
            {
                found.push(*specifier);
            }
        }
    }

    /// Augment this environment with a variable binding.
    /// Returns a new Environment frame with only the new binding; the current
    /// environment becomes the parent (O(1) per augmentation via parent chain).
    pub fn augment_variable(&self, name: EgclVal, info: VariableInfo) -> Environment {
        let mut variables = HashMap::new();
        variables.insert(name.0, info);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables,
            functions: HashMap::new(),
            declarations: Vec::new(),
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Augment this environment with a function binding.
    /// Returns a new Environment frame with only the new binding; the current
    /// environment becomes the parent.
    pub fn augment_function(&self, name: EgclVal, info: FunctionInfo) -> Environment {
        let mut functions = HashMap::new();
        functions.insert(name.0, info);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions,
            declarations: Vec::new(),
            no_gc_roots: false,
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
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Augment with a block name in scope.
    pub fn augment_block(&self, block_name: EgclVal) -> Environment {
        let mut blocks = HashSet::new();
        blocks.insert(block_name.0);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: Vec::new(),
            no_gc_roots: false,
            blocks,
            tags: HashSet::new(),
        }
    }

    /// Augment with a tag name in scope.
    pub fn augment_tag(&self, tag_name: EgclVal) -> Environment {
        let mut tags = HashSet::new();
        tags.insert(tag_name.0);
        Environment {
            parent: Some(Arc::new(self.clone())),
            variables: HashMap::new(),
            functions: HashMap::new(),
            declarations: Vec::new(),
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags,
        }
    }

    /// General-purpose augment-environment (CLtL2 compatible).
    /// Creates a new environment augmented with given bindings and declarations.
    /// Skip scanning this environment through shared children only when its
    /// values need neither relocation nor liveness marking. Uninterned symbols
    /// are immediate values, but their backing objects are collectible.
    pub fn mark_no_gc_roots(mut self) -> Environment {
        let mut needs_roots = false;
        self.visit_gc_roots(&mut |slot| {
            let value = unsafe { *slot };
            needs_roots |= value.is_cons()
                || value.is_heap_object()
                || value
                    .symbol_index()
                    .is_some_and(egcl_rt::symbols::is_uninterned);
        });
        self.no_gc_roots = !needs_roots;
        self
    }

    /// A child of an already-shared parent.
    ///
    /// `augment_environment` builds its parent as `Arc::new(self.clone())`,
    /// which copies a whole level even though `parent` is an `Arc` precisely so
    /// levels can be SHARED. Callers that already hold the parent behind an
    /// `Arc` should use this instead: it costs one refcount bump.
    pub fn child_of(
        parent: Arc<Environment>,
        variables: Vec<(EgclVal, VariableInfo)>,
        functions: Vec<(EgclVal, FunctionInfo)>,
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
            parent: Some(parent),
            variables: var_map,
            functions: fn_map,
            declarations,
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    pub fn augment_environment(
        &self,
        variables: Vec<(EgclVal, VariableInfo)>,
        functions: Vec<(EgclVal, FunctionInfo)>,
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
            no_gc_roots: false,
            blocks: HashSet::new(),
            tags: HashSet::new(),
        }
    }

    /// Check if a function has a notinline declaration in this environment.
    pub fn is_notinline(&self, name: EgclVal) -> bool {
        for decl in &self.declarations {
            if let DeclInfo::Custom(key, val) = decl {
                // Convention: notinline stored as Custom with a sentinel
                if *key == name.0 && val.0 == NOTINLINE_SENTINEL {
                    return true;
                }
            }
        }
        if let Some(ref parent) = self.parent {
            let mut frame = parent.as_ref();
            loop {
                for decl in &frame.declarations {
                    if let DeclInfo::Custom(key, val) = decl
                        && *key == name.0
                        && val.0 == NOTINLINE_SENTINEL
                    {
                        return true;
                    }
                }
                match frame.parent {
                    Some(ref next) => frame = next,
                    None => return false,
                }
            }
        }
        false
    }
}

/// Sentinel value used to mark notinline declarations in DeclInfo::Custom.
const NOTINLINE_SENTINEL: u64 = 0xFFFF_FFFF_DEAD_BEEF;

// ── Global macro table ────────────────────────────────────────────

/// Global macro table: maps operator symbol keys to expander EgclVals.
/// Protected by RwLock for concurrent compilation.
static GLOBAL_MACRO_TABLE: LazyLock<OrderedRwLock<HashMap<u64, EgclVal>>> = LazyLock::new(|| {
    OrderedRwLock::new(
        LockLevel::GcWorld,
        7,
        "GC-rooted global macro table",
        HashMap::new(),
    )
});

fn scan_global_macro_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut table = GLOBAL_MACRO_TABLE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (&key, expander) in table.iter_mut() {
        visit(&mut EgclVal(key));
        visit(expander);
    }
}

fn install_global_macro_root_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_global_macro_roots));
}

/// Global compiler macro table: maps function name keys to compiler macro
/// expander functions. Protected by RwLock.
/// The expander takes (form, env) and returns either a replacement form
/// or the original form (to decline).
static COMPILER_MACRO_TABLE: LazyLock<OrderedRwLock<HashMap<u64, CompilerMacroFn>>> =
    LazyLock::new(|| {
        OrderedRwLock::new(
            LockLevel::CodeCache,
            10,
            "compiler macro table",
            HashMap::new(),
        )
    });

/// Compiler macro function type.
/// Takes (whole_form, env) and returns either a transformed form or the
/// original `whole_form` (pointer-equal) to decline the transformation.
pub type CompilerMacroFn =
    Arc<dyn Fn(EgclVal, &Environment) -> Result<EgclVal, EgclError> + Send + Sync>;

/// Parsed macro lambda-expression used by `parse_macro`/`enclose`.
#[derive(Clone, Debug)]
pub struct ParsedMacro {
    name: EgclVal,
    lambda_list: EgclVal,
    body: EgclVal,
}

/// Register a global macro (DEFMACRO).
pub fn define_global_macro(name: EgclVal, expander: EgclVal) {
    install_global_macro_root_scanner();
    let mut table = GLOBAL_MACRO_TABLE.write().unwrap();
    table.insert(name.0, expander);
}

/// Remove a global macro.
pub fn undefine_global_macro(name: EgclVal) {
    let mut table = GLOBAL_MACRO_TABLE.write().unwrap();
    table.remove(&name.0);
}

/// Look up a global macro by name.
fn lookup_global_macro(name: EgclVal) -> Option<EgclVal> {
    let table = GLOBAL_MACRO_TABLE.read().unwrap();
    table.get(&name.0).copied()
}

/// Cheap "are there ANY compiler macros defined?" gate. The tree-walker
/// consults compiler macros on every global function call (so interpreted code
/// gives the same results as compiled code — bliss-x5y.24); this AtomicBool lets
/// the overwhelmingly common no-compiler-macro program skip the table read-lock
/// entirely. Set the first time a compiler macro is defined; never cleared (the
/// cost of a spurious read-lock after every compiler macro is undefined is
/// negligible and clearing it racily would be wrong).
static ANY_COMPILER_MACROS: AtomicBool = AtomicBool::new(false);

/// True if at least one compiler macro has ever been defined. A `false` return
/// is authoritative (no lock needed); a `true` return means callers should
/// consult [`has_compiler_macro`] / [`compiler_macroexpand_1`].
pub fn any_compiler_macros() -> bool {
    ANY_COMPILER_MACROS.load(AtomicOrdering::Relaxed)
}

/// True if `name` currently has a compiler macro registered. Cheap-gated by
/// [`any_compiler_macros`] so the common case pays a single relaxed load.
pub fn has_compiler_macro(name: EgclVal) -> bool {
    if !any_compiler_macros() {
        return false;
    }
    let table = COMPILER_MACRO_TABLE.read().unwrap();
    table.contains_key(&name.0)
}

/// Register a compiler macro (DEFINE-COMPILER-MACRO).
pub fn define_compiler_macro(name: EgclVal, expander: CompilerMacroFn) {
    ANY_COMPILER_MACROS.store(true, AtomicOrdering::Relaxed);
    let mut table = COMPILER_MACRO_TABLE.write().unwrap();
    table.insert(name.0, expander);
}

/// Remove a compiler macro.
pub fn undefine_compiler_macro(name: EgclVal) {
    let mut table = COMPILER_MACRO_TABLE.write().unwrap();
    table.remove(&name.0);
}

/// Discard process-local compiler macro callbacks before registering a restored
/// image's definitions. The cheap presence flag may safely remain conservative.
pub fn clear_compiler_macros() {
    COMPILER_MACRO_TABLE.write().unwrap().clear();
}

/// Look up a compiler macro by name.
fn lookup_compiler_macro(name: EgclVal) -> Option<CompilerMacroFn> {
    let table = COMPILER_MACRO_TABLE.read().unwrap();
    table.get(&name.0).cloned()
}

/// Apply one compiler-macro expander to an ordinary call form. This is the
/// non-recursive entry used by T0 lowering: it reports whether the expander
/// accepted by returning a non-EQ form, leaving recursive expansion/lowering
/// to the caller.
pub fn compiler_macroexpand_1(
    mut form: EgclVal,
    env: &Environment,
) -> Result<(EgclVal, bool), EgclError> {
    // The expander may allocate and relocate FORM. Besides keeping the call
    // live, updating this local is essential for the EQ decline check below:
    // comparing against its stale nursery address can mistake a newly allocated
    // expansion that reuses the old slot for the unchanged original form.
    egcl_rt::rooted_ref!(_form_root = &mut form);
    if !form.is_cons() {
        return Ok((form, false));
    }
    let operator = unsafe { cons_car(form) };
    if !operator.is_symbol() || env.is_notinline(operator) {
        return Ok((form, false));
    }
    let Some(expander) = lookup_compiler_macro(operator) else {
        return Ok((form, false));
    };
    let expanded = expander(form, env)?;
    Ok((expanded, expanded.0 != form.0))
}

// ── Macro function invocation registry ────────────────────────────

/// Registry mapping EgclVal expander identities to callable Rust functions.
/// This enables the default_hook (funcall) to actually invoke macro expanders
/// that are represented as EgclVal handles.
static MACRO_FUNCTION_REGISTRY: LazyLock<OrderedRwLock<HashMap<u64, Arc<MacroFn>>>> =
    LazyLock::new(|| {
        OrderedRwLock::new(
            LockLevel::CodeCache,
            11,
            "macro function registry",
            HashMap::new(),
        )
    });

static MACRO_FUNCTION_KEY_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Register a macro expander function that can be invoked by the default hook.
/// The `key` is the EgclVal that appears as FunctionInfo::Macro(key).
pub fn register_macro_function(key: EgclVal, func: Arc<MacroFn>) {
    let mut registry = MACRO_FUNCTION_REGISTRY.write().unwrap();
    registry.insert(key.0, func);
}

/// Release an expander registration proven unreachable by image delivery.
pub fn unregister_macro_function(key: EgclVal) {
    MACRO_FUNCTION_REGISTRY.write().unwrap().remove(&key.0);
}

/// Mint a fresh, process-unique key for `MACRO_FUNCTION_REGISTRY`.
///
/// This is the SINGLE source of truth for registry keys. Both macrolet-local
/// expanders (via [`enclose`]) and the interpreter's global-macro handles
/// (cli's `next_macro_function_handle`) must draw from this one counter:
/// the registry is keyed by `key.0`, so two independent fixnum counters would
/// mint colliding keys and one macro's expander would silently overwrite
/// another's (bliss-6b2 — a macrolet-local `check` clobbering global `defvar`,
/// producing an intermittently wrong `defvar` expansion during asdf load).
pub fn next_registered_macro_key() -> EgclVal {
    // Mint a SPECIAL-tagged macro handle, NOT a bare fixnum: the registry is
    // keyed by `key.0`, and `default_hook` looks up its `expander` argument
    // there, so a fixnum key could be aliased by any ordinary literal whose
    // integer value collided with it (bliss-skx: a fixnum symbol-macro
    // expansion misread as a macro handle → wrong expander; same family as
    // bliss-6b2). `from_macro_handle` puts keys on a tag no literal — and no
    // CLOS meta handle — can occupy, making the conflation impossible.
    EgclVal::from_macro_handle(
        MACRO_FUNCTION_KEY_COUNTER.fetch_add(1, AtomicOrdering::Relaxed) as i64,
    )
}

/// Parse a macro definition into a lambda-expression suitable for `enclose`.
pub fn parse_macro(
    name: EgclVal,
    lambda_list: EgclVal,
    body: EgclVal,
    _env: Option<&Environment>,
) -> Result<ParsedMacro, EgclError> {
    if !name.is_symbol() {
        return Err(EgclError::Internal(
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
/// A MACROLET local-macro expander's captured state, held where the GC can
/// find it. The `lambda_list`/`body` are reader-built forms and `defining_env`
/// carries movable binding payloads; a relocating minor GC moves them while the
/// expander sits registered, so capturing them by value inside the `Arc` closure
/// would leave stale pointers that corrupt every later expansion of the macro
/// (bliss-noh — observed as wrong or cyclic macrolet expansions under
/// EGCL_GC_STRESS). The closure re-reads the GC-updated values from this table
/// by key at call time instead.
struct MacroletCapture {
    lambda_list: EgclVal,
    body: EgclVal,
    defining_env: Environment,
}

static MACROLET_CAPTURE_TABLE: LazyLock<OrderedRwLock<HashMap<u64, MacroletCapture>>> =
    LazyLock::new(|| {
        OrderedRwLock::new(
            LockLevel::GcWorld,
            8,
            "GC-rooted macrolet expander captures",
            HashMap::new(),
        )
    });

fn scan_macrolet_capture_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut table = MACROLET_CAPTURE_TABLE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for capture in table.values_mut() {
        visit(&mut capture.lambda_list);
        visit(&mut capture.body);
        capture.defining_env.visit_gc_roots(visit);
    }
}

fn install_macrolet_capture_root_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_macrolet_capture_roots));
}

pub fn enclose(parsed: ParsedMacro, env: &Environment) -> Result<EgclVal, EgclError> {
    let key = next_registered_macro_key();
    let _ = parsed.name;
    install_macrolet_capture_root_scanner();
    MACROLET_CAPTURE_TABLE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            key.0,
            MacroletCapture {
                lambda_list: parsed.lambda_list,
                body: parsed.body,
                defining_env: env.clone(),
            },
        );
    let func = Arc::new(move |whole_form: EgclVal, call_env: &Environment| {
        // Re-read the GC-updated capture rather than a stale by-value copy
        // (bliss-noh). No `alloc_typed` runs while the read lock is held (the
        // Environment clone uses the Rust allocator, which never fires the
        // egcl collector), so the GC scanner's write lock cannot deadlock here.
        let (lambda_list, body, defining_env) = {
            let table = MACROLET_CAPTURE_TABLE
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let capture = table.get(&key.0).ok_or_else(|| {
                EgclError::Internal("MACROLET: local macro expander capture vanished".into())
            })?;
            (
                capture.lambda_list,
                capture.body,
                capture.defining_env.clone(),
            )
        };
        expand_local_macro_call(whole_form, call_env, &defining_env, lambda_list, body)
    });
    register_macro_function(key, func);
    Ok(key)
}

/// Look up a registered macro function by its EgclVal identity.
fn lookup_macro_function(key: EgclVal) -> Option<Arc<MacroFn>> {
    let registry = MACRO_FUNCTION_REGISTRY.read().unwrap();
    registry.get(&key.0).cloned()
}

// ── Expansion hook ─────────────────────────────────────────────────

/// The type of `*macroexpand-hook*`.
/// Signature: `(expander form env) -> expanded_form`.
pub type MacroexpandHook = fn(EgclVal, EgclVal, &Environment) -> Result<EgclVal, EgclError>;

/// Default hook: implements `funcall` semantics.
///
/// For function macros (FunctionInfo::Macro), the expander is a EgclVal handle.
/// The hook looks up the registered Rust-side callable in MACRO_FUNCTION_REGISTRY
/// and invokes it with (form, env). If no callable is registered, it falls back
/// to returning the expander value — this supports symbol macros where the
/// expander IS the expansion value.
///
/// For full funcall semantics with arbitrary Lisp functions, the runtime must
/// register each macro's expander via `register_macro_function`.
fn default_hook(
    expander: EgclVal,
    form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    invoke_macro_expander(expander, form, env)
}

/// Invoke an expansion function without consulting the expansion hook again.
/// Hosts use this when a Lisp hook calls the expander it was handed.
pub fn invoke_macro_expander(
    expander: EgclVal,
    form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Try to look up and invoke the expander as a registered macro function.
    // This implements true funcall semantics: (funcall expander form env).
    if let Some(func) = lookup_macro_function(expander) {
        return func(form, env);
    }
    // Symbol macros carry their expansion directly rather than a registry key.
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

/// A cons cell is a headerless 16-byte pair: [car: EgclVal, cdr: EgclVal].
/// The cons-tagged pointer (tag 001) points to the first byte of the pair.
#[repr(C)]
struct ConsCell {
    car: EgclVal,
    cdr: EgclVal,
}

/// Extract the CAR of a cons cell.
///
/// # Safety
/// Caller must ensure `val` is a valid cons-tagged `EgclVal` (tag `001`)
/// whose underlying pointer refers to a live, properly-aligned cons cell.
unsafe fn cons_car(val: EgclVal) -> EgclVal {
    debug_assert_eq!(val.0 & TAG_MASK, TAG_CONS, "cons_car called on non-cons");
    let ptr = (val.0 & !TAG_MASK) as *const ConsCell;
    unsafe { (*ptr).car }
}

/// Extract the CDR of a cons cell.
///
/// # Safety
/// Same safety requirements as `cons_car`.
unsafe fn cons_cdr(val: EgclVal) -> EgclVal {
    debug_assert_eq!(val.0 & TAG_MASK, TAG_CONS, "cons_cdr called on non-cons");
    let ptr = (val.0 & !TAG_MASK) as *const ConsCell;
    unsafe { (*ptr).cdr }
}

/// Allocate a new cons cell with the given car and cdr on the shared GC heap.
/// Returns a cons-tagged EgclVal pointing to the new cell.
///
/// Uses the same allocation path and layout as the reader and the T0 evaluator
/// (a headered CONS object whose body is `[car@0, cdr@8]`, which `from_cons_ptr`
/// points at), so the rebuilt-form spines this module builds are traceable by
/// the collector rather than leaked Rust boxes with fields that rot after a
/// moving GC (bliss-noh; the earlier `Box::into_raw` cells silently corrupted
/// macrolet expansions under EGCL_GC_STRESS). Roots `car`/`cdr` across the
/// allocation, which can itself fire a relocating minor GC.
fn alloc_cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
    egcl_rt::rooted!(car = car);
    egcl_rt::rooted!(cdr = cdr);
    let body = match egcl_rt::gc::alloc_typed(16, egcl_rt::object::type_id::CONS) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(std::alloc::Layout::new::<ConsCell>()),
    };
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = *car;
        (*cell).cdr = *cdr;
        EgclVal::from_cons_ptr(body)
    }
}

// ── Expansion functions ────────────────────────────────────────────

/// Perform one step of macro expansion (CLHS `macroexpand-1`).
/// Returns `(expanded_form, expanded_p)`.
///
/// If form has a SymbolMacro binding in env, returns the expansion value.
///
/// If form is a cons whose car is a symbol with a Macro function binding,
/// invokes the macroexpand hook with (expander, form, env) and returns
/// (result, true).
///
/// If the operator is not found in the local environment, consults the
/// global macro table.
///
/// Otherwise returns (form, false).
/// Restores the `*PACKAGE*` value cell when dropped. Macro expansion must be
/// *PACKAGE*-neutral, but a macro expander runs in a fresh expansion env whose
/// (CL-USER) package leaks into the global cell — so expanding a macro defined in
/// another package silently clobbered the caller's *PACKAGE*, breaking subsequent
/// reads and nested LOADs (bliss-cpm9: a DEFTEST macroexpand in :cl-test left
/// *PACKAGE* = CL-USER, so a following `(load "cons.lsp")` read DEFTEST as an
/// undefined function). Snapshot the cell around every expansion and restore it.
struct PackageCellGuard {
    sym: Option<u32>,
    saved: Option<EgclVal>,
}
impl Drop for PackageCellGuard {
    fn drop(&mut self) {
        if let (Some(sym), Some(saved)) = (self.sym, self.saved) {
            egcl_rt::symbols::set_symbol_value(sym, saved);
        }
    }
}

pub fn macroexpand_1(
    mut form: EgclVal,
    env: &Environment,
) -> Result<(EgclVal, bool), EgclError> {
    let _pkg_guard = {
        let sym = egcl_rt::symbols::find_index("*PACKAGE*");
        let saved = sym.and_then(egcl_rt::symbols::symbol_value);
        PackageCellGuard { sym, saved }
    };
    egcl_rt::rooted_ref!(_form_root = &mut form);
    // Symbol macro hooks receive the original symbol, just as ordinary macro
    // hooks receive the original call. The host reifies the expansion value as
    // a callable constant expander before handing it to a Lisp hook.
    if let Some(VariableInfo::SymbolMacro(mut expansion)) = env.variable_information(form) {
        egcl_rt::rooted_ref!(_expansion_root = &mut expansion);
        let hook = get_macroexpand_hook();
        // Fast-path the default hook: its result for a symbol macro is just the
        // expansion (the expander IS the value), so return it directly and skip
        // the macro-function-registry lock. Correctness here no longer depends on
        // this shortcut — macro keys are now SPECIAL-tagged handles that no fixnum
        // expansion can alias (bliss-skx), so even routed through default_hook a
        // fixnum expansion would fall through the registry miss to `Ok(expander)`.
        // This is purely an allocation/lock elision. A CUSTOM hook is still
        // invoked with the original form so it can transform symbol-macro expansions
        // (CLHS: *macroexpand-hook* mediates symbol-macro expansion too; bliss-ms0).
        if hook as usize == (default_hook as MacroexpandHook) as usize {
            return Ok((expansion, true));
        }
        let result = hook(expansion, form, env)?;
        return Ok((result, true));
    }

    // 2. Check if form is a cons with a macro operator
    if form.is_cons() {
        let mut operator = unsafe { cons_car(form) };
        egcl_rt::rooted_ref!(_operator_root = &mut operator);

        // 2.b: Look up operator in local environment
        match env.function_information(operator) {
            Some(FunctionInfo::Macro(mut expander)) => {
                egcl_rt::rooted_ref!(_expander_root = &mut expander);
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
                if let Some(mut expander) = lookup_global_macro(operator) {
                    egcl_rt::rooted_ref!(_expander_root = &mut expander);
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
/// Detects circular expansion by tracking seen forms.
/// Enforces *macroexpand-limit* iteration cap (default 65536).
pub fn macroexpand(form: EgclVal, env: &Environment) -> Result<(EgclVal, bool), EgclError> {
    macroexpand_until(form, env, |_| false)
}

/// Expand with the usual hook, cycle detection, and iteration limit, stopping
/// before a caller-owned syntactic protocol takes precedence over a macro.
pub fn macroexpand_until(
    form: EgclVal,
    env: &Environment,
    stop: impl Fn(EgclVal) -> bool,
) -> Result<(EgclVal, bool), EgclError> {
    let mut current = form;
    egcl_rt::rooted_ref!(_current_root = &mut current);
    let mut ever_expanded = false;
    let mut seen = HashSet::new();
    let mut iteration_count: usize = 0;
    let limit = get_macroexpand_limit();

    // Insert the original form to detect self-referential expansions
    seen.insert(structural_fingerprint(current, 8));

    loop {
        if stop(current) {
            return Ok((current, ever_expanded));
        }
        let (expanded, did_expand) = macroexpand_1(current, env)?;
        if !did_expand {
            return Ok((current, ever_expanded));
        }
        ever_expanded = true;
        iteration_count += 1;

        // Check iteration limit
        if iteration_count > limit {
            return Err(EgclError::Internal(format!(
                "Macro expansion limit ({}) exceeded",
                limit
            )));
        }

        // Check for circular expansion
        if !seen.insert(structural_fingerprint(expanded, 8)) {
            return Err(EgclError::Internal("circular macro expansion".into()));
        }

        current = expanded;
    }
}

/// Get the name of a symbol, if it is one (and not NIL or T).
fn get_symbol_name(val: EgclVal) -> Option<String> {
    if !val.is_symbol() || val.is_nil() || val.0 == egcl_rt::value::T.0 {
        return None;
    }
    let idx = val.as_symbol_index();
    symbol_name(idx)
}

/// Check if a EgclVal is a symbol with the given name.
fn is_symbol_named(val: EgclVal, name: &str) -> bool {
    match get_symbol_name(val) {
        Some(n) => n == name,
        None => false,
    }
}

/// Intern a CL symbol name and return it as a EgclVal.
fn make_symbol(name: &str) -> EgclVal {
    EgclVal::from_symbol_index(intern_symbol(name))
}

/// Collect cons list elements into a Vec (proper list only).
fn cons_to_vec(form: EgclVal) -> Vec<EgclVal> {
    let mut result = Vec::new();
    let mut current = form;
    while current.is_cons() {
        result.push(unsafe { cons_car(current) });
        current = unsafe { cons_cdr(current) };
    }
    result
}

/// Build a proper cons list from a slice.
fn vec_to_cons(items: &[EgclVal]) -> EgclVal {
    let mut result = egcl_rt::value::NIL;
    for item in items.iter().rev() {
        result = alloc_cons(*item, result);
    }
    result
}

fn structural_fingerprint(form: EgclVal, depth: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    fingerprint_into(form, depth, &mut hasher);
    hasher.finish()
}

fn fingerprint_into(form: EgclVal, depth: usize, hasher: &mut DefaultHasher) {
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
fn expand_body(mut forms: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_forms_root = &mut forms);
    egcl_rt::rooted!(items = cons_to_vec(forms));
    egcl_rt::rooted!(expanded_items = Vec::with_capacity(items.len()));

    // A declaration governs every form in the containing body.  Preserve the
    // DECLARE forms themselves, but augment the macro-expansion environment
    // before expanding the executable forms so an &ENVIRONMENT parameter sees
    // the same lexical declarations as the compiler.  The optional leading
    // string accounts for function/macro docstrings.
    let (declaration_end, declarations) = collect_body_declarations(&items);
    let changed = if declarations.is_empty() {
        expand_body_items(&items, declaration_end, env, &mut expanded_items)?
    } else {
        let mut body_env = env.augment_declarations(declarations);
        // Custom declaration payloads are raw source conses.  They must move
        // with the nursery while a later macro expansion allocates.
        egcl_rt::rooted_ref!(_body_env_root = &mut body_env);
        expand_body_items(&items, declaration_end, &body_env, &mut expanded_items)?
    };
    if !changed {
        Ok(forms)
    } else {
        Ok(vec_to_cons(&expanded_items))
    }
}

/// Collect the leading declaration specifiers in a body and return the index
/// of its first executable form.  Every specifier is retained verbatim as a
/// `Custom` declaration; implementation compatibility layers decide how to
/// interpret it.
fn collect_body_declarations(items: &[EgclVal]) -> (usize, Vec<DeclInfo>) {
    let mut index = usize::from(items.first().is_some_and(|form| form.is_string()));
    let mut declarations = Vec::new();

    while let Some(&form) = items.get(index) {
        if !form.is_cons() || !is_symbol_named(unsafe { cons_car(form) }, "DECLARE") {
            break;
        }
        let mut specifiers = unsafe { cons_cdr(form) };
        while specifiers.is_cons() {
            let specifier = unsafe { cons_car(specifiers) };
            if specifier.is_cons() {
                let name = unsafe { cons_car(specifier) };
                if name.is_symbol() {
                    declarations.push(DeclInfo::Custom(name.0, specifier));
                }
            }
            specifiers = unsafe { cons_cdr(specifiers) };
        }
        index += 1;
    }

    (index, declarations)
}

fn expand_body_items(
    items: &[EgclVal],
    declaration_end: usize,
    env: &Environment,
    expanded_items: &mut Vec<EgclVal>,
) -> Result<bool, EgclError> {
    let mut changed = false;
    for (index, &item) in items.iter().enumerate() {
        let expanded = if index < declaration_end {
            item
        } else {
            macroexpand_all(item, env)?
        };
        changed |= expanded != item;
        expanded_items.push(expanded);
    }
    Ok(changed)
}

/// Check if operator is a lambda expression: (LAMBDA params body...)
fn is_lambda_expression(val: EgclVal) -> bool {
    if !val.is_cons() {
        return false;
    }
    let car = unsafe { cons_car(val) };
    is_symbol_named(car, "LAMBDA")
}

/// Fully expand a form and all its subforms (recursive code-walk).
///
/// The code-walk algorithm:
/// 1. Macroexpand the top-level form.
/// 2. If the result is a self-evaluating atom or symbol, return it.
/// 3. If the result is a cons (compound form):
///    a. Check for QUOTE — quoted data is opaque, no sub-form expansion
///    occurs.
///    b. Dispatch to special-form handlers for special operators.
///    c. Handle lambda expressions in operator position.
///    d. Check for compiler macros — if a compiler
///    macro exists for the operator and notinline is NOT declared,
///    invoke it. If it declines (returns form unchanged), fall through.
///    e. The operator is NOT
///    recursively code-walked (only arguments are expanded). The
///    operator was already checked for macros by macroexpand above.
pub fn macroexpand_all(form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    if form.is_cons() {
        let operator = unsafe { cons_car(form) };
        if get_symbol_name(operator).is_some_and(|name| {
            matches!(
                name.rsplit(':').next().unwrap_or(&name),
                "ATOMIC-INCF" | "ATOMIC-DECF"
            )
        }) {
            return Ok(form);
        }
    }
    // Step 1: Macroexpand the top-level form
    let (mut expanded, _) = macroexpand(form, env)?;

    // Step 2: If the result is an atom (not a cons), we're done.
    // NIL is an atom (it's the empty list, not a cons cell).
    if !expanded.is_cons() {
        return Ok(expanded);
    }

    // Step 3: The form is a compound (cons cell).
    let mut operator = unsafe { cons_car(expanded) };

    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_expanded_root = &mut expanded);
    egcl_rt::rooted_ref!(_operator_root = &mut operator);

    // Step 3.a: QUOTE suppression.
    // Quoted data is opaque — no sub-form expansion should occur.
    if is_quote_symbol(operator) {
        return Ok(expanded);
    }

    // Quasiquote: the template is DATA, not code. Expand ONLY the argument of
    // each UNQUOTE / UNQUOTE-SPLICING (depth-aware for nested quasiquotes), and
    // preserve all template structure plus the quasiquote/unquote markers. The
    // generic walk below would treat template sub-forms as code and macroexpand
    // them — e.g. a template `(defvar ,x 0)` would expand the DEFVAR macro, which
    // quotes its name argument, dropping the unquote and losing any symbol-macro
    // substitution on `x`. That is bliss-jmde: WITH-SLOTS / SYMBOL-MACROLET used
    // inside a macro's backquote produced "variable X unbound" (broke trivia,
    // lisp-namespace, serapeum).
    if is_symbol_named(operator, "EGCL::QUASIQUOTE") {
        let arg = unsafe { cons_cdr(expanded) };
        let template = if arg.is_cons() {
            unsafe { cons_car(arg) }
        } else {
            egcl_rt::value::NIL
        };
        let mut new_template = expand_quasiquote_template(template, env, 1)?;
        egcl_rt::rooted_ref!(_new_template_root = &mut new_template);
        return Ok(alloc_cons(
            operator,
            alloc_cons(new_template, egcl_rt::value::NIL),
        ));
    }

    // Step 3.b: Special operator dispatch.
    // Special operators have structural subforms (binding names, block names,
    // tag labels) that must NOT be expanded as expressions.
    let is_special = matches!(
        env.function_information(operator),
        Some(FunctionInfo::SpecialOperator)
    );
    if is_special || (operator.is_symbol() && is_known_special_operator(operator)) {
        return expand_special_form(operator, expanded, env);
    }

    // Step 3.c: Lambda expression in operator position.
    // A form like ((lambda (x) x) 42) should have its lambda body expanded.
    if is_lambda_expression(operator) {
        return expand_lambda_call(operator, expanded, env);
    }

    // Step 3.d: Compiler macro check.
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
    expand_function_call_args(operator, expanded, env)
}

/// Expand the arguments of a function call form, leaving the operator untouched.
fn expand_function_call_args(
    mut operator: EgclVal,
    mut form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut cdr = unsafe { cons_cdr(form) };
    egcl_rt::rooted_ref!(_cdr_root = &mut cdr);
    let mut expanded_cdr = if cdr.is_cons() {
        walk_cons(cdr, env)?
    } else if !cdr.is_nil() {
        let (expanded_cdr_val, _) = macroexpand(cdr, env)?;
        expanded_cdr_val
    } else {
        cdr
    };
    egcl_rt::rooted_ref!(_expanded_cdr_root = &mut expanded_cdr);

    if expanded_cdr == cdr {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, expanded_cdr))
    }
}

/// Check if a symbol names a known CL special operator.
fn is_known_special_operator(val: EgclVal) -> bool {
    match get_symbol_name(val) {
        Some(name) => matches!(
            name.as_str(),
            "BLOCK"
                | "CATCH"
                // SETF's first subform of each pair is a PLACE, not an
                // expression, so it must not be walked as a call (bliss-msyk).
                | "SETF"
                | "EGCL::%SETF"
                | "PSETF"
                // Definers whose LAMBDA LIST binds variables over the body: the
                // parameters shadow an enclosing symbol macro, and the name,
                // qualifiers and lambda list are not expressions (bliss-ump7).
                | "DEFINE-COMPILER-MACRO"
                | "DEFMACRO"
                | "DEFMETHOD"
                | "DEFUN"
                // COND is lowered directly (not a macro): macroexpand must expand
                // its clauses so symbol-macros inside them are handled (bliss-x5y.20).
                | "COND"
                | "EVAL-WHEN"
                | "FLET"
                | "FUNCTION"
                | "GO"
                | "IF"
                | "LABELS"
                // A bare `(lambda (params) body)` form: expand the body with the
                // params shadowing any enclosing symbol-macro (bliss-x5y.21).
                | "LAMBDA"
                | "LET"
                | "LET*"
                | "LOAD-TIME-VALUE"
                | "LOCALLY"
                | "MACROLET"
                | "MULTIPLE-VALUE-CALL"
                | "MULTIPLE-VALUE-PROG1"
                | "MULTIPLE-VALUE-SETQ"
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

/// Dispatch to the correct special-form expansion handler.
/// Walk a quasiquote template as data, macroexpanding ONLY the argument of each
/// UNQUOTE / UNQUOTE-SPLICING at the outermost level. Nested EGCL::QUASIQUOTE
/// raises the depth; an UNQUOTE lowers it, so only a `depth == 1` unquote holds
/// code to expand. All structure — including the quasiquote/unquote markers — is
/// rebuilt unchanged (bliss-jmde).
fn expand_quasiquote_template(
    mut form: EgclVal,
    env: &Environment,
    depth: u32,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    if !form.is_cons() {
        return Ok(form);
    }
    let mut op = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_op_root = &mut op);
    if is_symbol_named(op, "EGCL::UNQUOTE") || is_symbol_named(op, "EGCL::UNQUOTE-SPLICING") {
        let rest = unsafe { cons_cdr(form) };
        let arg = if rest.is_cons() {
            unsafe { cons_car(rest) }
        } else {
            egcl_rt::value::NIL
        };
        let mut new_arg = if depth <= 1 {
            macroexpand_all(arg, env)?
        } else {
            expand_quasiquote_template(arg, env, depth - 1)?
        };
        egcl_rt::rooted_ref!(_new_arg_root = &mut new_arg);
        return Ok(alloc_cons(op, alloc_cons(new_arg, egcl_rt::value::NIL)));
    }
    if is_symbol_named(op, "EGCL::QUASIQUOTE") {
        let rest = unsafe { cons_cdr(form) };
        let arg = if rest.is_cons() {
            unsafe { cons_car(rest) }
        } else {
            egcl_rt::value::NIL
        };
        let mut new_arg = expand_quasiquote_template(arg, env, depth + 1)?;
        egcl_rt::rooted_ref!(_new_arg_root = &mut new_arg);
        return Ok(alloc_cons(op, alloc_cons(new_arg, egcl_rt::value::NIL)));
    }
    // Ordinary template cons: walk car and cdr as data.
    let mut new_car = expand_quasiquote_template(unsafe { cons_car(form) }, env, depth)?;
    egcl_rt::rooted_ref!(_new_car_root = &mut new_car);
    let mut new_cdr = expand_quasiquote_template(unsafe { cons_cdr(form) }, env, depth)?;
    egcl_rt::rooted_ref!(_new_cdr_root = &mut new_cdr);
    Ok(alloc_cons(new_car, new_cdr))
}

fn expand_special_form(
    operator: EgclVal,
    form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    let name = get_symbol_name(operator);
    let name_str = name.as_deref().unwrap_or("");

    match name_str {
        "QUOTE" => Ok(form),
        "GO" => Ok(form),
        "BLOCK" => expand_block(form, env),
        "RETURN-FROM" => expand_return_from(form, env),
        "TAGBODY" => expand_tagbody(form, env),
        "SETQ" => expand_setq(form, env),
        "SETF" | "PSETF" | "EGCL::%SETF" => expand_setf(form, env),
        "MULTIPLE-VALUE-SETQ" => expand_multiple_value_setq(form, env),
        "THE" => expand_the(form, env),
        "EVAL-WHEN" => expand_eval_when(form, env),
        "FUNCTION" => expand_function_special(form, env),
        "LAMBDA" => expand_lambda_expression(form, env),
        "COND" => expand_cond(form, env),
        "LET" => expand_let(form, env, false),
        "LET*" => expand_let(form, env, true),
        "FLET" => expand_flet(form, env),
        "LABELS" => expand_labels(form, env),
        "LOCALLY" => expand_locally(form, env),
        "MACROLET" => expand_macrolet(form, env),
        "SYMBOL-MACROLET" => expand_symbol_macrolet(form, env),
        // The value form is a separate compilation in the null lexical
        // environment. Its compiler expands global macros later; walking it
        // here would capture this body's MACROLET/SYMBOL-MACROLET bindings.
        "LOAD-TIME-VALUE" => Ok(form),
        // A definer's LAMBDA LIST binds variables over its body, so those names
        // shadow an enclosing SYMBOL-MACROLET (CLHS 3.1.2.1.1). Walking these as
        // ordinary calls expanded the body without the shadowing, so a symbol
        // macro whose expansion mentions a parameter re-expanded forever and hit
        // the circular-expansion guard (bliss-ump7).
        "DEFUN" | "DEFMETHOD" | "DEFMACRO" | "DEFINE-COMPILER-MACRO" => {
            expand_definer_body(form, env)
        }
        // For IF, PROGN, CATCH, THROW, UNWIND-PROTECT, MULTIPLE-VALUE-CALL,
        // MULTIPLE-VALUE-PROG1, PROGV — all subforms are
        // expression positions, so the generic walk is correct.
        _ => expand_function_call_args(operator, form, env),
    }
}

// ── Special form handlers ─────────────────────────────────────────

/// Expand COND: `(cond (test body...) ...)`. COND is not a macro in egcl (it is
/// lowered directly by the tree-walker and the bytecode compiler), so
/// macroexpand_all must expand each clause's test and body itself — otherwise a
/// symbol-macro referenced inside a clause (e.g. a WITH-SLOTS slot used in a
/// `(cond ((zerop maximum) …))` test) is left unexpanded and reads as an unbound
/// variable once the clause is compiled (bliss-x5y.20). WHEN/UNLESS/AND/OR need
/// no handler — their subforms are plain expressions the generic arg-walk
/// already expands; only COND's `(test . body)` clause shape needs this.
fn expand_cond(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    egcl_rt::rooted!(operator = unsafe { cons_car(form) });
    egcl_rt::rooted!(clauses = cons_to_vec(unsafe { cons_cdr(form) }));
    egcl_rt::rooted!(out = Vec::<EgclVal>::with_capacity(clauses.len()));
    for i in 0..clauses.len() {
        let clause = clauses[i];
        if !clause.is_cons() {
            out.push(clause);
            continue;
        }
        // Root test and body before any allocation (macroexpand/expand_body and
        // alloc_cons all allocate; moving GC — bliss-noh).
        egcl_rt::rooted!(test = unsafe { cons_car(clause) });
        egcl_rt::rooted!(cbody = unsafe { cons_cdr(clause) });
        egcl_rt::rooted!(etest = macroexpand_all(*test, env)?);
        egcl_rt::rooted!(ebody = expand_body(*cbody, env)?);
        out.push(alloc_cons(*etest, *ebody));
    }
    egcl_rt::rooted!(clause_list = vec_to_cons(&out));
    Ok(alloc_cons(*operator, *clause_list))
}

/// Expand BLOCK: (block name body...)
/// Block name is NOT expanded; body forms are expanded.
/// The block name is registered in the environment.
fn expand_block(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    let mut operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let mut block_name = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };

    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    egcl_rt::rooted_ref!(_block_name_root = &mut block_name);
    egcl_rt::rooted_ref!(_body_root = &mut body);

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
fn expand_return_from(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    let mut operator = unsafe { cons_car(form) };
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let mut block_name = unsafe { cons_car(args) };
    let rest = unsafe { cons_cdr(args) };

    if !rest.is_cons() {
        return Ok(form);
    }
    let mut result_form = unsafe { cons_car(rest) };
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    egcl_rt::rooted_ref!(_block_name_root = &mut block_name);
    egcl_rt::rooted_ref!(_result_form_root = &mut result_form);
    let expanded_result = macroexpand_all(result_form, env)?;

    if expanded_result == result_form {
        Ok(form)
    } else {
        // Sequence the conses so `block_name`/`operator` are re-read after
        // each inner allocation instead of being copied into stale argument
        // temps before it (moving GC; bliss-8qf).
        egcl_rt::rooted!(tail = alloc_cons(expanded_result, egcl_rt::value::NIL));
        egcl_rt::rooted!(name_tail = alloc_cons(block_name, *tail));
        Ok(alloc_cons(operator, *name_tail))
    }
}

/// Expand TAGBODY: (tagbody {tag|form}*)
/// Tags (symbols and integers) are NOT expanded. Non-tag forms are expanded.
fn expand_tagbody(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    let mut operator = unsafe { cons_car(form) };
    let body = unsafe { cons_cdr(form) };

    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    egcl_rt::rooted_ref!(_operator_root = &mut operator);

    // First pass: register all tags in the environment
    let mut new_env = env.clone();
    egcl_rt::rooted!(items = cons_to_vec(body));
    for i in 0..items.len() {
        // Tags are symbols or integers (atoms that are not cons)
        if items[i].is_symbol() && !items[i].is_nil() {
            new_env = new_env.augment_tag(items[i]);
        }
    }

    // Second pass: expand non-tag forms
    egcl_rt::rooted!(expanded_items = Vec::with_capacity(items.len()));
    let mut changed = false;
    for i in 0..items.len() {
        if (items[i].is_symbol() && !items[i].is_nil()) || items[i].is_fixnum() {
            // Tags: symbols and integers are NOT expanded
            expanded_items.push(items[i]);
        } else {
            let exp = macroexpand_all(items[i], &new_env)?;
            if exp != items[i] {
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

/// Expand MULTIPLE-VALUE-SETQ: (multiple-value-setq (var*) form)
///
/// A var that names a SYMBOL MACRO is assigned as a PLACE, not as a variable —
/// CLHS defines the form as `(values (setf (values var*) form))`. Without this,
/// MULTIPLE-VALUE-SETQ was not in the special-form list at all, so its var LIST
/// was walked as if it were a call and the symbol macros in it were never
/// substituted. SYMBOL-MACROLET expands its body eagerly through this
/// macroexpander (see eval_symbol_macrolet), so nothing downstream could
/// recover the binding: the assignment silently went to a variable named by the
/// symbol instead of to the place, and the place kept its old value with no
/// error (ansi MULTIPLE-VALUE-SETQ.3/.4/.6/.7). A GLOBAL DEFINE-SYMBOL-MACRO
/// already worked, because that table survives to the tree-walker.
///
/// Vars that are NOT symbol macros are left alone, exactly as SETQ leaves them.
fn expand_multiple_value_setq(
    mut form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    egcl_rt::rooted!(items = cons_to_vec(args));
    if items.is_empty() {
        return Ok(form);
    }
    egcl_rt::rooted!(vars = cons_to_vec(items[0]));
    let val_form = items.get(1).copied().unwrap_or(egcl_rt::value::NIL);
    egcl_rt::rooted!(expanded_val = macroexpand_all(val_form, env)?);

    egcl_rt::rooted!(places = Vec::<EgclVal>::new());
    let mut any_symbol_macro = false;
    for i in 0..vars.len() {
        let var = vars[i];
        if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(var) {
            any_symbol_macro = true;
            places.push(expansion);
        } else {
            places.push(var);
        }
    }

    if !any_symbol_macro {
        // Rebuild with the expanded value form; the var list is untouched.
        egcl_rt::rooted!(rebuilt = vec_to_cons(&[items[0], *expanded_val]));
        return Ok(alloc_cons(operator, *rebuilt));
    }

    // (values (setf (values place*) expanded-value)) — the VALUES wrapper
    // truncates to the primary value, which is what MULTIPLE-VALUE-SETQ returns.
    let values_sym = make_symbol("VALUES");
    egcl_rt::rooted!(values_place = alloc_cons(values_sym, vec_to_cons(&places)));
    egcl_rt::rooted!(
        setf_form = vec_to_cons(&[make_symbol("SETF"), *values_place, *expanded_val])
    );
    egcl_rt::rooted!(wrapped = vec_to_cons(&[make_symbol("VALUES"), *setf_form]));
    macroexpand_all(*wrapped, env)
}

/// Expand SETF: `(setf {place value}*)`.
///
/// A place is not an expression. Its value forms expand normally, but the place
/// itself expands as a place: a symbol macro expands (its expansion is again a
/// place), and the place's own subforms expand as expressions — while the head
/// keeps its identity. Walking a place as an ordinary call let a compiler macro
/// on the accessor rewrite it, and the rewritten form was no longer a place at
/// all: CFFI's WITH-FOREIGN-SLOTS binds a symbol macro standing for
/// `(foreign-slot-value …)`, whose accessor has both a compiler macro and a SETF
/// expander, so `(setf slot value)` reached SETF as
/// `(setf (translate-from-foreign …) value)` and failed as an unsupported place
/// (bliss-msyk). CLHS 3.2.2.1 allows a compiler macro only for a function call.
fn expand_setf(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    egcl_rt::rooted!(items = cons_to_vec(unsafe { cons_cdr(form) }));
    // An odd number of subforms is a program error SETF itself reports; leave
    // such a form alone rather than guessing at its pairing.
    if items.is_empty() || items.len() % 2 != 0 {
        return Ok(form);
    }
    egcl_rt::rooted!(rebuilt = Vec::<EgclVal>::new());
    let mut changed = false;
    for i in (0..items.len()).step_by(2) {
        let place = expand_place(items[i], env, 0)?;
        changed |= place != items[i];
        rebuilt.push(place);
        let value = macroexpand_all(items[i + 1], env)?;
        changed |= value != items[i + 1];
        rebuilt.push(value);
    }
    // Share the original form when nothing in it expanded, rather than
    // allocating an identical copy: a caller may hold the place by identity.
    if !changed {
        return Ok(form);
    }
    Ok(alloc_cons(operator, vec_to_cons(&rebuilt)))
}

/// Expand a SETF place, keeping it a place.
///
/// A symbol macro expands and its expansion is expanded again as a place; an
/// ordinary macro in the head position expands the same way (CLHS 5.1.2.7). A
/// function-call place keeps its head and expands only its argument subforms —
/// no compiler macro is applied to it.
fn expand_place(
    mut place: EgclVal,
    env: &Environment,
    depth: usize,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_place_root = &mut place);
    // A place that expands to itself — `(symbol-macrolet ((a a)) (setf a 1))` —
    // must be reported, not recursed on until the Rust stack runs out.
    if depth > PLACE_EXPANSION_LIMIT {
        return Err(EgclError::Internal("circular SETF place expansion".into()));
    }
    if place.is_symbol() {
        if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(place) {
            return expand_place(expansion, env, depth + 1);
        }
        return Ok(place);
    }
    if !place.is_cons() {
        return macroexpand_all(place, env);
    }
    let mut head = unsafe { cons_car(place) };
    egcl_rt::rooted_ref!(_head_root = &mut head);
    if head.is_symbol() && !is_quote_symbol(head) {
        // A macro place (a DEFMACRO accessor, or one a MACROLET bound) expands
        // first; whatever it expands to is again a place.
        let (expanded, changed) = macroexpand_1(place, env)?;
        if changed {
            return expand_place(expanded, env, depth + 1);
        }
    }
    // Some places hold places of their own, and one holds a type specifier.
    // Expanding those as expressions would walk a subform that is not one.
    let nested: &[PlaceArgument] = match get_symbol_name(head).as_deref() {
        // (the type place): the type is not a form at all.
        Some("THE") => &[PlaceArgument::Verbatim, PlaceArgument::Place],
        // (values place*): every subform is a place.
        Some("VALUES") => &[PlaceArgument::Place],
        // (ldb bytespec place) / (mask-field bytespec place)
        Some("LDB") | Some("MASK-FIELD") => &[PlaceArgument::Expression, PlaceArgument::Place],
        // (getf place indicator &optional default)
        Some("GETF") => &[PlaceArgument::Place, PlaceArgument::Expression],
        _ => &[PlaceArgument::Expression],
    };
    egcl_rt::rooted!(arguments = Vec::<EgclVal>::new());
    let mut cursor = unsafe { cons_cdr(place) };
    egcl_rt::rooted_ref!(_cursor_root = &mut cursor);
    let mut index = 0usize;
    let mut changed = false;
    while cursor.is_cons() {
        let argument = unsafe { cons_car(cursor) };
        // The last entry repeats, so `(values a b c)` treats every subform as a
        // place and an ordinary call treats every subform as an expression.
        let expanded = match nested[index.min(nested.len() - 1)] {
            PlaceArgument::Verbatim => argument,
            PlaceArgument::Place => expand_place(argument, env, depth + 1)?,
            PlaceArgument::Expression => macroexpand_all(argument, env)?,
        };
        changed |= expanded != argument;
        arguments.push(expanded);
        cursor = unsafe { cons_cdr(cursor) };
        index += 1;
    }
    // A place whose subforms did not expand is returned as it came in, so the
    // caller's own reference to it stays EQ (spec_reader_macroexpand asserts
    // exactly that for a symbol macro's expansion).
    if !changed {
        return Ok(place);
    }
    Ok(alloc_cons(head, vec_to_cons(&arguments)))
}

/// How deep a place may expand before it is treated as circular. A real place
/// nests a handful of levels; this only stops a self-referential one.
const PLACE_EXPANSION_LIMIT: usize = 100;

/// How one subform of a place is to be expanded.
#[derive(Clone, Copy)]
enum PlaceArgument {
    /// An ordinary expression.
    Expression,
    /// A place in its own right.
    Place,
    /// Not a form: THE's type specifier.
    Verbatim,
}

/// Expand SETQ: (setq {var value}*)
/// For each pair: if var is a symbol macro, convert to (setf expansion expanded-value).
/// Otherwise, expand value only (var is NOT expanded).
fn expand_setq(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    egcl_rt::rooted!(items = cons_to_vec(args));

    if items.len() % 2 != 0 {
        return Err(EgclError::Internal(
            "SETQ requires an even number of arguments".into(),
        ));
    }

    // result_pairs split into parallel rooted Vecs (3-tuples are not HostRoot-able).
    egcl_rt::rooted!(rp_vars = Vec::<EgclVal>::new());
    egcl_rt::rooted!(rp_valforms = Vec::<EgclVal>::new());
    egcl_rt::rooted!(rp_setf = Vec::<Option<EgclVal>>::new());
    let mut any_symbol_macro = false;
    let mut changed = false;

    for i in (0..items.len()).step_by(2) {
        let var = items[i];
        let val_form = items[i + 1];

        // Check if var is a symbol macro
        if let Some(VariableInfo::SymbolMacro(expansion)) = env.variable_information(var) {
            // Convert (setq sym val) -> (setf expansion expanded-val)
            any_symbol_macro = true;
            let mut expansion = expansion;
            egcl_rt::rooted_ref!(_expansion_root = &mut expansion);
            let mut expanded_val = macroexpand_all(val_form, env)?;
            egcl_rt::rooted_ref!(_expanded_val_root = &mut expanded_val);
            let setf_sym = make_symbol("SETF");
            let setf_form = alloc_cons(
                setf_sym,
                alloc_cons(expansion, alloc_cons(expanded_val, egcl_rt::value::NIL)),
            );
            // Re-enter expand-form on the setf form
            let expanded_setf = macroexpand_all(setf_form, env)?;
            rp_vars.push(items[i]);
            rp_valforms.push(items[i + 1]);
            rp_setf.push(Some(expanded_setf));
        } else {
            // Normal case: expand value, don't expand var
            let expanded_val = macroexpand_all(val_form, env)?;
            if expanded_val != items[i + 1] {
                changed = true;
            }
            rp_vars.push(items[i]);
            rp_valforms.push(items[i + 1]);
            rp_setf.push(None);
        }
    }

    if any_symbol_macro {
        // If we have multiple pairs and any had symbol-macro conversion,
        // wrap in PROGN for multiple setf forms, or return single form.
        egcl_rt::rooted!(setf_forms = Vec::<EgclVal>::new());
        egcl_rt::rooted!(normal_pairs = Vec::<(EgclVal, EgclVal)>::new());
        for idx in 0..rp_setf.len() {
            if rp_setf[idx].is_some() {
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
                setf_forms.push(rp_setf[idx].unwrap());
            } else {
                let expanded_val = macroexpand_all(rp_valforms[idx], env)?;
                normal_pairs.push((rp_vars[idx], expanded_val));
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
            return Ok(setf_forms[0]);
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
    for idx in 0..rp_vars.len() {
        new_args.push(rp_vars[idx]);
    }
    // Re-expand to get the right values
    egcl_rt::rooted!(final_args = Vec::<EgclVal>::with_capacity(items.len()));
    for i in (0..items.len()).step_by(2) {
        final_args.push(items[i]); // var unchanged
        final_args.push(macroexpand_all(items[i + 1], env)?); // expand value
    }
    Ok(alloc_cons(operator, vec_to_cons(&final_args)))
}

/// Expand THE: (the type-spec value-form)
/// Type specifier is NOT expanded; value form is expanded.
fn expand_the(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let mut type_spec = unsafe { cons_car(args) };
    egcl_rt::rooted_ref!(_type_spec_root = &mut type_spec);
    let rest = unsafe { cons_cdr(args) };
    if !rest.is_cons() {
        return Ok(form);
    }
    let mut value_form = unsafe { cons_car(rest) };
    egcl_rt::rooted_ref!(_value_form_root = &mut value_form);
    let expanded_value = macroexpand_all(value_form, env)?;

    if expanded_value == value_form {
        Ok(form)
    } else {
        // Sequence the conses so `type_spec` (which can be a heap cons, e.g.
        // (integer 0 10)) and `operator` are re-read after each inner
        // allocation instead of being copied into stale argument temps
        // before it (moving GC; bliss-8qf).
        egcl_rt::rooted!(tail = alloc_cons(expanded_value, egcl_rt::value::NIL));
        egcl_rt::rooted!(spec_tail = alloc_cons(type_spec, *tail));
        Ok(alloc_cons(operator, *spec_tail))
    }
}

/// Expand EVAL-WHEN: (eval-when (situation...) body...)
/// Situations list is NOT expanded; body forms are expanded.
fn expand_eval_when(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let mut situations = unsafe { cons_car(args) };
    egcl_rt::rooted_ref!(_situations_root = &mut situations);
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

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
fn expand_function_special(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let mut arg = unsafe { cons_car(args) };
    egcl_rt::rooted_ref!(_arg_root = &mut arg);

    // Check if the argument is a lambda expression
    if is_lambda_expression(arg) {
        let expanded_lambda = expand_lambda_expression(arg, env)?;
        if expanded_lambda == arg {
            Ok(form)
        } else {
            Ok(alloc_cons(
                operator,
                alloc_cons(expanded_lambda, egcl_rt::value::NIL),
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
fn expand_lambda_expression(
    mut lambda: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_lambda_root = &mut lambda);
    let mut lambda_sym = unsafe { cons_car(lambda) }; // LAMBDA
    egcl_rt::rooted_ref!(_lambda_sym_root = &mut lambda_sym);
    let rest = unsafe { cons_cdr(lambda) };
    if !rest.is_cons() {
        return Ok(lambda);
    }
    let mut params = unsafe { cons_car(rest) }; // parameter list
    egcl_rt::rooted_ref!(_params_root = &mut params);
    let mut body = unsafe { cons_cdr(rest) }; // body forms
    egcl_rt::rooted_ref!(_body_root = &mut body);

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

/// Expand the body of a definer that binds a lambda list — DEFUN, DEFMETHOD,
/// DEFMACRO, DEFINE-COMPILER-MACRO — with the parameters shadowing any enclosing
/// symbol macros, and with the name, qualifiers and lambda list left alone (they
/// are not expressions).
///
/// Serapeum's DEFINE-ENV-METHOD is what needs this: it wraps a DEFMETHOD in a
/// SYMBOL-MACROLET binding every slot name, including SELF, to
/// `(slot-value self 'slot)`. Inside the method SELF is a parameter, so the
/// expansion is final; without the shadowing the walker re-expanded SELF inside
/// it, looped, and fell back to a path that left the other slot macros
/// unexpanded — "unbound variable: SLOT" (bliss-ump7).
fn expand_definer_body(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    // (op name [qualifier…] lambda-list . body): the lambda list is the first
    // element after the name that is a list — DEFMETHOD qualifiers are symbols.
    egcl_rt::rooted!(items = cons_to_vec(form));
    if items.len() < 3 {
        return Ok(form);
    }
    let Some(lambda_list_index) = (2..items.len()).find(|&index| {
        let item = items[index];
        item.is_nil() || item.is_cons()
    }) else {
        return Ok(form);
    };

    // Parameter names shadow symbol macros over the body. `(var default)`,
    // `(var specializer)` and `((:keyword var) default)` all name their variable
    // in the car (or the cadr of the keyword pair).
    let mut body_env = env.clone();
    for param in cons_to_vec(items[lambda_list_index]) {
        let name = if param.is_symbol() {
            param
        } else if param.is_cons() {
            let first = unsafe { cons_car(param) };
            if first.is_cons() {
                // ((:keyword var) …)
                let rest = unsafe { cons_cdr(param) };
                if rest.is_cons() {
                    unsafe { cons_car(rest) }
                } else {
                    continue;
                }
            } else {
                first
            }
        } else {
            continue;
        };
        if !name.is_symbol() || name.is_nil() {
            continue;
        }
        if get_symbol_name(name).is_some_and(|text| text.starts_with('&')) {
            continue;
        }
        body_env = body_env.augment_variable(name, VariableInfo::Lexical);
    }

    let mut changed = false;
    egcl_rt::rooted!(expanded = Vec::<EgclVal>::with_capacity(items.len()));
    for index in 0..items.len() {
        if index <= lambda_list_index {
            expanded.push(items[index]);
            continue;
        }
        let out = macroexpand_all(items[index], &body_env)?;
        changed |= out != items[index];
        expanded.push(out);
    }
    if !changed {
        return Ok(form);
    }
    Ok(vec_to_cons(&expanded))
}

/// Expand a lambda call: ((lambda (params...) body...) arg1 arg2 ...)
/// Expand the lambda body AND the arguments.
fn expand_lambda_call(
    mut operator: EgclVal,
    mut form: EgclVal,
    env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut args = unsafe { cons_cdr(form) };
    egcl_rt::rooted_ref!(_args_root = &mut args);

    // Expand the lambda expression
    let mut expanded_lambda = expand_lambda_expression(operator, env)?;
    egcl_rt::rooted_ref!(_expanded_lambda_root = &mut expanded_lambda);

    // Expand the arguments
    let mut expanded_args = if args.is_cons() {
        walk_cons(args, env)?
    } else {
        args
    };
    egcl_rt::rooted_ref!(_expanded_args_root = &mut expanded_args);

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
fn expand_let(
    mut form: EgclVal,
    env: &Environment,
    sequential: bool,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let bindings_list = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

    egcl_rt::rooted!(bindings = cons_to_vec(bindings_list));

    // Expand init-forms and collect variable names
    egcl_rt::rooted!(expanded_bindings = Vec::<EgclVal>::with_capacity(bindings.len()));
    let mut bindings_changed = false;
    let mut current_env = env.clone();

    for idx in 0..bindings.len() {
        let binding = bindings[idx];
        if binding.is_cons() {
            // (var init-form) pair
            let mut var = unsafe { cons_car(binding) };
            egcl_rt::rooted_ref!(_var_root = &mut var);
            let init_rest = unsafe { cons_cdr(binding) };
            let mut init_form = if init_rest.is_cons() {
                unsafe { cons_car(init_rest) }
            } else {
                egcl_rt::value::NIL
            };
            egcl_rt::rooted_ref!(_init_form_root = &mut init_form);

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
                alloc_cons(expanded_init, egcl_rt::value::NIL),
            ));

            // For LET*, augment env after each binding
            if sequential {
                current_env = current_env.augment_variable(var, VariableInfo::Lexical);
            }
        } else {
            // Bare symbol — (let (x) ...) means (let ((x nil)) ...)
            expanded_bindings.push(binding);
        }
    }

    // Build augmented env for the body
    let mut body_env = if sequential { current_env } else { env.clone() };
    if !sequential {
        for idx in 0..bindings.len() {
            let binding = bindings[idx];
            let var = if binding.is_cons() {
                unsafe { cons_car(binding) }
            } else {
                binding
            };
            if var.is_symbol() && !var.is_nil() {
                body_env = body_env.augment_variable(var, VariableInfo::Lexical);
            }
        }
    }

    let mut expanded_body = expand_body(body, &body_env)?;
    // Root the expanded body: `vec_to_cons` below allocates (moving GC), and an
    // unrooted `expanded_body` would go stale — its slot reused by one of the
    // freshly-consed bindings, producing a cyclic form that later walkers loop on
    // (bliss-9u6d; observed as a macroexpand CYCLE under EGCL_GC_STRESS).
    egcl_rt::rooted_ref!(_expanded_body_root = &mut expanded_body);

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
fn expand_flet(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let fn_defs = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

    egcl_rt::rooted!(defs = cons_to_vec(fn_defs));
    egcl_rt::rooted!(expanded_defs = Vec::with_capacity(defs.len()));
    let mut defs_changed = false;

    // FLET: function bodies are expanded in the OUTER env (not the augmented one)
    for i in 0..defs.len() {
        let def = defs[i];
        if !def.is_cons() {
            expanded_defs.push(def);
            continue;
        }
        let mut fn_name = unsafe { cons_car(def) };
        egcl_rt::rooted_ref!(_fn_name_root = &mut fn_name);
        let fn_rest = unsafe { cons_cdr(def) };
        if !fn_rest.is_cons() {
            expanded_defs.push(def);
            continue;
        }
        let mut params = unsafe { cons_car(fn_rest) };
        egcl_rt::rooted_ref!(_params_root = &mut params);
        let mut fn_body = unsafe { cons_cdr(fn_rest) };
        egcl_rt::rooted_ref!(_fn_body_root = &mut fn_body);

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
    for i in 0..defs.len() {
        let def = defs[i];
        if def.is_cons() {
            let fn_name = unsafe { cons_car(def) };
            body_env = body_env.augment_function(fn_name, FunctionInfo::Lexical);
        }
    }

    let mut expanded_body = expand_body(body, &body_env)?;
    egcl_rt::rooted_ref!(_expanded_body_root = &mut expanded_body);

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
fn expand_labels(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let fn_defs = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

    egcl_rt::rooted!(defs = cons_to_vec(fn_defs));

    // LABELS: first augment env with ALL function names (recursive visibility)
    let mut augmented_env = env.clone();
    egcl_rt::rooted_ref!(_augmented_env_root = &mut augmented_env);
    for i in 0..defs.len() {
        let def = defs[i];
        if def.is_cons() {
            let fn_name = unsafe { cons_car(def) };
            augmented_env = augmented_env.augment_function(fn_name, FunctionInfo::Lexical);
        }
    }

    // Now expand function bodies in the augmented env
    egcl_rt::rooted!(expanded_defs = Vec::with_capacity(defs.len()));
    let mut defs_changed = false;
    for i in 0..defs.len() {
        let def = defs[i];
        if !def.is_cons() {
            expanded_defs.push(def);
            continue;
        }
        let mut fn_name = unsafe { cons_car(def) };
        egcl_rt::rooted_ref!(_fn_name_root = &mut fn_name);
        let fn_rest = unsafe { cons_cdr(def) };
        if !fn_rest.is_cons() {
            expanded_defs.push(def);
            continue;
        }
        let mut params = unsafe { cons_car(fn_rest) };
        egcl_rt::rooted_ref!(_params_root = &mut params);
        let mut fn_body = unsafe { cons_cdr(fn_rest) };
        egcl_rt::rooted_ref!(_fn_body_root = &mut fn_body);

        // Augment with params
        let mut fn_env = augmented_env.clone();
        egcl_rt::rooted_ref!(_fn_env_root = &mut fn_env);
        egcl_rt::rooted!(param_list = cons_to_vec(params));
        for j in 0..param_list.len() {
            let param = param_list[j];
            if param.is_symbol() && !param.is_nil() {
                if let Some(name) = get_symbol_name(param) {
                    if name.starts_with('&') {
                        continue;
                    }
                }
                fn_env = fn_env.augment_variable(param, VariableInfo::Lexical);
            }
        }

        let mut expanded_fn_body = expand_body(fn_body, &fn_env)?;
        egcl_rt::rooted_ref!(_expanded_fn_body_root = &mut expanded_fn_body);
        if expanded_fn_body != fn_body {
            defs_changed = true;
        }
        expanded_defs.push(alloc_cons(fn_name, alloc_cons(params, expanded_fn_body)));
    }

    let mut expanded_body = expand_body(body, &augmented_env)?;
    egcl_rt::rooted_ref!(_expanded_body_root = &mut expanded_body);

    if !defs_changed && expanded_body == body {
        Ok(form)
    } else {
        let new_defs = vec_to_cons(&expanded_defs);
        Ok(alloc_cons(operator, alloc_cons(new_defs, expanded_body)))
    }
}

/// Expand LOCALLY: (locally decl* body*)
/// Declarations are processed but NOT expanded. Body is expanded.
fn expand_locally(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let mut body = unsafe { cons_cdr(form) };
    egcl_rt::rooted_ref!(_body_root = &mut body);
    let expanded_body = expand_body(body, env)?;

    if expanded_body == body {
        Ok(form)
    } else {
        Ok(alloc_cons(operator, expanded_body))
    }
}

/// Expand MACROLET: (macrolet ((name lambda-list macro-body...) ...) body...)
/// Install local macro definitions into a new environment.
/// Expand body in the augmented env. Strip MACROLET from output.
fn expand_macrolet(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    // Root across the allocating expand recursion (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let macro_defs = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

    // Install macro definitions in a new environment frame
    let mut augmented_env = env.clone();
    egcl_rt::rooted_ref!(_augmented_env_root = &mut augmented_env);
    egcl_rt::rooted!(defs = cons_to_vec(macro_defs));
    for i in 0..defs.len() {
        let def = defs[i];
        if !def.is_cons() {
            continue;
        }
        let mut macro_name = unsafe { cons_car(def) };
        egcl_rt::rooted_ref!(_macro_name_root = &mut macro_name);
        let key = make_local_macrolet_expander(def, env.clone())?;
        augmented_env = augmented_env.augment_function(macro_name, FunctionInfo::Macro(key));
    }

    // Expand body in augmented env
    let mut expanded_body = expand_body(body, &augmented_env)?;
    egcl_rt::rooted_ref!(_expanded_body_root = &mut expanded_body);

    // Strip MACROLET wrapper: output as (LOCALLY expanded-body...) or
    // if single body form, just return it.
    egcl_rt::rooted!(body_items = cons_to_vec(expanded_body));
    if body_items.len() == 1 {
        Ok(body_items[0])
    } else {
        let progn_sym = make_symbol("PROGN");
        Ok(alloc_cons(progn_sym, expanded_body))
    }
}

/// Expand SYMBOL-MACROLET: (symbol-macrolet ((sym expansion)...) body...)
/// Install symbol-macro bindings in the environment.
/// Expand body in the augmented env. Strip SYMBOL-MACROLET from output.
fn expand_symbol_macrolet(mut form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    let args = unsafe { cons_cdr(form) };
    if !args.is_cons() {
        return Ok(form);
    }
    let bindings_list = unsafe { cons_car(args) };
    let mut body = unsafe { cons_cdr(args) };
    egcl_rt::rooted_ref!(_body_root = &mut body);

    // Install symbol-macro bindings
    let mut augmented_env = env.clone();
    egcl_rt::rooted_ref!(_augmented_env_root = &mut augmented_env);
    egcl_rt::rooted!(bindings = cons_to_vec(bindings_list));
    for i in 0..bindings.len() {
        let binding = bindings[i];
        if !binding.is_cons() {
            continue;
        }
        let sym = unsafe { cons_car(binding) };
        let expansion_rest = unsafe { cons_cdr(binding) };
        let expansion = if expansion_rest.is_cons() {
            unsafe { cons_car(expansion_rest) }
        } else {
            egcl_rt::value::NIL
        };

        // Validate: symbol-macrolet of a special variable is an error
        if let Some(VariableInfo::Special) = env.variable_information(sym) {
            return Err(EgclError::Internal(
                "SYMBOL-MACROLET: cannot define symbol macro for special variable".into(),
            ));
        }

        augmented_env = augmented_env.augment_variable(sym, VariableInfo::SymbolMacro(expansion));
    }

    // Expand body in augmented env
    let mut expanded_body = expand_body(body, &augmented_env)?;
    // Root across the allocating make_symbol (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_expanded_body_root = &mut expanded_body);

    // Strip SYMBOL-MACROLET wrapper from output
    egcl_rt::rooted!(body_items = cons_to_vec(expanded_body));
    if body_items.len() == 1 {
        Ok(body_items[0])
    } else {
        let progn_sym = make_symbol("PROGN");
        Ok(alloc_cons(progn_sym, expanded_body))
    }
}

/// Check if a EgclVal is the QUOTE symbol.
fn is_quote_symbol(val: EgclVal) -> bool {
    if !val.is_symbol() {
        return false;
    }
    // NIL and T are special symbols that are not QUOTE
    if val.is_nil() || val.0 == egcl_rt::value::T.0 {
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
fn walk_cons(form: EgclVal, env: &Environment) -> Result<EgclVal, EgclError> {
    debug_assert!(form.is_cons(), "walk_cons called on non-cons value");

    // Root across the allocating expansion (moving GC; bliss-noh). `rooted!`
    // (bliss-a03) is the intrusive lock-free root: O(1) link/unlink, vs. a global
    // mutex + O(n) drop per StackRoot — this runs once per cons of every
    // macroexpanded form, so the difference is the O(n²) hot case it targets.
    egcl_rt::rooted!(form = form);
    // The SPINE is walked ITERATIVELY. Recursing into the cdr cost one Rust
    // frame per list ELEMENT, so a machine-generated form overflowed the control
    // stack and the process took SIGSEGV: Serapeum's LOCAL expands
    // internal-definitions.lisp into a list of more than 10,000 elements
    // (bliss-ump7). Nesting still recurses through `macroexpand_all` on each car,
    // which is bounded by how deeply the source is written.
    egcl_rt::rooted!(expanded_cars = Vec::<EgclVal>::new());
    egcl_rt::rooted!(cursor = *form);
    egcl_rt::rooted!(tail = egcl_rt::value::NIL);
    let mut changed = false;
    loop {
        // Re-read through the rooted cursor each time: the expansion below
        // allocates and may have relocated this cell.
        let car = unsafe { cons_car(*cursor) };
        let expanded_car = macroexpand_all(car, env)?;
        // Compare against the car AS IT IS NOW, not the pre-expansion copy: a
        // collection during the expansion would have moved it.
        changed |= expanded_car != unsafe { cons_car(*cursor) };
        expanded_cars.push(expanded_car);
        let next = unsafe { cons_cdr(*cursor) };
        if next.is_cons() {
            *cursor = next;
        } else {
            *tail = next;
            break;
        }
    }

    // A dotted tail is itself a form; NIL ends a proper list and expands to
    // itself.
    if !tail.is_nil() {
        let (expanded_tail, _) = macroexpand(*tail, env)?;
        changed |= expanded_tail != *tail;
        *tail = expanded_tail;
    }

    // Unchanged: return the original structure so identity is preserved — the
    // compiler-macro decline check compares pointers.
    if !changed {
        return Ok(*form);
    }

    // Rebuild from the tail inwards (non-destructive: shared quoted structure
    // must not be mutated).
    egcl_rt::rooted!(rebuilt = *tail);
    for index in (0..expanded_cars.len()).rev() {
        *rebuilt = alloc_cons(expanded_cars[index], *rebuilt);
    }
    Ok(*rebuilt)
}
type MacroFn = dyn Fn(EgclVal, &Environment) -> Result<EgclVal, EgclError> + Send + Sync;

/// A hosting Lisp evaluator can execute arbitrary local macro bodies. The
/// standalone compiler keeps its small evaluator when no host is installed.
pub type LocalMacroEvaluator = fn(EgclVal, EgclVal, EgclVal, &Environment, &Environment)
    -> Result<EgclVal, EgclError>;
thread_local! {
    static LOCAL_MACRO_EVALUATOR: Cell<Option<LocalMacroEvaluator>> = const { Cell::new(None) };
}

pub fn set_local_macro_evaluator(evaluator: LocalMacroEvaluator) {
    LOCAL_MACRO_EVALUATOR.with(|hook| hook.set(Some(evaluator)));
}

fn make_local_macrolet_expander(
    mut def: EgclVal,
    mut defining_env: Environment,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_def_root = &mut def);
    egcl_rt::rooted_ref!(_defining_env_root = &mut defining_env);
    let mut name = unsafe { cons_car(def) };
    egcl_rt::rooted_ref!(_name_root = &mut name);
    let rest = unsafe { cons_cdr(def) };
    if !rest.is_cons() {
        return Err(EgclError::Internal(
            "MACROLET: malformed local macro definition".into(),
        ));
    }
    let mut params = unsafe { cons_car(rest) };
    egcl_rt::rooted_ref!(_params_root = &mut params);
    let mut body = unsafe { cons_cdr(rest) };
    egcl_rt::rooted_ref!(_body_root = &mut body);
    let parsed = parse_macro(name, params, body, Some(&defining_env))?;
    enclose(parsed, &defining_env)
}

fn expand_local_macro_call(
    whole_form: EgclVal,
    call_env: &Environment,
    defining_env: &Environment,
    params: EgclVal,
    mut body: EgclVal,
) -> Result<EgclVal, EgclError> {
    // Root body across the allocating bind_macrolet_lambda_list (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_body_root = &mut body);
    if let Some(evaluate) = LOCAL_MACRO_EVALUATOR.with(Cell::get) {
        return evaluate(whole_form, params, body, defining_env, call_env);
    }
    let arg_forms = if whole_form.is_cons() {
        unsafe { cons_cdr(whole_form) }
    } else {
        egcl_rt::value::NIL
    };
    let bindings = bind_macrolet_lambda_list(params, arg_forms)?;
    let mut expansion_env = defining_env.augment_environment(bindings, Vec::new(), Vec::new());
    // Root expansion_env: it binds the macro arguments (possibly movable conses)
    // and the mini-evaluator allocates while reading them (bliss-noh).
    egcl_rt::rooted_ref!(_expansion_env_root = &mut expansion_env);
    eval_local_macro_body(body, &expansion_env, call_env)
}

fn bind_macrolet_lambda_list(
    params: EgclVal,
    args: EgclVal,
) -> Result<Vec<(EgclVal, VariableInfo)>, EgclError> {
    let params_vec = cons_to_vec(params);
    let args_vec = cons_to_vec(args);
    // Constant values are hidden inside VariableInfo (not scanned by HostRoot),
    // so accumulate keys/values in parallel rooted Vecs across the trailing
    // vec_to_cons alloc, then assemble the bindings (moving GC; bliss-noh).
    egcl_rt::rooted!(keys = Vec::<EgclVal>::new());
    egcl_rt::rooted!(vals = Vec::<EgclVal>::new());
    let mut arg_i = 0usize;
    let mut rest_target: Option<EgclVal> = None;
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
                        return Err(EgclError::Internal(
                            "MACROLET: &REST requires a parameter".into(),
                        ));
                    }
                    rest_target = Some(params_vec[i + 1]);
                    break;
                }
                _ if name.starts_with('&') => {
                    return Err(EgclError::Internal(format!(
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
            egcl_rt::value::NIL
        } else {
            return Err(EgclError::Internal(
                "MACROLET: too few arguments for local macro".into(),
            ));
        };
        keys.push(param);
        vals.push(value);
        i += 1;
    }

    if let Some(rest) = rest_target {
        let rest_val = vec_to_cons(&args_vec[arg_i..]);
        keys.push(rest);
        vals.push(rest_val);
    } else if arg_i != args_vec.len() {
        return Err(EgclError::Internal(
            "MACROLET: too many arguments for local macro".into(),
        ));
    }

    let mut bindings = Vec::with_capacity(keys.len());
    for i in 0..keys.len() {
        bindings.push((keys[i], VariableInfo::Constant(vals[i])));
    }
    Ok(bindings)
}

fn eval_local_macro_body(
    body: EgclVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root across the allocating eval recursion (moving GC; bliss-noh).
    egcl_rt::rooted!(forms = cons_to_vec(body));
    let mut result = egcl_rt::value::NIL;
    for i in 0..forms.len() {
        result = eval_local_macro_form(forms[i], env, call_env)?;
    }
    Ok(result)
}

fn eval_local_macro_form(
    form: EgclVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<EgclVal, EgclError> {
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

    let mut operator = unsafe { cons_car(form) };
    // Root across the allocating eval recursion / macroexpand_1 (moving GC; bliss-noh).
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    let args = unsafe { cons_cdr(form) };
    let op_name = get_symbol_name(operator).unwrap_or_default();
    match op_name.as_str() {
        "QUOTE" => Ok(if args.is_cons() {
            unsafe { cons_car(args) }
        } else {
            egcl_rt::value::NIL
        }),
        "LIST" => {
            egcl_rt::rooted!(src = cons_to_vec(args));
            egcl_rt::rooted!(out = Vec::new());
            for i in 0..src.len() {
                out.push(eval_local_macro_form(src[i], env, call_env)?);
            }
            Ok(vec_to_cons(&out))
        }
        "CONS" => {
            egcl_rt::rooted!(items = cons_to_vec(args));
            if items.len() != 2 {
                return Err(EgclError::Internal("MACROLET: CONS expects 2 args".into()));
            }
            let mut car_val = eval_local_macro_form(items[0], env, call_env)?;
            egcl_rt::rooted_ref!(_car_root = &mut car_val);
            let cdr_val = eval_local_macro_form(items[1], env, call_env)?;
            Ok(alloc_cons(car_val, cdr_val))
        }
        "APPEND" => eval_local_macro_append(args, env, call_env),
        "PROGN" => eval_local_macro_body(args, env, call_env),
        "IF" => {
            egcl_rt::rooted!(forms = cons_to_vec(args));
            if forms.len() < 2 || forms.len() > 3 {
                return Err(EgclError::Internal(
                    "MACROLET: IF expects two or three arguments".into(),
                ));
            }
            let test = eval_local_macro_form(forms[0], env, call_env)?;
            if !test.is_nil() {
                eval_local_macro_form(forms[1], env, call_env)
            } else if forms.len() == 3 {
                eval_local_macro_form(forms[2], env, call_env)
            } else {
                Ok(egcl_rt::value::NIL)
            }
        }
        // UIOP's ENSURE-PATHNAME macrolet turns a constraint name into the
        // corresponding keyword with `(intern* constraint :keyword)`.  Local
        // macro bodies run in this deliberately small, pure evaluator, so
        // support that deterministic subset without calling back into the
        // host interpreter during compilation.
        name if name == "INTERN*" || name.ends_with("::INTERN*") => {
            eval_local_macro_keyword_intern(args, env, call_env)
        }
        "EGCL::QUASIQUOTE" => expand_local_quasiquote(
            if args.is_cons() {
                unsafe { cons_car(args) }
            } else {
                egcl_rt::value::NIL
            },
            env,
            call_env,
        ),
        _ => {
            let (expanded, did_expand) = macroexpand_1(form, call_env)?;
            if did_expand {
                eval_local_macro_form(expanded, env, call_env)
            } else {
                // This mini-evaluator only knows how to run macrolet expander
                // bodies built from quote/list/cons/append/progn/quasiquote and
                // (macro) calls. A cons whose operator is a special form (flet,
                // let, if, …) or an ordinary function (reduce, mapcar, …) must be
                // *evaluated* to produce the expansion, which we cannot do here.
                // Returning it unevaluated would emit garbage code that references
                // the expander's lexicals at runtime (e.g. asdf's `=?` macrolet,
                // whose flet+reduce body leaves `accessors` free → an "unbound
                // variable ACCESSORS" at load). Fail instead, so the caller (the
                // portable lowerer's macrolet pre-expansion) bails and the full
                // tree-walker — which can evaluate the expander — handles it.
                let op = get_symbol_name(operator).unwrap_or_else(|| "?".to_string());
                Err(EgclError::Internal(format!(
                    "MACROLET: expander body uses `{op}`, which the local-macro \
                     mini-evaluator cannot evaluate"
                )))
            }
        }
    }
}

fn eval_local_macro_keyword_intern(
    args: EgclVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(forms = cons_to_vec(args));
    if forms.len() < 2 || forms.len() > 3 {
        return Err(EgclError::Internal(
            "MACROLET: INTERN* expects two or three arguments".into(),
        ));
    }

    egcl_rt::rooted!(values = Vec::<EgclVal>::with_capacity(forms.len()));
    for i in 0..forms.len() {
        values.push(eval_local_macro_form(forms[i], env, call_env)?);
    }
    let package = local_macro_string_designator(values[1]).ok_or_else(|| {
        EgclError::Internal("MACROLET: INTERN* package is not a string designator".into())
    })?;
    if package != "KEYWORD" {
        return Err(EgclError::Internal(format!(
            "MACROLET: INTERN* only supports the KEYWORD package, got {package}"
        )));
    }
    let name = local_macro_string_designator(values[0]).ok_or_else(|| {
        EgclError::Internal("MACROLET: INTERN* name is not a string designator".into())
    })?;
    Ok(EgclVal::from_symbol_index(intern_symbol(&format!(
        "KEYWORD:{name}"
    ))))
}

fn local_macro_string_designator(value: EgclVal) -> Option<String> {
    if value.is_string() {
        return Some(value.as_string());
    }
    let key = get_symbol_name(value)?;
    Some(
        egcl_rt::symbols::split_registry_key(&key)
            .map(|(_, name)| name)
            .unwrap_or(&key)
            .to_string(),
    )
}

fn eval_local_macro_append(
    args: EgclVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<EgclVal, EgclError> {
    // Root result and parts across the allocating eval recursion (moving GC; bliss-noh).
    let mut result = egcl_rt::value::NIL;
    egcl_rt::rooted_ref!(_result_root = &mut result);
    egcl_rt::rooted!(parts = cons_to_vec(args));
    for idx in (0..parts.len()).rev() {
        let part = parts[idx];
        egcl_rt::rooted!(items = cons_to_vec(eval_local_macro_form(part, env, call_env,)?));
        while let Some(item) = items.pop() {
            result = alloc_cons(item, result);
        }
    }
    Ok(result)
}

fn expand_local_quasiquote(
    mut form: EgclVal,
    env: &Environment,
    call_env: &Environment,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_form_root = &mut form);
    if !form.is_cons() {
        return Ok(form);
    }

    let mut operator = unsafe { cons_car(form) };
    egcl_rt::rooted_ref!(_operator_root = &mut operator);
    if is_symbol_named(operator, "EGCL::UNQUOTE") {
        let mut args = unsafe { cons_cdr(form) };
        egcl_rt::rooted_ref!(_args_root = &mut args);
        return Ok(if args.is_cons() {
            eval_local_macro_form(unsafe { cons_car(args) }, env, call_env)?
        } else {
            egcl_rt::value::NIL
        });
    }

    // Root out and cursor across the allocating eval recursion (moving GC; bliss-noh).
    egcl_rt::rooted!(out = Vec::new());
    let mut cursor = form;
    egcl_rt::rooted_ref!(_cursor_root = &mut cursor);
    while cursor.is_cons() {
        let mut item = unsafe { cons_car(cursor) };
        egcl_rt::rooted_ref!(_item_root = &mut item);
        if item.is_cons() && is_symbol_named(unsafe { cons_car(item) }, "EGCL::UNQUOTE-SPLICING") {
            let mut splice_args = unsafe { cons_cdr(item) };
            egcl_rt::rooted_ref!(_splice_args_root = &mut splice_args);
            let mut splice_form = if splice_args.is_cons() {
                unsafe { cons_car(splice_args) }
            } else {
                egcl_rt::value::NIL
            };
            egcl_rt::rooted_ref!(_splice_form_root = &mut splice_form);
            let mut splice_value = eval_local_macro_form(splice_form, env, call_env)?;
            egcl_rt::rooted_ref!(_splice_value_root = &mut splice_value);
            out.extend(cons_to_vec(splice_value));
        } else {
            let expanded_item = expand_local_quasiquote(item, env, call_env)?;
            out.push(expanded_item);
        }
        cursor = unsafe { cons_cdr(cursor) };
    }
    Ok(vec_to_cons(&out))
}

#[cfg(test)]
mod registry_key_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cached_parent_keeps_uninterned_symbol_keys_visible() {
        let symbol = EgclVal::from_symbol_index(0x8000_1234);
        let mut parent = Environment::null();
        parent.functions.insert(symbol.0, FunctionInfo::Lexical);
        let parent = Arc::new(parent.mark_no_gc_roots());
        let mut child = Environment::child_of(parent, vec![], vec![], vec![]);
        let mut seen = false;
        child.visit_gc_roots(&mut |slot| {
            seen |= unsafe { *slot } == symbol;
        });
        assert!(seen, "cached parent skipped a collectible symbol");
    }

    #[test]
    fn environment_traces_symbol_identities_in_keys_and_declarations() {
        use egcl_rt::value::NIL;
        let keys: Vec<_> = (0..10)
            .map(|n| EgclVal::from_symbol_index(0x8000_1000 + n).0)
            .collect();
        let mut env = Environment::null();
        env.variables.insert(keys[0], VariableInfo::Lexical);
        env.functions.insert(keys[1], FunctionInfo::Lexical);
        env.blocks.insert(keys[2]);
        env.tags.insert(keys[3]);
        env.declarations = vec![
            DeclInfo::Declaration(keys[4]),
            DeclInfo::Type(keys[5], NIL),
            DeclInfo::Ignore(keys[6]),
            DeclInfo::Ignorable(keys[7]),
            DeclInfo::Dynamic(keys[8]),
            DeclInfo::Custom(keys[9], NIL),
        ];
        let mut seen = HashSet::new();
        env.visit_gc_roots(&mut |slot| {
            seen.insert(unsafe { *slot }.0);
        });
        for key in keys {
            assert!(
                seen.contains(&key),
                "compiler environment did not trace {key:#x}"
            );
        }
    }

    /// bliss-6b2 regression: `MACRO_FUNCTION_REGISTRY` is a single table keyed by
    /// `key.0`, and BOTH macrolet-local expanders (via `enclose`) and the
    /// interpreter's global-macro handles must draw their keys from the one
    /// `next_registered_macro_key` counter. When two independent fixnum counters
    /// (both starting at 1) fed this table, a macrolet-local macro's expander
    /// silently overwrote a global macro's at the same numeric key — an asdf
    /// macrolet `check` clobbering global `defvar`, so `(defvar x v)` expanded to
    /// `check`'s `(when x (unless v (err x)))` and `x` was read unbound.
    ///
    /// This asserts freshly minted keys are distinct and that registering under
    /// each leaves BOTH expanders independently retrievable — the property the
    /// unified counter guarantees.
    #[test]
    fn distinct_keys_do_not_clobber_each_other() {
        let k1 = next_registered_macro_key();
        let k2 = next_registered_macro_key();
        assert_ne!(k1.0, k2.0, "registry keys must be unique");

        let f1: Arc<MacroFn> = Arc::new(|_form, _env| Ok(EgclVal::from_fixnum(111)));
        let f2: Arc<MacroFn> = Arc::new(|_form, _env| Ok(EgclVal::from_fixnum(222)));
        register_macro_function(k1, f1);
        register_macro_function(k2, f2);

        let g1 = lookup_macro_function(k1).expect("k1 registered");
        let g2 = lookup_macro_function(k2).expect("k2 registered");
        let dummy = egcl_rt::value::NIL;
        let env = Environment::null();
        assert_eq!(g1(dummy, &env).unwrap().0, EgclVal::from_fixnum(111).0);
        assert_eq!(g2(dummy, &env).unwrap().0, EgclVal::from_fixnum(222).0);
    }

    /// bliss-skx regression: `default_hook` looks up its `expander` argument in
    /// `MACRO_FUNCTION_REGISTRY` (keyed by `key.0`). When keys were bare fixnums
    /// minted from a counter, a literal expander (e.g. a fixnum symbol-macro
    /// expansion) whose `.0` aliased a live key was misread as a macro handle and
    /// the WRONG expander ran. Macro keys are now SPECIAL-tagged handles
    /// (`from_macro_handle`), so no fixnum value can occupy a key's bit pattern.
    ///
    /// This registers many keys, then passes fixnum expanders straight through
    /// `default_hook` and asserts each is returned verbatim (the symbol-macro
    /// contract) rather than dispatched to a registered function.
    #[test]
    fn fixnum_expander_never_aliases_a_macro_key() {
        // Mint and register enough keys to cover the low fixnum range that a
        // symbol-macro expansion would land in.
        let mut keys = Vec::new();
        for i in 0..64i64 {
            let k = next_registered_macro_key();
            // Keys must never carry the fixnum tag, or a plain integer could alias
            // them.
            assert!(
                k.is_macro_handle(),
                "key must be a macro handle, not a fixnum"
            );
            assert!(!k.is_fixnum(), "macro key must not be fixnum-tagged");
            let sentinel = 900_000 + i; // distinct from any fixnum expander below
            let f: Arc<MacroFn> = Arc::new(move |_form, _env| Ok(EgclVal::from_fixnum(sentinel)));
            register_macro_function(k, f);
            keys.push(k);
        }

        let env = Environment::null();
        // Every small fixnum, routed through default_hook as its own expander
        // (the symbol-macro calling convention), must come back unchanged — never
        // a sentinel from a registered function.
        for n in 0..64i64 {
            let expansion = EgclVal::from_fixnum(n);
            let got = default_hook(expansion, expansion, &env).unwrap();
            assert_eq!(
                got.0, expansion.0,
                "fixnum expansion {n} was misdispatched to a registered macro fn"
            );
        }

        // And the real handles still dispatch to their functions.
        for (i, k) in keys.iter().enumerate() {
            let got = default_hook(*k, egcl_rt::value::NIL, &env).unwrap();
            assert_eq!(got.0, EgclVal::from_fixnum(900_000 + i as i64).0);
        }
    }
}
