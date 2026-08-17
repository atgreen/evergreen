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
// Label-based assembler backing the native (T1) code emitter (see cli::asm).
use bliss_rt::asm::{Asm, Cc, Label};

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
// The bytecode data types are the canonical cross-tier coordinate system and
// now live in `bliss-rt` (spec §4.4/§4.10) so that `bliss-compiler`'s T2 can
// build its SSA IR from bytecode without a `bliss → bliss-compiler → bliss`
// dependency cycle. This crate keeps the *lowering* (forms → bytecode), the T0
// interpreter, and the T1 native emitter that operate on these types.
use bliss_rt::bytecode::{
    BytecodeFunction, ClauseInfo, HandlerBindInfo, HandlerCaseInfo, Instr, RestartCaseInfo, VarLoc,
};

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

/// Call a registered GLOBAL bytecode function `sym` from the tree-walker,
/// driving T0→T1 promotion — the same trigger the bytecode `CallNamed` path
/// uses. Without this, a function called only from tree-walked code (e.g. a
/// top-level `loop`, which is a tree-walker special form and never compiles to
/// bytecode) would never cross the promotion threshold and would run
/// interpreted forever. Returns `None` if `sym` is not a registered bytecode
/// function of matching arity, so the caller falls back to the tree-walker.
///
/// The caller MUST have already established that `sym` names the *global*
/// function (no lexical FLET/LABELS shadow) and counted the invocation (via
/// `callable_body`/`global_fn`), so this reads — not re-bumps — the counter for
/// a function object.
pub fn call_registered(
    sym: u32,
    args: &[BlissVal],
    fn_val: BlissVal,
    env: &mut Env,
) -> Option<Result<BlissVal, BlissError>> {
    let callee = registry_get(sym)?;
    if callee.arity as usize != args.len() {
        return None; // arity mismatch (e.g. &optional/&rest): let the tree-walker bind it
    }
    let fn_obj = bliss_rt::symbols::symbol_function(sym)
        .filter(|&c| bliss_rt::function::is_interpreted_function(c));
    // Use installed native code, or promote on crossing the threshold.
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
        let nc = try_promote(sym)?;
        if let Some(f) = fn_obj {
            bliss_rt::function::set_entry(f, nc.entry as *mut u8);
            bliss_rt::function::set_tier(f, 1);
        }
        Some(nc)
    });
    // Dispatch: native if promoted and under the depth cap, else run the callee
    // as BYTECODE — the profiling warmup tier. This is what gathers the operand
    // -type profile a function needs before it can be speculated at T2, even when
    // it is only ever reached from tree-walked code (e.g. a top-level `loop`).
    // Running through bytecode is MV-correct now that `run` clears stale values
    // on entry, and is equivalent to the tree-walker for a compiled function.
    match native {
        Some(nc) if NATIVE_DEPTH.with(|d| d.get()) < native_depth_cap() => {
            Some(run_native(&nc, sym, args, env))
        }
        _ => Some(run(callee, args, fn_val, env)),
    }
}

/// Render a constant-pool value compactly for a bytecode annotation.
fn fmt_const_val(v: BlissVal) -> String {
    if v == NIL {
        "NIL".to_string()
    } else if v == T {
        "T".to_string()
    } else if v.is_fixnum() {
        format!("{}", v.as_fixnum())
    } else if v.is_single_float() {
        format!("{}", v.as_single_float())
    } else if v.is_symbol() {
        bliss_rt::symbols::symbol_name(v.as_symbol_index()).unwrap_or_else(|| "?sym".to_string())
    } else if v.is_cons() {
        "#<list>".to_string()
    } else {
        format!("#<0x{:016x}>", v.0)
    }
}

/// The name for a symbol index, for annotations.
fn sym_label(sym: u32) -> String {
    bliss_rt::symbols::symbol_name(sym).unwrap_or_else(|| format!("#{sym}"))
}

/// `disassemble` (spec §6, CL:DISASSEMBLE): render a function's *current tier* —
/// the annotated bytecode listing when it runs in the T0 interpreter, or the
/// decoded x86-64 machine instructions when it has been promoted to native (T1).
/// Returns `None` if `sym` names no compiled Bliss function (e.g. a builtin or a
/// tree-walked closure), so the caller can fall back.
pub fn disassemble_by_symbol(sym: u32) -> Option<String> {
    let bf = registry_get(sym)?;
    let native = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned());
    let mut out = String::new();
    use std::fmt::Write;

    let name = sym_label(sym);
    let tier = match &native {
        Some(nc) if nc.is_t2 => "T2 (native, profile-guided)",
        Some(_) => "T1 (native)",
        None => "T0 (bytecode interpreter)",
    };
    let _ = writeln!(
        out,
        "; disassembly of {name} — {} arg(s), {} local(s), {} stack slot(s)  [tier: {tier}]",
        bf.arity, bf.n_locals, bf.max_stack
    );

    match native {
        // Promoted to native: decode the installed machine code (spec: "otherwise
        // machine instructions"). The code is R+X-mapped, so reading it is safe.
        Some(nc) => {
            let _ = writeln!(out, "; {} bytes of x86-64 at {:p}", nc.code_len, nc.entry);
            let bytes = unsafe { std::slice::from_raw_parts(nc.entry, nc.code_len) };
            let mut dec =
                iced_x86::Decoder::with_ip(64, bytes, nc.entry as u64, iced_x86::DecoderOptions::NONE);
            let mut fmt = iced_x86::NasmFormatter::new();
            let mut insn = iced_x86::Instruction::default();
            let mut line = String::new();
            while dec.can_decode() {
                dec.decode_out(&mut insn);
                line.clear();
                use iced_x86::Formatter;
                fmt.format(&insn, &mut line);
                let _ = writeln!(out, "  {:#018x}:  {line}", insn.ip());
            }
        }
        // Interpreted: the annotated bytecode listing (spec: "show the bytecode").
        None => {
            let func_ptr = Rc::as_ptr(&bf) as usize;
            for (pc, instr) in bf.code.iter().enumerate() {
                let ann = match instr {
                    Instr::Const(i) => bf
                        .constants
                        .get(*i as usize)
                        .map(|c| fmt_const_val(*c))
                        .unwrap_or_default(),
                    Instr::CallNamed { sym, nargs } => {
                        let mut a = format!("({} …) / {nargs} arg(s)", sym_label(*sym));
                        if let Some(p) = type_profile_at(func_ptr, pc as u32) {
                            let spec = match p.dominant() {
                                Some(SpecType::Fixnum) => " ⇒ speculate FIXNUM",
                                Some(SpecType::SingleFloat) => " ⇒ speculate SINGLE-FLOAT",
                                None => " ⇒ generic (polymorphic / cold)",
                            };
                            a.push_str(&format!(
                                "  [profile fix:{} float:{} other:{}{}]",
                                p.fixnum, p.single_float, p.other, spec
                            ));
                        }
                        a
                    }
                    Instr::LoadGlobal(s) | Instr::StoreGlobal(s) => sym_label(*s),
                    Instr::Br(t) | Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => format!("→ {t}"),
                    Instr::Go { target_bcp, .. } => format!("→ {target_bcp}"),
                    _ => String::new(),
                };
                if ann.is_empty() {
                    let _ = writeln!(out, "  {pc:>4}: {instr:?}");
                } else {
                    let _ = writeln!(out, "  {pc:>4}: {instr:?}    ; {ann}");
                }
            }
        }
    }
    Some(out)
}

// ── Lowering ───────────────────────────────────────────────────────

/// A form the compiler does not (yet) lower. Propagated up to trigger a
/// clean bail to the tree-walker.
struct Bail;

type LowerResult<T> = Result<T, Bail>;

thread_local! {
    /// Histogram of *why* the bytecode lowerer bailed, keyed by a short reason
    /// (e.g. "call:MAKE-HASH-TABLE", "special:HANDLER-CASE"), for the
    /// compile-coverage diagnostic (bliss-x5y.1). Populated only when
    /// BLISS_BAIL_TRACE is set; read via `bliss-ext:bail-report`.
    static BAIL_LOG: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
}

fn bail_trace_on() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BLISS_BAIL_TRACE").is_some())
}

/// Record why the lowerer is about to bail. The reason is built lazily so
/// there is no cost unless BLISS_BAIL_TRACE is set. Returns `Bail` for
/// `return Err(record_bail(...))` at bail sites.
fn record_bail(reason: impl FnOnce() -> String) -> Bail {
    if bail_trace_on() {
        BAIL_LOG.with(|m| *m.borrow_mut().entry(reason()).or_insert(0) += 1);
    }
    Bail
}

