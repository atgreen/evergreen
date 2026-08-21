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
use std::sync::Once;

use bliss_compiler::macroexpand::{self as compiler_macroexpand, Environment as MacroexpandEnv};
use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::gc::HostRoot;
use bliss_rt::stack::StackMapEntry;
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_rt::{CodeInfo, Frame};

use super::{
    DynBind, Env, EnvFrame, HandlerCluster, HandlerEntry, HandlerImpl, RestartEntry,
    RestartFunction, apply_function, arena_cons, arena_str, bliss_error_to_condition,
    condition_matches_handler, cp, eval_form, handler_case_token, list_to_vec,
    next_control_token, resolve_sym, restart_invoked_name, run_handler_bind_handlers,
    store_control_value, sym_name, symbol_bare_name, tag_key, take_control_value, val_as_str,
    vec_to_list,
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
    BytecodeFunction, ClauseInfo, DeclaredType, HandlerBindInfo, HandlerCaseInfo, Instr,
    RestartCaseInfo, RestartClauseInfo, VarLoc,
};

// ── Per-thread registry of compiled functions ─────────────────────

thread_local! {
    /// Bytecode functions keyed by symbol index. A `CallNamed` checks this
    /// first; a hit runs as a native frame on the `BlissStack`, a miss falls
    /// back to `apply_function` (builtins, generics, tree-walker functions).
    static REGISTRY: RefCell<HashMap<u32, Rc<BytecodeFunction>>> = RefCell::new(HashMap::new());
    /// Definition generations reject background compilations that finish after
    /// a DEFUN has replaced their bytecode snapshot.
    static REGISTRY_GENERATION: RefCell<HashMap<u32, u64>> = RefCell::new(HashMap::new());
    /// Bytecode bodies currently executing but not necessarily present in the
    /// global registry (top-level thunks, nested closures, and macro expanders).
    static ACTIVE_BYTECODE_FUNCTIONS: RefCell<Vec<usize>> = RefCell::new(Vec::new());
}

unsafe fn trace_bytecode_function(
    function: *mut BytecodeFunction,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    // SAFETY: the scanner runs during a stop-the-world GC. BytecodeFunction is
    // held behind Rc during execution/registration, but its BlissVal slots must
    // still be rewritten when the moving collector forwards their referents.
    let function = unsafe { &mut *function };
    for constant in &mut function.constants {
        visit(constant as *mut BlissVal);
    }
    visit(&mut function.params_form as *mut BlissVal);
    for handler_bind in &mut function.handler_binds {
        for (_, form) in &mut handler_bind.bindings {
            visit(form as *mut BlissVal);
        }
    }
    for restart_case in &mut function.restart_cases {
        for restart in &mut restart_case.restarts {
            unsafe {
                trace_bytecode_function(&mut *restart.function, visit);
            }
        }
    }
    for nested in &mut function.nested_functions {
        unsafe {
            trace_bytecode_function(&mut **nested, visit);
        }
    }
}

fn scan_bytecode_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    REGISTRY.with(|registry| {
        for function in registry.borrow().values() {
            unsafe {
                trace_bytecode_function(Rc::as_ptr(function) as *mut BytecodeFunction, visit);
            }
        }
    });
    ACTIVE_BYTECODE_FUNCTIONS.with(|active| {
        for &function in active.borrow().iter() {
            unsafe {
                trace_bytecode_function(function as *mut BytecodeFunction, visit);
            }
        }
    });
}

fn install_bytecode_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_bytecode_roots));
}

thread_local! {
    /// Constant tables of Lowerers currently compiling. During lowering the
    /// function is not yet registered/active, so scan_bytecode_roots does not see
    /// it; without this, a heap literal (float/bignum/string/quoted list) already
    /// placed in `constants` would be freed or left stale by a relocating GC fired
    /// by a later lowering allocation — the compiled code would then read a stale
    /// constant (observed as `NIL is not of type number`) (bliss-6b2 #2).
    static ACTIVE_LOWERER_CONSTS: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn scan_lowerer_consts(visit: &mut dyn FnMut(*mut BlissVal)) {
    ACTIVE_LOWERER_CONSTS.with(|r| {
        for &p in r.borrow().iter() {
            // SAFETY: registered only while the owning Lowerer local is live and
            // immobile (the guard is dropped before the Lowerer).
            unsafe {
                for e in (*(p as *mut Vec<BlissVal>)).iter_mut() {
                    visit(e as *mut BlissVal);
                }
            }
        }
    });
}

fn install_lowerer_const_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_lowerer_consts));
}

/// Roots a Lowerer's `constants` table for the extent of compilation.
struct LowererConstGuard(usize);

impl LowererConstGuard {
    fn new(constants: &mut Vec<BlissVal>) -> Self {
        install_lowerer_const_scanner();
        let p = constants as *mut Vec<BlissVal> as usize;
        ACTIVE_LOWERER_CONSTS.with(|r| r.borrow_mut().push(p));
        LowererConstGuard(p)
    }
}

impl Drop for LowererConstGuard {
    fn drop(&mut self) {
        ACTIVE_LOWERER_CONSTS.with(|r| {
            let mut v = r.borrow_mut();
            if let Some(i) = v.iter().rposition(|&x| x == self.0) {
                v.remove(i);
            }
        });
    }
}

struct ActiveBytecodeRoot {
    ptr: usize,
}

impl ActiveBytecodeRoot {
    fn new(function: &Rc<BytecodeFunction>) -> Self {
        install_bytecode_root_scanner();
        let ptr = Rc::as_ptr(function) as usize;
        ACTIVE_BYTECODE_FUNCTIONS.with(|active| active.borrow_mut().push(ptr));
        Self { ptr }
    }
}

impl Drop for ActiveBytecodeRoot {
    fn drop(&mut self) {
        ACTIVE_BYTECODE_FUNCTIONS.with(|active| {
            let mut active = active.borrow_mut();
            let index = active
                .iter()
                .rposition(|&ptr| ptr == self.ptr)
                .expect("active bytecode root missing");
            active.remove(index);
        });
    }
}

/// Build a proper list `(items...)` in the arena, for synthesising macro-style
/// expansions during lowering (bliss-jtc.28, e.g. DOTIMES → block/tagbody).
fn form_list(items: &[BlissVal]) -> BlissVal {
    let items = HostRoot::new(items.to_vec());
    let mut acc = NIL;
    let _acc_root = bliss_rt::gc::StackRoot::new(&mut acc);
    for index in (0..items.len()).rev() {
        let mut x = items[index];
        let _x_root = bliss_rt::gc::StackRoot::new(&mut x);
        acc = arena_cons(x, acc);
    }
    acc
}

fn registry_get(sym: u32) -> Option<Rc<BytecodeFunction>> {
    install_bytecode_root_scanner();
    REGISTRY.with(|r| r.borrow().get(&sym).cloned())
}

/// Whether `sym` currently names a registered bytecode function (e.g. a
/// source-free `(defun (setf place) …)` writer installed from a `.bfasl`).
pub(super) fn is_registered(sym: u32) -> bool {
    REGISTRY.with(|r| r.borrow().contains_key(&sym))
}

fn registry_put(sym: u32, f: Rc<BytecodeFunction>) {
    install_bytecode_root_scanner();
    REGISTRY_GENERATION.with(|g| {
        let mut generations = g.borrow_mut();
        let next = generations.get(&sym).copied().unwrap_or(0).wrapping_add(1);
        generations.insert(sym, next);
    });
    let old = REGISTRY.with(|r| r.borrow_mut().insert(sym, f));
    if let Some(old) = old {
        clear_bytecode_profiles(Rc::as_ptr(&old) as usize);
    }
    // A new definition starts a new tiering lifetime.  In particular, a T2
    // decline for the old body must not suppress compilation of the new one,
    // and no native entry compiled from the old bytecode may remain callable.
    NATIVE_REGISTRY.with(|r| r.borrow_mut().remove(&sym));
    T2_DECLINED.with(|s| s.borrow_mut().remove(&sym));
    T2_QUEUED.with(|s| s.borrow_mut().remove(&sym));
    INVOKE_COUNTS.with(|m| m.borrow_mut().remove(&sym));
    DEOPT_COUNTS.with(|m| m.borrow_mut().remove(&sym));
    DEOPT_BLACKLIST.with(|s| s.borrow_mut().remove(&sym));
    PROMOTED_FRESH.with(|s| s.borrow_mut().remove(&sym));
    LAST_FAILED_SPECULATION.with(|m| m.borrow_mut().remove(&sym));
}

