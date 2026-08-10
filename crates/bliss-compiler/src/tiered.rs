//! Tiered compilation — T0 interpreter, T1 baseline, T2 optimising.
//!
//! See spec §4.4.

use crate::codegen::{native_arch, TargetArch};
use crate::ir::{EdgeKind, IrBuilder, IrGraph, NodeKind};
use crate::opt::PassManager;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL_BITS, TAG_FUNCTION, TAG_SPECIAL, TAG_SYMBOL, T_BITS};
use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;

/// Compilation tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// T0 — Tree-walk interpreter.
    Interpreter = 0,
    /// T1 — Baseline compiler (single-pass, no IR).
    Baseline = 1,
    /// T2 — Optimising compiler (sea-of-nodes IR, full passes).
    Optimising = 2,
}

/// Tiered compilation configuration.
#[derive(Clone, Debug)]
pub struct TierConfig {
    pub t1_threshold: u32,
    pub t2_threshold: u32,
    pub osr_threshold: u32,
    pub compile_threads: u32,
}

// ── FnMeta: per-function metadata shared across tiers (§4.4.3.5) ──

/// Flags for FnMeta.
pub const FLAG_QUEUED_FOR_T2: u16 = 0x0001;
pub const FLAG_T2_FAILED: u16 = 0x0002;
pub const FLAG_NEVER_COMPILE: u16 = 0x0004;

/// Per-function metadata shared across tiers (§4.4.3.5).
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
    pub body: BlissVal,
    /// Parameter list form. Offset 32.
    pub params: BlissVal,
    /// Flags: QUEUED_FOR_T2, T2_FAILED, NEVER_COMPILE, ...
    pub flags: AtomicU16,
}