/// Snapshot of the bail histogram, most frequent first (bliss-x5y.1).
pub fn bail_report() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = BAIL_LOG.with(|m| {
        m.borrow().iter().map(|(k, &n)| (k.clone(), n)).collect()
    });
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

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
    // Pure/positional builtins surfaced by the x5y.1 bail histogram over
    // lib/asdf.lisp (bliss-x5y.6): ordinary functions with fixed positional
    // args, reachable through apply_function, so safe to CallNamed.
    "BOUNDP",
    "FBOUNDP",
    "TYPEP",
    "TYPE-OF",
    "SLOT-VALUE",
    "SLOT-BOUNDP",
    "CHAR-CODE",
    "CODE-CHAR",
    "CHAR",
    "STRING",
    "STRINGP",
    "STRING=",
    "SYMBOLP",
    "KEYWORDP",
    "NUMBERP",
    "INTEGERP",
    "CHARACTERP",
    "FUNCTIONP",
    "ELT",
    "NTH",
    "NTHCDR",
    "SUBSEQ",
    "PATHNAME",
    "NAMESTRING",
    "PROVIDE",
    "GETHASH",
    "GENSYM",
    "MAKE-SYMBOL",
    "SYMBOL-NAME",
    "SYMBOL-PACKAGE",
    "FIND-PACKAGE",
    "PACKAGE-NAME",
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
                _ => return Err(record_bail(|| format!("macroexpand:{name}"))),
            }
        }
        // An unhandled special operator is not a call.
        if is_bail_special(name) {
            return Err(record_bail(|| format!("special:{name}")));
        }
        // Only emit a call when the callee is certainly a function: a
        // user-defined function (lexical name map or global function cell —
        // bliss-jtc.6.8) or an allowlisted primitive.
        let is_user_fn = self.env.funs.contains_key(name) || super::global_fn(name).is_some();
        let is_prim = PRIMITIVE_ALLOWLIST.contains(&name);
        if !is_user_fn && !is_prim {
            return Err(record_bail(|| format!("call:{name}")));
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
    // These lambda-list bails happen before the body is even lowered, so record
    // them here for the x5y.1 diagnostic — they are a top blocker for real
    // functions (uiop:ensure-package and friends use &key/&optional/&rest).
    let mut param_names = Vec::new();
    for p in &params {
        if !p.is_symbol() {
            let _ = record_bail(|| "lambda-list:destructure".to_string());
            return None;
        }
        let pn = sym_name(*p);
        if pn.starts_with('&') {
            let _ = record_bail(|| format!("lambda-list:{pn}"));
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
// 0x0101 (bliss-jtc.23): faithful registry-key symbol encoding + a loadable
// unit. The loader requires this version; older units fall back to source.
const BBU_BYTECODE_VERSION: u16 = 0x0101;
const BBU_VERIFIER_VERSION: u16 = 0x0100;
const BBU_NO_INDEX: u32 = u32::MAX;
/// Unit-flags bit: every load form is represented in `load_actions`, so the
/// loader may execute the bytecode unit in place of the source (bliss-jtc.23).
const BBU_UNIT_COMPLETE: u32 = 1 << 0;

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

    /// Faithful symbol reference (bliss-jtc.23): store the symbol's exact
    /// registry key (tag 14 + string ref). `intern(key)` at load reconstructs
    /// the identical symbol — package and all — so compiled code references the
    /// same symbol across a compile/load boundary. Returns `None` for uninterned
    /// symbols (no key), which makes the containing function/value unserializable
    /// and falls the form back to source loading.
    fn symbol_by_index(&mut self, idx: u32) -> Option<u32> {
        let key = bliss_rt::symbols::registry_key(idx)?;
        let name_ref = self.string(&key);
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 14);
        put_u32(&mut bytes, name_ref);
        Some(self.intern_encoded(bytes))
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
            return self.symbol_by_index(v.as_symbol_index());
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
                let cp = pool.symbol_by_index(*sym)?;
                put_u32(&mut code, cp);
            }
            Instr::StoreGlobal(sym) => {
                put_u8(&mut code, 0x08);
                let cp = pool.symbol_by_index(*sym)?;
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
                let cp = pool.symbol_by_index(*sym)?;
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

    // Every load form must be representable in order for the unit to be loaded
    // *instead of* the source (bliss-jtc.23): try, per form, (1) a precompiled
    // named function, (2) a precompiled load-time thunk, (3) the raw form to be
    // EVAL'd at load. If a form fits none (e.g. an unserialisable literal), the
    // unit is marked incomplete and the loader falls back to the source section.
    let mut complete = true;
    for &form in forms {
        let mut done = false;

        // (1) A DEFUN whose name and body serialise faithfully → install the
        // precompiled function directly at load (skips read/macroexpand/compile).
        if let Some((name, params, body)) = as_defun(form) {
            if let Some(sym) = symbol_index_of(&name) {
                if let Some(name_ref) = pool.symbol_by_index(sym) {
                    if let Some(bf) = compile_function(&name, params, body, env) {
                        if let Some(serialized) =
                            serialize_bbu_function(&bf, name_ref, BBU_FUNC_NAMED, &mut pool)
                        {
                            let function_index = functions.len() as u32;
                            functions.push(serialized);
                            load_actions.push((3, 0, function_index, name_ref, BBU_NO_INDEX));
                            done = true;
                        }
                    }
                }
            }
        }

        // (2) Any form compilable to a serialisable thunk → run precompiled.
        if !done {
            if let Some(bf) = compile_thunk(form, env) {
                if let Some(serialized) =
                    serialize_bbu_function(&bf, BBU_NO_INDEX, BBU_FUNC_LOAD_TIME_THUNK, &mut pool)
                {
                    let function_index = functions.len() as u32;
                    functions.push(serialized);
                    load_actions.push((7, 0, function_index, BBU_NO_INDEX, BBU_NO_INDEX));
                    done = true;
                }
            }
        }

        // (3) Fallback: serialise the raw form into the const pool and EVAL it at
        // load — correct and in order, just not precompiled (e.g. defmacro,
        // defpackage, or a defun with an unsupported body).
        if !done {
            if let Some(form_ref) = pool.value(form) {
                load_actions.push((9, 0, form_ref, BBU_NO_INDEX, BBU_NO_INDEX));
                done = true;
            }
        }

        // A form we cannot represent means the load plan is not a faithful,
        // ordered substitute for the source — mark the unit incomplete so the
        // loader keeps using the source section. Keep going so the function
        // records we *can* build are still present (harmless when incomplete).
        if !done {
            complete = false;
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(BBU_MAGIC);
    put_u16(&mut out, BBU_BYTECODE_VERSION);
    put_u16(&mut out, BBU_VERIFIER_VERSION);
    // Unit flags: COMPLETE means every load form is represented in `load_actions`
    // (in order), so the loader can run the unit instead of the source section.
    put_u32(&mut out, if complete { BBU_UNIT_COMPLETE } else { 0 });
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
    /// Symbol index of the function this activation runs, for OSR compilation
    /// keying (bliss-izt.1). `u32::MAX` for anonymous/toplevel wrappers, which
    /// are never OSR-compiled.
    sym: u32,
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
    // A callee's return values are determined by its own body — discard any
    // multiple-values state left by the caller's argument evaluation, so a
    // single-valued function returns exactly one value (an enclosing
    // multiple-value-bind sees NIL secondaries). This matches the tree-walker,
    // which resets the values on each single-valued form. A function that
    // genuinely returns multiple values re-establishes them via SetValues before
    // its Return.
    env.clear_mv();
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
        // is_symbol() is true for the NIL/T constants too, but as_symbol_index
        // only accepts a TAG_SYMBOL value — thunks pass NIL here, so exclude them.
        sym: if entry_fn_val.is_symbol() && !entry_fn_val.is_nil() && entry_fn_val != T {
            entry_fn_val.as_symbol_index()
        } else {
            u32::MAX
        },
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

                // Type profiling: record operand types at speculatable arithmetic
                // sites so the optimising tier can commit to one type. `bcp` was
                // already advanced past this instruction, so the site is bcp-1.
                if is_arith_speculatable(sym) {
                    let func_ptr = Rc::as_ptr(&acts[top_idx].func) as usize;
                    let call_bcp = acts[top_idx].bcp as u32 - 1;
                    record_type_profile(func_ptr, call_bcp, &args);
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
                            let nc = try_promote(sym)?;
                            if let Some(f) = fn_obj {
                                bliss_rt::function::set_entry(f, nc.entry as *mut u8);
                                bliss_rt::function::set_tier(f, 1);
                            }
                            Some(nc)
                        });
                        // Run native only while under the depth cap (bliss-x5y.4);
                        // once the native call stack is deep, dispatch the callee
                        // through the flat T0 path below instead of pushing yet
                        // another native frame, so the real C stack stays bounded.
                        let over_cap = NATIVE_DEPTH.with(|d| d.get()) >= native_depth_cap();
                        if let Some(nc) = native {
                            if !over_cap {
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
                            sym,
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
                // OSR (bliss-izt.1): once this loop is hot and the operand stack
                // is empty, finish the activation in native code instead of
                // interpreting the rest of the loop.
                match maybe_osr(&acts[top_idx], target_bcp, env) {
                    Some(Ok(OsrOutcome::Finished(v))) => {
                        release_activation_handlers(&mut acts[top_idx], env);
                        stack.pop_frame();
                        acts.pop();
                        match acts.last_mut() {
                            Some(caller) => caller.push_op(v),
                            None => return Ok(v),
                        }
                    }
                    Some(Ok(OsrOutcome::Deopt { bcp, sp_top })) => {
                        // A speculating OSR loop hit a fixnum guard mid-run
                        // (bliss-izt.2). The live frame already holds the updated
                        // locals and the peek-preserved operands, and this
                        // activation's handlers were established top-down in T0
                        // before the loop, so we simply reposition it at the guard
                        // and keep interpreting — no new frame, no handler replay.
                        DEOPT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Backoff: clear the hot-loop counter so the loop must
                        // re-warm before another OSR attempt, bounding
                        // enter/deopt thrash on a loop that keeps overflowing.
                        if let Some(f) = acts[top_idx].fn_obj {
                            bliss_rt::function::reset_back_edge_count(f);
                        }
                        let act = &mut acts[top_idx];
                        act.bcp = bcp as usize;
                        act.sp_top = sp_top;
                    }
                    Some(Err(e)) => {
                        let pending = error_to_pending(e, env);
                        initiate_unwind(acts, stack, env, pending)?;
                    }
                    None => {
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
                }
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
    let sym32 = sym as u32;
    let fn_val = BlissVal::from_symbol_index(sym32);
    // Dispatch a compiled (bytecode) callee through the T0 path `run()`, whose
    // run_loop dispatches ITS calls flatly on the BlissStack (bliss-x5y.4). This
    // is what keeps recursion through a native caller bounded: without it, a
    // native function's self/mutual recursion would go through apply_function
    // (the tree-walker), which recurses on the unbounded Rust stack and aborts.
    // With it, a bounded number of native/run frames (the NATIVE_DEPTH cap) give
    // way to flat Activations, so a runaway recursion raises a catchable
    // STORAGE-CONDITION instead of a native stack overflow. Non-bytecode callees
    // (builtins, generics, closures, arity mismatches) still go via apply_function.
    let result = match registry_get(sym32) {
        Some(callee) if callee.arity == n as u16 => {
            // bliss-x5y.8: if the callee is itself installed as native code and
            // we are under the native depth cap, call its native entry directly
            // (native → native) instead of rebuilding a full T0 `run()`
            // activation on every call. This is the difference between tiering
            // being a win or a loss for call-heavy code: previously every native
            // call bounced back into the interpreter. Beyond the cap, fall
            // through to the flat T0 `run()` so deep recursion stays bounded and
            // raises a catchable STORAGE-CONDITION rather than a C-stack abort.
            let native = if NATIVE_DEPTH.with(|d| d.get()) >= native_depth_cap() {
                None
            } else {
                NATIVE_REGISTRY.with(|r| r.borrow().get(&sym32).cloned())
            };
            match native {
                Some(nc) => run_native(&nc, sym32, args, env),
                None => run(callee, args, fn_val, env),
            }
        }
        _ => apply_function(fn_val, args, env),
    };
    match result {
        Ok(v) => v.0,
        // A raw error can't unwind native code mid-function, so stash it and
        // return NIL; native execution continues but run_native re-raises on
        // exit. Crucially, keep the FIRST error (first-error-wins): once one is
        // pending, later c2i calls that see the placeholder NIL (e.g. `(+ 1 nil)`)
        // must not overwrite it, or a real STORAGE-CONDITION from deep recursion
        // gets masked by a spurious downstream type error (bliss-x5y.4).
        Err(e) => {
            NATIVE_ERROR.with(|c| {
                let mut slot = c.borrow_mut();
                if slot.is_none() {
                    *slot = Some(e);
                }
            });
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
    /// Where a speculative guard failed, for state-transfer deopt (bliss-izt.2):
    /// `(bcp, operand_depth)`. Set alongside `NATIVE_DEOPT` by `c2i_deopt_state`.
    /// `run_native` reads it to resume T0 at that exact bytecode position on the
    /// SAME frame — locals and the operand stack are already in the shared frame
    /// slots — instead of re-running the whole function from the top. `None`
    /// falls back to the re-run path (e.g. a legacy non-state-recording guard).
    static NATIVE_DEOPT_RESUME: std::cell::Cell<Option<(u32, u16)>> =
        const { std::cell::Cell::new(None) };
    /// Current native (T1) call-stack depth (bliss-x5y.4). Non-leaf T1 functions
    /// call through c2i, and although those calls run in the interpreter (which
    /// is BlissStack-bounded, not native-recursive), this counter bounds any
    /// native re-entry defensively: past `native_depth_cap()` the dispatcher
    /// runs the callee in T0 instead of pushing another native frame, so the
    /// real C stack can never run away.
    static NATIVE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Cap on native (T1) call-stack depth before the dispatcher falls back to the
/// flat T0 interpreter path (bliss-x5y.4). Env-overridable for tests.
fn native_depth_cap() -> u32 {
    std::env::var("BLISS_NATIVE_DEPTH_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(64)
}

/// Decrements `NATIVE_DEPTH` on drop, so every exit from `run_native` (Ok, Err,
/// or deopt) restores the count.
struct NativeDepthGuard;
impl Drop for NativeDepthGuard {
    fn drop(&mut self) {
        NATIVE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
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
#[allow(dead_code)] // superseded by c2i_deopt_state (bliss-izt.2); kept for reference
extern "C" fn c2i_deopt() {
    NATIVE_DEOPT.with(|d| d.set(true));
}

/// Signal a speculative deoptimization AND record where to resume in T0
/// (bliss-izt.2). `bcp` is the bytecode index of the guarded `CallNamed`; `depth`
/// is the operand-stack depth there (derived native-side from r15/r14). Because
/// the inlined fast paths PEEK-guard-commit — they never mutate the operand
/// stack pointer before every guard has passed — the frame slots at deopt still
/// hold the live locals and the untouched operands, so `run_native` can build a
/// T0 activation at exactly this position instead of re-running from the top.
extern "C" fn c2i_deopt_state(bcp: u64, depth: u64) {
    NATIVE_DEOPT.with(|d| d.set(true));
    NATIVE_DEOPT_RESUME.with(|c| c.set(Some((bcp as u32, depth as u16))));
}

/// Installed T1 native code for a function. Its CL activation (locals + operand
/// stack) lives in a BlissStack frame that the i2c adapter pushes; the native
/// code addresses it through the frame-slot pointer passed in rdi (§D2.04).
struct NativeCode {
    entry: *const u8,
    /// Length of the installed machine code, so `disassemble` can read the
    /// (R+X mapped) code bytes back for decoding.
    code_len: usize,
    /// True if this is T2 optimising code (profile-guided single-type
    /// speculation), false for the T1 baseline. Both share the run_native ABI.
    is_t2: bool,
    num_slots: u16,
    /// Byte offset of the compiled-caller entry point within the code (args in
    /// registers `[rcx, r8, r9, r10]`, no frame — spec: compiled-caller ABI).
    /// 0 means "no distinct compiled entry"; the interpreter entry is always at 0.
    compiled_entry: usize,
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

// ── Type profiling (HotSpot-style speculation feedback) ────────────
//
// At each speculatable arithmetic call site we record what types the operands
// actually are, so the optimising tier can commit to ONE type (fixnum OR
// single-float, mutually exclusive) and guard it, rather than emitting a path
// for every possibility. Keyed by (containing bytecode function pointer, the
// CallNamed's bcp); the pointer is the registry `Rc`, so any tier that holds the
// same `BytecodeFunction` recovers the key. A deopt updates the profile too, so
// a site that proves polymorphic stops being speculated on the next recompile.

/// The single type a call site may be speculated as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpecType {
    Fixnum,
    SingleFloat,
}

/// Observed operand-type distribution at one call site.
#[derive(Default, Clone, Copy, Debug)]
pub struct TypeProfile {
    pub fixnum: u32,
    pub single_float: u32,
    pub other: u32,
}

impl TypeProfile {
    pub fn total(&self) -> u32 {
        self.fixnum + self.single_float + self.other
    }
    /// The dominant speculatable type, if the site is overwhelmingly consistent
    /// (≥90% one type over ≥`MIN` samples). `None` means "not enough data" or
    /// "polymorphic" — in which case that op is left generic (no speculation).
    pub fn dominant(&self) -> Option<SpecType> {
        // Low floor so speculation can fire by the T1 promotion threshold (the
        // function only warms up in bytecode until it promotes).
        const MIN: u32 = 8;
        let t = self.total();
        if t < MIN {
            return None;
        }
        if self.fixnum * 100 >= t * 90 {
            Some(SpecType::Fixnum)
        } else if self.single_float * 100 >= t * 90 {
            Some(SpecType::SingleFloat)
        } else {
            None
        }
    }
}

/// Whether `sym` names an op we would speculate a single numeric type for.
fn is_arith_speculatable(sym: u32) -> bool {
    inlinable_fixnum_op(sym).is_some()
}

thread_local! {
    /// Functions promoted to native and not yet deoptimized since that promotion.
    /// Used to decay the profile exactly once per promotion when a speculation
    /// fails, so a phase change adapts fast without thrashing.
    static PROMOTED_FRESH: std::cell::RefCell<std::collections::HashSet<u32>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// A speculation for `func_ptr` just failed. Zero the currently-dominant type at
/// each of its sites — that is the type we bet on and lost — so the new phase's
/// samples take over quickly (reaching dominance in ~MIN samples instead of having
/// to out-vote a large stale count). Called once per promotion; the other type's
/// samples keep accumulating, so the profile never goes fully cold (which would,
/// with the cold-site fixnum guess, just re-promote the same wrong type).
fn decay_failed_speculation(func_ptr: usize) {
    TYPE_PROFILE.with(|m| {
        for ((fp, _), p) in m.borrow_mut().iter_mut() {
            if *fp == func_ptr {
                if p.fixnum >= p.single_float {
                    p.fixnum = 0;
                } else {
                    p.single_float = 0;
                }
            }
        }
    });
}

/// Record the operand types seen at call site `(func_ptr, bcp)`.
fn record_type_profile(func_ptr: usize, bcp: u32, args: &[BlissVal]) {
    let all_fixnum = args.iter().all(|a| a.is_fixnum());
    // Float contagion: an arithmetic op whose operands are all fixnum-or-float and
    // include at least one float IS a single-float op — the fixnums coerce
    // ((* 2.5 5) = 12.5). Classifying `(* float-x 5)` as single_float (not "other")
    // lets a genuinely float-hot site speculate FloatMul, coercing the fixnum
    // constant, instead of looking polymorphic and staying generic.
    let numeric = args.iter().all(|a| a.is_fixnum() || a.is_single_float());
    let any_float = args.iter().any(|a| a.is_single_float());
    TYPE_PROFILE.with(|m| {
        let mut b = m.borrow_mut();
        let e = b.entry((func_ptr, bcp)).or_default();
        if all_fixnum {
            e.fixnum += 1;
        } else if numeric && any_float {
            e.single_float += 1;
        } else {
            e.other += 1;
        }
    });
}

/// The profile observed at call site `(func_ptr, bcp)`, if any.
fn type_profile_at(func_ptr: usize, bcp: u32) -> Option<TypeProfile> {
    TYPE_PROFILE.with(|m| m.borrow().get(&(func_ptr, bcp)).copied())
}

thread_local! {
    /// Operand-type profiles keyed by (bytecode-function pointer, CallNamed bcp).
    static TYPE_PROFILE: RefCell<HashMap<(usize, u32), TypeProfile>> = RefCell::new(HashMap::new());
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
    NATIVE_DEPTH.with(|d| d.set(d.get() + 1));
    let _depth_guard = NativeDepthGuard;
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
    // NATIVE_ERROR is per-invocation. Native calls now nest directly
    // (native → c2i → run_native → native, bliss-x5y.8), so save any pending
    // error from an outer native frame, run with a fresh slot, then restore the
    // outer's on exit — otherwise a nested call would clobber a first-error-wins
    // error stashed by an enclosing native function.
    let saved_err = NATIVE_ERROR.with(|c| c.borrow_mut().take());
    NATIVE_DEOPT.with(|d| d.set(false));
    // SAFETY: `entry` is installed executable code from emit_native_x86 with the
    // SysV signature `fn(*mut u64) -> u64`, reading its activation from `slots`.
    let f: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(nc.entry) };
    let ret = f(slots);
    NATIVE_ENV.with(|e| e.set(saved));

    let deopt = NATIVE_DEOPT.with(|d| d.replace(false));
    let resume = NATIVE_DEOPT_RESUME.with(|c| c.take());
    let my_err = NATIVE_ERROR.with(|c| c.borrow_mut().take());
    NATIVE_ERROR.with(|c| *c.borrow_mut() = saved_err);
    if let Some(err) = my_err {
        stack.pop_frame();
        return Err(err);
    }
    // Deoptimization (bliss-jtc.27): a speculative guard failed. With
    // state-transfer deopt (bliss-izt.2) we resume T0 at the guard's bytecode
    // position on the SAME frame — no work is redone. (The legacy re-run path is
    // kept as a fallback when no resume point was recorded; purity of every call
    // makes re-running from the top observably equivalent.)
    if deopt {
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
        // Decay the profile once per promotion: the first deopt means the
        // speculation was wrong for the current phase, so zero the type we bet on
        // and let the new phase's samples take over quickly.
        if PROMOTED_FRESH.with(|s| s.borrow_mut().remove(&sym)) {
            if let Some(bf) = registry_get(sym) {
                decay_failed_speculation(Rc::as_ptr(&bf) as usize);
            }
        }
        if t2_log_target().is_some() {
            let nm = registry_get(sym).map(|b| b.name.clone()).unwrap_or_default();
            t2_log_write(format_args!(
                "{nm}: native guard deopt #{n} => re-running interpreted{}",
                if n >= deopt_blacklist_threshold() {
                    " (threshold hit: uninstall + blacklist => T0)"
                } else {
                    ""
                }
            ));
        }
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
        if let Some((bcp, sp_top)) = resume {
            // State-transfer: resume T0 on this frame; `resume_in_t0` owns the
            // frame's lifecycle from here (do NOT pop it first).
            return resume_in_t0(entry, frame, bcp, sp_top, sym, env);
        }
        stack.pop_frame();
        return run(entry, args, BlissVal::from_symbol_index(sym), env);
    }
    stack.pop_frame();
    Ok(BlissVal(ret))
}

/// Resume T0 execution on an EXISTING frame at `(bcp, sp_top)` after a native
/// speculative guard failed (bliss-izt.2). The frame slots already hold the live
/// locals and the untouched operands (the fast paths PEEK-guard-commit, so a
/// failed guard leaves the operand stack coherent), so no value copying is
/// needed. Native compiles the only handler-establishers it accepts —
/// `PushBlock`/`PushTag` — as no-ops (their `return-from`/`go` become jumps), so
/// we replay those over `code[0..bcp]` to rebuild the exact T0 handler stack and
/// `env.block_stack` a top-down interpretation would hold here. Owns the frame
/// lifecycle like [`run`]: `run_loop`'s `Return` pops it on the normal path; on
/// an error we pop any frames left live.
fn resume_in_t0(
    entry: Rc<BytecodeFunction>,
    frame: *mut Frame,
    bcp: u32,
    sp_top: u16,
    sym: u32,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();

    let entry_fn_val = BlissVal::from_symbol_index(sym);
    let fn_obj =
        Some(entry_fn_val).filter(|&v| bliss_rt::function::is_interpreted_function(v));

    let mut handlers: Vec<Handler> = Vec::new();
    for instr in entry.code[..bcp as usize].iter() {
        match instr {
            Instr::PushBlock {
                block_id,
                name_idx,
                resume_bcp,
                sp_restore,
            } => {
                let name = entry.names[*name_idx as usize].clone();
                let token = next_control_token("__RETURN_FROM__");
                env.block_stack.push((name, token.clone()));
                handlers.push(Handler::Block {
                    block_id: *block_id,
                    token,
                    resume_bcp: *resume_bcp,
                    sp_restore: *sp_restore,
                });
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                handlers.push(Handler::Tag {
                    tagbody_id: *tagbody_id,
                    sp_restore: *sp_restore,
                });
            }
            Instr::PopHandler => {
                if let Some(Handler::Block { token, .. }) = handlers.pop() {
                    env.block_stack.retain(|(_, t)| *t != token);
                }
            }
            _ => {}
        }
    }

    let n_locals = entry.n_locals;
    let mut acts: Vec<Activation> = vec![Activation {
        frame,
        n_locals,
        env_frame: None,
        func: entry,
        bcp: bcp as usize,
        sp_top,
        handlers,
        cleanup_conts: Vec::new(),
        fn_obj,
        sym,
    }];
    let result = run_loop(&mut acts, env);
    while !acts.is_empty() {
        stack.pop_frame();
        acts.pop();
    }
    result
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
/// Emit native x86-64 for `bf`. `allow_speculation` enables the speculative
/// fixnum fast paths, whose guards deoptimize via state-transfer: they
/// PEEK-guard-commit and record `(bcp, depth)` so a failure resumes T0 at the
/// exact position (bliss-izt.2). Both the normal call path and OSR pass `true`;
/// only differential-testing oracles that want a pure c2i lowering pass `false`.
/// Returns the code plus, for each OSR-eligible loop header (a backward-`Go`
/// target whose operand stack is empty), the byte offset of an alternate entry
/// stub that sets up the
/// activation registers and jumps straight to that header.
fn emit_native_x86(
    bf: &BytecodeFunction,
    allow_speculation: bool,
) -> Option<(Vec<u8>, Vec<(u32, usize)>)> {
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
    let deopt_state_addr = c2i_deopt_state as extern "C" fn(u64, u64) as usize as u64;

    let mut c = Asm::new();
    // One label per bytecode index, bound as each instruction is emitted, so a
    // Br/Go/ReturnFrom/BrIfFalse targeting bcp `t` is `c.jmp(bcp_labels[t])` and
    // the assembler resolves the displacement in `finish`. A target of exactly
    // `code.len()` (only a malformed body reaches it) has no label and bails via
    // the `?` on `bcp_labels.get`, matching the old out-of-range `offsets.get`.
    let bcp_labels: Vec<Label> = (0..bf.code.len()).map(|_| c.label()).collect();

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
    let reset_r15 = |c: &mut Asm, depth: u16| {
        c.extend_from_slice(&[0x4D, 0x8D, 0xBE]); // lea r15, [r14 + disp32]
        let disp = 8 * (n_locals + depth as i32);
        c.extend_from_slice(&disp.to_le_bytes());
    };

    // Speculative-fixnum eligibility (bliss-jtc.27): only when every call in the
    // function is to a re-execution-safe primitive may we inline fixnum fast
    // paths whose guards deoptimize by re-running the whole function. Purity of
    // every call makes that re-run observably equivalent.
    let deopt_safe = allow_speculation
        && bf.code.iter().all(|i| match i {
            Instr::CallNamed { sym, .. } => is_deopt_safe_primitive(*sym),
            _ => true,
        });

    // OSR-eligible loop headers (bliss-izt.1): the target of a backward `Go`
    // whose tagbody sits at an empty operand stack (sp_restore == 0), so an OSR
    // entry needs only set up the activation registers and jump — the live
    // locals are already in the shared frame slots. Collect the target bcps.
    let mut osr_headers: Vec<u32> = Vec::new();
    for (i, instr) in bf.code.iter().enumerate() {
        if let Instr::Go {
            tagbody_id,
            target_bcp,
        } = instr
        {
            if (*target_bcp as usize) < i
                && tag_sp.get(tagbody_id) == Some(&0)
                && !osr_headers.contains(target_bcp)
            {
                osr_headers.push(*target_bcp);
            }
        }
    }
    // Sites of `jcc rel32` guard branches, each tagged with the bytecode index
    // (`bcp`) of the `CallNamed` it guards (bliss-izt.2). Patched at the end to
    // point at a per-site deopt stub that records `bcp` for state-transfer
    // resume. Every guard in one op shares that op's `bcp`.
    // One deopt-stub label per distinct guarded `bcp`; the guard's `jcc` targets
    // it, and the epilogue binds it to a stub that records `bcp` for
    // state-transfer resume. Get-or-create keeps every guard on the same op
    // sharing one stub, as before.
    // BTreeMap (not HashMap) so the epilogue emits stubs in a deterministic
    // order — the generated bytes are then reproducible across runs.
    let mut deopt_labels: std::collections::BTreeMap<u32, Label> =
        std::collections::BTreeMap::new();
    // Emit a conditional jump to this op's deopt stub (allocating its label on
    // first use).
    let jcc_deopt =
        |c: &mut Asm, labels: &mut std::collections::BTreeMap<u32, Label>, cc: Cc, bcp: u32| {
            let l = *labels.entry(bcp).or_insert_with(|| c.label());
            c.jcc(cc, l);
        };

    // ── Prologue ───────────────────────────────────────────────
    // Shared by the normal entry and every OSR entry stub (bliss-izt.1): both
    // receive the frame-slots pointer in rdi (SysV) and must set up r14/r15 and
    // 16-align rsp identically, so the one Return epilogue balances either.
    let emit_prologue = |c: &mut Asm| {
        c.extend_from_slice(&[0x41, 0x56]); // push r14
        c.extend_from_slice(&[0x41, 0x57]); // push r15
        c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8 (16-align)
        c.extend_from_slice(&[0x49, 0x89, 0xFE]); // mov r14, rdi (frame slots)
        c.extend_from_slice(&[0x4D, 0x8D, 0xBE]); // lea r15, [r14 + 8*n_locals]
        c.extend_from_slice(&(8 * n_locals).to_le_bytes());
    };
    emit_prologue(&mut c);

    // push_op rax: mov [r15], rax ; add r15, 8
    let push_rax = |c: &mut Asm| {
        c.extend_from_slice(&[0x49, 0x89, 0x07]); // mov [r15], rax
        c.extend_from_slice(&[0x49, 0x83, 0xC7, 0x08]); // add r15, 8
    };
    // pop_op into reg: sub r15,8 ; mov reg, [r15]
    let pop_into = |c: &mut Asm, modrm_reg: u8, rex_r: bool| {
        c.extend_from_slice(&[0x49, 0x83, 0xEF, 0x08]); // sub r15, 8
        let rex = 0x49 | if rex_r { 0x04 } else { 0x00 };
        c.push(rex);
        c.push(0x8B);
        c.push(0b00_000_111 | (modrm_reg << 3)); // mod=00, reg, rm=111 (r15)
    };

    for (bcp_idx, instr) in bf.code.iter().enumerate() {
        c.bind(bcp_labels[bcp_idx]);
        let bcp = bcp_idx as u32;
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
                        // PEEK-guard-commit (bliss-izt.2): read x without moving
                        // r15, so a failed guard leaves the operand in its slot for
                        // a T0 resume at this `CallNamed`. Commit stores in place
                        // (one operand in, one result out ⇒ r15 unchanged).
                        c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF8]); // mov rax, [r15-8]
                        c.extend_from_slice(&[0xA8, 0x07]); // test al, 7
                        jcc_deopt(&mut c, &mut deopt_labels, Cc::Ne, bcp); // jnz deopt
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
                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp); // jo deopt
                        c.extend_from_slice(&[0x49, 0x89, 0x47, 0xF8]); // mov [r15-8], rax
                        continue;
                    }
                    if let Some(pred) = inlinable_fixnum_pred(*sym) {
                        // PEEK-guard-commit (bliss-izt.2): x stays in its slot
                        // until the fixnum guard passes.
                        c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF8]); // mov rax, [r15-8]
                        c.extend_from_slice(&[0xA8, 0x07]); // test al, 7
                        jcc_deopt(&mut c, &mut deopt_labels, Cc::Ne, bcp); // jnz deopt
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
                        c.extend_from_slice(&[0x49, 0x89, 0x47, 0xF8]); // mov [r15-8], rax
                        continue;
                    }
                    if let Some(offset) = inlinable_cons_accessor(*sym) {
                        // (car/cdr x): NIL yields NIL; a cons (tag 001) loads the
                        // field at `offset`; anything else deoptimizes to the
                        // interpreter (which signals the type error) — the same
                        // discrimination the c2i path performs, so equally safe.
                        // No allocation or c2i, hence no GC point mid-sequence.
                        // PEEK-guard-commit (bliss-izt.2): x stays in its slot
                        // until the cons/nil guard passes.
                        c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF8]); // mov rax, [r15-8]
                        c.extend_from_slice(&[0x48, 0x3D]); // cmp rax, imm32
                        c.extend_from_slice(&(bliss_rt::value::NIL_BITS as u32).to_le_bytes());
                        c.extend_from_slice(&[0x74, 0x00]); // je rel8 → L_nil (patched)
                        let je_site = c.len() - 1;
                        c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                        c.extend_from_slice(&[0x83, 0xE2, 0x07]); // and edx, 7 (tag)
                        c.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1 (TAG_CONS)
                        jcc_deopt(&mut c, &mut deopt_labels, Cc::Ne, bcp); // jne deopt
                        c.extend_from_slice(&[0x48, 0x83, 0xE0, 0xF8]); // and rax, -8 (ptr)
                        if offset == 0 {
                            c.extend_from_slice(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                        } else {
                            c.extend_from_slice(&[0x48, 0x8B, 0x40, offset as u8]); // mov rax, [rax+8]
                        }
                        // L_nil: both paths converge here (rax = NIL, or the
                        // loaded field). Patch the forward rel8.
                        let l_nil = c.len();
                        c.patch_u8(je_site, (l_nil - (je_site + 1)) as u8);
                        c.extend_from_slice(&[0x49, 0x89, 0x47, 0xF8]); // mov [r15-8], rax
                        continue;
                    }
                    if let Some(pred) = inlinable_total_unary(*sym) {
                        // Total predicate: set flags, then cmov T/NIL. No guard,
                        // no deopt — correct for every operand type.
                        pop_into(&mut c, 0, false); // x -> rax
                        let cc = match pred {
                            TotalUnaryPred::Null => {
                                c.extend_from_slice(&[0x48, 0x3D]); // cmp rax, imm32
                                c.extend_from_slice(&(bliss_rt::value::NIL_BITS as u32).to_le_bytes());
                                0x44 // cmove: x == NIL
                            }
                            TotalUnaryPred::Consp => {
                                c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                                c.extend_from_slice(&[0x83, 0xE2, 0x07]); // and edx, 7
                                c.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1
                                0x44 // cmove: tag == cons
                            }
                            TotalUnaryPred::Atom => {
                                c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                                c.extend_from_slice(&[0x83, 0xE2, 0x07]); // and edx, 7
                                c.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1
                                0x45 // cmovne: tag != cons
                            }
                        };
                        c.extend_from_slice(&[0x48, 0xB8]); // mov rax, NIL
                        c.extend_from_slice(&bliss_rt::value::NIL_BITS.to_le_bytes());
                        c.extend_from_slice(&[0x48, 0xBA]); // mov rdx, T
                        c.extend_from_slice(&bliss_rt::value::T_BITS.to_le_bytes());
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
                    if is_inlinable_eq(*sym) {
                        // EQ: bit-identity → total, no guard, no deopt.
                        pop_into(&mut c, 1, false); // a1 -> rcx
                        pop_into(&mut c, 0, false); // a0 -> rax
                        c.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                        c.extend_from_slice(&[0x48, 0xB8]); // mov rax, NIL
                        c.extend_from_slice(&bliss_rt::value::NIL_BITS.to_le_bytes());
                        c.extend_from_slice(&[0x48, 0xBA]); // mov rdx, T
                        c.extend_from_slice(&bliss_rt::value::T_BITS.to_le_bytes());
                        c.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC2]); // cmove rax, rdx
                        push_rax(&mut c);
                        continue;
                    }
                    if let Some(op) = inlinable_fixnum_op(*sym) {
                        // PEEK-guard-commit (bliss-izt.2): read both operands
                        // WITHOUT moving r15 — top=a1 -> rcx at [r15-8],
                        // next=a0 -> rax at [r15-16]. Every guard below fires
                        // before any operand-stack mutation, so on deopt the two
                        // inputs remain in their slots and r15 still reflects the
                        // pre-call depth, letting T0 resume at this `CallNamed`.
                        c.extend_from_slice(&[0x49, 0x8B, 0x4F, 0xF8]); // mov rcx, [r15-8]
                        c.extend_from_slice(&[0x49, 0x8B, 0x47, 0xF0]); // mov rax, [r15-16]
                        // Commit: two operands consumed, one result pushed ⇒
                        // r15 drops by one slot and the result lands in a0's slot.
                        let commit_bin = |c: &mut Asm| {
                            c.extend_from_slice(&[0x49, 0x89, 0x47, 0xF0]); // mov [r15-16], rax
                            c.extend_from_slice(&[0x49, 0x83, 0xEF, 0x08]); // sub r15, 8
                        };
                        match op {
                            // Single-type speculation: with no profile, T1 guesses
                            // FIXNUM and deopts everything else (float, ratio,
                            // bignum, mixed, overflow). One path, one guard — no
                            // hedging. A site that is actually float-hot deopts,
                            // the profiler observes it, and the optimising tier (T2)
                            // recompiles it committed to float. T1 never emits both.
                            FixnumOp::Add | FixnumOp::Sub | FixnumOp::Mul => {
                                // Fixnum guard: (a0 | a1) low 3 bits must be 000; a
                                // non-fixnum operand deopts to T0.
                                c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                                c.extend_from_slice(&[0x48, 0x09, 0xCA]); // or rdx, rcx
                                c.extend_from_slice(&[0xF6, 0xC2, 0x07]); // test dl, 7
                                jcc_deopt(&mut c, &mut deopt_labels, Cc::Ne, bcp); // jnz deopt
                                match op {
                                    FixnumOp::Add => {
                                        // (a0<<3)+(a1<<3)=(a0+a1)<<3; jo on overflow.
                                        c.extend_from_slice(&[0x48, 0x01, 0xC8]); // add rax, rcx
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp); // jo deopt
                                    }
                                    FixnumOp::Sub => {
                                        c.extend_from_slice(&[0x48, 0x29, 0xC8]); // sub rax, rcx
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp); // jo deopt
                                    }
                                    FixnumOp::Mul => {
                                        // Untag a0, then a0 * (a1<<3) = (a0*a1)<<3.
                                        c.extend_from_slice(&[0x48, 0xC1, 0xF8, 0x03]); // sar rax, 3
                                        c.extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC1]); // imul rax, rcx
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp); // jo deopt
                                    }
                                    _ => unreachable!(),
                                }
                                commit_bin(&mut c);
                            }
                            FixnumOp::Lt | FixnumOp::Gt | FixnumOp::Le | FixnumOp::Ge
                            | FixnumOp::NumEq => {
                                // Fixnum-only (float compares deopt): guard both
                                // operands are fixnums, else deopt.
                                c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                                c.extend_from_slice(&[0x48, 0x09, 0xCA]); // or rdx, rcx
                                c.extend_from_slice(&[0xF6, 0xC2, 0x07]); // test dl, 7
                                jcc_deopt(&mut c, &mut deopt_labels, Cc::Ne, bcp); // jnz deopt
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
                                commit_bin(&mut c);
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
                c.jmp(*bcp_labels.get(*target as usize)?);
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
                c.jmp(*bcp_labels.get(*target_bcp as usize)?);
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
                c.jmp(*bcp_labels.get(resume_bcp as usize)?);
            }
            Instr::BrIfFalse(target) => {
                pop_into(&mut c, 0, false); // rax = value
                c.extend_from_slice(&[0x48, 0x3D]); // cmp rax, imm32
                c.extend_from_slice(&(bliss_rt::value::NIL_BITS as u32).to_le_bytes());
                c.jcc(Cc::E, *bcp_labels.get(*target as usize)?); // je rel32
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

    // Deopt machinery (bliss-izt.2): a shared state-recording tail plus one small
    // stub per distinct guard `bcp`. A failed guard jumps to its bcp's stub,
    // which loads the bcp into edi and falls into the tail; the tail derives the
    // operand depth from r15/r14 and records (bcp, depth) via c2i_deopt_state, so
    // run_native resumes T0 at exactly that `CallNamed` instead of re-running the
    // whole function. rsp is 16-aligned here (as at any CallNamed), so the call
    // is well-formed. The value returned through the epilogue is ignored.
    if !deopt_labels.is_empty() {
        let tail = c.label();
        c.bind(tail);
        // depth = (r15 - r14) / 8 - n_locals  → rsi (SysV arg 2).
        c.extend_from_slice(&[0x4C, 0x89, 0xFE]); // mov rsi, r15
        c.extend_from_slice(&[0x4C, 0x29, 0xF6]); // sub rsi, r14
        c.extend_from_slice(&[0x48, 0xC1, 0xFE, 0x03]); // sar rsi, 3
        c.extend_from_slice(&[0x48, 0x81, 0xEE]); // sub rsi, imm32
        c.extend_from_slice(&n_locals.to_le_bytes());
        c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i_deopt_state)
        c.extend_from_slice(&deopt_state_addr.to_le_bytes());
        c.extend_from_slice(&[0xFF, 0xD0]); // call rax  (edi=bcp set by the stub)
        c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
        c.extend_from_slice(&[0x41, 0x5F]); // pop r15
        c.extend_from_slice(&[0x41, 0x5E]); // pop r14
        c.extend_from_slice(&[0xC3]); // ret

        // One stub per distinct bcp: bind its label (the guards' `jcc`s already
        // point here) then `mov edi, bcp ; jmp tail`. finish() resolves the jmp.
        for (&bcp, &stub) in &deopt_labels {
            c.bind(stub);
            c.push(0xBF); // mov edi, imm32
            c.extend_from_slice(&bcp.to_le_bytes());
            c.jmp(tail);
        }
    }

    // OSR entry stubs (bliss-izt.1): one alternate entry per eligible loop
    // header. Each runs the shared prologue then jumps straight into the body at
    // the header; the header sits at an empty operand stack, so no operand
    // values need transferring (the live locals are already in the frame slots
    // passed in rdi). The function's normal Return epilogue balances the stub's
    // prologue. `finish()` patches bytes in place, so a stub offset captured now
    // stays valid in the returned buffer.
    let mut osr_entries: Vec<(u32, usize)> = Vec::new();
    for header in osr_headers {
        let target = *bcp_labels.get(header as usize)?;
        let stub_off = c.here();
        emit_prologue(&mut c);
        c.jmp(target);
        osr_entries.push((header, stub_off));
    }
    Some((c.finish()?, osr_entries))
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

/// car (offset 0) or cdr (offset 8) — the cons-cell field an inlined accessor
/// loads (bliss-jtc.27). A headerless 16-byte cons {car@0, cdr@8} is tagged 001.
fn inlinable_cons_accessor(sym: u32) -> Option<i8> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("CAR") | Some("FIRST") => Some(0),
        Some("CDR") | Some("REST") => Some(8),
        _ => None,
    }
}

/// Total unary type/nil predicates (bliss-jtc.27): correct for every value, so
/// they inline as a compare + cmov with no guard and no deopt.
#[derive(Clone, Copy)]
enum TotalUnaryPred {
    Null,  // null / not: x is NIL
    Consp, // x is a cons (tag 001)
    Atom,  // x is not a cons
}

fn inlinable_total_unary(sym: u32) -> Option<TotalUnaryPred> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("NULL") | Some("NOT") => Some(TotalUnaryPred::Null),
        Some("CONSP") => Some(TotalUnaryPred::Consp),
        Some("ATOM") => Some(TotalUnaryPred::Atom),
        _ => None,
    }
}

