//! T0 bytecode interpreter — slice 1: spine + fallback harness (bliss-nmq.1).
//!
//! This is the first slice of the unified control-stack execution model
//! (spec §2.4.4/§2.4.5). The tree-walker in `cli.rs` runs every CL activation
//! on the OS worker's Rust shadow stack; a recursive host-language interpreter
//! can never share the per-green-thread `BlissStack` (spec §2.4.5 note — the
//! *recursive + host-language* combination is exactly the two-stack condition
//! bliss-nmq removes).
//!
//! This module builds the minimal explicit-stack alternative: a compiler that
//! lowers a small set of forms to a linear bytecode, and a dispatch loop whose
//! `CALL`/`RETURN` push and pop D2.03 frames on the `BlissStack`. Because the
//! loop is iterative over an explicit control stack, N levels of CL recursion
//! consume N `BlissStack` frames and only O(1) Rust stack — so deep recursion
//! raises `STORAGE-CONDITION` (R2.20) instead of overflowing the host stack.
//!
//! ## Discipline (spec §4.6, the SBCL/ECL two-backend model)
//!
//! The bytecode backend is opt-in behind `BLISS_BACKEND=bytecode`. The
//! tree-walker (`super::eval_form`) is the oracle and the fallback: any form
//! the compiler does not yet lower is executed by the tree-walker (compile-time
//! bail), and leaf primitives / unknown callees are delegated to the
//! tree-walker's `apply_function`. So the bytecode backend is *never wrong*:
//! for supported forms it runs native bytecode on the `BlissStack`; for the
//! rest it is exactly the tree-walker. Differential testing (see
//! `tests/bytecode_differential.rs`) asserts the two backends agree.
//!
//! Later slices grow coverage: nmq.4 (non-local control flow), nmq.5
//! (closures / multiple values / special vars), nmq.6 (parity + default flip),
//! nmq.2 (codegen via i2c/c2i), nmq.3 (precise GC of frames).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bliss_compiler::macroexpand::{self as compiler_macroexpand, Environment as MacroexpandEnv};
use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_rt::stack::StackMapEntry;
use bliss_rt::{CodeInfo, Frame};

use super::{
    Env, EnvFrame, HandlerCluster, HandlerEntry, HandlerImpl, RestartEntry, RestartFunction,
    apply_function,
    arena_cons, bliss_error_to_condition, condition_matches_handler, cp, eval_form,
    handler_case_token, list_to_vec, next_control_token, resolve_sym, restart_invoked_name,
    run_handler_bind_handlers, store_control_value, sym_name, tag_key, take_control_value,
    val_as_str, vec_to_list,
};

// ── Backend selection ──────────────────────────────────────────────

/// Whether the bytecode backend is enabled.
///
/// The bytecode backend is the **default** (nmq.6); it compiles what it can and
/// falls back to the tree-walker for the rest, so behaviour is identical. Set
/// `BLISS_BACKEND` to `tree-walker` / `treewalker` / `tw` / `interp` to force
/// the pure tree-walker (used as the differential-testing oracle). Read once and
/// cached.
pub fn backend_is_bytecode() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("BLISS_BACKEND") {
        Ok(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "tree-walker" | "treewalker" | "tree_walker" | "tw" | "interp" | "walker"
        ),
        Err(_) => true,
    })
}

// ── Bytecode ───────────────────────────────────────────────────────

/// A single bytecode instruction. Operand-stack based; see the dispatch loop
/// in [`run`] for the exact semantics of each.
#[derive(Clone, Debug)]
enum Instr {
    /// Push `constants[idx]`.
    Const(u16),
    /// Push the value of local slot `idx`.
    LoadLocal(u16),
    /// Pop and store into local slot `idx`.
    StoreLocal(u16),
    /// Push the dynamic/global value of symbol `sym` (spec `LOAD_SPECIAL`).
    LoadGlobal(u32),
    /// Pop and store into the dynamic/global value of `sym` (`STORE_SPECIAL`).
    StoreGlobal(u32),
    /// Pop `n` values, set them as the multiple values, and push the primary
    /// (`VALUES`). `n = 0` pushes NIL.
    SetValues(u16),
    /// Clear the pending multiple values (single-value context).
    ClearMv,
    /// Pop the primary; read the current multiple values; store the first
    /// `nvars` of them (primary, then secondaries, NIL-padded) into local slots
    /// `slot_base..slot_base+nvars` (`MULTIPLE-VALUE-BIND`).
    TakeValuesToLocals { nvars: u16, slot_base: u16 },
    /// Pop the primary; push a fresh list of the current multiple values
    /// (`MULTIPLE-VALUE-LIST`).
    ValuesToList,
    /// Evaluate a constant form via the tree-walker and push the result. Used
    /// for non-capturing `(lambda …)` / `(function …)`: the resulting closure
    /// is callable by both backends (`funcall`/`apply`/`mapcar` go host).
    EvalHost(u16),

    // ── Captured locals / closures (nmq.5) ─────────────────────────
    /// Push a boxed (closure-captured) variable from the heap `EnvFrame`.
    LoadEnvVar(u16),
    /// Pop and store into an existing boxed variable (`setq`).
    StoreEnvVar(u16),
    /// Pop and bind a boxed variable in the current heap `EnvFrame`.
    DefineEnvVar(u16),
    /// Enter a fresh child `EnvFrame` (a `let` that binds captured variables).
    PushEnvChild,
    /// Leave the current child `EnvFrame`.
    PopEnvChild,
    /// Evaluate a `(lambda …)` constant with `env.frame` bound to this
    /// activation's heap `EnvFrame`, so the closure captures it (shared, live).
    MakeClosureEnv(u16),
    /// Discard the top of the operand stack.
    Pop,
    /// Duplicate the top of the operand stack.
    Dup,
    /// Unconditional jump: set the bytecode pointer to `target`.
    Br(u32),
    /// Pop; if it is NIL, jump to `target`, else fall through.
    BrIfFalse(u32),
    /// Pop; if it is non-NIL, jump to `target`, else fall through.
    BrIfTrue(u32),
    /// Call `sym` with `nargs` operands. Resolves to a bytecode function
    /// (native frame push) or falls back to the tree-walker's `apply_function`.
    CallNamed { sym: u32, nargs: u16 },
    /// Return the top of the operand stack to the caller.
    Return,

    // ── Non-local control flow (nmq.4) — unwind-table opcodes ──────
    /// Pop a tag and establish a `CATCH` handler. Generates a control token
    /// (shared with the tree-walker via `env.catch_stack`), so a `THROW` from
    /// either backend lands here, resumes at `resume_bcp`, resets the operand
    /// stack to `sp_restore`, and pushes the thrown value.
    PushCatch { resume_bcp: u32, sp_restore: u16 },
    /// Establish a `BLOCK` exit handler keyed by the lexical `block_id`;
    /// `name_idx` names the block for `env.block_stack` (tree-walker interop).
    PushBlock {
        block_id: u32,
        name_idx: u16,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// Establish a `TAGBODY` handler keyed by the lexical `tagbody_id`.
    PushTag { tagbody_id: u32, sp_restore: u16 },
    /// Establish an `UNWIND-PROTECT` cleanup handler; on any unwind through it
    /// the cleanup at `cleanup_bcp` runs (operand stack reset to `sp_restore`).
    PushUnwind { cleanup_bcp: u32, sp_restore: u16 },
    /// Remove the most-recently established handler (normal completion).
    PopHandler,
    /// Pop a tag and a value and throw: unwind to the matching `CATCH`.
    Throw,
    /// Pop a value and return from the lexical block `block_id`.
    ReturnFrom { block_id: u32 },
    /// Transfer to tag `target_bcp` within tagbody `tagbody_id`, running any
    /// intervening `UNWIND-PROTECT` cleanups.
    Go { tagbody_id: u32, target_bcp: u32 },
    /// Normal-path `UNWIND-PROTECT`: save the protected value, run the cleanup
    /// at `cleanup_bcp`, then resume at `resume_bcp` with the value restored.
    EnterCleanupNormal { cleanup_bcp: u32, resume_bcp: u32 },
    /// End of a cleanup body: act on the saved continuation (resume normally,
    /// or continue an in-progress unwind).
    CleanupReturn,
    /// Establish a `HANDLER-CASE` cluster (registered in `env.handlers` so a
    /// host-signalled condition finds it) — `hc` indexes the static clause
    /// table. A matching condition unwinds into the selected clause body.
    PushHandlerCase { hc: u32, sp_restore: u16 },
    /// Normal completion of `HANDLER-CASE`: disestablish the cluster.
    PopHandlerCase,
    /// Establish a `HANDLER-BIND` cluster (`hb` indexes the static binding
    /// table). Handlers run in the signalling context via the shared machinery.
    PushHandlerBind { hb: u32 },
    /// Normal completion of `HANDLER-BIND`: disestablish the cluster.
    PopHandlerBind,
    /// Establish a `RESTART-CASE` (`rc` indexes the static restart table),
    /// registered in `env.restarts` for INVOKE-RESTART. `resume_bcp` is where a
    /// delivered restart result resumes.
    PushRestartCase {
        rc: u32,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// Normal completion of `RESTART-CASE`: disestablish the restarts.
    PopRestartCase,
}

/// A lowered CL function: a linear bytecode plus its constant pool and frame
/// shape. The frame's value-slot area holds `n_locals` lexical slots followed
/// by `max_stack` operand slots (D2.03).
#[derive(Debug)]
pub struct BytecodeFunction {
    code: Vec<Instr>,
    constants: Vec<BlissVal>,
    /// Static per-`handler-case` clause tables (indexed by `PushHandlerCase`).
    handler_cases: Vec<HandlerCaseInfo>,
    /// Static per-`handler-bind` binding tables (indexed by `PushHandlerBind`).
    handler_binds: Vec<HandlerBindInfo>,
    /// Interned block names (referenced by `PushBlock` for `env.block_stack`).
    names: Vec<String>,
    /// Static per-`restart-case` tables (indexed by `PushRestartCase`).
    restart_cases: Vec<RestartCaseInfo>,
    /// Per-parameter `(name, location)` for the entry sequence.
    param_layout: Vec<(String, VarLoc)>,
    /// Whether this function needs a heap `EnvFrame` (has captured locals).
    has_env: bool,
    /// Number of lexical local slots (params + `let` bindings).
    n_locals: u16,
    /// Maximum operand-stack depth.
    max_stack: u16,
    /// Fixed argument count (slice 1 lowers only fixed lambda lists).
    arity: u16,
    /// Function name, for debugging.
    #[allow(dead_code)]
    name: String,
}

impl BytecodeFunction {
    /// Total value-slot count reserved in the frame (locals + operand stack).
    fn num_slots(&self) -> u16 {
        self.n_locals + self.max_stack
    }
}

/// Static description of one `handler-case` form: its clauses plus the PC to
/// resume at after the whole form.
#[derive(Debug, Clone)]
struct HandlerCaseInfo {
    clauses: Vec<ClauseInfo>,
}

/// Static description of one `handler-case` clause.
#[derive(Debug, Clone)]
struct ClauseInfo {
    /// Condition type name the clause handles (`T` = catch-all).
    type_name: String,
    /// Bytecode PC of the clause body.
    body_bcp: u32,
    /// Local slot the condition is bound to, if the clause has a variable.
    var_slot: Option<u16>,
}

/// Static description of one `handler-bind` form: `(type . handler-form)` pairs.
/// The handler form is stored raw (unevaluated) exactly as the tree-walker does,
/// so the shared signal machinery invokes it identically.
#[derive(Debug, Clone)]
struct HandlerBindInfo {
    bindings: Vec<(String, BlissVal)>,
}

/// Where a lexical variable lives: a fast frame slot, or boxed in the shared
/// heap `EnvFrame` because a closure captures it.
#[derive(Debug, Clone, Copy)]
enum VarLoc {
    Slot(u16),
    Boxed,
}

/// Static description of one `restart-case` form. Each restart's clause is a
/// `(lambda params . body)` form run by the shared INVOKE-RESTART machinery
/// (in `env.frame`); the bytecode only catches the restart-invoked transfer and
/// delivers the stored result.
#[derive(Debug, Clone)]
struct RestartCaseInfo {
    restarts: Vec<(String, BlissVal)>,
}

// ── Per-thread registry of compiled functions ─────────────────────

thread_local! {
    /// Bytecode functions keyed by symbol index. A `CallNamed` checks this
    /// first; a hit runs as a native frame on the `BlissStack`, a miss falls
    /// back to `apply_function` (builtins, generics, tree-walker functions).
    static REGISTRY: RefCell<HashMap<u32, Rc<BytecodeFunction>>> = RefCell::new(HashMap::new());
}

/// Build a proper list `(items...)` in the arena, for synthesising macro-style
/// expansions during lowering (bliss-jtc.28, e.g. DOTIMES → block/tagbody).
fn form_list(items: &[BlissVal]) -> BlissVal {
    let mut acc = NIL;
    for &x in items.iter().rev() {
        acc = arena_cons(x, acc);
    }
    acc
}

fn registry_get(sym: u32) -> Option<Rc<BytecodeFunction>> {
    REGISTRY.with(|r| r.borrow().get(&sym).cloned())
}

fn registry_put(sym: u32, f: Rc<BytecodeFunction>) {
    REGISTRY.with(|r| r.borrow_mut().insert(sym, f));
}

fn registry_remove(sym: u32) {
    REGISTRY.with(|r| r.borrow_mut().remove(&sym));
}

// ── Lowering ───────────────────────────────────────────────────────

/// A form the compiler does not (yet) lower. Propagated up to trigger a
/// clean bail to the tree-walker.
struct Bail;

type LowerResult<T> = Result<T, Bail>;

/// Primitives whose call semantics the tree-walker owns; a `CallNamed` to one
/// of these delegates to `apply_function`. The allowlist keeps slice 1 *safe*:
/// the compiler only emits a call when it is certain the callee is a real
/// function (a user `defun`, or one of these), never a macro or a special
/// operator masquerading as a call.
const PRIMITIVE_ALLOWLIST: &[&str] = &[
    "+", "-", "*", "/", "<", ">", "<=", ">=", "=", "/=", "1+", "1-", "CAR", "CDR", "CONS", "LIST",
    "NULL", "NOT", "EQ", "EQL", "EQUAL", "ZEROP", "PLUSP", "MINUSP", "ABS", "MIN", "MAX", "MOD",
    "REM", "CONSP", "ATOM", "LISTP", "EVENP", "ODDP", "GCD", "EXPT", "FLOOR", "CEILING", "TRUNCATE",
    "VALUES-LIST", "IDENTITY", "FIRST", "REST", "SECOND", "THIRD", "LENGTH", "APPEND", "REVERSE",
    // Condition-signalling functions (ordinary functions, normal arg order) —
    // reachable via apply_function, so safe to call from bytecode.
    "ERROR",
    "SIGNAL",
    "WARN",
    "CERROR",
    "INVOKE-RESTART",
    "MAKE-CONDITION",
    // Higher-order application functions (apply a closure/function value).
    // Only the simple applicators without &key/&test arguments are safe through
    // apply_function's synthesize path; sequence functions taking :key/:test
    // (sort, remove-if, find-if, reduce, ...) are left to bail to the tree-walker.
    "FUNCALL",
    "APPLY",
    "MAPCAR",
    "MAPC",
    "MAPCAN",
    "MAPCON",
    "MAPLIST",
    // I/O functions (fixed positional args) — common in recursive bodies.
    "FORMAT",
    "PRINT",
    "PRINC",
    "PRIN1",
    "WRITE-STRING",
    "WRITE-LINE",
    "TERPRI",
    "WRITE-CHAR",
    "PRINC-TO-STRING",
    "PRIN1-TO-STRING",
    "WRITE-TO-STRING",
    "FRESH-LINE",
];

/// Compiler state for lowering one function body.
struct Lowerer<'e> {
    code: Vec<Instr>,
    constants: Vec<BlissVal>,
    /// Lexical scope: name → variable location. A `Vec` of frames so `let`
    /// bindings shadow correctly and unbind at scope exit.
    scopes: Vec<HashMap<String, VarLoc>>,
    /// Names captured by a nested closure — these locals live in the shared
    /// heap `EnvFrame` (boxed) instead of a frame slot.
    captured_names: std::collections::HashSet<String>,
    /// Lexically-visible local functions (flet/labels): name → the gensym symbol
    /// index its compiled body is registered under. A call to such a name lowers
    /// to a bytecode `CallNamed` on that gensym.
    local_fns: std::collections::HashMap<String, u32>,
    /// Lazily-built macro-expansion environment (mirrors `env`'s macros), used
    /// to compile macro forms by expanding then lowering.
    macro_env: Option<MacroexpandEnv>,
    /// Whether this function needs a heap `EnvFrame` (has a boxed local).
    has_env: bool,
    /// Next free local slot index.
    next_local: u16,
    /// Highest local slot index used (frames need this many local slots).
    n_locals: u16,
    /// Current operand-stack depth (compile-time model).
    cur_stack: u16,
    /// Maximum operand-stack depth observed.
    max_stack: u16,
    /// Counter for unique lexical block/tagbody ids.
    next_id: u32,
    /// Lexically enclosing blocks: `(name, block_id)`, innermost last.
    block_scope: Vec<(String, u32)>,
    /// Lexically enclosing tagbodies: `(tagbody_id, tag → target bcp)`.
    tag_scope: Vec<TagScope>,
    /// `Go` instructions awaiting target-bcp patching once their tagbody's tag
    /// positions are known: `(instr_index, tagbody_id, tag_name)`.
    pending_gos: Vec<(usize, u32, String)>,
    /// Static `handler-case` clause tables.
    handler_cases: Vec<HandlerCaseInfo>,
    /// Static `handler-bind` binding tables.
    handler_binds: Vec<HandlerBindInfo>,
    /// Interned block names.
    names: Vec<String>,
    /// Static `restart-case` tables.
    restart_cases: Vec<RestartCaseInfo>,
    env: &'e Env,
}

/// A lexically active tagbody during lowering.
struct TagScope {
    id: u32,
    /// Tag name → bytecode target. `usize::MAX` until the tag is emitted.
    tags: HashMap<String, usize>,
}

impl<'e> Lowerer<'e> {
    fn new(env: &'e Env) -> Self {
        Lowerer {
            code: Vec::new(),
            constants: Vec::new(),
            scopes: vec![HashMap::new()],
            captured_names: std::collections::HashSet::new(),
            local_fns: std::collections::HashMap::new(),
            macro_env: None,
            has_env: false,
            next_local: 0,
            n_locals: 0,
            cur_stack: 0,
            max_stack: 0,
            next_id: 0,
            block_scope: Vec::new(),
            tag_scope: Vec::new(),
            pending_gos: Vec::new(),
            handler_cases: Vec::new(),
            handler_binds: Vec::new(),
            names: Vec::new(),
            restart_cases: Vec::new(),
            env,
        }
    }

    fn intern_name(&mut self, name: &str) -> u16 {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return i as u16;
        }
        let i = self.names.len() as u16;
        self.names.push(name.to_string());
        i
    }

    fn fresh_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn emit(&mut self, i: Instr) {
        self.code.push(i);
    }

    /// Model the operand-stack effect of the next pushes/pops so `max_stack`
    /// is exact.
    fn push_n(&mut self, n: u16) {
        self.cur_stack += n;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
    }

    fn pop_n(&mut self, n: u16) {
        self.cur_stack = self.cur_stack.saturating_sub(n);
    }

    fn add_const(&mut self, v: BlissVal) -> u16 {
        let idx = self.constants.len() as u16;
        self.constants.push(v);
        idx
    }