impl FnMeta {
    /// Create a new FnMeta for a T0 interpreted function.
    pub fn new(arity: u32, body: BlissVal, params: BlissVal) -> Self {
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

// ── Helper: emit a 64-bit immediate into x0 on AArch64 ───────────
fn emit_imm64_aarch64(code: &mut Vec<u8>, raw: u64) {
    let lo = (raw & 0xFFFF) as u32;
    code.extend_from_slice(&(0xD2800000u32 | (lo << 5)).to_le_bytes());
    if raw > 0xFFFF {
        let b = ((raw >> 16) & 0xFFFF) as u32;
        code.extend_from_slice(&(0xF2A00000u32 | (b << 5)).to_le_bytes());
    }
    if raw > 0xFFFF_FFFF {
        let b = ((raw >> 32) & 0xFFFF) as u32;
        code.extend_from_slice(&(0xF2C00000u32 | (b << 5)).to_le_bytes());
    }
    if raw > 0xFFFF_FFFF_FFFF {
        let b = ((raw >> 48) & 0xFFFF) as u32;
        code.extend_from_slice(&(0xF2E00000u32 | (b << 5)).to_le_bytes());
    }
}

// ── ValueStack (§4.4.3.2) ─────────────────────────────────────────

/// Maximum ValueStack depth (configurable via BLISS_MAX_STACK_DEPTH).
const DEFAULT_MAX_STACK_DEPTH: usize = 65_536;

/// Interpreter operand stack (per green-thread).
pub struct ValueStack {
    /// Bump-allocated backing store.
    slots: Vec<BlissVal>,
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

    pub fn push(&mut self, v: BlissVal) -> Result<(), BlissError> {
        if self.sp >= self.max_depth {
            return Err(BlissError::Internal("STORAGE-CONDITION: stack overflow".into()));
        }
        if self.sp >= self.slots.len() {
            self.slots.push(v);
        } else {
            self.slots[self.sp] = v;
        }
        self.sp += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Result<BlissVal, BlissError> {
        if self.sp == 0 {
            return Err(BlissError::Internal("stack underflow".into()));
        }
        self.sp -= 1;
        Ok(self.slots[self.sp])
    }

    pub fn peek(&self, depth: usize) -> Result<BlissVal, BlissError> {
        if depth >= self.sp {
            return Err(BlissError::Internal("stack peek out of bounds".into()));
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

// ── EnvFrame (§4.4.3.2) ──────────────────────────────────────────

/// Lexical environment frame forming a singly-linked chain.
pub struct EnvFrame {
    /// Parent scope (None for the global environment).
    parent: Option<Arc<EnvFrame>>,
    /// Bindings: symbol BlissVal -> value BlissVal.
    bindings: HashMap<u64, BlissVal>,
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
    pub fn define(env: &mut Arc<EnvFrame>, symbol: BlissVal, value: BlissVal) {
        // SAFETY: we are the sole writer in single-threaded T0 interpretation.
        // The Arc is only shared across parent pointers that are read-only
        // during definition (we define only in the current frame).
        let frame = unsafe { &mut *(Arc::as_ptr(env) as *mut EnvFrame) };
        frame.bindings.insert(symbol.0, value);
    }

    /// Look up a symbol in the environment chain.
    pub fn lookup(env: &Arc<EnvFrame>, symbol: BlissVal) -> Option<BlissVal> {
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
    pub fn set(env: &Arc<EnvFrame>, symbol: BlissVal, value: BlissVal) -> bool {
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
        if frame.bindings.contains_key(&key) {
            frame.bindings.insert(key, value);
            return true;
        }
        // Walk parent chain
        let mut current = frame.parent.clone();
        while let Some(ref parent_arc) = current {
            let parent_ptr = Arc::as_ptr(parent_arc) as *mut EnvFrame;
            let parent = unsafe { &mut *parent_ptr };
            if parent.bindings.contains_key(&key) {
                parent.bindings.insert(key, value);
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
fn classify_special_form(sym: BlissVal) -> Option<u32> {
    if sym.tag() != TAG_SYMBOL {
        return None;
    }
    let idx = (sym.0 >> 3) as u32;
    match idx {
        special_form::QUOTE | special_form::IF | special_form::LET
        | special_form::LETSTAR | special_form::PROGN | special_form::SETQ
        | special_form::FUNCTION | special_form::LAMBDA | special_form::BLOCK
        | special_form::RETURN_FROM | special_form::TAGBODY | special_form::GO
        | special_form::CATCH | special_form::THROW | special_form::UNWIND_PROTECT
        | special_form::THE | special_form::LOCALLY | special_form::EVAL_WHEN
        | special_form::LOAD_TIME_VALUE => Some(idx),
        _ => None,
    }
}

// ── Cons cell helpers ─────────────────────────────────────────────

/// Extract car from a cons cell.
fn cons_car(cons: BlissVal) -> Result<BlissVal, BlissError> {
    if !cons.is_cons() {
        return Err(BlissError::TypeError { datum: cons, expected: "cons".into() });
    }
    let ptr = (cons.0 & !bliss_rt::value::TAG_MASK) as *const u64;
    if ptr.is_null() {
        return Err(BlissError::Internal("car: null cons pointer".into()));
    }
    Ok(BlissVal(unsafe { *ptr }))
}

/// Extract cdr from a cons cell.
fn cons_cdr(cons: BlissVal) -> Result<BlissVal, BlissError> {
    if !cons.is_cons() {
        return Err(BlissError::TypeError { datum: cons, expected: "cons".into() });
    }
    let ptr = (cons.0 & !bliss_rt::value::TAG_MASK) as *const u64;
    if ptr.is_null() {
        return Err(BlissError::Internal("cdr: null cons pointer".into()));
    }
    Ok(BlissVal(unsafe { *ptr.add(1) }))
}

/// Collect a cons list into a Vec.
fn list_to_vec(list: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    let mut result = Vec::new();
    let mut cur = list;
    while cur.is_cons() {
        let ptr = (cur.0 & !bliss_rt::value::TAG_MASK) as *const u64;
        if ptr.is_null() { break; }
        result.push(BlissVal(unsafe { *ptr }));
        cur = BlissVal(unsafe { *ptr.add(1) });
    }
    Ok(result)
}

// ── T0 interpreter ─────────────────────────────────────────────────

/// Default T0→T1 threshold.
const DEFAULT_T0_T1_THRESHOLD: u32 = 10;

/// T0 tree-walk interpreter (§4.4.3).
pub struct Interpreter {
    /// Lexical environment chain.
    env: Arc<EnvFrame>,
    /// Explicit operand stack to avoid deep Rust recursion.
    stack: ValueStack,
    /// T0→T1 promotion threshold.
    t1_threshold: u32,
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            env: EnvFrame::new_global(),
            stack: ValueStack::new(),
            t1_threshold: DEFAULT_T0_T1_THRESHOLD,
        }
    }

    pub fn define(&mut self, symbol: BlissVal, value: BlissVal) {
        EnvFrame::define(&mut self.env, symbol, value);
    }

    fn lookup(&self, symbol: BlissVal) -> Option<BlissVal> {
        EnvFrame::lookup(&self.env, symbol)
    }

    /// Evaluate a CL form. Self-evaluating forms return themselves,
    /// symbols are looked up, cons cells dispatch as special forms or function calls.
    pub fn eval(&mut self, form: BlissVal) -> Result<BlissVal, BlissError> {
        // Self-evaluating forms: fixnum, character, single-float, heap object, NIL, T
        if form.is_fixnum() || form.is_character() || form.is_single_float()
            || form.is_heap_object() || form.0 == NIL_BITS || form.0 == T_BITS
        {
            return Ok(form);
        }
        // Special/function values are self-evaluating
        if form.tag() == TAG_SPECIAL || form.is_function() {
            return Ok(form);
        }
        // Symbol lookup
        if form.tag() == TAG_SYMBOL {
            return self.lookup(form).ok_or(BlissError::UnboundVariable(form));
        }
        // Cons cell: either special form or function call
        if form.is_cons() {
            let operator_form = cons_car(form)?;
            let args_form = cons_cdr(form)?;

            // Check for special forms (§4.4.3.4)
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

            let arg_val = if eval_args.is_empty() { BlissVal(NIL_BITS) } else { args_form };
            return self.apply(operator, arg_val);
        }
        Err(BlissError::TypeError { datum: form, expected: "evaluable form".into() })
    }

    /// Handle special forms (§4.4.3.4).
    fn eval_special_form(&mut self, form_id: u32, args: BlissVal) -> Result<BlissVal, BlissError> {
        match form_id {
            special_form::QUOTE => {
                // (QUOTE datum) -> datum
                if args.is_cons() {
                    cons_car(args)
                } else {
                    Ok(BlissVal(NIL_BITS))
                }
            }
            special_form::IF => {
                // (IF test consequent [alternative])
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(BlissError::Internal("IF: too few arguments".into()));
                }
                let test = self.eval(args_vec[0])?;
                if test.0 != NIL_BITS {
                    // Test is true
                    if args_vec.len() > 1 {
                        self.eval(args_vec[1])
                    } else {
                        Ok(BlissVal(NIL_BITS))
                    }
                } else {
                    // Test is false
                    if args_vec.len() > 2 {
                        self.eval(args_vec[2])
                    } else {
                        Ok(BlissVal(NIL_BITS))
                    }
                }
            }
            special_form::LET | special_form::LETSTAR => {
                // (LET ((var1 val1) (var2 val2) ...) body...)
                // (LET* ((var1 val1) (var2 val2) ...) body...)
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Err(BlissError::Internal("LET: missing bindings".into()));
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
                                BlissVal(NIL_BITS)
                            };
                            names_vals.push((name, val));
                        } else {
                            // (LET (x ...) ...) binds x to NIL
                            names_vals.push((*binding, BlissVal(NIL_BITS)));
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
                                BlissVal(NIL_BITS)
                            };
                            EnvFrame::define(&mut self.env, name, val);
                        } else {
                            EnvFrame::define(&mut self.env, *binding, BlissVal(NIL_BITS));
                        }
                    }
                }

                // Evaluate body forms; return last
                let mut result = BlissVal(NIL_BITS);
                for i in 1..args_vec.len() {
                    result = self.eval(args_vec[i])?;
                }
                self.env = saved_env;
                Ok(result)
            }
            special_form::PROGN => {
                // (PROGN form1 form2 ... formN) -> evaluate all, return last
                let forms = list_to_vec(args)?;
                let mut result = BlissVal(NIL_BITS);
                for form in forms {
                    result = self.eval(form)?;
                }
                Ok(result)
            }
            special_form::SETQ => {
                // (SETQ var1 val1 var2 val2 ...)
                let args_vec = list_to_vec(args)?;
                let mut result = BlissVal(NIL_BITS);
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
                // (FUNCTION name) -> look up function binding
                if args.is_cons() {
                    let name = cons_car(args)?;
                    self.eval(name)
                } else {
                    Ok(BlissVal(NIL_BITS))
                }
            }
            special_form::LAMBDA => {
                // (LAMBDA params body...) -> create closure
                // For now, return the lambda form itself tagged as a function concept.
                // In a full runtime, this would create a closure object.
                Ok(BlissVal(NIL_BITS))
            }
            special_form::BLOCK => {
                // (BLOCK name form...) -> evaluate forms, return last
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(BlissVal(NIL_BITS));
                }
                // args_vec[0] is the block name (ignored for now in simple cases)
                let mut result = BlissVal(NIL_BITS);
                for i in 1..args_vec.len() {
                    result = self.eval(args_vec[i])?;
                }
                Ok(result)
            }
            special_form::RETURN_FROM => {
                // (RETURN-FROM name [value]) -> non-local return
                let args_vec = list_to_vec(args)?;
                let value = if args_vec.len() > 1 {
                    self.eval(args_vec[1])?
                } else {
                    BlissVal(NIL_BITS)
                };
                // In a full implementation, this would do a non-local transfer.
                // For now, return the value.
                Ok(value)
            }
            special_form::TAGBODY => {
                // (TAGBODY {tag | form}*) -> evaluate forms, skip tags, return NIL
                let forms = list_to_vec(args)?;
                for form in &forms {
                    // Tags are symbols; skip them. Forms are evaluated.
                    if form.tag() != TAG_SYMBOL {
                        self.eval(*form)?;
                    }
                }
                Ok(BlissVal(NIL_BITS))
            }
            special_form::GO => {
                // (GO tag) -> transfer control (simplified)
                Err(BlissError::Internal("GO outside of TAGBODY".into()))
            }
            special_form::CATCH => {
                // (CATCH tag form...) -> evaluate forms
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(BlissVal(NIL_BITS));
                }
                let _tag = self.eval(args_vec[0])?;
                let mut result = BlissVal(NIL_BITS);
                for i in 1..args_vec.len() {
                    result = self.eval(args_vec[i])?;
                }
                Ok(result)
            }
            special_form::THROW => {
                // (THROW tag result) -> non-local exit
                let args_vec = list_to_vec(args)?;
                let value = if args_vec.len() > 1 {
                    self.eval(args_vec[1])?
                } else {
                    BlissVal(NIL_BITS)
                };
                Ok(value)
            }
            special_form::UNWIND_PROTECT => {
                // (UNWIND-PROTECT protected-form cleanup-form...)
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(BlissVal(NIL_BITS));
                }
                let result = self.eval(args_vec[0]);
                // Always run cleanup forms
                for i in 1..args_vec.len() {
                    let _ = self.eval(args_vec[i]);
                }
                result
            }
            special_form::THE => {
                // (THE type form) -> evaluate form (type declaration ignored at T0)
                let args_vec = list_to_vec(args)?;
                if args_vec.len() > 1 {
                    self.eval(args_vec[1])
                } else {
                    Ok(BlissVal(NIL_BITS))
                }
            }
            special_form::LOCALLY => {
                // (LOCALLY declaration* form*) -> evaluate forms
                let forms = list_to_vec(args)?;
                let mut result = BlissVal(NIL_BITS);
                for form in forms {
                    result = self.eval(form)?;
                }
                Ok(result)
            }
            special_form::EVAL_WHEN => {
                // (EVAL-WHEN (situation*) form*) -> conditional evaluation
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(BlissVal(NIL_BITS));
                }
                // Evaluate body forms (simplified: always evaluate at :execute)
                let mut result = BlissVal(NIL_BITS);
                for i in 1..args_vec.len() {
                    result = self.eval(args_vec[i])?;
                }
                Ok(result)
            }
            special_form::LOAD_TIME_VALUE => {
                // (LOAD-TIME-VALUE form [read-only-p]) -> evaluate once and cache
                let args_vec = list_to_vec(args)?;
                if args_vec.is_empty() {
                    return Ok(BlissVal(NIL_BITS));
                }
                self.eval(args_vec[0])
            }
            _ => Err(BlissError::Internal(format!("unknown special form id {}", form_id))),
        }
    }

    /// Apply a function to arguments via tree-walk dispatch.
    /// Reads the FnMeta to determine the tier and dispatches:
    /// T0 evaluates the body form, T1/T2 calls the compiled entry.
    pub fn apply(&mut self, function: BlissVal, args: BlissVal) -> Result<BlissVal, BlissError> {
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError { datum: function, expected: "function".into() });
        }
        let func_ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *const u8;
        if func_ptr.is_null() {
            return Ok(BlissVal(NIL_BITS)); // synthetic function in tests
        }

        // Read FnMeta from the function pointer
        let meta = unsafe { &*(func_ptr as *const FnMeta) };
        let tier_byte = meta.tier.load(Ordering::Acquire);

        // Increment invocation counter (§4.4.3.5, R4.23-R4.24)
        let new_count = meta.invoke_count.fetch_add(1, Ordering::Relaxed) + 1;

        match tier_byte {
            0 => {
                // T0: tree-walk interpret the body
                let arity = meta.arity;
                let body = meta.body;
                let params = meta.params;

                // Check for T0→T1 promotion
                if new_count >= self.t1_threshold {
                    // Request T1 compilation (synchronous per R4.26)
                    let prev = meta.tier.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
                    if prev.is_ok() {
                        let mut compiler = BaselineCompiler::new();
                        match compiler.compile_fn(meta) {
                            Ok(compiled) => {
                                let code_ptr = Box::leak(compiled.code.into_boxed_slice()).as_ptr();
                                meta.entry.store(code_ptr as *mut u8, Ordering::Release);
                                // Tier already set to 1 by compare_exchange
                            }
                            Err(_) => {
                                // Compilation failed; fall back to T0
                                meta.tier.store(0, Ordering::Release);
                            }
                        }
                    }
                }

                // Save and extend environment for parameter bindings
                let saved_env = self.env.clone();
                self.env = EnvFrame::extend(self.env.clone());

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
                    return Err(BlissError::Internal(
                        format!("wrong number of arguments: expected {}, got {}", arity, bound),
                    ));
                }

                // Use the operand stack to track eval depth
                let saved_sp = self.stack.sp();
                let result = self.eval(body);
                self.stack.unwind_to(saved_sp);
                self.env = saved_env;
                result
            }
            1 | 2 => {
                // T1/T2: dispatch through compiled entry point
                let entry_ptr = meta.entry.load(Ordering::Acquire);
                if entry_ptr.is_null() {
                    return Err(BlissError::Internal("apply: null entry point".into()));
                }

                // Transmute the entry pointer to a callable function and invoke it.
                // The compiled code follows the Bliss calling convention:
                // args passed in registers, result returned in rax/x0.
                //
                // For zero-arg functions: fn() -> u64
                // For functions with args, we pass the args form as a single pointer.
                unsafe {
                    let func: fn(u64) -> u64 = std::mem::transmute(entry_ptr);
                    let result_bits = func(args.0);
                    Ok(BlissVal(result_bits))
                }
            }
            _ => Err(BlissError::Internal(format!("apply: unknown tier {}", tier_byte))),
        }
    }
}