/// True if `sym` is EQ — bit-identity, which in this tagged representation is
/// exactly EQ semantics (immediates compare by value, pointers by identity), so
/// it inlines as a compare + cmov with no guard and no deopt (bliss-jtc.27).
/// EQL is deliberately excluded: two distinct bignums with equal value are EQL
/// but not bit-equal.
fn is_inlinable_eq(sym: u32) -> bool {
    bliss_rt::symbols::symbol_name(sym).as_deref() == Some("EQ")
}

#[cfg(not(target_arch = "x86_64"))]
fn emit_native_x86(
    _bf: &BytecodeFunction,
    _allow_speculation: bool,
) -> Option<(Vec<u8>, Vec<(u32, usize)>)> {
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
    // Non-leaf functions promote too (bliss-x5y.4): a T1 function's CallNamed to
    // another user function crosses c2i into `apply_function`, which runs the
    // callee in the interpreter (NOT via `run_native`), so the native call path
    // is not self-recursive. Any native re-entry is still bounded by the
    // NATIVE_DEPTH cap in the dispatcher. (The old leaf-only guard here kept
    // essentially all real, call-heavy library code out of T1.) Speculative
    // fixnum codegen stays gated on `is_deopt_safe_primitive`, so a non-leaf
    // function that calls impure helpers emits no deopt-able ops.
    let (code, _osr) = emit_native_x86(&bf, true)?;
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
        code_len: code.len(),
        is_t2: false,
        num_slots,
        compiled_entry: 0, // T1 baseline has no distinct register entry yet
        code_info,
    });
    NATIVE_REGISTRY.with(|r| r.borrow_mut().insert(sym, Rc::clone(&nc)));
    Some(nc)
}

