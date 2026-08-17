//! T2 machine-code emission — MachFunc → executable x86-64 bytes (spec §4.7).
//!
//! The final lowering stage: after P5 selected `MachInst`s and P6 assigned every
//! `VReg` a `Location`, this walks the `MachFunc` and emits real x86-64 machine
//! code through the shared label assembler (`bliss_rt::asm`). The result is a
//! byte buffer that `bliss_rt::jit::JitBuffer` can make executable.
//!
//! **First cut.** Supports register-resident straight-line + unconditional-jump
//! code (MOV_IMM, reg-reg MOV, ADD, SUB, RET, JMP) — enough to emit and *run* a
//! T2-compiled leaf function. Deferred (documented, not silent): spilled operands
//! (`Location::Stack`), conditional branches (need the condition-code model P5
//! flagged), calls, float/XMM ops, and folding immediates into ALU ops. An
//! unsupported instruction returns `EmitError` rather than emitting wrong bytes.

use bliss_rt::asm::{Asm, Cc};

use crate::t2::ir::Function;
use crate::t2::mach::{Location, MachFunc, MachInst, PhysReg, RegClass, VReg};

/// Why emission could not complete (the function stays at T1, spec R4.28/R4.42).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// An opcode this first-cut emitter does not encode yet.
    UnsupportedOp(u32),
    /// A value was spilled to the stack; the frame/spill path isn't emitted yet.
    Spilled(VReg),
    /// A vreg had no allocation (P6 did not run, or left it unbound).
    Unallocated(VReg),
    /// A `MOV_IMM` with no immediate (lowering did not populate it).
    MissingImm,
    /// A float/XMM operand reached the integer-only first cut.
    FloatUnsupported,
    /// Branch resolution failed (out-of-range / unbound label).
    BadBranch,
}

// ── PReg index → x86-64 hardware register encoding ──────────────────
//
// P6 stores regalloc2's PReg index (0..N_GPR) in `PhysReg.encoding`. This table
// maps those abstract indices to real x86-64 GPR encodings, caller-saved first
// so small functions never touch a callee-saved register (and thus need no
// save/restore). rsp(4)/rbp(5) are excluded (frame/stack).
const GPR_X86: [u8; 14] = [
    0,  // rax   caller-saved
    1,  // rcx   caller-saved
    2,  // rdx   caller-saved
    6,  // rsi   caller-saved
    7,  // rdi   caller-saved
    8,  // r8    caller-saved
    9,  // r9    caller-saved
    10, // r10   caller-saved
    11, // r11   caller-saved
    3,  // rbx   callee-saved
    12, // r12   callee-saved
    13, // r13   callee-saved
    14, // r14   callee-saved
    15, // r15   callee-saved
];

/// x86-64 encodings of the callee-saved GPRs SysV requires a function to
/// preserve (rbx, r12–r15). rbp/rsp are handled separately.
const CALLEE_SAVED_X86: [u8; 5] = [3, 12, 13, 14, 15];

const RAX: u8 = 0;

/// The x86 encoding a GPR `PhysReg` maps to.
fn gpr_enc(p: PhysReg) -> Result<u8, EmitError> {
    if p.class != RegClass::Gpr {
        return Err(EmitError::FloatUnsupported);
    }
    GPR_X86.get(p.encoding as usize).copied().ok_or(EmitError::UnsupportedOp(0))
}

// ── x86-64 instruction encoders (REX.W 64-bit forms) ────────────────

/// `REX.W` prefix with the R (reg-field) and B (rm-field) extension bits.
fn rex_w(reg: u8, rm: u8) -> u8 {
    0x48 | ((reg >> 3) & 1) << 2 | ((rm >> 3) & 1)
}

/// ModRM for register-direct (mod=11): reg field + rm field.
fn modrm_rr(reg: u8, rm: u8) -> u8 {
    0xC0 | ((reg & 7) << 3) | (rm & 7)
}