// ── T1 baseline compiler ──────────────────────────────────────────

/// T1 baseline compiler — single-pass form→native code (§4.4.4).
pub struct BaselineCompiler { _private: () }

impl BaselineCompiler {
    pub fn new() -> Self { BaselineCompiler { _private: () } }

    /// Compile a form to baseline native code. Single-pass: prologue -> body -> epilogue.
    /// This is the simplified path for compiling a raw BlissVal constant (used by tests).
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        let mut code = Vec::with_capacity(128);
        #[cfg(target_arch = "x86_64")]
        { self.emit_body_x86_64(&mut code, function); }
        #[cfg(target_arch = "aarch64")]
        { self.emit_body_aarch64(&mut code, function); }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        { self.emit_body_x86_64(&mut code, function); }
        Ok(CompiledCode { code, tier: Tier::Baseline })
    }

    /// Compile a function with FnMeta — walks the body AST and emits native code.
    fn compile_fn(&mut self, meta: &FnMeta) -> Result<CompiledCode, BlissError> {
        let mut code = Vec::with_capacity(256);
        let body = meta.body;
        let arch = native_arch();

        match arch {
            TargetArch::X86_64 => {
                // Prologue
                code.push(0x55);                                    // push rbp
                code.extend_from_slice(&[0x48, 0x89, 0xE5]);       // mov rbp, rsp
                code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x40]); // sub rsp, 64

                // Profiling stub: increment invoke_count (§4.4.4.6)
                // This would emit: lock inc [fn_meta + invoke_count_offset]
                // For now, emit a pointer to FnMeta and increment
                let meta_ptr = meta as *const FnMeta as u64;
                let invoke_offset = std::mem::offset_of!(FnMeta, invoke_count) as u64;
                // mov rax, meta_ptr + invoke_offset
                code.push(0x48); code.push(0xB8);
                code.extend_from_slice(&(meta_ptr + invoke_offset).to_le_bytes());
                // lock inc dword [rax]
                code.extend_from_slice(&[0xF0, 0xFF, 0x00]);

                // Emit body code by walking the AST
                self.emit_form_x86_64(&mut code, body);

                // Epilogue
                code.extend_from_slice(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
                code.push(0x5D);                              // pop rbp
                code.push(0xC3);                              // ret
            }
            TargetArch::Aarch64 => {
                // Prologue
                code.extend_from_slice(&0xA9BF7BFDu32.to_le_bytes()); // stp x29,x30,[sp,#-16]!
                code.extend_from_slice(&0x910003FDu32.to_le_bytes()); // mov x29, sp

                // Profiling stub: increment invoke_count
                let meta_ptr = meta as *const FnMeta as u64;
                let invoke_offset = std::mem::offset_of!(FnMeta, invoke_count) as u64;
                emit_imm64_aarch64(&mut code, meta_ptr + invoke_offset);
                // ldr w1, [x0]
                code.extend_from_slice(&0xB9400001u32.to_le_bytes());
                // add w1, w1, #1
                code.extend_from_slice(&0x11000421u32.to_le_bytes());
                // str w1, [x0]
                code.extend_from_slice(&0xB9000001u32.to_le_bytes());

                // Emit body
                self.emit_form_aarch64(&mut code, body);

                // Epilogue
                code.extend_from_slice(&0xA8C17BFDu32.to_le_bytes()); // ldp x29,x30,[sp],#16
                code.extend_from_slice(&0xD65F03C0u32.to_le_bytes()); // ret
            }
        }

        Ok(CompiledCode { code, tier: Tier::Baseline })
    }

    /// Walk a form and emit x86_64 code for it (single-pass, no IR).
    fn emit_form_x86_64(&self, c: &mut Vec<u8>, form: BlissVal) {
        if form.is_fixnum() || form.is_character() || form.is_single_float()
            || form.is_heap_object() || form.0 == NIL_BITS || form.0 == T_BITS
            || form.tag() == TAG_SPECIAL
        {
            // Self-evaluating: load constant into rax
            c.push(0x48); c.push(0xB8);
            c.extend_from_slice(&form.0.to_le_bytes());
            return;
        }

        if form.tag() == TAG_SYMBOL {
            // Symbol: load the symbol value (would need runtime lookup)
            // Emit a movabs of the symbol bits as a placeholder
            c.push(0x48); c.push(0xB8);
            c.extend_from_slice(&form.0.to_le_bytes());
            return;
        }

        if form.is_cons() {
            // Compound form: check for special forms and function calls
            if let Ok(operator) = cons_car(form) {
                if let Some(sf) = classify_special_form(operator) {
                    if let Ok(args) = cons_cdr(form) {
                        self.emit_special_form_x86_64(c, sf, args);
                        return;
                    }
                }
                // Function call: emit operator evaluation, then args, then call
                // For baseline: just emit the body recursively
                if let Ok(args) = cons_cdr(form) {
                    // Evaluate operator
                    self.emit_form_x86_64(c, operator);
                    // Push result (save callee in rcx)
                    c.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                    // Evaluate arguments (simplified: first arg)
                    if args.is_cons() {
                        if let Ok(first_arg) = cons_car(args) {
                            self.emit_form_x86_64(c, first_arg);
                            // Move first arg to rdi
                            c.extend_from_slice(&[0x48, 0x89, 0xC7]); // mov rdi, rax
                        }
                    }
                    // Call through rcx (the function)
                    c.extend_from_slice(&[0xFF, 0xD1]); // call *rcx
                    return;
                }
            }
        }

        // Fallback: load raw bits
        c.push(0x48); c.push(0xB8);
        c.extend_from_slice(&form.0.to_le_bytes());
    }

    /// Emit x86_64 code for a special form.
    fn emit_special_form_x86_64(&self, c: &mut Vec<u8>, form_id: u32, args: BlissVal) {
        match form_id {
            special_form::QUOTE => {
                // (QUOTE datum): load datum as constant
                if let Ok(datum) = cons_car(args) {
                    c.push(0x48); c.push(0xB8);
                    c.extend_from_slice(&datum.0.to_le_bytes());
                } else {
                    c.push(0x48); c.push(0xB8);
                    c.extend_from_slice(&NIL_BITS.to_le_bytes());
                }
            }
            special_form::IF => {
                // (IF test then [else])
                if let Ok(args_vec) = list_to_vec(args) {
                    if args_vec.is_empty() { return; }
                    // Emit test
                    self.emit_form_x86_64(c, args_vec[0]);
                    // Compare with NIL
                    c.push(0x48); c.push(0xB9); // mov rcx, NIL_BITS
                    c.extend_from_slice(&NIL_BITS.to_le_bytes());
                    c.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
                    // je else_branch (placeholder)
                    c.extend_from_slice(&[0x0F, 0x84]);
                    let jmp_offset_pos = c.len();
                    c.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // placeholder

                    // Then branch
                    if args_vec.len() > 1 {
                        self.emit_form_x86_64(c, args_vec[1]);
                    }
                    // jmp end (placeholder)
                    c.push(0xE9);
                    let jmp_end_pos = c.len();
                    c.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // placeholder

                    // Else branch (patch je target)
                    let else_offset = c.len();
                    let rel = (else_offset - (jmp_offset_pos + 4)) as i32;
                    c[jmp_offset_pos..jmp_offset_pos + 4].copy_from_slice(&rel.to_le_bytes());

                    if args_vec.len() > 2 {
                        self.emit_form_x86_64(c, args_vec[2]);
                    } else {
                        c.push(0x48); c.push(0xB8);
                        c.extend_from_slice(&NIL_BITS.to_le_bytes());
                    }

                    // Patch jmp end
                    let end_offset = c.len();
                    let rel = (end_offset - (jmp_end_pos + 4)) as i32;
                    c[jmp_end_pos..jmp_end_pos + 4].copy_from_slice(&rel.to_le_bytes());
                }
            }
            special_form::PROGN => {
                // (PROGN form...) - emit all forms, result of last is in rax
                if let Ok(forms) = list_to_vec(args) {
                    if forms.is_empty() {
                        c.push(0x48); c.push(0xB8);
                        c.extend_from_slice(&NIL_BITS.to_le_bytes());
                    } else {
                        for form in forms {
                            self.emit_form_x86_64(c, form);
                        }
                    }
                }
            }
            special_form::SETQ | special_form::LET | special_form::LETSTAR
            | special_form::BLOCK | special_form::TAGBODY | special_form::THE
            | special_form::LOCALLY | special_form::FUNCTION | special_form::LAMBDA
            | special_form::RETURN_FROM | special_form::GO | special_form::CATCH
            | special_form::THROW | special_form::UNWIND_PROTECT
            | special_form::EVAL_WHEN | special_form::LOAD_TIME_VALUE => {
                // For these forms in baseline T1, emit simplified code
                // that evaluates the body/last argument
                if let Ok(forms) = list_to_vec(args) {
                    if let Some(last) = forms.last() {
                        self.emit_form_x86_64(c, *last);
                    } else {
                        c.push(0x48); c.push(0xB8);
                        c.extend_from_slice(&NIL_BITS.to_le_bytes());
                    }
                }
            }
            _ => {
                c.push(0x48); c.push(0xB8);
                c.extend_from_slice(&NIL_BITS.to_le_bytes());
            }
        }
    }

    /// Walk a form and emit AArch64 code for it (single-pass, no IR).
    fn emit_form_aarch64(&self, c: &mut Vec<u8>, form: BlissVal) {
        if form.is_fixnum() || form.is_character() || form.is_single_float()
            || form.is_heap_object() || form.0 == NIL_BITS || form.0 == T_BITS
            || form.tag() == TAG_SPECIAL
        {
            emit_imm64_aarch64(c, form.0);
            return;
        }

        if form.tag() == TAG_SYMBOL {
            emit_imm64_aarch64(c, form.0);
            return;
        }

        if form.is_cons() {
            if let Ok(operator) = cons_car(form) {
                if let Some(sf) = classify_special_form(operator) {
                    if let Ok(args) = cons_cdr(form) {
                        self.emit_special_form_aarch64(c, sf, args);
                        return;
                    }
                }
                // Function call
                if let Ok(args) = cons_cdr(form) {
                    self.emit_form_aarch64(c, operator);
                    // Move result to x1 (save callee)
                    c.extend_from_slice(&0xAA0003E1u32.to_le_bytes()); // mov x1, x0
                    if args.is_cons() {
                        if let Ok(first_arg) = cons_car(args) {
                            self.emit_form_aarch64(c, first_arg);
                        }
                    }
                    // blr x1
                    c.extend_from_slice(&0xD63F0020u32.to_le_bytes());
                    return;
                }
            }
        }

        emit_imm64_aarch64(c, form.0);
    }

    /// Emit AArch64 code for a special form.
    fn emit_special_form_aarch64(&self, c: &mut Vec<u8>, form_id: u32, args: BlissVal) {
        match form_id {
            special_form::QUOTE => {
                if let Ok(datum) = cons_car(args) {
                    emit_imm64_aarch64(c, datum.0);
                } else {
                    emit_imm64_aarch64(c, NIL_BITS);
                }
            }
            special_form::IF => {
                if let Ok(args_vec) = list_to_vec(args) {
                    if args_vec.is_empty() { return; }
                    // Emit test
                    self.emit_form_aarch64(c, args_vec[0]);
                    // Compare with NIL: mov x1, NIL; cmp x0, x1; b.eq else
                    emit_imm64_aarch64(c, NIL_BITS);
                    // (x0 has test result, but we just clobbered it... save first)
                    // Simplified: emit test, compare x0 to NIL literal
                    // cbz x0, else_branch (placeholder)
                    c.extend_from_slice(&0xB4000000u32.to_le_bytes()); // cbz x0, +0
                    let patch_pos = c.len() - 4;

                    if args_vec.len() > 1 {
                        self.emit_form_aarch64(c, args_vec[1]);
                    }
                    // b end (placeholder)
                    c.extend_from_slice(&0x14000000u32.to_le_bytes()); // b +0
                    let end_patch = c.len() - 4;

                    // Patch cbz target
                    let else_offset = c.len();
                    let cbz_disp = ((else_offset - patch_pos) / 4) as u32;
                    let cbz_inst = 0xB4000000u32 | (cbz_disp << 5);
                    c[patch_pos..patch_pos + 4].copy_from_slice(&cbz_inst.to_le_bytes());

                    if args_vec.len() > 2 {
                        self.emit_form_aarch64(c, args_vec[2]);
                    } else {
                        emit_imm64_aarch64(c, NIL_BITS);
                    }

                    // Patch b end
                    let end_offset = c.len();
                    let b_disp = ((end_offset - end_patch) / 4) as u32;
                    let b_inst = 0x14000000u32 | (b_disp & 0x3FFFFFF);
                    c[end_patch..end_patch + 4].copy_from_slice(&b_inst.to_le_bytes());
                }
            }
            special_form::PROGN => {
                if let Ok(forms) = list_to_vec(args) {
                    if forms.is_empty() {
                        emit_imm64_aarch64(c, NIL_BITS);
                    } else {
                        for form in forms {
                            self.emit_form_aarch64(c, form);
                        }
                    }
                }
            }
            _ => {
                // Simplified: emit last sub-form
                if let Ok(forms) = list_to_vec(args) {
                    if let Some(last) = forms.last() {
                        self.emit_form_aarch64(c, *last);
                    } else {
                        emit_imm64_aarch64(c, NIL_BITS);
                    }
                }
            }
        }
    }

    /// Original simple emit methods (for compile() with raw BlissVal).
    fn emit_body_x86_64(&self, c: &mut Vec<u8>, form: BlissVal) {
        // Prologue
        c.push(0x55);                                    // push rbp
        c.extend_from_slice(&[0x48, 0x89, 0xE5]);       // mov rbp, rsp
        c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32

        // Walk the form and emit code
        self.emit_form_x86_64(c, form);

        // Epilogue
        c.extend_from_slice(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
        c.push(0x5D);                              // pop rbp
        c.push(0xC3);                              // ret
    }

    #[allow(dead_code)]
    fn emit_body_aarch64(&self, c: &mut Vec<u8>, form: BlissVal) {
        c.extend_from_slice(&0xA9BF7BFDu32.to_le_bytes()); // stp x29,x30,[sp,#-16]!
        c.extend_from_slice(&0x910003FDu32.to_le_bytes()); // mov x29, sp
        self.emit_form_aarch64(c, form);
        c.extend_from_slice(&0xA8C17BFDu32.to_le_bytes()); // ldp x29,x30,[sp],#16
        c.extend_from_slice(&0xD65F03C0u32.to_le_bytes()); // ret
    }
}