    /// Allocate a lexical binding for a param or `let` variable: a fast frame
    /// slot, or a boxed heap `EnvFrame` binding if it is captured by a closure.
    fn alloc_local(&mut self, name: &str) -> VarLoc {
        let loc = if self.captured_names.contains(name) {
            self.has_env = true;
            VarLoc::Boxed
        } else {
            let idx = self.next_local;
            self.next_local += 1;
            if self.next_local > self.n_locals {
                self.n_locals = self.next_local;
            }
            VarLoc::Slot(idx)
        };
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), loc);
        loc
    }

    /// Allocate a binding that must live in a frame slot regardless of capture
    /// (e.g. `multiple-value-bind` / `handler-case` clause variables).
    fn alloc_slot(&mut self, name: &str) -> u16 {
        let idx = self.next_local;
        self.next_local += 1;
        if self.next_local > self.n_locals {
            self.n_locals = self.next_local;
        }
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), VarLoc::Slot(idx));
        idx
    }

    fn lookup_local(&self, name: &str) -> Option<VarLoc> {
        for scope in self.scopes.iter().rev() {
            if let Some(&loc) = scope.get(name) {
                return Some(loc);
            }
        }
        None
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Leave a `let` scope, freeing its slots for reuse by later siblings.
    fn exit_scope(&mut self, saved_next_local: u16) {
        self.scopes.pop();
        self.next_local = saved_next_local;
    }

    // ── Expression lowering ────────────────────────────────────────

    /// Lower a form so its single value is left on the operand stack.
    fn lower_expr(&mut self, form: BlissVal) -> LowerResult<()> {
        // Self-evaluating atoms.
        if form.is_nil() || form == T || form.is_fixnum() || form.is_single_float() {
            let c = self.add_const(form);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        if form.is_character() || form.is_string() {
            let c = self.add_const(form);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        // Keyword symbols self-evaluate; other symbols are variable refs.
        if form.is_symbol() {
            let name = sym_name(form);
            if name.starts_with(':') || name.starts_with("KEYWORD:") {
                let c = self.add_const(form);
                self.emit(Instr::Const(c));
                self.push_n(1);
                return Ok(());
            }
            if let Some(loc) = self.lookup_local(&name) {
                match loc {
                    VarLoc::Slot(slot) => self.emit(Instr::LoadLocal(slot)),
                    VarLoc::Boxed => {
                        let ni = self.intern_name(&name);
                        self.emit(Instr::LoadEnvVar(ni));
                    }
                }
                self.push_n(1);
                return Ok(());
            }
            // A global / special / symbol-macro reference. Symbol-macros must
            // expand (tree-walker semantics) — bail on those; otherwise emit a
            // dynamic value load.
            if self.env.symbol_macros.contains_key(&form.as_symbol_index()) {
                return Err(Bail);
            }
            self.emit(Instr::LoadGlobal(form.as_symbol_index()));
            self.push_n(1);
            return Ok(());
        }
        // Cons: special form or call.
        if form.is_cons() {
            let (op, rest) = cp(form);
            if !op.is_symbol() {
                // ((lambda ...) ...) etc. — not lowered in slice 1.
                return Err(Bail);
            }
            let name = sym_name(op);
            match name.as_str() {
                "QUOTE" => {
                    let (datum, _) = cp(rest);
                    let c = self.add_const(datum);
                    self.emit(Instr::Const(c));
                    self.push_n(1);
                    Ok(())
                }
                "IF" => self.lower_if(rest),
                "WHEN" => self.lower_when(rest, false),
                "UNLESS" => self.lower_when(rest, true),
                "AND" => self.lower_and(rest),
                "OR" => self.lower_or(rest),
                "COND" => self.lower_cond(rest),
                "CASE" => self.lower_case(rest),
                "PROGN" => self.lower_progn(rest),
                // Declarations are no-ops at runtime; yield NIL.
                "DECLARE" => {
                    let c = self.add_const(NIL);
                    self.emit(Instr::Const(c));
                    self.push_n(1);
                    Ok(())
                }
                // (the type expr) — evaluate expr, ignore the type declaration.
                "THE" => {
                    let (_type, r) = cp(rest);
                    if r.is_cons() {
                        self.lower_expr(cp(r).0)
                    } else {
                        Err(Bail)
                    }
                }
                // (locally decl... body...) — declarations lower to NIL no-ops.
                "LOCALLY" => self.lower_progn(rest),
                "LET" => self.lower_let(rest, false),
                "LET*" => self.lower_let(rest, true),
                "SETQ" => self.lower_setq(rest),
                "BLOCK" => self.lower_block(rest),
                "RETURN-FROM" => self.lower_return_from(rest),
                "RETURN" => self.lower_return(rest),
                "CATCH" => self.lower_catch(rest),
                "THROW" => self.lower_throw(rest),
                "TAGBODY" => self.lower_tagbody(rest),
                "GO" => self.lower_go(rest),
                "DOTIMES" => self.lower_dotimes(rest),
                "DOLIST" => self.lower_dolist(rest),
                "LOOP" => self.lower_loop(rest),
                "UNWIND-PROTECT" => self.lower_unwind_protect(rest),
                "HANDLER-CASE" => self.lower_handler_case(rest),
                "HANDLER-BIND" => self.lower_handler_bind(rest),
                "RESTART-CASE" => self.lower_restart_case(rest),
                "VALUES" => self.lower_values(rest),
                "MULTIPLE-VALUE-BIND" => self.lower_mvb(rest),
                "MULTIPLE-VALUE-LIST" => self.lower_mvlist(rest),
                "LAMBDA" => self.lower_lambda(op, rest),
                "FUNCTION" => self.lower_function(rest),
                "FLET" => self.lower_flet(rest, false),
                "LABELS" => self.lower_flet(rest, true),
                _ => self.lower_call(&name, op, rest),
            }
        } else {
            // Any other object type self-evaluates.
            let c = self.add_const(form);
            self.emit(Instr::Const(c));
            self.push_n(1);
            Ok(())
        }
    }

    fn lower_if(&mut self, rest: BlissVal) -> LowerResult<()> {
        let parts = list_to_vec(rest);
        if parts.len() < 2 || parts.len() > 3 {
            return Err(Bail);
        }
        // test
        self.lower_expr(parts[0])?;
        self.emit(Instr::BrIfFalse(0)); // patched
        let br_if_false_at = self.code.len() - 1;
        self.pop_n(1);
        // then
        self.lower_expr(parts[1])?;
        self.emit(Instr::Br(0)); // patched
        let br_at = self.code.len() - 1;
        self.pop_n(1); // then-value is conceptually the result; model branches independently
        // else
        let else_pc = self.code.len() as u32;
        if parts.len() == 3 {
            self.lower_expr(parts[2])?;
        } else {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
        }
        let end_pc = self.code.len() as u32;
        self.code[br_if_false_at] = Instr::BrIfFalse(else_pc);
        self.code[br_at] = Instr::Br(end_pc);
        Ok(())
    }

    fn lower_progn(&mut self, rest: BlissVal) -> LowerResult<()> {
        let forms = list_to_vec(rest);
        if forms.is_empty() {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        let n = forms.len();
        for (i, f) in forms.into_iter().enumerate() {
            self.lower_expr(f)?;
            if i + 1 < n {
                // Discard non-final values.
                self.emit(Instr::Pop);
                self.pop_n(1);
            }
        }
        Ok(())
    }

    /// `(when test body...)` / `(unless test body...)`.
    fn lower_when(&mut self, rest: BlissVal, negate: bool) -> LowerResult<()> {
        let (test, body) = cp(rest);
        let base = self.cur_stack;
        self.lower_expr(test)?;
        if negate {
            // UNLESS: run the body when the test is NIL. Branch on the test:
            // BrIfFalse jumps to the body; the true path yields NIL.
            self.emit(Instr::BrIfFalse(0));
            let to_body = self.code.len() - 1;
            self.pop_n(1);
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            self.emit(Instr::Br(0));
            let to_end = self.code.len() - 1;
            let body_pc = self.code.len() as u32;
            self.cur_stack = base;
            self.lower_progn(body)?;
            let end = self.code.len() as u32;
            self.code[to_body] = Instr::BrIfFalse(body_pc);
            self.code[to_end] = Instr::Br(end);
        } else {
            self.emit(Instr::BrIfFalse(0));
            let to_else = self.code.len() - 1;
            self.pop_n(1);
            self.lower_progn(body)?;
            self.emit(Instr::Br(0));
            let to_end = self.code.len() - 1;
            let else_pc = self.code.len() as u32;
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            let end = self.code.len() as u32;
            self.code[to_else] = Instr::BrIfFalse(else_pc);
            self.code[to_end] = Instr::Br(end);
        }
        self.cur_stack = base + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(and a b ...)` — short-circuit; NIL on the first false, else the last.
    fn lower_and(&mut self, rest: BlissVal) -> LowerResult<()> {
        let args = list_to_vec(rest);
        let base = self.cur_stack;
        if args.is_empty() {
            let c = self.add_const(T);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        let n = args.len();
        let mut false_jumps = Vec::new();
        for (i, a) in args.into_iter().enumerate() {
            self.lower_expr(a)?;
            if i + 1 < n {
                self.emit(Instr::BrIfFalse(0)); // pops; jump to the NIL result
                false_jumps.push(self.code.len() - 1);
                self.pop_n(1);
            }
        }
        // Last value is on the stack (the result of a successful AND).
        self.emit(Instr::Br(0));
        let to_end = self.code.len() - 1;
        let false_pc = self.code.len() as u32;
        let c = self.add_const(NIL);
        self.emit(Instr::Const(c));
        let end = self.code.len() as u32;
        for j in false_jumps {
            self.code[j] = Instr::BrIfFalse(false_pc);
        }
        self.code[to_end] = Instr::Br(end);
        self.cur_stack = base + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(or a b ...)` — short-circuit; the first true value, else the last.
    fn lower_or(&mut self, rest: BlissVal) -> LowerResult<()> {
        let args = list_to_vec(rest);
        let base = self.cur_stack;
        if args.is_empty() {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        let n = args.len();
        let mut end_jumps = Vec::new();
        for (i, a) in args.into_iter().enumerate() {
            self.lower_expr(a)?;
            if i + 1 < n {
                self.emit(Instr::Dup);
                self.push_n(1);
                self.emit(Instr::BrIfFalse(0)); // pop the copy; if false, try next
                let to_next = self.code.len() - 1;
                self.pop_n(1);
                self.emit(Instr::Br(0)); // truthy: keep the value, done
                end_jumps.push(self.code.len() - 1);
                let next_pc = self.code.len() as u32;
                self.code[to_next] = Instr::BrIfFalse(next_pc);
                self.emit(Instr::Pop); // discard the false value before the next
                self.pop_n(1);
            }
        }
        let end = self.code.len() as u32;
        for j in end_jumps {
            self.code[j] = Instr::Br(end);
        }
        self.cur_stack = base + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(case key (vals body...)... (otherwise body...))` — evaluate the key
    /// once, EQL-compare against each clause's designator(s).
    fn lower_case(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (key_form, clauses_form) = cp(rest);
        let clauses = list_to_vec(clauses_form);
        let base = self.cur_stack;

        // Evaluate the key once into a temporary slot.
        self.lower_expr(key_form)?;
        let key_slot = self.alloc_slot("__case_key__");
        self.emit(Instr::StoreLocal(key_slot));
        self.pop_n(1);

        let eql_sym = resolve_sym("EQL").ok_or(Bail)?.as_symbol_index();
        let mut end_jumps = Vec::new();
        for clause in clauses {
            if !clause.is_cons() {
                return Err(Bail);
            }
            let (designator, body) = cp(clause);
            self.cur_stack = base;
            let is_default = (designator.is_symbol()
                && matches!(sym_name(designator).as_str(), "OTHERWISE" | "T"))
                || designator == T;
            if is_default {
                self.lower_progn(body)?;
                self.emit(Instr::Br(0));
                end_jumps.push(self.code.len() - 1);
                break; // default is terminal
            }
            // Designator is a single object or a list of objects.
            let keys = if designator.is_cons() {
                list_to_vec(designator)
            } else {
                vec![designator]
            };
            let mut to_body = Vec::new();
            for k in keys {
                self.emit(Instr::LoadLocal(key_slot));
                self.push_n(1);
                let c = self.add_const(k);
                self.emit(Instr::Const(c));
                self.push_n(1);
                self.emit(Instr::CallNamed {
                    sym: eql_sym,
                    nargs: 2,
                });
                self.pop_n(2);
                self.push_n(1);
                self.emit(Instr::BrIfTrue(0));
                to_body.push(self.code.len() - 1);
                self.pop_n(1);
            }
            // No key matched → skip to the next clause.
            self.emit(Instr::Br(0));
            let to_next = self.code.len() - 1;
            let body_pc = self.code.len() as u32;
            for j in to_body {
                self.code[j] = Instr::BrIfTrue(body_pc);
            }
            self.cur_stack = base;
            self.lower_progn(body)?;
            self.emit(Instr::Br(0));
            end_jumps.push(self.code.len() - 1);
            let next_pc = self.code.len() as u32;
            self.code[to_next] = Instr::Br(next_pc);
        }
        // Fell through all clauses with no default → NIL.
        self.cur_stack = base;
        let c = self.add_const(NIL);
        self.emit(Instr::Const(c));
        let end = self.code.len() as u32;
        for j in end_jumps {
            self.code[j] = Instr::Br(end);
        }
        self.cur_stack = base + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(cond (test body...)...)` — nested IF; a testless-body clause yields the
    /// test value.
    fn lower_cond(&mut self, rest: BlissVal) -> LowerResult<()> {
        let clauses = list_to_vec(rest);
        let base = self.cur_stack;
        let mut end_jumps = Vec::new();
        for clause in clauses {
            if !clause.is_cons() {
                return Err(Bail);
            }
            let (test, body) = cp(clause);
            self.cur_stack = base;
            self.lower_expr(test)?; // test value on stack
            if body.is_nil() {
                // (cond (test)) — the test's value is the result if non-NIL.
                self.emit(Instr::Dup);
                self.push_n(1);
                self.emit(Instr::BrIfFalse(0));
                let to_next = self.code.len() - 1;
                self.pop_n(1);
                self.emit(Instr::Br(0));
                end_jumps.push(self.code.len() - 1);
                let next_pc = self.code.len() as u32;
                self.code[to_next] = Instr::BrIfFalse(next_pc);
                self.emit(Instr::Pop);
                self.pop_n(1);
            } else {
                self.emit(Instr::BrIfFalse(0));
                let to_next = self.code.len() - 1;
                self.pop_n(1);
                self.lower_progn(body)?;
                self.emit(Instr::Br(0));
                end_jumps.push(self.code.len() - 1);
                let next_pc = self.code.len() as u32;
                self.code[to_next] = Instr::BrIfFalse(next_pc);
            }
        }
        // No clause matched → NIL.
        self.cur_stack = base;
        let c = self.add_const(NIL);
        self.emit(Instr::Const(c));
        let end = self.code.len() as u32;
        for j in end_jumps {
            self.code[j] = Instr::Br(end);
        }
        self.cur_stack = base + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    fn lower_let(&mut self, rest: BlissVal, sequential: bool) -> LowerResult<()> {
        let (bindings, body) = cp(rest);
        let binding_forms = list_to_vec(bindings);

        // A special (dynamically-scoped) variable must be bound in the global
        // value cell so called functions see it; the bytecode path binds only
        // lexical locals, so bail to the tree-walker (which dynamic-binds
        // correctly) whenever a binding names a special — bliss-lb6.14, ASDF's
        // `(let ((*asdf-session* …)) …)` read by helper functions.
        if binding_forms
            .iter()
            .any(|b| binding_name_init(*b).map(|(n, _)| is_special_name(&n)).unwrap_or(false))
        {
            return Err(Bail);
        }

        let saved_next_local = self.next_local;

        // A let that binds any captured variable needs a fresh child EnvFrame so
        // each entry captures a distinct binding.
        let has_boxed = binding_forms.iter().any(|b| {
            binding_name_init(*b)
                .map(|(n, _)| self.captured_names.contains(&n))
                .unwrap_or(false)
        });
        if has_boxed {
            self.has_env = true;
            self.emit(Instr::PushEnvChild);
        }

        // Emit the store for one binding based on its location.
        let store = |lo: &mut Self, name: &str, loc: VarLoc| match loc {
            VarLoc::Slot(slot) => lo.emit(Instr::StoreLocal(slot)),
            VarLoc::Boxed => {
                let ni = lo.intern_name(name);
                lo.emit(Instr::DefineEnvVar(ni));
            }
        };

        if sequential {
            // LET*: each init sees prior bindings.
            self.enter_scope();
            for b in &binding_forms {
                let (name, init) = binding_name_init(*b)?;
                self.lower_expr(init)?;
                let loc = self.alloc_local(&name);
                store(self, &name, loc);
                self.pop_n(1);
            }
        } else {
            // LET (parallel): evaluate all inits in the outer scope, then bind.
            let mut names = Vec::new();
            for b in &binding_forms {
                let (name, init) = binding_name_init(*b)?;
                self.lower_expr(init)?; // compiled against the *current* scope
                names.push(name);
            }
            self.enter_scope();
            let mut locs = Vec::with_capacity(names.len());
            for name in &names {
                locs.push((name.clone(), self.alloc_local(name)));
            }
            // Values are on the stack in binding order; store in reverse.
            for (name, loc) in locs.into_iter().rev() {
                store(self, &name, loc);
                self.pop_n(1);
            }
        }

        // Body as an implicit progn.
        let body_forms = list_to_vec(body);
        if body_forms.is_empty() {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
        } else {
            let n = body_forms.len();
            for (i, f) in body_forms.into_iter().enumerate() {
                self.lower_expr(f)?;
                if i + 1 < n {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            }
        }
        self.exit_scope(saved_next_local);
        if has_boxed {
            self.emit(Instr::PopEnvChild);
        }
        Ok(())
    }

    fn lower_call(&mut self, name: &str, op: BlissVal, rest: BlissVal) -> LowerResult<()> {
        // A lexically-visible local function (flet/labels): call its compiled
        // body directly by the gensym it is registered under.
        if let Some(&sym) = self.local_fns.get(name) {
            let args = list_to_vec(rest);
            let nargs = args.len();
            if nargs > u16::MAX as usize {
                return Err(Bail);
            }
            for a in args {
                self.lower_expr(a)?;
            }
            self.emit(Instr::CallNamed {
                sym,
                nargs: nargs as u16,
            });
            self.pop_n(nargs as u16);
            self.push_n(1);
            return Ok(());
        }
        // A macro: expand one level (with the same macro functions the
        // tree-walker uses) and lower the expansion. lower_expr recurses, so a
        // macro that expands to another macro is handled too.
        if super::macro_defined(self.env, name) {
            let form = arena_cons(op, rest);
            if self.macro_env.is_none() {
                self.macro_env = Some(super::macroexpand_environment_from_cli(self.env));
            }
            let menv = self.macro_env.as_ref().unwrap();
            match compiler_macroexpand::macroexpand_1(form, menv) {
                Ok((expanded, true)) => return self.lower_expr(expanded),
                _ => return Err(Bail),
            }
        }
        // An unhandled special operator is not a call.
        if is_bail_special(name) {
            return Err(Bail);
        }
        // Only emit a call when the callee is certainly a function: a
        // user-defined function (lexical name map or global function cell —
        // bliss-jtc.6.8) or an allowlisted primitive.
        let is_user_fn = self.env.funs.contains_key(name) || super::global_fn(name).is_some();
        let is_prim = PRIMITIVE_ALLOWLIST.contains(&name);
        if !is_user_fn && !is_prim {
            return Err(Bail);
        }
        let args = list_to_vec(rest);
        let nargs = args.len();
        if nargs > u16::MAX as usize {
            return Err(Bail);
        }
        for a in args {
            self.lower_expr(a)?;
        }
        self.emit(Instr::CallNamed {
            sym: op.as_symbol_index(),
            nargs: nargs as u16,
        });
        // Call pops nargs and pushes one result.
        self.pop_n(nargs as u16);
        self.push_n(1);
        Ok(())
    }

    /// `(setq var val ...)` — store into lexical local slots only; any special
    /// or global assignment bails (a nmq.5 concern).
    fn lower_setq(&mut self, rest: BlissVal) -> LowerResult<()> {
        let items = list_to_vec(rest);
        if items.is_empty() {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        if items.len() % 2 != 0 {
            return Err(Bail);
        }
        let npairs = items.len() / 2;
        for i in 0..npairs {
            let var = items[2 * i];
            let val = items[2 * i + 1];
            if !var.is_symbol() {
                return Err(Bail);
            }
            let name = sym_name(var);
            // A symbol-macro `setq` is really a `setf` of the expansion — bail.
            if self.env.symbol_macros.contains_key(&var.as_symbol_index()) {
                return Err(Bail);
            }
            let last = i + 1 == npairs;
            self.lower_expr(val)?; // +1
            match self.lookup_local(&name) {
                Some(VarLoc::Slot(slot)) => {
                    self.emit(Instr::StoreLocal(slot));
                    self.pop_n(1);
                    // SETQ is not multiple-value-preserving.
                    self.emit(Instr::ClearMv);
                    if last {
                        self.emit(Instr::LoadLocal(slot));
                        self.push_n(1);
                    }
                }
                Some(VarLoc::Boxed) => {
                    let ni = self.intern_name(&name);
                    self.emit(Instr::StoreEnvVar(ni));
                    self.pop_n(1);
                    self.emit(Instr::ClearMv);
                    if last {
                        self.emit(Instr::LoadEnvVar(ni));
                        self.push_n(1);
                    }
                }
                None => {
                    let sym = var.as_symbol_index();
                    self.emit(Instr::StoreGlobal(sym));
                    self.pop_n(1);
                    self.emit(Instr::ClearMv);
                    if last {
                        self.emit(Instr::LoadGlobal(sym));
                        self.push_n(1);
                    }
                }
            }
        }
        Ok(())
    }

    // ── Non-local control flow lowering (nmq.4) ────────────────────

    /// `(block name body...)` — establish a lexical exit, run the body.
    fn lower_block(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (name_form, body) = cp(rest);
        if !name_form.is_symbol() && !name_form.is_nil() {
            return Err(Bail);
        }
        let name = sym_name(name_form);
        let block_id = self.fresh_id();
        let name_idx = self.intern_name(&name);
        let sp_restore = self.cur_stack;
        self.emit(Instr::PushBlock {
            block_id,
            name_idx,
            resume_bcp: 0,
            sp_restore,
        });
        let push_at = self.code.len() - 1;
        self.block_scope.push((name, block_id));
        self.lower_progn(body)?; // body value on stack (+1)
        self.block_scope.pop();
        self.emit(Instr::PopHandler);
        let after = self.code.len() as u32;
        if let Instr::PushBlock { resume_bcp, .. } = &mut self.code[push_at] {
            *resume_bcp = after;
        }
        Ok(())
    }

    /// `(return-from name value?)` — non-local exit to a lexical block.
    fn lower_return_from(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (name_form, vrest) = cp(rest);
        if !name_form.is_symbol() && !name_form.is_nil() {
            return Err(Bail);
        }
        let name = sym_name(name_form);
        let block_id = match self.block_scope.iter().rev().find(|(n, _)| *n == name) {
            Some((_, id)) => *id,
            // Block is not lexically in this function (a closed-over block is a
            // nmq.5 concern) — bail.
            None => return Err(Bail),
        };
        let val_form = if vrest.is_cons() { cp(vrest).0 } else { NIL };
        self.lower_expr(val_form)?; // +1
        self.emit(Instr::ReturnFrom { block_id });
        // ReturnFrom transfers control; model it as consuming the value and
        // notionally yielding one (the trailing slot is dead code).
        self.pop_n(1);
        self.push_n(1);
        Ok(())
    }

    /// `(return value?)` == `(return-from nil value?)`.
    fn lower_return(&mut self, rest: BlissVal) -> LowerResult<()> {
        let block_id = match self.block_scope.iter().rev().find(|(n, _)| n == "NIL") {
            Some((_, id)) => *id,
            None => return Err(Bail),
        };
        let val_form = if rest.is_cons() { cp(rest).0 } else { NIL };
        self.lower_expr(val_form)?;
        self.emit(Instr::ReturnFrom { block_id });
        self.pop_n(1);
        self.push_n(1);
        Ok(())
    }

    /// `(catch tag body...)` — dynamic non-local exit; shares the tree-walker's
    /// `env.catch_stack` control-token protocol for cross-backend interop.
    fn lower_catch(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (tag_form, body) = cp(rest);
        self.lower_expr(tag_form)?; // +1 (tag on stack)
        let sp_restore = self.cur_stack - 1; // depth after PushCatch consumes the tag
        self.emit(Instr::PushCatch {
            resume_bcp: 0,
            sp_restore,
        });
        let push_at = self.code.len() - 1;
        self.pop_n(1); // PushCatch pops the tag
        self.lower_progn(body)?; // body value on stack (+1)
        self.emit(Instr::PopHandler);
        let after = self.code.len() as u32;
        if let Instr::PushCatch { resume_bcp, .. } = &mut self.code[push_at] {
            *resume_bcp = after;
        }
        Ok(())
    }

    /// `(throw tag value)` — dynamic transfer to a matching `catch`.
    fn lower_throw(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (tag_form, r2) = cp(rest);
        if !r2.is_cons() {
            return Err(Bail);
        }
        let (val_form, _) = cp(r2);
        self.lower_expr(tag_form)?; // tag
        self.lower_expr(val_form)?; // value (on top)
        self.emit(Instr::Throw);
        self.pop_n(2);
        self.push_n(1);
        Ok(())
    }

    /// `(tagbody {tag | statement}*)` — statements run in order, `go` jumps to
    /// a tag, the form returns NIL.
    fn lower_tagbody(&mut self, rest: BlissVal) -> LowerResult<()> {
        let items = list_to_vec(rest);
        let tagbody_id = self.fresh_id();
        let sp_restore = self.cur_stack;
        self.emit(Instr::PushTag {
            tagbody_id,
            sp_restore,
        });
        let push_at = self.code.len() - 1;

        // Pre-register tag names so forward `go`s resolve to this scope.
        let mut tags: HashMap<String, usize> = HashMap::new();
        for item in &items {
            if let Some(name) = tag_key(*item) {
                tags.entry(name).or_insert(usize::MAX);
            }
        }
        self.tag_scope.push(TagScope {
            id: tagbody_id,
            tags,
        });

        for item in &items {
            if let Some(name) = tag_key(*item) {
                let bcp = self.code.len();
                if let Some(scope) = self.tag_scope.last_mut() {
                    scope.tags.insert(name, bcp);
                }
            } else {
                self.lower_expr(*item)?; // +1
                self.emit(Instr::Pop);
                self.pop_n(1);
            }
        }

        let scope = self.tag_scope.pop().unwrap();
        // `push_at` marks where tags become inactive on normal exit.
        let _ = push_at;
        self.emit(Instr::PopHandler);

        // Patch `go`s targeting this tagbody now that tag PCs are known.
        let mut i = 0;
        while i < self.pending_gos.len() {
            let (idx, id, tag) = self.pending_gos[i].clone();
            if id == tagbody_id {
                let target = *scope.tags.get(&tag).ok_or(Bail)?;
                if target == usize::MAX {
                    return Err(Bail);
                }
                if let Instr::Go { target_bcp, .. } = &mut self.code[idx] {
                    *target_bcp = target as u32;
                }
                self.pending_gos.remove(i);
            } else {
                i += 1;
            }
        }

        // TAGBODY returns NIL.
        let c = self.add_const(NIL);
        self.emit(Instr::Const(c));
        self.push_n(1);
        Ok(())
    }

    /// `(go tag)` — transfer to a tag in a lexically enclosing tagbody.
    fn lower_go(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (tag_form, _) = cp(rest);
        let name = match tag_key(tag_form) {
            Some(n) => n,
            None => return Err(Bail),
        };
        let tagbody_id = match self
            .tag_scope
            .iter()
            .rev()
            .find(|s| s.tags.contains_key(&name))
        {
            Some(s) => s.id,
            // Tag not lexically visible (closed-over tagbody is a nmq.5 concern).
            None => return Err(Bail),
        };
        self.emit(Instr::Go {
            tagbody_id,
            target_bcp: 0,
        });
        let idx = self.code.len() - 1;
        self.pending_gos.push((idx, tagbody_id, name));
        self.push_n(1); // notional (go never yields)
        Ok(())
    }

    /// `(dotimes (var count [result]) body...)` (bliss-jtc.28). Lowered by
    /// building the standard block/let/tagbody expansion and recursing through
    /// the already-tested lowering, so idiomatic counting loops become
    /// promotable bytecode instead of falling back to the tree-walker:
    ///
    ///   (block nil
    ///     (let ((var 0) (limit count))
    ///       (tagbody top (when (< var limit) body... (setq var (+ var 1)) (go top)))
    ///       (setq var limit)   ; result-form sees var = count (CLHS)
    ///       result))
    fn lower_dotimes(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (binding, body) = cp(rest);
        if !binding.is_cons() {
            return Err(Bail);
        }
        let (var, br) = cp(binding);
        if !var.is_symbol() {
            return Err(Bail);
        }
        let (count_form, result_rest) = cp(br);
        let result = if result_rest.is_cons() { cp(result_rest).0 } else { NIL };

        let id = self.fresh_id();
        let limit = resolve_sym(&format!("%DOTIMES-LIMIT{id}")).ok_or(Bail)?;
        let top = resolve_sym(&format!("%DOTIMES-TOP{id}")).ok_or(Bail)?;
        let s = |n: &str| resolve_sym(n).ok_or(Bail);

        let test = form_list(&[s("<")?, var, limit]);
        let mut when_items = vec![s("WHEN")?, test];
        when_items.extend(list_to_vec(body));
        when_items.push(form_list(&[
            s("SETQ")?,
            var,
            form_list(&[s("+")?, var, BlissVal::from_fixnum(1)]),
        ]));
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        let bindings = form_list(&[
            form_list(&[var, BlissVal::from_fixnum(0)]),
            form_list(&[limit, count_form]),
        ]);
        let let_form = form_list(&[
            s("LET")?,
            bindings,
            tagbody_form,
            form_list(&[s("SETQ")?, var, limit]),
            result,
        ]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// `(dolist (var list [result]) body...)` (bliss-jtc.28). Expansion:
    ///
    ///   (block nil
    ///     (let ((var nil) (rest list))
    ///       (tagbody top (when rest (setq var (car rest)) body...
    ///                                (setq rest (cdr rest)) (go top)))
    ///       (setq var nil)   ; result-form sees var = nil (CLHS)
    ///       result))
    fn lower_dolist(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (binding, body) = cp(rest);
        if !binding.is_cons() {
            return Err(Bail);
        }
        let (var, br) = cp(binding);
        if !var.is_symbol() {
            return Err(Bail);
        }
        let (list_form, result_rest) = cp(br);
        let result = if result_rest.is_cons() { cp(result_rest).0 } else { NIL };

        let id = self.fresh_id();
        let rest_var = resolve_sym(&format!("%DOLIST-REST{id}")).ok_or(Bail)?;
        let top = resolve_sym(&format!("%DOLIST-TOP{id}")).ok_or(Bail)?;
        let s = |n: &str| resolve_sym(n).ok_or(Bail);

        let mut when_items = vec![
            s("WHEN")?,
            rest_var,
            form_list(&[s("SETQ")?, var, form_list(&[s("CAR")?, rest_var])]),
        ];
        when_items.extend(list_to_vec(body));
        when_items.push(form_list(&[
            s("SETQ")?,
            rest_var,
            form_list(&[s("CDR")?, rest_var]),
        ]));
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        let bindings = form_list(&[
            form_list(&[var, NIL]),
            form_list(&[rest_var, list_form]),
        ]);
        let let_form = form_list(&[
            s("LET")?,
            bindings,
            tagbody_form,
            form_list(&[s("SETQ")?, var, NIL]),
            result,
        ]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// `(loop form*)` — only the *simple* loop form (bliss-jtc.28 follow-up):
    /// every form is a compound form and the loop repeats them until an explicit
    /// RETURN/RETURN-FROM. Lowered to `(block nil (tagbody top form* (go top)))`
    /// so it promotes like any other loop. The *extended* LOOP (with atomic
    /// keywords such as FOR/WHILE/COLLECT) is left to the tree-walker.
    fn lower_loop(&mut self, rest: BlissVal) -> LowerResult<()> {
        let forms = list_to_vec(rest);
        // Simple loop iff there is at least one form and all are compound. A bare
        // atom (a loop keyword or an atom clause) means the extended grammar.
        if forms.is_empty() || !forms.iter().all(|f| f.is_cons()) {
            return Err(Bail);
        }
        let id = self.fresh_id();
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let s = |n: &str| resolve_sym(n).ok_or(Bail);

        let mut tb = vec![s("TAGBODY")?, top];
        tb.extend(forms);
        tb.push(form_list(&[s("GO")?, top]));
        let block = form_list(&[s("BLOCK")?, NIL, form_list(&tb)]);
        self.lower_expr(block)
    }

    /// `(unwind-protect protected cleanup...)` — the cleanup runs on both the
    /// normal path and any non-local exit through the protected form.
    fn lower_unwind_protect(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (protected, cleanup_forms) = cp(rest);
        let sp_restore = self.cur_stack;
        self.emit(Instr::PushUnwind {
            cleanup_bcp: 0,
            sp_restore,
        });
        let push_at = self.code.len() - 1;
        self.lower_expr(protected)?; // V on stack (+1)
        self.emit(Instr::PopHandler);
        self.emit(Instr::EnterCleanupNormal {
            cleanup_bcp: 0,
            resume_bcp: 0,
        });
        let enter_at = self.code.len() - 1;
        self.pop_n(1); // EnterCleanupNormal saves V off-stack
        self.emit(Instr::Br(0)); // resume point (Br after)
        let br_at = self.code.len() - 1;

        // Cleanup body (values discarded).
        let cleanup_bcp = self.code.len() as u32;
        let saved = self.cur_stack;
        for f in list_to_vec(cleanup_forms) {
            self.lower_expr(f)?;
            self.emit(Instr::Pop);
            self.pop_n(1);
        }
        self.cur_stack = saved;
        self.emit(Instr::CleanupReturn);
        let after = self.code.len() as u32;

        // Patch targets.
        if let Instr::PushUnwind { cleanup_bcp: c, .. } = &mut self.code[push_at] {
            *c = cleanup_bcp;
        }
        if let Instr::EnterCleanupNormal {
            cleanup_bcp: c,
            resume_bcp: r,
        } = &mut self.code[enter_at]
        {
            *c = cleanup_bcp;
            *r = br_at as u32;
        }
        if let Instr::Br(t) = &mut self.code[br_at] {
            *t = after;
        }

        // Net effect of the whole form: the protected value V (restored at `after`).
        self.cur_stack = sp_restore + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(handler-case protected (type (var?) body...)...)` — on a matching
    /// condition signalled during `protected`, unwind into the clause body with
    /// the condition bound to `var`.
    fn lower_handler_case(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (protected, clauses_form) = cp(rest);
        let clauses = list_to_vec(clauses_form);
        // `:no-error` clauses are a different protocol — bail if present.
        for clause in &clauses {
            let (type_form, _) = cp(*clause);
            if type_form.is_symbol() && sym_name(type_form).ends_with("NO-ERROR") {
                return Err(Bail);
            }
        }

        let sp_restore = self.cur_stack;
        let hc = self.handler_cases.len() as u32;
        self.emit(Instr::PushHandlerCase { hc, sp_restore });
        // Reserve the table slot; filled in once bodies are laid out.
        self.handler_cases.push(HandlerCaseInfo {
            clauses: Vec::new(),
        });

        self.lower_expr(protected)?; // protected value V (+1)
        self.emit(Instr::PopHandlerCase);
        self.emit(Instr::Br(0)); // skip clause bodies
        let normal_br = self.code.len() - 1;

        let mut clause_infos = Vec::new();
        let mut clause_brs = Vec::new();
        for clause in &clauses {
            let (type_form, clause_rest) = cp(*clause);
            if !type_form.is_symbol() && !type_form.is_nil() {
                return Err(Bail); // compound type specifiers not yet handled
            }
            let type_name = sym_name(type_form);
            let (bind_list, body) = cp(clause_rest);
            let saved_next_local = self.next_local;
            self.enter_scope();
            let var_slot = if bind_list.is_cons() {
                let (var, _) = cp(bind_list);
                if !var.is_symbol() {
                    return Err(Bail);
                }
                Some(self.alloc_slot(&sym_name(var)))
            } else {
                None
            };

            let body_bcp = self.code.len() as u32;
            // The driver lands here with the operand stack at `sp_restore` and
            // the condition already stored in `var_slot`.
            self.cur_stack = sp_restore;
            self.lower_progn(body)?; // clause value (+1)
            // The tree-walker runs the clause in a child env, so a clause's
            // secondary values do not propagate out of the handler-case.
            self.emit(Instr::ClearMv);
            self.exit_scope(saved_next_local);
            self.emit(Instr::Br(0));
            clause_brs.push(self.code.len() - 1);

            clause_infos.push(ClauseInfo {
                type_name,
                body_bcp,
                var_slot,
            });
        }

        let after = self.code.len() as u32;
        if let Instr::Br(t) = &mut self.code[normal_br] {
            *t = after;
        }
        for br in clause_brs {
            if let Instr::Br(t) = &mut self.code[br] {
                *t = after;
            }
        }
        self.handler_cases[hc as usize].clauses = clause_infos;

        // The whole form yields one value (protected's, or a clause's).
        self.cur_stack = sp_restore + 1;
        if self.cur_stack > self.max_stack {
            self.max_stack = self.cur_stack;
        }
        Ok(())
    }

    /// `(handler-bind ((type handler-form)...) body...)` — handlers run in the
    /// signalling context (no unwind unless a handler transfers control).
    fn lower_handler_bind(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (bindings_form, body) = cp(rest);
        let mut bindings = Vec::new();
        for binding in list_to_vec(bindings_form) {
            if !binding.is_cons() {
                return Err(Bail);
            }
            let (type_form, handler_rest) = cp(binding);
            if !type_form.is_symbol() && !type_form.is_nil() {
                return Err(Bail);
            }
            let handler_form = if handler_rest.is_cons() {
                cp(handler_rest).0
            } else {
                return Err(Bail);
            };
            bindings.push((sym_name(type_form), handler_form));
        }

        let hb = self.handler_binds.len() as u32;
        self.handler_binds.push(HandlerBindInfo { bindings });
        self.emit(Instr::PushHandlerBind { hb });
        self.lower_progn(body)?; // body value (+1)
        self.emit(Instr::PopHandlerBind);
        Ok(())
    }

    /// `(restart-case expr (name (params) body...)...)` — establish restarts,
    /// run `expr`; an INVOKE-RESTART transfers the restart's result out.
    ///
    /// Restart clause bodies run via the shared INVOKE-RESTART machinery in
    /// `env.frame` (the tree-walker's model), which cannot see this compiled
    /// function's BlissStack locals. So we conservatively bail the whole form
    /// if any clause body might reference an enclosing lexical local — those
    /// restart-cases run correctly on the tree-walker instead.
    fn lower_restart_case(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (restartable_form, clauses_form) = cp(rest);
        let clauses = list_to_vec(clauses_form);

        let enclosing_locals: std::collections::HashSet<String> =
            self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();

        let lambda_sym = resolve_sym("LAMBDA").ok_or(Bail)?;
        let mut restarts = Vec::new();
        for clause in &clauses {
            let (name_form, clause_rest) = cp(*clause);
            if !name_form.is_symbol() {
                return Err(Bail);
            }
            let name = sym_name(name_form).to_uppercase();
            let (params_form, body) = cp(clause_rest);

            // Conservative check: clause body must reference only its own params
            // and globals, never an enclosing compiled local.
            let params: std::collections::HashSet<String> = list_to_vec(params_form)
                .iter()
                .filter(|p| p.is_symbol())
                .map(|p| sym_name(*p))
                .filter(|n| !n.starts_with('&'))
                .collect();
            let mut used = std::collections::HashSet::new();
            collect_symbol_names(body, &mut used);
            if used
                .iter()
                .any(|u| enclosing_locals.contains(u) && !params.contains(u))
            {
                return Err(Bail);
            }

            let lambda_form = arena_cons(lambda_sym, arena_cons(params_form, body));
            restarts.push((name, lambda_form));
        }

        let rc = self.restart_cases.len() as u32;
        self.restart_cases.push(RestartCaseInfo { restarts });
        let sp_restore = self.cur_stack;
        self.emit(Instr::PushRestartCase {
            rc,
            resume_bcp: 0,
            sp_restore,
        });
        let push_at = self.code.len() - 1;
        self.lower_expr(restartable_form)?; // restartable value (+1)
        self.emit(Instr::PopRestartCase);
        let after = self.code.len() as u32;
        if let Instr::PushRestartCase { resume_bcp, .. } = &mut self.code[push_at] {
            *resume_bcp = after;
        }
        // Net +1 (the restartable form's value, or a delivered restart result).
        Ok(())
    }

    // ── Multiple values (nmq.5) ────────────────────────────────────

    /// `(values v0 v1 ...)` — set the multiple values, leave the primary.
    fn lower_values(&mut self, rest: BlissVal) -> LowerResult<()> {
        let args = list_to_vec(rest);
        if args.len() > u16::MAX as usize {
            return Err(Bail);
        }
        let n = args.len() as u16;
        for a in args {
            self.lower_expr(a)?;
        }
        self.emit(Instr::SetValues(n));
        self.pop_n(n);
        self.push_n(1);
        Ok(())
    }

    /// `(multiple-value-bind (vars...) values-form body...)`.
    fn lower_mvb(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (vars_form, r2) = cp(rest);
        let (values_form, body) = cp(r2);
        let vars = list_to_vec(vars_form);
        for v in &vars {
            if !v.is_symbol() {
                return Err(Bail);
            }
        }
        if vars.len() > u16::MAX as usize {
            return Err(Bail);
        }

        // Single-value context around the values form: clear, evaluate, read.
        self.emit(Instr::ClearMv);
        self.lower_expr(values_form)?; // primary on stack (+1), mv set

        let saved_next_local = self.next_local;
        self.enter_scope();
        let slot_base = self.next_local;
        for v in &vars {
            self.alloc_slot(&sym_name(*v));
        }
        self.emit(Instr::TakeValuesToLocals {
            nvars: vars.len() as u16,
            slot_base,
        });
        self.pop_n(1); // consumes the primary

        self.lower_progn(body)?; // body value (+1)
        self.exit_scope(saved_next_local);
        Ok(())
    }

    /// `(multiple-value-list form)`.
    fn lower_mvlist(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (form, _) = cp(rest);
        self.emit(Instr::ClearMv);
        self.lower_expr(form)?; // primary (+1), mv set
        self.emit(Instr::ValuesToList); // pop primary, push list
        Ok(())
    }

    // ── Closures (nmq.5) ───────────────────────────────────────────

    /// Enclosing lexical locals a `(lambda params body)` captures (free symbols
    /// of the body minus the lambda's own params, restricted to names bound in
    /// an enclosing scope).
    fn lambda_captured_locals(&self, params_form: BlissVal, body: BlissVal) -> Vec<String> {
        let params: std::collections::HashSet<String> = list_to_vec(params_form)
            .iter()
            .filter(|p| p.is_symbol())
            .map(|p| sym_name(*p))
            .filter(|n| !n.starts_with('&'))
            .collect();
        let mut used = std::collections::HashSet::new();
        for f in list_to_vec(body) {
            collect_symbol_names(f, &mut used);
        }
        let enclosing: std::collections::HashSet<&String> =
            self.scopes.iter().flat_map(|s| s.keys()).collect();
        used.into_iter()
            .filter(|u| !params.contains(u) && enclosing.contains(u))
            .collect()
    }

    /// Emit a closure value for `form` (a `(lambda …)` / `(function …)` form)
    /// given the enclosing locals it captures. Non-capturing → EvalHost (the
    /// tree-walker builds it against the global frame). Capturing → MakeClosureEnv
    /// (captures this activation's heap frame) provided every captured local is
    /// boxed; if any is a plain frame slot (e.g. a `multiple-value-bind` var),
    /// bail so the enclosing function runs on the tree-walker.
    fn emit_closure(&mut self, form: BlissVal, captured: &[String]) -> LowerResult<()> {
        let c = self.add_const(form);
        if captured.is_empty() {
            self.emit(Instr::EvalHost(c));
        } else {
            for name in captured {
                match self.lookup_local(name) {
                    Some(VarLoc::Boxed) | None => {}
                    Some(VarLoc::Slot(_)) => return Err(Bail),
                }
            }
            self.has_env = true;
            self.emit(Instr::MakeClosureEnv(c));
        }
        self.push_n(1);
        Ok(())
    }

    /// `(lambda params body...)` — a closure value (capturing or not).
    fn lower_lambda(&mut self, op: BlissVal, rest: BlissVal) -> LowerResult<()> {
        let (params_form, body) = cp(rest);
        let captured = self.lambda_captured_locals(params_form, body);
        let form = arena_cons(op, rest);
        self.emit_closure(form, &captured)
    }

    /// `(function name)` / `#'(lambda …)` — a function designator or a closure.
    fn lower_function(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (target, _) = cp(rest);
        let captured = if target.is_cons() {
            let (t_op, t_rest) = cp(target);
            if t_op.is_symbol() && sym_name(t_op) == "LAMBDA" {
                let (params_form, body) = cp(t_rest);
                self.lambda_captured_locals(params_form, body)
            } else {
                return Err(Bail);
            }
        } else if target.is_symbol() {
            Vec::new()
        } else {
            return Err(Bail);
        };
        let function_sym = resolve_sym("FUNCTION").ok_or(Bail)?;
        let form = arena_cons(function_sym, rest);
        self.emit_closure(form, &captured)
    }

    /// `(flet ((name params body...)...) body...)` / `(labels (...) body...)`.
    ///
    /// Each local function's body is compiled to a `BytecodeFunction`, registered
    /// under a fresh gensym, and calls to it lower to a bytecode `CallNamed` on
    /// that gensym — so local (and mutually) recursive functions run on the
    /// BlissStack, bounded recursion. Local functions must be non-capturing (no
    /// enclosing lexical local) since a `CallNamed` callee gets no heap-frame
    /// inheritance; anything unsupported inside bails the whole form (so no
    /// tree-walker `env.funs` scoping is needed).
    fn lower_flet(&mut self, rest: BlissVal, is_labels: bool) -> LowerResult<()> {
        let (defs_form, body) = cp(rest);
        let defs = list_to_vec(defs_form);

        // Parse (name params . fbody) and gensym each local function.
        let mut parsed: Vec<(String, u32, BlissVal, BlissVal)> = Vec::new();
        for d in &defs {
            if !d.is_cons() {
                return Err(Bail);
            }
            let (name_form, d_rest) = cp(*d);
            if !name_form.is_symbol() {
                return Err(Bail);
            }
            let (params_form, fbody) = cp(d_rest);
            // A colon would be read as a package separator, so keep the gensym
            // name colon-free and unique.
            let gname = next_control_token("__FLET__").replace(':', "_");
            let gensym = resolve_sym(&gname).ok_or(Bail)?.as_symbol_index();
            parsed.push((sym_name(name_form), gensym, params_form, fbody));
        }

        // Non-capturing check: no local function body may reference an enclosing
        // lexical local.
        let enclosing: std::collections::HashSet<String> =
            self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        for (_, _, params_form, fbody) in &parsed {
            let params: std::collections::HashSet<String> = list_to_vec(*params_form)
                .iter()
                .filter(|p| p.is_symbol())
                .map(|p| sym_name(*p))
                .filter(|n| !n.starts_with('&'))
                .collect();
            let mut used = std::collections::HashSet::new();
            for f in list_to_vec(*fbody) {
                collect_symbol_names(f, &mut used);
            }
            if used
                .iter()
                .any(|u| enclosing.contains(u) && !params.contains(u))
            {
                return Err(Bail);
            }
        }

        let saved_local_fns = self.local_fns.clone();

        // LABELS bodies see all siblings + self; FLET bodies do not.
        let body_local_fns = if is_labels {
            let mut m = self.local_fns.clone();
            for (name, sym, _, _) in &parsed {
                m.insert(name.clone(), *sym);
            }
            m
        } else {
            self.local_fns.clone()
        };

        // Compile and register each local function body.
        for (_, sym, params_form, fbody) in &parsed {
            let bf = compile_local_function(*params_form, *fbody, self.env, &body_local_fns)
                .ok_or(Bail)?;
            registry_put(*sym, Rc::new(bf));
        }

        // Compile the form body with all local functions visible.
        for (name, sym, _, _) in &parsed {
            self.local_fns.insert(name.clone(), *sym);
        }
        let r = self.lower_progn(body);
        self.local_fns = saved_local_fns;
        r
    }
}

/// Compile a local (flet/labels) function body to a [`BytecodeFunction`],
/// inheriting the visible local-function namespace so sibling/self calls
/// resolve. Returns `None` on any unsupported form.
fn compile_local_function(
    params_form: BlissVal,
    fbody: BlissVal,
    env: &Env,
    local_fns: &std::collections::HashMap<String, u32>,
) -> Option<BytecodeFunction> {
    let params = list_to_vec(params_form);
    let mut param_names = Vec::new();
    for p in &params {
        if !p.is_symbol() {
            return None;
        }
        let pn = sym_name(*p);
        if pn.starts_with('&') {
            return None;
        }
        param_names.push(pn);
    }
    let mut lo = Lowerer::new(env);
    lo.local_fns = local_fns.clone();
    lo.captured_names = compute_captured_names(fbody);
    let mut param_layout = Vec::with_capacity(param_names.len());
    for pn in &param_names {
        let loc = lo.alloc_local(pn);
        param_layout.push((pn.clone(), loc));
    }
    if lower_body(&mut lo, fbody).is_err() {
        return None;
    }
    lo.emit(Instr::Return);
    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        handler_binds: lo.handler_binds,
        names: lo.names,
        restart_cases: lo.restart_cases,
        param_layout,
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: param_names.len() as u16,
        name: "<flet>".to_string(),
    })
}

/// Collect the names of all symbols appearing in `form` (recursively), except
/// inside `quote`. Used for a conservative free-variable over-approximation.
fn collect_symbol_names(form: BlissVal, out: &mut std::collections::HashSet<String>) {
    if form.is_symbol() {
        out.insert(sym_name(form));
        return;
    }
    if form.is_cons() {
        let (car, cdr) = cp(form);
        if car.is_symbol() && sym_name(car) == "QUOTE" {
            return;
        }
        collect_symbol_names(car, out);
        collect_symbol_names(cdr, out);
    }
}

/// Extract `(name init)` from a `let` binding, which may also be a bare symbol.
/// True if `name` (a symbol name, possibly package-qualified) is spelled as a
/// special variable by the earmuff convention `*…*`. Matches the tree-walker's
/// `is_special_var` so the two agree on which LET bindings are dynamic.
fn is_special_name(name: &str) -> bool {
    let bare = name.rsplit(':').next().unwrap_or(name);
    let b = bare.as_bytes();
    b.len() > 2 && b[0] == b'*' && b[b.len() - 1] == b'*'
}

fn binding_name_init(b: BlissVal) -> LowerResult<(String, BlissVal)> {
    if b.is_symbol() {
        return Ok((sym_name(b), NIL));
    }
    if b.is_cons() {
        let (name, rest) = cp(b);
        if !name.is_symbol() {
            return Err(Bail);
        }
        let init = if rest.is_cons() { cp(rest).0 } else { NIL };
        return Ok((sym_name(name), init));
    }
    Err(Bail)
}

/// ANSI special operators (and Bliss macros treated specially by the
/// tree-walker) that slice 1 does not lower: seeing one as an operator forces
/// a bail rather than a wrong `CallNamed`. `IF`/`LET`/`LET*`/`PROGN`/`QUOTE`
/// are handled directly and so are deliberately absent.
fn is_bail_special(name: &str) -> bool {
    matches!(
        name,
        "SETQ"
            | "SETF"
            | "DEFUN"
            | "DEFMACRO"
            | "DEFVAR"
            | "DEFPARAMETER"
            | "DEFCONSTANT"
            | "MACROLET"
            | "SYMBOL-MACROLET"
            | "EVAL-WHEN"
            | "LOAD-TIME-VALUE"
            | "PROGV"
            | "MULTIPLE-VALUE-CALL"
            | "MULTIPLE-VALUE-PROG1"
            | "TYPECASE"
            | "ECASE"
            | "DO"
            | "DO*"
            | "DOLIST"
            | "DOTIMES"
            | "LOOP"
            | "PROG1"
            | "PROG2"
            | "PROG"
            | "PROG*"
            | "DESTRUCTURING-BIND"
            | "RESTART-BIND"
            | "IGNORE-ERRORS"
            | "WITH-OPEN-FILE"
            | "WITH-SLOTS"
            | "WITH-ACCESSORS"
            | "DEFCLASS"
            | "DEFSTRUCT"
            | "DEFGENERIC"
            | "DEFMETHOD"
            | "DEFPACKAGE"
            | "IN-PACKAGE"
            | "DEFINE-SYMBOL-MACRO"
            | "DEFINE-COMPILER-MACRO"
            | "EVAL"
    )
}

/// Lower a fixed-arity function `(params . body)` to a [`BytecodeFunction`].
/// Returns `None` if the lambda list is non-trivial or the body uses a form
/// slice 1 does not yet handle.
fn compile_function(
    name: &str,
    params_form: BlissVal,
    body: BlissVal,
    env: &Env,
) -> Option<BytecodeFunction> {
    let params = list_to_vec(params_form);
    // Slice 1: only simple fixed lambda lists (no &optional/&rest/&key/&aux).
    let mut param_names = Vec::new();
    for p in &params {
        if !p.is_symbol() {
            return None;
        }
        let pn = sym_name(*p);
        if pn.starts_with('&') {
            return None;
        }
        param_names.push(pn);
    }

    let mut lo = Lowerer::new(env);
    lo.captured_names = compute_captured_names(body);
    let mut param_layout = Vec::with_capacity(param_names.len());
    for pn in &param_names {
        let loc = lo.alloc_local(pn);
        param_layout.push((pn.clone(), loc));
    }
    // Body as an implicit progn producing the return value.
    if lower_body(&mut lo, body).is_err() {
        return None;
    }
    lo.emit(Instr::Return);

    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        handler_binds: lo.handler_binds,
        names: lo.names,
        restart_cases: lo.restart_cases,
        param_layout,
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: param_names.len() as u16,
        name: name.to_string(),
    })
}

/// Names of variables captured by a nested `(lambda …)` in `body` (an
/// over-approximation: a nested lambda's free symbols minus its own params).
/// A function/`let` binding whose name is in this set is boxed into the heap
/// `EnvFrame` so a closure can share it.
fn compute_captured_names(body: BlissVal) -> std::collections::HashSet<String> {
    fn walk(form: BlissVal, out: &mut std::collections::HashSet<String>) {
        if !form.is_cons() {
            return;
        }
        let (car, cdr) = cp(form);
        if car.is_symbol() {
            let n = sym_name(car);
            if n == "LAMBDA" {
                let (params_form, lbody) = cp(cdr);
                let params: std::collections::HashSet<String> = list_to_vec(params_form)
                    .iter()
                    .filter(|p| p.is_symbol())
                    .map(|p| sym_name(*p))
                    .collect();
                let mut used = std::collections::HashSet::new();
                for f in list_to_vec(lbody) {
                    collect_symbol_names(f, &mut used);
                    walk(f, out); // nested lambdas
                }
                for u in used {
                    if !params.contains(&u) {
                        out.insert(u);
                    }
                }
                return;
            }
            if n == "QUOTE" {
                return;
            }
        }
        walk(car, out);
        walk(cdr, out);
    }
    let mut out = std::collections::HashSet::new();
    for f in list_to_vec(body) {
        walk(f, &mut out);
    }
    out
}

/// Lower an implicit-progn body, leaving the last value on the stack.
fn lower_body(lo: &mut Lowerer, body: BlissVal) -> LowerResult<()> {
    let forms = list_to_vec(body);
    if forms.is_empty() {
        let c = lo.add_const(NIL);
        lo.emit(Instr::Const(c));
        lo.push_n(1);
        return Ok(());
    }
    let n = forms.len();
    for (i, f) in forms.into_iter().enumerate() {
        lo.lower_expr(f)?;
        if i + 1 < n {
            lo.emit(Instr::Pop);
            lo.pop_n(1);
        }
    }
    Ok(())
}

/// Compile a top-level form as a zero-argument thunk. Returns `None` on bail.
fn compile_thunk(form: BlissVal, env: &Env) -> Option<BytecodeFunction> {
    let mut lo = Lowerer::new(env);
    lo.captured_names = compute_captured_names(arena_cons(form, NIL));
    if lo.lower_expr(form).is_err() {
        return None;
    }
    lo.emit(Instr::Return);
    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        handler_binds: lo.handler_binds,
        names: lo.names,
        restart_cases: lo.restart_cases,
        param_layout: Vec::new(),
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: 0,
        name: "<toplevel>".to_string(),
    })
}

// ── BFASL Bytecode Unit serialization ─────────────────────────────

const BBU_MAGIC: &[u8; 4] = b"BBU\0";
const BBU_BYTECODE_VERSION: u16 = 0x0100;
const BBU_VERIFIER_VERSION: u16 = 0x0100;
const BBU_NO_INDEX: u32 = u32::MAX;

const BBU_FUNC_NAMED: u32 = 1 << 0;
const BBU_FUNC_LOAD_TIME_THUNK: u32 = 1 << 3;

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Default)]
struct BbuConstPool {
    entries: Vec<Vec<u8>>,
    index: HashMap<Vec<u8>, u32>,
}

impl BbuConstPool {
    fn intern_encoded(&mut self, bytes: Vec<u8>) -> u32 {
        if let Some(&idx) = self.index.get(&bytes) {
            return idx;
        }
        let idx = self.entries.len() as u32;
        self.entries.push(bytes.clone());
        self.index.insert(bytes, idx);
        idx
    }

    fn string(&mut self, s: &str) -> u32 {
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 8);
        put_u32(&mut bytes, s.len() as u32);
        bytes.extend_from_slice(s.as_bytes());
        self.intern_encoded(bytes)
    }

    fn package(&mut self, name: &str) -> u32 {
        let name_ref = self.string(&name.to_uppercase());
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 10);
        put_u32(&mut bytes, name_ref);
        put_u32(&mut bytes, 0);
        self.intern_encoded(bytes)
    }

    fn symbol_name_parts(name: &str) -> (String, String, bool) {
        if let Some(rest) = name
            .strip_prefix("KEYWORD:")
            .or_else(|| name.strip_prefix(':'))
        {
            return ("KEYWORD".to_string(), rest.to_uppercase(), true);
        }
        if let Some((pkg, bare)) = name.rsplit_once("::") {
            return (pkg.to_uppercase(), bare.to_uppercase(), false);
        }
        if let Some((pkg, bare)) = name.rsplit_once(':') {
            return (pkg.to_uppercase(), bare.to_uppercase(), false);
        }
        ("COMMON-LISP".to_string(), name.to_uppercase(), false)
    }

    fn symbol_by_name(&mut self, name: &str) -> u32 {
        let (pkg, bare, keyword) = Self::symbol_name_parts(name);
        if keyword {
            let name_ref = self.string(&bare);
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 12);
            put_u32(&mut bytes, name_ref);
            return self.intern_encoded(bytes);
        }

        let package_ref = self.package(&pkg);
        let name_ref = self.string(&bare);
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 11);
        put_u32(&mut bytes, package_ref);
        put_u32(&mut bytes, name_ref);
        put_u8(&mut bytes, 0);
        self.intern_encoded(bytes)
    }

    fn symbol_by_index(&mut self, idx: u32) -> u32 {
        let name = reader::symbol_name(idx).unwrap_or_else(|| format!("SYM#{idx}"));
        self.symbol_by_name(&name)
    }

    fn value(&mut self, v: BlissVal) -> Option<u32> {
        if v.is_nil() {
            return Some(self.intern_encoded(vec![0]));
        }
        if v == T {
            return Some(self.intern_encoded(vec![1]));
        }
        if v.is_fixnum() {
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 2);
            put_i64(&mut bytes, v.as_fixnum());
            return Some(self.intern_encoded(bytes));
        }
        if v.is_single_float() {
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 5);
            bytes.extend_from_slice(&v.as_single_float().to_bits().to_le_bytes());
            return Some(self.intern_encoded(bytes));
        }
        if v.is_character() {
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 7);
            put_u32(&mut bytes, v.as_char() as u32);
            return Some(self.intern_encoded(bytes));
        }
        if v.is_string() {
            return Some(self.string(&val_as_str(v)));
        }
        if v.is_symbol() {
            return Some(self.symbol_by_index(v.as_symbol_index()));
        }
        if v.is_cons() {
            let (car, cdr) = cp(v);
            let car_ref = self.value(car)?;
            let cdr_ref = self.value(cdr)?;
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 13);
            put_u32(&mut bytes, car_ref);
            put_u32(&mut bytes, cdr_ref);
            return Some(self.intern_encoded(bytes));
        }
        None
    }
}