/// `mov r64, imm64` (REX.W, B8+rd, imm64).
fn mov_imm64(a: &mut Asm, dst: u8, imm: i64) {
    a.push(0x48 | ((dst >> 3) & 1)); // REX.W (+B for r8..r15)
    a.push(0xB8 + (dst & 7));
    a.extend_from_slice(&imm.to_le_bytes());
}

/// A two-operand ALU/mov of the form `op r/m64, r64` (dst is r/m, src is reg).
fn alu_rr(a: &mut Asm, opcode: u8, dst: u8, src: u8) {
    a.push(rex_w(src, dst));
    a.push(opcode);
    a.push(modrm_rr(src, dst));
}

fn mov_rr(a: &mut Asm, dst: u8, src: u8) {
    if dst != src {
        alu_rr(a, 0x89, dst, src);
    }
}

fn push_reg(a: &mut Asm, r: u8) {
    if r >= 8 {
        a.push(0x41);
    }
    a.push(0x50 + (r & 7));
}

fn pop_reg(a: &mut Asm, r: u8) {
    if r >= 8 {
        a.push(0x41);
    }
    a.push(0x58 + (r & 7));
}

/// `mov r64, [r14 + 8*i]` — load an argument/local from its interpreter frame
/// slot (r14 = the slots pointer, per the run_native ABI).
fn mov_from_frame(a: &mut Asm, dst: u8, i: usize) {
    a.push(0x49 | if dst >= 8 { 0x04 } else { 0 }); // REX.W + B(base r14) [+R]
    a.push(0x8B); // mov r64, r/m64
    a.push(0x40 | ((dst & 7) << 3) | 6); // mod=01(disp8), rm=110(r14)
    a.push((8 * i) as u8);
}

/// `sar r64, imm8`.
fn sar_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xF8 | (r & 7)); // /7
    a.push(imm);
}

/// `imul r64, r/m64` (two-operand: dst *= src).
fn imul_rr(a: &mut Asm, dst: u8, src: u8) {
    a.push(rex_w(dst, src));
    a.push(0x0F);
    a.push(0xAF);
    a.push(modrm_rr(dst, src));
}

// ── Framed emitter (T2, interpreter-callable) ───────────────────────
//
// Emits a straight-line speculated function as native code callable by the T0
// run_native adapter: the slots pointer arrives in rdi (→ r14), arguments are
// read from frame slots, the result is returned in rax, and a guard miss calls
// `c2i_deopt` — the no-resume deopt that makes run_native re-run the (pure)
// function in the interpreter. This is where the operand stack disappears: the
// fast path works in registers, only touching memory to load the args.

const SCRATCH: u8 = 2; // rdx — reserved for guard temporaries, never a value reg

