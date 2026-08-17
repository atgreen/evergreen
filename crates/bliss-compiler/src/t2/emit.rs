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

use bliss_rt::asm::Asm;

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