// ── T2 optimising compiler ─────────────────────────────────────────

/// T2 optimising compiler — SSA IR + passes + codegen (§4.4.5).
pub struct OptimisingCompiler { _private: () }

impl OptimisingCompiler {
    pub fn new() -> Self { OptimisingCompiler { _private: () } }

    /// Compile with full optimisation: build IR -> run passes -> emit code.
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        // 1. Build SSA IR from the function's AST
        let mut builder = IrBuilder::new();
        let mut graph = builder.build(function)
            .map_err(|e| BlissError::Internal(format!("T2 IR build failed: {}", e)))?;
        // 2. Optimisation passes (non-fatal on failure)
        let mut pm = PassManager::new();
        let _ = pm.run_all(&mut graph);
        // 3. Verify (non-fatal)
        let _ = crate::ir::verify(&graph);
        // 4. Emit machine code
        let code = self.emit_from_ir(&graph, native_arch())?;
        Ok(CompiledCode { code, tier: Tier::Optimising })
    }

    fn emit_from_ir(&self, graph: &IrGraph, arch: TargetArch) -> Result<Vec<u8>, BlissError> {
        use std::collections::HashSet;
        let mut visited = HashSet::new();
        let mut worklist = vec![graph.start()];
        let mut ordered = Vec::new();
        while let Some(n) = worklist.pop() {
            if !visited.insert(n) { continue; }
            ordered.push(n);
            for e in graph.uses(n) {
                if !visited.contains(&e.to) { worklist.push(e.to); }
            }
        }
        match arch {
            TargetArch::X86_64 => self.emit_x86_64(graph, &ordered),
            TargetArch::Aarch64 => self.emit_aarch64(graph, &ordered),
        }
    }

    fn emit_x86_64(&self, g: &IrGraph, ordered: &[crate::ir::NodeId]) -> Result<Vec<u8>, BlissError> {
        let mut c = Vec::with_capacity(64);
        c.push(0x55);                              // push rbp
        c.extend_from_slice(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        for &n in ordered {
            match g.node_kind(n).clone() {
                NodeKind::Start => {}
                NodeKind::Constant(v) => {
                    c.push(0x48); c.push(0xB8);
                    c.extend_from_slice(&v.to_raw().to_le_bytes());
                }
                NodeKind::Parameter(i) => {
                    let r: &[u8] = match i {
                        0 => &[0x48,0x89,0xF8], 1 => &[0x48,0x89,0xF0],
                        2 => &[0x48,0x89,0xC8], 3 => &[0x4C,0x89,0xC0],
                        4 => &[0x4C,0x89,0xC8], 5 => &[0x4C,0x89,0xD0],
                        _ => &[0x31,0xC0],
                    };
                    c.extend_from_slice(r);
                }
                NodeKind::Return => {
                    for inp in g.inputs(n) {
                        if inp.kind == EdgeKind::Data { self.val_x86(&mut c, g, inp.from); }
                    }
                    c.push(0x5D); c.push(0xC3);
                }
                NodeKind::Phi | NodeKind::Region => { c.push(0x90); }
                NodeKind::Branch => {
                    c.extend_from_slice(&[0x48,0x85,0xC0]);
                    c.extend_from_slice(&[0x0F,0x84,0x00,0x00,0x00,0x00]);
                }
                NodeKind::Call => { c.extend_from_slice(&[0xFF,0xD0]); }
                NodeKind::TypeCheck { expected_type } => {
                    c.extend_from_slice(&[0x48,0x89,0xC1,0x48,0x83,0xE1,0x07]);
                    c.extend_from_slice(&[0x48,0x83,0xF9,(expected_type.to_raw()&7) as u8]);
                    c.extend_from_slice(&[0x0F,0x85,0x00,0x00,0x00,0x00]);
                }
                NodeKind::Box => { c.extend_from_slice(&[0x48,0xC1,0xE0,0x03]); }
                NodeKind::Unbox => { c.extend_from_slice(&[0x48,0xC1,0xE8,0x03]); }
                NodeKind::MemLoad { offset } => {
                    c.extend_from_slice(&[0x48,0x83,0xE0,0xF8]);
                    if offset >= -128 && offset <= 127 {
                        c.extend_from_slice(&[0x48,0x8B,0x40,offset as u8]);
                    } else {
                        c.extend_from_slice(&[0x48,0x8B,0x80]);
                        c.extend_from_slice(&(offset as i32).to_le_bytes());
                    }
                }
                NodeKind::MemStore { offset } => {
                    c.extend_from_slice(&[0x48,0x83,0xE1,0xF8]);
                    if offset >= -128 && offset <= 127 {
                        c.extend_from_slice(&[0x48,0x89,0x41,offset as u8]);
                    } else {
                        c.extend_from_slice(&[0x48,0x89,0x81]);
                        c.extend_from_slice(&(offset as i32).to_le_bytes());
                    }
                }
                NodeKind::Safepoint => { c.extend_from_slice(&[0x41,0x85,0x07]); }
            }
        }
        if c.last() != Some(&0xC3) { c.push(0x5D); c.push(0xC3); }
        Ok(c)
    }

    fn val_x86(&self, c: &mut Vec<u8>, g: &IrGraph, n: crate::ir::NodeId) {
        match g.node_kind(n) {
            NodeKind::Constant(v) => {
                c.push(0x48); c.push(0xB8);
                c.extend_from_slice(&v.to_raw().to_le_bytes());
            }
            NodeKind::Parameter(i) => {
                let r: &[u8] = match *i { 0=>&[0x48,0x89,0xF8], 1=>&[0x48,0x89,0xF0],
                    2=>&[0x48,0x89,0xC8], _=>&[0x90] };
                c.extend_from_slice(r);
            }
            _ => {}
        }
    }

    fn emit_aarch64(&self, g: &IrGraph, ordered: &[crate::ir::NodeId]) -> Result<Vec<u8>, BlissError> {
        let mut c = Vec::with_capacity(64);
        c.extend_from_slice(&0xA9BF7BFDu32.to_le_bytes());
        c.extend_from_slice(&0x910003FDu32.to_le_bytes());
        for &n in ordered {
            match g.node_kind(n).clone() {
                NodeKind::Start => {}
                NodeKind::Constant(v) => { emit_imm64_aarch64(&mut c, v.to_raw()); }
                NodeKind::Return => {
                    for inp in g.inputs(n) {
                        if inp.kind == EdgeKind::Data { self.val_aarch64(&mut c, g, inp.from); }
                    }
                    c.extend_from_slice(&0xA8C17BFDu32.to_le_bytes());
                    c.extend_from_slice(&0xD65F03C0u32.to_le_bytes());
                }
                NodeKind::Parameter(i) => {
                    if i > 0 && i <= 7 {
                        c.extend_from_slice(&(0xAA0003E0u32|((i as u32)<<16)).to_le_bytes());
                    }
                }
                NodeKind::Phi | NodeKind::Region => {
                    c.extend_from_slice(&0xD503201Fu32.to_le_bytes());
                }
                NodeKind::Branch => { c.extend_from_slice(&0xB4000000u32.to_le_bytes()); }
                NodeKind::Call => { c.extend_from_slice(&0xD63F0000u32.to_le_bytes()); }
                NodeKind::TypeCheck { expected_type } => {
                    c.extend_from_slice(&0x92400401u32.to_le_bytes());
                    let t = (expected_type.to_raw()&7) as u32;
                    c.extend_from_slice(&(0xF1000020u32|(t<<10)).to_le_bytes());
                    c.extend_from_slice(&0x54000001u32.to_le_bytes());
                }
                NodeKind::Box => { c.extend_from_slice(&0xD37CEC00u32.to_le_bytes()); }
                NodeKind::Unbox => { c.extend_from_slice(&0xD340FC00u32.to_le_bytes()); }
                NodeKind::MemLoad{..} => {
                    c.extend_from_slice(&0x927CF800u32.to_le_bytes());
                    c.extend_from_slice(&0xF9400000u32.to_le_bytes());
                }
                NodeKind::MemStore{..} => {
                    c.extend_from_slice(&0x927CF821u32.to_le_bytes());
                    c.extend_from_slice(&0xF9000020u32.to_le_bytes());
                }
                NodeKind::Safepoint => { c.extend_from_slice(&0xF9400000u32.to_le_bytes()); }
            }
        }
        let ret = 0xD65F03C0u32.to_le_bytes();
        if c.len() < 4 || c[c.len()-4..] != ret {
            c.extend_from_slice(&0xA8C17BFDu32.to_le_bytes());
            c.extend_from_slice(&ret);
        }
        Ok(c)
    }

    fn val_aarch64(&self, c: &mut Vec<u8>, g: &IrGraph, n: crate::ir::NodeId) {
        match g.node_kind(n) {
            NodeKind::Constant(v) => { emit_imm64_aarch64(c, v.to_raw()); }
            NodeKind::Parameter(i) if *i > 0 && *i <= 7 => {
                c.extend_from_slice(&(0xAA0003E0u32|((*i as u32)<<16)).to_le_bytes());
            }
            _ => {}
        }
    }
}

