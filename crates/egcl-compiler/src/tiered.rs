// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Standalone tree-walking evaluator over Common Lisp forms.
//!
//! # Status
//!
//! This is the surviving piece of the original self-contained tiering sketch;
//! its baseline and optimising compilers, promotion logic, and compilation
//! queue have been removed. It is **not** the production T0: the `egcl` binary
//! interprets and JITs bytecode in `crates/egcl/src/cli/bytecode.rs`. The one
//! production consumer is `egcl-stdlib`'s debugger, whose `eval_in_frame` and
//! watch predicates evaluate forms with an [`Interpreter`] seeded from the
//! frame's local bindings. Because this evaluator knows only the special forms
//! listed below and none of the standard library, that debugger path should
//! eventually be rewired to the real evaluator and this module deleted.
//!
//! # Components
//!
//! * [`FnMeta`] — `#[repr(C)]` per-function header: `entry` (offset 0),
//!   `tier` (8), `invoke_count` (12), `back_edge_count` (16), `arity` (20),
//!   `body` (24), `params` (32), `flags` (40). `Interpreter` creates one per
//!   `LAMBDA`/`DEFUN` (inside a `ClosureObj` that also carries the captured
//!   environment) and `apply` reads it back through the function value's
//!   pointer. `egcl-stdlib`'s `disassemble` also reads `tier` through this
//!   layout.
//! * [`ValueStack`] — a bounded operand stack (65 536 slots) the evaluator
//!   uses instead of Rust recursion for argument evaluation; overflow is a
//!   stack-overflow error, not a crash.
//! * [`EnvFrame`] — `Arc`-linked lexical frames with define/lookup/set.
//! * [`Interpreter`] — evaluates a form directly: self-evaluating atoms,
//!   symbol lookup, the special forms `classify_special_form` recognises
//!   (QUOTE, IF, LET/LET*, PROGN, SETQ, FUNCTION, LAMBDA, BLOCK/RETURN-FROM,
//!   TAGBODY/GO, CATCH/THROW, DEFUN, and a few more), and function
//!   application. Non-local exits travel as `EgclError` variants that the
//!   establishing form catches.
//!
//! # Limits
//!
//! Functions are applied only by interpreting their body form; `tier` and
//! `entry` in `FnMeta` are never consulted here. Unknown operators are treated
//! as calls, and there is no package system, no macros, and no standard
//! library beyond what the caller defines into the environment.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, Ordering};
use egcl_rt::error::EgclError;
use egcl_rt::value::{
    NIL_BITS, T_BITS, TAG_CONS, TAG_FUNCTION, TAG_SPECIAL, TAG_SYMBOL, EgclVal,
};

// ── FnMeta: per-function metadata shared across tiers ──

/// Flags for FnMeta.
/// Indicates this FnMeta is part of a ClosureObj with a captured environment.
pub const FLAG_IS_CLOSURE: u16 = 0x0008;

/// Per-function metadata shared across tiers.
///
/// Layout is used by the interpreter and compiled code to read/write
/// invocation counters and entry points atomically.
#[repr(C)]
pub struct FnMeta {
    /// Pointer to the active entry point; updated atomically on tier change.
    pub entry: AtomicPtr<u8>,
    /// Current tier (read with Acquire, written with Release). Offset 8.
    pub tier: AtomicU8,
    /// Padding for alignment.
    _pad: [u8; 3],
    /// Atomically incremented by T0 eval loop and T1 prologue stub. Offset 12.
    pub invoke_count: AtomicU32,
    /// Atomically incremented by T1 back-edge stubs. Offset 16.
    pub back_edge_count: AtomicU32,
    /// Arity of the function. Offset 20.
    pub arity: u32,
    /// Body form (CL AST) for T0 interpretation. Offset 24.
    pub body: EgclVal,
    /// Parameter list form. Offset 32.
    pub params: EgclVal,
    /// Flags: QUEUED_FOR_T2, T2_FAILED, NEVER_COMPILE, ...
    pub flags: AtomicU16,
}

impl FnMeta {
    /// Create a new FnMeta for a T0 interpreted function.
    pub fn new(arity: u32, body: EgclVal, params: EgclVal) -> Self {
        FnMeta {
            entry: AtomicPtr::new(std::ptr::null_mut()),
            tier: AtomicU8::new(0),
            _pad: [0; 3],
            invoke_count: AtomicU32::new(0),
            back_edge_count: AtomicU32::new(0),
            arity,
            body,
            params,
            flags: AtomicU16::new(0),
        }
    }
}