/// Emit `f` (a single-block, speculated straight-line function) as native code.
/// `c2i_deopt_addr` is the address of the interpreter's `c2i_deopt` routine.
pub fn emit_framed(f: &Function, c2i_deopt_addr: u64) -> Result<Vec<u8>, EmitError> {
    use crate::t2::ir::{AuxData, Opcode, Value};
    use std::collections::HashMap;

    if f.block_order().len() != 1 {
        return Err(EmitError::UnsupportedOp(0xF0)); // only straight-line for now
    }
    let entry = f.entry();

    // Trivial register assignment: one x86 GPR per SSA value from a small pool
    // (rax reserved for return/imul, rdx for guard scratch, r14 for the frame).
    let mut pool: Vec<u8> = vec![10, 9, 8, 6, 1]; // r10, r9, r8, rsi, rcx (pop→rcx first)
    let mut reg: HashMap<Value, u8> = HashMap::new();
    let mut alloc = |v: Value, reg: &mut HashMap<Value, u8>, pool: &mut Vec<u8>| -> Result<u8, EmitError> {
        if let Some(&r) = reg.get(&v) {
            return Ok(r);
        }
        let r = pool.pop().ok_or(EmitError::UnsupportedOp(0xF1))?; // out of registers
        reg.insert(v, r);
        Ok(r)
    };
    let get = |v: Value, reg: &HashMap<Value, u8>| -> Result<u8, EmitError> {
        reg.get(&v).copied().ok_or(EmitError::UnsupportedOp(0xF2))
    };

    let mut a = Asm::new();
    let deopt = a.label();

    // Prologue: save r14, point it at the frame slots (rdi), load the params.
    push_reg(&mut a, 14);
    a.extend_from_slice(&[0x49, 0x89, 0xFE]); // mov r14, rdi
    let params = f.block(entry).params.clone();
    for (i, &p) in params.iter().enumerate() {
        let r = alloc(p, &mut reg, &mut pool)?;
        mov_from_frame(&mut a, r, i);
    }

    // Body.
    for &inst in &f.block(entry).insts.clone() {
        let data = f.inst(inst).clone();
        match data.opcode {
            Opcode::ConstFixnum => {
                let imm = match data.aux {
                    AuxData::FixnumImm(v) => v,
                    _ => return Err(EmitError::MissingImm),
                };
                let dst = alloc(data.results[0], &mut reg, &mut pool)?;
                // Materialise the tagged BlissVal fixnum.
                mov_imm64(&mut a, dst, bliss_rt::value::BlissVal::from_fixnum(imm).0 as i64);
            }
            Opcode::FixnumMul => {
                let x = get(data.args[0], &reg)?;
                let y = get(data.args[1], &reg)?;
                let r = alloc(data.results[0], &mut reg, &mut pool)?;
                // Guard: both operands fixnum, else deopt.
                alu_rr(&mut a, 0x89, SCRATCH, x); // mov rdx, x
                alu_rr(&mut a, 0x09, SCRATCH, y); // or  rdx, y
                a.extend_from_slice(&[0xF6, 0xC2, 0x07]); // test dl, 7
                a.jcc(Cc::Ne, deopt);
                // r = (untag x) * y  → tagged product; overflow → deopt.
                mov_rr(&mut a, r, x);
                sar_imm(&mut a, r, 3);
                imul_rr(&mut a, r, y);
                a.jcc(Cc::O, deopt);
            }
            other if other.is_terminator() => break, // handled below
            other => return Err(EmitError::UnsupportedOp(op_tag(other))),
        }
    }

    // Terminator: Return the value in rax.
    let term = f.terminator(entry).ok_or(EmitError::UnsupportedOp(0xF3))?;
    let term_data = f.inst(term).clone();
    if term_data.opcode != Opcode::Return {
        return Err(EmitError::UnsupportedOp(op_tag(term_data.opcode)));
    }
    if let Some(&v) = term_data.args.first() {
        let r = get(v, &reg)?;
        mov_rr(&mut a, 0, r); // mov rax, r
    }
    pop_reg(&mut a, 14);
    a.push(0xC3); // ret

    // Deopt stub: call c2i_deopt (sets NATIVE_DEOPT with no resume point, so
    // run_native re-runs the pure function in the interpreter), then return.
    a.bind(deopt);
    mov_imm64(&mut a, 0, c2i_deopt_addr as i64); // mov rax, c2i_deopt
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    pop_reg(&mut a, 14);
    a.push(0xC3); // ret (value ignored on deopt)

    a.finish().ok_or(EmitError::BadBranch)
}

fn op_tag(op: crate::t2::ir::Opcode) -> u32 {
    op as u32 | 0x1000
}

// ── Emitter ─────────────────────────────────────────────────────────