struct BbuFunction {
    name_ref: u32,
    flags: u32,
    arity: u16,
    n_locals: u16,
    max_stack: u16,
    code: Vec<u8>,
    literal_refs: Vec<u32>,
}

fn bbu_instr_len(i: &Instr) -> Option<usize> {
    Some(match i {
        Instr::Const(_) => 5,
        Instr::LoadLocal(_) | Instr::StoreLocal(_) => 3,
        Instr::LoadGlobal(_) | Instr::StoreGlobal(_) => 5,
        Instr::SetValues(_) => 3,
        Instr::ClearMv => 1,
        Instr::TakeValuesToLocals { .. } => 5,
        Instr::ValuesToList => 1,
        Instr::EvalHost(_) => return None,
        Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_) | Instr::DefineEnvVar(_) => return None,
        Instr::PushEnvChild | Instr::PopEnvChild => 1,
        Instr::MakeClosureEnv(_) => return None,
        Instr::Pop | Instr::Dup => 1,
        Instr::Br(_) | Instr::BrIfFalse(_) | Instr::BrIfTrue(_) => 5,
        Instr::CallNamed { .. } => 7,
        Instr::Return => 3,
        Instr::PushCatch { .. } => 7,
        Instr::PushBlock { .. } => 15,
        Instr::PushTag { .. } => 7,
        Instr::PushUnwind { .. } => 7,
        Instr::PopHandler => 1,
        Instr::Throw => 1,
        Instr::ReturnFrom { .. } => 5,
        Instr::Go { .. } => 9,
        Instr::EnterCleanupNormal { .. } => 9,
        Instr::CleanupReturn => 1,
        Instr::PushHandlerCase { .. } => 7,
        Instr::PopHandlerCase => 1,
        Instr::PushHandlerBind { .. } => 5,
        Instr::PopHandlerBind => 1,
        Instr::PushRestartCase { .. } => 11,
        Instr::PopRestartCase => 1,
    })
}

