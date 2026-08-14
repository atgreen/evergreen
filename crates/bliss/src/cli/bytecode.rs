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

use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_rt::{CodeInfo, Frame};

use super::{
    Env, HandlerCluster, HandlerEntry, HandlerImpl, apply_function, bliss_error_to_condition,
    condition_matches_handler, cp, eval_form, handler_case_token, list_to_vec, next_control_token,
    store_control_value, sym_name, tag_key, take_control_value, val_as_str,
};

// ── Backend selection ──────────────────────────────────────────────

/// Whether the bytecode backend is enabled (`BLISS_BACKEND=bytecode`).
///
/// Read once and cached; the tree-walker remains the default.
pub fn backend_is_bytecode() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("BLISS_BACKEND")
            .map(|v| v.eq_ignore_ascii_case("bytecode"))
            .unwrap_or(false)
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
    /// Discard the top of the operand stack.
    Pop,
    /// Unconditional jump: set the bytecode pointer to `target`.
    Br(u32),
    /// Pop; if it is NIL, jump to `target`, else fall through.
    BrIfFalse(u32),
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
    /// Establish a `BLOCK` exit handler keyed by the lexical `block_id`.
    PushBlock { block_id: u32, resume_bcp: u32, sp_restore: u16 },
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

// ── Per-thread registry of compiled functions ─────────────────────

thread_local! {
    /// Bytecode functions keyed by symbol index. A `CallNamed` checks this
    /// first; a hit runs as a native frame on the `BlissStack`, a miss falls
    /// back to `apply_function` (builtins, generics, tree-walker functions).
    static REGISTRY: RefCell<HashMap<u32, Rc<BytecodeFunction>>> = RefCell::new(HashMap::new());
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
];

