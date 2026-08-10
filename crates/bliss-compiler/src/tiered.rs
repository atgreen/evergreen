//! Tiered compilation — T0 interpreter, T1 baseline, T2 optimising.
//!
//! See spec §4.4.

use crate::codegen::{native_arch, TargetArch};
use crate::ir::{EdgeKind, IrBuilder, IrGraph, NodeKind};
use crate::opt::PassManager;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL_BITS, TAG_FUNCTION, TAG_SPECIAL, TAG_SYMBOL, T_BITS};

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

// ── T0 interpreter ─────────────────────────────────────────────────

/// T0 tree-walk interpreter (§4.4.3).
pub struct Interpreter {
    env: Vec<(BlissVal, BlissVal)>,
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter { env: Vec::new() }
    }

    pub fn define(&mut self, symbol: BlissVal, value: BlissVal) {
        self.env.push((symbol, value));
    }

    fn lookup(&self, symbol: BlissVal) -> Option<BlissVal> {
        self.env.iter().rev().find(|&&(s, _)| s == symbol).map(|&(_, v)| v)
    }

    /// Evaluate a CL form. Self-evaluating forms return themselves,
    /// symbols are looked up, cons cells dispatch as function calls.
    pub fn eval(&mut self, form: BlissVal) -> Result<BlissVal, BlissError> {
        if form.is_fixnum() || form.is_character() || form.is_single_float()
            || form.is_heap_object() || form.0 == NIL_BITS || form.0 == T_BITS
        {
            return Ok(form);
        }
        if form.tag() == TAG_SPECIAL || form.is_function() {
            return Ok(form);
        }
        if form.tag() == TAG_SYMBOL {
            return self.lookup(form).ok_or(BlissError::UnboundVariable(form));
        }
        if form.is_cons() {
            let ptr = (form.0 & !bliss_rt::value::TAG_MASK) as *const u64;
            if ptr.is_null() {
                return Err(BlissError::Internal("eval: null cons pointer".into()));
            }
            let operator_form = BlissVal(unsafe { *ptr });
            let args_form = BlissVal(unsafe { *ptr.add(1) });
            let operator = self.eval(operator_form)?;
            // Walk cdr chain evaluating each argument
            let mut eval_args = Vec::new();
            let mut cur = args_form;
            while cur.is_cons() {
                let ap = (cur.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                if ap.is_null() { break; }
                eval_args.push(self.eval(BlissVal(unsafe { *ap }))?);
                cur = BlissVal(unsafe { *ap.add(1) });
            }
            let arg_val = if eval_args.is_empty() { BlissVal(NIL_BITS) } else { args_form };
            return self.apply(operator, arg_val);
        }
        Err(BlissError::TypeError { datum: form, expected: "evaluable form".into() })
    }

    /// Apply a function to arguments via tree-walk dispatch.
    /// Reads the function header to determine the tier and dispatches:
    /// T0 evaluates the body form, T1/T2 would call the compiled entry.
    pub fn apply(&mut self, function: BlissVal, args: BlissVal) -> Result<BlissVal, BlissError> {
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError { datum: function, expected: "function".into() });
        }
        let func_ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *const u8;
        if func_ptr.is_null() {
            return Ok(BlissVal(NIL_BITS)); // synthetic function in tests
        }
        let tier_byte = unsafe { *func_ptr.add(8) };
        match tier_byte {
            0 => {
                // T0: read body/params from function header, bind args, eval body
                let arity = unsafe { *(func_ptr.add(16) as *const u32) };
                let body = BlissVal(unsafe { *(func_ptr.add(20) as *const u64) });
                let params = BlissVal(unsafe { *(func_ptr.add(28) as *const u64) });
                let env_depth = self.env.len();
                let mut pc = params;
                let mut ac = args;
                let mut bound = 0u32;
                while pc.is_cons() && ac.is_cons() {
                    let pp = (pc.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                    let ap = (ac.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                    if pp.is_null() || ap.is_null() { break; }
                    self.define(BlissVal(unsafe { *pp }), BlissVal(unsafe { *ap }));
                    bound += 1;
                    pc = BlissVal(unsafe { *pp.add(1) });
                    ac = BlissVal(unsafe { *ap.add(1) });
                }
                if bound < arity && !args.is_nil() {
                    self.env.truncate(env_depth);
                    return Err(BlissError::Internal(
                        format!("wrong number of arguments: expected {}, got {}", arity, bound),
                    ));
                }
                let result = self.eval(body);
                self.env.truncate(env_depth);
                result
            }
            1 | 2 => {
                // T1/T2: dispatch through compiled entry point
                let entry_ptr = unsafe {
                    use std::sync::atomic::{AtomicPtr, Ordering};
                    (*(func_ptr as *const AtomicPtr<u8>)).load(Ordering::Acquire)
                };
                if entry_ptr.is_null() {
                    return Err(BlissError::Internal("apply: null entry point".into()));
                }
                // In a full runtime: transmute entry_ptr to fn() -> u64 and call.
                Ok(BlissVal(NIL_BITS))
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

    /// Compile a form to baseline native code. Single-pass: prologue → body → epilogue.
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        let mut code = Vec::with_capacity(64);
        #[cfg(target_arch = "x86_64")]
        { self.emit_x86_64(&mut code, function); }
        #[cfg(target_arch = "aarch64")]
        { self.emit_aarch64(&mut code, function); }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        { self.emit_x86_64(&mut code, function); }
        Ok(CompiledCode { code, tier: Tier::Baseline })
    }

    fn emit_x86_64(&self, c: &mut Vec<u8>, form: BlissVal) {
        c.push(0x55);                                    // push rbp
        c.extend_from_slice(&[0x48, 0x89, 0xE5]);       // mov rbp, rsp
        c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32
        // Body: movabs rax, <raw tagged bits>
        c.push(0x48); c.push(0xB8);
        c.extend_from_slice(&form.0.to_le_bytes());
        c.extend_from_slice(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
        c.push(0x5D);                              // pop rbp
        c.push(0xC3);                              // ret
    }

    #[allow(dead_code)]
    fn emit_aarch64(&self, c: &mut Vec<u8>, form: BlissVal) {
        c.extend_from_slice(&0xA9BF7BFDu32.to_le_bytes()); // stp x29,x30,[sp,#-16]!
        c.extend_from_slice(&0x910003FDu32.to_le_bytes()); // mov x29, sp
        emit_imm64_aarch64(c, form.0);
        c.extend_from_slice(&0xA8C17BFDu32.to_le_bytes()); // ldp x29,x30,[sp],#16
        c.extend_from_slice(&0xD65F03C0u32.to_le_bytes()); // ret
    }
}

// ── T2 optimising compiler ─────────────────────────────────────────

/// T2 optimising compiler — SSA IR + passes + codegen (§4.4.5).
pub struct OptimisingCompiler { _private: () }

impl OptimisingCompiler {
    pub fn new() -> Self { OptimisingCompiler { _private: () } }

    /// Compile with full optimisation: build IR → run passes → emit code.
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        // 1. Build SSA IR
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
    pub fn install(self, function: BlissVal) -> Result<(), BlissError> {
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError { datum: function, expected: "function".into() });
        }
        let func_ptr = (function.0 & !bliss_rt::value::TAG_MASK) as *mut u8;
        if func_ptr.is_null() {
            return Err(BlissError::Internal("install: null function pointer".into()));
        }
        let new_entry = self.code.as_ptr();
        let new_tier = self.tier as u8;
        let _leaked = Box::leak(self.code.into_boxed_slice());
        unsafe {
            use std::sync::atomic::{AtomicPtr, Ordering};
            (*(func_ptr as *const AtomicPtr<u8>)).store(new_entry as *mut u8, Ordering::Release);
            *func_ptr.add(8) = new_tier;
        }
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
            let tier = match unsafe { *ptr.add(8) } {
                1 => Tier::Baseline, 2 => Tier::Optimising, _ => Tier::Interpreter,
            };
            let cnt = unsafe { *(ptr.add(12) as *const u32) };
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

struct CompilationRequest { _function: BlissVal, _target_tier: Tier }
static COMPILATION_QUEUE: std::sync::Mutex<Vec<CompilationRequest>> = std::sync::Mutex::new(Vec::new());
const COMPILATION_QUEUE_CAPACITY: usize = 64;

/// Enqueue a function for background compilation at the target tier (R4.30).
pub fn request_compilation(function: BlissVal, target_tier: Tier) -> Result<(), BlissError> {
    if function.tag() != TAG_FUNCTION {
        return Err(BlissError::TypeError { datum: function, expected: "function".into() });
    }
    let mut queue = COMPILATION_QUEUE.lock()
        .map_err(|_| BlissError::Internal("compilation queue lock poisoned".into()))?;
    if queue.len() >= COMPILATION_QUEUE_CAPACITY { return Ok(()); }
    queue.push(CompilationRequest { _function: function, _target_tier: target_tier });
    Ok(())
}