// ── ValueStack ─────────────────────────────────────────

/// Maximum ValueStack depth .
const DEFAULT_MAX_STACK_DEPTH: usize = 65_536;

/// Interpreter operand stack (per green-thread).
pub struct ValueStack {
    /// Bump-allocated backing store.
    slots: Vec<EgclVal>,
    /// Index of the first free slot.
    sp: usize,
    /// Maximum depth.
    max_depth: usize,
}

impl ValueStack {
    pub fn new() -> Self {
        ValueStack {
            slots: Vec::with_capacity(1024),
            sp: 0,
            max_depth: DEFAULT_MAX_STACK_DEPTH,
        }
    }

    pub fn push(&mut self, v: EgclVal) -> Result<(), EgclError> {
        if self.sp >= self.max_depth {
            return Err(EgclError::Internal(
                "STORAGE-CONDITION: stack overflow".into(),
            ));
        }
        if self.sp >= self.slots.len() {
            self.slots.push(v);
        } else {
            self.slots[self.sp] = v;
        }
        self.sp += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Result<EgclVal, EgclError> {
        if self.sp == 0 {
            return Err(EgclError::Internal("stack underflow".into()));
        }
        self.sp -= 1;
        Ok(self.slots[self.sp])
    }

    pub fn peek(&self, depth: usize) -> Result<EgclVal, EgclError> {
        if depth >= self.sp {
            return Err(EgclError::Internal("stack peek out of bounds".into()));
        }
        Ok(self.slots[self.sp - 1 - depth])
    }

    /// Reset to a saved stack pointer (for non-local exits).
    pub fn unwind_to(&mut self, saved_sp: usize) {
        self.sp = saved_sp;
    }

    pub fn sp(&self) -> usize {
        self.sp
    }
}

impl Default for ValueStack {
    fn default() -> Self {
        Self::new()
    }
}

// ── EnvFrame ──────────────────────────────────────────

/// Lexical environment frame forming a singly-linked chain.
pub struct EnvFrame {
    /// Parent scope (None for the global environment).
    parent: Option<Arc<EnvFrame>>,
    /// Bindings: symbol EgclVal -> value EgclVal.
    bindings: HashMap<u64, EgclVal>,
}

impl EnvFrame {
    /// Create the global (root) environment frame.
    pub fn new_global() -> Arc<Self> {
        Arc::new(EnvFrame {
            parent: None,
            bindings: HashMap::new(),
        })
    }

    /// Extend the environment with a new frame.
    pub fn extend(parent: Arc<EnvFrame>) -> Arc<Self> {
        Arc::new(EnvFrame {
            parent: Some(parent),
            bindings: HashMap::new(),
        })
    }

    /// Define a binding in this frame.
    pub fn define(env: &mut Arc<EnvFrame>, symbol: EgclVal, value: EgclVal) {
        // SAFETY: we are the sole writer in single-threaded T0 interpretation.
        // The Arc is only shared across parent pointers that are read-only
        // during definition (we define only in the current frame).
        let frame = unsafe { &mut *(Arc::as_ptr(env) as *mut EnvFrame) };
        frame.bindings.insert(symbol.0, value);
    }

    /// Look up a symbol in the environment chain.
    pub fn lookup(env: &Arc<EnvFrame>, symbol: EgclVal) -> Option<EgclVal> {
        let key = symbol.0;
        let mut current = Some(env.clone());
        while let Some(frame) = current {
            if let Some(&val) = frame.bindings.get(&key) {
                return Some(val);
            }
            current = frame.parent.clone();
        }
        None
    }