/// Try to compile `sym` to T2 optimising native code (profile-guided single-type
/// speculation). Gated behind `BLISS_T2=1` — off by default. Returns `None`
/// (fall back to T1) when disabled, when no arithmetic site has a consistent
/// profile yet, or when the function's shape is beyond the first-cut framed
/// emitter (branches, unsupported ops). Installs like T1 and shares the
/// run_native ABI, so dispatch and deopt are identical.
/// Where T2 trace output goes, decided once from `BLISS_T2_LOG`:
/// `-`/`1`/`stderr` → stderr; any other non-empty value → that file (appended);
/// unset → no logging. This is the observability channel: it explains, per
/// function, why T2 was or wasn't reached and what specialization it chose.
enum T2LogTarget {
    Stderr,
    File(std::sync::Mutex<std::fs::File>),
}

fn t2_log_target() -> Option<&'static T2LogTarget> {
    use std::sync::OnceLock;
    static T: OnceLock<Option<T2LogTarget>> = OnceLock::new();
    T.get_or_init(|| match std::env::var("BLISS_T2_LOG") {
        Ok(v) if v == "-" || v == "1" || v == "stderr" => Some(T2LogTarget::Stderr),
        Ok(v) if !v.is_empty() => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&v)
            .ok()
            .map(|f| T2LogTarget::File(std::sync::Mutex::new(f))),
        _ => None,
    })
    .as_ref()
}