fn bbu_pc(target: u32, offsets: &[u32], end: u32) -> Option<u32> {
    let target = target as usize;
    if target == offsets.len() {
        Some(end)
    } else {
        offsets.get(target).copied()
    }
}

fn serialize_bbu_function(
    bf: &BytecodeFunction,
    name_ref: u32,
    flags: u32,
    pool: &mut BbuConstPool,
) -> Option<BbuFunction> {
    if !bf.handler_cases.is_empty() || !bf.handler_binds.is_empty() || !bf.restart_cases.is_empty()
    {
        return None;
    }

    let mut literal_refs = Vec::with_capacity(bf.constants.len());
    for &c in &bf.constants {
        literal_refs.push(pool.value(c)?);
    }

    let mut offsets = Vec::with_capacity(bf.code.len());
    let mut pc = 0u32;
    for instr in &bf.code {
        offsets.push(pc);
        pc = pc.checked_add(bbu_instr_len(instr)? as u32)?;
    }
    let end_pc = pc;

    let mut code = Vec::with_capacity(end_pc as usize);
    for instr in &bf.code {
        match instr {
            Instr::Const(idx) => {
                put_u8(&mut code, 0x01);
                put_u32(&mut code, *idx as u32);
            }
            Instr::LoadLocal(slot) => {
                put_u8(&mut code, 0x02);
                put_u16(&mut code, *slot);
            }
            Instr::StoreLocal(slot) => {
                put_u8(&mut code, 0x03);
                put_u16(&mut code, *slot);
            }
            Instr::LoadGlobal(sym) => {
                put_u8(&mut code, 0x07);
                let cp = pool.symbol_by_index(*sym);
                put_u32(&mut code, cp);
            }
            Instr::StoreGlobal(sym) => {
                put_u8(&mut code, 0x08);
                let cp = pool.symbol_by_index(*sym);
                put_u32(&mut code, cp);
            }
            Instr::SetValues(n) => {
                put_u8(&mut code, 0x13);
                put_u16(&mut code, *n);
            }
            Instr::ClearMv => put_u8(&mut code, 0x14),
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                put_u8(&mut code, 0x15);
                put_u16(&mut code, *slot_base);
                put_u16(&mut code, *nvars);
            }
            Instr::ValuesToList => put_u8(&mut code, 0x16),
            Instr::PushEnvChild => put_u8(&mut code, 0x36),
            Instr::PopEnvChild => put_u8(&mut code, 0x37),
            Instr::Pop => put_u8(&mut code, 0x11),
            Instr::Dup => put_u8(&mut code, 0x12),
            Instr::Br(target) => {
                put_u8(&mut code, 0x18);
                put_u32(&mut code, bbu_pc(*target, &offsets, end_pc)?);
            }
            Instr::BrIfFalse(target) => {
                put_u8(&mut code, 0x19);
                put_u32(&mut code, bbu_pc(*target, &offsets, end_pc)?);
            }
            Instr::BrIfTrue(target) => {
                put_u8(&mut code, 0x1a);
                put_u32(&mut code, bbu_pc(*target, &offsets, end_pc)?);
            }
            Instr::CallNamed { sym, nargs } => {
                put_u8(&mut code, 0x0d);
                let cp = pool.symbol_by_index(*sym);
                put_u32(&mut code, cp);
                put_u16(&mut code, *nargs);
            }
            Instr::Return => {
                put_u8(&mut code, 0x10);
                put_u16(&mut code, 1);
            }
            Instr::PushCatch {
                resume_bcp,
                sp_restore,
            } => {
                put_u8(&mut code, 0x20);
                put_u32(&mut code, bbu_pc(*resume_bcp, &offsets, end_pc)?);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PushBlock {
                block_id,
                name_idx,
                resume_bcp,
                sp_restore,
            } => {
                put_u8(&mut code, 0x1e);
                put_u32(&mut code, *block_id);
                let name_ref = bf
                    .names
                    .get(*name_idx as usize)
                    .map(|name| pool.string(name))
                    .unwrap_or(BBU_NO_INDEX);
                put_u32(&mut code, name_ref);
                put_u32(&mut code, bbu_pc(*resume_bcp, &offsets, end_pc)?);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                put_u8(&mut code, 0x22);
                put_u32(&mut code, *tagbody_id);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PushUnwind {
                cleanup_bcp,
                sp_restore,
            } => {
                put_u8(&mut code, 0x24);
                put_u32(&mut code, bbu_pc(*cleanup_bcp, &offsets, end_pc)?);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PopHandler => put_u8(&mut code, 0x27),
            Instr::Throw => put_u8(&mut code, 0x21),
            Instr::ReturnFrom { block_id } => {
                put_u8(&mut code, 0x1f);
                put_u32(&mut code, *block_id);
            }
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                put_u8(&mut code, 0x23);
                put_u32(&mut code, *tagbody_id);
                put_u32(&mut code, bbu_pc(*target_bcp, &offsets, end_pc)?);
            }
            Instr::EnterCleanupNormal {
                cleanup_bcp,
                resume_bcp,
            } => {
                put_u8(&mut code, 0x25);
                put_u32(&mut code, bbu_pc(*cleanup_bcp, &offsets, end_pc)?);
                put_u32(&mut code, bbu_pc(*resume_bcp, &offsets, end_pc)?);
            }
            Instr::CleanupReturn => put_u8(&mut code, 0x26),
            Instr::PushHandlerCase { hc, sp_restore } => {
                put_u8(&mut code, 0x28);
                put_u32(&mut code, *hc);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PopHandlerCase => put_u8(&mut code, 0x29),
            Instr::PushHandlerBind { hb } => {
                put_u8(&mut code, 0x2a);
                put_u32(&mut code, *hb);
            }
            Instr::PopHandlerBind => put_u8(&mut code, 0x2b),
            Instr::PushRestartCase {
                rc,
                resume_bcp,
                sp_restore,
            } => {
                put_u8(&mut code, 0x2c);
                put_u32(&mut code, *rc);
                put_u32(&mut code, bbu_pc(*resume_bcp, &offsets, end_pc)?);
                put_u16(&mut code, *sp_restore);
            }
            Instr::PopRestartCase => put_u8(&mut code, 0x2d),
            Instr::EvalHost(_)
            | Instr::LoadEnvVar(_)
            | Instr::StoreEnvVar(_)
            | Instr::DefineEnvVar(_)
            | Instr::MakeClosureEnv(_) => return None,
        }
    }

    Some(BbuFunction {
        name_ref,
        flags,
        arity: bf.arity,
        n_locals: bf.n_locals,
        max_stack: bf.max_stack.max(1),
        code,
        literal_refs,
    })
}

fn serialize_bbu_function_record(out: &mut Vec<u8>, f: &BbuFunction) {
    put_u32(out, f.name_ref);
    put_u32(out, BBU_NO_INDEX);
    put_u32(out, BBU_NO_INDEX);
    put_u32(out, f.flags);
    put_u16(out, f.arity);
    put_u16(out, f.arity);
    put_u16(out, f.n_locals);
    put_u16(out, f.max_stack);
    put_u32(out, f.code.len() as u32);
    out.extend_from_slice(&f.code);
    put_u32(out, f.literal_refs.len() as u32);
    for &r in &f.literal_refs {
        put_u32(out, r);
    }
    put_u32(out, 0);
    put_u32(out, 0);
    put_u32(out, BBU_NO_INDEX);
}

/// Build the BFASL `BYTECODE_UNIT` payload for top-level forms that the current
/// T0 bytecode lowerer can serialize. The legacy source section remains the
/// active loader path until the BBU deserializer/installer lands.
pub fn build_bbu_from_forms(
    forms: &[BlissVal],
    src_path: &str,
    source: &str,
    env: &Env,
) -> Vec<u8> {
    let mut pool = BbuConstPool::default();
    let source_file_ref = pool.string(src_path);
    let mut functions = Vec::new();
    let mut load_actions: Vec<(u8, u8, u32, u32, u32)> = Vec::new();

    for &form in forms {
        if let Some((name, params, body)) = as_defun(form) {
            if let Some(sym) = symbol_index_of(&name) {
                let name_ref = pool.symbol_by_index(sym);
                if let Some(bf) = compile_function(&name, params, body, env) {
                    if let Some(serialized) =
                        serialize_bbu_function(&bf, name_ref, BBU_FUNC_NAMED, &mut pool)
                    {
                        let function_index = functions.len() as u32;
                        functions.push(serialized);
                        load_actions.push((3, 0, function_index, name_ref, BBU_NO_INDEX));
                    }
                }
            }
            continue;
        }

        if let Some(bf) = compile_thunk(form, env) {
            if let Some(serialized) =
                serialize_bbu_function(&bf, BBU_NO_INDEX, BBU_FUNC_LOAD_TIME_THUNK, &mut pool)
            {
                let function_index = functions.len() as u32;
                functions.push(serialized);
                load_actions.push((7, 0, function_index, BBU_NO_INDEX, BBU_NO_INDEX));
            }
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(BBU_MAGIC);
    put_u16(&mut out, BBU_BYTECODE_VERSION);
    put_u16(&mut out, BBU_VERIFIER_VERSION);
    put_u32(&mut out, 0);
    put_u32(&mut out, pool.entries.len() as u32);
    put_u32(&mut out, functions.len() as u32);
    put_u32(&mut out, load_actions.len() as u32);
    put_u32(&mut out, 0);
    put_u32(&mut out, source_file_ref);
    put_u64(&mut out, fnv1a64(source.as_bytes()));

    for entry in &pool.entries {
        out.extend_from_slice(entry);
    }
    for function in &functions {
        serialize_bbu_function_record(&mut out, function);
    }
    for (kind, flags, arg0, arg1, arg2) in load_actions {
        put_u8(&mut out, kind);
        put_u8(&mut out, flags);
        put_u32(&mut out, arg0);
        put_u32(&mut out, arg1);
        put_u32(&mut out, arg2);
    }
    out
}

// ── Execution ──────────────────────────────────────────────────────

/// A live non-local-exit handler established on an activation (§2.4.3 frame
/// types CATCH / UNWIND / a block/tag marker). Handlers form a per-activation
/// stack; unwinding walks them newest-first.
#[derive(Clone)]
#[allow(clippy::enum_variant_names)] // `HandlerCase` mirrors the tree-walker's naming
enum Handler {
    /// `CATCH`: keyed by the control token shared with `env.catch_stack`.
    Catch {
        token: String,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// `BLOCK`: keyed by a lexical compile-time id (compiled `return-from`) and
    /// a control token registered in `env.block_stack` (tree-walker `return-from`).
    Block {
        block_id: u32,
        token: String,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// `TAGBODY`: keyed by a lexical compile-time id; `GO` targets a tag PC.
    Tag { tagbody_id: u32, sp_restore: u16 },
    /// `UNWIND-PROTECT`: a cleanup to run on any unwind through this point.
    Unwind { cleanup_bcp: u32, sp_restore: u16 },
    /// `HANDLER-CASE`: a cluster of condition-typed clauses. `cluster_base` is
    /// the `env.handlers` length before this cluster was pushed.
    HandlerCase {
        clauses: Vec<RuntimeClause>,
        sp_restore: u16,
        cluster_base: usize,
    },
    /// `HANDLER-BIND`: handlers already registered in `env.handlers`. On a raw
    /// structured error unwinding through here, the handlers get their turn.
    HandlerBind { cluster_base: usize },
    /// `RESTART-CASE`: restarts registered in `env.restarts`. A restart-invoked
    /// transfer unwinding through here delivers the stored result.
    RestartCase {
        restart_base: usize,
        resume_bcp: u32,
        sp_restore: u16,
    },
}

/// A live `handler-case` clause: its control token (shared with `env.handlers`),
/// condition type, body PC, and the local slot for its condition variable.
#[derive(Clone)]
struct RuntimeClause {
    token: String,
    type_name: String,
    body_bcp: u32,
    var_slot: Option<u16>,
}

/// What to do when a cleanup body finishes (`CleanupReturn`).
enum CleanupCont {
    /// Normal completion of `unwind-protect`: restore the protected value and
    /// resume at `resume_bcp`.
    Normal { resume_bcp: u32, value: BlissVal },
    /// The cleanup ran during an unwind: resume that unwind afterwards.
    Resume(Pending),
}

/// An in-progress non-local transfer looking for its matching handler.
enum Pending {
    /// A `THROW` (or a tree-walker control transfer propagated as
    /// `Err(Internal(token))`): value is held by `store_control_value(token)`.
    Token(String),
    /// A `RETURN-FROM` to the lexical block `block_id`.
    Return { block_id: u32, value: BlissVal },
    /// A `GO` to `target_bcp` within tagbody `tagbody_id`.
    Go { tagbody_id: u32, target_bcp: u32 },
    /// A genuine error (or uncaught throw): unwind all handlers running
    /// cleanups, then re-raise.
    Propagate(BlissError),
}

/// One live bytecode activation. `frame` owns the value slots on the
/// `BlissStack`; `bcp`/`sp_top` are the interpreter cursor (D2.03 keeps these
/// per-`bcp` for OSR/deopt — slice 1 keeps them Rust-side; nmq.3 moves them
/// in-frame for precise GC).
struct Activation {
    frame: *mut Frame,
    func: Rc<BytecodeFunction>,
    bcp: usize,
    sp_top: u16,
    n_locals: u16,
    /// Non-local-exit handlers established within this activation (newest last).
    handlers: Vec<Handler>,
    /// Pending cleanup continuations (for `unwind-protect`).
    cleanup_conts: Vec<CleanupCont>,
    /// Heap `EnvFrame` chain holding this activation's captured (boxed) locals,
    /// shared with any closure it creates. `None` when the function has no
    /// captured locals (the common, fast, slot-only case).
    env_frame: Option<Rc<RefCell<EnvFrame>>>,
    /// The tiered function object (FnMeta) this activation is executing, when
    /// one exists. Present for named functions dispatched through `CallNamed`;
    /// `None` for anonymous/gensym lambdas and toplevel wrappers with no
    /// interpreted-function object. Back-edges taken inside this activation are
    /// counted against this object's `back_edge_count` (bliss-jtc.10.1), the
    /// hot-loop profiling signal the compiler scheduler reads.
    fn_obj: Option<BlissVal>,
}

/// Build the base heap `EnvFrame` for a function with captured locals, binding
/// its boxed parameters; returns `None` for the slot-only case.
fn make_env_frame(
    func: &BytecodeFunction,
    args: &[BlissVal],
    parent: Rc<RefCell<EnvFrame>>,
) -> Option<Rc<RefCell<EnvFrame>>> {
    if !func.has_env {
        return None;
    }
    let frame = Rc::new(RefCell::new(EnvFrame {
        vars: std::collections::HashMap::new(),
        symbol_vars: std::collections::HashMap::new(),
        parent: Some(parent),
    }));
    for (i, (pname, loc)) in func.param_layout.iter().enumerate() {
        if let VarLoc::Boxed = loc {
            if let Some(a) = args.get(i) {
                frame.borrow_mut().vars.insert(pname.clone(), *a);
            }
        }
    }
    Some(frame)
}

#[inline]
unsafe fn slot_get(frame: *mut Frame, i: u16) -> BlissVal {
    unsafe { *(frame.add(1) as *const BlissVal).add(i as usize) }
}

#[inline]
unsafe fn slot_set(frame: *mut Frame, i: u16, v: BlissVal) {
    unsafe {
        *(frame.add(1) as *mut BlissVal).add(i as usize) = v;
    }
}

/// Count a loop back-edge against the executing function object's profiling
/// counter (bliss-jtc.10.1). A branch is a back-edge when its `target` is at or
/// before the branch instruction itself — i.e. `target < act.bcp`, since `bcp`
/// has already been advanced past the branch (so it equals the branch address
/// plus one). Forward branches (target >= bcp) are not loop edges and are
/// ignored. Anonymous lambdas and toplevel wrappers carry no `fn_obj`, so their
/// hot loops simply go uncounted here rather than costing an atomic per edge.
#[inline]
fn record_back_edge_if_backward(act: &Activation, target: u32) {
    if let Some(f) = act.fn_obj {
        if (target as usize) < act.bcp {
            bliss_rt::function::record_back_edge(f);
        }
    }
}

impl Activation {
    #[inline]
    fn push_op(&mut self, v: BlissVal) {
        unsafe { slot_set(self.frame, self.n_locals + self.sp_top, v) };
        self.sp_top += 1;
    }

    #[inline]
    fn pop_op(&mut self) -> BlissVal {
        self.sp_top -= 1;
        unsafe { slot_get(self.frame, self.n_locals + self.sp_top) }
    }
}

/// Frame-type/flags value for an interpreted CALL frame (§2.4.3 `CALL` = 0b00).
const FLAG_CALL: u32 = 0b00;

/// Bind a call's arguments into their parameters' frame slots (boxed params go
/// to the heap `EnvFrame` via [`make_env_frame`], not here).
fn bind_params(func: &BytecodeFunction, frame: *mut Frame, args: &[BlissVal]) {
    for (i, (_, loc)) in func.param_layout.iter().enumerate() {
        if let VarLoc::Slot(s) = loc {
            if let Some(a) = args.get(i) {
                unsafe { slot_set(frame, *s, *a) };
            }
        }
    }
}

/// Run a compiled function to completion on the current green thread's
/// `BlissStack`. `args` are the actual arguments bound into the entry frame's
/// leading local slots.
fn run(
    entry: Rc<BytecodeFunction>,
    args: &[BlissVal],
    entry_fn_val: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();

    let frame = stack
        .push_frame(
            entry_fn_val,
            std::ptr::null::<CodeInfo>(),
            entry.num_slots(),
            FLAG_CALL,
        )
        .ok_or_else(|| BlissError::StackOverflow(bliss_rt::current_thread_id()))?;
    bind_params(&entry, frame, args);
    let env_frame = make_env_frame(&entry, args, Rc::clone(&env.frame));
    let entry_obj = Some(entry_fn_val)
        .filter(|&v| bliss_rt::function::is_interpreted_function(v));
    let mut acts: Vec<Activation> = vec![Activation {
        frame,
        n_locals: entry.n_locals,
        env_frame,
        func: entry,
        bcp: 0,
        sp_top: 0,
        handlers: Vec::new(),
        cleanup_conts: Vec::new(),
        fn_obj: entry_obj,
    }];

    // Ensure the whole control stack is popped on any early return (error).
    let result = run_loop(&mut acts, env);
    // Unwind any frames still live (error path); the normal path leaves none.
    while !acts.is_empty() {
        stack.pop_frame();
        acts.pop();
    }
    result
}

fn run_loop(acts: &mut Vec<Activation>, env: &mut Env) -> Result<BlissVal, BlissError> {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();

    loop {
        let (instr, top_idx) = {
            let act = acts.last_mut().unwrap();
            let instr = act.func.code[act.bcp].clone();
            act.bcp += 1;
            (instr, acts.len() - 1)
        };

        match instr {
            Instr::Const(idx) => {
                let act = &mut acts[top_idx];
                let v = act.func.constants[idx as usize];
                act.push_op(v);
            }
            Instr::LoadLocal(idx) => {
                let act = &mut acts[top_idx];
                let v = unsafe { slot_get(act.frame, idx) };
                act.push_op(v);
            }
            Instr::StoreLocal(idx) => {
                let act = &mut acts[top_idx];
                let v = act.pop_op();
                unsafe { slot_set(act.frame, idx, v) };
            }
            Instr::LoadGlobal(sym) => {
                let s = BlissVal::from_symbol_index(sym);
                let val = env
                    .lookup_var_symbol(s)
                    .or_else(|| env.lookup_var(&sym_name(s)));
                match val {
                    Some(v) => acts[top_idx].push_op(v),
                    None => {
                        // Unbound: raise like the tree-walker, but via the unwind
                        // driver so unwind-protect cleanups still run.
                        initiate_unwind(
                            acts,
                            stack,
                            env,
                            Pending::Propagate(BlissError::UnboundVariable(s)),
                        )?;
                    }
                }
            }
            Instr::StoreGlobal(sym) => {
                let s = BlissVal::from_symbol_index(sym);
                let v = acts[top_idx].pop_op();
                env.set_var_symbol(s, v);
            }
            Instr::SetValues(n) => {
                let act = &mut acts[top_idx];
                let mut vals = vec![NIL; n as usize];
                for i in (0..n as usize).rev() {
                    vals[i] = act.pop_op();
                }
                let primary = vals.first().copied().unwrap_or(NIL);
                env.set_mv(vals);
                act.push_op(primary);
            }
            Instr::ClearMv => {
                env.clear_mv();
            }
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                let act = &mut acts[top_idx];
                let primary = act.pop_op();
                let frame = act.frame;
                let vals: Vec<BlissVal> = if env.mv_active {
                    env.mv.clone()
                } else {
                    Vec::new()
                };
                for i in 0..nvars {
                    let v = if i == 0 {
                        primary
                    } else {
                        vals.get(i as usize).copied().unwrap_or(NIL)
                    };
                    unsafe { slot_set(frame, slot_base + i, v) };
                }
            }
            Instr::ValuesToList => {
                let act = &mut acts[top_idx];
                let primary = act.pop_op();
                let vals: Vec<BlissVal> = if env.mv_active {
                    env.mv.clone()
                } else {
                    vec![primary]
                };
                act.push_op(vec_to_list(&vals));
            }
            Instr::EvalHost(idx) => {
                let form = acts[top_idx].func.constants[idx as usize];
                match eval_form(form, env) {
                    Ok(v) => acts[top_idx].push_op(v),
                    Err(e) => {
                        let pending = error_to_pending(e, env);
                        initiate_unwind(acts, stack, env, pending)?;
                    }
                }
            }
            Instr::LoadEnvVar(name_idx) => {
                let name = acts[top_idx].func.names[name_idx as usize].clone();
                let ef = acts[top_idx]
                    .env_frame
                    .clone()
                    .expect("LoadEnvVar without a heap EnvFrame");
                match Env::lookup_frame(&ef, &name) {
                    Some(v) => acts[top_idx].push_op(v),
                    None => {
                        let s = resolve_sym(&name).unwrap_or(NIL);
                        initiate_unwind(
                            acts,
                            stack,
                            env,
                            Pending::Propagate(BlissError::UnboundVariable(s)),
                        )?;
                    }
                }
            }
            Instr::StoreEnvVar(name_idx) => {
                let name = acts[top_idx].func.names[name_idx as usize].clone();
                let v = acts[top_idx].pop_op();
                let ef = acts[top_idx]
                    .env_frame
                    .clone()
                    .expect("StoreEnvVar without a heap EnvFrame");
                Env::set_frame_var(&ef, &name, v);
            }
            Instr::DefineEnvVar(name_idx) => {
                let name = acts[top_idx].func.names[name_idx as usize].clone();
                let v = acts[top_idx].pop_op();
                let ef = acts[top_idx]
                    .env_frame
                    .clone()
                    .expect("DefineEnvVar without a heap EnvFrame");
                ef.borrow_mut().vars.insert(name, v);
            }
            Instr::PushEnvChild => {
                let act = &mut acts[top_idx];
                let parent = act.env_frame.clone();
                act.env_frame = Some(Rc::new(RefCell::new(EnvFrame {
                    vars: std::collections::HashMap::new(),
                    symbol_vars: std::collections::HashMap::new(),
                    parent,
                })));
            }
            Instr::PopEnvChild => {
                let act = &mut acts[top_idx];
                let parent = act
                    .env_frame
                    .as_ref()
                    .and_then(|f| f.borrow().parent.clone());
                act.env_frame = parent;
            }
            Instr::MakeClosureEnv(idx) => {
                let form = acts[top_idx].func.constants[idx as usize];
                let ef = acts[top_idx]
                    .env_frame
                    .clone()
                    .expect("MakeClosureEnv without a heap EnvFrame");
                // Build the closure with env.frame bound to this activation's
                // heap frame so it captures the live, shared bindings.
                let saved = std::mem::replace(&mut env.frame, ef);
                let result = eval_form(form, env);
                env.frame = saved;
                match result {
                    Ok(v) => acts[top_idx].push_op(v),
                    Err(e) => {
                        let pending = error_to_pending(e, env);
                        initiate_unwind(acts, stack, env, pending)?;
                    }
                }
            }
            Instr::Pop => {
                acts[top_idx].pop_op();
            }
            Instr::Dup => {
                let act = &mut acts[top_idx];
                let v = act.pop_op();
                act.push_op(v);
                act.push_op(v);
            }
            Instr::Br(target) => {
                record_back_edge_if_backward(&acts[top_idx], target);
                acts[top_idx].bcp = target as usize;
            }
            Instr::BrIfFalse(target) => {
                let v = acts[top_idx].pop_op();
                if v.is_nil() {
                    record_back_edge_if_backward(&acts[top_idx], target);
                    acts[top_idx].bcp = target as usize;
                }
            }
            Instr::BrIfTrue(target) => {
                let v = acts[top_idx].pop_op();
                if !v.is_nil() {
                    record_back_edge_if_backward(&acts[top_idx], target);
                    acts[top_idx].bcp = target as usize;
                }
            }
            Instr::CallNamed { sym, nargs } => {
                // Collect arguments (pushed left-to-right, so arg0 is deepest).
                let mut args = Vec::with_capacity(nargs as usize);
                {
                    let act = &mut acts[top_idx];
                    for _ in 0..nargs {
                        args.push(act.pop_op());
                    }
                    args.reverse();
                }

                // bliss-jtc.6.8: bump the callee's FnMeta invoke counter (the
                // unified tiering substrate) so the function object reflects real
                // invocations from the bytecode path, not just the tree-walker.
                if let Some(cell) = bliss_rt::symbols::symbol_function(sym) {
                    if bliss_rt::function::is_interpreted_function(cell) {
                        bliss_rt::function::record_invocation(cell);
                    }
                }

                // Bytecode callee → native frame on the BlissStack.
                if let Some(callee) = registry_get(sym) {
                    if callee.arity == nargs {
                        // T1: installed native code → call via the i2c adapter.
                        // Unified tiering (bliss-jtc.3): the function object's
                        // FnMeta is the single tiering record — invoke counter
                        // (bumped at the top of CallNamed), current tier, and the
                        // active compiled entry. Promotion is driven by that
                        // counter and, on success, tier + entry are recorded on
                        // the object. Anonymous compiled lambdas (gensyms) have no
                        // function object, so they keep the INVOKE_COUNTS fallback.
                        let fn_obj = bliss_rt::symbols::symbol_function(sym)
                            .filter(|&c| bliss_rt::function::is_interpreted_function(c));
                        let native = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned());
                        let native = native.or_else(|| {
                            let count = match fn_obj {
                                Some(f) => bliss_rt::function::invoke_count(f),
                                None => INVOKE_COUNTS.with(|m| {
                                    let mut b = m.borrow_mut();
                                    let e = b.entry(sym).or_insert(0);
                                    *e += 1;
                                    *e
                                }),
                            };
                            if count < t1_threshold() {
                                return None;
                            }
                            let nc = try_promote_to_t1(sym)?;
                            if let Some(f) = fn_obj {
                                bliss_rt::function::set_entry(f, nc.entry as *mut u8);
                                bliss_rt::function::set_tier(f, 1);
                            }
                            Some(nc)
                        });
                        if let Some(nc) = native {
                            match run_native(&nc, sym, &args, env) {
                                Ok(v) => {
                                    acts[top_idx].push_op(v);
                                    continue;
                                }
                                Err(e) => {
                                    let pending = error_to_pending(e, env);
                                    initiate_unwind(acts, stack, env, pending)?;
                                    continue;
                                }
                            }
                        }
                        let fn_val = BlissVal::from_symbol_index(sym);
                        let frame = match stack.push_frame(
                            fn_val,
                            std::ptr::null::<CodeInfo>(),
                            callee.num_slots(),
                            FLAG_CALL,
                        ) {
                            Some(f) => f,
                            None => {
                                // BlissStack full → STORAGE-CONDITION, routed
                                // through the unwind driver so unwind-protect
                                // cleanups run and a handler-case can catch it.
                                let e = BlissError::StackOverflow(bliss_rt::current_thread_id());
                                initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                                continue;
                            }
                        };
                        bind_params(&callee, frame, &args);
                        let env_frame = make_env_frame(&callee, &args, Rc::clone(&env.frame));
                        acts.push(Activation {
                            frame,
                            n_locals: callee.n_locals,
                            env_frame,
                            func: callee,
                            bcp: 0,
                            sp_top: 0,
                            handlers: Vec::new(),
                            cleanup_conts: Vec::new(),
                            fn_obj,
                        });
                        continue;
                    }
                    // Arity mismatch: let the tree-walker enforce lambda-list rules.
                }

                // Fallback: tree-walker apply (builtins, generics, functions).
                let fn_val = BlissVal::from_symbol_index(sym);
                match apply_function(fn_val, &args, env) {
                    Ok(result) => acts[top_idx].push_op(result),
                    Err(e) => {
                        // Route the error through the bytecode unwind so
                        // unwind-protect cleanups run and a matching bytecode
                        // CATCH catches a tree-walker THROW.
                        let pending = error_to_pending(e, env);
                        initiate_unwind(acts, stack, env, pending)?;
                    }
                }
            }
            Instr::Return => {
                let v = {
                    let act = &mut acts[top_idx];
                    // Balanced bytecode leaves no live handlers at RETURN; drop
                    // any catch_stack entries defensively.
                    for h in act.handlers.drain(..) {
                        match h {
                            Handler::Catch { token, .. } => {
                                env.catch_stack.retain(|(_, t)| *t != token);
                            }
                            Handler::Block { token, .. } => {
                                env.block_stack.retain(|(_, t)| *t != token);
                            }
                            Handler::HandlerCase { cluster_base, .. }
                            | Handler::HandlerBind { cluster_base } => {
                                env.handlers.truncate(cluster_base);
                            }
                            Handler::RestartCase { restart_base, .. } => {
                                env.restarts.truncate(restart_base);
                            }
                            _ => {}
                        }
                    }
                    act.pop_op()
                };
                stack.pop_frame();
                acts.pop();
                match acts.last_mut() {
                    Some(caller) => caller.push_op(v),
                    None => return Ok(v),
                }
            }

            // ── Non-local control flow (nmq.4) ─────────────────────
            Instr::PushCatch {
                resume_bcp,
                sp_restore,
            } => {
                let tag = acts[top_idx].pop_op();
                let tag_str = val_as_str(tag);
                let token = next_control_token("__THROW__");
                env.catch_stack.push((tag_str, token.clone()));
                acts[top_idx].handlers.push(Handler::Catch {
                    token,
                    resume_bcp,
                    sp_restore,
                });
            }
            Instr::PushBlock {
                block_id,
                name_idx,
                resume_bcp,
                sp_restore,
            } => {
                let name = acts[top_idx].func.names[name_idx as usize].clone();
                let token = next_control_token("__RETURN_FROM__");
                env.block_stack.push((name, token.clone()));
                acts[top_idx].handlers.push(Handler::Block {
                    block_id,
                    token,
                    resume_bcp,
                    sp_restore,
                });
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                acts[top_idx].handlers.push(Handler::Tag {
                    tagbody_id,
                    sp_restore,
                });
            }
            Instr::PushUnwind {
                cleanup_bcp,
                sp_restore,
            } => {
                acts[top_idx].handlers.push(Handler::Unwind {
                    cleanup_bcp,
                    sp_restore,
                });
            }
            Instr::PopHandler => match acts[top_idx].handlers.pop() {
                Some(Handler::Catch { token, .. }) => {
                    env.catch_stack.retain(|(_, t)| *t != token);
                }
                Some(Handler::Block { token, .. }) => {
                    env.block_stack.retain(|(_, t)| *t != token);
                }
                _ => {}
            },
            Instr::Throw => {
                let (value, tag) = {
                    let act = &mut acts[top_idx];
                    let v = act.pop_op();
                    let t = act.pop_op();
                    (v, t)
                };
                let tag_str = val_as_str(tag);
                let token = env
                    .catch_stack
                    .iter()
                    .rev()
                    .find(|(t, _)| *t == tag_str)
                    .map(|(_, tok)| tok.clone());
                match token {
                    Some(tok) => {
                        store_control_value(&tok, value);
                        initiate_unwind(acts, stack, env, Pending::Token(tok))?;
                    }
                    None => {
                        let e = BlissError::Internal(format!("uncaught throw to {tag_str}"));
                        initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                    }
                }
            }
            Instr::ReturnFrom { block_id } => {
                let value = acts[top_idx].pop_op();
                initiate_unwind(acts, stack, env, Pending::Return { block_id, value })?;
            }
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                // A `go` to an earlier tag within this function is the canonical
                // loop back-edge — LOOP/DO/DOTIMES all expand to tagbody + go
                // (bliss-jtc.10.1). Record it against the executing function
                // object. Cross-function non-local `go` (rare) may unwind to an
                // enclosing activation; attributing that edge to the current
                // frame is a harmless profiling approximation, never a
                // correctness issue.
                record_back_edge_if_backward(&acts[top_idx], target_bcp);
                initiate_unwind(
                    acts,
                    stack,
                    env,
                    Pending::Go {
                        tagbody_id,
                        target_bcp,
                    },
                )?;
            }
            Instr::EnterCleanupNormal {
                cleanup_bcp,
                resume_bcp,
            } => {
                let act = &mut acts[top_idx];
                let value = act.pop_op();
                act.cleanup_conts
                    .push(CleanupCont::Normal { resume_bcp, value });
                act.bcp = cleanup_bcp as usize;
            }
            Instr::CleanupReturn => {
                let cont = acts[top_idx]
                    .cleanup_conts
                    .pop()
                    .expect("CleanupReturn without a pending cleanup continuation");
                match cont {
                    CleanupCont::Normal { resume_bcp, value } => {
                        let act = &mut acts[top_idx];
                        act.push_op(value);
                        act.bcp = resume_bcp as usize;
                    }
                    CleanupCont::Resume(pending) => {
                        initiate_unwind(acts, stack, env, pending)?;
                    }
                }
            }
            Instr::PushHandlerCase { hc, sp_restore } => {
                let info = acts[top_idx].func.handler_cases[hc as usize].clone();
                let cluster_base = env.handlers.len();
                let mut runtime_clauses = Vec::with_capacity(info.clauses.len());
                let mut entries = Vec::with_capacity(info.clauses.len());
                for clause in &info.clauses {
                    // A control token shared with the tree-walker: a host SIGNAL
                    // that matches this clause's type stores the condition here
                    // and returns Err(__HANDLER_CASE__:token), which the bytecode
                    // loop then routes into the clause body.
                    let token = next_control_token("__HANDLER_CASE__");
                    entries.push(HandlerEntry {
                        type_name: clause.type_name.clone(),
                        handler: HandlerImpl::HandlerCase {
                            token: token.clone(),
                            var_name: None,
                            body: NIL,
                            captured_frame: Rc::clone(&env.frame),
                        },
                    });
                    runtime_clauses.push(RuntimeClause {
                        token,
                        type_name: clause.type_name.clone(),
                        body_bcp: clause.body_bcp,
                        var_slot: clause.var_slot,
                    });
                }
                env.handlers.push(HandlerCluster { entries });
                acts[top_idx].handlers.push(Handler::HandlerCase {
                    clauses: runtime_clauses,
                    sp_restore,
                    cluster_base,
                });
            }
            Instr::PopHandlerCase => {
                if let Some(Handler::HandlerCase { cluster_base, .. }) =
                    acts[top_idx].handlers.pop()
                {
                    env.handlers.truncate(cluster_base);
                }
            }
            Instr::PushHandlerBind { hb } => {
                let info = acts[top_idx].func.handler_binds[hb as usize].clone();
                let env_frame = acts[top_idx].env_frame.clone();
                let cluster_base = env.handlers.len();
                let mut entries = Vec::with_capacity(info.bindings.len());
                for (type_name, handler_form) in &info.bindings {
                    // When this function has boxed (captured) locals, evaluate
                    // the handler form now into a closure that captures them, so
                    // a handler run at signal time (in the tree-walker, where
                    // env.frame is global) still sees the lexical bindings.
                    let handler = if let Some(ef) = &env_frame {
                        let saved = std::mem::replace(&mut env.frame, ef.clone());
                        let v = eval_form(*handler_form, env);
                        env.frame = saved;
                        match v {
                            Ok(val) => HandlerImpl::Function(val),
                            Err(_) => HandlerImpl::Function(*handler_form),
                        }
                    } else {
                        HandlerImpl::Function(*handler_form)
                    };
                    entries.push(HandlerEntry {
                        type_name: type_name.clone(),
                        handler,
                    });
                }
                env.handlers.push(HandlerCluster { entries });
                acts[top_idx]
                    .handlers
                    .push(Handler::HandlerBind { cluster_base });
            }
            Instr::PopHandlerBind => {
                if let Some(Handler::HandlerBind { cluster_base }) = acts[top_idx].handlers.pop() {
                    env.handlers.truncate(cluster_base);
                }
            }
            Instr::PushRestartCase {
                rc,
                resume_bcp,
                sp_restore,
            } => {
                let info = acts[top_idx].func.restart_cases[rc as usize].clone();
                let restart_base = env.restarts.len();
                for (name, lambda_form) in &info.restarts {
                    env.restarts.push(RestartEntry {
                        name: name.clone(),
                        function: RestartFunction::FunctionForm {
                            function_form: *lambda_form,
                            captured_frame: Rc::clone(&env.frame),
                        },
                        interactive_function: None,
                        test_function: None,
                        unwind_on_invoke: true,
                    });
                }
                acts[top_idx].handlers.push(Handler::RestartCase {
                    restart_base,
                    resume_bcp,
                    sp_restore,
                });
            }
            Instr::PopRestartCase => {
                if let Some(Handler::RestartCase { restart_base, .. }) =
                    acts[top_idx].handlers.pop()
                {
                    env.restarts.truncate(restart_base);
                }
            }
        }
    }
}

/// Drive a non-local transfer: walk handlers newest-first (across activations),
/// running `unwind-protect` cleanups, until the matching handler is found (or
/// the stack is exhausted). On success the target activation's `bcp`/operand
/// stack are set so the main loop resumes there; running a cleanup returns
/// early with the cleanup queued (its `CleanupReturn` re-drives the unwind).
fn initiate_unwind(
    acts: &mut Vec<Activation>,
    stack: &bliss_rt::BlissStack,
    env: &mut Env,
    mut pending: Pending,
) -> Result<(), BlissError> {
    loop {
        let top = acts.len() - 1;
        let handler = acts[top].handlers.last().cloned();
        match handler {
            Some(Handler::Unwind {
                cleanup_bcp,
                sp_restore,
            }) => {
                let act = &mut acts[top];
                act.handlers.pop();
                act.sp_top = sp_restore;
                act.cleanup_conts.push(CleanupCont::Resume(pending));
                act.bcp = cleanup_bcp as usize;
                return Ok(());
            }
            Some(Handler::Catch {
                token,
                resume_bcp,
                sp_restore,
            }) => {
                let matched = matches!(&pending, Pending::Token(t) if *t == token);
                acts[top].handlers.pop();
                env.catch_stack.retain(|(_, t)| *t != token);
                if matched {
                    let v = take_control_value(&token);
                    let act = &mut acts[top];
                    act.sp_top = sp_restore;
                    act.bcp = resume_bcp as usize;
                    act.push_op(v);
                    return Ok(());
                }
                // Non-matching catch: unwound past.
            }
            Some(Handler::Block {
                block_id,
                token,
                resume_bcp,
                sp_restore,
            }) => {
                acts[top].handlers.pop();
                env.block_stack.retain(|(_, t)| *t != token);
                // A compiled `return-from` matches by lexical id; a tree-walker
                // `return-from` (e.g. from a handler function) arrives as this
                // block's control token.
                let matched = match &pending {
                    Pending::Return {
                        block_id: bid,
                        value,
                    } if *bid == block_id => Some(*value),
                    Pending::Token(t) if *t == token => Some(take_control_value(&token)),
                    _ => None,
                };
                if let Some(v) = matched {
                    let act = &mut acts[top];
                    act.sp_top = sp_restore;
                    act.bcp = resume_bcp as usize;
                    act.push_op(v);
                    return Ok(());
                }
            }
            Some(Handler::Tag {
                tagbody_id,
                sp_restore,
            }) => {
                if let Pending::Go {
                    tagbody_id: tid,
                    target_bcp,
                } = &pending
                {
                    if *tid == tagbody_id {
                        // Keep the tag handler live — tags can be re-targeted.
                        let act = &mut acts[top];
                        act.sp_top = sp_restore;
                        act.bcp = *target_bcp as usize;
                        return Ok(());
                    }
                }
                acts[top].handlers.pop();
            }
            Some(Handler::HandlerCase {
                clauses,
                sp_restore,
                cluster_base,
            }) => {
                acts[top].handlers.pop();
                env.handlers.truncate(cluster_base);
                // Only an error (raw, or a host SIGNAL that selected one of this
                // cluster's clauses) can be caught by HANDLER-CASE. Block/go/throw
                // transfers pass straight through.
                let matched: Option<(RuntimeClause, BlissVal)> = match &pending {
                    Pending::Propagate(error) => {
                        if let Some(tok) = handler_case_token(error) {
                            clauses
                                .iter()
                                .find(|c| c.token == tok)
                                .map(|c| (c.clone(), take_control_value(&tok)))
                        } else if let Ok(Some(cond)) = bliss_error_to_condition(env, error) {
                            clauses
                                .iter()
                                .find(|c| condition_matches_handler(env, cond, &c.type_name))
                                .map(|c| (c.clone(), cond))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((clause, cond)) = matched {
                    if let Some(slot) = clause.var_slot {
                        unsafe { slot_set(acts[top].frame, slot, cond) };
                    }
                    let act = &mut acts[top];
                    act.sp_top = sp_restore;
                    act.bcp = clause.body_bcp as usize;
                    return Ok(());
                }
                // No clause matched — keep unwinding.
            }
            Some(Handler::HandlerBind { cluster_base }) => {
                acts[top].handlers.pop();
                // Mirror eval_handler_bind: only a *raw* structured error (one
                // bliss_error_to_condition can denote) gives these handlers their
                // turn here — conditions raised via SIGNAL/ERROR already ran the
                // handler stack at signal time. A handler that declines lets the
                // original error keep unwinding; one that transfers control
                // replaces the pending transfer.
                if let Pending::Propagate(error) = &pending {
                    if let Ok(Some(cond)) = bliss_error_to_condition(env, error) {
                        match run_handler_bind_handlers(env, cond, cluster_base) {
                            Ok(()) => {}
                            Err(transfer) => {
                                pending = error_to_pending(transfer, env);
                            }
                        }
                    }
                }
                env.handlers.truncate(cluster_base);
                // Keep unwinding with the (possibly transferred) pending.
            }
            Some(Handler::RestartCase {
                restart_base,
                resume_bcp,
                sp_restore,
            }) => {
                acts[top].handlers.pop();
                env.restarts.truncate(restart_base);
                // A restart invoked (by a handler) unwinds here carrying its
                // stored result — mirror eval_restart_case.
                let delivered = match &pending {
                    Pending::Propagate(error) => restart_invoked_name(error)
                        .map(|name| take_control_value(&format!("RESTART-RESULT:{name}"))),
                    _ => None,
                };
                if let Some(v) = delivered {
                    let act = &mut acts[top];
                    act.sp_top = sp_restore;
                    act.bcp = resume_bcp as usize;
                    act.push_op(v);
                    return Ok(());
                }
                // Not a restart transfer — keep unwinding.
            }
            None => {
                // No handler here — this activation is fully unwound.
                stack.pop_frame();
                acts.pop();
                if acts.is_empty() {
                    return Err(unmatched_error(pending));
                }
            }
        }
    }
}

/// Convert an error returned by a host (tree-walker) call into a bytecode
/// unwind: a control token naming a live bytecode CATCH becomes a `Token`
/// transfer (so tree-walker THROWs reach bytecode catches); anything else
/// propagates after running cleanups.
fn error_to_pending(e: BlissError, env: &Env) -> Pending {
    if let BlissError::Internal(token) = &e {
        // A control token naming one of our live bytecode CATCH or BLOCK handlers
        // (a THROW / RETURN-FROM performed by tree-walker code) becomes a Token
        // transfer the unwind driver routes to that handler.
        if env.catch_stack.iter().any(|(_, t)| t == token)
            || env.block_stack.iter().any(|(_, t)| t == token)
        {
            return Pending::Token(token.clone());
        }
    }
    Pending::Propagate(e)
}

/// The error to raise when an unwind reaches the bottom with no matching
/// handler.
fn unmatched_error(pending: Pending) -> BlissError {
    match pending {
        Pending::Propagate(e) => e,
        Pending::Token(token) => BlissError::Internal(token),
        Pending::Return { .. } => BlissError::Internal("RETURN-FROM: no visible block".into()),
        Pending::Go { .. } => BlissError::Internal("GO: no such tag".into()),
    }
}

// ── T1 native code (codegen → execution, nmq.2) ────────────────────
//
// A hot bytecode function is compiled to native x86-64 by `emit_native_x86`,
// installed into executable memory (`bliss_rt::jit::JitBuffer`), and called via
// an *i2c adapter* (`run_native`) that marshals the operand-stack arguments into
// the SysV calling convention. Native code that calls a non-arithmetic function
// crosses back through a *c2i adapter* (`c2i_call*`) into the interpreter. Both
// frames stay on the one BlissStack (the interpreter still pushes D2.03 frames);
// results are identical to pure interpretation (differential-verified).

thread_local! {
    /// The `Env` in scope while native T1 code runs, so a c2i callback can
    /// invoke interpreted functions. Set by `run_native` around the call.
    static NATIVE_ENV: std::cell::Cell<*mut Env> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// c2i adapter: call the interpreted function named by `sym` with `n` arguments
/// (SysV registers), returning the result's raw bits. Also the arithmetic
/// slow path — native code jumps here on fixnum overflow so bignum promotion
/// matches pure interpretation.
/// Clear the current thread's multiple-values state from native (T1) code
/// (bliss-jtc.25). Mirrors the interpreter's `ClearMv` opcode, which SETQ and a
/// few other non-value-preserving forms emit. Without it a native function that
/// stored the primary of a multiple-valued call would leak the extra values to
/// its caller.
extern "C" fn c2i_clear_mv() {
    let env_ptr = NATIVE_ENV.with(|e| e.get());
    if env_ptr.is_null() {
        return;
    }
    // SAFETY: same window/contract as `c2i_call` — `run_native` keeps
    // NATIVE_ENV pointing at a live &mut Env for the duration of the call.
    let env = unsafe { &mut *env_ptr };
    env.clear_mv();
}

extern "C" fn c2i_call(sym: u64, n: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let env_ptr = NATIVE_ENV.with(|e| e.get());
    if env_ptr.is_null() {
        return NIL.0;
    }
    // SAFETY: `run_native` sets NATIVE_ENV to a live &mut Env for the duration
    // of the native call, and native code only calls this synchronously within
    // that window.
    let env = unsafe { &mut *env_ptr };
    let args: &[BlissVal] = &[BlissVal(a0), BlissVal(a1), BlissVal(a2)][..n as usize];
    let fn_val = BlissVal::from_symbol_index(sym as u32);
    match apply_function(fn_val, args, env) {
        Ok(v) => v.0,
        // A raw error can't unwind through native code cleanly here; stash it so
        // run_native can re-raise. Return NIL bits as a placeholder.
        Err(e) => {
            NATIVE_ERROR.with(|c| *c.borrow_mut() = Some(e));
            NIL.0
        }
    }
}

thread_local! {
    /// Error raised by a c2i callback, re-raised by `run_native` after the
    /// native call returns.
    static NATIVE_ERROR: RefCell<Option<BlissError>> = const { RefCell::new(None) };
    /// Set by native (T1) code when a speculative guard fails (bliss-jtc.27): a
    /// non-fixnum operand or a fixnum-overflowing arithmetic result. `run_native`
    /// observes it, discards the native result, and re-runs the function in the
    /// interpreter (T0) — a deoptimization that returns the correct value.
    static NATIVE_DEOPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Count of speculative deoptimizations, for observability/tests (bliss-jtc.27).
static DEOPT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Total T1 speculative deoptimizations observed this process.
pub fn deopt_count() -> u64 {
    DEOPT_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Append a Linux `perf` symbol-map entry for a freshly installed T1 function
/// (bliss-jtc.10). When BLISS_PERF_MAP is set, `perf` symbolicates JIT frames by
/// reading `/tmp/perf-<pid>.map`, whose lines are `<hex-addr> <hex-size> <name>`
/// — so `perf top`/`perf report` show `T1:<function>` instead of an unknown
/// address, exactly how HotSpot exposes its compiled code. Setting the variable
/// to a path (any value containing '/') redirects the file, which the tests use;
/// "1" (or any non-path value) writes the perf-standard `/tmp/perf-<pid>.map`.
fn maybe_write_perf_map(addr: usize, size: usize, sym: u32) {
    let Some(val) = std::env::var_os("BLISS_PERF_MAP") else {
        return;
    };
    let val = val.to_string_lossy();
    let path = if val.contains('/') {
        val.into_owned()
    } else {
        format!("/tmp/perf-{}.map", std::process::id())
    };
    let raw = bliss_rt::symbols::symbol_name(sym).unwrap_or_else(|| format!("fn{sym}"));
    // perf reads everything after the size as the symbol name; keep it a single
    // token so tools that split on whitespace stay happy.
    let name: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{addr:x} {size:x} T1:{name}");
    }
}

/// Signal a speculative deoptimization from native (T1) code (bliss-jtc.27).
/// Sets the thread's deopt flag; `run_native` re-runs the function in the
/// interpreter after the native frame returns.
extern "C" fn c2i_deopt() {
    NATIVE_DEOPT.with(|d| d.set(true));
}

/// Installed T1 native code for a function. Its CL activation (locals + operand
/// stack) lives in a BlissStack frame that the i2c adapter pushes; the native
/// code addresses it through the frame-slot pointer passed in rdi (§D2.04).
struct NativeCode {
    entry: *const u8,
    num_slots: u16,
    /// Validated GC stack-map metadata for this function's activation, installed
    /// alongside the code (bliss-jtc.4). Passed into every frame the i2c adapter
    /// pushes, so the collector scans compiled frames through the map.
    code_info: &'static CodeInfo,
}

/// Build and register validated GC stack-map metadata for a T1 native function
/// whose CL activation holds `num_slots` tagged BlissVal slots (bliss-jtc.4).
/// Returns `None` if a map for the entry safepoint could not be constructed, in
/// which case the caller MUST reject installation (R4.46: no safepoint without a
/// stack map). Every activation slot is a tagged BlissVal, so its ref bitmap
/// marks all `num_slots` slots (an unboxed slot would clear its bit; the T1
/// baseline emits none). The bitmap and entry table are leaked for the lifetime
/// of the installed code.
fn install_stack_map(num_slots: u16) -> Option<&'static CodeInfo> {
    let n = num_slots as usize;
    let mut bitmap = vec![0u8; n.div_ceil(8)];
    for i in 0..n {
        bitmap[i / 8] |= 1 << (i % 8);
    }
    let bitmap: &'static [u8] = Box::leak(bitmap.into_boxed_slice());
    let entry = StackMapEntry {
        pc_offset: 0,
        bytes: bitmap.as_ptr() as usize,
        len: bitmap.len(),
    };
    let entries: &'static [StackMapEntry] = Box::leak(vec![entry].into_boxed_slice());
    let ci = CodeInfo::new(&[], entries);
    // Validate the entry safepoint (pc 0) resolves to the installed map. A frame
    // with reference slots must have a non-empty map; a leaf frame with no
    // reference slots legitimately has none.
    if n > 0 && ci.stack_map(0).is_none() {
        return None;
    }
    Some(ci)
}

thread_local! {
    /// Native T1 code keyed by the same symbol index as the bytecode registry.
    static NATIVE_REGISTRY: RefCell<HashMap<u32, Rc<NativeCode>>> = RefCell::new(HashMap::new());
    /// Per-function invocation counters driving T0→T1 promotion.
    static INVOKE_COUNTS: RefCell<HashMap<u32, u32>> = RefCell::new(HashMap::new());
    /// Per-function speculative-deopt counters (bliss-jtc.27). When a function
    /// deopts more than the backoff threshold, its speculative native code is
    /// uninstalled and it is blacklisted from re-promotion — HotSpot's policy of
    /// not repeatedly recompiling code that keeps deoptimizing.
    static DEOPT_COUNTS: RefCell<HashMap<u32, u32>> = RefCell::new(HashMap::new());
    /// Functions whose speculation proved unprofitable; kept in T0 thereafter.
    static DEOPT_BLACKLIST: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Per-function deopt count before a function's speculative code is uninstalled
/// and blacklisted (bliss-jtc.27). Env-overridable for tests; default 8.
fn deopt_blacklist_threshold() -> u32 {
    std::env::var("BLISS_DEOPT_BLACKLIST_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(8)
}

/// T0→T1 promotion threshold (invocations). Env-overridable for tests.
fn t1_threshold() -> u32 {
    std::env::var("BLISS_T1_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(10)
}

/// i2c adapter: push a BlissStack frame for the native activation, bind the
/// arguments into its leading local slots, run the installed native code (which
/// addresses the frame through rdi), then pop the frame. The native frame and
/// any interpreter frames a c2i callback pushes all live on the one BlissStack.
fn run_native(
    nc: &NativeCode,
    sym: u32,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();
    let frame = stack
        .push_frame(
            NIL,
            nc.code_info as *const CodeInfo,
            nc.num_slots,
            FLAG_CALL,
        )
        .ok_or_else(|| BlissError::StackOverflow(bliss_rt::current_thread_id()))?;
    for (i, a) in args.iter().enumerate() {
        unsafe { slot_set(frame, i as u16, *a) };
    }
    let slots = unsafe { frame.add(1) as *mut u64 };

    let saved = NATIVE_ENV.with(|e| e.replace(env as *mut Env));
    NATIVE_ERROR.with(|c| *c.borrow_mut() = None);
    NATIVE_DEOPT.with(|d| d.set(false));
    // SAFETY: `entry` is installed executable code from emit_native_x86 with the
    // SysV signature `fn(*mut u64) -> u64`, reading its activation from `slots`.
    let f: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(nc.entry) };
    let ret = f(slots);
    NATIVE_ENV.with(|e| e.set(saved));

    stack.pop_frame();
    if let Some(err) = NATIVE_ERROR.with(|c| c.borrow_mut().take()) {
        return Err(err);
    }
    // Deoptimization (bliss-jtc.27): a speculative guard failed. The native code
    // only speculates in pure functions (no side effects before any guard), so
    // re-running the whole function in the interpreter (T0) is observably
    // equivalent and yields the correct result — e.g. a bignum where the fixnum
    // fast path overflowed.
    if NATIVE_DEOPT.with(|d| d.replace(false)) {
        DEOPT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        trace("t1 speculative deopt → interpreter");
        // Backoff/blacklist: an occasional deopt (a rare overflow) is fine and
        // the fast path stays installed. But a function that keeps deopting is
        // being called outside its speculated fixnum domain, so past a threshold
        // uninstall its native code and blacklist it from re-promotion — it runs
        // in T0 from then on, correct and without per-call deopt overhead. This
        // is HotSpot's policy of not repeatedly recompiling deoptimizing code.
        let n = DEOPT_COUNTS.with(|m| {
            let mut b = m.borrow_mut();
            let e = b.entry(sym).or_insert(0);
            *e += 1;
            *e
        });
        if n >= deopt_blacklist_threshold() {
            NATIVE_REGISTRY.with(|r| r.borrow_mut().remove(&sym));
            DEOPT_BLACKLIST.with(|s| s.borrow_mut().insert(sym));
            if let Some(f) = bliss_rt::symbols::symbol_function(sym)
                .filter(|&c| bliss_rt::function::is_interpreted_function(c))
            {
                bliss_rt::function::set_tier(f, 0);
            }
            trace("t1 speculation blacklisted → staying T0");
        }
        let entry = registry_get(sym).ok_or_else(|| {
            BlissError::Internal("deopt: bytecode function vanished".into())
        })?;
        return run(entry, args, BlissVal::from_symbol_index(sym), env);
    }
    Ok(BlissVal(ret))
}

/// Compile a bytecode function to native x86-64 T1 code, or `None` if it uses
/// an opcode the baseline emitter does not handle.
///
/// The emitted function has the SysV signature
/// `fn(a0..a5: u64) -> u64` (BlissVals). It manages the operand stack in-frame
/// (via r15, keeping rsp 16-aligned for calls) and delegates every `CallNamed`
/// to the interpreter through the c2i adapter — so a T1 function's arithmetic,
/// calls, and conditionals produce results identical to pure interpretation,
/// while the dispatch/operand-stack plumbing runs as native code (nmq.2).
#[cfg(target_arch = "x86_64")]
fn emit_native_x86(bf: &BytecodeFunction) -> Option<Vec<u8>> {
    if bf.arity > 6 {
        return None;
    }
    // The activation lives in the BlissStack frame passed in rdi. r14 = frame
    // slots pointer; local i at [r14 + 8*i]; r15 = operand-stack pointer
    // (grows up from r14 + 8*n_locals). c2i preserves callee-saved r14/r15.
    let n_locals = bf.n_locals as i32;
    let local_disp = |i: i32| 8 * i;
    let c2i_addr = c2i_call as extern "C" fn(u64, u64, u64, u64, u64) -> u64 as usize as u64;
    let clear_mv_addr = c2i_clear_mv as extern "C" fn() as usize as u64;
    let deopt_addr = c2i_deopt as extern "C" fn() as usize as u64;

    let mut c: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(bf.code.len());
    let mut patches: Vec<(usize, u32)> = Vec::new();

    // Loop support (bliss-jtc.25): pre-scan block/tag establishments so a local
    // `ReturnFrom`/`Go` can reset the operand stack to the target's `sp_restore`
    // and jump. The interpreter resets the operand stack on every non-local
    // transfer; a nested `(return-from nil x)` / `(go tag)` can sit above the
    // target depth (enclosing operands live on the stack), so a bare jump would
    // leak them. `lea r15, [r14 + 8*(n_locals + sp_restore)]` restores depth.
    let mut block_targets: std::collections::HashMap<u32, (u32, u16)> =
        std::collections::HashMap::new();
    let mut tag_sp: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
    for instr in bf.code.iter() {
        match instr {
            Instr::PushBlock {
                block_id,
                resume_bcp,
                sp_restore,
                ..
            } => {
                block_targets.insert(*block_id, (*resume_bcp, *sp_restore));
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                tag_sp.insert(*tagbody_id, *sp_restore);
            }
            _ => {}
        }
    }
    // Reset the operand-stack pointer r15 to hold `depth` values above the
    // locals: r15 = r14 + 8*(n_locals + depth). Mirrors the prologue's lea.
    let reset_r15 = |c: &mut Vec<u8>, depth: u16| {
        c.extend_from_slice(&[0x4D, 0x8D, 0xBE]); // lea r15, [r14 + disp32]
        let disp = 8 * (n_locals + depth as i32);
        c.extend_from_slice(&disp.to_le_bytes());
    };

    // Speculative-fixnum eligibility (bliss-jtc.27): only when every call in the
    // function is to a re-execution-safe primitive may we inline fixnum fast
    // paths whose guards deoptimize by re-running the whole function. Purity of
    // every call makes that re-run observably equivalent.
    let deopt_safe = bf.code.iter().all(|i| match i {
        Instr::CallNamed { sym, .. } => is_deopt_safe_primitive(*sym),
        _ => true,
    });
    // Sites of `jcc rel32` guard branches that jump to the shared deopt block,
    // patched once the block's offset is known.
    let mut deopt_sites: Vec<usize> = Vec::new();
    // Emit a two-byte-opcode conditional jump (0F xx) to the deopt block.
    let jcc_deopt = |c: &mut Vec<u8>, sites: &mut Vec<usize>, opcode2: u8| {
        c.extend_from_slice(&[0x0F, opcode2]);
        sites.push(c.len());
        c.extend_from_slice(&[0, 0, 0, 0]);
    };

    // ── Prologue ───────────────────────────────────────────────
    c.extend_from_slice(&[0x41, 0x56]); // push r14
    c.extend_from_slice(&[0x41, 0x57]); // push r15
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8 (16-align for calls)
    c.extend_from_slice(&[0x49, 0x89, 0xFE]); // mov r14, rdi (frame slots)
    // lea r15, [r14 + 8*n_locals]   4D 8D BE d32
    c.extend_from_slice(&[0x4D, 0x8D, 0xBE]);
    c.extend_from_slice(&(8 * n_locals).to_le_bytes());

    // push_op rax: mov [r15], rax ; add r15, 8
    let push_rax = |c: &mut Vec<u8>| {
        c.extend_from_slice(&[0x49, 0x89, 0x07]); // mov [r15], rax
        c.extend_from_slice(&[0x49, 0x83, 0xC7, 0x08]); // add r15, 8
    };
    // pop_op into reg: sub r15,8 ; mov reg, [r15]
    let pop_into = |c: &mut Vec<u8>, modrm_reg: u8, rex_r: bool| {
        c.extend_from_slice(&[0x49, 0x83, 0xEF, 0x08]); // sub r15, 8
        let rex = 0x49 | if rex_r { 0x04 } else { 0x00 };
        c.push(rex);
        c.push(0x8B);
        c.push(0b00_000_111 | (modrm_reg << 3)); // mod=00, reg, rm=111 (r15)
    };

    for instr in bf.code.iter() {
        offsets.push(c.len());
        match instr {
            Instr::Const(k) => {
                let bits = bf.constants[*k as usize].0;
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
                c.extend_from_slice(&bits.to_le_bytes());
                push_rax(&mut c);
            }
            Instr::LoadLocal(i) => {
                // mov rax, [r14 + d32]   49 8B 86 d32
                c.extend_from_slice(&[0x49, 0x8B, 0x86]);
                c.extend_from_slice(&local_disp(*i as i32).to_le_bytes());
                push_rax(&mut c);
            }
            Instr::StoreLocal(i) => {
                pop_into(&mut c, 0, false); // -> rax
                // mov [r14 + d32], rax   49 89 86 d32
                c.extend_from_slice(&[0x49, 0x89, 0x86]);
                c.extend_from_slice(&local_disp(*i as i32).to_le_bytes());
            }
            Instr::Pop => {
                c.extend_from_slice(&[0x49, 0x83, 0xEF, 0x08]); // sub r15, 8
            }
            Instr::Dup => {
                // mov rax, [r15-8] ; push
                c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF8]); // mov rax, [r15-8]
                push_rax(&mut c);
            }
            Instr::CallNamed { sym, nargs } => {
                if *nargs > 3 {
                    return None;
                }
                // Unary speculative fast path (bliss-jtc.27): 1+, 1-, and unary -
                // saturate loop bodies. Guard the operand is a fixnum, then work
                // on the tagged value directly: +1<<3 / -1<<3 / two's-complement
                // negate, each with `jo` for the fixnum-overflow edge (negating
                // the most-negative fixnum sets OF → correct deopt).
                if deopt_safe && *nargs == 1 {
                    if let Some(op) = inlinable_unary_fixnum_op(*sym) {
                        pop_into(&mut c, 0, false); // x -> rax
                        c.extend_from_slice(&[0xA8, 0x07]); // test al, 7
                        jcc_deopt(&mut c, &mut deopt_sites, 0x85); // jnz deopt
                        match op {
                            UnaryFixnumOp::Incr => {
                                c.extend_from_slice(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
                            }
                            UnaryFixnumOp::Decr => {
                                c.extend_from_slice(&[0x48, 0x83, 0xE8, 0x08]); // sub rax, 8
                            }
                            UnaryFixnumOp::Neg => {
                                c.extend_from_slice(&[0x48, 0xF7, 0xD8]); // neg rax
                            }
                        }
                        jcc_deopt(&mut c, &mut deopt_sites, 0x80); // jo deopt
                        push_rax(&mut c);
                        continue;
                    }
                    if let Some(pred) = inlinable_fixnum_pred(*sym) {
                        pop_into(&mut c, 0, false); // x -> rax
                        c.extend_from_slice(&[0xA8, 0x07]); // test al, 7
                        jcc_deopt(&mut c, &mut deopt_sites, 0x85); // jnz deopt
                        // Set flags: sign tests use `test rax,rax`; parity tests
                        // check tagged bit 3 (value bit 0) via `test al, 8`.
                        match pred {
                            FixnumPred::Zerop | FixnumPred::Plusp | FixnumPred::Minusp => {
                                c.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
                            }
                            FixnumPred::Evenp | FixnumPred::Oddp => {
                                c.extend_from_slice(&[0xA8, 0x08]); // test al, 8
                            }
                        }
                        c.extend_from_slice(&[0x48, 0xB8]); // mov rax, NIL
                        c.extend_from_slice(&bliss_rt::value::NIL_BITS.to_le_bytes());
                        c.extend_from_slice(&[0x48, 0xBA]); // mov rdx, T
                        c.extend_from_slice(&bliss_rt::value::T_BITS.to_le_bytes());
                        let cc = match pred {
                            FixnumPred::Zerop => 0x44, // cmove  (== 0)
                            FixnumPred::Plusp => 0x4F, // cmovg  (> 0)
                            FixnumPred::Minusp => 0x4C, // cmovl (< 0)
                            FixnumPred::Evenp => 0x44, // cmove  (bit clear)
                            FixnumPred::Oddp => 0x45,  // cmovne (bit set)
                        };
                        c.extend_from_slice(&[0x48, 0x0F, cc, 0xC2]); // cmovCC rax, rdx
                        push_rax(&mut c);
                        continue;
                    }
                }
                // Speculative fixnum fast path (bliss-jtc.27): in a pure function,
                // inline binary +,-,*,<,>,<=,>=,= for fixnum operands, guarding on
                // both being fixnums and (for arithmetic) no overflow. A failed
                // guard jumps to the shared deopt block, which flags a deopt and
                // returns; run_native then re-runs the function in the
                // interpreter, yielding the correct value (e.g. a bignum).
                if deopt_safe && *nargs == 2 {
                    if let Some(op) = inlinable_fixnum_op(*sym) {
                        // Pop operands: top=a1 -> rcx, next=a0 -> rax.
                        pop_into(&mut c, 1, false); // a1 -> rcx
                        pop_into(&mut c, 0, false); // a0 -> rax
                        // Fixnum guard: (a0 | a1) low 3 bits must be 000.
                        c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                        c.extend_from_slice(&[0x48, 0x09, 0xCA]); // or rdx, rcx
                        c.extend_from_slice(&[0xF6, 0xC2, 0x07]); // test dl, 7
                        jcc_deopt(&mut c, &mut deopt_sites, 0x85); // jnz deopt
                        match op {
                            FixnumOp::Add => {
                                // (a0<<3)+(a1<<3) = (a0+a1)<<3; jo iff fixnum
                                // overflow (result exceeds 61-bit signed).
                                c.extend_from_slice(&[0x48, 0x01, 0xC8]); // add rax, rcx
                                jcc_deopt(&mut c, &mut deopt_sites, 0x80); // jo deopt
                                push_rax(&mut c);
                            }
                            FixnumOp::Sub => {
                                c.extend_from_slice(&[0x48, 0x29, 0xC8]); // sub rax, rcx
                                jcc_deopt(&mut c, &mut deopt_sites, 0x80); // jo deopt
                                push_rax(&mut c);
                            }
                            FixnumOp::Mul => {
                                // Untag a0, then a0 * (a1<<3) = (a0*a1)<<3.
                                c.extend_from_slice(&[0x48, 0xC1, 0xF8, 0x03]); // sar rax, 3
                                c.extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC1]); // imul rax, rcx
                                jcc_deopt(&mut c, &mut deopt_sites, 0x80); // jo deopt
                                push_rax(&mut c);
                            }
                            FixnumOp::Lt | FixnumOp::Gt | FixnumOp::Le | FixnumOp::Ge
                            | FixnumOp::NumEq => {
                                // cmp a0, a1 (order-preserving on tagged fixnums),
                                // then materialise T/NIL by the signed condition.
                                c.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, NIL
                                c.extend_from_slice(&bliss_rt::value::NIL_BITS.to_le_bytes());
                                c.extend_from_slice(&[0x48, 0xBA]); // mov rdx, T
                                c.extend_from_slice(&bliss_rt::value::T_BITS.to_le_bytes());
                                let cmov = match op {
                                    FixnumOp::Lt => 0x4C,    // cmovl
                                    FixnumOp::Gt => 0x4F,    // cmovg
                                    FixnumOp::Le => 0x4E,    // cmovle
                                    FixnumOp::Ge => 0x4D,    // cmovge
                                    FixnumOp::NumEq => 0x44, // cmove
                                    _ => unreachable!(),
                                };
                                // cmovCC rax, rdx  (48 0F cc C2)
                                c.extend_from_slice(&[0x48, 0x0F, cmov, 0xC2]);
                                push_rax(&mut c);
                            }
                        }
                        continue;
                    }
                }
                // c2i_call(sym, n, a0, a1, a2): rdi=sym, rsi=n, rdx=a0, rcx=a1,
                // r8=a2. Top of stack is the last arg.
                match *nargs {
                    0 => {}
                    1 => pop_into(&mut c, 2, false), // a0 -> rdx
                    2 => {
                        pop_into(&mut c, 1, false); // a1 -> rcx
                        pop_into(&mut c, 2, false); // a0 -> rdx
                    }
                    3 => {
                        pop_into(&mut c, 0, true); // a2 -> r8
                        pop_into(&mut c, 1, false); // a1 -> rcx
                        pop_into(&mut c, 2, false); // a0 -> rdx
                    }
                    _ => return None,
                }
                c.extend_from_slice(&[0x48, 0xBF]); // mov rdi, imm64 (sym)
                c.extend_from_slice(&(*sym as u64).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xBE]); // mov rsi, imm64 (nargs)
                c.extend_from_slice(&(*nargs as u64).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i)
                c.extend_from_slice(&c2i_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]); // call rax
                push_rax(&mut c);
            }
            Instr::Br(target) => {
                c.extend_from_slice(&[0xE9]); // jmp rel32
                patches.push((c.len(), *target));
                c.extend_from_slice(&[0, 0, 0, 0]);
            }
            // Block/tagbody loops (bliss-jtc.25): DO/DOTIMES/LOOP lower to
            // (block nil (tagbody ...)). In the interpreter PushBlock/PushTag
            // install handlers so a *non-local* return/go (from a captured
            // block/tag or a deeper frame) can unwind here. A T1-eligible
            // function is a leaf — try_promote_to_t1 rejects inter-bytecode
            // calls, and lower_block/lower_go bail on captured block/tag names —
            // so every compiled ReturnFrom/Go is a lexically local transfer with
            // no non-local entry possible; the handler state is dead. Native
            // execution has no Activation handler stack, so PushBlock/PushTag/
            // PopHandler are elided. Any catch/unwind-protect emits opcodes this
            // codegen does not handle and bails at the catch-all, so a
            // PopHandler reaching here can only partner a PushBlock/PushTag.
            Instr::PushBlock { .. } => {}
            Instr::PushTag { .. } => {}
            Instr::PopHandler => {}
            Instr::ClearMv => {
                // Reset the thread's multiple-values state (SETQ et al. are not
                // value-preserving). rax is dead between statements; r14/r15 are
                // callee-saved across the call, and the prologue keeps rsp
                // 16-aligned for calls (same contract as CallNamed).
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i_clear_mv)
                c.extend_from_slice(&clear_mv_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]); // call rax
            }
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                // Reset the operand stack to the tagbody's entry depth, then
                // jump (go yields no value).
                if let Some(&sp) = tag_sp.get(tagbody_id) {
                    reset_r15(&mut c, sp);
                } else {
                    return None; // go with no lexically visible tag: not T1-safe
                }
                c.extend_from_slice(&[0xE9]); // jmp rel32
                patches.push((c.len(), *target_bcp));
                c.extend_from_slice(&[0, 0, 0, 0]);
            }
            Instr::ReturnFrom { block_id } => {
                // The return value is on top of the operand stack. Restore the
                // block's entry depth, re-push the value (the block yields it),
                // and jump to the block's resume point.
                let (resume_bcp, sp) = match block_targets.get(block_id) {
                    Some(&t) => t,
                    None => return None, // non-local block: not T1-safe
                };
                c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF8]); // mov rax, [r15-8]
                reset_r15(&mut c, sp);
                push_rax(&mut c); // depth = sp_restore + 1 (value on top)
                c.extend_from_slice(&[0xE9]); // jmp rel32
                patches.push((c.len(), resume_bcp));
                c.extend_from_slice(&[0, 0, 0, 0]);
            }
            Instr::BrIfFalse(target) => {
                pop_into(&mut c, 0, false); // rax = value
                c.extend_from_slice(&[0x48, 0x3D]); // cmp rax, imm32
                c.extend_from_slice(&(bliss_rt::value::NIL_BITS as u32).to_le_bytes());
                c.extend_from_slice(&[0x0F, 0x84]); // je rel32
                patches.push((c.len(), *target));
                c.extend_from_slice(&[0, 0, 0, 0]);
            }
            Instr::Return => {
                pop_into(&mut c, 0, false); // rax = result
                c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
                c.extend_from_slice(&[0x41, 0x5F]); // pop r15
                c.extend_from_slice(&[0x41, 0x5E]); // pop r14
                c.extend_from_slice(&[0xC3]); // ret
            }
            _ => return None,
        }
    }

    // Shared deopt block (bliss-jtc.27): reached only via guard branches. Flag a
    // deoptimization, then return through the normal epilogue (the value is
    // ignored — run_native re-runs the function in the interpreter). rsp is
    // 16-aligned here exactly as at any CallNamed, so the call is well-formed.
    if !deopt_sites.is_empty() {
        let deopt_off = c.len();
        c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i_deopt)
        c.extend_from_slice(&deopt_addr.to_le_bytes());
        c.extend_from_slice(&[0xFF, 0xD0]); // call rax
        c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
        c.extend_from_slice(&[0x41, 0x5F]); // pop r15
        c.extend_from_slice(&[0x41, 0x5E]); // pop r14
        c.extend_from_slice(&[0xC3]); // ret
        for site in deopt_sites {
            let rel = deopt_off as i64 - (site as i64 + 4);
            let rel32 = i32::try_from(rel).ok()?;
            c[site..site + 4].copy_from_slice(&rel32.to_le_bytes());
        }
    }

    for (site, target) in patches {
        // A branch/return target must land on a real instruction offset. A
        // block `resume_bcp` can equal code.len() only for a well-formed body
        // that always ends in Return, so an out-of-range target means malformed
        // input — bail rather than index out of bounds.
        let target_off = *offsets.get(target as usize)? as i64;
        let rel = target_off - (site as i64 + 4);
        let rel32 = i32::try_from(rel).ok()?;
        c[site..site + 4].copy_from_slice(&rel32.to_le_bytes());
    }
    Some(c)
}