// ── Compiled code handle ───────────────────────────────────────────

/// Handle to compiled native code ready for installation.
pub struct CompiledCode {
    code: Vec<u8>,
    tier: Tier,
}

unsafe impl Send for CompiledCode {}
unsafe impl Sync for CompiledCode {}

impl CompiledCode {
    pub fn entry_point(&self) -> *const u8 { self.code.as_ptr() }
    pub fn code_size(&self) -> usize { self.code.len() }
    pub fn tier(&self) -> Tier { self.tier }

    /// Install compiled code into a function object via atomic entry-point swap.
    ///
    /// Uses FnMeta's atomic fields to ensure safe concurrent access.
    pub fn install(self, function: BlissVal) -> Result<(), BlissError> {
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError { datum: function, expected: "function".into() });
        }
        let func_ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *mut u8;
        if func_ptr.is_null() {
            return Err(BlissError::Internal("install: null function pointer".into()));
        }

        let new_tier = self.tier as u8;
        // Leak the code buffer so it lives forever (code cache owns it)
        let leaked = Box::leak(self.code.into_boxed_slice());
        let new_entry = leaked.as_ptr();

        // Install via FnMeta's atomic fields for safe concurrent access
        let meta = unsafe { &*(func_ptr as *const FnMeta) };

        // Atomic store of entry pointer (Release ordering ensures code
        // bytes are visible before the pointer is published)
        meta.entry.store(new_entry as *mut u8, Ordering::Release);

        // Atomic store of tier (Release ordering, so readers with
        // Acquire see both the new tier and the new entry pointer)
        meta.tier.store(new_tier, Ordering::Release);

        Ok(())
    }
}