fn t2_log_write(msg: std::fmt::Arguments) {
    use std::io::Write;
    match t2_log_target() {
        Some(T2LogTarget::Stderr) => {
            let _ = writeln!(std::io::stderr(), "[T2] {msg}");
        }
        Some(T2LogTarget::File(m)) => {
            if let Ok(mut f) = m.lock() {
                let _ = writeln!(f, "[T2] {msg}");
            }
        }
        None => {}
    }
}

macro_rules! t2_log {
    ($($a:tt)*) => { t2_log_write(format_args!($($a)*)) };
}

/// Log every profiled call site of `func_ptr` and the type it resolves to — the
/// "why" behind a speculation (or a decline).
fn t2_log_profile(name: &str, func_ptr: usize) {
    if t2_log_target().is_none() {
        return;
    }
    TYPE_PROFILE.with(|m| {
        for ((fp, bcp), p) in m.borrow().iter() {
            if *fp == func_ptr {
                t2_log!(
                    "{name}: site bcp={bcp} profile fix={} float={} other={} => {:?}",
                    p.fixnum,
                    p.single_float,
                    p.other,
                    p.dominant()
                );
            }
        }
    });
}

fn try_promote_to_t2(sym: u32) -> Option<Rc<NativeCode>> {
    if std::env::var("BLISS_T2").ok().as_deref() != Some("1") {
        return None;
    }
    let bf = registry_get(sym)?;
    let func_ptr = Rc::as_ptr(&bf) as usize;
    let name = bf.name.clone();
    t2_log!("{name}: considering for T2 (arity {})", bf.arity);
    t2_log_profile(&name, func_ptr);

    // The profile closure the speculative-lowering pass consumes: this function's
    // observed operand type at each call-site bcp, mapped to the compiler's type.
    // A site with a clear dominant type speculates that type. A site with samples
    // but no dominant type is genuinely polymorphic → left generic. A COLD site
    // (no samples — e.g. a branch not taken during the brief profiling window)
    // optimistically guesses fixnum: the guard makes it safe (a wrong guess just
    // deopts), and it stops one cold branch from declining the whole function.
    let profile = |bcp: u32| -> Option<bliss_compiler::t2::speculate::SpecType> {
        use bliss_compiler::t2::speculate::SpecType as Bc;
        match type_profile_at(func_ptr, bcp) {
            Some(p) => match p.dominant() {
                Some(SpecType::Fixnum) => Some(Bc::Fixnum),
                Some(SpecType::SingleFloat) => Some(Bc::SingleFloat),
                None => None, // has samples but polymorphic
            },
            None => Some(Bc::Fixnum), // cold: optimistic, guarded fixnum guess
        }
    };

    let mut f = match bliss_compiler::t2::build::build_from_bytecode(&bf) {
        Ok(f) => f,
        Err(e) => {
            t2_log!("{name}: build_from_bytecode failed: {e:?} => stay T1");
            return None;
        }
    };
    let speculated = bliss_compiler::t2::speculate::speculate(&mut f, &profile);
    if speculated == 0 {
        t2_log!("{name}: 0 speculatable sites (cold or polymorphic profile) => stay T1");
        return None; // nothing to specialise → T1 is as good; skip T2
    }
    t2_log!("{name}: speculated {speculated} site(s)");

    // Route the speculated IR through the mid-end: constant folding + strength
    // reduction (P4f), global value numbering (P4a), then deopt-aware DCE (P4c).
    // Each pass preserves well-formedness (spec §4.10 R4.60); re-verify before
    // emitting, and decline T2 (fall back to T1) if anything went wrong.
    {
        use bliss_compiler::t2::pass::PassManager;
        let mut pm = PassManager::new();
        pm.add(Box::new(bliss_compiler::t2::opt_fold::ConstFold));
        pm.add(Box::new(bliss_compiler::t2::opt_gvn::Gvn));
        pm.add(Box::new(bliss_compiler::t2::opt_dce::Dce));
        pm.run(&mut f);
    }
    if let Err(e) = bliss_compiler::t2::verify::verify(&f) {
        t2_log!("{name}: post-mid-end verify failed: {e:?} => stay T1");
        return None;
    }

    let deopt_addr = c2i_deopt as extern "C" fn() as usize as u64;
    let framed = match bliss_compiler::t2::emit::emit_framed(&f, deopt_addr) {
        Ok(fc) => fc,
        Err(e) => {
            t2_log!("{name}: emit_framed failed: {e:?} (shape beyond emitter) => stay T1");
            return None;
        }
    };
    let code = framed.code;
    t2_log!(
        "{name}: T2 INSTALLED — {} bytes, compiled_entry=+{}",
        code.len(),
        framed.compiled_entry
    );

    let num_slots = bf.num_slots();
    let code_info = install_stack_map(num_slots)?;
    let buf = bliss_rt::jit::JitBuffer::new(&code)?;
    let entry = buf.leak();
    maybe_write_perf_map(entry as usize, code.len(), sym);
    let nc = Rc::new(NativeCode {
        entry,
        code_len: code.len(),
        is_t2: true,
        num_slots,
        compiled_entry: framed.compiled_entry,
        code_info,
    });
    NATIVE_REGISTRY.with(|r| r.borrow_mut().insert(sym, Rc::clone(&nc)));
    Some(nc)
}