/// Whether `sym` names a primitive that is safe to re-execute from scratch — no
/// observable side effects (bliss-jtc.27). Speculative T1 codegen deoptimizes by
/// re-running the whole function in the interpreter, so it may only speculate in
/// functions whose every call is to such a primitive; then re-running from the
/// start is guaranteed to produce the same result. Pure numeric/comparison/list
/// constructors and accessors qualify; anything doing I/O, mutation, RNG, or
/// time does not, and simply keeps the function out of speculative mode.
fn is_deopt_safe_primitive(sym: u32) -> bool {
    matches!(
        bliss_rt::symbols::symbol_name(sym).as_deref(),
        Some(
            // arithmetic / comparison (some inlined, all re-run-safe)
            "+" | "-" | "*" | "/" | "<" | ">" | "<=" | ">=" | "=" | "/="
            | "1+" | "1-" | "MIN" | "MAX" | "ABS" | "MOD" | "REM" | "GCD" | "LCM"
            | "FLOOR" | "CEILING" | "TRUNCATE" | "ROUND" | "EXPT" | "ISQRT"
            | "LOGAND" | "LOGIOR" | "LOGXOR" | "LOGNOT" | "ASH"
            | "ZEROP" | "PLUSP" | "MINUSP" | "EVENP" | "ODDP"
            | "NUMBERP" | "INTEGERP" | "FLOATP" | "REALP" | "RATIONALP"
            // pure list constructors / accessors (allocation is not observable)
            | "CONS" | "CAR" | "CDR" | "LIST" | "NULL" | "NOT" | "EQ" | "EQL"
            | "FIRST" | "REST" | "CONSP" | "ATOM" | "SYMBOLP"
        )
    )
}