/// Compiler state for lowering one function body.
struct Lowerer<'e> {
    code: Vec<Instr>,
    constants: Vec<BlissVal>,
    /// Lexical scope: name → local slot index. A `Vec` of frames so `let`
    /// bindings shadow correctly and unbind at scope exit.
    scopes: Vec<HashMap<String, u16>>,
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
            next_local: 0,
            n_locals: 0,
            cur_stack: 0,
            max_stack: 0,
            next_id: 0,
            block_scope: Vec::new(),
            tag_scope: Vec::new(),
            pending_gos: Vec::new(),
            handler_cases: Vec::new(),
            env,
        }
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

    fn alloc_local(&mut self, name: &str) -> u16 {
        let idx = self.next_local;
        self.next_local += 1;
        if self.next_local > self.n_locals {
            self.n_locals = self.next_local;
        }
        self.scopes.last_mut().unwrap().insert(name.to_string(), idx);
        idx
    }

    fn lookup_local(&self, name: &str) -> Option<u16> {
        for scope in self.scopes.iter().rev() {
            if let Some(&idx) = scope.get(name) {
                return Some(idx);
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
            if let Some(slot) = self.lookup_local(&name) {
                self.emit(Instr::LoadLocal(slot));
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
                "PROGN" => self.lower_progn(rest),
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
                "UNWIND-PROTECT" => self.lower_unwind_protect(rest),
                "HANDLER-CASE" => self.lower_handler_case(rest),
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

    fn lower_let(&mut self, rest: BlissVal, sequential: bool) -> LowerResult<()> {
        let (bindings, body) = cp(rest);
        let binding_forms = list_to_vec(bindings);
        let saved_next_local = self.next_local;

        if sequential {
            // LET*: each init sees prior bindings.
            self.enter_scope();
            for b in &binding_forms {
                let (name, init) = binding_name_init(*b)?;
                self.lower_expr(init)?;
                let slot = self.alloc_local(&name);
                self.emit(Instr::StoreLocal(slot));
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
            // Values are on the stack in binding order; store in reverse.
            let mut slots = Vec::with_capacity(names.len());
            for name in &names {
                slots.push(self.alloc_local(name));
            }
            for slot in slots.into_iter().rev() {
                self.emit(Instr::StoreLocal(slot));
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
        Ok(())
    }

    fn lower_call(&mut self, name: &str, op: BlissVal, rest: BlissVal) -> LowerResult<()> {
        // Never treat a macro or an unhandled special operator as a call.
        if self.env.macros.contains_key(name) || is_bail_special(name) {
            return Err(Bail);
        }
        // Only emit a call when the callee is certainly a function: a
        // user-defined function or an allowlisted primitive.
        let is_user_fn = self.env.funs.contains_key(name);
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
                Some(slot) => {
                    self.emit(Instr::StoreLocal(slot));
                    self.pop_n(1);
                    if last {
                        self.emit(Instr::LoadLocal(slot));
                        self.push_n(1);
                    }
                }
                None => {
                    let sym = var.as_symbol_index();
                    self.emit(Instr::StoreGlobal(sym));
                    self.pop_n(1);
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
        let sp_restore = self.cur_stack;
        self.emit(Instr::PushBlock {
            block_id,
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
                Some(self.alloc_local(&sym_name(var)))
            } else {
                None
            };

            let body_bcp = self.code.len() as u32;
            // The driver lands here with the operand stack at `sp_restore` and
            // the condition already stored in `var_slot`.
            self.cur_stack = sp_restore;
            self.lower_progn(body)?; // clause value (+1)
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
}

/// Extract `(name init)` from a `let` binding, which may also be a bare symbol.
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
        "FUNCTION"
            | "LAMBDA"
            | "SETQ"
            | "SETF"
            | "DEFUN"
            | "DEFMACRO"
            | "DEFVAR"
            | "DEFPARAMETER"
            | "DEFCONSTANT"
            | "FLET"
            | "LABELS"
            | "MACROLET"
            | "SYMBOL-MACROLET"
            | "THE"
            | "LOCALLY"
            | "DECLARE"
            | "EVAL-WHEN"
            | "LOAD-TIME-VALUE"
            | "PROGV"
            | "MULTIPLE-VALUE-BIND"
            | "MULTIPLE-VALUE-CALL"
            | "MULTIPLE-VALUE-PROG1"
            | "MULTIPLE-VALUE-LIST"
            | "VALUES"
            | "COND"
            | "CASE"
            | "TYPECASE"
            | "ECASE"
            | "AND"
            | "OR"
            | "WHEN"
            | "UNLESS"
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
            | "HANDLER-BIND"
            | "RESTART-CASE"
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
            | "APPLY"
            | "FUNCALL"
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
    for pn in &param_names {
        lo.alloc_local(pn);
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
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: param_names.len() as u16,
        name: name.to_string(),
    })
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
    if lo.lower_expr(form).is_err() {
        return None;
    }
    lo.emit(Instr::Return);
    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: 0,
        name: "<toplevel>".to_string(),
    })
}

// ── Execution ──────────────────────────────────────────────────────

/// A live non-local-exit handler established on an activation (§2.4.3 frame
/// types CATCH / UNWIND / a block/tag marker). Handlers form a per-activation
/// stack; unwinding walks them newest-first.
#[derive(Clone)]
enum Handler {
    /// `CATCH`: keyed by the control token shared with `env.catch_stack`.
    Catch { token: String, resume_bcp: u32, sp_restore: u16 },
    /// `BLOCK`: keyed by a lexical compile-time id.
    Block { block_id: u32, resume_bcp: u32, sp_restore: u16 },
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
        .push_frame(entry_fn_val, std::ptr::null::<CodeInfo>(), entry.num_slots(), FLAG_CALL)
        .ok_or_else(|| BlissError::StackOverflow(bliss_rt::current_thread_id()))?;
    for (i, a) in args.iter().enumerate() {
        unsafe { slot_set(frame, i as u16, *a) };
    }
    let mut acts: Vec<Activation> = vec![Activation {
        frame,
        n_locals: entry.n_locals,
        func: entry,
        bcp: 0,
        sp_top: 0,
        handlers: Vec::new(),
        cleanup_conts: Vec::new(),
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
            Instr::Pop => {
                acts[top_idx].pop_op();
            }
            Instr::Br(target) => {
                acts[top_idx].bcp = target as usize;
            }
            Instr::BrIfFalse(target) => {
                let v = acts[top_idx].pop_op();
                if v.is_nil() {
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

                // Bytecode callee → native frame on the BlissStack.
                if let Some(callee) = registry_get(sym) {
                    if callee.arity == nargs {
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
                                let e =
                                    BlissError::StackOverflow(bliss_rt::current_thread_id());
                                initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                                continue;
                            }
                        };
                        for (i, a) in args.iter().enumerate() {
                            unsafe { slot_set(frame, i as u16, *a) };
                        }
                        acts.push(Activation {
                            frame,
                            n_locals: callee.n_locals,
                            func: callee,
                            bcp: 0,
                            sp_top: 0,
                            handlers: Vec::new(),
                            cleanup_conts: Vec::new(),
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
                        if let Handler::Catch { token, .. } = h {
                            env.catch_stack.retain(|(_, t)| *t != token);
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
                resume_bcp,
                sp_restore,
            } => {
                acts[top_idx].handlers.push(Handler::Block {
                    block_id,
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
            Instr::PopHandler => {
                if let Some(Handler::Catch { token, .. }) = acts[top_idx].handlers.pop() {
                    env.catch_stack.retain(|(_, t)| *t != token);
                }
            }
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
    pending: Pending,
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
                resume_bcp,
                sp_restore,
            }) => {
                acts[top].handlers.pop();
                if let Pending::Return {
                    block_id: bid,
                    value,
                } = &pending
                {
                    if *bid == block_id {
                        let v = *value;
                        let act = &mut acts[top];
                        act.sp_top = sp_restore;
                        act.bcp = resume_bcp as usize;
                        act.push_op(v);
                        return Ok(());
                    }
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
        if env.catch_stack.iter().any(|(_, t)| t == token) {
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
        Pending::Return { .. } => {
            BlissError::Internal("RETURN-FROM: no visible block".into())
        }
        Pending::Go { .. } => BlissError::Internal("GO: no such tag".into()),
    }
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