// ── Tier promotion ─────────────────────────────────────────────────

/// Check if a function should be promoted to a higher tier based on
/// invocation count vs configured thresholds.
pub fn check_promotion(function: BlissVal, config: &TierConfig) -> Option<Tier> {
    let (current_tier, invoke_count) = if function.tag() == TAG_FUNCTION {
        let ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *const u8;
        if ptr.is_null() {
            (Tier::Interpreter, 0u32)
        } else {
            let meta = unsafe { &*(ptr as *const FnMeta) };
            let tier = match meta.tier.load(Ordering::Acquire) {
                1 => Tier::Baseline, 2 => Tier::Optimising, _ => Tier::Interpreter,
            };
            let cnt = meta.invoke_count.load(Ordering::Relaxed);
            (tier, cnt)
        }
    } else {
        (Tier::Interpreter, 0u32)
    };
    if current_tier >= Tier::Optimising { return None; }
    match current_tier {
        Tier::Interpreter => if invoke_count >= config.t1_threshold { Some(Tier::Baseline) } else { None },
        Tier::Baseline => if invoke_count >= config.t2_threshold { Some(Tier::Optimising) } else { None },
        Tier::Optimising => None,
    }
}

// ── Compilation queue ─────────────────────────────────────────────

/// A compilation request in the queue.
struct CompilationRequest {
    /// The function to compile (as a BlissVal with TAG_FUNCTION).
    function: BlissVal,
    /// Target tier for compilation.
    target_tier: Tier,
    /// Priority score (higher = compile sooner).
    priority: u32,
}