/// Which binary primitive `sym` has an inlined fixnum fast path in T1 speculative
/// codegen (bliss-jtc.27), if any. Returns a tag the codegen switches on.
#[derive(Clone, Copy)]
enum FixnumOp {
    Add,
    Sub,
    Mul,
    Lt,
    Gt,
    Le,
    Ge,
    NumEq,
}

fn inlinable_fixnum_op(sym: u32) -> Option<FixnumOp> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("+") => Some(FixnumOp::Add),
        Some("-") => Some(FixnumOp::Sub),
        Some("*") => Some(FixnumOp::Mul),
        Some("<") => Some(FixnumOp::Lt),
        Some(">") => Some(FixnumOp::Gt),
        Some("<=") => Some(FixnumOp::Le),
        Some(">=") => Some(FixnumOp::Ge),
        Some("=") => Some(FixnumOp::NumEq),
        _ => None,
    }
}

/// Unary fixnum ops with an inlined T1 fast path (bliss-jtc.27): the increment,
/// decrement, and negation that saturate loop bodies. On the tagged
/// representation (n<<3) these are add/sub of 1<<3 and a two's-complement negate,
/// each guarded by `jo` for the fixnum-overflow boundary.
#[derive(Clone, Copy)]
enum UnaryFixnumOp {
    Incr, // 1+
    Decr, // 1-
    Neg,  // - (unary)
}