/// Emit `mf` to x86-64 machine code. `mf.allocation` must be populated (P6).
pub fn emit(mf: &MachFunc) -> Result<Vec<u8>, EmitError> {
    // VReg → its assigned x86 GPR encoding.
    let loc_of = |v: VReg| -> Result<u8, EmitError> {
        let (_, loc) = mf
            .allocation
            .iter()
            .find(|(vr, _)| *vr == v)
            .ok_or(EmitError::Unallocated(v))?;
        match loc {
            Location::Register(p) => gpr_enc(*p),
            Location::Stack(_) => Err(EmitError::Spilled(v)),
        }
    };

    // Which callee-saved registers the body actually uses → prologue/epilogue.
    let mut used_callee: Vec<u8> = Vec::new();
    for (_, loc) in &mf.allocation {
        if let Location::Register(p) = loc {
            if let Ok(enc) = gpr_enc(*p) {
                if CALLEE_SAVED_X86.contains(&enc) && !used_callee.contains(&enc) {
                    used_callee.push(enc);
                }
            }
        }
    }

    let mut a = Asm::new();

    // ── Prologue: save callee-saved registers this function clobbers.
    for &r in &used_callee {
        push_reg(&mut a, r);
    }

    // Emit the body. A `RET` runs the epilogue (restore + `ret`) inline. Block
    // structure is used for labels once branches are supported; the straight-line
    // first cut walks the flat inst vector in order.
    let epilogue = |a: &mut Asm| {
        for &r in used_callee.iter().rev() {
            pop_reg(a, r);
        }
        a.push(0xC3); // ret
    };

    for mi in &mf.insts {
        emit_inst(&mut a, mi, &loc_of, &epilogue)?;
    }

    a.finish().ok_or(EmitError::BadBranch)
}