fn registry_remove(sym: u32) {
    REGISTRY_GENERATION.with(|g| {
        let mut generations = g.borrow_mut();
        let next = generations.get(&sym).copied().unwrap_or(0).wrapping_add(1);
        generations.insert(sym, next);
    });
    let old = REGISTRY.with(|r| r.borrow_mut().remove(&sym));
    if let Some(old) = old {
        clear_bytecode_profiles(Rc::as_ptr(&old) as usize);
    }
    NATIVE_REGISTRY.with(|r| r.borrow_mut().remove(&sym));
    T2_DECLINED.with(|s| s.borrow_mut().remove(&sym));
    T2_QUEUED.with(|s| s.borrow_mut().remove(&sym));
    INVOKE_COUNTS.with(|m| m.borrow_mut().remove(&sym));
    DEOPT_COUNTS.with(|m| m.borrow_mut().remove(&sym));
    DEOPT_BLACKLIST.with(|s| s.borrow_mut().remove(&sym));
    PROMOTED_FRESH.with(|s| s.borrow_mut().remove(&sym));
    LAST_FAILED_SPECULATION.with(|m| m.borrow_mut().remove(&sym));
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
/// Whether a compiled callee accepts `n` arguments: at least its required count,
/// and at most `max_args` unless the lambda list is unbounded (`&rest`/`&key`).
/// A too-few/too-many count declines to the tree-walker, which raises the proper
/// PROGRAM-ERROR (x5y.7).
fn arity_accepts(callee: &BytecodeFunction, n: usize) -> bool {
    n >= callee.min_args as usize && callee.max_args.is_none_or(|m| n <= m as usize)
}

pub fn call_registered(
    sym: u32,
    args: &[BlissVal],
    fn_val: BlissVal,
    env: &mut Env,
) -> Option<Result<BlissVal, BlissError>> {
    let callee = registry_get(sym)?;
    if !arity_accepts(&callee, args.len()) {
        return None; // arg count outside the lambda list's range: tree-walker binds it
    }
    let fn_obj = bliss_rt::symbols::symbol_function(sym)
        .filter(|&c| bliss_rt::function::is_interpreted_function(c));
    // The tree-walker already bumped named function objects in `callable_body`.
    // Anonymous/gensym bytecode functions have no FnMeta, so maintain their
    // fallback counter here on every call (including calls after T1 installs).
    let count = dispatch_invoke_count(sym, fn_obj);
    if std::env::var_os("BLISS_T1_TRACE").is_some() {
        eprintln!("[T1] {}: dispatch at invocation {count}", sym_label(sym));
    }
    let native = native_for_dispatch(sym, fn_obj, count);
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
    let declared: Vec<String> = bf
        .param_layout
        .iter()
        .zip(&bf.param_types)
        .filter_map(|((parameter, _), ty)| {
            let name = match ty {
                DeclaredType::Any => return None,
                DeclaredType::Fixnum => "FIXNUM",
                DeclaredType::SingleFloat => "SINGLE-FLOAT",
            };
            Some(format!("{parameter}: {name}"))
        })
        .collect();
    if !declared.is_empty() {
        let _ = writeln!(
            out,
            "; checked parameter declarations: {}",
            declared.join(", ")
        );
    }

    match native {
        // Promoted to native: decode the installed machine code (spec: "otherwise
        // machine instructions"). The code is R+X-mapped, so reading it is safe.
        Some(nc) => {
            let _ = writeln!(out, "; {} bytes of x86-64 at {:p}", nc.code_len, nc.entry);
            let bytes = unsafe { std::slice::from_raw_parts(nc.entry, nc.code_len) };
            let mut dec = iced_x86::Decoder::with_ip(
                64,
                bytes,
                nc.entry as u64,
                iced_x86::DecoderOptions::NONE,
            );
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
    /// Most recent lowering failure, used by the opt-in named compile trace.
    static LAST_BAIL_REASON: RefCell<Option<String>> = const { RefCell::new(None) };
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
        let reason = reason();
        LAST_BAIL_REASON.with(|last| *last.borrow_mut() = Some(reason.clone()));
        BAIL_LOG.with(|m| *m.borrow_mut().entry(reason).or_insert(0) += 1);
    }
    Bail
}

fn reset_last_bail_reason() {
    if bail_trace_on() {
        LAST_BAIL_REASON.with(|last| *last.borrow_mut() = None);
    }
}

fn last_bail_reason() -> Option<String> {
    LAST_BAIL_REASON.with(|last| last.borrow().clone())
}

/// Snapshot of the bail histogram, most frequent first (bliss-x5y.1).
pub fn bail_report() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> =
        BAIL_LOG.with(|m| m.borrow().iter().map(|(k, &n)| (k.clone(), n)).collect());
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Primitives whose call semantics the tree-walker owns; a `CallNamed` to one
/// of these delegates to `apply_function`. The allowlist keeps slice 1 *safe*:
/// the compiler only emits a call when it is certain the callee is a real
/// function (a user `defun`, or one of these), never a macro or a special
/// operator masquerading as a call.
#[allow(dead_code)] // retained as documentation of the original slice-1 surface
const PRIMITIVE_ALLOWLIST: &[&str] = &[
    "+",
    "-",
    "*",
    "/",
    "<",
    ">",
    "<=",
    ">=",
    "=",
    "/=",
    "1+",
    "1-",
    "CAR",
    "CDR",
    "CONS",
    "LIST",
    "NULL",
    "NOT",
    "EQ",
    "EQL",
    "EQUAL",
    "ZEROP",
    "PLUSP",
    "MINUSP",
    "ABS",
    "MIN",
    "MAX",
    "MOD",
    "REM",
    "CONSP",
    "ATOM",
    "LISTP",
    "EVENP",
    "ODDP",
    "GCD",
    "EXPT",
    "FLOOR",
    "CEILING",
    "TRUNCATE",
    "VALUES-LIST",
    "IDENTITY",
    "FIRST",
    "REST",
    "SECOND",
    "THIRD",
    "LENGTH",
    "APPEND",
    "REVERSE",
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
    // Hash tables (bliss-x5y.2): ordinary functions dispatched through
    // apply_function's synthesize path. GETHASH reads; the (setf gethash) store
    // is lowered directly to BLISS::PUT-GETHASH by lower_setf. MAKE-HASH-TABLE's
    // &key args pass positionally and are parsed by the callee. (Only the ops that
    // are actually implemented in the interpreter are listed.)
    "GETHASH",
    "MAKE-HASH-TABLE",
    "REMHASH",
    "CLRHASH",
    "MAPHASH",
    "HASH-TABLE-COUNT",
    "HASH-TABLE-P",
    "HASH-TABLE-KEYS",
    "HASH-TABLE-VALUES",
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
    /// Lexically-visible *capturing* local functions (flet/labels whose bodies
    /// close over enclosing lexicals). Each is a closure value stored in a boxed
    /// binding of the same name; a call to it lowers to `(funcall name args…)`.
    closure_fns: std::collections::HashSet<String>,
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
    /// Names of blocks established in an *enclosing* function that this body (a
    /// capturing closure) may still `return-from` non-locally, resolved at run
    /// time through the shared block-token stack.
    enclosing_blocks: std::collections::HashSet<String>,
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
    /// Nested noncapturing lambda bodies.
    nested_functions: Vec<Box<BytecodeFunction>>,
    /// Portable BFASL compilation may lower quasiquote to explicit allocation;
    /// the ordinary opportunistic compiler keeps the tree-walker as its oracle.
    portable: bool,
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
            closure_fns: std::collections::HashSet::new(),
            macro_env: None,
            has_env: false,
            next_local: 0,
            n_locals: 0,
            cur_stack: 0,
            max_stack: 0,
            next_id: 0,
            block_scope: Vec::new(),
            enclosing_blocks: std::collections::HashSet::new(),
            tag_scope: Vec::new(),
            pending_gos: Vec::new(),
            handler_cases: Vec::new(),
            handler_binds: Vec::new(),
            names: Vec::new(),
            restart_cases: Vec::new(),
            nested_functions: Vec::new(),
            portable: false,
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
        let result = self.lower_expr_inner(form);
        if result.is_err() && bail_trace_on() && last_bail_reason().is_none() {
            let label = if form.is_cons() {
                let op = cp(form).0;
                if op.is_symbol() {
                    format!("form:{}", sym_name(op))
                } else {
                    "form:<computed-operator>".to_string()
                }
            } else if form.is_symbol() {
                format!("symbol:{}", sym_name(form))
            } else {
                "form:<object>".to_string()
            };
            let _ = record_bail(|| label);
        }
        result
    }

    fn lower_expr_inner(&mut self, form: BlissVal) -> LowerResult<()> {
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
            if self
                .env
                .symbol_macros
                .borrow()
                .contains_key(&form.as_symbol_index())
            {
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
                "EVAL-WHEN" => self.lower_eval_when(rest),
                "LET" => self.lower_let(rest, false),
                "LET*" => self.lower_let(rest, true),
                "SETQ" => self.lower_setq(rest),
                "SETF" => self.lower_setf(rest),
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
                "BLISS::QUASIQUOTE" => {
                    if !self.portable {
                        return Err(Bail);
                    }
                    if !rest.is_cons() {
                        return Err(Bail);
                    }
                    self.lower_quasiquote_template(cp(rest).0, 1)
                }
                "VALUES" => self.lower_values(rest),
                "DESTRUCTURING-BIND" => self.lower_destructuring_bind(rest),
                "IGNORE-ERRORS" => self.lower_ignore_errors(rest),
                "ASSERT" => self.lower_assert(rest),
                "MULTIPLE-VALUE-CALL" => self.lower_multiple_value_call(rest),
                "MULTIPLE-VALUE-BIND" => self.lower_mvb(rest),
                "MULTIPLE-VALUE-LIST" => self.lower_mvlist(rest),
                "LAMBDA" => self.lower_lambda(op, rest),
                "FUNCTION" => self.lower_function(rest),
                "FLET" => self.lower_flet(rest, false),
                "LABELS" => self.lower_flet(rest, true),
                "MACROLET" | "SYMBOL-MACROLET" => self.lower_macrolet(form),
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

    /// `(eval-when (situations) body...)` in a non-top-level position (top-level
    /// ones are handled by `eval_toplevel`). Per CLHS 3.2.3.1 this reduces to
    /// `(progn body)` when the situations fire at load/execute time, else NIL
    /// (bliss-x5y.6 follow-up). Real eval-whens carry `:execute`, so this matches
    /// the tree-walker; the union with `:load-toplevel` matches load-time forms.
    fn lower_eval_when(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !rest.is_cons() {
            return Err(Bail);
        }
        let (situations, body) = cp(rest);
        let fires = list_to_vec(situations).iter().any(|s| {
            s.is_symbol()
                && matches!(
                    symbol_bare_name(&sym_name(*s)).as_str(),
                    "EXECUTE" | "EVAL" | "LOAD-TOPLEVEL" | "LOAD"
                )
        });
        if fires {
            self.lower_progn(body)
        } else {
            let c = self.add_const(NIL);
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

        // A binding captured by a lambda introduced *within* this let body — in
        // particular one produced by a macro / compiler-macro expansion, e.g.
        // alexandria's CURRY, which the enclosing function's up-front capture
        // analysis never saw — must still be boxed. Merge this body's capture set
        // now so `alloc_local` boxes those variables.
        if self.portable {
            for name in compute_captured_names(body) {
                self.captured_names.insert(name);
            }
        }

        let saved_next_local = self.next_local;
        let special_count = binding_forms
            .iter()
            .filter(|binding| {
                binding_name_init(**binding)
                    .map(|(name, _)| is_special_name(&name))
                    .unwrap_or(false)
            })
            .count() as u16;

        // A let that binds any captured variable needs a fresh child EnvFrame so
        // each entry captures a distinct binding.
        let has_boxed = binding_forms.iter().any(|b| {
            binding_name_init(*b)
                .map(|(n, _)| !is_special_name(&n) && self.captured_names.contains(&n))
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
                if is_special_name(&name) {
                    let symbol = resolve_sym(&name).ok_or(Bail)?.as_symbol_index();
                    self.emit(Instr::BindSpecial(symbol));
                } else {
                    let loc = self.alloc_local(&name);
                    store(self, &name, loc);
                }
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
                locs.push((
                    name.clone(),
                    (!is_special_name(name)).then(|| self.alloc_local(name)),
                ));
            }
            // Values are on the stack in binding order; store in reverse.
            for (name, loc) in locs.into_iter().rev() {
                if let Some(loc) = loc {
                    store(self, &name, loc);
                } else {
                    let symbol = resolve_sym(&name).ok_or(Bail)?.as_symbol_index();
                    self.emit(Instr::BindSpecial(symbol));
                }
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
        if special_count != 0 {
            self.emit(Instr::UnbindSpecial(special_count));
        }
        if has_boxed {
            self.emit(Instr::PopEnvChild);
        }
        Ok(())
    }

    /// `(macrolet ((name ll body…) …) body…)` / `(symbol-macrolet …)` in a
    /// function body: local macros and symbol-macros are purely expand-time, so
    /// fully expand the whole form (which installs the local definitions, expands
    /// their uses in the body, and strips the wrapper per §4.2.7) and lower the
    /// resulting macro-free body. `macroexpand_all` walks into ordinary special
    /// forms, keeping their operators intact, so the lowerer's dedicated handlers
    /// (LOOP, TAGBODY, …) still see the structure they expect.
    fn lower_macrolet(&mut self, form: BlissVal) -> LowerResult<()> {
        if self.macro_env.is_none() {
            self.macro_env = Some(super::macroexpand_environment_from_cli(self.env));
        }
        let menv = self.macro_env.as_ref().unwrap();
        match compiler_macroexpand::macroexpand_all(form, menv) {
            Ok(expanded) => self.lower_expr(expanded),
            Err(_) => Err(record_bail(|| "macroexpand:macrolet".to_string())),
        }
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
        // A capturing local function is a closure value in a boxed binding of its
        // name: `(fn args…)` calls it through `funcall`, which dispatches the
        // closure's registered bytecode (with its captured environment installed).
        if self.closure_fns.contains(name) {
            let funcall = resolve_sym("FUNCALL").ok_or(Bail)?;
            let call = arena_cons(funcall, arena_cons(op, rest));
            return self.lower_expr(call);
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
        // Compiler macros are optional call rewrites, distinct from ordinary
        // macro operators. Apply exactly one step here and let lower_expr
        // process the accepted replacement recursively.
        let form = arena_cons(op, rest);
        if self.macro_env.is_none() {
            self.macro_env = Some(super::macroexpand_environment_from_cli(self.env));
        }
        if let Ok((expanded, true)) =
            compiler_macroexpand::compiler_macroexpand_1(form, self.macro_env.as_ref().unwrap())
        {
            return self.lower_expr(expanded);
        }
        // An unhandled special operator is not a call.
        if is_bail_special(name) {
            return Err(record_bail(|| format!("special:{name}")));
        }
        // Emit a named call only when the compile environment establishes that
        // the operator is a function. This includes the complete builtin
        // predicate, not merely the original slice-1 allowlist: treating every
        // unknown operator as a forward function miscompiled implementation
        // forms such as quasiquote and can silently change a large source load.
        // A genuinely forward-referenced function remains correct via the
        // tree-walker fallback and can be predeclared by a future compilation-
        // unit pass without weakening this safety boundary.
        let bare = symbol_bare_name(name);
        let is_user_fn =
            self.env.funs.borrow().contains_key(name) || super::global_fn(name).is_some();
        let is_builtin = super::is_builtin_function(&bare) || PRIMITIVE_ALLOWLIST.contains(&name);
        if !is_user_fn && !is_builtin {
            // Portable (BFASL) compilation represents a forward reference to a
            // not-yet-defined function directly: emit a named call that resolves
            // against the symbol's function cell at load/run time. Special
            // operators, macros, symbol-macros, and quasiquote have all been
            // handled above, so a remaining unknown operator is a function call.
            // The opportunistic compiler stays conservative and defers an unknown
            // operator to the tree-walker instead.
            if !self.portable {
                return Err(record_bail(|| format!("call:{name}")));
            }
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
            let last = i + 1 == npairs;
            self.lower_expr(val)?; // +1
            self.store_to_symbol_place(var, last)?;
        }
        Ok(())
    }

    /// Store the value on top of the operand stack into a symbol place (local
    /// slot, boxed env var, or global), clearing multiple values. If `last`, the
    /// stored value is reloaded so SETQ/SETF leaves it on the stack as the result;
    /// otherwise the stack is left one shorter. Bails on a symbol-macro place
    /// (that is really a SETF of the expansion).
    fn store_to_symbol_place(&mut self, var: BlissVal, last: bool) -> LowerResult<()> {
        if self
            .env
            .symbol_macros
            .borrow()
            .contains_key(&var.as_symbol_index())
        {
            return Err(Bail);
        }
        let name = sym_name(var);
        match self.lookup_local(&name) {
            Some(VarLoc::Slot(slot)) => {
                self.emit(Instr::StoreLocal(slot));
                self.pop_n(1);
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
        Ok(())
    }

    /// `(setf place val ...)` — when every place is a plain symbol, SETF is
    /// exactly SETQ, which the lowerer already handles. Complex places
    /// (car/aref/slot/…) need the setf-expander machinery, which isn't available
    /// here, so bail to the tree-walker. This is what unblocks counting loops —
    /// the increment `(setf i (1+ i))` is the reason idiomatic loops never
    /// promoted to native code (bliss-jtc.26).
    fn lower_setf(&mut self, rest: BlissVal) -> LowerResult<()> {
        let items = list_to_vec(rest);
        if items.len() % 2 != 0 {
            return Err(Bail);
        }
        if items.is_empty() {
            let c = self.add_const(NIL);
            self.emit(Instr::Const(c));
            self.push_n(1);
            return Ok(());
        }
        let npairs = items.len() / 2;
        for i in 0..npairs {
            let place = items[2 * i];
            let val = items[2 * i + 1];
            let last = i + 1 == npairs;
            if place.is_symbol() {
                // Symbol place: identical to SETQ.
                self.lower_expr(val)?;
                self.store_to_symbol_place(place, last)?;
            } else if let Some((setter, cons_arg)) = cons_setf_place(place) {
                // `(setf (car|cdr X) V)` → the internal store primitive
                // BLISS::SET-CAR / SET-CDR (cons, value), which writes in place
                // and returns the value.
                let sym = resolve_sym(setter).ok_or(Bail)?.as_symbol_index();
                self.lower_expr(cons_arg)?; // cons
                self.lower_expr(val)?; // value
                self.emit(Instr::CallNamed { sym, nargs: 2 });
                self.pop_n(2);
                self.push_n(1); // result: the stored value
                if !last {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            } else if let Some((key, table)) = self.gethash_place(place) {
                // `(setf (gethash key table) val)` → the internal store primitive
                // BLISS::PUT-GETHASH (bliss-x5y.2). Push value, key, table in the
                // interpreter's value-first order, then call; the result is the
                // value. Other complex places (car/aref/slot/…) still bail — the
                // setf-expander machinery is not available here.
                let sym = resolve_sym("BLISS::PUT-GETHASH")
                    .ok_or(Bail)?
                    .as_symbol_index();
                self.lower_expr(val)?; // value
                self.lower_expr(key)?; // key
                self.lower_expr(table)?; // table
                self.emit(Instr::CallNamed { sym, nargs: 3 });
                self.pop_n(3);
                self.push_n(1); // result: the stored value
                if !last {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            } else if self.documentation_place_args(place).is_some() {
                // Bliss currently treats documentation metadata as advisory:
                // the tree-walker accepts the store without retaining metadata
                // or evaluating the ignored place arguments, then returns the
                // assigned value. Preserve those existing semantics here.
                self.lower_expr(val)?;
                self.emit(Instr::ClearMv);
                if !last {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            } else if let Some((seq, index)) = aref_setf_place(place) {
                // `(setf (aref|svref|char|schar|row-major-aref|elt seq i) val)` →
                // the internal store primitive BLISS::SET-AREF (seq, index, value).
                let sym = resolve_sym("BLISS::SET-AREF").ok_or(Bail)?.as_symbol_index();
                self.lower_expr(seq)?;
                self.lower_expr(index)?;
                self.lower_expr(val)?;
                self.emit(Instr::CallNamed { sym, nargs: 3 });
                self.pop_n(3);
                self.push_n(1);
                if !last {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            } else if let Some((writer, args)) = self.user_setf_writer_place(place) {
                // `(setf (f a b) val)` for a user `(defun (setf f) …)` writer:
                // call the writer as `(writer val a b)` (new value first, then the
                // place subforms), which returns the stored value.
                let sym = resolve_sym(&writer).ok_or(Bail)?.as_symbol_index();
                self.lower_expr(val)?; // new value
                let mut nargs = 1u16;
                for arg in args {
                    self.lower_expr(arg)?;
                    nargs += 1;
                }
                self.emit(Instr::CallNamed { sym, nargs });
                self.pop_n(nargs);
                self.push_n(1); // result: the stored value
                if !last {
                    self.emit(Instr::Pop);
                    self.pop_n(1);
                }
            } else {
                return Err(Bail);
            }
        }
        Ok(())
    }

    /// Recognise a `(f a b …)` place whose accessor `f` has a user
    /// `(defun (setf f) …)` writer, returning the mangled writer-symbol name and
    /// the place's argument forms. Only in portable mode.
    fn user_setf_writer_place(&self, place: BlissVal) -> Option<(String, Vec<BlissVal>)> {
        if !self.portable || !place.is_cons() {
            return None;
        }
        let (op, rest) = cp(place);
        if !op.is_symbol() {
            return None;
        }
        let name = sym_name(op);
        if !super::env_has_setf_writer(&name) {
            return None;
        }
        Some((super::setf_writer_symbol_name(&name), list_to_vec(rest)))
    }

    /// Recognise a `(gethash key table)` place — exactly two arguments, head
    /// symbol `GETHASH` (any package) — returning `(key, table)` (bliss-x5y.2).
    fn gethash_place(&self, place: BlissVal) -> Option<(BlissVal, BlissVal)> {
        if !place.is_cons() {
            return None;
        }
        let items = list_to_vec(place);
        if items.len() != 3 || !items[0].is_symbol() {
            return None;
        }
        if symbol_bare_name(&sym_name(items[0])) != "GETHASH" {
            return None;
        }
        Some((items[1], items[2]))
    }

    /// Recognise `(documentation object [doc-type])`, whose setter is an
    /// intentionally metadata-only no-op in the current runtime.
    fn documentation_place_args(&self, place: BlissVal) -> Option<Vec<BlissVal>> {
        if !place.is_cons() {
            return None;
        }
        let items = list_to_vec(place);
        if !(2..=3).contains(&items.len()) || !items[0].is_symbol() {
            return None;
        }
        (symbol_bare_name(&sym_name(items[0])) == "DOCUMENTATION").then(|| items[1..].to_vec())
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
        let val_form = if vrest.is_cons() { cp(vrest).0 } else { NIL };
        self.lower_expr(val_form)?; // +1
        self.emit_return_from(&name)?;
        // ReturnFrom transfers control; model it as consuming the value and
        // notionally yielding one (the trailing slot is dead code).
        self.pop_n(1);
        self.push_n(1);
        Ok(())
    }

    /// Emit the appropriate `return-from` transfer for block `name`: a local
    /// `ReturnFrom` when the block is lexical to this function, or a
    /// `ReturnFromNamed` when it lives in an enclosing function this body closes
    /// over. Bails if the block is neither.
    fn emit_return_from(&mut self, name: &str) -> LowerResult<()> {
        if let Some((_, id)) = self.block_scope.iter().rev().find(|(n, _)| n == name) {
            let block_id = *id;
            self.emit(Instr::ReturnFrom { block_id });
            return Ok(());
        }
        if self.enclosing_blocks.contains(name) {
            let name_idx = self.intern_name(name);
            self.emit(Instr::ReturnFromNamed { name_idx });
            return Ok(());
        }
        Err(Bail)
    }

    /// `(return value?)` == `(return-from nil value?)`.
    fn lower_return(&mut self, rest: BlissVal) -> LowerResult<()> {
        let val_form = if rest.is_cons() { cp(rest).0 } else { NIL };
        self.lower_expr(val_form)?;
        self.emit_return_from("NIL")?;
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
        let result = if result_rest.is_cons() {
            cp(result_rest).0
        } else {
            NIL
        };

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
        let result = if result_rest.is_cons() {
            cp(result_rest).0
        } else {
            NIL
        };

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

        let bindings = form_list(&[form_list(&[var, NIL]), form_list(&[rest_var, list_form])]);
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
        if forms.is_empty() {
            return Err(Bail);
        }
        // Simple loop iff all clauses are compound. A bare atom (a loop keyword)
        // means the extended grammar — dispatch the common single-`for` shapes by
        // their iteration keyword, else bail to the tree-walker's full LOOP.
        if !forms.iter().all(|f| f.is_cons()) {
            let kw = |f: BlissVal| -> Option<String> {
                f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
            };
            // Try the proven specialized handlers first for the shapes they
            // recognize; each bails during parsing (before emitting) on an
            // unsupported clause, so falling through to the general compiler is
            // safe. A partial emit (code grew before a bail) can't be recovered,
            // so give up in that case.
            let snapshot = self.code.len();
            let specialized = match kw(forms[0]).as_deref() {
                Some("WHILE") | Some("UNTIL") => Some(self.lower_loop_while(&forms)),
                Some("REPEAT") => Some(self.lower_loop_repeat(&forms)),
                Some("FOR") if forms.len() >= 3 => match kw(forms[2]).as_deref() {
                    Some("FROM") | Some("UPFROM") | Some("DOWNFROM") => {
                        Some(self.lower_loop_numeric_for(&forms))
                    }
                    Some("IN") => Some(self.lower_loop_for_in(&forms)),
                    Some("ON") => Some(self.lower_loop_for_on(&forms)),
                    Some("BEING") => Some(self.lower_loop_for_being_hash(&forms)),
                    _ => None,
                },
                _ => None,
            };
            match specialized {
                Some(Ok(())) => return Ok(()),
                Some(Err(_)) if self.code.len() != snapshot => return Err(Bail),
                _ => {}
            }
            return self.lower_loop_general(&forms);
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

    /// General extended-LOOP compiler: expand the clause grammar to core forms
    /// (`block`/`let*`/`tagbody`/`go`/`setq`) and lower that. This is the
    /// source-free path for the richer shapes the specialized handlers above do
    /// not cover — `with`, `for … {in|on} … by`, parallel accumulation with
    /// `when`/`unless`/`and`/`into`, `finally`, `initially`, `while`/`until`,
    /// `repeat`, `return`. Anything unrecognized bails before any code is
    /// emitted (the whole form is built as data, then lowered once).
    fn lower_loop_general(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        let forms = HostRoot::new(forms.to_vec());
        let id = self.fresh_id();
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        let top = resolve_sym(&format!("%LG-TOP{id}")).ok_or(Bail)?;
        let end = resolve_sym(&format!("%LG-END{id}")).ok_or(Bail)?;

        let mut bindings = HostRoot::new(Vec::<BlissVal>::new());
        let mut top_tests = HostRoot::new(Vec::<BlissVal>::new());
        let mut pre = HostRoot::new(Vec::<BlissVal>::new());
        let mut body = HostRoot::new(Vec::<BlissVal>::new());
        let mut steps = HostRoot::new(Vec::<BlissVal>::new());
        let mut initially = HostRoot::new(Vec::<BlissVal>::new());
        let mut finally = HostRoot::new(Vec::<BlissVal>::new());
        let mut finalize = HostRoot::new(Vec::<BlissVal>::new());
        let mut list_acc = HostRoot::new(None::<BlissVal>);
        let mut sum_acc = HostRoot::new(None::<BlissVal>);
        let mut count_acc = HostRoot::new(None::<BlissVal>);
        let mut explicit_result = false;
        let mut nsym = 0usize;
        let fresh = |tag: &str, nsym: &mut usize| -> LowerResult<BlissVal> {
            *nsym += 1;
            resolve_sym(&format!("%LG-{tag}{id}_{}", *nsym)).ok_or(Bail)
        };

        let mut i = 0;
        while i < forms.len() {
            let key = kw(forms[i]);
            match key.as_deref() {
                Some("WITH") => {
                    // with VAR [= EXPR] {and VAR [= EXPR]}*
                    loop {
                        let var = *forms.get(i + 1).ok_or(Bail)?;
                        if !var.is_symbol() {
                            return Err(Bail);
                        }
                        let (init, adv) =
                            if kw(*forms.get(i + 2).ok_or(Bail)?).as_deref() == Some("=") {
                                (*forms.get(i + 3).ok_or(Bail)?, 4)
                            } else {
                                (NIL, 2)
                            };
                        bindings.push(form_list(&[var, init]));
                        i += adv;
                        if kw(*forms.get(i).unwrap_or(&NIL)).as_deref() == Some("AND") {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                }
                Some("FOR") | Some("AS") => {
                    let var = *forms.get(i + 1).ok_or(Bail)?;
                    match kw(*forms.get(i + 2).ok_or(Bail)?).as_deref() {
                        Some("IN") | Some("ON") => {
                            let on = kw(forms[i + 2]).as_deref() == Some("ON");
                            let list = *forms.get(i + 3).ok_or(Bail)?;
                            let lst = fresh("LST", &mut nsym)?;
                            bindings.push(form_list(&[lst, list]));
                            let mut adv = 4;
                            // optional `by STEPFN`
                            let step_expr = if kw(*forms.get(i + 4).unwrap_or(&NIL)).as_deref()
                                == Some("BY")
                            {
                                let f = *forms.get(i + 5).ok_or(Bail)?;
                                adv = 6;
                                form_list(&[s("FUNCALL")?, f, lst])
                            } else {
                                form_list(&[s("CDR")?, lst])
                            };
                            top_tests.push(form_list(&[
                                s("WHEN")?,
                                form_list(&[s("NULL")?, lst]),
                                form_list(&[s("GO")?, end]),
                            ]));
                            let cur = if on {
                                lst
                            } else {
                                form_list(&[s("CAR")?, lst])
                            };
                            loop_pattern_assign(var, cur, &mut bindings, &mut pre)?;
                            steps.push(form_list(&[s("SETQ")?, lst, step_expr]));
                            i += adv;
                        }
                        Some("=") => {
                            let init = *forms.get(i + 3).ok_or(Bail)?;
                            if !var.is_symbol() {
                                return Err(Bail);
                            }
                            if kw(*forms.get(i + 4).unwrap_or(&NIL)).as_deref() == Some("THEN") {
                                // `for VAR = INIT then STEP`: INIT on the first
                                // iteration, STEP on every subsequent one. Bind to
                                // INIT once; re-assign to STEP in the step section
                                // (which runs after the body, before the next top
                                // test), so iteration 1 sees INIT and later
                                // iterations see STEP.
                                let step = *forms.get(i + 5).ok_or(Bail)?;
                                bindings.push(form_list(&[var, init]));
                                steps.push(form_list(&[s("SETQ")?, var, step]));
                                i += 6;
                            } else {
                                // `for VAR = FORM` (no `then`): FORM is re-evaluated
                                // each iteration.
                                bindings.push(form_list(&[var, NIL]));
                                pre.push(form_list(&[s("SETQ")?, var, init]));
                                i += 4;
                            }
                        }
                        Some("FROM") | Some("UPFROM") | Some("DOWNFROM") => {
                            if !var.is_symbol() {
                                return Err(Bail);
                            }
                            let down = kw(forms[i + 2]).as_deref() == Some("DOWNFROM");
                            let start = *forms.get(i + 3).ok_or(Bail)?;
                            bindings.push(form_list(&[var, start]));
                            let mut adv = 4;
                            let mut limit: Option<(BlissVal, &str)> = None;
                            match kw(*forms.get(i + 4).unwrap_or(&NIL)).as_deref() {
                                Some("TO") | Some("UPTO") => {
                                    limit = Some((*forms.get(i + 5).ok_or(Bail)?, "<="));
                                    adv = 6;
                                }
                                Some("BELOW") => {
                                    limit = Some((*forms.get(i + 5).ok_or(Bail)?, "<"));
                                    adv = 6;
                                }
                                Some("DOWNTO") => {
                                    limit = Some((*forms.get(i + 5).ok_or(Bail)?, ">="));
                                    adv = 6;
                                }
                                Some("ABOVE") => {
                                    limit = Some((*forms.get(i + 5).ok_or(Bail)?, ">"));
                                    adv = 6;
                                }
                                _ => {}
                            }
                            let step = if kw(*forms.get(i + adv).unwrap_or(&NIL)).as_deref()
                                == Some("BY")
                            {
                                let by = *forms.get(i + adv + 1).ok_or(Bail)?;
                                adv += 2;
                                by
                            } else {
                                BlissVal::from_fixnum(1)
                            };
                            if let Some((bound, cmp)) = limit {
                                // terminate when var passes the bound
                                let test_cmp = if cmp == "<=" {
                                    ">"
                                } else if cmp == "<" {
                                    ">="
                                } else if cmp == ">=" {
                                    "<"
                                } else {
                                    "<="
                                };
                                top_tests.push(form_list(&[
                                    s("WHEN")?,
                                    form_list(&[s(test_cmp)?, var, bound]),
                                    form_list(&[s("GO")?, end]),
                                ]));
                            }
                            let op = if down { "-" } else { "+" };
                            steps.push(form_list(&[
                                s("SETQ")?,
                                var,
                                form_list(&[s(op)?, var, step]),
                            ]));
                            i += adv;
                        }
                        _ => return Err(Bail),
                    }
                }
                Some("REPEAT") => {
                    let n = *forms.get(i + 1).ok_or(Bail)?;
                    let counter = fresh("REP", &mut nsym)?;
                    bindings.push(form_list(&[counter, n]));
                    top_tests.push(form_list(&[
                        s("WHEN")?,
                        form_list(&[s("<=")?, counter, BlissVal::from_fixnum(0)]),
                        form_list(&[s("GO")?, end]),
                    ]));
                    steps.push(form_list(&[
                        s("SETQ")?,
                        counter,
                        form_list(&[s("1-")?, counter]),
                    ]));
                    i += 2;
                }
                Some("WHILE") => {
                    let test = *forms.get(i + 1).ok_or(Bail)?;
                    top_tests.push(form_list(&[
                        s("WHEN")?,
                        form_list(&[s("NOT")?, test]),
                        form_list(&[s("GO")?, end]),
                    ]));
                    i += 2;
                }
                Some("UNTIL") => {
                    let test = *forms.get(i + 1).ok_or(Bail)?;
                    top_tests.push(form_list(&[
                        s("WHEN")?,
                        test,
                        form_list(&[s("GO")?, end]),
                    ]));
                    i += 2;
                }
                Some("DO") | Some("DOING") => {
                    i += 1;
                    while i < forms.len() && kw(forms[i]).is_none() {
                        body.push(forms[i]);
                        i += 1;
                    }
                }
                Some("INITIALLY") => {
                    i += 1;
                    while i < forms.len() && kw(forms[i]).is_none() {
                        initially.push(forms[i]);
                        i += 1;
                    }
                }
                Some("FINALLY") => {
                    i += 1;
                    while i < forms.len() && kw(forms[i]).is_none() {
                        finally.push(forms[i]);
                        i += 1;
                    }
                }
                Some("RETURN") => {
                    let e = *forms.get(i + 1).ok_or(Bail)?;
                    body.push(form_list(&[s("RETURN")?, e]));
                    explicit_result = true;
                    i += 2;
                }
                Some("WHEN") | Some("IF") | Some("UNLESS") => {
                    let negate = key.as_deref() == Some("UNLESS");
                    let test = *forms.get(i + 1).ok_or(Bail)?;
                    i += 2;
                    // then-branch: one or more selectable clauses (do/return/
                    // accumulations, joined by `and`).
                    let mut then_forms = HostRoot::new(Vec::<BlissVal>::new());
                    parse_loop_selectable(
                        &forms,
                        &mut i,
                        &kw,
                        &mut bindings,
                        &mut list_acc,
                        &mut sum_acc,
                        &mut count_acc,
                        &mut nsym,
                        id,
                        &mut then_forms,
                        &mut explicit_result,
                    )?;
                    if then_forms.is_empty() {
                        return Err(Bail);
                    }
                    // optional `else` branch.
                    let mut else_forms = HostRoot::new(Vec::<BlissVal>::new());
                    if kw(*forms.get(i).unwrap_or(&NIL)).as_deref() == Some("ELSE") {
                        i += 1;
                        parse_loop_selectable(
                            &forms,
                            &mut i,
                            &kw,
                            &mut bindings,
                            &mut list_acc,
                            &mut sum_acc,
                            &mut count_acc,
                            &mut nsym,
                            id,
                            &mut else_forms,
                            &mut explicit_result,
                        )?;
                    }
                    // optional terminating `end`.
                    if kw(*forms.get(i).unwrap_or(&NIL)).as_deref() == Some("END") {
                        i += 1;
                    }
                    // Bind the `it` anaphor (CLHS 6.1.5) to the test value in a
                    // fresh temp, then rewrite every `it`/`:it` in the branch
                    // forms to reference it. LOOP matches the anaphor by *name*,
                    // so the keyword `:it` works too; a keyword cannot be a
                    // variable, hence substitution rather than a lexical `it`
                    // binding. TEST is evaluated exactly once (into the temp) and
                    // the temp doubles as the branch guard. then_forms/else_forms
                    // stay HostRoot-rooted across the allocating form_list calls.
                    let it_temp = fresh("IT", &mut nsym)?;
                    let mut then_progn = HostRoot::new(vec![s("PROGN")?]);
                    then_progn.extend(then_forms.iter().map(|f| subst_loop_it(*f, it_temp)));
                    let cond_form = if else_forms.is_empty() {
                        let guard = if negate { "UNLESS" } else { "WHEN" };
                        form_list(&[s(guard)?, it_temp, form_list(&then_progn)])
                    } else {
                        let mut else_progn = HostRoot::new(vec![s("PROGN")?]);
                        else_progn.extend(else_forms.iter().map(|f| subst_loop_it(*f, it_temp)));
                        // `(if it then else)`, flipping arms for `unless`.
                        let (a, b) = if negate {
                            (form_list(&else_progn), form_list(&then_progn))
                        } else {
                            (form_list(&then_progn), form_list(&else_progn))
                        };
                        form_list(&[s("IF")?, it_temp, a, b])
                    };
                    let binding = form_list(&[it_temp, test]);
                    body.push(form_list(&[
                        s("LET")?,
                        form_list(&[binding]),
                        cond_form,
                    ]));
                }
                Some("COLLECT") | Some("COLLECTING") | Some("APPEND") | Some("APPENDING")
                | Some("NCONC") | Some("NCONCING") | Some("SUM") | Some("SUMMING")
                | Some("COUNT") | Some("COUNTING") => {
                    let mut acc_forms = HostRoot::new(Vec::<BlissVal>::new());
                    parse_loop_accumulations(
                        &forms,
                        &mut i,
                        &kw,
                        &mut bindings,
                        &mut finalize,
                        &mut list_acc,
                        &mut sum_acc,
                        &mut count_acc,
                        &mut nsym,
                        id,
                        &mut acc_forms,
                    )?;
                    body.extend(acc_forms.iter().copied());
                }
                _ => return Err(Bail),
            }
        }

        // Assemble the result value.
        let result = if let Some(acc) = list_acc.as_ref().copied() {
            form_list(&[s("NREVERSE")?, acc])
        } else if let Some(acc) = sum_acc.as_ref().copied() {
            acc
        } else if let Some(acc) = count_acc.as_ref().copied() {
            acc
        } else {
            NIL
        };
        let result = HostRoot::new(result);

        // (tagbody %top <tests> <pre> <body> <steps> (go %top) %end)
        let mut tb = HostRoot::new(vec![s("TAGBODY")?, top]);
        tb.extend(top_tests.iter().copied());
        tb.extend(pre.iter().copied());
        tb.extend(body.iter().copied());
        tb.extend(steps.iter().copied());
        tb.push(form_list(&[s("GO")?, top]));
        tb.push(end);
        let tagbody = form_list(&tb);
        let tagbody = HostRoot::new(tagbody);

        // (let* (bindings) initially... tagbody finalize... finally... result)
        let mut let_items = HostRoot::new(vec![s("LET*")?, vec_to_list(&bindings)]);
        let_items.extend(initially.iter().copied());
        let_items.push(*tagbody);
        let_items.extend(finalize.iter().copied());
        let_items.extend(finally.iter().copied());
        // The result is unreachable only if an explicit RETURN always fires; it is
        // still a valid trailing form.
        let _ = explicit_result;
        let_items.push(*result);
        let let_form = vec_to_list(&let_items);
        let let_form = HostRoot::new(let_form);

        self.lower_expr(form_list(&[s("BLOCK")?, NIL, *let_form]))
    }

    /// `(loop repeat COUNT {do ...|collect EXPR|sum EXPR|count EXPR})`.
    fn lower_loop_repeat(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        if forms.len() < 4 || symbol_bare_name(&sym_name(forms[0])) != "REPEAT" {
            return Err(Bail);
        }
        let id = self.fresh_id();
        let s = |name: &str| resolve_sym(name).ok_or(Bail);
        let count = resolve_sym(&format!("%LOOP-REPEAT{id}")).ok_or(Bail)?;
        let acc = resolve_sym(&format!("%LOOP-ACC{id}")).ok_or(Bail)?;
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let (uses_acc, acc_init, per_iter, result) = self.lower_loop_action(&forms[2..], acc)?;

        let mut when_items = vec![
            s("WHEN")?,
            form_list(&[s(">")?, count, BlissVal::from_fixnum(0)]),
        ];
        when_items.extend(per_iter);
        when_items.push(form_list(&[
            s("SETQ")?,
            count,
            form_list(&[s("1-")?, count]),
        ]));
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);
        let mut bindings = vec![form_list(&[count, forms[1]])];
        if uses_acc {
            bindings.push(form_list(&[acc, acc_init]));
        }
        let let_form = form_list(&[s("LET")?, form_list(&bindings), tagbody, result]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// Extended-LOOP stage 1 (bliss-x5y.3): the pervasive ascending numeric
    /// counter — `(loop for VAR from START (below|to|upto) END [by STEP] do
    /// BODY...)` — expanded to the same block/let/tagbody/go shape as DOTIMES so
    /// it promotes to native T1. Anything else (list/hash iteration, collect/sum,
    /// while/until, descending, multiple `for`) bails to the tree-walker.
    fn lower_loop_numeric_for(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        // Bare (KEYWORD:-stripped, upcased) name of a loop-keyword token.
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        let kw_is = |f: BlissVal, name: &str| kw(f).as_deref() == Some(name);

        // for VAR from START LIMIT-KW END [by STEP] do BODY...  (min 8 tokens)
        if forms.len() < 8 || !kw_is(forms[0], "FOR") || !forms[1].is_symbol() {
            return Err(Bail);
        }
        let var = forms[1];
        // Direction keyword: `from` is neutral (the limit decides), `upfrom`
        // ascends, `downfrom` descends.
        if !(kw_is(forms[2], "FROM") || kw_is(forms[2], "UPFROM") || kw_is(forms[2], "DOWNFROM")) {
            return Err(Bail);
        }
        let start = forms[3];
        // The LIMIT keyword decides direction: below/to/upto ascend, downto/above
        // descend (CL writes `from N downto M`, so direction comes from here).
        let (cmp, descending) = match kw(forms[4]).as_deref() {
            Some("BELOW") => ("<", false),
            Some("TO") | Some("UPTO") => ("<=", false),
            Some("ABOVE") => (">", true),
            Some("DOWNTO") => (">=", true),
            _ => return Err(Bail),
        };
        // Reject a direction keyword that contradicts the limit (upfrom..downto).
        if (descending && kw_is(forms[2], "UPFROM")) || (!descending && kw_is(forms[2], "DOWNFROM"))
        {
            return Err(Bail);
        }
        let step_op = if descending { "-" } else { "+" };
        let end = forms[5];
        // Optional `by STEP`; then `do`.
        let (step, do_at) = if kw_is(forms[6], "BY") {
            if forms.len() < 10 {
                return Err(Bail);
            }
            (forms[7], 8)
        } else {
            (BlissVal::from_fixnum(1), 6)
        };
        let id = self.fresh_id();
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        let end_v = resolve_sym(&format!("%LOOP-END{id}")).ok_or(Bail)?;
        let step_v = resolve_sym(&format!("%LOOP-STEP{id}")).ok_or(Bail)?;
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let acc = resolve_sym(&format!("%LOOP-ACC{id}")).ok_or(Bail)?;
        // The action clause (do / collect / sum / count) starting at `do_at`.
        let (uses_acc, acc_init, per_iter, result) =
            self.lower_loop_action(&forms[do_at..], acc)?;

        // (tagbody top (when (cmp VAR %end) per-iter... (setq VAR (+ VAR %step)) (go top)))
        let test = form_list(&[s(cmp)?, var, end_v]);
        let mut when_items = vec![s("WHEN")?, test];
        when_items.extend(per_iter);
        when_items.push(form_list(&[
            s("SETQ")?,
            var,
            form_list(&[s(step_op)?, var, step_v]),
        ]));
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        let mut binding_items = vec![
            form_list(&[var, start]),
            form_list(&[end_v, end]),
            form_list(&[step_v, step]),
        ];
        if uses_acc {
            binding_items.push(form_list(&[acc, acc_init]));
        }
        let bindings = form_list(&binding_items);
        let let_form = form_list(&[s("LET")?, bindings, tagbody_form, result]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// Parse a LOOP action clause that follows the iteration spec (bliss-x5y.3
    /// stage 3): `do BODY...` | `{collect|sum|count} EXPR`. Returns
    /// `(uses_acc, acc_init, per_iteration_forms, result_form)` — the caller adds
    /// `(acc acc_init)` to the LET when `uses_acc`, runs `per_iteration_forms`
    /// each turn, and makes `result_form` the loop's value. `acc` is a caller-
    /// supplied fresh accumulator symbol. Anything else (a trailing clause, a
    /// second accumulator, `into`, `it`) bails to the tree-walker.
    fn lower_loop_action(
        &self,
        tail: &[BlissVal],
        acc: BlissVal,
    ) -> LowerResult<(bool, BlissVal, Vec<BlissVal>, BlissVal)> {
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        if tail.is_empty() {
            return Err(Bail);
        }
        match kw(tail[0]).as_deref() {
            Some("DO") | Some("DOING") => {
                let body = &tail[1..];
                if body.is_empty() || body.iter().any(|f| !f.is_cons()) {
                    return Err(Bail);
                }
                Ok((false, NIL, body.to_vec(), NIL))
            }
            // `collect EXPR`: push onto acc, reverse at the end (O(1) per item).
            Some("COLLECT") | Some("COLLECTING") => {
                if tail.len() != 2 {
                    return Err(Bail);
                }
                let per = vec![form_list(&[
                    s("SETQ")?,
                    acc,
                    form_list(&[s("CONS")?, tail[1], acc]),
                ])];
                Ok((true, NIL, per, form_list(&[s("NREVERSE")?, acc])))
            }
            Some("SUM") | Some("SUMMING") => {
                if tail.len() != 2 {
                    return Err(Bail);
                }
                let per = vec![form_list(&[
                    s("SETQ")?,
                    acc,
                    form_list(&[s("+")?, acc, tail[1]]),
                ])];
                Ok((true, BlissVal::from_fixnum(0), per, acc))
            }
            Some("COUNT") | Some("COUNTING") => {
                if tail.len() != 2 {
                    return Err(Bail);
                }
                let inc = form_list(&[
                    s("SETQ")?,
                    acc,
                    form_list(&[s("+")?, acc, BlissVal::from_fixnum(1)]),
                ]);
                let per = vec![form_list(&[s("WHEN")?, tail[1], inc])];
                Ok((true, BlissVal::from_fixnum(0), per, acc))
            }
            _ => Err(Bail),
        }
    }

    /// Extended-LOOP list iteration —
    /// `(loop for PATTERN in LIST [for VAR = EXPR]* ACTION...)`. PATTERN may
    /// be a symbol or a cons pattern such as `(package . symbols)`; secondary
    /// variables are recomputed in order after the primary bindings.
    fn lower_loop_for_in(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        if forms.len() < 5
            || kw(forms[0]).as_deref() != Some("FOR")
            || kw(forms[2]).as_deref() != Some("IN")
        {
            return Err(Bail);
        }
        let id = self.fresh_id();
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let acc = resolve_sym(&format!("%LOOP-ACC{id}")).ok_or(Bail)?;

        fn bind_pattern(
            pattern: BlissVal,
            value: BlissVal,
            bindings: &mut Vec<BlissVal>,
            assignments: &mut Vec<BlissVal>,
        ) -> LowerResult<()> {
            let s = |n: &str| resolve_sym(n).ok_or(Bail);
            if pattern.is_nil() {
                return Ok(());
            }
            if pattern.is_symbol() {
                bindings.push(form_list(&[pattern, NIL]));
                assignments.push(form_list(&[s("SETQ")?, pattern, value]));
                return Ok(());
            }
            if !pattern.is_cons() {
                return Err(Bail);
            }
            let (head, tail) = cp(pattern);
            bind_pattern(head, form_list(&[s("CAR")?, value]), bindings, assignments)?;
            bind_pattern(tail, form_list(&[s("CDR")?, value]), bindings, assignments)
        }

        // Parse one or more parallel `for PATTERN in LIST` clauses plus trailing
        // `for VAR = EXPR` clauses. Parallel list iteration stops when any list is
        // exhausted (the WHEN guard ANDs every list variable).
        let mut for_in_lists: Vec<BlissVal> = Vec::new(); // per-clause list variable
        let mut binding_items: Vec<BlissVal> = Vec::new();
        let mut assignments: Vec<BlissVal> = Vec::new();
        let mut steps: Vec<BlissVal> = Vec::new();

        let add_for_in = |pattern: BlissVal,
                          list: BlissVal,
                          idx: usize,
                          binding_items: &mut Vec<BlissVal>,
                          assignments: &mut Vec<BlissVal>,
                          steps: &mut Vec<BlissVal>|
         -> LowerResult<BlissVal> {
            let lst = resolve_sym(&format!("%LOOP-LST{id}_{idx}")).ok_or(Bail)?;
            binding_items.push(form_list(&[lst, list]));
            bind_pattern(pattern, form_list(&[s("CAR")?, lst]), binding_items, assignments)?;
            steps.push(form_list(&[s("SETQ")?, lst, form_list(&[s("CDR")?, lst])]));
            Ok(lst)
        };
        let lst0 = add_for_in(
            forms[1],
            forms[3],
            0,
            &mut binding_items,
            &mut assignments,
            &mut steps,
        )?;
        for_in_lists.push(lst0);

        let mut action_at = 4;
        while forms.get(action_at).and_then(|f| kw(*f)).as_deref() == Some("FOR") {
            match forms.get(action_at + 2).and_then(|f| kw(*f)).as_deref() {
                Some("IN") => {
                    let pattern = *forms.get(action_at + 1).ok_or(Bail)?;
                    let list = *forms.get(action_at + 3).ok_or(Bail)?;
                    let lst = add_for_in(
                        pattern,
                        list,
                        for_in_lists.len(),
                        &mut binding_items,
                        &mut assignments,
                        &mut steps,
                    )?;
                    for_in_lists.push(lst);
                }
                Some("=") => {
                    let var = *forms.get(action_at + 1).ok_or(Bail)?;
                    if !var.is_symbol() {
                        return Err(Bail);
                    }
                    let expr = *forms.get(action_at + 3).ok_or(Bail)?;
                    binding_items.push(form_list(&[var, NIL]));
                    assignments.push(form_list(&[s("SETQ")?, var, expr]));
                }
                _ => return Err(Bail),
            }
            action_at += 4;
        }
        let action = forms.get(action_at..).ok_or(Bail)?;
        let (uses_acc, acc_init, per_iter, result) = self.lower_loop_action(action, acc)?;

        // Guard: every list variable must still be non-nil.
        let guard = if for_in_lists.len() == 1 {
            for_in_lists[0]
        } else {
            let mut and_items = vec![s("AND")?];
            and_items.extend(for_in_lists.iter().copied());
            form_list(&and_items)
        };
        let mut when_items = vec![s("WHEN")?, guard];
        when_items.append(&mut assignments);
        when_items.extend(per_iter);
        when_items.append(&mut steps);
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        if uses_acc {
            binding_items.push(form_list(&[acc, acc_init]));
        }
        let bindings = form_list(&binding_items);
        let let_form = form_list(&[s("LET")?, bindings, tagbody_form, result]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// Hash-table iteration — `(loop for VAR being [the] hash-keys|hash-values
    /// of TABLE <action>)`. Normalize it to the already-supported `for ... in`
    /// lowering over a stdlib-owned snapshot, preserving the unspecified table
    /// traversal order while making ASDF/UIOP hash loops tierable.
    fn lower_loop_for_being_hash(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        if forms.len() < 8
            || kw(forms[0]).as_deref() != Some("FOR")
            || !forms[1].is_symbol()
            || kw(forms[2]).as_deref() != Some("BEING")
        {
            return Err(Bail);
        }

        let mut pos = 3;
        if kw(forms[pos]).as_deref() == Some("THE") {
            pos += 1;
        }
        let snapshot_fn = match forms.get(pos).and_then(|f| kw(*f)).as_deref() {
            Some("HASH-KEYS") => "HASH-TABLE-KEYS",
            Some("HASH-VALUES") => "HASH-TABLE-VALUES",
            _ => return Err(Bail),
        };
        pos += 1;
        if forms.get(pos).and_then(|f| kw(*f)).as_deref() != Some("OF") {
            return Err(Bail);
        }
        let table = *forms.get(pos + 1).ok_or(Bail)?;
        let action = forms.get(pos + 2..).ok_or(Bail)?;
        if action.is_empty() {
            return Err(Bail);
        }

        let snapshot_sym = resolve_sym(snapshot_fn).ok_or(Bail)?;
        let snapshot = form_list(&[snapshot_sym, table]);
        let mut normalized = vec![forms[0], forms[1], resolve_sym("IN").ok_or(Bail)?, snapshot];
        normalized.extend_from_slice(action);
        self.lower_loop_for_in(&normalized)
    }

    /// Extended-LOOP: cons-cell iteration — `(loop for VAR on LIST <action>)` —
    /// where VAR walks successive tails. Expanded to
    /// `(block nil (let ((VAR LIST)) (tagbody top (when VAR per-iter...
    ///     (setq VAR (cdr VAR)) (go top)) result)))`.
    fn lower_loop_for_on(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        if forms.len() < 5
            || kw(forms[0]).as_deref() != Some("FOR")
            || !forms[1].is_symbol()
            || kw(forms[2]).as_deref() != Some("ON")
        {
            return Err(Bail);
        }
        let var = forms[1];
        let list = forms[3];

        let id = self.fresh_id();
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let acc = resolve_sym(&format!("%LOOP-ACC{id}")).ok_or(Bail)?;
        let (uses_acc, acc_init, per_iter, result) = self.lower_loop_action(&forms[4..], acc)?;

        let mut when_items = vec![s("WHEN")?, var];
        when_items.extend(per_iter);
        when_items.push(form_list(&[s("SETQ")?, var, form_list(&[s("CDR")?, var])]));
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        let mut binding_items = vec![form_list(&[var, list])];
        if uses_acc {
            binding_items.push(form_list(&[acc, acc_init]));
        }
        let let_form = form_list(&[s("LET")?, form_list(&binding_items), tagbody_form, result]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
    }

    /// Extended-LOOP: `(loop while TEST <action>)` / `(loop until TEST <action>)`.
    /// `while` runs while TEST is true; `until` while `(not TEST)` (which itself
    /// bails to the tree-walker if NOT isn't natively lowerable — still correct).
    /// The test is re-evaluated each turn and references enclosing-scope vars.
    fn lower_loop_while(&mut self, forms: &[BlissVal]) -> LowerResult<()> {
        let kw = |f: BlissVal| -> Option<String> {
            f.is_symbol().then(|| symbol_bare_name(&sym_name(f)))
        };
        if forms.len() < 3 {
            return Err(Bail);
        }
        let until = match kw(forms[0]).as_deref() {
            Some("WHILE") => false,
            Some("UNTIL") => true,
            _ => return Err(Bail),
        };
        let test_raw = forms[1];

        let id = self.fresh_id();
        let s = |n: &str| resolve_sym(n).ok_or(Bail);
        let top = resolve_sym(&format!("%LOOP-TOP{id}")).ok_or(Bail)?;
        let acc = resolve_sym(&format!("%LOOP-ACC{id}")).ok_or(Bail)?;
        let (uses_acc, acc_init, per_iter, result) = self.lower_loop_action(&forms[2..], acc)?;

        let test = if until {
            form_list(&[s("NOT")?, test_raw])
        } else {
            test_raw
        };
        let mut when_items = vec![s("WHEN")?, test];
        when_items.extend(per_iter);
        when_items.push(form_list(&[s("GO")?, top]));
        let tagbody_form = form_list(&[s("TAGBODY")?, top, form_list(&when_items)]);

        let bindings = if uses_acc {
            form_list(&[form_list(&[acc, acc_init])])
        } else {
            NIL
        };
        let let_form = form_list(&[s("LET")?, bindings, tagbody_form, result]);
        self.lower_expr(form_list(&[s("BLOCK")?, NIL, let_form]))
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
    /// Restart clauses are nested bytecode functions. Enclosing locals used by
    /// a clause are boxed in this activation's heap environment and the restart
    /// captures that live frame when it is established.
    fn lower_restart_case(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !self.portable {
            return Err(Bail);
        }
        let (restartable_form, clauses_form) = cp(rest);
        let clauses = list_to_vec(clauses_form);

        let enclosing_locals: std::collections::HashSet<String> =
            self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();

        let mut restarts = Vec::new();
        for clause in &clauses {
            let (name_form, clause_rest) = cp(*clause);
            if !name_form.is_symbol() {
                return Err(Bail);
            }
            let name = sym_name(name_form).to_uppercase();
            let (params_form, body) = cp(clause_rest);

            let params: std::collections::HashSet<String> = list_to_vec(params_form)
                .iter()
                .filter(|p| p.is_symbol())
                .map(|p| sym_name(*p))
                .filter(|n| !n.starts_with('&'))
                .collect();
            let mut used = std::collections::HashSet::new();
            collect_symbol_names(body, &mut used);
            let captures = used
                .into_iter()
                .filter(|u| enclosing_locals.contains(u) && !params.contains(u))
                .collect::<std::collections::HashSet<_>>();
            let function =
                compile_restart_clause(params_form, body, self.env, &captures, self.portable)
                    .ok_or(Bail)?;
            restarts.push(RestartClauseInfo {
                name,
                function: Box::new(function),
            });
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

    /// Lower a quasiquote template to ordinary value-producing bytecode. This
    /// keeps macro expanders source-free: literal structure is data, while
    /// unquotes are compiled expressions and list construction is explicit.
    fn lower_quasiquote_template(&mut self, form: BlissVal, depth: usize) -> LowerResult<()> {
        if !form.is_cons() {
            let constant = self.add_const(form);
            self.emit(Instr::Const(constant));
            self.push_n(1);
            return Ok(());
        }
        let (head, tail) = cp(form);
        if head.is_symbol() {
            let name = sym_name(head);
            // Nested quasiquote counts depth: QUASIQUOTE raises it, UNQUOTE and
            // UNQUOTE-SPLICING lower it. An UNQUOTE only evaluates once its depth
            // reaches the surrounding template (depth 1); at a deeper level it is
            // preserved as `(UNQUOTE <arg one level shallower>)` structure so the
            // outer expansion re-processes it (e.g. `,,g` inside ``…``).
            if name == "BLISS::UNQUOTE" {
                let expr = tail.is_cons().then(|| cp(tail).0).ok_or(Bail)?;
                if depth == 1 {
                    return self.lower_expr(expr);
                }
                return self.build_qq_operator(head, expr, depth - 1);
            }
            if name == "BLISS::UNQUOTE-SPLICING" {
                // A bare splice form only makes sense as preserved deeper-level
                // structure; at the active level it must appear in element
                // position (handled below), never as a whole template.
                let expr = tail.is_cons().then(|| cp(tail).0).ok_or(Bail)?;
                if depth == 1 {
                    return Err(Bail);
                }
                return self.build_qq_operator(head, expr, depth - 1);
            }
            if name == "BLISS::QUASIQUOTE" {
                let inner = tail.is_cons().then(|| cp(tail).0).ok_or(Bail)?;
                return self.build_qq_operator(head, inner, depth + 1);
            }
        }

        // A spliced element at the active level contributes its list ahead of the
        // recursively built tail. APPEND copies the splice's spine, matching CL
        // quasiquote. At a deeper level the `(UNQUOTE-SPLICING …)` element is
        // preserved as ordinary structure by the general recursion below (its
        // head re-enters the symbol branch above).
        if head.is_cons() {
            let (splice_op, splice_rest) = cp(head);
            if splice_op.is_symbol()
                && sym_name(splice_op) == "BLISS::UNQUOTE-SPLICING"
                && depth == 1
            {
                let expr = splice_rest
                    .is_cons()
                    .then(|| cp(splice_rest).0)
                    .ok_or(Bail)?;
                self.lower_expr(expr)?;
                self.lower_quasiquote_template(tail, depth)?;
                let append = resolve_sym("APPEND").ok_or(Bail)?.as_symbol_index();
                self.emit(Instr::CallNamed {
                    sym: append,
                    nargs: 2,
                });
                self.pop_n(1);
                return Ok(());
            }
            // `,,@x` — (UNQUOTE (UNQUOTE-SPLICING x)) as an element at depth 2:
            // the outer `,` reduces the level and `,@x` splices at the enclosing
            // level. Match the tree-walker (eval_quasiquote_depth): evaluate x now
            // and wrap each spliced element in a single UNQUOTE for the inner
            // backquote, i.e. splice `(mapcar (lambda (e) (list 'UNQUOTE e)) x)`.
            if splice_op.is_symbol() && sym_name(splice_op) == "BLISS::UNQUOTE" && depth == 2 {
                if let Some(inner) = splice_rest.is_cons().then(|| cp(splice_rest).0) {
                    if inner.is_cons() {
                        let (inner_op, inner_rest) = cp(inner);
                        if inner_op.is_symbol()
                            && sym_name(inner_op) == "BLISS::UNQUOTE-SPLICING"
                            && inner_rest.is_cons()
                        {
                            let x = cp(inner_rest).0;
                            let wrapped = self.wrap_unquote_mapcar_form(splice_op, x)?;
                            self.lower_expr(wrapped)?;
                            self.lower_quasiquote_template(tail, depth)?;
                            let append = resolve_sym("APPEND").ok_or(Bail)?.as_symbol_index();
                            self.emit(Instr::CallNamed {
                                sym: append,
                                nargs: 2,
                            });
                            self.pop_n(1);
                            return Ok(());
                        }
                    }
                }
            }
        }

        self.lower_quasiquote_template(head, depth)?;
        self.lower_quasiquote_template(tail, depth)?;
        self.emit(Instr::AllocCons);
        self.pop_n(1);
        Ok(())
    }

    /// Lower `(destructuring-bind pattern expr body...)` by expanding it into a
    /// portable `let*` that binds the pattern from a fresh temporary via
    /// `nth`/`nthcdr`. This keeps the form source-free without a dedicated
    /// runtime binder. Supports required, `&optional` (with defaults),
    /// `&rest`/`&body`, dotted patterns, and nested sub-patterns (recursively);
    /// `&key`, `&aux`, `&whole`, and `&environment` bail so the tree-walker's
    /// full binder handles them.
    fn lower_destructuring_bind(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !self.portable {
            return Err(Bail);
        }
        let (pattern, after) = cp(rest);
        if !after.is_cons() {
            return Err(Bail);
        }
        let (value_form, body) = cp(after);
        let g_name = next_control_token("__DBIND__").replace(':', "_");
        let g = resolve_sym(&g_name).ok_or(Bail)?;
        // First binding evaluates the expression once into the temporary.
        let mut bindings: Vec<BlissVal> = vec![form_list(&[g, value_form])];
        destructure_pattern(pattern, g, &mut bindings)?;

        let mut items = vec![resolve_sym("LET*").ok_or(Bail)?, vec_to_list(&bindings)];
        items.extend(list_to_vec(body));
        let expansion = vec_to_list(&items);
        self.lower_expr(expansion)
    }

    /// Build the form `(mapcar (lambda (#:e) (list 'unquote #:e)) x)` used to
    /// realize `,,@x` at depth 2: it wraps each element produced by `x` in a
    /// single UNQUOTE for the inner backquote. `unquote_sym` is the concrete
    /// `BLISS::UNQUOTE` symbol taken from the source form. The lambda is
    /// noncapturing, so it lowers to a source-free `MakeClosure`.
    fn wrap_unquote_mapcar_form(
        &self,
        unquote_sym: BlissVal,
        x: BlissVal,
    ) -> LowerResult<BlissVal> {
        let e_name = next_control_token("__QQE__").replace(':', "_");
        let e = resolve_sym(&e_name).ok_or(Bail)?;
        let quoted_unquote = form_list(&[resolve_sym("QUOTE").ok_or(Bail)?, unquote_sym]);
        let list_call = form_list(&[resolve_sym("LIST").ok_or(Bail)?, quoted_unquote, e]);
        let lambda = form_list(&[resolve_sym("LAMBDA").ok_or(Bail)?, form_list(&[e]), list_call]);
        Ok(form_list(&[resolve_sym("MAPCAR").ok_or(Bail)?, lambda, x]))
    }

    /// Build a two-element preserved quasiquote form `(op arg-template)` where
    /// `arg` is lowered as a template at `arg_depth`. Used for nested
    /// QUASIQUOTE/UNQUOTE/UNQUOTE-SPLICING that survive as structure rather than
    /// being evaluated at the current level.
    fn build_qq_operator(
        &mut self,
        op: BlissVal,
        arg: BlissVal,
        arg_depth: usize,
    ) -> LowerResult<()> {
        let op_const = self.add_const(op);
        self.emit(Instr::Const(op_const));
        self.push_n(1);
        self.lower_quasiquote_template(arg, arg_depth)?;
        let nil = self.add_const(NIL);
        self.emit(Instr::Const(nil));
        self.push_n(1);
        // (arg . NIL)
        self.emit(Instr::AllocCons);
        self.pop_n(1);
        // (op . (arg))
        self.emit(Instr::AllocCons);
        self.pop_n(1);
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

    /// `(ignore-errors body...)` — a builtin macro the lowerer's user-macro
    /// expansion never sees; lower it to core by expanding to
    /// `(handler-case (progn body...) (error (#:c) (values nil #:c)))`.
    fn lower_ignore_errors(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !self.portable {
            return Err(Bail);
        }
        let c_name = next_control_token("__IE__").replace(':', "_");
        let cvar = resolve_sym(&c_name).ok_or(Bail)?;
        let mut progn_items = vec![resolve_sym("PROGN").ok_or(Bail)?];
        progn_items.extend(list_to_vec(rest));
        let progn = vec_to_list(&progn_items);
        let values = form_list(&[resolve_sym("VALUES").ok_or(Bail)?, NIL, cvar]);
        let clause = form_list(&[resolve_sym("ERROR").ok_or(Bail)?, form_list(&[cvar]), values]);
        let hc = form_list(&[resolve_sym("HANDLER-CASE").ok_or(Bail)?, progn, clause]);
        self.lower_expr(hc)
    }

    /// `(assert test [places [format-control format-args...]])` — portable
    /// subset: lower to `(unless test (error …))`. The interactive
    /// restart/retry protocol and `places` re-reads are not represented (they
    /// require the debugger), which is sound for compiled non-interactive code.
    /// The opportunistic compiler keeps the tree-walker's full ASSERT.
    fn lower_assert(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !self.portable {
            return Err(Bail);
        }
        let (test, after) = cp(rest);
        // Skip the optional `places` list; take format-control + args if present.
        let error_form = if after.is_cons() {
            let (_places, fmt_and_args) = cp(after);
            if fmt_and_args.is_cons() {
                let mut items = vec![resolve_sym("ERROR").ok_or(Bail)?];
                items.extend(list_to_vec(fmt_and_args));
                vec_to_list(&items)
            } else {
                form_list(&[
                    resolve_sym("ERROR").ok_or(Bail)?,
                    arena_str("Assertion failed"),
                ])
            }
        } else {
            form_list(&[
                resolve_sym("ERROR").ok_or(Bail)?,
                arena_str("Assertion failed"),
            ])
        };
        let unless = form_list(&[resolve_sym("UNLESS").ok_or(Bail)?, test, error_form]);
        self.lower_expr(unless)
    }

    /// `(multiple-value-call fn form...)` — apply `fn` to all values produced by
    /// each `form`, concatenated. Lowered portably as
    /// `(apply fn (append (multiple-value-list f1) (multiple-value-list f2) …))`,
    /// reusing MULTIPLE-VALUE-LIST, APPEND, and APPLY. The opportunistic compiler
    /// defers to the tree-walker's native form.
    fn lower_multiple_value_call(&mut self, rest: BlissVal) -> LowerResult<()> {
        if !self.portable {
            return Err(Bail);
        }
        if !rest.is_cons() {
            return Err(Bail);
        }
        let (fn_form, forms) = cp(rest);
        let mvl = resolve_sym("MULTIPLE-VALUE-LIST").ok_or(Bail)?;
        let mut value_lists: Vec<BlissVal> = list_to_vec(forms)
            .into_iter()
            .map(|f| form_list(&[mvl, f]))
            .collect();
        let args_list = match value_lists.len() {
            0 => NIL,
            1 => value_lists.pop().unwrap(),
            _ => {
                let mut items = vec![resolve_sym("APPEND").ok_or(Bail)?];
                items.extend(value_lists);
                vec_to_list(&items)
            }
        };
        let apply_form = form_list(&[resolve_sym("APPLY").ok_or(Bail)?, fn_form, args_list]);
        self.lower_expr(apply_form)
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

    /// Portable lowering of a noncapturing `(lambda …)`: compile its body to a
    /// nested [`BytecodeFunction`], stash it in `self.nested_functions`, and emit
    /// a `MakeClosure` referencing it. This keeps the closure source-free so a
    /// BFASL never has to retain an executable `(lambda …)` form. A body slice 1
    /// cannot yet lower makes the whole enclosing function bail to the
    /// tree-walker (`Bail`), rather than falling back to a non-portable
    /// `EvalHost` the serializer would reject.
    fn emit_portable_closure(
        &mut self,
        params_form: BlissVal,
        body: BlissVal,
        captured: &[String],
    ) -> LowerResult<()> {
        let enclosing_blocks: std::collections::HashSet<String> = self
            .block_scope
            .iter()
            .map(|(n, _)| n.clone())
            .chain(self.enclosing_blocks.iter().cloned())
            .collect();
        let bf = if captured.is_empty() {
            compile_function_in(
                "<lambda>",
                params_form,
                body,
                self.env,
                self.portable,
                false,
                &enclosing_blocks,
            )
            .ok_or(Bail)?
        } else {
            // A capturing lambda reaches the enclosing lexicals through the heap
            // frame it captures. Every captured variable must be boxed (in the
            // frame); a reference to an enclosing frame slot cannot be captured,
            // so bail to the tree-walker. The closure resolves any boxed name in
            // scope, so pass the whole boxed set (plus the enclosing capturing
            // closures it may call).
            for name in captured {
                if matches!(self.lookup_local(name), Some(VarLoc::Slot(_))) {
                    return Err(Bail);
                }
            }
            let captures: std::collections::HashSet<String> = self
                .scopes
                .iter()
                .flat_map(|s| s.iter())
                .filter_map(|(k, v)| matches!(v, VarLoc::Boxed).then(|| k.clone()))
                .collect();
            self.has_env = true;
            let callable = self.closure_fns.clone();
            compile_capturing_local(
                params_form,
                body,
                self.env,
                &captures,
                &callable,
                &enclosing_blocks,
                self.portable,
            )
            .ok_or(Bail)?
        };
        let idx = self.nested_functions.len();
        if idx > u32::MAX as usize {
            return Err(Bail);
        }
        self.nested_functions.push(Box::new(bf));
        self.emit(Instr::MakeClosure {
            func: idx as u32,
            capture_env: !captured.is_empty(),
        });
        self.push_n(1);
        Ok(())
    }

    /// `(lambda params body...)` — a closure value (capturing or not).
    fn lower_lambda(&mut self, op: BlissVal, rest: BlissVal) -> LowerResult<()> {
        let (params_form, body) = cp(rest);
        let captured = self.lambda_captured_locals(params_form, body);
        if self.portable {
            return self.emit_portable_closure(params_form, body, &captured);
        }
        let form = arena_cons(op, rest);
        self.emit_closure(form, &captured)
    }

    /// `(function name)` / `#'(lambda …)` — a function designator or a closure.
    fn lower_function(&mut self, rest: BlissVal) -> LowerResult<()> {
        let (target, _) = cp(rest);
        // `#'localfn` for a capturing flet/labels function is the closure value
        // itself, held in a boxed binding of its name — load it as a variable.
        if target.is_symbol() && self.closure_fns.contains(&sym_name(target)) {
            return self.lower_expr(target);
        }
        if target.is_symbol() && self.portable {
            self.emit(Instr::LoadFunction(target.as_symbol_index()));
            self.push_n(1);
            return Ok(());
        }
        let captured = if target.is_cons() {
            let (t_op, t_rest) = cp(target);
            if t_op.is_symbol() && sym_name(t_op) == "LAMBDA" {
                let (params_form, body) = cp(t_rest);
                let captured = self.lambda_captured_locals(params_form, body);
                if self.portable {
                    return self.emit_portable_closure(params_form, body, &captured);
                }
                captured
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

        let local_names: std::collections::HashSet<String> =
            parsed.iter().map(|(n, _, _, _)| n.clone()).collect();

        // Portable (BFASL) compilation lowers every local function to an
        // env-capturing closure stored in a heap frame: those serialize as nested
        // bytecode functions, so the artifact is source-free and a fresh process
        // can load it. (The gensym `CallNamed` fast path below registers local
        // bodies only in the compiling process's registry, so it never survives a
        // .bfasl round-trip.) A capturing closure additionally reaches the
        // enclosing lexicals through the frame.
        if self.portable {
            return self.lower_flet_capturing(&parsed, body, is_labels);
        }

        // Non-portable opportunistic path. A local function that references an
        // enclosing lexical (a capture) needs the closure machinery, which is
        // portable-only, so defer such a form to the tree-walker.
        let enclosing: std::collections::HashSet<String> =
            self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        let is_capturing = parsed.iter().any(|(_, _, params_form, fbody)| {
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
            used.iter().any(|u| {
                enclosing.contains(u) && !params.contains(u) && !local_names.contains(u)
            })
        });
        if is_capturing {
            return Err(Bail);
        }

        // If any local function is referenced as a VALUE (`#'localfn`) — in the
        // body or in a sibling's body (mutual `labels`) — bail to the tree-walker,
        // which returns a proper closure for it. The bytecode backend only knows
        // how to CALL a lowered local function, not to yield it as a value.
        if references_local_fn_value(body, &local_names)
            || parsed
                .iter()
                .any(|(_, _, _, fbody)| references_local_fn_value(*fbody, &local_names))
        {
            return Err(Bail);
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
            let bf = compile_local_function(
                *params_form,
                *fbody,
                self.env,
                &body_local_fns,
                self.portable,
            )
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

    /// Portable capturing `flet`/`labels`: each local function closes over
    /// enclosing lexicals (and, for `labels`, its siblings). The closures live in
    /// a shared child heap frame so mutual/self recursion resolves; each is bound
    /// to a boxed variable of its name and called through `funcall` (see
    /// `lower_call`). Enclosing captured lexicals must already be boxed
    /// (arranged by `compute_captured_names`); a local that references an unboxed
    /// enclosing slot bails.
    fn lower_flet_capturing(
        &mut self,
        parsed: &[(String, u32, BlissVal, BlissVal)],
        body: BlissVal,
        is_labels: bool,
    ) -> LowerResult<()> {
        let local_names: std::collections::HashSet<String> =
            parsed.iter().map(|(n, _, _, _)| n.clone()).collect();

        // Snapshot the enclosing environment the closures may capture.
        let enclosing_boxed: std::collections::HashSet<String> = self
            .scopes
            .iter()
            .flat_map(|s| s.iter())
            .filter_map(|(k, v)| matches!(v, VarLoc::Boxed).then(|| k.clone()))
            .collect();
        let enclosing_slots: std::collections::HashSet<String> = self
            .scopes
            .iter()
            .flat_map(|s| s.iter())
            .filter_map(|(k, v)| matches!(v, VarLoc::Slot(_)).then(|| k.clone()))
            .collect();
        let enclosing_callable = self.closure_fns.clone();

        // A captured enclosing lexical must be boxed to live in the heap frame;
        // if a local body references an unboxed enclosing slot, defer the whole
        // form to the tree-walker.
        for (_, _, params_form, fbody) in parsed {
            let params: std::collections::HashSet<String> = list_to_vec(*params_form)
                .iter()
                .filter(|p| p.is_symbol())
                .map(|p| sym_name(*p))
                .collect();
            let mut used = std::collections::HashSet::new();
            for f in list_to_vec(*fbody) {
                collect_symbol_names(f, &mut used);
            }
            if used.iter().any(|u| {
                enclosing_slots.contains(u)
                    && !enclosing_boxed.contains(u)
                    && !params.contains(u)
                    && !local_names.contains(u)
            }) {
                return Err(Bail);
            }
        }

        // Establish a child heap frame to hold the closures, so labels siblings
        // (defined into the same frame) resolve through capture.
        self.has_env = true;
        self.emit(Instr::PushEnvChild);
        let saved_next = self.next_local;
        self.enter_scope();
        let saved_closure_fns = self.closure_fns.clone();

        // Names each closure may resolve. labels closures additionally see all
        // siblings; flet closures do not.
        let mut captures = enclosing_boxed;
        let mut callable = enclosing_callable;
        if is_labels {
            for name in &local_names {
                captures.insert(name.clone());
                callable.insert(name.clone());
            }
        }

        // Register the local-function names as boxed closure bindings, visible in
        // the body (calls go through funcall via lower_call's closure_fns path).
        for name in &local_names {
            self.scopes
                .last_mut()
                .unwrap()
                .insert(name.clone(), VarLoc::Boxed);
            self.closure_fns.insert(name.clone());
        }

        // Blocks the closures may non-locally return-from: this function's live
        // blocks plus any it already inherits.
        let enclosing_blocks: std::collections::HashSet<String> = self
            .block_scope
            .iter()
            .map(|(n, _)| n.clone())
            .chain(self.enclosing_blocks.iter().cloned())
            .collect();

        // Build each closure and store it in the child frame.
        for (name, _gensym, params_form, fbody) in parsed {
            let bf = compile_capturing_local(
                *params_form,
                *fbody,
                self.env,
                &captures,
                &callable,
                &enclosing_blocks,
                self.portable,
            )
            .ok_or(Bail)?;
            let idx = self.nested_functions.len();
            if idx > u32::MAX as usize {
                return Err(Bail);
            }
            self.nested_functions.push(Box::new(bf));
            self.emit(Instr::MakeClosure {
                func: idx as u32,
                capture_env: true,
            });
            self.push_n(1);
            let ni = self.intern_name(name);
            self.emit(Instr::DefineEnvVar(ni));
            self.pop_n(1);
        }

        let r = self.lower_progn(body);
        self.closure_fns = saved_closure_fns;
        self.exit_scope(saved_next);
        self.emit(Instr::PopEnvChild);
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
    portable: bool,
) -> Option<BytecodeFunction> {
    let (param_names, min_args, max_args, variadic) = parse_lambda_list(params_form)?;
    let mut lo = Lowerer::new(env);
    let _const_guard = LowererConstGuard::new(&mut lo.constants);
    lo.portable = portable;
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
    let param_types = if variadic {
        vec![DeclaredType::Any; param_names.len()]
    } else {
        declared_parameter_types(fbody, &param_names)?
    };
    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        handler_binds: lo.handler_binds,
        names: lo.names,
        restart_cases: lo.restart_cases,
        nested_functions: lo.nested_functions,
        param_layout,
        param_types,
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: min_args,
        name: "<flet>".to_string(),
        params_form: if variadic { params_form } else { NIL },
        min_args,
        max_args,
        variadic,
    })
}

/// Compile a capturing `flet`/`labels` local function as an env-capturing
/// closure body. `captures` names bindings resolved through the captured heap
/// frame (enclosing lexicals plus, for `labels`, sibling closures); `callable`
/// is the subset of those that are themselves closures and must be invoked via
/// `funcall`. `has_env` is forced so the closure always has an environment to
/// reach the captured frame, even when it defines no boxed locals of its own.
fn compile_capturing_local(
    params_form: BlissVal,
    fbody: BlissVal,
    env: &Env,
    captures: &std::collections::HashSet<String>,
    callable: &std::collections::HashSet<String>,
    enclosing_blocks: &std::collections::HashSet<String>,
    portable: bool,
) -> Option<BytecodeFunction> {
    let (param_names, min_args, max_args, variadic) = parse_lambda_list(params_form)?;
    let mut lo = Lowerer::new(env);
    let _const_guard = LowererConstGuard::new(&mut lo.constants);
    lo.portable = portable;
    lo.captured_names = compute_captured_names(fbody);
    lo.has_env = true;
    lo.closure_fns = callable.clone();
    lo.enclosing_blocks = enclosing_blocks.clone();
    for name in captures {
        lo.scopes[0].insert(name.clone(), VarLoc::Boxed);
    }
    let mut param_layout = Vec::with_capacity(param_names.len());
    for pn in &param_names {
        // A parameter shadows a captured enclosing lexical of the same name.
        let loc = lo.alloc_local(pn);
        param_layout.push((pn.clone(), loc));
    }
    let param_types = if variadic {
        vec![DeclaredType::Any; param_names.len()]
    } else {
        declared_parameter_types(fbody, &param_names)?
    };
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
        nested_functions: lo.nested_functions,
        param_layout,
        param_types,
        has_env: true,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: min_args,
        name: "<flet-closure>".to_string(),
        params_form: if variadic { params_form } else { NIL },
        min_args,
        max_args,
        variadic,
    })
}

/// Recognize a single-index element SETF place `(aref|svref|char|schar|
/// row-major-aref|elt SEQ INDEX)`, returning the sequence and index subforms.
fn aref_setf_place(place: BlissVal) -> Option<(BlissVal, BlissVal)> {
    if !place.is_cons() {
        return None;
    }
    let items = list_to_vec(place);
    if items.len() != 3 || !items[0].is_symbol() {
        return None;
    }
    match symbol_bare_name(&sym_name(items[0])).as_str() {
        "AREF" | "SVREF" | "CHAR" | "SCHAR" | "ROW-MAJOR-AREF" | "ELT" | "BIT" | "SBIT" => {
            Some((items[1], items[2]))
        }
        _ => None,
    }
}

/// Recognize a `(cXr X)` SETF place — any `c[ad]+r` accessor plus `first`/`rest`
/// — returning the internal store primitive (`BLISS::SET-CAR`/`SET-CDR`) and the
/// target cons subform. For a composed accessor the outermost a/d selects the
/// primitive and the remaining a/d letters form the inner accessor applied to
/// `X` (e.g. `(cadr x)` → set-car of `(cdr x)`).
fn cons_setf_place(place: BlissVal) -> Option<(&'static str, BlissVal)> {
    if !place.is_cons() {
        return None;
    }
    let (op, rest) = cp(place);
    if !op.is_symbol() || !rest.is_cons() {
        return None;
    }
    let (arg, tail) = cp(rest);
    if !tail.is_nil() {
        return None; // exactly one argument
    }
    let name = match symbol_bare_name(&sym_name(op)).as_str() {
        "FIRST" => "CAR".to_string(),
        "REST" => "CDR".to_string(),
        other => other.to_string(),
    };
    // c<a/d…>r
    if name.len() < 3 || !name.starts_with('C') || !name.ends_with('R') {
        return None;
    }
    let middle = &name[1..name.len() - 1];
    if middle.is_empty() || !middle.chars().all(|c| c == 'A' || c == 'D') {
        return None;
    }
    let setter = if middle.starts_with('A') {
        "BLISS::SET-CAR"
    } else {
        "BLISS::SET-CDR"
    };
    let inner = &middle[1..];
    let target = if inner.is_empty() {
        arg
    } else {
        let inner_name = format!("C{inner}R");
        form_list(&[resolve_sym(&inner_name)?, arg])
    };
    Some((setter, target))
}

/// Emit `let*` binding + `setq` assignment pairs that bind a LOOP iteration
/// variable, which may be a destructuring cons pattern, from `value`.
fn loop_pattern_assign(
    pattern: BlissVal,
    value: BlissVal,
    bindings: &mut Vec<BlissVal>,
    assigns: &mut Vec<BlissVal>,
) -> LowerResult<()> {
    let pattern = HostRoot::new(pattern);
    let value = HostRoot::new(value);
    let s = |n: &str| resolve_sym(n).ok_or(Bail);
    if pattern.is_nil() {
        return Ok(());
    }
    if pattern.is_symbol() {
        bindings.push(form_list(&[*pattern, NIL]));
        assigns.push(form_list(&[s("SETQ")?, *pattern, *value]));
        return Ok(());
    }
    if !pattern.is_cons() {
        return Err(Bail);
    }
    let (h, t) = cp(*pattern);
    let h = HostRoot::new(h);
    let t = HostRoot::new(t);
    let car = HostRoot::new(form_list(&[s("CAR")?, *value]));
    loop_pattern_assign(*h, *car, bindings, assigns)?;
    let cdr = HostRoot::new(form_list(&[s("CDR")?, *value]));
    loop_pattern_assign(*t, *cdr, bindings, assigns)
}

/// Parse one or more LOOP accumulation clauses joined by `and`
/// (`{collect|append|nconc|sum|count} EXPR`), appending the per-iteration
/// `setq` forms to `out` and lazily creating the shared accumulators. `into` is
/// not supported and bails. All list accumulators use the push-and-reverse
/// convention (`cons`/`revappend` now, `nreverse` in the result), so `collect`,
/// `append`, and `nconc` compose into one shared list.
#[allow(clippy::too_many_arguments)]
fn parse_loop_accumulations(
    forms: &[BlissVal],
    i: &mut usize,
    kw: &dyn Fn(BlissVal) -> Option<String>,
    bindings: &mut Vec<BlissVal>,
    _finalize: &mut Vec<BlissVal>,
    list_acc: &mut Option<BlissVal>,
    sum_acc: &mut Option<BlissVal>,
    count_acc: &mut Option<BlissVal>,
    nsym: &mut usize,
    id: u32,
    out: &mut Vec<BlissVal>,
) -> LowerResult<()> {
    loop {
        let op = kw(forms[*i]).ok_or(Bail)?;
        let expr = *forms.get(*i + 1).ok_or(Bail)?;
        *i += 2;
        if kw(*forms.get(*i).unwrap_or(&NIL)).as_deref() == Some("INTO") {
            return Err(Bail); // `into` not supported yet
        }
        apply_loop_accumulation(
            &op, expr, bindings, list_acc, sum_acc, count_acc, nsym, id, out,
        )?;
        if kw(*forms.get(*i).unwrap_or(&NIL)).as_deref() == Some("AND") {
            *i += 1;
            continue;
        }
        break;
    }
    Ok(())
}

/// Emit the per-iteration form(s) for a single LOOP accumulation clause
/// (`{collect|append|nconc|sum|count} EXPR`), lazily creating the shared
/// accumulator on first use. Bails on an unrecognised operator.
#[allow(clippy::too_many_arguments)]
/// Replace every symbol whose *bare name* is "IT" (so both `it` and the keyword
/// `:it` — LOOP matches the `when`/`if`/`unless` anaphor by name, CLHS 6.1.5)
/// with `repl`, everywhere in `form` except inside quoted data or a nested
/// `loop` (which rebinds its own `it`). Used to wire the anaphor to a fresh temp
/// bound to the conditional's test value.
fn subst_loop_it(form: BlissVal, repl: BlissVal) -> BlissVal {
    if form.is_symbol() && symbol_bare_name(&sym_name(form)) == "IT" {
        return repl;
    }
    if !form.is_cons() {
        return form;
    }
    let (car, cdr) = cp(form);
    if car.is_symbol() {
        let n = symbol_bare_name(&sym_name(car));
        if n == "QUOTE" || n == "LOOP" {
            return form;
        }
    }
    arena_cons(subst_loop_it(car, repl), subst_loop_it(cdr, repl))
}

fn apply_loop_accumulation(
    op: &str,
    expr: BlissVal,
    bindings: &mut Vec<BlissVal>,
    list_acc: &mut Option<BlissVal>,
    sum_acc: &mut Option<BlissVal>,
    count_acc: &mut Option<BlissVal>,
    nsym: &mut usize,
    id: u32,
    out: &mut Vec<BlissVal>,
) -> LowerResult<()> {
    let expr = HostRoot::new(expr);
    let s = |n: &str| resolve_sym(n).ok_or(Bail);
    match op {
        "COLLECT" | "COLLECTING" | "APPEND" | "APPENDING" | "NCONC" | "NCONCING" => {
            if list_acc.is_none() {
                *nsym += 1;
                let acc = resolve_sym(&format!("%LG-ACC{id}_{}", *nsym)).ok_or(Bail)?;
                bindings.push(form_list(&[acc, NIL]));
                *list_acc = Some(acc);
            }
            let acc = list_acc.unwrap();
            let pushed = if op.starts_with("COLLECT") {
                form_list(&[s("CONS")?, *expr, acc])
            } else {
                form_list(&[s("REVAPPEND")?, *expr, acc])
            };
            let pushed = HostRoot::new(pushed);
            out.push(form_list(&[s("SETQ")?, acc, *pushed]));
        }
        "SUM" | "SUMMING" => {
            if sum_acc.is_none() {
                *nsym += 1;
                let acc = resolve_sym(&format!("%LG-SUM{id}_{}", *nsym)).ok_or(Bail)?;
                bindings.push(form_list(&[acc, BlissVal::from_fixnum(0)]));
                *sum_acc = Some(acc);
            }
            let acc = sum_acc.unwrap();
            let sum = HostRoot::new(form_list(&[s("+")?, acc, *expr]));
            out.push(form_list(&[s("SETQ")?, acc, *sum]));
        }
        "COUNT" | "COUNTING" => {
            if count_acc.is_none() {
                *nsym += 1;
                let acc = resolve_sym(&format!("%LG-CNT{id}_{}", *nsym)).ok_or(Bail)?;
                bindings.push(form_list(&[acc, BlissVal::from_fixnum(0)]));
                *count_acc = Some(acc);
            }
            let acc = count_acc.unwrap();
            let inc = HostRoot::new(form_list(&[s("1+")?, acc]));
            let setq = HostRoot::new(form_list(&[s("SETQ")?, acc, *inc]));
            out.push(form_list(&[
                s("WHEN")?,
                *expr,
                *setq,
            ]));
        }
        _ => return Err(Bail),
    }
    Ok(())
}

/// Parse the selectable clause body of a LOOP conditional (`when`/`if`/`unless`)
/// — the forms after the test (and after `else`). Handles `do`/`doing`,
/// `return`, the accumulation clauses, and chaining with `and`; stops at `else`,
/// `end`, or any other clause keyword (or end of input) without consuming it.
/// Sets `explicit_result` when a `return` clause is seen. Bails on an
/// unrecognised leading operator so the whole LOOP falls back.
#[allow(clippy::too_many_arguments)]
fn parse_loop_selectable(
    forms: &[BlissVal],
    i: &mut usize,
    kw: &dyn Fn(BlissVal) -> Option<String>,
    bindings: &mut Vec<BlissVal>,
    list_acc: &mut Option<BlissVal>,
    sum_acc: &mut Option<BlissVal>,
    count_acc: &mut Option<BlissVal>,
    nsym: &mut usize,
    id: u32,
    out: &mut Vec<BlissVal>,
    explicit_result: &mut bool,
) -> LowerResult<()> {
    let s = |n: &str| resolve_sym(n).ok_or(Bail);
    loop {
        let op = kw(*forms.get(*i).ok_or(Bail)?).ok_or(Bail)?;
        match op.as_str() {
            "DO" | "DOING" => {
                *i += 1;
                while *i < forms.len() && kw(forms[*i]).is_none() {
                    out.push(forms[*i]);
                    *i += 1;
                }
            }
            "RETURN" => {
                let e = *forms.get(*i + 1).ok_or(Bail)?;
                out.push(form_list(&[s("RETURN")?, e]));
                *explicit_result = true;
                *i += 2;
            }
            "COLLECT" | "COLLECTING" | "APPEND" | "APPENDING" | "NCONC" | "NCONCING" | "SUM"
            | "SUMMING" | "COUNT" | "COUNTING" => {
                let expr = *forms.get(*i + 1).ok_or(Bail)?;
                *i += 2;
                if kw(*forms.get(*i).unwrap_or(&NIL)).as_deref() == Some("INTO") {
                    return Err(Bail); // `into` not supported yet
                }
                apply_loop_accumulation(
                    &op, expr, bindings, list_acc, sum_acc, count_acc, nsym, id, out,
                )?;
            }
            _ => return Err(Bail),
        }
        if kw(*forms.get(*i).unwrap_or(&NIL)).as_deref() == Some("AND") {
            *i += 1;
            continue;
        }
        break;
    }
    Ok(())
}

/// True if `body` (a list of forms) syntactically contains a `MACROLET` or
/// `SYMBOL-MACROLET` anywhere (outside quoted data). Used to decide whether the
/// body must be macro-expanded before capture analysis: a local macro can
/// introduce references to enclosing lexicals that are invisible in the
/// unexpanded source (e.g. alexandria's `gaussian-random`).
fn body_uses_macrolet(body: BlissVal) -> bool {
    fn walk(form: BlissVal) -> bool {
        if !form.is_cons() {
            return false;
        }
        let (car, cdr) = cp(form);
        if car.is_symbol() {
            let n = symbol_bare_name(&sym_name(car));
            if n == "MACROLET" || n == "SYMBOL-MACROLET" {
                return true;
            }
            if n == "QUOTE" {
                return false;
            }
        }
        walk(car) || walk(cdr)
    }
    list_to_vec(body).into_iter().any(walk)
}

/// True if `body` (a list of forms) syntactically uses `RETURN-FROM` anywhere
/// (outside quoted data). A conservative over-approximation used to decide
/// whether a function needs its implicit block established.
fn body_uses_return_from(body: BlissVal) -> bool {
    fn walk(form: BlissVal) -> bool {
        if !form.is_cons() {
            return false;
        }
        let (car, cdr) = cp(form);
        if car.is_symbol() {
            let n = symbol_bare_name(&sym_name(car));
            if n == "RETURN-FROM" {
                return true;
            }
            if n == "QUOTE" {
                return false;
            }
        }
        walk(car) || walk(cdr)
    }
    list_to_vec(body).into_iter().any(walk)
}

/// Append `let*` bindings that destructure `pattern` against the value held in
/// the symbol `source`, using `nth`/`nthcdr`. Handles required, `&optional`
/// (with defaults), `&rest`/`&body`, a dotted tail, and nested list
/// sub-patterns (recursively, each through a fresh temporary). `&key`, `&aux`,
/// `&whole`, and `&environment` bail. Positions are indexed from `source`, so
/// the emitted bindings are order-independent and safe in a `let*`.
fn destructure_pattern(
    pattern: BlissVal,
    source: BlissVal,
    bindings: &mut Vec<BlissVal>,
) -> LowerResult<()> {
    let nth_sym = resolve_sym("NTH").ok_or(Bail)?;
    let nthcdr_sym = resolve_sym("NTHCDR").ok_or(Bail)?;
    let if_sym = resolve_sym("IF").ok_or(Bail)?;
    let nth_at = |i: i64| form_list(&[nth_sym, BlissVal::from_fixnum(i), source]);
    let nthcdr_at = |i: i64| form_list(&[nthcdr_sym, BlissVal::from_fixnum(i), source]);

    let mut cur = pattern;
    let mut index: i64 = 0;
    // 0 = required, 1 = &optional, 2 = &rest/&body
    let mut mode = 0u8;
    let mut rest_bound = false;
    loop {
        if cur.is_nil() {
            break;
        }
        if cur.is_symbol() {
            // A dotted tail binds the remaining cdr (e.g. `(a . rest)`).
            bindings.push(form_list(&[cur, nthcdr_at(index)]));
            break;
        }
        if !cur.is_cons() {
            return Err(Bail);
        }
        let (elem, next) = cp(cur);
        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&OPTIONAL" => {
                    mode = 1;
                    cur = next;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = 2;
                    cur = next;
                    continue;
                }
                "&KEY" | "&AUX" | "&WHOLE" | "&ENVIRONMENT" => return Err(Bail),
                _ => {}
            }
        }
        match mode {
            0 => {
                if elem.is_symbol() {
                    bindings.push(form_list(&[elem, nth_at(index)]));
                } else if elem.is_cons() {
                    // Nested sub-pattern: bind a fresh temporary to this position,
                    // then destructure the sub-pattern against it.
                    let temp_name = next_control_token("__DBIND__").replace(':', "_");
                    let temp = resolve_sym(&temp_name).ok_or(Bail)?;
                    bindings.push(form_list(&[temp, nth_at(index)]));
                    destructure_pattern(elem, temp, bindings)?;
                } else {
                    return Err(Bail);
                }
                index += 1;
            }
            1 => {
                let (var, default) = if elem.is_symbol() {
                    (elem, NIL)
                } else if elem.is_cons() {
                    let (v, drest) = cp(elem);
                    if !v.is_symbol() {
                        return Err(Bail);
                    }
                    (v, if drest.is_cons() { cp(drest).0 } else { NIL })
                } else {
                    return Err(Bail);
                };
                let guarded = form_list(&[if_sym, nthcdr_at(index), nth_at(index), default]);
                bindings.push(form_list(&[var, guarded]));
                index += 1;
            }
            _ => {
                if rest_bound || !elem.is_symbol() {
                    return Err(Bail);
                }
                bindings.push(form_list(&[elem, nthcdr_at(index)]));
                rest_bound = true;
            }
        }
        cur = next;
    }
    Ok(())
}

/// Compile a `RESTART-CASE` clause as a nested function. `captures` names
/// bindings in the establishing activation; they are resolved through the
/// child function's parent heap environment rather than copied as source.
fn compile_restart_clause(
    params_form: BlissVal,
    body: BlissVal,
    env: &Env,
    captures: &std::collections::HashSet<String>,
    portable: bool,
) -> Option<BytecodeFunction> {
    let (param_names, min_args, max_args, variadic) = parse_lambda_list(params_form)?;
    let mut lo = Lowerer::new(env);
    let _const_guard = LowererConstGuard::new(&mut lo.constants);
    lo.portable = portable;
    lo.captured_names = compute_captured_names(body);
    if !captures.is_empty() {
        lo.has_env = true;
        for name in captures {
            lo.scopes[0].insert(name.clone(), VarLoc::Boxed);
        }
    }
    let mut param_layout = Vec::with_capacity(param_names.len());
    for name in &param_names {
        let loc = lo.alloc_local(name);
        param_layout.push((name.clone(), loc));
    }
    let param_types = if variadic {
        vec![DeclaredType::Any; param_names.len()]
    } else {
        declared_parameter_types(body, &param_names)?
    };
    lower_body(&mut lo, body).ok()?;
    lo.emit(Instr::Return);
    Some(BytecodeFunction {
        code: lo.code,
        constants: lo.constants,
        handler_cases: lo.handler_cases,
        handler_binds: lo.handler_binds,
        names: lo.names,
        restart_cases: lo.restart_cases,
        nested_functions: lo.nested_functions,
        param_layout,
        param_types,
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: min_args,
        name: "<restart-clause>".to_string(),
        params_form: if variadic { params_form } else { NIL },
        min_args,
        max_args,
        variadic,
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

/// True if `form` references one of `names` as a *function value* via
/// `(function name)` / `#'name` anywhere within it. The bytecode backend lowers
/// a local `flet`/`labels` function to a `CallNamed` on a private gensym and has
/// no way to hand back a callable *value* for it, so a form that takes `#'localfn`
/// must bail to the tree-walker (which closes over it correctly).
fn references_local_fn_value(form: BlissVal, names: &std::collections::HashSet<String>) -> bool {
    if !form.is_cons() {
        return false;
    }
    let (head, rest) = cp(form);
    if head.is_symbol() && sym_name(head) == "FUNCTION" && rest.is_cons() {
        let (arg, _) = cp(rest);
        if arg.is_symbol() && names.contains(&sym_name(arg)) {
            return true;
        }
    }
    references_local_fn_value(head, names) || references_local_fn_value(rest, names)
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
            | "BLISS::QUASIQUOTE"
            | "BLISS::UNQUOTE"
            | "BLISS::UNQUOTE-SPLICING"
    )
}

/// The variable name(s) a single &optional/&aux spec binds: `var`, `(var)`,
/// `(var default)`, or `(var default supplied-p)`. Returns `(var, supplied_p?)`,
/// or `None` for a shape the binder can't handle (e.g. destructuring).
fn parse_opt_aux_spec(elem: BlissVal) -> Option<(String, Option<String>)> {
    if elem.is_symbol() {
        return Some((sym_name(elem), None));
    }
    if elem.is_cons() {
        let items = list_to_vec(elem);
        if items.is_empty() || !items[0].is_symbol() {
            return None;
        }
        let var = sym_name(items[0]);
        // items: [var], [var default], [var default supplied-p]
        let sp = items.get(2).filter(|v| v.is_symbol()).map(|v| sym_name(*v));
        return Some((var, sp));
    }
    None
}

/// The variable name(s) a single &key spec binds: `var`, `(var …)`, or
/// `((keyword var) …)`. Returns `(var, supplied_p?)`.
fn parse_key_spec(elem: BlissVal) -> Option<(String, Option<String>)> {
    if elem.is_symbol() {
        return Some((sym_name(elem), None));
    }
    if elem.is_cons() {
        let items = list_to_vec(elem);
        if items.is_empty() {
            return None;
        }
        // The name is either `var` or `(keyword var)`.
        let var = if items[0].is_symbol() {
            sym_name(items[0])
        } else if items[0].is_cons() {
            let kv = list_to_vec(items[0]);
            if kv.len() != 2 || !kv[1].is_symbol() {
                return None;
            }
            sym_name(kv[1])
        } else {
            return None;
        };
        let sp = items.get(2).filter(|v| v.is_symbol()).map(|v| sym_name(*v));
        return Some((var, sp));
    }
    None
}

/// Parse an ordinary lambda list into `(all-variable-names-in-order, min_args,
/// max_args, variadic)` (x5y.7). Every variable — required, &optional (+ its
/// supplied-p), &rest, &key (+ supplied-p), &aux — gets a name so the compiler
/// allocates it a slot; the *values* (including default-form evaluation) are
/// filled at call time by the tree-walker's `bind_lambda_list`. Returns `None`
/// for shapes not yet handled (destructuring, an unknown lambda-list keyword).
fn parse_lambda_list(params_form: BlissVal) -> Option<(Vec<String>, u16, Option<u16>, bool)> {
    #[derive(PartialEq, Clone, Copy)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }
    let mut names: Vec<String> = Vec::new();
    let mut n_required: u16 = 0;
    let mut n_optional: u16 = 0;
    let mut variadic = false;
    let mut unbounded = false; // &rest or &key ⇒ no upper arg bound
    let mut mode = Mode::Req;
    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        c = rest;
        if elem.is_symbol() {
            let n = sym_name(elem);
            match n.as_str() {
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    variadic = true;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    variadic = true;
                    unbounded = true;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    variadic = true;
                    unbounded = true;
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    variadic = true;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => continue,
                _ if n.starts_with('&') => return None, // unknown lambda-list keyword
                _ => {}
            }
        }
        match mode {
            Mode::Req => {
                if !elem.is_symbol() {
                    return None; // destructuring not supported
                }
                names.push(sym_name(elem));
                n_required += 1;
            }
            Mode::Opt => {
                let (var, sp) = parse_opt_aux_spec(elem)?;
                names.push(var);
                if let Some(sp) = sp {
                    names.push(sp);
                }
                n_optional += 1;
            }
            Mode::Rest => {
                if !elem.is_symbol() {
                    return None;
                }
                names.push(sym_name(elem));
            }
            Mode::Key => {
                let (var, sp) = parse_key_spec(elem)?;
                names.push(var);
                if let Some(sp) = sp {
                    names.push(sp);
                }
            }
            Mode::Aux => {
                let (var, _) = parse_opt_aux_spec(elem)?;
                names.push(var);
            }
        }
    }
    let max_args = if unbounded {
        None
    } else {
        Some(n_required + n_optional)
    };
    Some((names, n_required, max_args, variadic))
}

/// Parse a macro lambda list, including nested destructuring patterns. The
/// resulting names are the bindings copied from the macro binder's lexical
/// frame into bytecode locals; defaults remain structural lambda-list metadata.
fn parse_macro_lambda_list(params_form: BlissVal) -> Option<(Vec<String>, u16, Option<u16>, bool)> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }

    fn has_keyword(form: BlissVal) -> bool {
        let mut current = form;
        while current.is_cons() {
            let (item, rest) = cp(current);
            if item.is_symbol() && sym_name(item).starts_with('&') {
                return true;
            }
            current = rest;
        }
        false
    }

    fn collect_pattern(pattern: BlissVal, names: &mut Vec<String>) -> Option<()> {
        if pattern.is_nil() {
            return Some(());
        }
        if pattern.is_symbol() {
            names.push(sym_name(pattern));
            return Some(());
        }
        if !pattern.is_cons() {
            return None;
        }
        if has_keyword(pattern) {
            names.extend(parse_macro_lambda_list(pattern)?.0);
            return Some(());
        }
        let (car, cdr) = cp(pattern);
        collect_pattern(car, names)?;
        collect_pattern(cdr, names)
    }

    let mut names = Vec::new();
    let mut required = 0u16;
    let mut optional = 0u16;
    let mut unbounded = false;
    let mut mode = Mode::Req;
    let mut current = params_form;
    while current.is_cons() {
        let (item, rest) = cp(current);
        current = rest;
        if item.is_symbol() {
            match sym_name(item).as_str() {
                "&WHOLE" | "&ENVIRONMENT" => {
                    if !current.is_cons() {
                        return None;
                    }
                    let (binding, tail) = cp(current);
                    collect_pattern(binding, &mut names)?;
                    current = tail;
                    continue;
                }
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    unbounded = true;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    unbounded = true;
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => continue,
                name if name.starts_with('&') => return None,
                _ => {}
            }
        }
        match mode {
            Mode::Req => {
                collect_pattern(item, &mut names)?;
                required = required.checked_add(1)?;
            }
            Mode::Opt => {
                let parts = list_to_vec(item);
                let pattern = if item.is_symbol() {
                    item
                } else {
                    *parts.first()?
                };
                collect_pattern(pattern, &mut names)?;
                if let Some(supplied) = parts.get(2).filter(|value| value.is_symbol()) {
                    names.push(sym_name(*supplied));
                }
                optional = optional.checked_add(1)?;
            }
            Mode::Rest => collect_pattern(item, &mut names)?,
            Mode::Key => {
                let parts = list_to_vec(item);
                let head = if item.is_symbol() {
                    item
                } else {
                    *parts.first()?
                };
                let pattern = if head.is_cons() {
                    *list_to_vec(head).get(1)?
                } else {
                    head
                };
                collect_pattern(pattern, &mut names)?;
                if let Some(supplied) = parts.get(2).filter(|value| value.is_symbol()) {
                    names.push(sym_name(*supplied));
                }
            }
            Mode::Aux => {
                let pattern = if item.is_symbol() {
                    item
                } else {
                    *list_to_vec(item).first()?
                };
                collect_pattern(pattern, &mut names)?;
            }
        }
    }
    Some((
        names,
        required,
        (!unbounded).then_some(required + optional),
        true,
    ))
}

fn primitive_declared_type(type_form: BlissVal) -> Option<DeclaredType> {
    if !type_form.is_symbol() {
        return None;
    }
    match symbol_bare_name(&sym_name(type_form)).as_str() {
        "FIXNUM" => Some(DeclaredType::Fixnum),
        "SINGLE-FLOAT" => Some(DeclaredType::SingleFloat),
        _ => None,
    }
}

/// Retain primitive parameter assertions from the declaration prefix of a
/// function body. Both `(type fixnum x)` and the standard shorthand
/// `(fixnum x)` are accepted. Unknown declaration/type specifiers remain
/// runtime no-ops for now and therefore contribute `Any`, never an unsound
/// compiler assumption.
fn declared_parameter_types(body: BlissVal, params: &[String]) -> Option<Vec<DeclaredType>> {
    let mut result = vec![DeclaredType::Any; params.len()];
    let mut forms = list_to_vec(body).into_iter();
    let mut next = forms.next();
    if next.is_some_and(BlissVal::is_string) {
        next = forms.next(); // optional function docstring precedes declarations
    }

    while let Some(form) = next {
        if !form.is_cons() {
            break;
        }
        let (op, declarations) = cp(form);
        if !op.is_symbol() || symbol_bare_name(&sym_name(op)) != "DECLARE" {
            break;
        }
        for declaration in list_to_vec(declarations) {
            if !declaration.is_cons() {
                continue;
            }
            let (head, tail) = cp(declaration);
            if !head.is_symbol() {
                continue;
            }
            let head_name = symbol_bare_name(&sym_name(head));
            let (declared, variables) = if head_name == "TYPE" {
                if !tail.is_cons() {
                    continue;
                }
                let (type_form, variables) = cp(tail);
                let Some(declared) = primitive_declared_type(type_form) else {
                    continue;
                };
                (declared, variables)
            } else {
                let Some(declared) = primitive_declared_type(head) else {
                    continue;
                };
                (declared, tail)
            };

            for variable in list_to_vec(variables) {
                if !variable.is_symbol() {
                    continue;
                }
                let variable_name = sym_name(variable);
                let Some(index) = params.iter().position(|param| {
                    param == &variable_name
                        || symbol_bare_name(param) == symbol_bare_name(&variable_name)
                }) else {
                    continue;
                };
                let old = result[index];
                if !old.is_any() && old != declared {
                    return None; // contradictory primitive assertions
                }
                result[index] = declared;
            }
        }
        next = forms.next();
    }
    Some(result)
}

/// Lower a fixed-arity function `(params . body)` to a [`BytecodeFunction`].
/// Returns `None` if the lambda list is non-trivial or the body uses a form
/// slice 1 does not yet handle.
fn compile_function(
    name: &str,
    params_form: BlissVal,
    body: BlissVal,
    env: &Env,
    portable: bool,
    macro_lambda_list: bool,
) -> Option<BytecodeFunction> {
    compile_function_in(
        name,
        params_form,
        body,
        env,
        portable,
        macro_lambda_list,
        &std::collections::HashSet::new(),
    )
}

/// As [`compile_function`], but with a set of enclosing block names the body may
/// non-locally `return-from` (used when compiling a lambda nested in another
/// function). Top-level definitions pass an empty set.
fn compile_function_in(
    name: &str,
    params_form: BlissVal,
    body: BlissVal,
    env: &Env,
    portable: bool,
    macro_lambda_list: bool,
    enclosing_blocks: &std::collections::HashSet<String>,
) -> Option<BytecodeFunction> {
    let roots = bliss_rt::ShadowRootScope::new();
    let params_form = roots.root(params_form);
    let body = roots.root(body);
    // Parse the lambda list. Fixed and variadic (&optional/&rest/&key/&aux) are
    // both supported (x5y.7); destructuring / unknown keywords still bail.
    let parsed = if macro_lambda_list {
        parse_macro_lambda_list(params_form.get())
    } else {
        parse_lambda_list(params_form.get())
    };
    let (param_names, min_args, max_args, variadic) = match parsed {
        Some(p) => p,
        None => {
            let _ = record_bail(|| "lambda-list:destructure".to_string());
            return None;
        }
    };

    // If the body contains a local `macrolet`/`symbol-macrolet`, fully expand it
    // up front (macroexpand_all installs the local definitions, expands their
    // uses, and strips the wrapper while preserving ordinary special forms). This
    // must happen BEFORE capture analysis and the implicit-block decision below:
    // a local macro can introduce references to enclosing lexicals (or a
    // `return-from`) that are invisible in the unexpanded source, so scanning the
    // raw body would fail to box a captured variable (alexandria's
    // `gaussian-random`).
    if body_uses_macrolet(body.get()) {
        let progn = arena_cons(resolve_sym("PROGN")?, body.get());
        let menv = super::macroexpand_environment_from_cli(env);
        match compiler_macroexpand::macroexpand_all(progn, &menv) {
            Ok(expanded) => body.set(arena_cons(expanded, NIL)),
            Err(_) => {
                let _ = record_bail(|| "macroexpand:macrolet".to_string());
                return None;
            }
        }
    }

    let mut lo = Lowerer::new(env);
    let _const_guard = LowererConstGuard::new(&mut lo.constants);
    lo.portable = portable;
    lo.captured_names = compute_captured_names(body.get());
    lo.enclosing_blocks = enclosing_blocks.clone();
    let mut param_layout = Vec::with_capacity(param_names.len());
    for pn in &param_names {
        let loc = lo.alloc_local(pn);
        param_layout.push((pn.clone(), loc));
    }
    let param_types = if variadic {
        vec![DeclaredType::Any; param_names.len()]
    } else {
        declared_parameter_types(body.get(), &param_names)?
    };

    // A named DEFUN/DEFMACRO body runs inside an implicit block named after the
    // function, so `(return-from NAME …)` exits it (directly or from a nested
    // closure). Only establish it when the body actually uses RETURN-FROM, to
    // avoid a per-call block handler for the common case. Lambdas ("<lambda>")
    // have no implicit block.
    if name != "<lambda>" && body_uses_return_from(body.get()) {
        if let Some(block_sym) = resolve_sym(name) {
            let block_op = resolve_sym("BLOCK")?;
            let mut items = vec![block_op, block_sym];
            items.extend(list_to_vec(body.get()));
            body.set(arena_cons(vec_to_list(&items), NIL));
        }
    }

    // Body as an implicit progn producing the return value.
    if lower_body(&mut lo, body.get()).is_err() {
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
        nested_functions: lo.nested_functions,
        param_layout,
        param_types,
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: min_args,
        name: name.to_string(),
        params_form: if variadic { params_form.get() } else { NIL },
        min_args,
        max_args,
        variadic,
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
            if n == "RESTART-CASE" {
                let (restartable, clauses) = cp(cdr);
                walk(restartable, out);
                for clause in list_to_vec(clauses) {
                    if !clause.is_cons() {
                        continue;
                    }
                    let (_, clause_rest) = cp(clause);
                    let (params_form, clause_body) = cp(clause_rest);
                    let params = list_to_vec(params_form)
                        .into_iter()
                        .filter(|param| param.is_symbol())
                        .map(sym_name)
                        .collect::<std::collections::HashSet<_>>();
                    let mut used = std::collections::HashSet::new();
                    collect_symbol_names(clause_body, &mut used);
                    for name in used {
                        if !params.contains(&name) {
                            out.insert(name);
                        }
                    }
                    for form in list_to_vec(clause_body) {
                        walk(form, out);
                    }
                }
                return;
            }
            if n == "FLET" || n == "LABELS" {
                // Enclosing lexicals referenced by a local function's body must
                // be boxed so a capturing flet/labels closure can reach them
                // through the shared heap frame. Over-approximate: a local body's
                // free symbols (minus its own params) that name an enclosing
                // local get boxed; unrelated globals simply never match a local.
                let (defs_form, flet_body) = cp(cdr);
                for def in list_to_vec(defs_form) {
                    if !def.is_cons() {
                        continue;
                    }
                    let (_name, def_rest) = cp(def);
                    let (params_form, fbody) = cp(def_rest);
                    let params = list_to_vec(params_form)
                        .into_iter()
                        .filter(|param| param.is_symbol())
                        .map(sym_name)
                        .collect::<std::collections::HashSet<_>>();
                    let mut used = std::collections::HashSet::new();
                    collect_symbol_names(fbody, &mut used);
                    for name in used {
                        if !params.contains(&name) {
                            out.insert(name);
                        }
                    }
                    for form in list_to_vec(fbody) {
                        walk(form, out);
                    }
                }
                for form in list_to_vec(flet_body) {
                    walk(form, out);
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
    let forms = bliss_rt::gc::HostRoot::new(list_to_vec(body));
    if forms.is_empty() {
        let c = lo.add_const(NIL);
        lo.emit(Instr::Const(c));
        lo.push_n(1);
        return Ok(());
    }
    let n = forms.len();
    for i in 0..forms.len() {
        lo.lower_expr(forms[i])?;
        if i + 1 < n {
            lo.emit(Instr::Pop);
            lo.pop_n(1);
        }
    }
    Ok(())
}

/// Compile a top-level form as a zero-argument thunk. Returns `None` on bail.
fn compile_thunk(form: BlissVal, env: &Env, portable: bool) -> Option<BytecodeFunction> {
    let roots = bliss_rt::ShadowRootScope::new();
    let form = roots.root(form);
    let mut lo = Lowerer::new(env);
    let _const_guard = LowererConstGuard::new(&mut lo.constants);
    lo.portable = portable;
    lo.captured_names = compute_captured_names(arena_cons(form.get(), NIL));
    if lo.lower_expr(form.get()).is_err() {
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
        nested_functions: lo.nested_functions,
        param_layout: Vec::new(),
        param_types: Vec::new(),
        has_env: lo.has_env,
        n_locals: lo.n_locals,
        max_stack: lo.max_stack.max(1),
        arity: 0,
        name: "<toplevel>".to_string(),
        params_form: NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    })
}

// ── BFASL Bytecode Unit serialization ─────────────────────────────

const BBU_MAGIC: &[u8; 4] = b"BBU\0";
// 0x0104 (bliss-jtc.23.4): adds source-free nested restart-clause functions,
// restart-table metadata, and captured-environment variable bytecodes. The
// reader retains the
// provisional v1.1 tag-14 registry-key symbol encoding for compatibility.
// 0x0105 (bliss-jtc.23.3): adds the MAKE_CLOSURE opcode (0x0b) for source-free
// noncapturing nested lambdas. The referenced nested function is serialized
// before its owner (backwards/topological), so the reference embedded in the
// instruction needs no auxiliary table.
// 0x0106 (bliss-1ja): MAKE_CLOSURE gains an env-capture indicator (capture
// count 1) for capturing flet/labels locals and capturing lambdas; the closure
// records the creating activation's heap EnvFrame.
// 0x0107: source-free handler-case — the handler-case clause tables (type name,
// clause body PC, var slot) serialize in a new auxiliary table (kind 9).
// A BBU is authoritative: an unsupported version is rejected, never replaced
// by executing source text from the container.
const BBU_BYTECODE_VERSION: u16 = 0x0107;
const BBU_VERIFIER_VERSION: u16 = 0x0100;
const BBU_NO_INDEX: u32 = u32::MAX;
/// Unit-flags bit: every load form is represented in `load_actions`, so the
/// loader may execute the bytecode unit in place of the source (bliss-jtc.23).
const BBU_UNIT_COMPLETE: u32 = 1 << 0;

const BBU_FUNC_NAMED: u32 = 1 << 0;
const BBU_FUNC_MACRO: u32 = 1 << 1;
const BBU_FUNC_COMPILER_MACRO: u32 = 1 << 2;
const BBU_FUNC_LOAD_TIME_THUNK: u32 = 1 << 3;
const BBU_FUNC_NESTED: u32 = 1 << 4;
const BBU_AUX_FUNCTION_METADATA: u16 = 6;
const BBU_AUX_RESTART_TABLES: u16 = 7;
const BBU_AUX_HANDLER_CASES: u16 = 9;

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

    fn package(&mut self, name: &str, nicknames: &[String]) -> u32 {
        let name_ref = self.string(name);
        let nickname_refs = nicknames
            .iter()
            .map(|nickname| self.string(nickname))
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 10);
        put_u32(&mut bytes, name_ref);
        put_u32(&mut bytes, nickname_refs.len() as u32);
        for nickname_ref in nickname_refs {
            put_u32(&mut bytes, nickname_ref);
        }
        self.intern_encoded(bytes)
    }

    fn vector(&mut self, refs: &[u32]) -> u32 {
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 14);
        put_u32(&mut bytes, refs.len() as u32);
        for &reference in refs {
            put_u32(&mut bytes, reference);
        }
        self.intern_encoded(bytes)
    }

    /// Faithful symbol reference (bliss-jtc.23.1). Package and bare-name are
    /// structural references, while `kind` preserves the exact registry-key
    /// separator (internal `::`, external `:`, or unqualified). Keywords use
    /// their dedicated tag. Returns `None` for genuinely uninterned symbols.
    fn symbol_by_index(&mut self, idx: u32) -> Option<u32> {
        let key = bliss_rt::symbols::registry_key(idx)?;
        if let Some(name) = key.strip_prefix("KEYWORD:") {
            let name_ref = self.string(name);
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 12);
            put_u32(&mut bytes, name_ref);
            return Some(self.intern_encoded(bytes));
        }
        let (package, name, kind) = if let Some((package, name)) = key.rsplit_once("::") {
            (Some(package), name, 0u8)
        } else if let Some((package, name)) = key.rsplit_once(':') {
            (Some(package), name, 3u8)
        } else {
            (None, key.as_str(), 0u8)
        };
        let package_ref = package
            .map(|package| self.package(package, &[]))
            .unwrap_or(BBU_NO_INDEX);
        let name_ref = self.string(name);
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 11);
        put_u32(&mut bytes, package_ref);
        put_u32(&mut bytes, name_ref);
        put_u8(&mut bytes, kind);
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
            let idx = v.as_symbol_index();
            if let Some(r) = self.symbol_by_index(idx) {
                return Some(r);
            }
            // An uninterned / homeless symbol (no registry key), e.g. a gensym
            // embedded in macro output or a symbol whose home package is NIL.
            // Encode by name plus the process-local index: the index keeps
            // distinct symbols distinct (so the pool does not merge two same-named
            // gensyms), while load reconstructs a fresh uninterned symbol.
            let name = bliss_rt::symbols::symbol_name(idx)?;
            let name_ref = self.string(&name);
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 16);
            put_u32(&mut bytes, name_ref);
            put_u32(&mut bytes, idx);
            return Some(self.intern_encoded(bytes));
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
        // A ratio (e.g. `1/2`): pool its numerator and denominator (fixnums or
        // bignums, themselves poolable) and reconstruct the RATIO heap object on
        // load. Bignum parts that are not poolable make the whole ratio bail.
        if let Some((num, den)) = super::ratio_parts_val(v) {
            let num_ref = self.value(num)?;
            let den_ref = self.value(den)?;
            let mut bytes = Vec::new();
            put_u8(&mut bytes, 17);
            put_u32(&mut bytes, num_ref);
            put_u32(&mut bytes, den_ref);
            return Some(self.intern_encoded(bytes));
        }
        None
    }
}

struct BbuFunction {
    name_ref: u32,
    lambda_list_ref: u32,
    flags: u32,
    arity_min: u16,
    arity_max: u16,
    n_locals: u16,
    max_stack: u16,
    code: Vec<u8>,
    literal_refs: Vec<u32>,
    param_layout: Vec<(u32, VarLoc, DeclaredType)>,
    has_env: bool,
    variadic: bool,
    restart_tables: Vec<Vec<(u32, u32)>>,
    // Per `handler-case` form: its clauses as (type-name string ref, clause body
    // byte offset, var slot or NO_INDEX).
    handler_cases: Vec<Vec<(u32, u32, u32)>>,
}

fn bbu_instr_len(i: &Instr) -> Option<usize> {
    Some(match i {
        Instr::Const(_) => 5,
        Instr::LoadLocal(_) | Instr::StoreLocal(_) => 3,
        Instr::LoadGlobal(_) | Instr::StoreGlobal(_) => 5,
        Instr::LoadFunction(_) => 5,
        Instr::BindSpecial(_) => 5,
        Instr::UnbindSpecial(_) => 3,
        Instr::SetValues(_) => 3,
        Instr::ClearMv => 1,
        Instr::TakeValuesToLocals { .. } => 5,
        Instr::ValuesToList => 1,
        Instr::EvalHost(_) => return None,
        Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_) | Instr::DefineEnvVar(_) => 5,
        Instr::PushEnvChild | Instr::PopEnvChild => 1,
        Instr::MakeClosureEnv(_) => return None,
        // opcode + u32 global function index + u16 capture indicator.
        Instr::MakeClosure { .. } => 7,
        Instr::AllocCons => 1,
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
        Instr::ReturnFromNamed { .. } => 5,
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

fn traced_symbol_by_index(pool: &mut BbuConstPool, sym: u32) -> Option<u32> {
    match pool.symbol_by_index(sym) {
        Some(r) => Some(r),
        None => {
            if std::env::var_os("BLISS_BFASL_FORM").is_some() {
                let name = bliss_rt::symbols::symbol_name(sym);
                eprintln!("[bfasl] serialize: non-poolable operand symbol {name:?} (idx {sym})");
            }
            None
        }
    }
}

fn serialize_bbu_function(
    bf: &BytecodeFunction,
    name_ref: u32,
    flags: u32,
    pool: &mut BbuConstPool,
    restart_tables: Vec<Vec<(u32, u32)>>,
    // Maps each local nested-function index (as referenced by `MakeClosure`) to
    // the global serialized-function index its record was assigned. Populated by
    // `serialize_bbu_function_tree`, which serializes children before the owner.
    nested_global: &[u32],
) -> Option<BbuFunction> {
    // handler-bind stores raw (unlowered) handler source forms, so it is not yet
    // source-free; reject it. handler-case clauses are fully structural
    // (type-name, body PC, var slot) and are serialized below.
    if !bf.handler_binds.is_empty() {
        return None;
    }
    let mut literal_refs = Vec::with_capacity(bf.constants.len());
    for &c in &bf.constants {
        match pool.value(c) {
            Some(r) => literal_refs.push(r),
            None => {
                if std::env::var_os("BLISS_BFASL_FORM").is_some() {
                    let desc = if c.is_symbol() {
                        format!("symbol {:?}", sym_name(c))
                    } else {
                        format!("{c:?}")
                    };
                    eprintln!("[bfasl] serialize: non-poolable constant {desc}");
                }
                return None;
            }
        }
    }

    let mut offsets = Vec::with_capacity(bf.code.len());
    let mut pc = 0u32;
    for instr in &bf.code {
        offsets.push(pc);
        let len = match bbu_instr_len(instr) {
            Some(len) => len,
            None => {
                if std::env::var_os("BLISS_BFASL_FORM").is_some() {
                    eprintln!("[bfasl] serialize: non-portable instr {instr:?}");
                }
                return None;
            }
        };
        pc = pc.checked_add(len as u32)?;
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
                let cp = traced_symbol_by_index(pool, *sym)?;
                put_u32(&mut code, cp);
            }
            Instr::StoreGlobal(sym) => {
                put_u8(&mut code, 0x08);
                let cp = traced_symbol_by_index(pool, *sym)?;
                put_u32(&mut code, cp);
            }
            Instr::LoadFunction(sym) => {
                put_u8(&mut code, 0x09);
                put_u32(&mut code, traced_symbol_by_index(pool, *sym)?);
            }
            Instr::BindSpecial(sym) => {
                put_u8(&mut code, 0x1c);
                put_u32(&mut code, traced_symbol_by_index(pool, *sym)?);
            }
            Instr::UnbindSpecial(count) => {
                put_u8(&mut code, 0x1d);
                put_u16(&mut code, *count);
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
            Instr::LoadEnvVar(name_idx) => {
                put_u8(&mut code, 0x3c);
                put_u32(&mut code, pool.string(bf.names.get(*name_idx as usize)?));
            }
            Instr::StoreEnvVar(name_idx) => {
                put_u8(&mut code, 0x3d);
                put_u32(&mut code, pool.string(bf.names.get(*name_idx as usize)?));
            }
            Instr::DefineEnvVar(name_idx) => {
                put_u8(&mut code, 0x3e);
                put_u32(&mut code, pool.string(bf.names.get(*name_idx as usize)?));
            }
            Instr::PushEnvChild => put_u8(&mut code, 0x36),
            Instr::PopEnvChild => put_u8(&mut code, 0x37),
            Instr::AllocCons => put_u8(&mut code, 0x30),
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
                let cp = traced_symbol_by_index(pool, *sym)?;
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
            Instr::ReturnFromNamed { name_idx } => {
                put_u8(&mut code, 0x3f);
                put_u32(&mut code, pool.string(bf.names.get(*name_idx as usize)?));
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
            Instr::MakeClosure { func, capture_env } => {
                put_u8(&mut code, 0x0b);
                // The referenced nested function was already serialized (child
                // before owner), so its global index is known and backwards.
                put_u32(&mut code, *nested_global.get(*func as usize)?);
                // Capture indicator: 0 = noncapturing, 1 = capture enclosing
                // env frame. Individual capture slots are not used (the whole
                // heap frame is captured), so no slot payload follows.
                put_u16(&mut code, u16::from(*capture_env));
            }
            Instr::EvalHost(_) | Instr::MakeClosureEnv(_) => {
                if std::env::var_os("BLISS_BFASL_FORM").is_some() {
                    eprintln!("[bfasl] serialize: non-portable instr {instr:?}");
                }
                return None;
            }
        }
    }

    let lambda_list_ref = if bf.variadic {
        pool.value(bf.params_form)?
    } else {
        BBU_NO_INDEX
    };
    let param_layout = bf
        .param_layout
        .iter()
        .enumerate()
        .map(|(index, (name, location))| {
            (
                pool.string(name),
                *location,
                bf.param_types
                    .get(index)
                    .copied()
                    .unwrap_or(DeclaredType::Any),
            )
        })
        .collect();
    Some(BbuFunction {
        name_ref,
        lambda_list_ref,
        flags,
        arity_min: bf.min_args,
        arity_max: bf.max_args.unwrap_or(u16::MAX),
        n_locals: bf.n_locals,
        max_stack: bf.max_stack.max(1),
        code,
        literal_refs,
        param_layout,
        has_env: bf.has_env,
        variadic: bf.variadic,
        restart_tables,
        handler_cases: {
            let mut tables = Vec::with_capacity(bf.handler_cases.len());
            for hc in &bf.handler_cases {
                let mut clauses = Vec::with_capacity(hc.clauses.len());
                for clause in &hc.clauses {
                    clauses.push((
                        pool.string(&clause.type_name),
                        bbu_pc(clause.body_bcp, &offsets, end_pc)?,
                        clause.var_slot.map(u32::from).unwrap_or(BBU_NO_INDEX),
                    ));
                }
                tables.push(clauses);
            }
            tables
        },
    })
}

/// Serialize nested restart clauses before their establishing function. The
/// resulting backwards-only references make reconstruction deterministic and
/// rule out cycles before any code is installed.
fn serialize_bbu_function_tree(
    bf: &BytecodeFunction,
    name_ref: u32,
    flags: u32,
    pool: &mut BbuConstPool,
    functions: &mut Vec<BbuFunction>,
) -> Option<u32> {
    let original_len = functions.len();
    let result = (|| {
        // Nested `MakeClosure` bodies: serialize each child first so its global
        // index is a backwards reference the owner can embed directly.
        let mut nested_global = Vec::with_capacity(bf.nested_functions.len());
        for nested in &bf.nested_functions {
            let child_index = serialize_bbu_function_tree(
                nested,
                BBU_NO_INDEX,
                BBU_FUNC_NESTED,
                pool,
                functions,
            )?;
            nested_global.push(child_index);
        }
        let mut restart_tables = Vec::with_capacity(bf.restart_cases.len());
        for restart_case in &bf.restart_cases {
            let mut entries = Vec::with_capacity(restart_case.restarts.len());
            for restart in &restart_case.restarts {
                let clause_index = serialize_bbu_function_tree(
                    &restart.function,
                    BBU_NO_INDEX,
                    BBU_FUNC_NESTED,
                    pool,
                    functions,
                )?;
                entries.push((pool.string(&restart.name), clause_index));
            }
            restart_tables.push(entries);
        }
        let function =
            serialize_bbu_function(bf, name_ref, flags, pool, restart_tables, &nested_global)?;
        let index = functions.len() as u32;
        functions.push(function);
        Some(index)
    })();
    if result.is_none() {
        functions.truncate(original_len);
    }
    result
}

fn serialize_bbu_function_record(out: &mut Vec<u8>, f: &BbuFunction) {
    put_u32(out, f.name_ref);
    put_u32(out, f.lambda_list_ref);
    put_u32(out, BBU_NO_INDEX);
    put_u32(out, f.flags);
    put_u16(out, f.arity_min);
    put_u16(out, f.arity_max);
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

fn serialize_bbu_function_metadata(functions: &[BbuFunction]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, functions.len() as u32);
    for function in functions {
        put_u8(&mut out, u8::from(function.has_env));
        put_u8(&mut out, u8::from(function.variadic));
        put_u16(&mut out, function.param_layout.len() as u16);
        for &(name_ref, location, declared_type) in &function.param_layout {
            put_u32(&mut out, name_ref);
            match location {
                VarLoc::Slot(slot) => {
                    put_u8(&mut out, 0);
                    put_u16(&mut out, slot);
                }
                VarLoc::Boxed => {
                    put_u8(&mut out, 1);
                    put_u16(&mut out, u16::MAX);
                }
            }
            put_u8(
                &mut out,
                match declared_type {
                    DeclaredType::Any => 0,
                    DeclaredType::Fixnum => 1,
                    DeclaredType::SingleFloat => 2,
                },
            );
        }
    }
    out
}

fn serialize_bbu_restart_tables(functions: &[BbuFunction]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, functions.len() as u32);
    for function in functions {
        put_u32(&mut out, function.restart_tables.len() as u32);
        for table in &function.restart_tables {
            put_u32(&mut out, table.len() as u32);
            for &(name_ref, function_ref) in table {
                put_u32(&mut out, name_ref);
                put_u32(&mut out, function_ref);
            }
        }
    }
    out
}

fn serialize_bbu_handler_cases(functions: &[BbuFunction]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, functions.len() as u32);
    for function in functions {
        put_u32(&mut out, function.handler_cases.len() as u32);
        for clauses in &function.handler_cases {
            put_u32(&mut out, clauses.len() as u32);
            for &(type_ref, body_offset, var_slot) in clauses {
                put_u32(&mut out, type_ref);
                put_u32(&mut out, body_offset);
                put_u32(&mut out, var_slot);
            }
        }
    }
    out
}

/// Rewrite definition-like top-level forms into portable load-time bytecode.
/// `None` means the form has no load-time action after compile-file has already
/// resolved its effect (currently IN-PACKAGE and an initializer-less DEFVAR).
fn portable_load_thunk_form(form: BlissVal) -> Option<BlissVal> {
    if !form.is_cons() {
        return Some(form);
    }
    let (op, rest) = cp(form);
    if !op.is_symbol() {
        return Some(form);
    }
    match symbol_bare_name(&sym_name(op)).as_str() {
        // Package context has already selected the package-qualified identities
        // serialized into subsequent symbol references. LOAD dynamically
        // restores *PACKAGE*, so there is no observable load action to retain.
        "IN-PACKAGE" => None,
        "DEFVAR" | "DEFPARAMETER" | "DEFCONSTANT" => {
            let kind = symbol_bare_name(&sym_name(op));
            let (var, init_and_doc) = cp(rest);
            if !var.is_symbol() {
                return Some(form);
            }
            if kind == "DEFVAR" && !init_and_doc.is_cons() {
                return None;
            }
            let init = if init_and_doc.is_cons() {
                cp(init_and_doc).0
            } else {
                NIL
            };
            let setq = form_list(&[resolve_sym("SETQ")?, var, init]);
            if kind == "DEFVAR" {
                let quoted = form_list(&[resolve_sym("QUOTE")?, var]);
                let boundp = form_list(&[resolve_sym("BOUNDP")?, quoted]);
                Some(form_list(&[resolve_sym("UNLESS")?, boundp, setq]))
            } else if kind == "DEFCONSTANT" {
                let quoted = form_list(&[resolve_sym("QUOTE")?, var]);
                let mark = form_list(&[resolve_sym("BLISS-INTERNAL::%MARK-CONSTANT")?, quoted]);
                Some(form_list(&[resolve_sym("PROGN")?, setq, mark]))
            } else {
                Some(setq)
            }
        }
        "DEFINE-CONDITION" => {
            let (name, after_name) = cp(rest);
            let (parents, after_parents) = cp(after_name);
            let (slots, options) = cp(after_parents);
            if !name.is_symbol() {
                return Some(form);
            }
            let quote = resolve_sym("QUOTE")?;
            let quoted = |value| form_list(&[quote, value]);
            Some(form_list(&[
                resolve_sym("BLISS-INTERNAL::%DEFINE-CONDITION-PORTABLE")?,
                quoted(name),
                quoted(parents),
                quoted(slots),
                quoted(options),
            ]))
        }
        _ => Some(form),
    }
}

#[derive(Debug)]
struct BbuPackagePlan {
    name: String,
    nicknames: Vec<String>,
    uses: Vec<String>,
    exports: Vec<String>,
    interns: Vec<String>,
}

fn bbu_package_designator(value: BlissVal) -> Option<String> {
    let raw = if value.is_symbol() {
        symbol_bare_name(&sym_name(value))
    } else if value.is_string() {
        val_as_str(value)
    } else {
        return None;
    };
    Some(
        raw.trim_start_matches("KEYWORD:")
            .trim_start_matches(':')
            .to_uppercase(),
    )
}

/// Parse the package-definition subset whose runtime effects have portable
/// load actions. Options that the tree-walker currently treats as metadata or
/// no-ops remain harmless; options requiring symbol import/re-export identity
/// are rejected until their dedicated action encoding lands.
fn bbu_package_plan(form: BlissVal) -> Result<Option<BbuPackagePlan>, BlissError> {
    if !form.is_cons() {
        return Ok(None);
    }
    let (op, rest) = cp(form);
    if !op.is_symbol()
        || !matches!(
            symbol_bare_name(&sym_name(op)).as_str(),
            "DEFPACKAGE" | "DEFINE-PACKAGE"
        )
    {
        return Ok(None);
    }
    if !rest.is_cons() {
        return Err(bbu_error("package definition has no package name"));
    }
    let (name_form, options) = cp(rest);
    let name = bbu_package_designator(name_form)
        .ok_or_else(|| bbu_error("package name is not a literal designator"))?;
    let mut plan = BbuPackagePlan {
        name,
        nicknames: Vec::new(),
        uses: Vec::new(),
        exports: Vec::new(),
        interns: Vec::new(),
    };
    for option in list_to_vec(options) {
        if !option.is_cons() {
            continue;
        }
        let (key, values) = cp(option);
        if !key.is_symbol() {
            continue;
        }
        let key = symbol_bare_name(&sym_name(key));
        let values = list_to_vec(values);
        let destination = match key.as_str() {
            "USE" | "MIX" => Some(&mut plan.uses),
            "NICKNAMES" => Some(&mut plan.nicknames),
            "EXPORT" => Some(&mut plan.exports),
            "INTERN" | "SHADOW" => Some(&mut plan.interns),
            "IMPORT-FROM"
            | "SHADOWING-IMPORT-FROM"
            | "REEXPORT"
            | "USE-REEXPORT"
            | "MIX-REEXPORT" => {
                return Err(bbu_error(format!(
                    "package option :{key} has no portable load action"
                )));
            }
            // These options are intentionally no-ops in eval_defpackage too.
            "RECYCLE" | "UNINTERN" | "DOCUMENTATION" | "LOCAL-NICKNAMES" => None,
            _ => None,
        };
        if let Some(destination) = destination {
            for value in values {
                let item = if matches!(key.as_str(), "EXPORT" | "INTERN" | "SHADOW") {
                    if !value.is_symbol() && !value.is_string() {
                        return Err(bbu_error(format!(
                            "package option :{key} contains a non-literal designator"
                        )));
                    }
                    symbol_bare_name(&val_as_str(value))
                } else {
                    bbu_package_designator(value).ok_or_else(|| {
                        bbu_error(format!(
                            "package option :{key} contains a non-literal designator"
                        ))
                    })?
                };
                if !destination.contains(&item) {
                    destination.push(item);
                }
            }
        }
    }
    Ok(Some(plan))
}

/// Build a complete, source-independent BFASL `BYTECODE_UNIT`. Every top-level
/// load action must be representable as portable bytecode; otherwise
/// COMPILE-FILE fails and emits no artifact.
pub fn build_bbu_from_forms(
    forms: &[BlissVal],
    src_path: &str,
    source: &str,
    env: &Env,
) -> Result<Vec<u8>, BlissError> {
    let mut pool = BbuConstPool::default();
    let source_file_ref = pool.string(src_path);
    let mut functions = Vec::new();
    let mut load_actions: Vec<(u8, u8, u32, u32, u32)> = Vec::new();

    for (form_index, &form) in forms.iter().enumerate() {
        let mut done = false;
        reset_last_bail_reason();

        // (0) Package creation is a deterministic load action, not an
        // executable source thunk. Its structural constants are validated in
        // full before the loader mutates the package registry.
        if let Some(plan) = bbu_package_plan(form)? {
            let package_ref = pool.package(&plan.name, &plan.nicknames);
            let use_refs = plan
                .uses
                .iter()
                .map(|name| pool.string(name))
                .collect::<Vec<_>>();
            let export_refs = plan
                .exports
                .iter()
                .map(|name| pool.string(name))
                .collect::<Vec<_>>();
            let uses_ref = pool.vector(&use_refs);
            let exports_ref = pool.vector(&export_refs);
            load_actions.push((1, 0, package_ref, uses_ref, exports_ref));
            for name in plan.interns {
                let name_ref = pool.string(&name);
                load_actions.push((2, 0, package_ref, name_ref, BBU_NO_INDEX));
            }
            done = true;
        }

        // (1) Macro expanders are ordinary portable bytecode functions with a
        // dedicated installation action. The macro body itself is never kept
        // as an executable source-form constant.
        if !done {
            if let Some((compiler_macro, name, params, body)) = as_macro_definition(form) {
                if let Some(sym) = symbol_index_of(&name) {
                    if let Some(name_ref) = pool.symbol_by_index(sym) {
                        if let Some(bf) = compile_function(&name, params, body, env, true, true) {
                            let function_flag = if compiler_macro {
                                BBU_FUNC_COMPILER_MACRO
                            } else {
                                BBU_FUNC_MACRO
                            };
                            if let Some(function_index) = serialize_bbu_function_tree(
                                &bf,
                                name_ref,
                                function_flag,
                                &mut pool,
                                &mut functions,
                            ) {
                                load_actions.push((
                                    if compiler_macro { 5 } else { 4 },
                                    0,
                                    function_index,
                                    name_ref,
                                    BBU_NO_INDEX,
                                ));
                                done = true;
                            }
                        }
                    }
                }
            }
        }

        // (2) A DEFUN whose name and body serialise faithfully → install the
        // precompiled function directly at load (skips read/macroexpand/compile).
        if let Some((name, params, body)) = as_defun(form) {
            if let Some(sym) = symbol_index_of(&name) {
                if let Some(name_ref) = pool.symbol_by_index(sym) {
                    if let Some(bf) = compile_function(&name, params, body, env, true, false) {
                        if let Some(function_index) = serialize_bbu_function_tree(
                            &bf,
                            name_ref,
                            BBU_FUNC_NAMED,
                            &mut pool,
                            &mut functions,
                        ) {
                            load_actions.push((3, 0, function_index, name_ref, BBU_NO_INDEX));
                            done = true;
                        }
                    }
                }
            }
        }

        // (2b) DEFINE-SETF-EXPANDER: the expander is a macro-like function of the
        // place's subforms; compile it to bytecode and install it as a setf
        // expander at load, exactly like a macro. (Without this the form would
        // fall through to the thunk path and be miscompiled as a call that
        // evaluates the accessor name as a variable.)
        if !done {
            if let Some((name, params, body)) = as_setf_expander_definition(form) {
                if let Some(sym) = symbol_index_of(&name) {
                    if let Some(name_ref) = pool.symbol_by_index(sym) {
                        if let Some(bf) = compile_function(&name, params, body, env, true, true) {
                            if let Some(function_index) = serialize_bbu_function_tree(
                                &bf,
                                name_ref,
                                BBU_FUNC_MACRO,
                                &mut pool,
                                &mut functions,
                            ) {
                                load_actions.push((8, 0, function_index, name_ref, BBU_NO_INDEX));
                                done = true;
                            }
                        }
                    }
                }
            }
        }

        // (3) Any form compilable to a serialisable thunk → run precompiled.
        if !done {
            match portable_load_thunk_form(form) {
                None => done = true,
                Some(thunk_form) => {
                    if let Some(bf) = compile_thunk(thunk_form, env, true) {
                        if let Some(function_index) = serialize_bbu_function_tree(
                            &bf,
                            BBU_NO_INDEX,
                            BBU_FUNC_LOAD_TIME_THUNK,
                            &mut pool,
                            &mut functions,
                        ) {
                            load_actions.push((7, 0, function_index, BBU_NO_INDEX, BBU_NO_INDEX));
                            done = true;
                        }
                    }
                }
            }
        }

        if !done {
            if std::env::var_os("BLISS_BFASL_FORM").is_some() {
                eprintln!(
                    "[bfasl] form {} failed (bail: {}): {}",
                    form_index + 1,
                    last_bail_reason().unwrap_or_else(|| "?".to_string()),
                    super::fmt_form_debug(form)
                );
            }
            return Err(BlissError::FileError(format!(
                "compile-file: top-level form {} cannot be represented as portable bytecode",
                form_index + 1
            )));
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(BBU_MAGIC);
    put_u16(&mut out, BBU_BYTECODE_VERSION);
    put_u16(&mut out, BBU_VERIFIER_VERSION);
    put_u32(&mut out, BBU_UNIT_COMPLETE);
    put_u32(&mut out, pool.entries.len() as u32);
    put_u32(&mut out, functions.len() as u32);
    put_u32(&mut out, load_actions.len() as u32);
    put_u32(&mut out, 3);
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
    let metadata = serialize_bbu_function_metadata(&functions);
    put_u16(&mut out, BBU_AUX_FUNCTION_METADATA);
    put_u16(&mut out, 0);
    put_u32(&mut out, metadata.len() as u32);
    out.extend_from_slice(&metadata);
    let restart_tables = serialize_bbu_restart_tables(&functions);
    put_u16(&mut out, BBU_AUX_RESTART_TABLES);
    put_u16(&mut out, 0);
    put_u32(&mut out, restart_tables.len() as u32);
    out.extend_from_slice(&restart_tables);
    let handler_cases = serialize_bbu_handler_cases(&functions);
    put_u16(&mut out, BBU_AUX_HANDLER_CASES);
    put_u16(&mut out, 0);
    put_u32(&mut out, handler_cases.len() as u32);
    out.extend_from_slice(&handler_cases);
    Ok(out)
}

#[derive(Clone, Debug)]
enum BbuConstant {
    Nil,
    T,
    Fixnum(i64),
    SingleFloat(u32),
    Character(u32),
    String(String),
    Package {
        name_ref: u32,
        nickname_refs: Vec<u32>,
    },
    Symbol {
        package_ref: u32,
        name_ref: u32,
        kind: u8,
    },
    Keyword(u32),
    /// Provisional v1.1 tag-14 encoding: one exact registry-key String ref.
    LegacySymbol(u32),
    Cons(u32, u32),
    Vector(Vec<u32>),
    /// An uninterned / homeless symbol: name string ref (the trailing
    /// process-local index disambiguates distinct symbols at serialize time and
    /// is not retained).
    UninternedSymbol(u32),
    /// A ratio: numerator and denominator constant refs (each an integer).
    Ratio(u32, u32),
}

#[derive(Clone, Debug)]
struct EncodedBbuFunction {
    name_ref: u32,
    lambda_list_ref: u32,
    flags: u32,
    arity_min: u16,
    arity_max: u16,
    n_locals: u16,
    max_stack: u16,
    code: Vec<u8>,
    literal_refs: Vec<u32>,
    metadata: Option<BbuFunctionMetadata>,
}

#[derive(Clone, Debug)]
struct BbuFunctionMetadata {
    has_env: bool,
    variadic: bool,
    params: Vec<(u32, VarLoc, DeclaredType)>,
}

#[derive(Clone, Copy, Debug)]
struct BbuLoadAction {
    kind: u8,
    flags: u8,
    arg0: u32,
    arg1: u32,
    arg2: u32,
}

struct BbuCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> BbuCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], BlissError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| bbu_error("truncated bytecode unit"))?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, BlissError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, BlissError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, BlissError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, BlissError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, BlissError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn bbu_error(message: impl Into<String>) -> BlissError {
    BlissError::FileError(format!("invalid BBU: {}", message.into()))
}

fn bbu_index(index: u32, len: usize, what: &str) -> Result<usize, BlissError> {
    let index = index as usize;
    if index >= len {
        return Err(bbu_error(format!("{what} index {index} out of bounds")));
    }
    Ok(index)
}

fn bbu_string(constants: &[BbuConstant], index: u32) -> Result<&str, BlissError> {
    match &constants[bbu_index(index, constants.len(), "constant")?] {
        BbuConstant::String(value) => Ok(value),
        _ => Err(bbu_error("expected string constant")),
    }
}

fn bbu_package<'a>(
    constants: &'a [BbuConstant],
    index: u32,
) -> Result<(&'a str, &'a [u32]), BlissError> {
    match &constants[bbu_index(index, constants.len(), "package constant")?] {
        BbuConstant::Package {
            name_ref,
            nickname_refs,
        } => Ok((bbu_string(constants, *name_ref)?, nickname_refs)),
        _ => Err(bbu_error("expected package constant")),
    }
}

fn bbu_vector(constants: &[BbuConstant], index: u32) -> Result<&[u32], BlissError> {
    match &constants[bbu_index(index, constants.len(), "vector constant")?] {
        BbuConstant::Vector(refs) => Ok(refs),
        _ => Err(bbu_error("expected vector constant")),
    }
}

fn bbu_is_symbol_constant(constant: Option<&BbuConstant>) -> bool {
    matches!(
        constant,
        Some(BbuConstant::Symbol { .. } | BbuConstant::LegacySymbol(_))
    )
}

fn parse_bbu_constant(
    cursor: &mut BbuCursor<'_>,
    bytecode_version: u16,
) -> Result<BbuConstant, BlissError> {
    Ok(match cursor.u8()? {
        0 => BbuConstant::Nil,
        1 => BbuConstant::T,
        2 => BbuConstant::Fixnum(cursor.i64()?),
        5 => BbuConstant::SingleFloat(cursor.u32()?),
        7 => BbuConstant::Character(cursor.u32()?),
        8 => {
            let len = cursor.u32()? as usize;
            let text = std::str::from_utf8(cursor.take(len)?)
                .map_err(|_| bbu_error("string constant is not UTF-8"))?;
            BbuConstant::String(text.to_string())
        }
        10 => {
            let name_ref = cursor.u32()?;
            let count = cursor.u32()? as usize;
            let mut nickname_refs = Vec::with_capacity(count);
            for _ in 0..count {
                nickname_refs.push(cursor.u32()?);
            }
            BbuConstant::Package {
                name_ref,
                nickname_refs,
            }
        }
        11 => BbuConstant::Symbol {
            package_ref: cursor.u32()?,
            name_ref: cursor.u32()?,
            kind: cursor.u8()?,
        },
        12 => BbuConstant::Keyword(cursor.u32()?),
        13 => BbuConstant::Cons(cursor.u32()?, cursor.u32()?),
        // BBU v1.1 temporarily used tag 14 for a registry-key symbol. v1.2
        // restores the specified Vector encoding while preserving old reads.
        14 if bytecode_version <= 0x0101 => BbuConstant::LegacySymbol(cursor.u32()?),
        14 => {
            let count = cursor.u32()? as usize;
            let mut refs = Vec::with_capacity(count);
            for _ in 0..count {
                refs.push(cursor.u32()?);
            }
            BbuConstant::Vector(refs)
        }
        16 => {
            let name_ref = cursor.u32()?;
            let _index = cursor.u32()?; // disambiguation only; not retained
            BbuConstant::UninternedSymbol(name_ref)
        }
        17 => {
            let num_ref = cursor.u32()?;
            let den_ref = cursor.u32()?;
            BbuConstant::Ratio(num_ref, den_ref)
        }
        tag => return Err(bbu_error(format!("unsupported constant tag {tag}"))),
    })
}

fn parse_bbu_function(cursor: &mut BbuCursor<'_>) -> Result<EncodedBbuFunction, BlissError> {
    let name_ref = cursor.u32()?;
    let lambda_list_ref = cursor.u32()?;
    let _doc_ref = cursor.u32()?;
    let flags = cursor.u32()?;
    let arity_min = cursor.u16()?;
    let arity_max = cursor.u16()?;
    let n_locals = cursor.u16()?;
    let max_stack = cursor.u16()?;
    let code_len = cursor.u32()? as usize;
    let code = cursor.take(code_len)?.to_vec();
    let literal_count = cursor.u32()? as usize;
    let mut literal_refs = Vec::with_capacity(literal_count);
    for _ in 0..literal_count {
        literal_refs.push(cursor.u32()?);
    }
    if cursor.u32()? != 0 {
        return Err(bbu_error("v1.1 handler tables are not supported"));
    }
    if cursor.u32()? != 0 {
        return Err(bbu_error("v1.1 pc-info tables are not supported"));
    }
    let _debug_ref = cursor.u32()?;
    if max_stack == 0 {
        return Err(bbu_error("function max_stack must be nonzero"));
    }
    Ok(EncodedBbuFunction {
        name_ref,
        lambda_list_ref,
        flags,
        arity_min,
        arity_max,
        n_locals,
        max_stack,
        code,
        literal_refs,
        metadata: None,
    })
}

fn parse_bbu_function_metadata(
    bytes: &[u8],
    function_count: usize,
) -> Result<Vec<BbuFunctionMetadata>, BlissError> {
    let mut cursor = BbuCursor::new(bytes);
    if cursor.u32()? as usize != function_count {
        return Err(bbu_error(
            "function-metadata count does not match function table",
        ));
    }
    let mut metadata = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let has_env = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(bbu_error("invalid has-env metadata flag")),
        };
        let variadic = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(bbu_error("invalid variadic metadata flag")),
        };
        let count = cursor.u16()? as usize;
        let mut params = Vec::with_capacity(count);
        for _ in 0..count {
            let name_ref = cursor.u32()?;
            let location_kind = cursor.u8()?;
            let slot = cursor.u16()?;
            let location = match location_kind {
                0 if slot != u16::MAX => VarLoc::Slot(slot),
                1 if slot == u16::MAX => VarLoc::Boxed,
                _ => return Err(bbu_error("invalid parameter location metadata")),
            };
            let declared_type = match cursor.u8()? {
                0 => DeclaredType::Any,
                1 => DeclaredType::Fixnum,
                2 => DeclaredType::SingleFloat,
                _ => return Err(bbu_error("invalid declared parameter type")),
            };
            params.push((name_ref, location, declared_type));
        }
        metadata.push(BbuFunctionMetadata {
            has_env,
            variadic,
            params,
        });
    }
    if !cursor.done() {
        return Err(bbu_error("trailing function-metadata bytes"));
    }
    Ok(metadata)
}

type BbuRestartTables = Vec<Vec<(u32, u32)>>;

fn parse_bbu_restart_tables(
    bytes: &[u8],
    function_count: usize,
) -> Result<Vec<BbuRestartTables>, BlissError> {
    let mut cursor = BbuCursor::new(bytes);
    if cursor.u32()? as usize != function_count {
        return Err(bbu_error(
            "restart-table function count does not match function table",
        ));
    }
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let table_count = cursor.u32()? as usize;
        let mut tables = Vec::with_capacity(table_count);
        for _ in 0..table_count {
            let entry_count = cursor.u32()? as usize;
            let mut entries = Vec::with_capacity(entry_count);
            for _ in 0..entry_count {
                entries.push((cursor.u32()?, cursor.u32()?));
            }
            tables.push(entries);
        }
        functions.push(tables);
    }
    if !cursor.done() {
        return Err(bbu_error("trailing restart-table metadata"));
    }
    Ok(functions)
}

/// Per function: a list of handler-case clause tables; each clause is
/// (type-name string ref, clause body byte offset, var slot or NO_INDEX).
type BbuHandlerCases = Vec<Vec<(u32, u32, u32)>>;

fn parse_bbu_handler_cases(
    bytes: &[u8],
    function_count: usize,
) -> Result<Vec<BbuHandlerCases>, BlissError> {
    let mut cursor = BbuCursor::new(bytes);
    if cursor.u32()? as usize != function_count {
        return Err(bbu_error(
            "handler-case function count does not match function table",
        ));
    }
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let table_count = cursor.u32()? as usize;
        let mut tables = Vec::with_capacity(table_count);
        for _ in 0..table_count {
            let clause_count = cursor.u32()? as usize;
            let mut clauses = Vec::with_capacity(clause_count);
            for _ in 0..clause_count {
                clauses.push((cursor.u32()?, cursor.u32()?, cursor.u32()?));
            }
            tables.push(clauses);
        }
        functions.push(tables);
    }
    if !cursor.done() {
        return Err(bbu_error("trailing handler-case metadata"));
    }
    Ok(functions)
}

fn materialize_bbu_constants(constants: &[BbuConstant]) -> Result<Vec<BlissVal>, BlissError> {
    let mut values = Vec::with_capacity(constants.len());
    for (index, constant) in constants.iter().enumerate() {
        let value = match constant {
            BbuConstant::Nil => NIL,
            BbuConstant::T => T,
            BbuConstant::Fixnum(value) => {
                if !(-(1_i64 << 60)..(1_i64 << 60)).contains(value) {
                    return Err(bbu_error(format!("fixnum constant out of range: {value}")));
                }
                BlissVal::from_fixnum(*value)
            }
            BbuConstant::SingleFloat(bits) => BlissVal::from_single_float(f32::from_bits(*bits)),
            BbuConstant::Character(code) => char::from_u32(*code)
                .map(BlissVal::from_char)
                .ok_or_else(|| bbu_error(format!("invalid character scalar {code}")))?,
            BbuConstant::String(value) => arena_str(value),
            BbuConstant::Package { name_ref, .. } => arena_str(bbu_string(constants, *name_ref)?),
            BbuConstant::Symbol {
                package_ref,
                name_ref,
                kind,
            } => {
                let name = bbu_string(constants, *name_ref)?;
                let key = match (*package_ref, *kind) {
                    (BBU_NO_INDEX, 0) => name.to_string(),
                    (BBU_NO_INDEX, 1 | 2) => {
                        values.push(bliss_rt::symbols::make_uninterned(name));
                        continue;
                    }
                    (BBU_NO_INDEX, _) => return Err(bbu_error("external symbol has no package")),
                    (package_ref, 0) => {
                        let (package, _) = bbu_package(constants, package_ref)?;
                        format!("{package}::{name}")
                    }
                    (package_ref, 3) => {
                        let (package, _) = bbu_package(constants, package_ref)?;
                        format!("{package}:{name}")
                    }
                    (_, kind) => return Err(bbu_error(format!("invalid symbol kind {kind}"))),
                };
                BlissVal::from_symbol_index(bliss_rt::symbols::intern(&key))
            }
            BbuConstant::Keyword(name_ref) => {
                let name = bbu_string(constants, *name_ref)?;
                BlissVal::from_symbol_index(bliss_rt::symbols::intern(&format!("KEYWORD:{name}")))
            }
            BbuConstant::LegacySymbol(name_ref) => {
                let key = bbu_string(constants, *name_ref)?;
                BlissVal::from_symbol_index(bliss_rt::symbols::intern(key))
            }
            BbuConstant::UninternedSymbol(name_ref) => {
                let name = bbu_string(constants, *name_ref)?;
                bliss_rt::symbols::make_uninterned(name)
            }
            BbuConstant::Ratio(num_ref, den_ref) => {
                // Numerator/denominator were emitted (and materialised) before the
                // ratio, so they are already present in `values`.
                let num = *values.get(*num_ref as usize).ok_or_else(|| {
                    bbu_error(format!("forward/cyclic ratio reference at {index}"))
                })?;
                let den = *values.get(*den_ref as usize).ok_or_else(|| {
                    bbu_error(format!("forward/cyclic ratio reference at {index}"))
                })?;
                super::alloc_ratio_cli(num, den)
            }
            BbuConstant::Cons(car_ref, cdr_ref) => {
                // The writer emits structural children before their parent.
                let car = *values.get(*car_ref as usize).ok_or_else(|| {
                    bbu_error(format!("forward/cyclic cons reference at {index}"))
                })?;
                let cdr = *values.get(*cdr_ref as usize).ok_or_else(|| {
                    bbu_error(format!("forward/cyclic cons reference at {index}"))
                })?;
                arena_cons(car, cdr)
            }
            BbuConstant::Vector(refs) => {
                let elements = refs
                    .iter()
                    .map(|reference| {
                        values.get(*reference as usize).copied().ok_or_else(|| {
                            bbu_error(format!("forward/cyclic vector reference at {index}"))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                bliss_stdlib::build_simple_vector(&elements)
            }
        };
        values.push(value);
    }
    Ok(values)
}

fn bbu_symbol(constants: &[BlissVal], index: u32) -> Result<u32, BlissError> {
    let value = constants[bbu_index(index, constants.len(), "symbol constant")?];
    if !value.is_symbol() || value.is_nil() || value == T {
        return Err(bbu_error("expected symbol constant"));
    }
    Ok(value.as_symbol_index())
}

fn bbu_string_from_values(constants: &[BlissVal], index: u32) -> Result<String, BlissError> {
    let value = constants[bbu_index(index, constants.len(), "string constant")?];
    if !value.is_string() {
        return Err(bbu_error("expected materialized string constant"));
    }
    Ok(val_as_str(value))
}

fn decode_bbu_function(
    encoded: &EncodedBbuFunction,
    constants: &[BlissVal],
    handler_case_table: &[Vec<(u32, u32, u32)>],
) -> Result<BytecodeFunction, BlissError> {
    let mut cursor = BbuCursor::new(&encoded.code);
    let mut starts = Vec::new();
    let mut code = Vec::new();
    let mut names = Vec::new();
    while !cursor.done() {
        starts.push(cursor.pos as u32);
        let instr = match cursor.u8()? {
            0x01 => {
                let literal = cursor.u32()?;
                if literal > u16::MAX as u32 || literal as usize >= encoded.literal_refs.len() {
                    return Err(bbu_error("literal operand out of bounds"));
                }
                Instr::Const(literal as u16)
            }
            0x02 => Instr::LoadLocal(cursor.u16()?),
            0x03 => Instr::StoreLocal(cursor.u16()?),
            0x07 => Instr::LoadGlobal(bbu_symbol(constants, cursor.u32()?)?),
            0x08 => Instr::StoreGlobal(bbu_symbol(constants, cursor.u32()?)?),
            0x09 => Instr::LoadFunction(bbu_symbol(constants, cursor.u32()?)?),
            0x1c => Instr::BindSpecial(bbu_symbol(constants, cursor.u32()?)?),
            0x1d => Instr::UnbindSpecial(cursor.u16()?),
            0x0d => Instr::CallNamed {
                sym: bbu_symbol(constants, cursor.u32()?)?,
                nargs: cursor.u16()?,
            },
            0x10 => {
                if cursor.u16()? != 1 {
                    return Err(bbu_error("v1.1 RETURN must return one value"));
                }
                Instr::Return
            }
            0x0b => {
                // The operand temporarily holds the *global* serialized-function
                // index; reconstruction rewrites it to the owner's local nested
                // index after validating the reference is backwards and nested.
                let global = cursor.u32()?;
                let captures = cursor.u16()?;
                if captures > 1 {
                    return Err(bbu_error("MakeClosure capture indicator must be 0 or 1"));
                }
                Instr::MakeClosure {
                    func: global,
                    capture_env: captures == 1,
                }
            }
            0x11 => Instr::Pop,
            0x12 => Instr::Dup,
            0x13 => Instr::SetValues(cursor.u16()?),
            0x14 => Instr::ClearMv,
            0x15 => Instr::TakeValuesToLocals {
                slot_base: cursor.u16()?,
                nvars: cursor.u16()?,
            },
            0x16 => Instr::ValuesToList,
            0x18 => Instr::Br(cursor.u32()?),
            0x19 => Instr::BrIfFalse(cursor.u32()?),
            0x1a => Instr::BrIfTrue(cursor.u32()?),
            0x1e => {
                let block_id = cursor.u32()?;
                let name_ref = cursor.u32()?;
                let name = if name_ref == BBU_NO_INDEX {
                    "NIL".to_string()
                } else {
                    val_as_str(constants[bbu_index(name_ref, constants.len(), "block name")?])
                };
                let name_idx = names.len();
                if name_idx > u16::MAX as usize {
                    return Err(bbu_error("too many block names"));
                }
                names.push(name);
                Instr::PushBlock {
                    block_id,
                    name_idx: name_idx as u16,
                    resume_bcp: cursor.u32()?,
                    sp_restore: cursor.u16()?,
                }
            }
            0x1f => Instr::ReturnFrom {
                block_id: cursor.u32()?,
            },
            0x3f => {
                let name = bbu_string_from_values(constants, cursor.u32()?)?;
                let index = names.len();
                if index > u16::MAX as usize {
                    return Err(bbu_error("too many block names"));
                }
                names.push(name);
                Instr::ReturnFromNamed {
                    name_idx: index as u16,
                }
            }
            0x20 => Instr::PushCatch {
                resume_bcp: cursor.u32()?,
                sp_restore: cursor.u16()?,
            },
            0x21 => Instr::Throw,
            0x22 => Instr::PushTag {
                tagbody_id: cursor.u32()?,
                sp_restore: cursor.u16()?,
            },
            0x23 => Instr::Go {
                tagbody_id: cursor.u32()?,
                target_bcp: cursor.u32()?,
            },
            0x24 => Instr::PushUnwind {
                cleanup_bcp: cursor.u32()?,
                sp_restore: cursor.u16()?,
            },
            0x25 => Instr::EnterCleanupNormal {
                cleanup_bcp: cursor.u32()?,
                resume_bcp: cursor.u32()?,
            },
            0x26 => Instr::CleanupReturn,
            0x27 => Instr::PopHandler,
            0x30 => Instr::AllocCons,
            0x28 => Instr::PushHandlerCase {
                hc: cursor.u32()?,
                sp_restore: cursor.u16()?,
            },
            0x29 => Instr::PopHandlerCase,
            0x2c => Instr::PushRestartCase {
                rc: cursor.u32()?,
                resume_bcp: cursor.u32()?,
                sp_restore: cursor.u16()?,
            },
            0x2d => Instr::PopRestartCase,
            0x36 => Instr::PushEnvChild,
            0x37 => Instr::PopEnvChild,
            0x3c | 0x3d | 0x3e => {
                let opcode = encoded.code[starts.last().copied().unwrap() as usize];
                let name = bbu_string_from_values(constants, cursor.u32()?)?;
                let index = names.len();
                if index > u16::MAX as usize {
                    return Err(bbu_error("too many environment names"));
                }
                names.push(name);
                match opcode {
                    0x3c => Instr::LoadEnvVar(index as u16),
                    0x3d => Instr::StoreEnvVar(index as u16),
                    0x3e => Instr::DefineEnvVar(index as u16),
                    _ => unreachable!(),
                }
            }
            opcode => return Err(bbu_error(format!("unknown opcode 0x{opcode:02x}"))),
        };
        code.push(instr);
    }

    let mut pc_to_bcp = HashMap::new();
    for (bcp, &pc) in starts.iter().enumerate() {
        pc_to_bcp.insert(pc, bcp as u32);
    }
    pc_to_bcp.insert(encoded.code.len() as u32, code.len() as u32);
    let map_pc = |pc: u32| {
        pc_to_bcp
            .get(&pc)
            .copied()
            .ok_or_else(|| bbu_error(format!("branch target {pc} is not an instruction boundary")))
    };
    for instr in &mut code {
        match instr {
            Instr::Br(target) | Instr::BrIfFalse(target) | Instr::BrIfTrue(target) => {
                *target = map_pc(*target)?;
            }
            Instr::PushCatch { resume_bcp, .. } | Instr::PushBlock { resume_bcp, .. } => {
                *resume_bcp = map_pc(*resume_bcp)?
            }
            Instr::PushRestartCase { resume_bcp, .. } => *resume_bcp = map_pc(*resume_bcp)?,
            Instr::Go { target_bcp, .. } => *target_bcp = map_pc(*target_bcp)?,
            Instr::PushUnwind { cleanup_bcp, .. } => *cleanup_bcp = map_pc(*cleanup_bcp)?,
            Instr::EnterCleanupNormal {
                cleanup_bcp,
                resume_bcp,
            } => {
                *cleanup_bcp = map_pc(*cleanup_bcp)?;
                *resume_bcp = map_pc(*resume_bcp)?;
            }
            _ => {}
        }
    }

    for instr in &code {
        match instr {
            Instr::LoadLocal(slot) | Instr::StoreLocal(slot) if *slot >= encoded.n_locals => {
                return Err(bbu_error("local-slot operand out of bounds"));
            }
            Instr::TakeValuesToLocals { nvars, slot_base }
                if slot_base.saturating_add(*nvars) > encoded.n_locals =>
            {
                return Err(bbu_error("multiple-value local range out of bounds"));
            }
            _ => {}
        }
    }

    // Reconstruct the handler-case clause tables, mapping each clause body byte
    // offset back to a bytecode index.
    let mut handler_cases = Vec::with_capacity(handler_case_table.len());
    for clauses in handler_case_table {
        let mut infos = Vec::with_capacity(clauses.len());
        for &(type_ref, body_offset, var_slot) in clauses {
            infos.push(ClauseInfo {
                type_name: bbu_string_from_values(constants, type_ref)?,
                body_bcp: map_pc(body_offset)?,
                var_slot: (var_slot != BBU_NO_INDEX)
                    .then(|| u16::try_from(var_slot).map_err(|_| bbu_error("var slot too large")))
                    .transpose()?,
            });
        }
        handler_cases.push(HandlerCaseInfo { clauses: infos });
    }

    let literal_values = encoded
        .literal_refs
        .iter()
        .map(|&reference| {
            constants
                .get(bbu_index(reference, constants.len(), "literal")?)
                .copied()
                .ok_or_else(|| bbu_error("literal index out of bounds"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let name = if encoded.name_ref == BBU_NO_INDEX {
        "<toplevel>".to_string()
    } else {
        sym_name(BlissVal::from_symbol_index(bbu_symbol(
            constants,
            encoded.name_ref,
        )?))
    };
    let (param_layout, param_types, has_env, variadic) = if let Some(metadata) = &encoded.metadata {
        let mut layout = Vec::with_capacity(metadata.params.len());
        let mut types = Vec::with_capacity(metadata.params.len());
        for &(name_ref, location, declared_type) in &metadata.params {
            layout.push((bbu_string_from_values(constants, name_ref)?, location));
            types.push(declared_type);
        }
        (layout, types, metadata.has_env, metadata.variadic)
    } else {
        let layout = (0..encoded.arity_min)
            .map(|slot| (format!("%ARG{slot}"), VarLoc::Slot(slot)))
            .collect::<Vec<_>>();
        let types = vec![DeclaredType::Any; layout.len()];
        (layout, types, false, false)
    };
    let params_form = if encoded.lambda_list_ref == BBU_NO_INDEX {
        NIL
    } else {
        constants[bbu_index(encoded.lambda_list_ref, constants.len(), "lambda-list")?]
    };
    Ok(BytecodeFunction {
        code,
        constants: literal_values,
        handler_cases,
        handler_binds: Vec::new(),
        names,
        restart_cases: Vec::new(),
        // Filled during reconstruction once `MakeClosure` global references are
        // validated and rewritten to local indices.
        nested_functions: Vec::new(),
        param_types,
        param_layout,
        has_env,
        n_locals: encoded.n_locals,
        max_stack: encoded.max_stack,
        arity: encoded.arity_min,
        name,
        params_form,
        min_args: encoded.arity_min,
        max_args: (encoded.arity_max != u16::MAX).then_some(encoded.arity_max),
        variadic,
    })
}

fn installed_lambda_list(arity: u16) -> BlissVal {
    let params = (0..arity)
        .map(|slot| {
            BlissVal::from_symbol_index(bliss_rt::symbols::intern(&format!(
                "BLISS-INTERNAL::%BFASL-ARG-{slot}"
            )))
        })
        .collect::<Vec<_>>();
    vec_to_list(&params)
}

thread_local! {
    /// Heap `EnvFrame` captured by an env-capturing `MakeClosure`, keyed by the
    /// closure's private symbol index. `run_with_binding` installs it as the
    /// closure activation's environment so its body reaches the enclosing
    /// lexicals. Entries are pruned when their closure symbol's bytecode is
    /// removed from the registry.
    static CLOSURE_ENV: RefCell<HashMap<u32, Rc<RefCell<EnvFrame>>>> =
        RefCell::new(HashMap::new());
}

/// The heap environment an env-capturing closure was created in, if any.
fn closure_captured_env(fn_val: BlissVal) -> Option<Rc<RefCell<EnvFrame>>> {
    if !fn_val.is_heap_object() || !bliss_rt::function::is_interpreted_function(fn_val) {
        return None;
    }
    let name = bliss_rt::function::name(fn_val);
    if !name.is_symbol() {
        return None;
    }
    CLOSURE_ENV.with(|m| m.borrow().get(&name.as_symbol_index()).cloned())
}

/// Materialize a callable value for a nested-lambda `MakeClosure`. Each
/// evaluation yields a distinct function identity: a fresh uninterned symbol
/// whose bytecode body is registered, wrapped in an interpreted-function object.
/// The tree-walker's `apply_function` then dispatches it through the ordinary
/// registered-bytecode path, so the closure is callable by
/// `funcall`/`apply`/`mapcar`/`reduce` exactly like any global. When
/// `captured_env` is `Some`, the closure records that heap frame so its body can
/// read and write the enclosing lexical bindings (portable `flet`/`labels`).
fn make_bytecode_closure(
    nested: &BytecodeFunction,
    captured_env: Option<Rc<RefCell<EnvFrame>>>,
) -> BlissVal {
    let sym = bliss_rt::symbols::make_uninterned("CLOSURE");
    let sym_idx = sym.as_symbol_index();
    let lambda_list = if nested.variadic {
        nested.params_form
    } else {
        installed_lambda_list(nested.arity)
    };
    registry_put(sym_idx, Rc::new(nested.clone()));
    if let Some(frame) = captured_env {
        CLOSURE_ENV.with(|m| m.borrow_mut().insert(sym_idx, frame));
    }
    bliss_rt::function::alloc_interpreted(lambda_list, NIL, NIL, sym)
}

fn bbu_direct_symbol_for_package(
    encoded: &[BbuConstant],
    values: &[BlissVal],
    package: BlissVal,
    bare_name: &str,
) -> Result<Option<(BlissVal, bool)>, BlissError> {
    for (index, constant) in encoded.iter().enumerate() {
        let BbuConstant::Symbol {
            package_ref,
            name_ref,
            kind,
        } = constant
        else {
            continue;
        };
        if *package_ref == BBU_NO_INDEX || bbu_string(encoded, *name_ref)? != bare_name {
            continue;
        }
        let (package_name, _) = bbu_package(encoded, *package_ref)?;
        if bliss_stdlib::find_package(package_name) == Some(package) {
            return Ok(Some((values[index], *kind == 3)));
        }
    }
    Ok(None)
}

fn execute_bbu_ensure_package(
    encoded: &[BbuConstant],
    values: &[BlissVal],
    action: BbuLoadAction,
) -> Result<BlissVal, BlissError> {
    let (name, nickname_refs) = bbu_package(encoded, action.arg0)?;
    let nicknames = nickname_refs
        .iter()
        .map(|&reference| bbu_string(encoded, reference).map(str::to_string))
        .collect::<Result<Vec<_>, _>>()?;
    let uses = bbu_vector(encoded, action.arg1)?
        .iter()
        .map(|&reference| bbu_string(encoded, reference).map(str::to_string))
        .collect::<Result<Vec<_>, _>>()?;
    let exports = bbu_vector(encoded, action.arg2)?
        .iter()
        .map(|&reference| bbu_string(encoded, reference).map(str::to_string))
        .collect::<Result<Vec<_>, _>>()?;

    // DEFINE-PACKAGE permits forward use-list references in the source loader;
    // create the same placeholder package before linking the target.
    for used in &uses {
        reader::register_package(used);
        if bliss_stdlib::find_package(used).is_none() {
            bliss_stdlib::make_package(used, &[], &[])?;
        }
    }
    reader::register_package(name);
    for nickname in &nicknames {
        reader::register_package(nickname);
    }
    let package = if let Some(package) = bliss_stdlib::find_package(name) {
        for nickname in &nicknames {
            bliss_stdlib::add_nickname(package, nickname)?;
        }
        for used in &uses {
            bliss_stdlib::use_package_by_name(package, used)?;
        }
        package
    } else {
        let nickname_refs = nicknames.iter().map(String::as_str).collect::<Vec<_>>();
        let use_refs = uses.iter().map(String::as_str).collect::<Vec<_>>();
        bliss_stdlib::make_package(name, &nickname_refs, &use_refs)?
    };

    // Export accessible inherited symbols without changing their identity. If
    // the name is new, prefer the already-materialized structural Symbol so
    // function/global references and package lookup remain EQ.
    for export_name in &exports {
        let symbol = bliss_stdlib::find_present_symbol(package, export_name)
            .or(bliss_stdlib::find_symbol(export_name, package)?.map(|entry| entry.0))
            .or(
                bbu_direct_symbol_for_package(encoded, values, package, export_name)?
                    .map(|entry| entry.0),
            )
            .unwrap_or(bliss_stdlib::intern(export_name, package)?.0);
        bliss_stdlib::add_symbol(package, export_name, symbol, true)?;
        bliss_rt::symbols::set_symbol_package(symbol.as_symbol_index(), package);
    }

    // Any remaining qualified symbols referenced by functions/thunks are
    // present internal symbols after load, even when not explicitly exported.
    for (index, constant) in encoded.iter().enumerate() {
        let BbuConstant::Symbol {
            package_ref,
            name_ref,
            kind,
        } = constant
        else {
            continue;
        };
        if *package_ref == BBU_NO_INDEX {
            continue;
        }
        let (symbol_package_name, _) = bbu_package(encoded, *package_ref)?;
        if bliss_stdlib::find_package(symbol_package_name) != Some(package) {
            continue;
        }
        let bare_name = bbu_string(encoded, *name_ref)?;
        if bliss_stdlib::find_symbol(bare_name, package)?.is_none() {
            bliss_stdlib::add_symbol(package, bare_name, values[index], *kind == 3)?;
            bliss_rt::symbols::set_symbol_package(values[index].as_symbol_index(), package);
        }
    }
    Ok(package)
}

/// Verify, decode, and execute an authoritative portable BBU. No source reader
/// or macroexpander participates in this path.
pub fn load_bbu(bytes: &[u8], env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut cursor = BbuCursor::new(bytes);
    if cursor.take(4)? != BBU_MAGIC {
        return Err(bbu_error("bad magic"));
    }
    let bytecode_version = cursor.u16()?;
    if bytecode_version >> 8 != BBU_BYTECODE_VERSION >> 8
        || (bytecode_version & 0xff) > (BBU_BYTECODE_VERSION & 0xff)
    {
        return Err(bbu_error(format!(
            "unsupported bytecode version {bytecode_version:#06x}"
        )));
    }
    let verifier_version = cursor.u16()?;
    if verifier_version != BBU_VERIFIER_VERSION {
        return Err(bbu_error(format!(
            "unsupported verifier version {verifier_version:#06x}"
        )));
    }
    let unit_flags = cursor.u32()?;
    if unit_flags & BBU_UNIT_COMPLETE == 0 {
        return Err(bbu_error("incomplete unit (source fallback is forbidden)"));
    }
    let constant_count = cursor.u32()? as usize;
    let function_count = cursor.u32()? as usize;
    let action_count = cursor.u32()? as usize;
    let aux_count = cursor.u32()? as usize;
    let source_file_ref = cursor.u32()?;
    let _expanded_hash = cursor.u64()?;
    let mut encoded_constants = Vec::with_capacity(constant_count);
    for _ in 0..constant_count {
        encoded_constants.push(parse_bbu_constant(&mut cursor, bytecode_version)?);
    }
    if source_file_ref != BBU_NO_INDEX {
        bbu_string(&encoded_constants, source_file_ref)?;
    }
    for constant in &encoded_constants {
        match constant {
            BbuConstant::Package {
                name_ref,
                nickname_refs,
            } => {
                bbu_string(&encoded_constants, *name_ref)?;
                for &nickname_ref in nickname_refs {
                    bbu_string(&encoded_constants, nickname_ref)?;
                }
            }
            BbuConstant::Symbol {
                package_ref,
                name_ref,
                kind,
            } => {
                bbu_string(&encoded_constants, *name_ref)?;
                match (*package_ref, *kind) {
                    (BBU_NO_INDEX, 0 | 1 | 2) => {}
                    (BBU_NO_INDEX, _) => {
                        return Err(bbu_error("external symbol has no package"));
                    }
                    (package_ref, 0 | 3) => {
                        bbu_package(&encoded_constants, package_ref)?;
                    }
                    (_, kind) => {
                        return Err(bbu_error(format!("invalid symbol kind {kind}")));
                    }
                }
            }
            BbuConstant::Keyword(name_ref) | BbuConstant::LegacySymbol(name_ref) => {
                bbu_string(&encoded_constants, *name_ref)?;
            }
            BbuConstant::Cons(car, cdr) => {
                bbu_index(*car, encoded_constants.len(), "cons car")?;
                bbu_index(*cdr, encoded_constants.len(), "cons cdr")?;
            }
            BbuConstant::Vector(refs) => {
                for &reference in refs {
                    bbu_index(reference, encoded_constants.len(), "vector element")?;
                }
            }
            _ => {}
        }
    }

    let mut encoded_functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        encoded_functions.push(parse_bbu_function(&mut cursor)?);
    }
    for function in &encoded_functions {
        let role_flags = function.flags
            & (BBU_FUNC_NAMED
                | BBU_FUNC_MACRO
                | BBU_FUNC_COMPILER_MACRO
                | BBU_FUNC_LOAD_TIME_THUNK
                | BBU_FUNC_NESTED);
        if function.flags
            & !(BBU_FUNC_NAMED
                | BBU_FUNC_MACRO
                | BBU_FUNC_COMPILER_MACRO
                | BBU_FUNC_LOAD_TIME_THUNK
                | BBU_FUNC_NESTED)
            != 0
            || role_flags.count_ones() != 1
        {
            return Err(bbu_error("invalid function flags"));
        }
        if function.name_ref != BBU_NO_INDEX {
            match encoded_constants.get(bbu_index(
                function.name_ref,
                encoded_constants.len(),
                "function name",
            )?) {
                constant if bbu_is_symbol_constant(constant) => {}
                _ => return Err(bbu_error("function name is not a symbol")),
            }
        }
        for &literal in &function.literal_refs {
            bbu_index(literal, encoded_constants.len(), "function literal")?;
        }
        if function.lambda_list_ref != BBU_NO_INDEX {
            bbu_index(
                function.lambda_list_ref,
                encoded_constants.len(),
                "lambda-list",
            )?;
        }
    }
    let mut actions = Vec::with_capacity(action_count);
    for _ in 0..action_count {
        actions.push(BbuLoadAction {
            kind: cursor.u8()?,
            flags: cursor.u8()?,
            arg0: cursor.u32()?,
            arg1: cursor.u32()?,
            arg2: cursor.u32()?,
        });
    }
    let mut function_metadata = None;
    let mut restart_tables = None;
    let mut handler_case_tables = None;
    for _ in 0..aux_count {
        let kind = cursor.u16()?;
        let flags = cursor.u16()?;
        let len = cursor.u32()? as usize;
        let bytes = cursor.take(len)?;
        if flags != 0 {
            return Err(bbu_error("unsupported auxiliary-table flags"));
        }
        if kind == BBU_AUX_FUNCTION_METADATA {
            if function_metadata.is_some() {
                return Err(bbu_error("duplicate function-metadata table"));
            }
            function_metadata = Some(parse_bbu_function_metadata(bytes, function_count)?);
        } else if kind == BBU_AUX_RESTART_TABLES {
            if restart_tables.is_some() {
                return Err(bbu_error("duplicate restart-table metadata"));
            }
            restart_tables = Some(parse_bbu_restart_tables(bytes, function_count)?);
        } else if kind == BBU_AUX_HANDLER_CASES {
            if handler_case_tables.is_some() {
                return Err(bbu_error("duplicate handler-case metadata"));
            }
            handler_case_tables = Some(parse_bbu_handler_cases(bytes, function_count)?);
        }
    }
    let handler_case_tables =
        handler_case_tables.unwrap_or_else(|| vec![Vec::new(); function_count]);
    if !cursor.done() {
        return Err(bbu_error("trailing bytes"));
    }
    if bytecode_version >= 0x0103 && function_metadata.is_none() {
        return Err(bbu_error("v1.3 unit is missing function metadata"));
    }
    if bytecode_version >= 0x0104 && restart_tables.is_none() {
        return Err(bbu_error("v1.4 unit is missing restart-table metadata"));
    }
    if let Some(metadata) = function_metadata {
        for (function, metadata) in encoded_functions.iter_mut().zip(metadata) {
            function.metadata = Some(metadata);
        }
    }
    for function in &encoded_functions {
        if let Some(metadata) = &function.metadata {
            if !metadata.variadic && function.arity_min != function.arity_max {
                return Err(bbu_error("fixed function has a non-fixed arity range"));
            }
            if metadata.variadic && function.lambda_list_ref == BBU_NO_INDEX {
                return Err(bbu_error("variadic function has no lambda-list metadata"));
            }
            for &(name_ref, location, _) in &metadata.params {
                bbu_string(&encoded_constants, name_ref)?;
                match location {
                    VarLoc::Slot(slot) if slot < function.n_locals => {}
                    VarLoc::Boxed if metadata.has_env => {}
                    _ => return Err(bbu_error("parameter location is out of bounds")),
                }
            }
        } else if function.arity_min != function.arity_max {
            return Err(bbu_error("legacy unit contains a variadic function"));
        }
    }

    let restart_tables =
        restart_tables.unwrap_or_else(|| vec![Vec::new(); encoded_functions.len()]);
    for (owner, tables) in restart_tables.iter().enumerate() {
        for table in tables {
            for &(name_ref, function_ref) in table {
                bbu_string(&encoded_constants, name_ref)?;
                let target = bbu_index(function_ref, encoded_functions.len(), "restart function")?;
                if target >= owner {
                    return Err(bbu_error(
                        "restart function reference is not backwards/topological",
                    ));
                }
                if encoded_functions[target].flags & BBU_FUNC_NESTED == 0 {
                    return Err(bbu_error("restart table references a non-nested function"));
                }
            }
        }
    }

    // Validate the complete load plan before mutating any function/value cell.
    for action in &actions {
        match action.kind {
            1 => {
                if action.flags != 0 {
                    return Err(bbu_error("EnsurePackage has unsupported flags"));
                }
                bbu_package(&encoded_constants, action.arg0)?;
                for &reference in bbu_vector(&encoded_constants, action.arg1)? {
                    bbu_string(&encoded_constants, reference)?;
                }
                for &reference in bbu_vector(&encoded_constants, action.arg2)? {
                    bbu_string(&encoded_constants, reference)?;
                }
            }
            2 => {
                if action.flags != 0 || action.arg2 != BBU_NO_INDEX {
                    return Err(bbu_error("InternSymbol has unsupported flags/arguments"));
                }
                bbu_package(&encoded_constants, action.arg0)?;
                bbu_string(&encoded_constants, action.arg1)?;
            }
            3 => {
                if action.flags != 0 || action.arg2 != BBU_NO_INDEX {
                    return Err(bbu_error("InstallFunction has unsupported flags/arguments"));
                }
                let function = &encoded_functions
                    [bbu_index(action.arg0, encoded_functions.len(), "function")?];
                if function.flags & BBU_FUNC_NAMED == 0 {
                    return Err(bbu_error("InstallFunction references a non-named function"));
                }
                match encoded_constants.get(bbu_index(
                    action.arg1,
                    encoded_constants.len(),
                    "function name",
                )?) {
                    constant if bbu_is_symbol_constant(constant) => {}
                    _ => return Err(bbu_error("InstallFunction name is not a symbol")),
                }
            }
            4 | 5 | 8 => {
                if action.flags != 0 || action.arg2 != BBU_NO_INDEX {
                    return Err(bbu_error("macro installation has unsupported arguments"));
                }
                let function = &encoded_functions
                    [bbu_index(action.arg0, encoded_functions.len(), "macro function")?];
                // A setf-expander (kind 8) and a macro (kind 4) both serialize as
                // BBU_FUNC_MACRO expander functions; a compiler macro (kind 5) as
                // BBU_FUNC_COMPILER_MACRO.
                let expected = if action.kind == 5 {
                    BBU_FUNC_COMPILER_MACRO
                } else {
                    BBU_FUNC_MACRO
                };
                if function.flags & expected == 0 {
                    return Err(bbu_error("macro action references the wrong function role"));
                }
                match encoded_constants.get(bbu_index(
                    action.arg1,
                    encoded_constants.len(),
                    "macro name",
                )?) {
                    constant if bbu_is_symbol_constant(constant) => {}
                    _ => return Err(bbu_error("macro name is not a symbol")),
                }
            }
            7 => {
                if action.flags != 0 || action.arg2 != BBU_NO_INDEX {
                    return Err(bbu_error("EvalThunk has unsupported flags/arguments"));
                }
                let function =
                    &encoded_functions[bbu_index(action.arg0, encoded_functions.len(), "thunk")?];
                if function.flags & BBU_FUNC_LOAD_TIME_THUNK == 0 {
                    return Err(bbu_error("EvalThunk references a non-thunk function"));
                }
                if action.arg1 != BBU_NO_INDEX {
                    return Err(bbu_error("EvalThunk has an unexpected argument"));
                }
            }
            kind => return Err(bbu_error(format!("unsupported load action {kind}"))),
        }
    }

    let constants = materialize_bbu_constants(&encoded_constants)?;
    let mut decoded_functions: Vec<BytecodeFunction> = Vec::with_capacity(encoded_functions.len());
    for (index, encoded) in encoded_functions.iter().enumerate() {
        let mut function =
            decode_bbu_function(encoded, &constants, &handler_case_tables[index])?;
        function.restart_cases = restart_tables[index]
            .iter()
            .map(|table| {
                let restarts = table
                    .iter()
                    .map(|&(name_ref, function_ref)| {
                        Ok(RestartClauseInfo {
                            name: bbu_string_from_values(&constants, name_ref)?,
                            function: Box::new(decoded_functions[function_ref as usize].clone()),
                        })
                    })
                    .collect::<Result<Vec<_>, BlissError>>()?;
                Ok(RestartCaseInfo { restarts })
            })
            .collect::<Result<Vec<_>, BlissError>>()?;
        // Resolve `MakeClosure` global references (embedded in the instruction)
        // to the owner's local nested-function list. Every reference must be
        // backwards (a lower global index — the child was serialized first),
        // in range, and flagged as a nested function. First-appearance order is
        // preserved when assigning local indices, matching the serializer.
        let mut nested_functions: Vec<Box<BytecodeFunction>> = Vec::new();
        let mut global_to_local: HashMap<u32, u32> = HashMap::new();
        for instr in &mut function.code {
            if let Instr::MakeClosure { func, .. } = instr {
                let global = *func;
                if global as usize >= index {
                    return Err(bbu_error(
                        "closure function reference is not backwards/topological",
                    ));
                }
                if encoded_functions[global as usize].flags & BBU_FUNC_NESTED == 0 {
                    return Err(bbu_error("closure references a non-nested function"));
                }
                let local = *global_to_local.entry(global).or_insert_with(|| {
                    let local = nested_functions.len() as u32;
                    nested_functions.push(Box::new(decoded_functions[global as usize].clone()));
                    local
                });
                *func = local;
            }
        }
        function.nested_functions = nested_functions;
        decoded_functions.push(function);
    }
    let functions = decoded_functions
        .into_iter()
        .map(Rc::new)
        .collect::<Vec<_>>();

    let mut last = NIL;
    for action in actions {
        match action.kind {
            1 => {
                last = execute_bbu_ensure_package(&encoded_constants, &constants, action)?;
            }
            2 => {
                let (package_name, _) = bbu_package(&encoded_constants, action.arg0)?;
                let package = bliss_stdlib::find_package(package_name).ok_or_else(|| {
                    bbu_error(format!(
                        "package {package_name:?} was not ensured before INTERN"
                    ))
                })?;
                let name = bbu_string(&encoded_constants, action.arg1)?;
                last = bliss_stdlib::intern(name, package)?.0;
            }
            3 => {
                let function_index = action.arg0 as usize;
                let sym = bbu_symbol(&constants, action.arg1)?;
                let function = Rc::clone(&functions[function_index]);
                let symbol = BlissVal::from_symbol_index(sym);
                let lambda_list = if function.variadic {
                    function.params_form
                } else {
                    installed_lambda_list(function.arity)
                };
                let fn_obj = match bliss_rt::symbols::symbol_function(sym) {
                    Some(existing) if bliss_rt::function::is_interpreted_function(existing) => {
                        // SAFETY: checked immediately above.
                        unsafe { bliss_rt::function::redefine(existing, lambda_list, NIL, NIL) };
                        existing
                    }
                    _ => {
                        let allocated =
                            bliss_rt::function::alloc_interpreted(lambda_list, NIL, NIL, symbol);
                        bliss_rt::symbols::set_symbol_function(sym, allocated);
                        allocated
                    }
                };
                registry_put(sym, function);
                // Keep the function object's tier metadata tied to this loaded
                // definition; registry_put has already invalidated old native code.
                bliss_rt::function::set_tier(fn_obj, 0);
                last = symbol;
            }
            4 => {
                let symbol = constants[bbu_index(action.arg1, constants.len(), "macro name")?];
                super::install_loaded_macro(
                    symbol,
                    Rc::clone(&functions[action.arg0 as usize]),
                    env,
                );
                last = symbol;
            }
            5 => {
                let symbol =
                    constants[bbu_index(action.arg1, constants.len(), "compiler macro name")?];
                super::install_loaded_compiler_macro(
                    symbol,
                    Rc::clone(&functions[action.arg0 as usize]),
                );
                last = symbol;
            }
            8 => {
                let symbol =
                    constants[bbu_index(action.arg1, constants.len(), "setf expander name")?];
                super::install_loaded_setf_expander(
                    symbol,
                    Rc::clone(&functions[action.arg0 as usize]),
                    env,
                );
                last = symbol;
            }
            7 => {
                last = run(Rc::clone(&functions[action.arg0 as usize]), &[], NIL, env)?;
            }
            _ => unreachable!("load actions were verified above"),
        }
    }
    Ok(last)
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
    /// RAII guards for special-variable bindings established by this frame.
    dyn_binds: Vec<DynBind>,
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

/// Build the base heap `EnvFrame` for a function with captured locals. Parameter
/// values are populated by [`bind_params`] or [`bind_variadic`], after the
/// lambda-list binder has resolved defaults, supplied-p variables, and rest/key
/// arguments. Returns `None` for the slot-only case.
fn make_env_frame(
    func: &BytecodeFunction,
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
    Some(frame)
}

/// Define a captured parameter in an activation's heap environment. Keep the
/// name and symbol-index maps in sync, just like `DefineEnvVar`.
fn bind_boxed_param(env_frame: &Rc<RefCell<EnvFrame>>, name: &str, value: BlissVal) {
    let mut borrowed = env_frame.borrow_mut();
    if let Some(idx) = bliss_rt::symbols::find_index(name) {
        borrowed.symbol_vars.insert(idx, value);
    }
    borrowed.vars.insert(name.to_string(), value);
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

fn validate_declared_args(func: &BytecodeFunction, args: &[BlissVal]) -> Result<(), BlissError> {
    for (ty, value) in func.param_types.iter().copied().zip(args.iter().copied()) {
        let matches = match ty {
            DeclaredType::Any => true,
            DeclaredType::Fixnum => value.is_fixnum(),
            DeclaredType::SingleFloat => value.is_single_float(),
        };
        if !matches {
            let expected = match ty {
                DeclaredType::Any => unreachable!(),
                DeclaredType::Fixnum => "FIXNUM",
                DeclaredType::SingleFloat => "SINGLE-FLOAT",
            };
            return Err(BlissError::TypeError {
                datum: value,
                expected: expected.to_string(),
            });
        }
    }
    Ok(())
}

/// Bind a fixed-arity call's arguments to stack slots or boxed heap bindings.
fn bind_params(
    func: &BytecodeFunction,
    frame: *mut Frame,
    args: &[BlissVal],
    env_frame: Option<&Rc<RefCell<EnvFrame>>>,
) {
    for (i, (name, loc)) in func.param_layout.iter().enumerate() {
        if let Some(a) = args.get(i) {
            match loc {
                VarLoc::Slot(s) => unsafe { slot_set(frame, *s, *a) },
                VarLoc::Boxed => bind_boxed_param(
                    env_frame.expect("boxed parameter without a heap EnvFrame"),
                    name,
                    *a,
                ),
            }
        }
    }
}

/// Bind a variadic lambda list (`&optional`/`&rest`/`&key`/`&aux`) into `frame`'s
/// slots at call time (x5y.7). The tree-walker's `bind_lambda_list` does the
/// parsing and default-form evaluation into a throwaway child env — so a default
/// evaluates in a scope where the earlier parameters are visible, byte-for-byte
/// as the interpreter would — and then each parameter's value is copied into its
/// stack slot or boxed heap binding.
fn bind_variadic(
    func: &BytecodeFunction,
    frame: *mut Frame,
    args: &[BlissVal],
    env_frame: Option<&Rc<RefCell<EnvFrame>>>,
    env: &mut Env,
) -> Result<(), BlissError> {
    let parent = Rc::clone(&env.frame);
    super::with_child_frame(env, parent, |env| {
        super::bind_lambda_list(func.params_form, args, env)?;
        // Argument/default evaluation is a single-value context.
        env.clear_mv();
        let cur = Rc::clone(&env.frame);
        for (name, loc) in &func.param_layout {
            let v = super::Env::lookup_frame(&cur, name).unwrap_or(NIL);
            match loc {
                VarLoc::Slot(s) => unsafe { slot_set(frame, *s, v) },
                VarLoc::Boxed => bind_boxed_param(
                    env_frame.expect("boxed variadic parameter without a heap EnvFrame"),
                    name,
                    v,
                ),
            }
        }
        Ok(())
    })
}

fn bind_macro_variadic(
    func: &BytecodeFunction,
    frame: *mut Frame,
    args: &[BlissVal],
    env_frame: Option<&Rc<RefCell<EnvFrame>>>,
    env: &mut Env,
) -> Result<(), BlissError> {
    let parent = Rc::clone(&env.frame);
    super::with_child_frame(env, parent, |env| {
        super::bind_macro_lambda_list(func.params_form, args, env, None)?;
        env.clear_mv();
        let current = Rc::clone(&env.frame);
        for (name, location) in &func.param_layout {
            let value = super::Env::lookup_frame(&current, name).unwrap_or(NIL);
            match location {
                VarLoc::Slot(slot) => unsafe { slot_set(frame, *slot, value) },
                VarLoc::Boxed => bind_boxed_param(
                    env_frame.expect("boxed macro parameter without a heap EnvFrame"),
                    name,
                    value,
                ),
            }
        }
        Ok(())
    })
}

/// Run a compiled function to completion on the current green thread's
/// `BlissStack`. `args` are the actual arguments bound into the entry frame's
/// leading local slots.
pub(super) fn run(
    entry: Rc<BytecodeFunction>,
    args: &[BlissVal],
    entry_fn_val: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    run_with_binding(entry, args, entry_fn_val, env, false)
}

pub(super) fn run_macro(
    entry: Rc<BytecodeFunction>,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    run_with_binding(entry, args, NIL, env, true)
}

fn run_with_binding(
    entry: Rc<BytecodeFunction>,
    args: &[BlissVal],
    entry_fn_val: BlissVal,
    env: &mut Env,
    macro_lambda_list: bool,
) -> Result<BlissVal, BlissError> {
    let _active_bytecode_root = ActiveBytecodeRoot::new(&entry);
    validate_declared_args(&entry, args)?;
    record_profiled_invocation(Rc::as_ptr(&entry) as usize);
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
        .ok_or_else(|| {
            BlissError::StackOverflow(
                bliss_rt::current_fiber_id()
                    .unwrap_or_else(|| bliss_rt::FiberId(bliss_rt::current_thread_id().0)),
            )
        })?;
    // An env-capturing closure runs with its captured heap frame as the base of
    // its environment: the closure body's LoadEnvVar/StoreEnvVar reach the
    // enclosing lexicals (and sibling flet/labels closures) through it. A
    // closure with its own boxed locals gets a fresh child of the captured
    // frame; one that only reads the enclosing scope uses the captured frame
    // directly.
    let closure_env = closure_captured_env(entry_fn_val);
    let parent = closure_env
        .clone()
        .unwrap_or_else(|| Rc::clone(&env.frame));
    let env_frame = make_env_frame(&entry, parent).or(closure_env);
    if macro_lambda_list {
        if let Err(e) = bind_macro_variadic(&entry, frame, args, env_frame.as_ref(), env) {
            stack.pop_frame();
            return Err(e);
        }
    } else if entry.variadic {
        if let Err(e) = bind_variadic(&entry, frame, args, env_frame.as_ref(), env) {
            stack.pop_frame();
            return Err(e);
        }
    } else {
        bind_params(&entry, frame, args, env_frame.as_ref());
    }
    let entry_obj = Some(entry_fn_val).filter(|&v| bliss_rt::function::is_interpreted_function(v));
    let mut acts: Vec<Activation> = vec![Activation {
        frame,
        n_locals: entry.n_locals,
        env_frame,
        func: entry,
        bcp: 0,
        sp_top: 0,
        handlers: Vec::new(),
        cleanup_conts: Vec::new(),
        dyn_binds: Vec::new(),
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
            Instr::LoadFunction(sym) => {
                acts[top_idx].push_op(BlissVal::from_symbol_index(sym));
            }
            Instr::BindSpecial(sym) => {
                let value = acts[top_idx].pop_op();
                acts[top_idx]
                    .dyn_binds
                    .push(DynBind::establish(BlissVal::from_symbol_index(sym), value));
            }
            Instr::UnbindSpecial(count) => {
                for _ in 0..count {
                    acts[top_idx].dyn_binds.pop();
                }
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
                let vals = bliss_rt::gc::HostRoot::new(if env.mv_active {
                    env.mv.clone()
                } else {
                    vec![primary]
                });
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
                let mut borrowed = ef.borrow_mut();
                if let Some(idx) = bliss_rt::symbols::find_index(&name) {
                    borrowed.symbol_vars.insert(idx, v);
                }
                borrowed.vars.insert(name, v);
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
            Instr::MakeClosure { func, capture_env } => {
                let nested = acts[top_idx].func.nested_functions[func as usize].clone();
                // A capturing closure records the creating activation's heap
                // EnvFrame so its body reads/writes the enclosing lexicals; the
                // frame is installed as the closure's environment when it runs.
                let captured = if capture_env {
                    acts[top_idx].env_frame.clone()
                } else {
                    None
                };
                let closure = make_bytecode_closure(&nested, captured);
                acts[top_idx].push_op(closure);
            }
            Instr::AllocCons => {
                let mut cdr = acts[top_idx].pop_op();
                let mut car = acts[top_idx].pop_op();
                let _cdr_root = bliss_rt::gc::StackRoot::new(&mut cdr);
                let _car_root = bliss_rt::gc::StackRoot::new(&mut car);
                acts[top_idx].push_op(arena_cons(car, cdr));
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
                let args = bliss_rt::gc::HostRoot::new(args);

                // Argument forms are single-value contexts. A producer used as
                // an argument may have populated env.mv, but those secondary
                // values belong to argument evaluation, not to the callee's
                // return. Every dispatch below starts from a clean MV state;
                // genuine producers re-establish it while executing.
                env.clear_mv();

                // Only saved bytecode callees are body-inlining candidates.
                // Reuse this lookup for dispatch below and avoid profiling the
                // much larger population of builtin calls.
                let registered_callee = registry_get(sym);

                // Profile every call site's execution frequency.  Type details
                // remain restricted to speculatable arithmetic sites. `bcp` was
                // already advanced past this instruction, so the site is bcp-1.
                let func_ptr = Rc::as_ptr(&acts[top_idx].func) as usize;
                let call_bcp = acts[top_idx].bcp as u32 - 1;
                if registered_callee.is_some() {
                    record_call_site(func_ptr, call_bcp);
                }
                if is_arith_speculatable(sym) {
                    record_type_profile(func_ptr, call_bcp, &args);
                }

                // bliss-jtc.6.8: bump the callee's FnMeta invoke counter (the
                // unified tiering substrate) so the function object reflects real
                // invocations from the bytecode path, not just the tree-walker.
                let fn_obj = bliss_rt::symbols::symbol_function(sym)
                    .filter(|&cell| bliss_rt::function::is_interpreted_function(cell));
                if let Some(cell) = fn_obj {
                    bliss_rt::function::record_invocation(cell);
                }

                // Bytecode callee → native frame on the BlissStack.
                if let Some(callee) = registered_callee {
                    if arity_accepts(&callee, nargs as usize) {
                        // Installed native code → call via the i2c adapter.
                        // Unified tiering (bliss-jtc.3): the function object's
                        // FnMeta is the single tiering record — invoke counter
                        // (bumped at the top of CallNamed), current tier, and the
                        // active compiled entry. Promotion is driven by that
                        // counter and, on success, tier + entry are recorded on
                        // the object. Anonymous compiled lambdas (gensyms) have no
                        // function object, so they keep the INVOKE_COUNTS fallback.
                        let count = dispatch_invoke_count(sym, fn_obj);
                        let native = native_for_dispatch(sym, fn_obj, count);
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
                        // Native entry validates in `run_native`; the flat T0
                        // activation reaches no adapter, so enforce the same
                        // declaration contract here before binding its frame.
                        if let Err(e) = validate_declared_args(&callee, &args) {
                            let pending = error_to_pending(e, env);
                            initiate_unwind(acts, stack, env, pending)?;
                            continue;
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
                                let e = BlissError::StackOverflow(
                                    bliss_rt::current_fiber_id().unwrap_or_else(|| {
                                        bliss_rt::FiberId(bliss_rt::current_thread_id().0)
                                    }),
                                );
                                initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                                continue;
                            }
                        };
                        let env_frame = make_env_frame(&callee, Rc::clone(&env.frame));
                        if callee.variadic {
                            if let Err(e) =
                                bind_variadic(&callee, frame, &args, env_frame.as_ref(), env)
                            {
                                stack.pop_frame();
                                initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                                continue;
                            }
                        } else {
                            bind_params(&callee, frame, &args, env_frame.as_ref());
                        }
                        record_profiled_invocation(Rc::as_ptr(&callee) as usize);
                        acts.push(Activation {
                            frame,
                            n_locals: callee.n_locals,
                            env_frame,
                            func: callee,
                            bcp: 0,
                            sp_top: 0,
                            handlers: Vec::new(),
                            cleanup_conts: Vec::new(),
                            dyn_binds: Vec::new(),
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
            Instr::ReturnFromNamed { name_idx } => {
                // Non-local return from an enclosing block: resolve the block's
                // control token by name on the shared block stack and unwind to
                // it (like THROW to a CATCH). The unwind propagates out of this
                // closure's `run` via the token→BlissError::Internal bridge and
                // is resumed by the establishing function.
                let name = acts[top_idx].func.names[name_idx as usize].clone();
                let value = acts[top_idx].pop_op();
                let token = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(n, _)| *n == name)
                    .map(|(_, tok)| tok.clone());
                match token {
                    Some(tok) => {
                        store_control_value(&tok, value);
                        initiate_unwind(acts, stack, env, Pending::Token(tok))?;
                    }
                    None => {
                        let e = BlissError::Internal(format!(
                            "RETURN-FROM: no visible block {name}"
                        ));
                        initiate_unwind(acts, stack, env, Pending::Propagate(e))?;
                    }
                }
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
                let captured_frame = acts[top_idx]
                    .env_frame
                    .clone()
                    .unwrap_or_else(|| Rc::clone(&env.frame));
                for restart in &info.restarts {
                    env.restarts.push(RestartEntry {
                        name: restart.name.clone(),
                        function: RestartFunction::Bytecode {
                            function: Rc::new(RefCell::new((*restart.function).clone())),
                            captured_frame: Rc::clone(&captured_frame),
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
    /// Captured lexical environment for the currently executing native
    /// activation. Nested native calls save/restore this just like NATIVE_ENV.
    static NATIVE_ENV_FRAME: RefCell<Option<Rc<RefCell<EnvFrame>>>> = const { RefCell::new(None) };
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

/// Read a global/special symbol's dynamic value cell (bliss-x5y.15). Non-
/// allocating and side-effect-free, so — like `c2i_clear_mv` — it needs no GC
/// stack map: r14/r15 are callee-saved and rsp stays 16-aligned across the call.
extern "C" fn c2i_load_global(sym: u64) -> u64 {
    bliss_rt::symbols::symbol_value(sym as u32)
        .filter(|v| *v != bliss_rt::value::UNBOUND)
        .map(|v| v.0)
        .unwrap_or(bliss_rt::value::NIL.0)
}

/// Write a global/special symbol's dynamic value cell (bliss-x5y.15). A side
/// effect, so a function containing `StoreGlobal` opts out of speculation (see
/// `deopt_safe`) — a whole-function deopt-rerun must never re-run the store.
extern "C" fn c2i_store_global(sym: u64, val: u64) {
    bliss_rt::symbols::set_symbol_value(sym as u32, BlissVal(val));
}

fn stash_native_error(error: BlissError) {
    NATIVE_ERROR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    });
}

fn native_env_name(function_sym: u64, name_index: u64) -> Option<String> {
    registry_get(function_sym as u32).and_then(|body| body.names.get(name_index as usize).cloned())
}

extern "C" fn c2i_load_env(function_sym: u64, name_index: u64) -> u64 {
    let Some(name) = native_env_name(function_sym, name_index) else {
        stash_native_error(BlissError::Internal(
            "native environment name vanished".into(),
        ));
        return NIL.0;
    };
    NATIVE_ENV_FRAME.with(|slot| {
        let frame = slot.borrow();
        match frame
            .as_ref()
            .and_then(|frame| Env::lookup_frame(frame, &name))
        {
            Some(value) => value.0,
            None => {
                let symbol = resolve_sym(&name).unwrap_or(NIL);
                stash_native_error(BlissError::UnboundVariable(symbol));
                NIL.0
            }
        }
    })
}

extern "C" fn c2i_store_env(function_sym: u64, name_index: u64, value: u64) {
    let Some(name) = native_env_name(function_sym, name_index) else {
        stash_native_error(BlissError::Internal(
            "native environment name vanished".into(),
        ));
        return;
    };
    NATIVE_ENV_FRAME.with(|slot| {
        if let Some(frame) = slot.borrow().as_ref() {
            if !Env::set_frame_var(frame, &name, BlissVal(value)) {
                stash_native_error(BlissError::UnboundVariable(
                    resolve_sym(&name).unwrap_or(NIL),
                ));
            }
        }
    });
}

extern "C" fn c2i_define_env(function_sym: u64, name_index: u64, value: u64) {
    let Some(name) = native_env_name(function_sym, name_index) else {
        stash_native_error(BlissError::Internal(
            "native environment name vanished".into(),
        ));
        return;
    };
    NATIVE_ENV_FRAME.with(|slot| {
        if let Some(frame) = slot.borrow().as_ref() {
            let mut borrowed = frame.borrow_mut();
            if let Some(symbol_index) = bliss_rt::symbols::find_index(&name) {
                borrowed.symbol_vars.insert(symbol_index, BlissVal(value));
            }
            borrowed.vars.insert(name.clone(), BlissVal(value));
        }
    });
}

extern "C" fn c2i_push_env_child() {
    NATIVE_ENV_FRAME.with(|slot| {
        let parent = slot.borrow().clone();
        *slot.borrow_mut() = Some(Rc::new(RefCell::new(EnvFrame {
            vars: HashMap::new(),
            symbol_vars: HashMap::new(),
            parent,
        })));
    });
}

extern "C" fn c2i_pop_env_child() {
    NATIVE_ENV_FRAME.with(|slot| {
        let parent = slot
            .borrow()
            .as_ref()
            .and_then(|frame| frame.borrow().parent.clone());
        *slot.borrow_mut() = parent;
    });
}

fn c2i_eval_form_with_frame(form: BlissVal, capture_native_frame: bool) -> u64 {
    let env_ptr = NATIVE_ENV.with(|cell| cell.get());
    if env_ptr.is_null() {
        return NIL.0;
    }
    let env = unsafe { &mut *env_ptr };
    let saved_frame = if capture_native_frame {
        NATIVE_ENV_FRAME.with(|slot| {
            slot.borrow()
                .clone()
                .map(|frame| std::mem::replace(&mut env.frame, frame))
        })
    } else {
        None
    };
    let result = eval_form(form, env);
    if let Some(saved) = saved_frame {
        env.frame = saved;
    }
    match result {
        Ok(value) => value.0,
        Err(error) => {
            stash_native_error(error);
            NIL.0
        }
    }
}

extern "C" fn c2i_eval_host(form: u64) -> u64 {
    c2i_eval_form_with_frame(BlissVal(form), false)
}

extern "C" fn c2i_make_closure(form: u64) -> u64 {
    c2i_eval_form_with_frame(BlissVal(form), true)
}

extern "C" fn c2i_take_values(primary: u64, dst: *mut BlissVal, n: u64) {
    let env_ptr = NATIVE_ENV.with(|cell| cell.get());
    if env_ptr.is_null() || (n != 0 && dst.is_null()) {
        return;
    }
    let env = unsafe { &mut *env_ptr };
    let values = if env.mv_active {
        env.mv.clone()
    } else {
        Vec::new()
    };
    for index in 0..n as usize {
        let value = if index == 0 {
            BlissVal(primary)
        } else {
            values.get(index).copied().unwrap_or(NIL)
        };
        unsafe { dst.add(index).write(value) };
    }
}

extern "C" fn c2i_call(sym: u64, n: u64, a0: u64, a1: u64, a2: u64, profile_site: u64) -> u64 {
    if n > 3 {
        return NIL.0;
    }
    let storage = [BlissVal(a0), BlissVal(a1), BlissVal(a2)];
    c2i_call_args(sym, &storage[..n as usize], profile_site)
}

/// T1's unbounded-arity c2i adapter. Arguments remain contiguous in the native
/// activation's BlissStack operand area, so passing a slice avoids the old
/// three-register ceiling without copying or using the host call stack as a
/// Lisp value stack.
extern "C" fn c2i_call_slice(sym: u64, n: u64, args: *const BlissVal, profile_site: u64) -> u64 {
    if n != 0 && args.is_null() {
        return NIL.0;
    }
    // SAFETY: T1 passes a pointer into its live BlissStack frame and invokes the
    // adapter synchronously. The frame cannot disappear during this call.
    let args = unsafe { std::slice::from_raw_parts(args, n as usize) };
    c2i_call_args(sym, args, profile_site)
}

fn c2i_call_args(sym: u64, args: &[BlissVal], profile_site: u64) -> u64 {
    let env_ptr = NATIVE_ENV.with(|e| e.get());
    if env_ptr.is_null() {
        return NIL.0;
    }
    // SAFETY: `run_native` sets NATIVE_ENV to a live &mut Env for the duration
    // of the native call, and native code only calls this synchronously within
    // that window.
    let env = unsafe { &mut *env_ptr };
    record_native_call_site(profile_site);
    let n = args.len();
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
        Some(callee) if arity_accepts(&callee, n) => {
            // Calls emitted by native T1/T2 code do not pass through the
            // interpreter's CallNamed arm, so this adapter owns the callee's
            // invocation bump and tier transition. Without it, a callee reached
            // only from a native caller would stop warming up permanently.
            let fn_obj = bliss_rt::symbols::symbol_function(sym32)
                .filter(|&cell| bliss_rt::function::is_interpreted_function(cell));
            if let Some(cell) = fn_obj {
                bliss_rt::function::record_invocation(cell);
            }
            let count = dispatch_invoke_count(sym32, fn_obj);
            let selected = native_for_dispatch(sym32, fn_obj, count);
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
                selected
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
    static NATIVE_DEOPT_RESUME: RefCell<Option<NativeDeoptResume>> = const { RefCell::new(None) };
    /// Current native (T1) call-stack depth (bliss-x5y.4). Non-leaf T1 functions
    /// call through c2i, and although those calls run in the interpreter (which
    /// is BlissStack-bounded, not native-recursive), this counter bounds any
    /// native re-entry defensively: past `native_depth_cap()` the dispatcher
    /// runs the callee in T0 instead of pushing another native frame, so the
    /// real C stack can never run away.
    static NATIVE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

enum NativeDeoptResume {
    Single { bcp: u32, sp_top: u16 },
    Inlined(Vec<InlinedResumeScope>),
}

/// Slow poll reached by a T1 backward branch once per sampled batch. It never
/// compiles on the application thread: it publishes completed worker results,
/// queues a request when needed, and—if the matching optimized loop entry is
/// ready—runs the remainder of this same activation in T2.
///
/// Returns 1 when T1 must return through its normal epilogue (T2 completed,
/// errored, or deoptimized), and 0 when the branch should remain in T1.
extern "C" fn c2i_t1_backedge(sym: u64, header_bcp: u64, slots: *mut u64) -> u64 {
    let sym = sym as u32;
    let header_bcp = header_bcp as u32;
    let Some(body) = registry_get(sym) else {
        return 0;
    };
    let fn_obj = bliss_rt::symbols::symbol_function(sym)
        .filter(|&v| bliss_rt::function::is_interpreted_function(v));
    if let Some(f) = fn_obj {
        bliss_rt::function::record_back_edges(f, t2_backedge_threshold());
    }
    if !t2_enabled() {
        return 0;
    }

    poll_t2_completions();
    let current = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned());
    if current.as_ref().is_none_or(|nc| !nc.is_t2) {
        let back_edges = fn_obj
            .map(bliss_rt::function::back_edge_count)
            .unwrap_or(t2_backedge_threshold());
        let priority =
            (u64::from(back_edges) << 32) | u64::from(dispatch_invoke_count_snapshot(sym, fn_obj));
        let _ = request_t2_compilation(sym, priority);
        poll_t2_completions();
    }

    let Some(t2) = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned()) else {
        return 0;
    };
    if !t2.is_t2 {
        return 0;
    }
    let Some(&offset) = t2.osr_entries.get(&header_bcp) else {
        return 0;
    };
    let entry = t2.entry as usize + offset;
    // SAFETY: the T2 emitter records only alternate entries having the same
    // `fn(*mut u64)->u64` ABI. `slots` is r14 from the still-live T1 frame.
    let f: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(entry) };
    let result = f(slots);
    // On normal completion the first operand slot is dead and carries the
    // result to T1's shared epilogue. On deopt, however, c2i_deopt_t2 may have
    // reconstructed a non-empty operand stack starting at this exact slot; do
    // not overwrite the resume state with the callback's dummy return value.
    let deopt = NATIVE_DEOPT.with(|state| state.get());
    let errored = NATIVE_ERROR.with(|error| error.borrow().is_some());
    if !deopt && !errored {
        unsafe { slots.add(body.n_locals as usize).write(result) };
    }
    1
}

struct InlinedResumeScope {
    function: u32,
    bcp: u32,
    sp_top: u16,
    frame: *mut Frame,
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
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
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
    NATIVE_DEOPT_RESUME.with(|c| {
        *c.borrow_mut() = Some(NativeDeoptResume::Single {
            bcp: bcp as u32,
            sp_top: depth as u16,
        })
    });
}

/// Precise state-transfer deopt for T2 optimising code (bliss-mba). Unlike T1,
/// T2/`emit_framed` is register-based and does NOT keep the interpreter frame's
/// locals/operand-stack live in memory, so a failed guard cannot resume T0 on
/// the frame as-is. Instead the guard's per-instruction deopt stub reconstructs
/// the logical frames here. `buf` is an outer-to-inner stream of scope headers
/// followed by tagged locals and operand slots. The outer scope reuses the
/// current frame; inlined callees get new BlissStack frames. `run_native` then
/// resumes T0 on the reconstructed activation chain, so committed caller side
/// effects are not repeated.
extern "C" fn c2i_deopt_t2(n_scopes: u64, n_words: u64, buf: *const u64, _reserved: u64) {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();
    let outer = stack.fp() as *mut Frame;
    let fail = |message: &'static str| {
        NATIVE_ERROR.with(|c| {
            let mut error = c.borrow_mut();
            if error.is_none() {
                *error = Some(BlissError::Internal(message.into()));
            }
        });
    };
    if outer.is_null() || buf.is_null() || n_scopes == 0 {
        fail("T2 deopt supplied an empty virtual-frame stream");
        return;
    }

    let words = unsafe { std::slice::from_raw_parts(buf, n_words as usize) };
    let mut at = 0usize;
    let mut pushed = 0usize;
    let mut scopes = Vec::with_capacity(n_scopes as usize);
    for scope_index in 0..n_scopes as usize {
        if at + 4 > words.len() {
            fail("truncated T2 virtual-frame header");
            break;
        }
        let function = words[at] as u32;
        let bcp = words[at + 1] as u32;
        let n_locals = words[at + 2] as usize;
        let sp_top = words[at + 3] as usize;
        at += 4;
        let n_slots = n_locals.saturating_add(sp_top);
        let Some(entry) = registry_get(function) else {
            fail("T2 deopt function is absent from the bytecode registry");
            break;
        };
        if at + n_slots > words.len() || n_slots > entry.num_slots() as usize {
            fail("invalid T2 virtual-frame slot count");
            break;
        }
        let frame = if scope_index == 0 {
            outer
        } else {
            let Some(frame) = stack.push_frame(
                BlissVal::from_symbol_index(function),
                std::ptr::null::<CodeInfo>(),
                entry.num_slots(),
                FLAG_CALL,
            ) else {
                fail("BlissStack exhausted while reconstructing inlined frames");
                break;
            };
            pushed += 1;
            frame
        };
        for (slot, &bits) in words[at..at + n_slots].iter().enumerate() {
            unsafe { slot_set(frame, slot as u16, BlissVal(bits)) };
        }
        at += n_slots;
        scopes.push(InlinedResumeScope {
            function,
            bcp,
            sp_top: sp_top as u16,
            frame,
        });
    }
    if scopes.len() != n_scopes as usize || at != words.len() {
        for _ in 0..pushed {
            stack.pop_frame();
        }
        return;
    }
    NATIVE_DEOPT.with(|d| d.set(true));
    NATIVE_DEOPT_RESUME.with(|c| {
        *c.borrow_mut() = Some(NativeDeoptResume::Inlined(scopes));
    });
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
    /// Not yet consumed by the interpreter's dispatch (no compiled→compiled calls
    /// cross the c2i boundary yet); recorded so that wiring is a local change.
    #[allow(dead_code)]
    compiled_entry: usize,
    /// Optimized loop-header entry offsets for T1→T2 on-stack replacement.
    osr_entries: HashMap<u32, usize>,
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
    /// Per-site speculation types invalidated by the current native version's
    /// first deopt. Used to distinguish a supported numeric phase change from
    /// repeatedly failing the same specialization (for example, fixnum overflow).
    static LAST_FAILED_SPECULATION: RefCell<HashMap<u32, Vec<(u32, SpecType)>>> =
        RefCell::new(HashMap::new());
}

/// A speculation for `func_ptr` just failed. Zero the currently-dominant type at
/// each of its sites — that is the type we bet on and lost — so the new phase's
/// samples take over quickly (reaching dominance in ~MIN samples instead of having
/// to out-vote a large stale count). Called once per promotion; the other type's
/// samples keep accumulating, so the profile never goes fully cold (which would,
/// with the cold-site fixnum guess, just re-promote the same wrong type).
fn decay_failed_speculation(func_ptr: usize, forced: Option<SpecType>) -> Vec<(u32, SpecType)> {
    let mut failed = Vec::new();
    TYPE_PROFILE.with(|m| {
        for ((fp, bcp), p) in m.borrow_mut().iter_mut() {
            if *fp == func_ptr {
                let spec = forced.or_else(|| {
                    if p.fixnum >= p.single_float && p.fixnum > 0 {
                        Some(SpecType::Fixnum)
                    } else if p.single_float > 0 {
                        Some(SpecType::SingleFloat)
                    } else {
                        None
                    }
                });
                if let Some(spec) = spec {
                    match spec {
                        SpecType::Fixnum => p.fixnum = 0,
                        SpecType::SingleFloat => p.single_float = 0,
                    }
                    failed.push((*bcp, spec));
                }
            }
        }
    });
    failed
}

/// True when the failed version is now seeing the *other* supported numeric
/// phase at one of its guarded sites. This is an invalidation/recompile event,
/// not evidence that native execution is fundamentally unprofitable.
fn supported_numeric_phase_change(sym: u32) -> bool {
    let failed = LAST_FAILED_SPECULATION.with(|m| m.borrow().get(&sym).cloned());
    let Some(failed) = failed else { return false };
    let Some(body) = registry_get(sym) else {
        return false;
    };
    let func_ptr = Rc::as_ptr(&body) as usize;
    failed.into_iter().any(|(bcp, old)| {
        type_profile_at(func_ptr, bcp).is_some_and(|profile| {
            profile.other == 0
                && match old {
                    SpecType::Fixnum => profile.single_float > 0,
                    SpecType::SingleFloat => profile.fixnum > 0,
                }
        })
    })
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

/// Record entry to a saved bytecode function in the shared T0/T1 sampling window.
fn record_profiled_invocation(func_ptr: usize) {
    FUNCTION_SAMPLE_PROFILE.with(|m| {
        let mut counts = m.borrow_mut();
        let count = counts.entry(func_ptr).or_insert(0);
        *count = count.saturating_add(1);
    });
}

/// Record one execution of a named-call bytecode.
fn record_call_site(func_ptr: usize, bcp: u32) {
    let counter = call_site_counter(func_ptr, bcp);
    let _ = counter.calls.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |n| Some(n.saturating_add(1)),
    );
}

struct RuntimeCallSiteProfile {
    calls: std::sync::atomic::AtomicU32,
}

fn call_site_counter(func_ptr: usize, bcp: u32) -> &'static RuntimeCallSiteProfile {
    CALL_SITE_PROFILE.with(|m| {
        let mut profiles = m.borrow_mut();
        *profiles.entry((func_ptr, bcp)).or_insert_with(|| {
            Box::leak(Box::new(RuntimeCallSiteProfile {
                calls: std::sync::atomic::AtomicU32::new(0),
            }))
        })
    })
}

/// Stable pointer embedded in T1 code. The record is intentionally leaked:
/// installed native code may outlive a registry replacement, and an obsolete
/// site must remain safe to increment even after its profile is detached.
fn call_site_profile_token(func_ptr: usize, bcp: u32) -> u64 {
    call_site_counter(func_ptr, bcp) as *const RuntimeCallSiteProfile as usize as u64
}

fn record_native_call_site(token: u64) {
    if token == 0 {
        return;
    }
    // SAFETY: tokens are pointers returned by `Box::leak` above and therefore
    // remain valid for the process lifetime.
    let profile = unsafe { &*(token as usize as *const RuntimeCallSiteProfile) };
    let _ = profile.calls.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |n| Some(n.saturating_add(1)),
    );
}

/// Snapshot call frequencies for one saved body without retaining profiler
/// borrows while compiler options are assembled.
fn call_site_profile_snapshot(func_ptr: usize) -> (u32, Vec<(u32, u32)>) {
    let invocations =
        FUNCTION_SAMPLE_PROFILE.with(|m| m.borrow().get(&func_ptr).copied().unwrap_or(0));
    let sites = CALL_SITE_PROFILE.with(|m| {
        m.borrow()
            .iter()
            .filter_map(|(&(fp, bcp), profile)| {
                (fp == func_ptr).then_some((
                    bcp,
                    profile.calls.load(std::sync::atomic::Ordering::Relaxed),
                ))
            })
            .collect()
    });
    (invocations, sites)
}

fn clear_bytecode_profiles(func_ptr: usize) {
    TYPE_PROFILE.with(|m| m.borrow_mut().retain(|(fp, _), _| *fp != func_ptr));
    CALL_SITE_PROFILE.with(|m| m.borrow_mut().retain(|(fp, _), _| *fp != func_ptr));
    FUNCTION_SAMPLE_PROFILE.with(|m| {
        m.borrow_mut().remove(&func_ptr);
    });
}

thread_local! {
    /// Operand-type profiles keyed by (bytecode-function pointer, CallNamed bcp).
    static TYPE_PROFILE: RefCell<HashMap<(usize, u32), TypeProfile>> = RefCell::new(HashMap::new());
    /// T0/T1 entries per saved bytecode body. This is the denominator for
    /// call-site frequency and advances in the same sampling window as calls.
    static FUNCTION_SAMPLE_PROFILE: RefCell<HashMap<usize, u32>> = RefCell::new(HashMap::new());
    /// Executions per `(saved bytecode body, CallNamed bcp)`.
    static CALL_SITE_PROFILE: RefCell<HashMap<(usize, u32), &'static RuntimeCallSiteProfile>> =
        RefCell::new(HashMap::new());
    /// Installed T1/T2 code keyed by the same symbol index as the bytecode registry.
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
    /// Bodies that the optimising compiler cannot currently handle.  Remember
    /// the decline so a hot T1 function does not synchronously retry T2 on every
    /// invocation.  Redefining/removing the function clears this bit.
    static T2_DECLINED: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
    /// Symbols with one outstanding background T2 request.  Coalescing here
    /// prevents a hot dispatch/back-edge from flooding the global queue.
    static T2_QUEUED: RefCell<HashMap<u32, u64>> = RefCell::new(HashMap::new());
    /// Each mutator owns its completion channel. Workers return relocatable
    /// code bytes; executable-memory and tier publication remain on the owner.
    static T2_COMPLETIONS: RefCell<Option<(
        std::sync::mpsc::Sender<T2Completion>,
        std::sync::mpsc::Receiver<T2Completion>,
    )>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct T2BodySnapshot {
    symbol: u32,
    body: BytecodeFunction,
    invocations: u32,
    call_sites: Vec<(u32, u32)>,
}

struct T2CompileInput {
    sym: u32,
    generation: u64,
    priority: u64,
    body: BytecodeFunction,
    type_profiles: HashMap<u32, TypeProfile>,
    inline_bodies: Vec<T2BodySnapshot>,
}

struct T2Artifact {
    code: Vec<u8>,
    compiled_entry: usize,
    osr_entries: Vec<(u32, usize)>,
    shadow_root_slots: u16,
    emitted_safepoints: usize,
    root_sync_sites: Vec<bliss_compiler::t2::emit::RootSyncSite>,
}

/// Validate the emitter's native-root synchronization contract before any T2
/// bytes become executable. A site count mismatch, out-of-range PC, oversized
/// root set, or register entry that could bypass the owning BlissStack frame is
/// stale/missing GC metadata and rejects installation under R4.46.
fn validate_t2_root_sync(activation_slots: u16, artifact: &T2Artifact) -> Option<u16> {
    if artifact.emitted_safepoints != artifact.root_sync_sites.len()
        || (artifact.shadow_root_slots != 0 && artifact.compiled_entry != 0)
        || artifact.root_sync_sites.iter().any(|site| {
            site.code_offset as usize >= artifact.code.len()
                || site.live_roots > artifact.shadow_root_slots
                || site.register_roots.checked_add(site.spill_roots) != Some(site.live_roots)
        })
        || artifact
            .root_sync_sites
            .windows(2)
            .any(|pair| pair[0].code_offset >= pair[1].code_offset)
    {
        return None;
    }
    activation_slots.checked_add(artifact.shadow_root_slots)
}

struct T2Completion {
    sym: u32,
    generation: u64,
    artifact: Option<T2Artifact>,
}

struct T2Job {
    input: T2CompileInput,
    completion: std::sync::mpsc::Sender<T2Completion>,
}

struct T2QueueState {
    jobs: Vec<T2Job>,
    capacity: usize,
}

struct T2CompileQueue {
    state: std::sync::Mutex<T2QueueState>,
    ready: std::sync::Condvar,
    dropped: std::sync::atomic::AtomicU64,
}

impl T2CompileQueue {
    fn submit(&self, job: T2Job) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.jobs.len() >= state.capacity {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return false;
        }
        state.jobs.push(job);
        self.ready.notify_one();
        true
    }

    fn take(&self) -> T2Job {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some((index, _)) = state
                .jobs
                .iter()
                .enumerate()
                .max_by_key(|(_, job)| job.input.priority)
            {
                return state.jobs.swap_remove(index);
            }
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn t2_compile_queue() -> &'static std::sync::Arc<T2CompileQueue> {
    use std::sync::{Arc, OnceLock};
    static QUEUE: OnceLock<Arc<T2CompileQueue>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let capacity = positive_env(&["BLISS_COMPILE_QUEUE_SIZE"], 64) as usize;
        let queue = Arc::new(T2CompileQueue {
            state: std::sync::Mutex::new(T2QueueState {
                jobs: Vec::new(),
                capacity,
            }),
            ready: std::sync::Condvar::new(),
            dropped: std::sync::atomic::AtomicU64::new(0),
        });
        let threads = positive_env(&["BLISS_T2_THREADS"], 2).min(32);
        for index in 0..threads {
            let worker_queue = Arc::clone(&queue);
            let _ = std::thread::Builder::new()
                .name(format!("bliss-t2-{index}"))
                .spawn(move || {
                    loop {
                        let job = worker_queue.take();
                        let sym = job.input.sym;
                        let generation = job.input.generation;
                        // A compiler bug must fail this request, not silently kill
                        // a worker and strand force/debug callers waiting forever.
                        let artifact =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                compile_t2_artifact(job.input)
                            }))
                            .ok()
                            .flatten();
                        let _ = job.completion.send(T2Completion {
                            sym,
                            generation,
                            artifact,
                        });
                    }
                });
        }
        queue
    })
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

fn positive_env(names: &[&str], default: u32) -> u32 {
    names
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|&n| n > 0)
        })
        .unwrap_or(default)
}

/// T0→T1 promotion threshold (invocations).  The stage-5 spec name is
/// preferred; `BLISS_T1_THRESHOLD` remains as a compatibility alias.
fn t1_threshold() -> u32 {
    positive_env(&["BLISS_T0_T1_THRESHOLD", "BLISS_T1_THRESHOLD"], 10)
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// T2 is part of normal tiering.  `BLISS_DISABLE_T2=1` is the explicit debug
/// off-switch; `BLISS_T2=0` is accepted for compatibility with the old gate.
fn t2_enabled() -> bool {
    if env_flag("BLISS_DISABLE_T2") == Some(true) {
        return false;
    }
    env_flag("BLISS_T2") != Some(false)
}

/// T1→T2 invocation threshold.  The old `BLISS_T2=1` opt-in is retained only
/// as a force/debug shorthand: absent an explicit threshold it makes T2 eligible
/// immediately after T1 has been observed.  Normal, unset operation uses 5000.
fn t2_invoke_threshold() -> u32 {
    let explicit = [
        "BLISS_T1_T2_INVOKE_THRESHOLD",
        "BLISS_T1_T2_THRESHOLD",
        "BLISS_T2_THRESHOLD",
    ]
    .iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0)
    });
    explicit.unwrap_or_else(|| {
        if env_flag("BLISS_T2") == Some(true) {
            t1_threshold()
        } else {
            5_000
        }
    })
}

/// T1→T2 loop-hotness threshold.  Both spellings used by the stage-5 specs
/// are accepted; this is independent of the lower OSR threshold machinery.
fn t2_backedge_threshold() -> u32 {
    positive_env(
        &[
            "BLISS_T1_T2_BACKEDGE_THRESHOLD",
            "BLISS_LOOP_HEAT_THRESHOLD",
        ],
        10_000,
    )
}

/// Return the current invocation count. Named functions have already had their
/// FnMeta counter bumped by the caller; anonymous bytecode functions need the
/// local fallback counter bumped here on every dispatch.
fn dispatch_invoke_count(sym: u32, fn_obj: Option<BlissVal>) -> u32 {
    match fn_obj {
        Some(f) => bliss_rt::function::invoke_count(f),
        None => INVOKE_COUNTS.with(|m| {
            let mut counts = m.borrow_mut();
            let count = counts.entry(sym).or_insert(0);
            *count = count.saturating_add(1);
            *count
        }),
    }
}

fn dispatch_invoke_count_snapshot(sym: u32, fn_obj: Option<BlissVal>) -> u32 {
    fn_obj
        .map(bliss_rt::function::invoke_count)
        .unwrap_or_else(|| INVOKE_COUNTS.with(|m| m.borrow().get(&sym).copied().unwrap_or(0)))
}

fn publish_native(fn_obj: Option<BlissVal>, nc: &NativeCode) {
    if let Some(f) = fn_obj {
        bliss_rt::function::set_entry(f, nc.entry as *mut u8);
        bliss_rt::function::set_tier(f, if nc.is_t2 { 2 } else { 1 });
    }
}

fn background_safe_body(body: &BytecodeFunction) -> bool {
    // A raw heap/cons literal can move while a worker is reading its snapshot.
    // Arguments and globals are not embedded in the job, and immediate
    // constants are inherently stable. Bodies with movable literals retain T1
    // until the GC exposes a cross-thread compilation-root handle.
    body.constants
        .iter()
        .all(|v| !v.is_cons() && !v.is_heap_object())
}

fn t2_completion_sender() -> std::sync::mpsc::Sender<T2Completion> {
    T2_COMPLETIONS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(std::sync::mpsc::channel());
        }
        slot.as_ref().unwrap().0.clone()
    })
}

fn snapshot_t2_input(sym: u32, priority: u64) -> Option<T2CompileInput> {
    let root = registry_get(sym)?;
    if !background_safe_body(&root) {
        t2_log_write(format_args!(
            "{}: movable literal cannot be rooted by background compiler => stay T1",
            root.name
        ));
        return None;
    }
    let root_ptr = Rc::as_ptr(&root) as usize;
    let type_profiles = TYPE_PROFILE.with(|profiles| {
        profiles
            .borrow()
            .iter()
            .filter_map(|(&(fp, bcp), profile)| (fp == root_ptr).then_some((bcp, *profile)))
            .collect()
    });
    let inline_bodies = REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter_map(|(&symbol, body)| {
                if !background_safe_body(body) {
                    return None;
                }
                let ptr = Rc::as_ptr(body) as usize;
                let (invocations, call_sites) = call_site_profile_snapshot(ptr);
                Some(T2BodySnapshot {
                    symbol,
                    body: (**body).clone(),
                    invocations,
                    call_sites,
                })
            })
            .collect()
    });
    let generation = REGISTRY_GENERATION.with(|g| g.borrow().get(&sym).copied().unwrap_or(0));
    Some(T2CompileInput {
        sym,
        generation,
        priority,
        body: (*root).clone(),
        type_profiles,
        inline_bodies,
    })
}

fn request_t2_compilation(sym: u32, priority: u64) -> bool {
    if T2_DECLINED.with(|s| s.borrow().contains(&sym))
        || T2_QUEUED.with(|s| s.borrow().contains_key(&sym))
    {
        return false;
    }
    let Some(input) = snapshot_t2_input(sym, priority) else {
        T2_DECLINED.with(|s| s.borrow_mut().insert(sym));
        return false;
    };
    let generation = input.generation;
    let job = T2Job {
        input,
        completion: t2_completion_sender(),
    };
    if !t2_compile_queue().submit(job) {
        t2_log_write(format_args!(
            "{}: background compilation queue full; retaining T1",
            sym_label(sym)
        ));
        return false;
    }
    T2_QUEUED.with(|s| {
        s.borrow_mut().insert(sym, generation);
    });
    t2_log_write(format_args!(
        "{}: queued for background T2 compilation",
        sym_label(sym)
    ));
    true
}

fn install_t2_completion(done: T2Completion) -> Option<Rc<NativeCode>> {
    let current_generation =
        REGISTRY_GENERATION.with(|g| g.borrow().get(&done.sym).copied().unwrap_or(0));
    if current_generation != done.generation {
        return None;
    }
    T2_QUEUED.with(|s| {
        let mut queued = s.borrow_mut();
        if queued.get(&done.sym) == Some(&done.generation) {
            queued.remove(&done.sym);
        }
    });
    let Some(artifact) = done.artifact else {
        T2_DECLINED.with(|s| s.borrow_mut().insert(done.sym));
        return None;
    };
    let bf = registry_get(done.sym)?;
    let total_slots = validate_t2_root_sync(bf.num_slots(), &artifact)?;
    let code_info = install_stack_map(total_slots)?;
    let buf = bliss_rt::jit::JitBuffer::new(&artifact.code)?;
    let entry = buf.leak();
    maybe_write_perf_map(entry as usize, artifact.code.len(), done.sym);
    let nc = Rc::new(NativeCode {
        entry,
        code_len: artifact.code.len(),
        is_t2: true,
        num_slots: total_slots,
        compiled_entry: artifact.compiled_entry,
        osr_entries: artifact.osr_entries.into_iter().collect(),
        code_info,
    });
    NATIVE_REGISTRY.with(|r| r.borrow_mut().insert(done.sym, Rc::clone(&nc)));
    let fn_obj = bliss_rt::symbols::symbol_function(done.sym)
        .filter(|&v| bliss_rt::function::is_interpreted_function(v));
    mark_fresh_promotion(done.sym);
    publish_native(fn_obj, &nc);
    t2_log_write(format_args!("{}: background T2 result published", bf.name));
    Some(nc)
}

fn poll_t2_completions() {
    loop {
        let done = T2_COMPLETIONS.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|(_, receiver)| receiver.try_recv().ok())
        });
        let Some(done) = done else { break };
        let _ = install_t2_completion(done);
    }
}

/// Safepoint used by observability tooling. Yielding gives a newly-woken
/// compiler thread a chance to run; publication itself still happens only when
/// this owner thread polls.
pub(super) fn poll_background_compilation() {
    std::thread::yield_now();
    poll_t2_completions();
    if T2_QUEUED.with(|queued| !queued.borrow().is_empty()) {
        let done = T2_COMPLETIONS.with(|slot| {
            slot.borrow().as_ref().and_then(|(_, receiver)| {
                receiver
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .ok()
            })
        });
        if let Some(done) = done {
            let _ = install_t2_completion(done);
            poll_t2_completions();
        }
    }
}

fn wait_for_t2(sym: u32) -> Option<Rc<NativeCode>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(nc) = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned()) {
            if nc.is_t2 {
                return Some(nc);
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let done = T2_COMPLETIONS.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|(_, receiver)| receiver.recv_timeout(remaining).ok())
        })?;
        let installed_sym = done.sym;
        let installed = install_t2_completion(done);
        if installed_sym == sym && installed.is_none() {
            return None;
        }
    }
}

fn mark_fresh_promotion(sym: u32) {
    PROMOTED_FRESH.with(|s| s.borrow_mut().insert(sym));
    LAST_FAILED_SPECULATION.with(|m| m.borrow_mut().remove(&sym));
    DEOPT_COUNTS.with(|m| {
        m.borrow_mut().insert(sym, 0);
    });
}

/// Drive the shipping tier state machine for one invocation.  A function with
/// no native entry may only transition to T1 during this dispatch; a later
/// dispatch observes T1 and may independently transition to T2.  A T2 compiler
/// decline leaves the existing T1 entry installed.
fn native_for_dispatch(
    sym: u32,
    fn_obj: Option<BlissVal>,
    invoke_count: u32,
) -> Option<Rc<NativeCode>> {
    // Publication is deliberately performed by the owning mutator: workers
    // compile relocatable bytes only and never touch thread-local registries or
    // executable mappings.
    poll_t2_completions();
    let current = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned());
    let Some(current) = current else {
        if invoke_count < t1_threshold() {
            return None;
        }
        let nc = try_promote_to_t1(sym)?;
        mark_fresh_promotion(sym);
        publish_native(fn_obj, &nc);
        return Some(nc);
    };

    if current.is_t2 || !t2_enabled() {
        return Some(current);
    }
    let back_edges = fn_obj.map(bliss_rt::function::back_edge_count).unwrap_or(0);
    if invoke_count < t2_invoke_threshold() && back_edges < t2_backedge_threshold() {
        return Some(current);
    }
    let priority = (u64::from(back_edges) << 32) | u64::from(invoke_count);
    let requested = request_t2_compilation(sym, priority);
    // Legacy BLISS_T2=1 remains a deterministic force/debug mode for tests and
    // disassembly sessions. Compilation still runs on a compiler thread; only
    // this requesting dispatch waits for publication.
    if env_flag("BLISS_T2") == Some(true)
        && (requested || T2_QUEUED.with(|queued| queued.borrow().contains_key(&sym)))
    {
        return wait_for_t2(sym).or(Some(current));
    }
    Some(current)
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
    let bf = registry_get(sym);
    if let Some(body) = bf.as_ref() {
        validate_declared_args(body, args)?;
    }
    // A function call starts a fresh multiple-values context. Argument
    // evaluation may have left secondary values active (for example GETHASH's
    // present-p value), but a native callee that simply returns its argument
    // must return exactly one value. Genuine multiple-value producers called
    // by the native body re-establish the state through their own call path.
    env.clear_mv();
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
        .ok_or_else(|| {
            BlissError::StackOverflow(
                bliss_rt::current_fiber_id()
                    .unwrap_or_else(|| bliss_rt::FiberId(bliss_rt::current_thread_id().0)),
            )
        })?;
    let env_frame = bf
        .as_ref()
        .and_then(|body| make_env_frame(body, Rc::clone(&env.frame)));
    // Bind both stack and captured parameters before entering native code. The
    // same heap frame is published through NATIVE_ENV_FRAME for environment
    // bytecodes and closure construction.
    if let Some(body) = bf.as_ref() {
        record_profiled_invocation(Rc::as_ptr(body) as usize);
    }
    if let Some(bf) = bf.as_ref().filter(|b| b.variadic) {
        if let Err(e) = bind_variadic(bf, frame, args, env_frame.as_ref(), env) {
            stack.pop_frame();
            return Err(e);
        }
    } else if let Some(body) = bf.as_ref() {
        bind_params(body, frame, args, env_frame.as_ref());
    } else {
        for (i, a) in args.iter().enumerate() {
            unsafe { slot_set(frame, i as u16, *a) };
        }
    }
    let slots = unsafe { frame.add(1) as *mut u64 };

    let saved = NATIVE_ENV.with(|e| e.replace(env as *mut Env));
    let saved_env_frame =
        NATIVE_ENV_FRAME.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), env_frame.clone()));
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
    NATIVE_ENV_FRAME.with(|slot| *slot.borrow_mut() = saved_env_frame);

    let deopt = NATIVE_DEOPT.with(|d| d.replace(false));
    let resume = NATIVE_DEOPT_RESUME.with(|c| c.borrow_mut().take());
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
        trace("native speculative deopt → interpreter");
        // Backoff/blacklist: an occasional deopt (a rare overflow) is fine and
        // the fast path stays installed. Repeated failures either indicate a
        // supported numeric phase change (retire this version and recompile) or
        // a genuinely unsupported domain (permanently blacklist speculation).
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
                // T1's optimistic arithmetic templates are always Fixnum. T2
                // follows the profile that was dominant when it was compiled.
                let forced = (!nc.is_t2).then_some(SpecType::Fixnum);
                let failed = decay_failed_speculation(Rc::as_ptr(&bf) as usize, forced);
                LAST_FAILED_SPECULATION.with(|m| {
                    m.borrow_mut().insert(sym, failed);
                });
            }
        }
        let threshold_hit = n >= deopt_blacklist_threshold();
        let phase_change = threshold_hit && supported_numeric_phase_change(sym);
        if t2_log_target().is_some() {
            let nm = registry_get(sym)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            t2_log_write(format_args!(
                "{nm}: native guard deopt #{n} => resuming interpreted{}",
                if phase_change {
                    " (threshold hit: supported numeric phase change => generic T1 + recompile)"
                } else if threshold_hit {
                    " (threshold hit: uninstall + blacklist => T0)"
                } else {
                    ""
                }
            ));
        }
        if phase_change {
            // The current T2 assumptions are stale, but native execution itself
            // is profitable. Replace it immediately with non-speculating T1 so
            // subsequent calls stay native while dispatch queues a profile-led
            // replacement T2 version.
            NATIVE_REGISTRY.with(|r| r.borrow_mut().remove(&sym));
            T2_DECLINED.with(|s| s.borrow_mut().remove(&sym));
            DEOPT_COUNTS.with(|m| m.borrow_mut().insert(sym, 0));
            PROMOTED_FRESH.with(|s| s.borrow_mut().remove(&sym));
            LAST_FAILED_SPECULATION.with(|m| m.borrow_mut().remove(&sym));
            let fn_obj = bliss_rt::symbols::symbol_function(sym)
                .filter(|&value| bliss_rt::function::is_interpreted_function(value));
            if let Some(fallback) = try_promote_to_t1_with_speculation(sym, false) {
                publish_native(fn_obj, &fallback);
                trace("stale numeric specialization retired → generic T1");
            } else if let Some(function) = fn_obj {
                bliss_rt::function::set_tier(function, 0);
            }
        } else if threshold_hit {
            NATIVE_REGISTRY.with(|r| r.borrow_mut().remove(&sym));
            DEOPT_BLACKLIST.with(|s| s.borrow_mut().insert(sym));
            if let Some(function) = bliss_rt::symbols::symbol_function(sym)
                .filter(|&value| bliss_rt::function::is_interpreted_function(value))
            {
                bliss_rt::function::set_tier(function, 0);
            }
            trace("t1 speculation blacklisted → staying T0");
        }
        let entry = registry_get(sym)
            .ok_or_else(|| BlissError::Internal("deopt: bytecode function vanished".into()))?;
        if let Some(resume) = resume {
            return match resume {
                NativeDeoptResume::Single { bcp, sp_top } => {
                    // State-transfer: resume T0 on this frame; `resume_in_t0`
                    // owns the frame's lifecycle from here (do NOT pop it first).
                    resume_in_t0(entry, frame, bcp, sp_top, sym, env_frame, env)
                }
                NativeDeoptResume::Inlined(scopes) => resume_inlined_in_t0(scopes, env),
            };
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
    env_frame: Option<Rc<RefCell<EnvFrame>>>,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let thread = bliss_rt::current_thread();
    let stack = thread.stack();

    let entry_fn_val = BlissVal::from_symbol_index(sym);
    let fn_obj = Some(entry_fn_val).filter(|&v| bliss_rt::function::is_interpreted_function(v));

    let handlers = rebuild_resume_handlers(&entry, bcp, env);

    let n_locals = entry.n_locals;
    let mut acts: Vec<Activation> = vec![Activation {
        frame,
        n_locals,
        env_frame,
        func: entry,
        bcp: bcp as usize,
        sp_top,
        handlers,
        cleanup_conts: Vec::new(),
        dyn_binds: Vec::new(),
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

fn rebuild_resume_handlers(entry: &BytecodeFunction, bcp: u32, env: &mut Env) -> Vec<Handler> {
    let mut handlers = Vec::new();
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
    handlers
}

/// Resume the interpreter with all logical frames reconstructed by an inlined
/// T2 deopt. The vector is outermost-to-innermost; `run_loop` therefore returns
/// through the same activation chain an uninlined execution would have used.
fn resume_inlined_in_t0(
    scopes: Vec<InlinedResumeScope>,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let stack = bliss_rt::current_thread().stack();
    let mut acts = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let entry = registry_get(scope.function).ok_or_else(|| {
            BlissError::Internal("deopt: inlined bytecode function vanished".into())
        })?;
        let handlers = rebuild_resume_handlers(&entry, scope.bcp, env);
        let fn_obj = bliss_rt::symbols::symbol_function(scope.function)
            .filter(|&v| bliss_rt::function::is_interpreted_function(v));
        acts.push(Activation {
            frame: scope.frame,
            n_locals: entry.n_locals,
            env_frame: None,
            func: entry,
            bcp: scope.bcp as usize,
            sp_top: scope.sp_top,
            handlers,
            cleanup_conts: Vec::new(),
            dyn_binds: Vec::new(),
            fn_obj,
            sym: scope.function,
        });
    }
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
    sym: u32,
    backedge_counter: u64,
) -> Option<(Vec<u8>, Vec<(u32, usize)>)> {
    macro_rules! decline_t1 {
        ($($reason:tt)*) => {{
            if std::env::var_os("BLISS_T1_TRACE").is_some() {
                eprintln!("[T1] {}: declined: {}", bf.name, format_args!($($reason)*));
            }
            return None;
        }};
    }
    if bf.arity > 6 {
        decline_t1!("required arity {} exceeds entry ABI limit 6", bf.arity);
    }
    // The activation lives in the BlissStack frame passed in rdi. r14 = frame
    // slots pointer; local i at [r14 + 8*i]; r15 = operand-stack pointer
    // (grows up from r14 + 8*n_locals). c2i preserves callee-saved r14/r15.
    let n_locals = bf.n_locals as i32;
    let local_disp = |i: i32| 8 * i;
    let c2i_addr =
        c2i_call_slice as extern "C" fn(u64, u64, *const BlissVal, u64) -> u64 as usize as u64;
    let clear_mv_addr = c2i_clear_mv as extern "C" fn() as usize as u64;
    let load_global_addr = c2i_load_global as extern "C" fn(u64) -> u64 as usize as u64;
    let store_global_addr = c2i_store_global as extern "C" fn(u64, u64) as usize as u64;
    let load_env_addr = c2i_load_env as extern "C" fn(u64, u64) -> u64 as usize as u64;
    let store_env_addr = c2i_store_env as extern "C" fn(u64, u64, u64) as usize as u64;
    let define_env_addr = c2i_define_env as extern "C" fn(u64, u64, u64) as usize as u64;
    let push_env_addr = c2i_push_env_child as extern "C" fn() as usize as u64;
    let pop_env_addr = c2i_pop_env_child as extern "C" fn() as usize as u64;
    let eval_host_addr = c2i_eval_host as extern "C" fn(u64) -> u64 as usize as u64;
    let make_closure_addr = c2i_make_closure as extern "C" fn(u64) -> u64 as usize as u64;
    let take_values_addr =
        c2i_take_values as extern "C" fn(u64, *mut BlissVal, u64) as usize as u64;
    let deopt_state_addr = c2i_deopt_state as extern "C" fn(u64, u64) as usize as u64;
    let t2_backedge_addr =
        c2i_t1_backedge as extern "C" fn(u64, u64, *mut u64) -> u64 as usize as u64;

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
    // T1 deopt is always precise state-transfer: every guard's deopt stub records
    // its (bcp, depth) via c2i_deopt_state and `run_native` resumes T0 at that
    // exact bcp, so an already-executed side effect (e.g. StoreGlobal) at an
    // earlier bcp is never re-run — the guarded ops themselves are pure fixnum
    // fast paths. Hence side-effecting functions may speculate freely, like C2
    // (bliss-izt.3). (The whole-function-rerun tail exists only for the T2
    // emitter's c2i_deopt, which is a separate, non-T1 path.)
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
                            FixnumPred::Zerop => 0x44,  // cmove  (== 0)
                            FixnumPred::Plusp => 0x4F,  // cmovg  (> 0)
                            FixnumPred::Minusp => 0x4C, // cmovl (< 0)
                            FixnumPred::Evenp => 0x44,  // cmove  (bit clear)
                            FixnumPred::Oddp => 0x45,   // cmovne (bit set)
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
                            c.extend_from_slice(&[0x48, 0x8B, 0x40, offset as u8]);
                            // mov rax, [rax+8]
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
                                c.extend_from_slice(
                                    &(bliss_rt::value::NIL_BITS as u32).to_le_bytes(),
                                );
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
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp);
                                        // jo deopt
                                    }
                                    FixnumOp::Sub => {
                                        c.extend_from_slice(&[0x48, 0x29, 0xC8]); // sub rax, rcx
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp);
                                        // jo deopt
                                    }
                                    FixnumOp::Mul => {
                                        // Untag a0, then a0 * (a1<<3) = (a0*a1)<<3.
                                        c.extend_from_slice(&[0x48, 0xC1, 0xF8, 0x03]); // sar rax, 3
                                        c.extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC1]); // imul rax, rcx
                                        jcc_deopt(&mut c, &mut deopt_labels, Cc::O, bcp);
                                        // jo deopt
                                    }
                                    _ => unreachable!(),
                                }
                                commit_bin(&mut c);
                            }
                            FixnumOp::Lt
                            | FixnumOp::Gt
                            | FixnumOp::Le
                            | FixnumOp::Ge
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
                // c2i_call_slice(sym, n, args, profile): arguments are already
                // contiguous below r15 in the activation's BlissStack operand
                // area. This supports arbitrary CL arity without copying values
                // into a fixed set of host ABI registers.
                c.extend_from_slice(&[0x48, 0xBF]); // mov rdi, imm64 (sym)
                c.extend_from_slice(&(*sym as u64).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xBE]); // mov rsi, imm64 (nargs)
                c.extend_from_slice(&(*nargs as u64).to_le_bytes());
                c.extend_from_slice(&[0x49, 0x8D, 0x97]); // lea rdx,[r15 - 8*nargs]
                c.extend_from_slice(&(-8_i32 * i32::from(*nargs)).to_le_bytes());
                let profile_site = if registry_get(*sym).is_some() {
                    call_site_profile_token(bf as *const BytecodeFunction as usize, bcp as u32)
                } else {
                    0
                };
                c.extend_from_slice(&[0x48, 0xB9]); // mov rcx, imm64 (profile site)
                c.extend_from_slice(&profile_site.to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i)
                c.extend_from_slice(&c2i_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]); // call rax
                c.extend_from_slice(&[0x49, 0x81, 0xEF]); // sub r15, 8*nargs
                c.extend_from_slice(&(8_i32 * i32::from(*nargs)).to_le_bytes());
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
            Instr::LoadEnvVar(name_idx) => {
                c.push(0xBF); // mov edi, function symbol
                c.extend_from_slice(&sym.to_le_bytes());
                c.push(0xBE); // mov esi, name index
                c.extend_from_slice(&u32::from(*name_idx).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]);
                c.extend_from_slice(&load_env_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]);
                push_rax(&mut c);
            }
            Instr::StoreEnvVar(name_idx) | Instr::DefineEnvVar(name_idx) => {
                pop_into(&mut c, 2, false); // value -> rdx
                c.push(0xBF); // mov edi, function symbol
                c.extend_from_slice(&sym.to_le_bytes());
                c.push(0xBE); // mov esi, name index
                c.extend_from_slice(&u32::from(*name_idx).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]);
                let helper = if matches!(instr, Instr::StoreEnvVar(_)) {
                    store_env_addr
                } else {
                    define_env_addr
                };
                c.extend_from_slice(&helper.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]);
            }
            Instr::PushEnvChild | Instr::PopEnvChild => {
                c.extend_from_slice(&[0x48, 0xB8]);
                let helper = if matches!(instr, Instr::PushEnvChild) {
                    push_env_addr
                } else {
                    pop_env_addr
                };
                c.extend_from_slice(&helper.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]);
            }
            Instr::EvalHost(index) | Instr::MakeClosureEnv(index) => {
                let form = bf.constants.get(*index as usize)?.0;
                c.extend_from_slice(&[0x48, 0xBF]); // mov rdi, form
                c.extend_from_slice(&form.to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]);
                let helper = if matches!(instr, Instr::EvalHost(_)) {
                    eval_host_addr
                } else {
                    make_closure_addr
                };
                c.extend_from_slice(&helper.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]);
                push_rax(&mut c);
            }
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                pop_into(&mut c, 7, false); // primary -> rdi
                c.extend_from_slice(&[0x49, 0x8D, 0xB6]); // lea rsi,[r14+slot]
                c.extend_from_slice(&local_disp(*slot_base as i32).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xBA]); // mov rdx, nvars
                c.extend_from_slice(&u64::from(*nvars).to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]);
                c.extend_from_slice(&take_values_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]);
            }
            // Read a global/special var's value cell → push (bliss-x5y.15). Same
            // non-allocating call contract as ClearMv (r14/r15 callee-saved).
            Instr::LoadGlobal(sym) => {
                c.extend_from_slice(&[0xBF]); // mov edi, imm32 (sym) — zero-extends
                c.extend_from_slice(&sym.to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i_load_global)
                c.extend_from_slice(&load_global_addr.to_le_bytes());
                c.extend_from_slice(&[0xFF, 0xD0]); // call rax
                push_rax(&mut c); // result → operand stack
            }
            // Pop the value, write it to the global/special cell (bliss-x5y.15).
            // Consumes the operand and pushes nothing (SETQ then reloads for its
            // value). A side effect, so the function is not speculated.
            Instr::StoreGlobal(sym) => {
                pop_into(&mut c, 6, false); // value → rsi (arg 2)
                c.extend_from_slice(&[0xBF]); // mov edi, imm32 (sym, arg 1)
                c.extend_from_slice(&sym.to_le_bytes());
                c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64 (c2i_store_global)
                c.extend_from_slice(&store_global_addr.to_le_bytes());
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
                    decline_t1!("GO references non-local tagbody {tagbody_id}");
                }
                if (*target_bcp as usize) < bcp_idx
                    && tag_sp.get(tagbody_id) == Some(&0)
                    && sym != u32::MAX
                    && backedge_counter != 0
                {
                    let keep_t1 = c.label();
                    c.extend_from_slice(&[0x48, 0xB8]); // mov rax, counter
                    c.extend_from_slice(&backedge_counter.to_le_bytes());
                    c.extend_from_slice(&[0x83, 0x00, 0x01]); // add dword [rax],1
                    c.extend_from_slice(&[0x81, 0x38]); // cmp dword [rax],imm32
                    c.extend_from_slice(&t2_backedge_threshold().to_le_bytes());
                    c.jcc(Cc::L, keep_t1);
                    c.extend_from_slice(&[0xC7, 0x00, 0, 0, 0, 0]); // reset sample
                    c.push(0xBF); // mov edi,sym
                    c.extend_from_slice(&sym.to_le_bytes());
                    c.push(0xBE); // mov esi,header bcp
                    c.extend_from_slice(&target_bcp.to_le_bytes());
                    c.extend_from_slice(&[0x4C, 0x89, 0xF2]); // mov rdx,r14 slots
                    c.extend_from_slice(&[0x48, 0xB8]);
                    c.extend_from_slice(&t2_backedge_addr.to_le_bytes());
                    c.extend_from_slice(&[0xFF, 0xD0]); // call callback
                    c.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax,rax
                    c.jcc(Cc::E, keep_t1);
                    // T2 finished or deoptimized. Its result/dummy result was
                    // stored in the first operand slot for the shared epilogue.
                    c.extend_from_slice(&[0x49, 0x8B, 0x86]);
                    c.extend_from_slice(&(8 * n_locals).to_le_bytes());
                    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]);
                    c.extend_from_slice(&[0x41, 0x5F, 0x41, 0x5E, 0xC3]);
                    c.bind(keep_t1);
                }
                c.jmp(*bcp_labels.get(*target_bcp as usize)?);
            }
            Instr::ReturnFrom { block_id } => {
                // The return value is on top of the operand stack. Restore the
                // block's entry depth, re-push the value (the block yields it),
                // and jump to the block's resume point.
                let (resume_bcp, sp) = match block_targets.get(block_id) {
                    Some(&t) => t,
                    None => decline_t1!("RETURN-FROM references non-local block {block_id}"),
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
            unsupported => decline_t1!("unsupported opcode at bcp {bcp}: {unsupported:?}"),
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
    _sym: u32,
    _backedge_counter: u64,
) -> Option<(Vec<u8>, Vec<(u32, usize)>)> {
    None
}

/// Try to promote `sym`'s bytecode function to T1 native code (install into
/// executable memory). Returns the installed code, or `None` if it can't be
/// compiled to native.
fn try_promote_to_t1(sym: u32) -> Option<Rc<NativeCode>> {
    try_promote_to_t1_with_speculation(sym, true)
}

/// Build T1 with or without its optimistic fixnum templates. The
/// non-speculating form is the stable native fallback while a replacement T2
/// version is compiled for a newly observed numeric phase.
fn try_promote_to_t1_with_speculation(sym: u32, allow_speculation: bool) -> Option<Rc<NativeCode>> {
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
    let backedge_counter = Box::leak(Box::new(std::sync::atomic::AtomicU32::new(0)))
        as *mut std::sync::atomic::AtomicU32 as usize as u64;
    let (code, _osr) = emit_native_x86(&bf, allow_speculation, sym, backedge_counter)?;
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
        osr_entries: HashMap::new(),
        code_info,
    });
    NATIVE_REGISTRY.with(|r| r.borrow_mut().insert(sym, Rc::clone(&nc)));
    Some(nc)
}