fn inlinable_unary_fixnum_op(sym: u32) -> Option<UnaryFixnumOp> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("1+") => Some(UnaryFixnumOp::Incr),
        Some("1-") => Some(UnaryFixnumOp::Decr),
        Some("-") => Some(UnaryFixnumOp::Neg),
        _ => None,
    }
}

/// Unary fixnum predicates with an inlined T1 fast path (bliss-jtc.27): the sign
/// and parity tests that gate loop conditions. Each guards a fixnum operand,
/// sets flags, and materialises T/NIL with cmov — no branch, no c2i.
#[derive(Clone, Copy)]
enum FixnumPred {
    Zerop,
    Plusp,
    Minusp,
    Evenp,
    Oddp,
}

fn inlinable_fixnum_pred(sym: u32) -> Option<FixnumPred> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("ZEROP") => Some(FixnumPred::Zerop),
        Some("PLUSP") => Some(FixnumPred::Plusp),
        Some("MINUSP") => Some(FixnumPred::Minusp),
        Some("EVENP") => Some(FixnumPred::Evenp),
        Some("ODDP") => Some(FixnumPred::Oddp),
        _ => None,
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn emit_native_x86(_bf: &BytecodeFunction) -> Option<Vec<u8>> {
    None
}

/// Try to promote `sym`'s bytecode function to T1 native code (install into
/// executable memory). Returns the installed code, or `None` if it can't be
/// compiled to native.
fn try_promote_to_t1(sym: u32) -> Option<Rc<NativeCode>> {
    // Blacklisted (bliss-jtc.27): a function whose speculation repeatedly failed
    // is not recompiled — it stays in T0 to avoid churning through deopts.
    if DEOPT_BLACKLIST.with(|s| s.borrow().contains(&sym)) {
        return None;
    }
    let bf = registry_get(sym)?;
    // Only promote leaf-ish functions: a T1 function's calls cross back through
    // c2i into the interpreter, so a call to another *bytecode* function (which
    // could recurse) would grow the native/Rust stack per level. Functions that
    // call only primitives/interpreted builtins are safe; recursive and
    // inter-bytecode-calling functions stay T0 (flat loop, BlissStack-bounded).
    for instr in &bf.code {
        if let Instr::CallNamed { sym: callee, .. } = instr {
            if registry_get(*callee).is_some() {
                return None;
            }
        }
    }
    let code = emit_native_x86(&bf)?;
    let num_slots = bf.num_slots();
    // Install-time GC contract (bliss-jtc.4, R4.46): a validated stack map for
    // the activation's safepoint must exist, or the code is not installed.
    let code_info = install_stack_map(num_slots)?;
    let buf = bliss_rt::jit::JitBuffer::new(&code)?;
    let entry = buf.leak();
    // Emit a Linux perf symbol-map entry so `perf` can symbolicate this T1 frame
    // (bliss-jtc.10) — the same mechanism HotSpot uses for its JIT code.
    maybe_write_perf_map(entry as usize, code.len(), sym);
    let nc = Rc::new(NativeCode {
        entry,
        num_slots,
        code_info,
    });
    NATIVE_REGISTRY.with(|r| r.borrow_mut().insert(sym, Rc::clone(&nc)));
    Some(nc)
}