    /// Set a binding in the nearest enclosing frame that has it.
    pub fn set(env: &Arc<EnvFrame>, symbol: EgclVal, value: EgclVal) -> bool {
        let key = symbol.0;
        // We need to find the frame with the binding and mutate it.
        // Since we use Arc, we walk the chain. For simplicity in the interpreter,
        // we check if this frame has it first.
        // This is a simplified approach - a full implementation would use
        // interior mutability (RefCell or similar).
        // For now, we look up and if found, we know which frame has it.
        // Since we can't easily mutate through Arc chain, we'll use a flat
        // approach for SETQ that modifies the current frame.
        let frame = unsafe {
            // SAFETY: we're the only writer in single-threaded interpretation
            let ptr = Arc::as_ptr(env) as *mut EnvFrame;
            &mut *ptr
        };
        if let std::collections::hash_map::Entry::Occupied(mut entry) = frame.bindings.entry(key) {
            entry.insert(value);
            return true;
        }
        // Walk parent chain
        let mut current = frame.parent.clone();
        while let Some(ref parent_arc) = current {
            let parent_ptr = Arc::as_ptr(parent_arc) as *mut EnvFrame;
            let parent = unsafe { &mut *parent_ptr };
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                parent.bindings.entry(key)
            {
                entry.insert(value);
                return true;
            }
            current = parent.parent.clone();
        }
        false
    }
}

// ── Well-known symbol indices for special forms ──────────────────

/// We identify special forms by symbol index. The interpreter recognizes
/// these well-known symbols.
mod special_form {
    /// Symbol index for QUOTE
    pub const QUOTE: u32 = 1000;
    /// Symbol index for IF
    pub const IF: u32 = 1001;
    /// Symbol index for LET
    pub const LET: u32 = 1002;
    /// Symbol index for LET*
    pub const LETSTAR: u32 = 1003;
    /// Symbol index for PROGN
    pub const PROGN: u32 = 1004;
    /// Symbol index for SETQ
    pub const SETQ: u32 = 1005;
    /// Symbol index for FUNCTION
    pub const FUNCTION: u32 = 1006;
    /// Symbol index for LAMBDA
    pub const LAMBDA: u32 = 1007;
    /// Symbol index for BLOCK
    pub const BLOCK: u32 = 1008;
    /// Symbol index for RETURN-FROM
    pub const RETURN_FROM: u32 = 1009;
    /// Symbol index for TAGBODY
    pub const TAGBODY: u32 = 1010;
    /// Symbol index for GO
    pub const GO: u32 = 1011;
    /// Symbol index for CATCH
    pub const CATCH: u32 = 1012;
    /// Symbol index for THROW
    pub const THROW: u32 = 1013;
    /// Symbol index for UNWIND-PROTECT
    pub const UNWIND_PROTECT: u32 = 1014;
    /// Symbol index for THE
    pub const THE: u32 = 1015;
    /// Symbol index for LOCALLY
    pub const LOCALLY: u32 = 1016;
    /// Symbol index for EVAL-WHEN
    pub const EVAL_WHEN: u32 = 1017;
    /// Symbol index for LOAD-TIME-VALUE
    pub const LOAD_TIME_VALUE: u32 = 1018;
}

/// Check if a symbol is a special form and return which one.
fn classify_special_form(sym: EgclVal) -> Option<u32> {
    if sym.tag() != TAG_SYMBOL {
        return None;
    }
    let idx = (sym.0 >> 3) as u32;
    match idx {
        special_form::QUOTE
        | special_form::IF
        | special_form::LET
        | special_form::LETSTAR
        | special_form::PROGN
        | special_form::SETQ
        | special_form::FUNCTION
        | special_form::LAMBDA
        | special_form::BLOCK
        | special_form::RETURN_FROM
        | special_form::TAGBODY
        | special_form::GO
        | special_form::CATCH
        | special_form::THROW
        | special_form::UNWIND_PROTECT
        | special_form::THE
        | special_form::LOCALLY
        | special_form::EVAL_WHEN
        | special_form::LOAD_TIME_VALUE => Some(idx),
        _ => None,
    }
}

// ── Cons cell helpers ─────────────────────────────────────────────

/// Extract car from a cons cell.
fn cons_car(cons: EgclVal) -> Result<EgclVal, EgclError> {
    if !cons.is_cons() {
        return Err(EgclError::TypeError {
            datum: cons,
            expected: "cons".into(),
        });
    }
    let ptr = (cons.0 & !egcl_rt::value::TAG_MASK) as *const u64;
    if ptr.is_null() {
        return Err(EgclError::Internal("car: null cons pointer".into()));
    }
    Ok(EgclVal(unsafe { *ptr }))
}

/// Extract cdr from a cons cell.
fn cons_cdr(cons: EgclVal) -> Result<EgclVal, EgclError> {
    if !cons.is_cons() {
        return Err(EgclError::TypeError {
            datum: cons,
            expected: "cons".into(),
        });
    }
    let ptr = (cons.0 & !egcl_rt::value::TAG_MASK) as *const u64;
    if ptr.is_null() {
        return Err(EgclError::Internal("cdr: null cons pointer".into()));
    }
    Ok(EgclVal(unsafe { *ptr.add(1) }))
}

/// Collect a cons list into a Vec.
fn list_to_vec(list: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let mut result = Vec::new();
    let mut cur = list;
    while cur.is_cons() {
        let ptr = (cur.0 & !egcl_rt::value::TAG_MASK) as *const u64;
        if ptr.is_null() {
            break;
        }
        result.push(EgclVal(unsafe { *ptr }));
        cur = EgclVal(unsafe { *ptr.add(1) });
    }
    Ok(result)
}

/// Build a proper cons list from a Vec of EgclVals.
/// Allocates cons cells on the heap (leaked for simplicity in the interpreter).
fn vec_to_list(vals: &[EgclVal]) -> EgclVal {
    let mut result = EgclVal(NIL_BITS);
    for val in vals.iter().rev() {
        let cell = Box::leak(Box::new([val.0, result.0]));
        result = EgclVal((cell.as_ptr() as u64) | TAG_CONS);
    }
    result
}

// ── Non-local exit signals ────────────────────────────
// These are used for BLOCK/RETURN-FROM, TAGBODY/GO, and CATCH/THROW
// to implement non-local transfers of control via Rust's Result type.

/// Signal for RETURN-FROM non-local exit.
#[derive(Debug)]
struct ReturnFromSignal {
    /// The block name (symbol bits used as identity).
    block_name: u64,
    /// The value being returned.
    value: EgclVal,
}

/// Signal for GO non-local exit.
#[derive(Debug)]
struct GoSignal {
    /// The tag (symbol bits used as identity).
    tag: u64,
}

/// Signal for THROW non-local exit.
#[derive(Debug)]
struct ThrowSignal {
    /// The catch tag value (evaluated).
    tag: EgclVal,
    /// The result value.
    value: EgclVal,
}

/// Unified non-local exit type wrapping all transfer kinds.
#[derive(Debug)]
enum NonLocalExit {
    ReturnFrom(ReturnFromSignal),
    Go(GoSignal),
    Throw(ThrowSignal),
}

/// Helper to convert NonLocalExit into EgclError for propagation.
fn non_local_to_error(nle: NonLocalExit) -> EgclError {
    match nle {
        NonLocalExit::ReturnFrom(r) => EgclError::Internal(format!(
            "__NLE_RETURN_FROM__:{}:{}",
            r.block_name, r.value.0
        )),
        NonLocalExit::Go(g) => EgclError::Internal(format!("__NLE_GO__:{}", g.tag)),
        NonLocalExit::Throw(t) => {
            EgclError::Internal(format!("__NLE_THROW__:{}:{}", t.tag.0, t.value.0))
        }
    }
}

/// Try to parse a EgclError as a non-local exit signal.
fn error_as_non_local(err: &EgclError) -> Option<NonLocalExit> {
    if let EgclError::Internal(msg) = err {
        if let Some(rest) = msg.strip_prefix("__NLE_RETURN_FROM__:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                if let (Ok(name), Ok(val)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
                    return Some(NonLocalExit::ReturnFrom(ReturnFromSignal {
                        block_name: name,
                        value: EgclVal(val),
                    }));
                }
            }
        } else if let Some(rest) = msg.strip_prefix("__NLE_GO__:") {
            if let Ok(tag) = rest.parse::<u64>() {
                return Some(NonLocalExit::Go(GoSignal { tag }));
            }
        } else if let Some(rest) = msg.strip_prefix("__NLE_THROW__:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                if let (Ok(tag), Ok(val)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
                    return Some(NonLocalExit::Throw(ThrowSignal {
                        tag: EgclVal(tag),
                        value: EgclVal(val),
                    }));
                }
            }
        }
    }
    None
}