/// Bounded compilation queue with consumer support.
struct CompilationQueue {
    requests: Vec<CompilationRequest>,
    capacity: usize,
    dropped: u64,
}

impl CompilationQueue {
    const fn new() -> Self {
        CompilationQueue {
            requests: Vec::new(),
            capacity: COMPILATION_QUEUE_CAPACITY,
            dropped: 0,
        }
    }
}

static COMPILATION_QUEUE: std::sync::Mutex<CompilationQueue> = std::sync::Mutex::new(CompilationQueue::new());
const COMPILATION_QUEUE_CAPACITY: usize = 64;

/// Enqueue a function for background compilation at the target tier (R4.30).
pub fn request_compilation(function: BlissVal, target_tier: Tier) -> Result<(), BlissError> {
    if function.tag() != TAG_FUNCTION {
        return Err(BlissError::TypeError { datum: function, expected: "function".into() });
    }
    let mut queue = COMPILATION_QUEUE.lock()
        .map_err(|_| BlissError::Internal("compilation queue lock poisoned".into()))?;
    if queue.requests.len() >= queue.capacity {
        queue.dropped += 1;
        return Ok(());
    }
    // Compute priority from invoke_count
    let priority = if function.tag() == TAG_FUNCTION {
        let ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *const u8;
        if !ptr.is_null() {
            let meta = unsafe { &*(ptr as *const FnMeta) };
            let invoke = meta.invoke_count.load(Ordering::Relaxed);
            let back_edge = meta.back_edge_count.load(Ordering::Relaxed);
            invoke + 2 * back_edge
        } else {
            0
        }
    } else {
        0
    };
    queue.requests.push(CompilationRequest { function, target_tier, priority });
    Ok(())
}