// ── Top-level driver ───────────────────────────────────────────────

/// Evaluate one top-level form under the active backend.
///
/// The default backend is the tree-walker; with `BLISS_BACKEND=bytecode` this
/// compiles what it can and runs it on the `BlissStack`, falling back to the
/// tree-walker for everything else. Either way results match the tree-walker
/// oracle (see module docs).
pub fn eval_toplevel(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    if !backend_is_bytecode() {
        return eval_form(form, env);
    }

    // A top-level `defun`: let the tree-walker register it (so the oracle and
    // host-fallback path both see it), then compile a bytecode version so
    // calls to it run as native frames.
    if let Some((name, params, body)) = as_defun(form) {
        let result = eval_form(form, env)?;
        if let Some(sym) = symbol_index_of(&name) {
            match compile_function(&name, params, body, env) {
                Some(bf) => {
                    trace("compiled");
                    registry_put(sym, Rc::new(bf));
                }
                // Redefinition that no longer compiles must not leave stale
                // bytecode behind — drop it so calls fall back to the tree-walker.
                None => {
                    trace("bailed");
                    registry_remove(sym);
                }
            }
        }
        return Ok(result);
    }

    // Any other form: compile a thunk, else fall back.
    match compile_thunk(form, env) {
        Some(bf) => {
            trace("compiled");
            let arc = Rc::new(bf);
            let fn_val = NIL;
            run(arc, &[], fn_val, env)
        }
        None => {
            trace("bailed");
            eval_form(form, env)
        }
    }
}

/// Emit a one-word trace line when `BLISS_BYTECODE_TRACE` is set — used by the
/// differential tests to assert a program actually ran on the bytecode backend
/// rather than silently falling back to the tree-walker.
fn trace(what: &str) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var_os("BLISS_BYTECODE_TRACE").is_some());
    if on {
        eprintln!("[bytecode] {what}");
    }
}

/// Match `(DEFUN name params . body)`, returning `(name, params, body)`.
fn as_defun(form: BlissVal) -> Option<(String, BlissVal, BlissVal)> {
    if !form.is_cons() {
        return None;
    }
    let (op, rest) = cp(form);
    if !op.is_symbol() || sym_name(op) != "DEFUN" {
        return None;
    }
    let (name_sym, rest) = cp(rest);
    if !name_sym.is_symbol() {
        return None;
    }
    let (params, body) = cp(rest);
    Some((sym_name(name_sym), params, body))
}

/// Resolve a symbol name to its interned index via the reader.
fn symbol_index_of(name: &str) -> Option<u32> {
    match reader::read_from_string(name) {
        Ok((sym, _)) if sym.is_symbol() => Some(sym.as_symbol_index()),
        _ => None,
    }
}

// ── T1 GC stack-map install contract (bliss-jtc.4) ────────────────

#[cfg(test)]
mod jtc4_stack_map_tests {
    use super::*;

    /// install_stack_map builds a validated map covering every activation slot,
    /// and its entry safepoint (pc 0) resolves to a ref bitmap of the right size.
    #[test]
    fn install_stack_map_validates_and_covers_all_slots() {
        let ci = install_stack_map(3).expect("a 3-slot map must install");
        let map = ci
            .stack_map(0)
            .expect("entry safepoint must have a stack map");
        // 3 slots → 1 byte, bits 0..3 set (all tagged BlissVal references).
        assert_eq!(map.len(), 1);
        assert_eq!(map[0] & 0b0000_0111, 0b0000_0111);

        // A leaf activation with no reference slots legitimately has no bitmap.
        let leaf = install_stack_map(0).expect("a 0-slot leaf map must install");
        assert!(leaf.stack_map(0).is_none());
    }

    /// The install contract (R4.46): code whose entry safepoint has no registered
    /// stack map must be rejected. A CodeInfo built with an empty stack-map table
    /// exposes the "missing map" condition install checks for.
    #[test]
    fn missing_stack_map_is_detectable_and_would_abort_install() {
        let no_maps = CodeInfo::new(&[], &[]);
        assert!(
            no_maps.stack_map(0).is_none(),
            "a safepoint with no registered map resolves to None — install must reject it"
        );
    }
}