/// Promote `sym` to the best available native tier: T2 (profile-guided) if
/// enabled and applicable, else the T1 baseline.
fn try_promote(sym: u32) -> Option<Rc<NativeCode>> {
    let nc = try_promote_to_t2(sym).or_else(|| try_promote_to_t1(sym))?;
    // Mark this promotion fresh: the first deopt after it decays the profile once.
    PROMOTED_FRESH.with(|s| s.borrow_mut().insert(sym));
    Some(nc)
}

/// Installed OSR code for a function (bliss-izt.1): a non-speculating native
/// compilation plus, per OSR-eligible loop header bcp, the byte offset of the
/// entry stub that jumps into that header. Shares the frame/GC layout with the
/// normal native tier (same `num_slots`/stack map).
struct OsrCode {
    entry: *const u8,
    num_slots: u16,
    code_info: &'static CodeInfo,
    /// header bcp → entry-stub byte offset from `entry`.
    entries: std::collections::HashMap<u32, usize>,
}

thread_local! {
    /// Compiled OSR code per function symbol (bliss-izt.1).
    static OSR_REGISTRY: RefCell<HashMap<u32, Option<Rc<OsrCode>>>> =
        RefCell::new(HashMap::new());
}

/// OSR promotion threshold: number of loop back-edges taken in a single running
/// activation before its hot loop is compiled and entered natively. Env
/// override `BLISS_OSR_THRESHOLD` (tests use a small value).
fn osr_threshold() -> u32 {
    use std::sync::OnceLock;
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("BLISS_OSR_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100_000)
    })
}