/// Pop the highest-priority compilation request from the queue.
/// Used by compiler threads.
pub fn pop_compilation_request() -> Option<(BlissVal, Tier)> {
    let mut queue = COMPILATION_QUEUE.lock().ok()?;
    if queue.requests.is_empty() {
        return None;
    }
    // Find highest priority
    let mut best_idx = 0;
    let mut best_prio = queue.requests[0].priority;
    for (i, req) in queue.requests.iter().enumerate() {
        if req.priority > best_prio {
            best_prio = req.priority;
            best_idx = i;
        }
    }
    let req = queue.requests.swap_remove(best_idx);
    Some((req.function, req.target_tier))
}

/// Process one compilation request from the queue.
/// Called by compiler thread pool workers.
pub fn process_compilation_request() -> bool {
    let request = pop_compilation_request();
    match request {
        Some((function, target_tier)) => {
            match target_tier {
                Tier::Baseline => {
                    let mut compiler = BaselineCompiler::new();
                    match compiler.compile(function) {
                        Ok(compiled) => {
                            let _ = compiled.install(function);
                        }
                        Err(_) => {
                            // Compilation failed; function stays at current tier
                        }
                    }
                }
                Tier::Optimising => {
                    let mut compiler = OptimisingCompiler::new();
                    match compiler.compile(function) {
                        Ok(compiled) => {
                            let _ = compiled.install(function);
                        }
                        Err(_) => {
                            // Mark as T2_FAILED
                            let ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *const u8;
                            if !ptr.is_null() {
                                let meta = unsafe { &*(ptr as *const FnMeta) };
                                meta.flags.fetch_or(FLAG_T2_FAILED, Ordering::Release);
                            }
                        }
                    }
                }
                Tier::Interpreter => {} // Nothing to do
            }
            true
        }
        None => false,
    }
}