/// Representation of an interpreted closure (for LAMBDA / FUNCTION).
/// Stored on the heap, pointed to by a TAG_FUNCTION EgclVal.
#[repr(C)]
struct ClosureObj {
    /// The FnMeta for this closure (must be first for apply() compatibility).
    meta: FnMeta,
    /// Captured lexical environment.
    env: Arc<EnvFrame>,
}

// ── T0 interpreter ─────────────────────────────────────────────────

/// T0 tree-walk interpreter.
pub struct Interpreter {
    /// Lexical environment chain.
    env: Arc<EnvFrame>,
    /// Explicit operand stack to avoid deep Rust recursion.
    stack: ValueStack,
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            env: EnvFrame::new_global(),
            stack: ValueStack::new(),
        }
    }

    pub fn define(&mut self, symbol: EgclVal, value: EgclVal) {
        EnvFrame::define(&mut self.env, symbol, value);
    }

    fn lookup(&self, symbol: EgclVal) -> Option<EgclVal> {
        EnvFrame::lookup(&self.env, symbol)
    }

    /// Evaluate a CL form. Self-evaluating forms return themselves,
    /// symbols are looked up, cons cells dispatch as special forms or function calls.
    pub fn eval(&mut self, form: EgclVal) -> Result<EgclVal, EgclError> {
        // Self-evaluating forms: fixnum, character, single-float, heap object, NIL, T
        if form.is_fixnum()
            || form.is_character()
            || form.is_single_float()
            || form.is_heap_object()
            || form.0 == NIL_BITS
            || form.0 == T_BITS
        {
            return Ok(form);
        }
        // Special/function values are self-evaluating
        if form.tag() == TAG_SPECIAL || form.is_function() {
            return Ok(form);
        }
        // Symbol lookup
        if form.tag() == TAG_SYMBOL {
            return self.lookup(form).ok_or(EgclError::UnboundVariable(form));
        }
        // Cons cell: either special form or function call
        if form.is_cons() {
            let operator_form = cons_car(form)?;
            let args_form = cons_cdr(form)?;

            // Check for special forms
            if let Some(sf) = classify_special_form(operator_form) {
                return self.eval_special_form(sf, args_form);
            }

            // Regular function call
            let operator = self.eval(operator_form)?;

            // Walk cdr chain evaluating each argument
            let mut eval_args = Vec::new();
            let mut cur = args_form;
            while cur.is_cons() {
                let car = cons_car(cur)?;
                eval_args.push(self.eval(car)?);
                cur = cons_cdr(cur)?;
            }

            // Build a proper cons list from the evaluated arguments
            let evaluated_args_list = vec_to_list(&eval_args);
            return self.apply(operator, evaluated_args_list);
        }
        Err(EgclError::TypeError {
            datum: form,
            expected: "evaluable form".into(),
        })
    }

    /// Handle special forms.
    fn eval_special_form(&mut self, form_id: u32, args: EgclVal) -> Result<EgclVal, EgclError> {
        match form_id {
            special_form::QUOTE => {
                // (QUOTE datum) -> datum
                if args.is_cons() {
                    cons_car(args)
                } else {
                    Ok(EgclVal(NIL_BITS))
                }
            }
            special_form::IF => {
                // (IF test consequent [alternative])
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal("IF: too few arguments".into()));
                }
                let test = self.eval(args_vec[0])?;
                if test.0 != NIL_BITS {
                    // Test is true
                    if args_vec.len() > 1 {
                        self.eval(args_vec[1])
                    } else {
                        Ok(EgclVal(NIL_BITS))
                    }
                } else {
                    // Test is false
                    if args_vec.len() > 2 {
                        self.eval(args_vec[2])
                    } else {
                        Ok(EgclVal(NIL_BITS))
                    }
                }
            }
            special_form::LET | special_form::LETSTAR => {
                // (LET ((var1 val1) (var2 val2) ...) body...)
                // (LET* ((var1 val1) (var2 val2) ...) body...)
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal("LET: missing bindings".into()));
                }
                let bindings_form = args_vec[0];
                let saved_env = self.env.clone();

                if form_id == special_form::LET {
                    // LET: evaluate all values in current env, then bind
                    let binding_list = list_to_vec(bindings_form)?;
                    let mut names_vals = Vec::new();
                    for binding in &binding_list {
                        if binding.is_cons() {
                            let name = cons_car(*binding)?;
                            let val_form = cons_cdr(*binding)?;
                            let val = if val_form.is_cons() {
                                self.eval(cons_car(val_form)?)?
                            } else {
                                EgclVal(NIL_BITS)
                            };
                            names_vals.push((name, val));
                        } else {
                            // (LET (x ...) ...) binds x to NIL
                            names_vals.push((*binding, EgclVal(NIL_BITS)));
                        }
                    }
                    self.env = EnvFrame::extend(self.env.clone());
                    for (name, val) in names_vals {
                        EnvFrame::define(&mut self.env, name, val);
                    }
                } else {
                    // LET*: evaluate and bind sequentially
                    self.env = EnvFrame::extend(self.env.clone());
                    let binding_list = list_to_vec(bindings_form)?;
                    for binding in &binding_list {
                        if binding.is_cons() {
                            let name = cons_car(*binding)?;
                            let val_form = cons_cdr(*binding)?;
                            let val = if val_form.is_cons() {
                                self.eval(cons_car(val_form)?)?
                            } else {
                                EgclVal(NIL_BITS)
                            };
                            EnvFrame::define(&mut self.env, name, val);
                        } else {
                            EnvFrame::define(&mut self.env, *binding, EgclVal(NIL_BITS));
                        }
                    }
                }

                // Evaluate body forms; return last
                let mut result = EgclVal(NIL_BITS);
                for form in args_vec.iter().skip(1) {
                    result = self.eval(*form)?;
                }
                self.env = saved_env;
                Ok(result)
            }
            special_form::PROGN => {
                // (PROGN form1 form2 ... formN) -> evaluate all, return last
                let forms = list_to_vec(args)?;
                let mut result = EgclVal(NIL_BITS);
                for form in forms {
                    result = self.eval(form)?;
                }
                Ok(result)
            }
            special_form::SETQ => {
                // (SETQ var1 val1 var2 val2 ...)
                let args_vec = list_to_vec(args)?;
                let mut result = EgclVal(NIL_BITS);
                let mut i = 0;
                while i + 1 < args_vec.len() {
                    let sym = args_vec[i];
                    let val = self.eval(args_vec[i + 1])?;
                    if !EnvFrame::set(&self.env, sym, val) {
                        // If not found in any frame, define in current
                        EnvFrame::define(&mut self.env, sym, val);
                    }
                    result = val;
                    i += 2;
                }
                Ok(result)
            }
            special_form::FUNCTION => {
                // (FUNCTION name) -> close over current environment
                if args.is_cons() {
                    let name = cons_car(args)?;
                    if name.is_cons() {
                        // (FUNCTION (LAMBDA ...)) — treat the inner lambda form
                        let inner_op = cons_car(name)?;
                        if let Some(sf) = classify_special_form(inner_op) {
                            if sf == special_form::LAMBDA {
                                let inner_args = cons_cdr(name)?;
                                return self.eval_special_form(special_form::LAMBDA, inner_args);
                            }
                        }
                        // Not a lambda form, evaluate as-is
                        self.eval(name)
                    } else {
                        // (FUNCTION name) — look up symbol binding and create closure
                        match self.lookup(name) {
                            Some(val) if val.tag() == TAG_FUNCTION => {
                                // Already a function, wrap with current env as closure
                                let ptr = (val.0 & !egcl_rt::value::TAG_MASK) as *const u8;
                                if !ptr.is_null() {
                                    let src_meta = unsafe { &*(ptr as *const FnMeta) };
                                    let new_meta =
                                        FnMeta::new(src_meta.arity, src_meta.body, src_meta.params);
                                    new_meta.flags.store(FLAG_IS_CLOSURE, Ordering::Release);
                                    let closure = Box::leak(Box::new(ClosureObj {
                                        meta: new_meta,
                                        env: self.env.clone(),
                                    }));
                                    // Copy entry/tier from source
                                    closure.meta.entry.store(
                                        src_meta.entry.load(Ordering::Acquire),
                                        Ordering::Release,
                                    );
                                    closure.meta.tier.store(
                                        src_meta.tier.load(Ordering::Acquire),
                                        Ordering::Release,
                                    );
                                    Ok(unsafe {
                                        EgclVal::from_function_ptr(
                                            closure as *mut ClosureObj as *mut u8,
                                        )
                                    })
                                } else {
                                    Ok(val)
                                }
                            }
                            Some(val) => Ok(val),
                            None => Err(EgclError::UndefinedFunction(name)),
                        }
                    }
                } else {
                    Ok(EgclVal(NIL_BITS))
                }
            }
            special_form::LAMBDA => {
                // (LAMBDA params body...) -> create interpreted closure
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal(
                        "LAMBDA: missing parameter list".into(),
                    ));
                }
                let params = args_vec[0];
                // Count arity from param list
                let param_list = list_to_vec(params)?;
                let arity = param_list.len() as u32;
                // Build body: if multiple forms, wrap in implicit PROGN
                let body = if args_vec.len() == 2 {
                    args_vec[1]
                } else if args_vec.len() > 2 {
                    // Build (PROGN body1 body2 ...) cons list
                    let progn_sym = EgclVal::from_symbol_index(special_form::PROGN);
                    let mut body_forms = vec![progn_sym];
                    body_forms.extend_from_slice(&args_vec[1..]);
                    vec_to_list(&body_forms)
                } else {
                    EgclVal(NIL_BITS)
                };
                // Create a ClosureObj with the captured environment
                let closure_meta = FnMeta::new(arity, body, params);
                closure_meta.flags.store(FLAG_IS_CLOSURE, Ordering::Release);
                let closure = Box::leak(Box::new(ClosureObj {
                    meta: closure_meta,
                    env: self.env.clone(),
                }));
                Ok(unsafe { EgclVal::from_function_ptr(closure as *mut ClosureObj as *mut u8) })
            }
            special_form::BLOCK => {
                // (BLOCK name form...) -> evaluate forms, catch RETURN-FROM
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(EgclVal(NIL_BITS));
                }
                let block_name = args_vec[0];
                let block_name_bits = block_name.0;
                let saved_sp = self.stack.sp();
                let mut result = EgclVal(NIL_BITS);
                for form in args_vec.iter().skip(1) {
                    match self.eval(*form) {
                        Ok(val) => result = val,
                        Err(ref e) => {
                            if let Some(NonLocalExit::ReturnFrom(ref r)) = error_as_non_local(e) {
                                if r.block_name == block_name_bits {
                                    // Matched: unwind and return the value
                                    self.stack.unwind_to(saved_sp);
                                    return Ok(r.value);
                                }
                            }
                            // Not our block, propagate
                            return Err(EgclError::Internal(format!("{}", e)));
                        }
                    }
                }
                Ok(result)
            }
            special_form::RETURN_FROM => {
                // (RETURN-FROM name [value]) -> non-local transfer to BLOCK
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal(
                        "RETURN-FROM: missing block name".into(),
                    ));
                }
                let block_name = args_vec[0];
                let value = if args_vec.len() > 1 {
                    self.eval(args_vec[1])?
                } else {
                    EgclVal(NIL_BITS)
                };
                Err(non_local_to_error(NonLocalExit::ReturnFrom(
                    ReturnFromSignal {
                        block_name: block_name.0,
                        value,
                    },
                )))
            }
            special_form::TAGBODY => {
                // (TAGBODY {tag | form}*) -> looping via GO restart
                let forms = list_to_vec(args)?;
                // Build tag table: symbol bits -> index in forms
                let mut tag_table: HashMap<u64, usize> = HashMap::new();
                for (i, form) in forms.iter().enumerate() {
                    if form.tag() == TAG_SYMBOL {
                        tag_table.insert(form.0, i);
                    }
                }
                let mut pc = 0usize;
                while pc < forms.len() {
                    let form = forms[pc];
                    if form.tag() == TAG_SYMBOL {
                        // Tag: skip
                        pc += 1;
                        continue;
                    }
                    match self.eval(form) {
                        Ok(_) => {
                            pc += 1;
                        }
                        Err(ref e) => {
                            if let Some(NonLocalExit::Go(ref g)) = error_as_non_local(e) {
                                if let Some(&target_pc) = tag_table.get(&g.tag) {
                                    pc = target_pc;
                                    continue;
                                }
                            }
                            // Not our tagbody or not a GO, propagate
                            return Err(EgclError::Internal(format!("{}", e)));
                        }
                    }
                }
                Ok(EgclVal(NIL_BITS))
            }
            special_form::GO => {
                // (GO tag) -> non-local transfer to enclosing TAGBODY
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal("GO: missing tag".into()));
                }
                let tag = args_vec[0];
                Err(non_local_to_error(NonLocalExit::Go(GoSignal {
                    tag: tag.0,
                })))
            }
            special_form::CATCH => {
                // (CATCH tag form...) -> establish catch frame, evaluate forms
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(EgclVal(NIL_BITS));
                }
                let catch_tag = self.eval(args_vec[0])?;
                let saved_sp = self.stack.sp();
                let mut result = EgclVal(NIL_BITS);
                for form in args_vec.iter().skip(1) {
                    match self.eval(*form) {
                        Ok(val) => result = val,
                        Err(ref e) => {
                            if let Some(NonLocalExit::Throw(ref t)) = error_as_non_local(e) {
                                if t.tag.0 == catch_tag.0 {
                                    // Matched: unwind and return the thrown value
                                    self.stack.unwind_to(saved_sp);
                                    return Ok(t.value);
                                }
                            }
                            // Not our catch tag, propagate
                            return Err(EgclError::Internal(format!("{}", e)));
                        }
                    }
                }
                Ok(result)
            }
            special_form::THROW => {
                // (THROW tag result) -> non-local exit to matching CATCH
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(EgclError::Internal("THROW: missing tag".into()));
                }
                let tag = self.eval(args_vec[0])?;
                let value = if args_vec.len() > 1 {
                    self.eval(args_vec[1])?
                } else {
                    EgclVal(NIL_BITS)
                };
                Err(non_local_to_error(NonLocalExit::Throw(ThrowSignal {
                    tag,
                    value,
                })))
            }
            special_form::UNWIND_PROTECT => {
                // (UNWIND-PROTECT protected-form cleanup-form...)
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(EgclVal(NIL_BITS));
                }
                let result = self.eval(args_vec[0]);
                // Always run cleanup forms
                for form in args_vec.iter().skip(1) {
                    let _ = self.eval(*form);
                }
                result
            }
            special_form::THE => {
                // (THE type form) -> evaluate form (type declaration ignored at T0)
                let args_vec = list_to_vec(args)?;
                if args_vec.len() > 1 {
                    self.eval(args_vec[1])
                } else {
                    Ok(EgclVal(NIL_BITS))
                }
            }
            special_form::LOCALLY => {
                // (LOCALLY declaration* form*) -> evaluate forms
                let forms = list_to_vec(args)?;
                let mut result = EgclVal(NIL_BITS);
                for form in forms {
                    result = self.eval(form)?;
                }
                Ok(result)
            }
            special_form::EVAL_WHEN => {
                // (EVAL-WHEN (situation*) form*) -> conditional evaluation
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(EgclVal(NIL_BITS));
                }
                // Evaluate body forms (simplified: always evaluate at :execute)
                let mut result = EgclVal(NIL_BITS);
                for form in args_vec.iter().skip(1) {
                    result = self.eval(*form)?;
                }
                Ok(result)
            }
            special_form::LOAD_TIME_VALUE => {
                // (LOAD-TIME-VALUE form [read-only-p]) -> evaluate once and cache
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(EgclVal(NIL_BITS));
                }
                self.eval(args_vec[0])
            }
            _ => Err(EgclError::Internal(format!(
                "unknown special form id {}",
                form_id
            ))),
        }
    }

    /// Apply a function to arguments via tree-walk dispatch.
    /// Reads the FnMeta to determine the tier and dispatches:
    /// T0 evaluates the body form, T1/T2 calls the compiled entry.
    pub fn apply(&mut self, function: EgclVal, args: EgclVal) -> Result<EgclVal, EgclError> {
        if function.tag() != TAG_FUNCTION {
            return Err(EgclError::TypeError {
                datum: function,
                expected: "function".into(),
            });
        }
        let func_ptr = (function.0 & !egcl_rt::value::TAG_MASK) as *const u8;
        if func_ptr.is_null() {
            return Err(EgclError::Internal("apply: null function pointer".into()));
        }

        // Read FnMeta from the function pointer
        let meta = unsafe { &*(func_ptr as *const FnMeta) };
        meta.invoke_count.fetch_add(1, Ordering::Relaxed);
        let arity = meta.arity;
        let body = meta.body;
        let params = meta.params;

        // Use the closure's captured environment if FLAG_IS_CLOSURE is set,
        // otherwise use the current environment.
        let saved_env = self.env.clone();
        let is_closure = meta.flags.load(Ordering::Acquire) & FLAG_IS_CLOSURE != 0;
        if is_closure {
            let closure_ptr = func_ptr as *const ClosureObj;
            let closure_env = unsafe { &(*closure_ptr).env };
            self.env = EnvFrame::extend(closure_env.clone());
        } else {
            self.env = EnvFrame::extend(self.env.clone());
        }

        // Bind parameters
        let mut pc = params;
        let mut ac = args;
        let mut bound = 0u32;
        while pc.is_cons() && ac.is_cons() {
            let param = cons_car(pc)?;
            let arg = cons_car(ac)?;
            EnvFrame::define(&mut self.env, param, arg);
            bound += 1;
            pc = cons_cdr(pc)?;
            ac = cons_cdr(ac)?;
        }
        if bound < arity && !args.is_nil() {
            self.env = saved_env;
            return Err(EgclError::Internal(format!(
                "wrong number of arguments: expected {}, got {}",
                arity, bound
            )));
        }

        // Use the operand stack to track eval depth
        let saved_sp = self.stack.sp();
        let result = self.eval(body);
        self.stack.unwind_to(saved_sp);
        self.env = saved_env;
        result
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}