/// Compile (once, memoized) a non-speculating native version of `sym` with OSR
/// entry stubs. Returns `None` if it can't be compiled (cached so we don't retry
/// every back-edge).
fn compile_osr(sym: u32) -> Option<Rc<OsrCode>> {
    if let Some(cached) = OSR_REGISTRY.with(|r| r.borrow().get(&sym).cloned()) {
        return cached;
    }
    let result = (|| {
        let bf = registry_get(sym)?;
        // allow_speculation = true (bliss-izt.2): OSR now inlines fixnum
        // arithmetic. A mid-loop guard failure no longer needs to re-run from the
        // top — state-transfer deopt resumes T0 at the guard on the LIVE frame
        // (the loop's locals and operands are already in the shared slots), so
        // the loop runs at full native speed until (if ever) a value leaves the
        // fixnum domain. Speculation only actually engages for `deopt_safe`
        // functions (every call a pure primitive); others fall back to c2i.
        let (code, osr) = emit_native_x86(&bf, true)?;
        if osr.is_empty() {
            return None;
        }
        let num_slots = bf.num_slots();
        let code_info = install_stack_map(num_slots)?;
        let buf = bliss_rt::jit::JitBuffer::new(&code)?;
        let entry = buf.leak();
        maybe_write_perf_map(entry as usize, code.len(), sym);
        Some(Rc::new(OsrCode {
            entry,
            num_slots,
            code_info,
            entries: osr.into_iter().collect(),
        }))
    })();
    OSR_REGISTRY.with(|r| r.borrow_mut().insert(sym, result.clone()));
    result
}