fn emit_inst(
    a: &mut Asm,
    mi: &MachInst,
    loc_of: &impl Fn(VReg) -> Result<u8, EmitError>,
    epilogue: &impl Fn(&mut Asm),
) -> Result<(), EmitError> {
    use crate::t2::lower::op;
    match mi.op {
        op::MOV_IMM | op::MOV_TAGGED => {
            // Both materialise a 64-bit immediate into a GPR; MOV_TAGGED's is a
            // tagged BlissVal, MOV_IMM's a raw/tagged integer — identical encoding.
            let dst = loc_of(mi.defs[0])?;
            let imm = mi.imm.ok_or(EmitError::MissingImm)?;
            mov_imm64(a, dst, imm);
        }
        op::MOV => {
            let dst = loc_of(mi.defs[0])?;
            let src = loc_of(mi.uses[0])?;
            mov_rr(a, dst, src);
        }
        op::ADD | op::SUB => {
            // def = uses[0] (op) uses[1]. x86 ALU is 2-operand, so realise as
            // `mov def, a; op def, b` (skips the mov when def already is a).
            let dst = loc_of(mi.defs[0])?;
            let a0 = loc_of(mi.uses[0])?;
            let b0 = loc_of(mi.uses[1])?;
            mov_rr(a, dst, a0);
            let opcode = if mi.op == op::ADD { 0x01 } else { 0x29 };
            alu_rr(a, opcode, dst, b0);
        }
        op::RET => {
            // Move the return value into rax (SysV return register), then the
            // epilogue restores callee-saved regs and returns.
            if let Some(&v) = mi.uses.first() {
                let r = loc_of(v)?;
                mov_rr(a, RAX, r);
            }
            epilogue(a);
        }
        other => return Err(EmitError::UnsupportedOp(other)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::lower::op;
    use crate::t2::mach::{MachInst, VReg};
    use crate::t2::regalloc::allocate;

    fn gpr(num: u32) -> VReg {
        VReg { class: RegClass::Gpr, num }
    }

    fn mi(op: u32, defs: Vec<VReg>, uses: Vec<VReg>, imm: Option<i64>) -> MachInst {
        MachInst { op, defs, uses, imm, frame_state: None, safepoint: false }
    }

    /// Build `() -> imm`: MOV_IMM v0, imm ; RET v0. Allocate, emit, and check the
    /// byte stream is non-empty and ends in `ret`.
    #[test]
    fn emits_const_return_bytes() {
        let mut mf = MachFunc::default();
        mf.insts = vec![
            mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(12345)),
            mi(op::RET, vec![], vec![gpr(0)], None),
        ];
        allocate(&mut mf);
        let code = emit(&mf).expect("emit const-return");
        assert!(!code.is_empty());
        assert_eq!(*code.last().unwrap(), 0xC3, "must end in ret");
    }

    /// The real milestone: emit a T2 function and EXECUTE it. `() -> 12345` must
    /// return 12345 when called as a native C function.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn emitted_code_executes() {
        let mut mf = MachFunc::default();
        mf.insts = vec![
            mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(12345)),
            mi(op::RET, vec![], vec![gpr(0)], None),
        ];
        allocate(&mut mf);
        let code = emit(&mf).expect("emit");
        let buf = bliss_rt::jit::JitBuffer::new(&code).expect("mmap exec");
        // SAFETY: the emitted code is a leaf `extern "C" fn() -> u64` that only
        // writes rax and returns, preserving callee-saved registers.
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), 12345, "T2-emitted function must return its constant");
    }

    use std::sync::atomic::{AtomicBool, Ordering};
    static DEOPTED: AtomicBool = AtomicBool::new(false);
    extern "C" fn mock_c2i_deopt() {
        DEOPTED.store(true, Ordering::SeqCst);
    }

    // Build and speculate `(lambda (x) (* x 5))` into single-guarded-FixnumMul IR.
    #[cfg(all(target_arch = "x86_64", unix))]
    fn speculated_mul5() -> crate::t2::ir::Function {
        use crate::t2::speculate::{speculate, SpecType};
        use bliss_rt::bytecode::{BytecodeFunction, Instr};
        use bliss_rt::value::BlissVal;
        let star = bliss_rt::symbols::intern("*");
        let bf = BytecodeFunction {
            code: vec![
                Instr::LoadLocal(0),
                Instr::Const(0),
                Instr::CallNamed { sym: star, nargs: 2 },
                Instr::Return,
            ],
            constants: vec![BlissVal::from_fixnum(5)],
            handler_cases: vec![], handler_binds: vec![], names: vec![], restart_cases: vec![],
            param_layout: vec![], has_env: false, n_locals: 1, max_stack: 2, arity: 1,
            name: "mul5".into(),
        };
        let mut f = crate::t2::build::build_from_bytecode(&bf).expect("build");
        speculate(&mut f, &|bcp| if bcp == 2 { Some(SpecType::Fixnum) } else { None });
        f
    }

    /// The framed-emitter milestone: a T2-emitted `(* x 5)` runs via the
    /// interpreter frame ABI — a fixnum arg gives `x*5` (fast path, operand stack
    /// gone), a float arg trips the guard and calls the deopt routine.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn framed_fixnum_mul_runs_and_deopts() {
        use bliss_rt::value::BlissVal;
        let f = speculated_mul5();
        let code = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit_framed");
        let buf = bliss_rt::jit::JitBuffer::new(&code).expect("mmap");
        // extern "C" fn(*mut u64) -> u64 : rdi = frame slots, returns rax.
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

        // Fixnum arg 7 in slot 0 → fast path → 7*5 = 35.
        let mut frame = [BlissVal::from_fixnum(7).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let r = func(frame.as_mut_ptr());
        assert!(!DEOPTED.load(Ordering::SeqCst), "fixnum arg must not deopt");
        assert_eq!(BlissVal(r).as_fixnum(), 35, "T2 must compute 7*5 = 35");

        // Float arg → guard fails → deopt routine called.
        let mut frame = [BlissVal::from_single_float(2.0).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let _ = func(frame.as_mut_ptr());
        assert!(DEOPTED.load(Ordering::SeqCst), "a float operand must deopt to the interpreter");
    }

    /// A small computation `() -> 7 + 5 = 12`, executed.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn emitted_addition_executes() {
        let mut mf = MachFunc::default();
        mf.insts = vec![
            mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(7)),
            mi(op::MOV_IMM, vec![gpr(1)], vec![], Some(5)),
            mi(op::ADD, vec![gpr(2)], vec![gpr(0), gpr(1)], None),
            mi(op::RET, vec![], vec![gpr(2)], None),
        ];
        allocate(&mut mf);
        let code = emit(&mf).expect("emit");
        let buf = bliss_rt::jit::JitBuffer::new(&code).expect("mmap exec");
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), 12, "7 + 5 must execute to 12");
    }
}