/// Try to compile `sym` to T2 optimising native code (profile-guided single-type
/// speculation). The tier state machine calls this only after an installed T1
/// reaches an invocation/back-edge threshold. Returns `None` (retain T1) when
/// the function's shape is beyond the framed emitter. Installs like T1 and
/// shares the run_native ABI, so dispatch and deopt are identical.
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

fn compile_t2_artifact(input: T2CompileInput) -> Option<T2Artifact> {
    let sym = input.sym;
    let bf = &input.body;
    // T2's entry sequence (build.rs::seed_entry) binds only the fixed positional
    // parameters — locals `0..arity` — and leaves every other local NIL. It has
    // no support for collecting &optional/&rest/&key arguments, so a variadic
    // function compiled to T2 sees an EMPTY &rest: e.g. UIOP's STRCAT sums the
    // lengths of an empty list, make-string gets NIL, and the load aborts with
    // "NIL is not of type non-negative string size". T1's bind_variadic handles
    // these correctly, so keep variadic functions at T1 until T2 grows a variadic
    // entry (bliss captured-variadic-param follow-up).
    if bf.variadic {
        t2_log!(
            "{}: variadic lambda list not modelled by T2 entry => stay T1",
            bf.name
        );
        return None;
    }
    // (StoreGlobal functions used to be declined here, before T2 had precise
    // deopt. That is no longer needed: emit_framed now lowers SetSymbolValue and
    // gives every guard precise state-transfer deopt (bliss-izt.3, bliss-mzp), so
    // a global-accumulator loop optimises at T2 without double-applying its store.)
    let name = bf.name.clone();
    t2_log!("{name}: considering for T2 (arity {})", bf.arity);
    if t2_log_target().is_some() {
        for (bcp, p) in &input.type_profiles {
            t2_log!(
                "{name}: site bcp={bcp} profile fix={} float={} other={} => {:?}",
                p.fixnum,
                p.single_float,
                p.other,
                p.dominant()
            );
        }
    }

    // The profile closure the speculative-lowering pass consumes: this function's
    // observed operand type at each call-site bcp, mapped to the compiler's type.
    // A site with a clear dominant type speculates that type. A site with samples
    // but no dominant type is genuinely polymorphic → left generic. A COLD site
    // (no samples — e.g. a branch not taken during the brief profiling window)
    // optimistically guesses fixnum: the guard makes it safe (a wrong guess just
    // deopts), and it stops one cold branch from declining the whole function.
    let profile = |bcp: u32| -> Option<bliss_compiler::t2::speculate::SpecType> {
        use bliss_compiler::t2::speculate::SpecType as Bc;
        match input.type_profiles.get(&bcp).copied() {
            Some(p) => match p.dominant() {
                Some(SpecType::Fixnum) => Some(Bc::Fixnum),
                Some(SpecType::SingleFloat) => Some(Bc::SingleFloat),
                // Below the confidence threshold but UNANIMOUS (only one numeric
                // type ever seen, no mixed): speculate it anyway — the guard makes
                // a wrong guess safe, and this covers lightly-exercised sites like a
                // deep-recursion multiply that runs few times before promotion.
                None if p.other == 0 && p.single_float == 0 && p.fixnum > 0 => Some(Bc::Fixnum),
                None if p.other == 0 && p.fixnum == 0 && p.single_float > 0 => {
                    Some(Bc::SingleFloat)
                }
                None => None, // genuinely mixed/polymorphic
            },
            None => Some(Bc::Fixnum), // cold: optimistic, guarded fixnum guess
        }
    };

    let mut inline_options =
        bliss_compiler::t2::inlining::InlineOptions::default().with_root_symbol(sym);
    for saved in input.inline_bodies {
        let T2BodySnapshot {
            symbol,
            body,
            invocations,
            call_sites,
        } = saved;
        inline_options = inline_options.with_body(symbol, Rc::new(body));
        for (bcp, calls) in call_sites {
            inline_options = inline_options.with_call_site_profile(symbol, bcp, calls, invocations);
        }
    }
    let mut f = match bliss_compiler::t2::build::build_from_bytecode_with_inline_options(
        bf,
        inline_options,
    ) {
        Ok(f) => f,
        Err(e) => {
            t2_log!("{name}: build_from_bytecode failed: {e:?} => stay T1");
            return None;
        }
    };
    let speculated = bliss_compiler::t2::speculate::speculate(&mut f, &profile);
    // Speculation is an ENHANCEMENT, not the admission test for T2. Even with no
    // speculatable arithmetic site, T2's mid-end (const-fold + GVN + DCE) still
    // optimises the code beyond the T1 baseline template JIT — so we proceed and
    // let the emitter decide. If `emit_framed` can't yet handle this function's
    // shape (branches / unsupported ops — the first-cut emitter), it errors below
    // and we fall back to T1, exactly as before. (bliss-x5y: this decouples the
    // optimizer from speculation; measure how much of real code emit_framed takes.)
    if speculated == 0 {
        t2_log!("{name}: 0 speculatable sites — compiling generic-optimized (no speculation)");
    } else {
        t2_log!("{name}: speculated {speculated} site(s)");
    }

    // Route the speculated IR through the mid-end: constant folding + strength
    // reduction (P4f), global value numbering (P4a), dominance-based guard
    // elimination (P4d), then deopt-aware DCE (P4c).  Guard elimination runs
    // after body inlining in the builder, so equivalent proofs cloned from
    // separate callees collapse to one dominating guard.
    // Each pass preserves well-formedness (spec §4.10 R4.60); re-verify before
    // emitting, and decline T2 (fall back to T1) if anything went wrong.
    {
        use bliss_compiler::t2::pass::PassManager;
        let mut pm = PassManager::new();
        pm.add(Box::new(bliss_compiler::t2::opt_fold::ConstFold));
        pm.add(Box::new(bliss_compiler::t2::opt_gvn::Gvn));
        pm.add(Box::new(bliss_compiler::t2::opt_guard::GuardElim));
        pm.add(Box::new(bliss_compiler::t2::opt_dce::Dce));
        pm.run(&mut f);
    }
    if let Err(e) = bliss_compiler::t2::verify::verify(&f) {
        t2_log!("{name}: post-mid-end verify failed: {e:?} => stay T1");
        return None;
    }

    let deopt_addr = c2i_deopt as extern "C" fn() as usize as u64;
    let deopt_t2_addr = c2i_deopt_t2 as extern "C" fn(u64, u64, *const u64, u64) as usize as u64;
    let call_addr = c2i_call as extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 as usize as u64;
    let load_global_addr = c2i_load_global as extern "C" fn(u64) -> u64 as usize as u64;
    let store_global_addr = c2i_store_global as extern "C" fn(u64, u64) as usize as u64;
    let clear_mv_addr = c2i_clear_mv as extern "C" fn() as usize as u64;
    let framed = match bliss_compiler::t2::emit::emit_framed_with_activation_slots(
        &f,
        deopt_addr,
        deopt_t2_addr,
        call_addr,
        load_global_addr,
        store_global_addr,
        clear_mv_addr,
        bf.num_slots(),
        Some(sym),
    ) {
        Ok(fc) => fc,
        Err(e) => {
            t2_log!("{name}: emit_framed failed: {e:?} (shape beyond emitter) => stay T1");
            return None;
        }
    };
    let code = framed.code;
    t2_log!(
        "{name}: T2 INSTALLED — {} bytes, compiled_entry=+{}, spill_slots={} (regalloc2={}), edits={}, gc_shadow_slots={}, safepoints={}",
        code.len(),
        framed.compiled_entry,
        framed.native_spill_slots,
        framed.regalloc_spill_slots,
        framed.allocation_edits,
        framed.shadow_root_slots,
        framed.emitted_safepoints,
    );

    Some(T2Artifact {
        code,
        compiled_entry: framed.compiled_entry,
        osr_entries: framed.osr_entries,
        shadow_root_slots: framed.shadow_root_slots,
        emitted_safepoints: framed.emitted_safepoints,
        root_sync_sites: framed.root_sync_sites,
    })
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
        let (code, osr) = emit_native_x86(&bf, true, u32::MAX, 0)?;
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
    let resume = NATIVE_DEOPT_RESUME.with(|c| c.borrow_mut().take());
    let my_err = NATIVE_ERROR.with(|c| c.borrow_mut().take());
    NATIVE_ERROR.with(|c| *c.borrow_mut() = saved_err);
    let _ = osr.num_slots;
    let _ = osr.code_info;
    if let Some(err) = my_err {
        return Err(err);
    }
    if deopt {
        if let Some(NativeDeoptResume::Single { bcp, sp_top }) = resume {
            return Ok(OsrOutcome::Deopt { bcp, sp_top });
        }
        // No resume point recorded — should not happen for OSR (speculation
        // always records one), but treat it as a benign no-progress signal by
        // resuming at the loop's back-edge target is not possible here, so fall
        // through to Finished with the returned (dummy) value would be wrong.
        // Instead surface an internal error to avoid silently returning garbage.
        return Err(BlissError::Internal(
            "OSR deopt without resume point".into(),
        ));
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
            Handler::HandlerCase { cluster_base, .. } | Handler::HandlerBind { cluster_base } => {
                env.handlers.truncate(cluster_base)
            }
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
/// Top-level definition operators that are evaluated directly rather than
/// thunk-compiled: they run once to register a definition and never compile
/// (bliss-x5y.6 follow-up). Matched on the bare (package-stripped) name.
fn is_toplevel_definer(name: &str) -> bool {
    matches!(
        symbol_bare_name(name).as_str(),
        "DEFMACRO"
            | "DEFVAR"
            | "DEFPARAMETER"
            | "DEFCONSTANT"
            | "DEFINE-SYMBOL-MACRO"
            | "DEFINE-COMPILER-MACRO"
            | "DEFINE-SETF-EXPANDER"
            | "DEFSETF"
            // CLOS definers: register directly. Their bodies (a method's body,
            // a slot :initform) still run tree-walked — compiling method bodies
            // is a separate task — but this skips the wasted thunk-compile attempt
            // that macro-expanded (progn (defmethod …) …) groups otherwise pay on
            // every load (bliss-1xw).
            | "DEFCLASS"
            | "DEFGENERIC"
            | "DEFMETHOD"
            // Package operations: run once for effect (never usefully compiled).
            // They already reach eval_form via the thunk bail; short-circuiting
            // skips the wasted compile attempt on every top-level occurrence.
            | "IN-PACKAGE"
            | "DEFPACKAGE"
    )
}

pub fn eval_toplevel(mut form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    if !backend_is_bytecode() {
        return eval_form(form, env);
    }
    let _form_root = bliss_rt::gc::StackRoot::new(&mut form);

    // Macroexpand a top-level macro call before dispatching (CLHS 3.2.3.1): a
    // macro that expands to `(progn (defun …) …)` — asdf/UIOP's with-upgradability
    // and friends — must have its nested definitions processed as top-level forms,
    // not hidden inside a bailing thunk (bliss-1xw). Recursing on the expansion
    // loops through chained macros and then hits the progn/eval-when/definer/defun
    // cases below. Only the OUTER form is expanded here; inner forms are expanded
    // by the lowerer as before, so no form is double-expanded.
    if form.is_cons() {
        let (op, _) = cp(form);
        if op.is_symbol() {
            let name = sym_name(op);
            if super::macro_defined(env, &name) {
                let menv = super::macroexpand_environment_from_cli(env);
                if let Ok((expanded, true)) = compiler_macroexpand::macroexpand_1(form, &menv) {
                    return eval_toplevel(expanded, env);
                }
            }
        }
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
                let forms = bliss_rt::gc::HostRoot::new(list_to_vec(body));
                for index in 0..forms.len() {
                    last = eval_toplevel(forms[index], env)?;
                }
                return Ok(last);
            }
            return Ok(NIL);
        }
        // A top-level PROGN / LOCALLY: per CLHS 3.2.3.1 its subforms are
        // themselves top-level forms, so process each recursively (bliss-1xw).
        // Otherwise a wrapping `(progn (defun …) …)` — or an `(eval-when …
        // (progn (defun …)))` after the recursion above — hides the nested
        // definitions inside a thunk, which bails and tree-walks them. LOCALLY's
        // leading `(declare …)` forms recurse to a harmless NIL.
        if op.is_symbol() {
            let bare = symbol_bare_name(&sym_name(op));
            if bare == "PROGN" || bare == "LOCALLY" {
                let mut last = NIL;
                let forms = bliss_rt::gc::HostRoot::new(list_to_vec(cdr));
                for index in 0..forms.len() {
                    last = eval_toplevel(forms[index], env)?;
                }
                return Ok(last);
            }
        }
    }

    // A top-level `defun`: let the tree-walker register it (so the oracle and
    // host-fallback path both see it), then compile a bytecode version so
    // calls to it run as native frames.
    if let Some((name, params, body)) = as_defun(form) {
        let roots = bliss_rt::ShadowRootScope::new();
        let form = roots.root(form);
        let params = roots.root(params);
        let body = roots.root(body);
        let result = eval_form(form.get(), env)?;
        if let Some(sym) = symbol_index_of(&name) {
            reset_last_bail_reason();
            match compile_function(
                &name,
                params.get(),
                body.get(),
                env,
                false,
                false,
            ) {
                Some(bf) => {
                    trace("compiled");
                    trace_named(&name, "compiled", None);
                    registry_put(sym, Rc::new(bf));
                }
                // Redefinition that no longer compiles must not leave stale
                // bytecode behind — drop it so calls fall back to the tree-walker.
                None => {
                    trace("bailed");
                    trace_named(&name, "bailed", last_bail_reason().as_deref());
                    registry_remove(sym);
                }
            }
        }
        return Ok(result);
    }

    // A top-level definer (defmacro/defvar/defparameter/defconstant/define-*):
    // these only ever run once, to register the definition, and never compile as
    // a thunk (they are bail-specials). Evaluate directly so we neither waste a
    // thunk-compile attempt nor record a spurious bail (bliss-x5y.6 follow-up).
    // Note: this does NOT compile a defmacro's EXPANDER to bytecode — that needs
    // macro-lambda-list support (destructuring/&whole/&environment/&body) and is
    // a separate task; the expander stays tree-walked.
    if form.is_cons() {
        let (op, _) = cp(form);
        if op.is_symbol() && is_toplevel_definer(&sym_name(op)) {
            return eval_form(form, env);
        }
    }

    // Any other form: compile a thunk, else fall back.
    match compile_thunk(form, env, false) {
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

/// Optional per-definition compile trace. Kept separate from
/// `BLISS_BYTECODE_TRACE` so its stable one-word output remains suitable for
/// tests while workload investigations can identify the exact function and
/// final lowering reason.
fn trace_named(name: &str, what: &str, reason: Option<&str>) {
    if std::env::var_os("BLISS_BYTECODE_TRACE_NAMES").is_none() {
        return;
    }
    match reason {
        Some(reason) => eprintln!("[bytecode] {name}: {what} ({reason})"),
        None => eprintln!("[bytecode] {name}: {what}"),
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
    let (name_form, rest) = cp(rest);
    let (params, body) = cp(rest);
    if name_form.is_symbol() {
        return Some((sym_name(name_form), params, body));
    }
    // `(defun (setf place) …)`: install the writer under the canonical
    // BLISS-INTERNAL setf-writer symbol so the SETF store path can dispatch it.
    if name_form.is_cons() {
        let (head, tail) = cp(name_form);
        if head.is_symbol()
            && symbol_bare_name(&sym_name(head)) == "SETF"
            && tail.is_cons()
        {
            let (place, _) = cp(tail);
            if place.is_symbol() {
                return Some((
                    super::setf_writer_symbol_name(&sym_name(place)),
                    params,
                    body,
                ));
            }
        }
    }
    None
}

/// Match `(DEFMACRO name lambda-list . body)` or
/// `(DEFINE-COMPILER-MACRO name lambda-list . body)`.
fn as_macro_definition(form: BlissVal) -> Option<(bool, String, BlissVal, BlissVal)> {
    if !form.is_cons() {
        return None;
    }
    let (op, rest) = cp(form);
    if !op.is_symbol() {
        return None;
    }
    let compiler_macro = match symbol_bare_name(&sym_name(op)).as_str() {
        "DEFMACRO" => false,
        "DEFINE-COMPILER-MACRO" => true,
        _ => return None,
    };
    let (name_sym, rest) = cp(rest);
    if !name_sym.is_symbol() {
        return None;
    }
    let (params, body) = cp(rest);
    Some((compiler_macro, sym_name(name_sym), params, body))
}

/// Match `(DEFINE-SETF-EXPANDER access-fn lambda-list . body)`, returning the
/// accessor name, the expander lambda list, and the body.
fn as_setf_expander_definition(form: BlissVal) -> Option<(String, BlissVal, BlissVal)> {
    if !form.is_cons() {
        return None;
    }
    let (op, rest) = cp(form);
    if !op.is_symbol() || symbol_bare_name(&sym_name(op)) != "DEFINE-SETF-EXPANDER" {
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

    #[test]
    fn t2_install_rejects_missing_or_stale_native_root_sync_metadata() {
        let valid = T2Artifact {
            code: vec![0x90; 8],
            compiled_entry: 0,
            osr_entries: vec![],
            shadow_root_slots: 2,
            emitted_safepoints: 1,
            root_sync_sites: vec![bliss_compiler::t2::emit::RootSyncSite {
                code_offset: 3,
                live_roots: 2,
                register_roots: 1,
                spill_roots: 1,
            }],
        };
        assert_eq!(validate_t2_root_sync(3, &valid), Some(5));

        let mut missing = valid;
        missing.emitted_safepoints = 2;
        assert_eq!(validate_t2_root_sync(3, &missing), None);

        missing.emitted_safepoints = 1;
        missing.root_sync_sites[0].spill_roots = 0;
        assert_eq!(validate_t2_root_sync(3, &missing), None);

        missing.root_sync_sites[0].spill_roots = 1;
        missing.root_sync_sites[0].code_offset = missing.code.len() as u32;
        assert_eq!(validate_t2_root_sync(3, &missing), None);
    }

    #[test]
    fn native_call_site_token_updates_the_shared_frequency_window() {
        let func_ptr = 0x5a17_0000usize;
        record_profiled_invocation(func_ptr);
        record_profiled_invocation(func_ptr);
        let token = call_site_profile_token(func_ptr, 9);
        record_native_call_site(token);
        record_native_call_site(token);

        let (invocations, sites) = call_site_profile_snapshot(func_ptr);
        assert_eq!(invocations, 2);
        assert_eq!(sites, vec![(9, 2)]);
        clear_bytecode_profiles(func_ptr);
    }

    #[test]
    fn bbu_v11_registry_key_symbol_tag_remains_readable() {
        let mut cursor = BbuCursor::new(&[14, 3, 0, 0, 0]);
        assert!(matches!(
            parse_bbu_constant(&mut cursor, 0x0101).unwrap(),
            BbuConstant::LegacySymbol(3)
        ));
        assert!(cursor.done());
    }
}