/// The result of entering OSR native code (bliss-izt.1/izt.2): either the loop
/// ran to the function's `Return` (`Finished`), or a speculative guard failed
/// mid-loop and the running activation must continue in T0 from `bcp` with the
/// operand stack at `sp_top` — the live frame already holds the correct locals
/// and (peek-preserved) operands, so no value transfer is needed (`Deopt`).
enum OsrOutcome {
    Finished(BlissVal),
    Deopt { bcp: u32, sp_top: u16 },
}

/// Enter OSR native code at `stub_off` to finish the current activation, reading
/// and writing the LIVE frame slots (`frame`) — no new frame, no arg rebinding
/// (bliss-izt.1). With speculation (bliss-izt.2) the native code may hit a fixnum
/// guard failure; it records the resume position via `c2i_deopt_state` and
/// returns, which this surfaces as `OsrOutcome::Deopt` so the caller resumes T0
/// on the same activation. A c2i error surfaces via NATIVE_ERROR as usual.
fn run_native_osr(
    osr: &OsrCode,
    stub_off: usize,
    frame: *mut Frame,
    env: &mut Env,
) -> Result<OsrOutcome, BlissError> {
    NATIVE_DEPTH.with(|d| d.set(d.get() + 1));
    let _depth_guard = NativeDepthGuard;
    // The OSR entry reads its activation from the frame slots pointer (rdi), the
    // same slots T0 was using; locals + the (empty) operand stack are already in
    // place.
    let slots = unsafe { frame.add(1) as *mut u64 };
    let saved = NATIVE_ENV.with(|e| e.replace(env as *mut Env));
    let saved_err = NATIVE_ERROR.with(|c| c.borrow_mut().take());
    NATIVE_DEOPT.with(|d| d.set(false));
    let entry_addr = osr.entry as usize + stub_off;
    // SAFETY: `entry_addr` is inside the installed OSR buffer at a stub whose
    // contract is `fn(*mut u64) -> u64` (prologue + jump to the loop header).
    let f: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(entry_addr) };
    let ret = f(slots);
    NATIVE_ENV.with(|e| e.set(saved));
    let deopt = NATIVE_DEOPT.with(|d| d.replace(false));
    let resume = NATIVE_DEOPT_RESUME.with(|c| c.take());
    let my_err = NATIVE_ERROR.with(|c| c.borrow_mut().take());
    NATIVE_ERROR.with(|c| *c.borrow_mut() = saved_err);
    let _ = osr.num_slots;
    let _ = osr.code_info;
    if let Some(err) = my_err {
        return Err(err);
    }
    if deopt {
        if let Some((bcp, sp_top)) = resume {
            return Ok(OsrOutcome::Deopt { bcp, sp_top });
        }
        // No resume point recorded — should not happen for OSR (speculation
        // always records one), but treat it as a benign no-progress signal by
        // resuming at the loop's back-edge target is not possible here, so fall
        // through to Finished with the returned (dummy) value would be wrong.
        // Instead surface an internal error to avoid silently returning garbage.
        return Err(BlissError::Internal("OSR deopt without resume point".into()));
    }
    Ok(OsrOutcome::Finished(BlissVal(ret)))
}

/// If the back-edge to `target_bcp` is a hot loop at an empty operand stack,
/// compile and enter OSR native code to finish this activation (bliss-izt.1).
/// `None` means not (yet) eligible — the caller keeps interpreting.
fn maybe_osr(
    act: &Activation,
    target_bcp: u32,
    env: &mut Env,
) -> Option<Result<OsrOutcome, BlissError>> {
    // Backward edge into an empty-operand-stack header, in a named function.
    if (target_bcp as usize) >= act.bcp || act.sp_top != 0 || act.sym == u32::MAX {
        return None;
    }
    let fn_obj = act.fn_obj?;
    if bliss_rt::function::back_edge_count(fn_obj) < osr_threshold() {
        return None;
    }
    let osr = compile_osr(act.sym)?;
    let stub_off = *osr.entries.get(&target_bcp)?;
    Some(run_native_osr(&osr, stub_off, act.frame, env))
}

/// Drop a completed activation's non-local-exit handlers from the shared env
/// stacks (bliss-izt.1). Mirrors the `Return` handler's cleanup — used when an
/// OSR native finish completes the activation, so no stale block/catch tokens
/// referencing the popped frame remain.
fn release_activation_handlers(act: &mut Activation, env: &mut Env) {
    for h in act.handlers.drain(..) {
        match h {
            Handler::Catch { token, .. } => env.catch_stack.retain(|(_, t)| *t != token),
            Handler::Block { token, .. } => env.block_stack.retain(|(_, t)| *t != token),
            Handler::HandlerCase { cluster_base, .. }
            | Handler::HandlerBind { cluster_base } => env.handlers.truncate(cluster_base),
            Handler::RestartCase { restart_base, .. } => env.restarts.truncate(restart_base),
            _ => {}
        }
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

    // A top-level `(eval-when (situations) body...)` whose situations fire now:
    // process each body form as a top-level form (bliss-x5y.6). ASDF/UIOP wrap
    // nearly every definition in eval-when, and without this the whole form is
    // thunk-compiled, bails, and its inner `defun`s are only tree-walker-defined
    // — so they never reach compile_function/T1 (the reason uiop:ensure-package
    // stayed tier 0). `eval_when_should_run` is the SAME predicate the tree-
    // walker uses, so the situation semantics are unchanged.
    if form.is_cons() {
        let (op, cdr) = cp(form);
        if op.is_symbol() && sym_name(op) == "EVAL-WHEN" && cdr.is_cons() {
            let (situations, body) = cp(cdr);
            if super::eval_when_should_run(situations, env) {
                let mut last = NIL;
                for f in list_to_vec(body) {
                    last = eval_toplevel(f, env)?;
                }
                return Ok(last);
            }
            return Ok(NIL);
        }
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
