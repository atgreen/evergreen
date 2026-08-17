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

/// `mov dst, [base + 8*i]` — load an argument/local from its interpreter frame
/// slot. `base` is the slots-pointer register (rdi in the frame-less fast path,
/// or r14 when a frame is set up).
fn mov_from_frame(a: &mut Asm, dst: u8, base: u8, i: usize) {
    let mut rex = 0x48; // REX.W
    if dst >= 8 {
        rex |= 0x04; // REX.R
    }
    if base >= 8 {
        rex |= 0x01; // REX.B
    }
    a.push(rex);
    a.push(0x8B); // mov r64, r/m64
    a.push(0x40 | ((dst & 7) << 3) | (base & 7)); // mod=01(disp8), rm=base
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

// ── Single-float (XMM) encoders ─────────────────────────────────────
//
// A single-float BlissVal is the immediate `(f32_bits << 32) | 0b100`: the raw
// f32 lives in the high dword, tag 0b100 in the low bits. So the float path
// unpacks with `shr 32` + `movd` into an XMM, computes with a scalar-single SSE
// op, then repacks with `movd` + `shl 32` + `or 4`.

/// `mov r32, imm32` (zero-extends into r64).
fn mov_imm32(a: &mut Asm, r: u8, imm: u32) {
    if r >= 8 {
        a.push(0x41); // REX.B
    }
    a.push(0xB8 + (r & 7));
    a.extend_from_slice(&imm.to_le_bytes());
}

/// `movd xmm, r32` — GPR low dword → XMM (66 0F 6E /r).
fn movd_xmm_r32(a: &mut Asm, xmm: u8, r: u8) {
    a.push(0x66);
    if xmm >= 8 || r >= 8 {
        a.push(0x40 | (((xmm >> 3) & 1) << 2) | ((r >> 3) & 1)); // REX.R(xmm)+B(r)
    }
    a.extend_from_slice(&[0x0F, 0x6E]);
    a.push(modrm_rr(xmm, r)); // reg=xmm, rm=r
}

/// `movd r32, xmm` — XMM low dword → GPR, zero-extended (66 0F 7E /r).
fn movd_r32_xmm(a: &mut Asm, r: u8, xmm: u8) {
    a.push(0x66);
    if xmm >= 8 || r >= 8 {
        a.push(0x40 | (((xmm >> 3) & 1) << 2) | ((r >> 3) & 1)); // REX.R(xmm)+B(r)
    }
    a.extend_from_slice(&[0x0F, 0x7E]);
    a.push(modrm_rr(xmm, r)); // reg=xmm, rm=r
}

/// A scalar-single SSE op `<op>ss xmm_dst, xmm_src` (F3 0F <sub> /r):
/// addss=0x58, mulss=0x59, subss=0x5C.
fn ss_op(a: &mut Asm, sub: u8, dst: u8, src: u8) {
    a.push(0xF3);
    if dst >= 8 || src >= 8 {
        a.push(0x40 | (((dst >> 3) & 1) << 2) | ((src >> 3) & 1));
    }
    a.extend_from_slice(&[0x0F, sub]);
    a.push(modrm_rr(dst, src));
}

/// `shr r64, imm8` (/5).
fn shr_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xE8 | (r & 7));
    a.push(imm);
}

/// `shl r64, imm8` (/4).
fn shl_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xE0 | (r & 7));
    a.push(imm);
}

/// `or r64, imm8` sign-extended (/1).
fn or_imm8(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0x83);
    a.push(0xC8 | (r & 7));
    a.push(imm);
}

/// Guard that `r` holds a single-float (tag == 0b100), else deopt.
fn guard_single_float(a: &mut Asm, r: u8, deopt: bliss_rt::asm::Label) {
    alu_rr(a, 0x89, SCRATCH, r); // mov rdx, r
    a.extend_from_slice(&[0x80, 0xE2, 0x07]); // and dl, 7
    a.extend_from_slice(&[0x80, 0xFA, 0x04]); // cmp dl, 4
    a.jcc(Cc::Ne, deopt);
}

// ── Framed emitter (T2, interpreter-callable) ───────────────────────
//
// Emits a straight-line speculated function as native code callable by the T0
// run_native adapter. The slots pointer arrives in rdi; arguments are read
// straight from `[rdi + 8*i]`. The fast path makes no call, so it needs no stack
// frame and no callee-saved register — it works entirely in registers and
// returns in rax. A guard miss jumps to a cold stub that aligns the stack and
// calls `c2i_deopt` (the no-resume deopt that makes run_native re-run the pure
// function in the interpreter). Instruction selection folds a constant operand
// into the multiply immediate and never materialises it, and only the *variable*
// operand is tag-checked — a statically-known fixnum constant needs no guard.

const SCRATCH: u8 = 2; // rdx — reserved for guard temporaries, never a value reg

/// `test <r low byte>, 7 ; jne deopt` — the fixnum-tag guard on one operand.
/// Correct for registers 0..=3 and 8..=15 (the framed value pool); registers
/// 4..=7 would need a REX prefix to name their low byte and are never allocated.
fn guard_fixnum(a: &mut Asm, r: u8, deopt: bliss_rt::asm::Label) {
    if r >= 8 {
        a.push(0x41); // REX.B → r8b..r15b
    }
    a.push(0xF6); // test r/m8, imm8  (/0)
    a.push(0xC0 | (r & 7)); // mod=11, rm=r
    a.push(0x07);
    a.jcc(Cc::Ne, deopt);
}

/// `imul dst, src, imm32` — dst = src * imm, setting OF on overflow. Multiplying
/// the *tagged* operand by the raw constant yields the tagged product directly
/// (tagged(x)·c = (x<<3)·c = (x·c)<<3 = tagged(x·c)), so no untag/retag is needed.
fn imul_imm(a: &mut Asm, dst: u8, src: u8, imm: i32) {
    a.push(rex_w(dst, src)); // REX.W + R(dst) + B(src)
    a.push(0x69); // imul r64, r/m64, imm32
    a.push(modrm_rr(dst, src)); // reg=dst, rm=src
    a.extend_from_slice(&imm.to_le_bytes());
}

// Fixnum value range: 3 tag bits leave 61 signed value bits.
const FIXNUM_MAX: i64 = (1 << 60) - 1;
const FIXNUM_MIN: i64 = -(1 << 60);

/// The SIB scale bits for `x·c` expressed as `lea [x + x*(c-1)]`, i.e. c-1 ∈
/// {1,2,4,8}. Returns `None` for multipliers `lea` can't do in one instruction.
fn lea_scale(c: i64) -> Option<u8> {
    match c {
        2 => Some(0b00), // x + x*1
        3 => Some(0b01), // x + x*2
        5 => Some(0b10), // x + x*4
        9 => Some(0b11), // x + x*8
        _ => None,
    }
}

/// `lea dst, [base + base*scale]` — dst = base·(1+2^scale_field). Because it works
/// on the *tagged* operand (tagged(x)·c = tagged(x·c)) it needs no untag; because
/// `lea` sets no flags it can only be used when overflow is proven impossible.
fn lea_mul(a: &mut Asm, dst: u8, base: u8, scale: u8) {
    let mut rex = 0x48; // REX.W
    if dst >= 8 {
        rex |= 0x04; // REX.R (reg = dst)
    }
    if base >= 8 {
        rex |= 0x03; // REX.X + REX.B (both index and base are `base`)
    }
    a.push(rex);
    a.push(0x8D); // lea r64, m
    a.push(((dst & 7) << 3) | 0x04); // mod=00, reg=dst, rm=100 → SIB
    a.push((scale << 6) | ((base & 7) << 3) | (base & 7)); // scale, index=base, base=base
}

/// Whether `range · c` is provably inside the fixnum range — so the tagged
/// product `(range·c)<<3` cannot overflow a 64-bit register and no `jo`/deopt is
/// needed. Conservative: an unknown range, a non-positive `c`, or any arithmetic
/// overflow in the check itself all answer "no".
fn mul_cannot_overflow(range: Option<crate::t2::ir::Range>, c: i64) -> bool {
    let Some(r) = range else { return false };
    if c <= 0 {
        return false;
    }
    match (r.lo.checked_mul(c), r.hi.checked_mul(c)) {
        (Some(lo), Some(hi)) => lo >= FIXNUM_MIN && hi <= FIXNUM_MAX,
        _ => false,
    }
}

/// Get value `v` into a register: an already-assigned one, or a deferred fixnum
/// constant materialised (tagged) into a fresh register on demand.
fn framed_mat(
    a: &mut Asm,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    v: crate::t2::ir::Value,
) -> Result<u8, EmitError> {
    if let Some(&r) = reg.get(&v) {
        return Ok(r);
    }
    if let Some(&c) = consts.get(&v) {
        let r = pool.pop().ok_or(EmitError::UnsupportedOp(0xF1))?;
        reg.insert(v, r);
        mov_imm64(a, r, bliss_rt::value::BlissVal::from_fixnum(c).0 as i64);
        return Ok(r);
    }
    Err(EmitError::UnsupportedOp(0xF2))
}

/// Assign a register to an instruction result — honouring a pre-assignment (the
/// return value is bound to rax so the arithmetic lands in the return register).
fn framed_alloc(
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    v: crate::t2::ir::Value,
) -> Result<u8, EmitError> {
    if let Some(&r) = reg.get(&v) {
        return Ok(r);
    }
    let r = pool.pop().ok_or(EmitError::UnsupportedOp(0xF1))?;
    reg.insert(v, r);
    Ok(r)
}

/// Load one operand of a float op into `xmm`. A fixnum constant is coerced to
/// single-float at compile time (float contagion — `(* 2.5 5)` = `12.5`) and
/// materialised; a variable is guarded to be a single-float, then its f32 bits
/// (high dword of the tagged value) are unpacked into the XMM register.
fn float_operand_to_xmm(
    a: &mut Asm,
    xmm: u8,
    v: crate::t2::ir::Value,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    float_consts: &std::collections::HashMap<crate::t2::ir::Value, u32>,
    deopt: bliss_rt::asm::Label,
) -> Result<(), EmitError> {
    if let Some(&bits) = float_consts.get(&v) {
        mov_imm32(a, SCRATCH, bits);
        movd_xmm_r32(a, xmm, SCRATCH);
    } else if let Some(&c) = consts.get(&v) {
        mov_imm32(a, SCRATCH, (c as f32).to_bits());
        movd_xmm_r32(a, xmm, SCRATCH);
    } else {
        let r = *reg.get(&v).ok_or(EmitError::UnsupportedOp(0xF2))?;
        guard_single_float(a, r, deopt);
        shr_imm(a, r, 32); // r's low dword now holds the f32 bits
        movd_xmm_r32(a, xmm, r);
    }
    Ok(())
}

/// A framed T2 compilation with its two entry points (spec: compiled-caller ABI).
///
/// The code has a single body reached by two entries:
/// - **interpreter entry** at offset 0 — `extern "C" fn(*mut u64 slots) -> u64`
///   (the run_native ABI): a short prologue loads arguments from the frame slots
///   into the argument registers, then falls into the body.
/// - **compiled entry** at [`compiled_entry`](Self::compiled_entry) — arguments
///   are already in the argument registers `[rcx, r8, r9, r10]` (arg0..arg3), so
///   a compiled caller skips the frame load entirely. Result in rax.
pub struct FramedCode {
    pub code: Vec<u8>,
    /// Byte offset of the compiled-caller entry within `code`.
    pub compiled_entry: usize,
}

/// Emit `f` (a single-block, speculated straight-line function) as native code.
/// `c2i_deopt_addr` is the address of the interpreter's `c2i_deopt` routine.
pub fn emit_framed(f: &Function, c2i_deopt_addr: u64) -> Result<FramedCode, EmitError> {
    use crate::t2::ir::{AuxData, Opcode, Value};
    use std::collections::HashMap;

    if f.block_order().len() != 1 {
        return Err(EmitError::UnsupportedOp(0xF0)); // only straight-line for now
    }
    let entry = f.entry();

    // rax = return value, rdx = guard scratch, rdi = frame slots pointer. Value
    // registers: r10, r9, r8, rcx (pop→rcx first) — all have REX-free or REX.B
    // low bytes, so the tag guard needs no special-casing.
    let mut pool: Vec<u8> = vec![10, 9, 8, 1];
    let mut reg: HashMap<Value, u8> = HashMap::new();
    // Deferred fixnum constants: folded into imul immediates, materialised only
    // if used somewhere that needs a register (e.g. returned directly).
    let mut consts: HashMap<Value, i64> = HashMap::new();
    // Deferred single-float constants (raw f32 bits), materialised into XMM.
    let mut float_consts: HashMap<Value, u32> = HashMap::new();

    // The terminator must be a Return; bind its value to rax up front so a
    // FixnumMul writing the return value targets rax with no extra move.
    let term = f.terminator(entry).ok_or(EmitError::UnsupportedOp(0xF3))?;
    let term_data = f.inst(term).clone();
    if term_data.opcode != Opcode::Return {
        return Err(EmitError::UnsupportedOp(op_tag(term_data.opcode)));
    }
    let ret_val = term_data.args.first().copied();
    if let Some(rv) = ret_val {
        reg.insert(rv, 0); // rax
    }

    let mut a = Asm::new();
    let deopt = a.label();

    // Interpreter entry (offset 0): load params from the frame slots [rdi + 8*i]
    // into the argument registers, then fall through into the body. Arg register i
    // is exactly framed_alloc's i-th pick ([rcx, r8, r9, r10]), so a compiled
    // caller that places args there can jump straight to `compiled_entry` below.
    let params = f.block(entry).params.clone();
    for (i, &p) in params.iter().enumerate() {
        let r = framed_alloc(&mut reg, &mut pool, p)?;
        mov_from_frame(&mut a, r, 7 /* rdi */, i);
    }
    // Compiled-caller entry: the body, entered with args already in registers.
    // `here()` is stable — the prologue precedes every branch, so branch patching
    // in finish() cannot shift it.
    let compiled_entry = a.here();

    // Body.
    for &inst in &f.block(entry).insts.clone() {
        let data = f.inst(inst).clone();
        match data.opcode {
            Opcode::ConstFixnum => {
                let imm = match data.aux {
                    AuxData::FixnumImm(v) => v,
                    _ => return Err(EmitError::MissingImm),
                };
                consts.insert(data.results[0], imm); // defer; fold where possible
            }
            Opcode::ConstFloat => {
                let fv = match data.aux {
                    AuxData::FloatImm(v) => v,
                    _ => return Err(EmitError::MissingImm),
                };
                float_consts.insert(data.results[0], fv.to_bits());
            }
            Opcode::FixnumMul => {
                let (a0, b0) = (data.args[0], data.args[1]);
                let dst = framed_alloc(&mut reg, &mut pool, data.results[0])?;
                // One operand a constant → guard only the other, fold the constant
                // into the multiply immediate (tagged·raw = tagged product).
                let folded = if let Some(&c) = consts.get(&b0) {
                    Some((a0, c))
                } else if let Some(&c) = consts.get(&a0) {
                    Some((b0, c))
                } else {
                    None
                };
                if let Some((var, c)) = folded {
                    if let Ok(c32) = i32::try_from(c) {
                        let range = f.value(var).ty.range;
                        let x = framed_mat(&mut a, &mut reg, &mut pool, &consts, var)?;
                        // The type guard always stays — we speculate that the
                        // operand is a fixnum. The *overflow* check is what range
                        // analysis lets us drop: when the product provably stays in
                        // fixnum range, strength-reduce to `lea` (no flags, no jo);
                        // otherwise the overflow-detecting `imul + jo` is required.
                        guard_fixnum(&mut a, x, deopt);
                        if mul_cannot_overflow(range, c) {
                            match lea_scale(c) {
                                Some(scale) => lea_mul(&mut a, dst, x, scale),
                                None => imul_imm(&mut a, dst, x, c32), // safe: no jo
                            }
                        } else {
                            imul_imm(&mut a, dst, x, c32);
                            a.jcc(Cc::O, deopt);
                        }
                        continue;
                    }
                    // constant too wide for imm32 → fall through to the general path
                }
                // General two-register multiply: guard both, untag one, multiply.
                let x = framed_mat(&mut a, &mut reg, &mut pool, &consts, a0)?;
                let y = framed_mat(&mut a, &mut reg, &mut pool, &consts, b0)?;
                alu_rr(&mut a, 0x89, SCRATCH, x); // mov rdx, x
                alu_rr(&mut a, 0x09, SCRATCH, y); // or  rdx, y
                a.extend_from_slice(&[0xF6, 0xC2, 0x07]); // test dl, 7
                a.jcc(Cc::Ne, deopt);
                mov_rr(&mut a, dst, x);
                sar_imm(&mut a, dst, 3);
                imul_rr(&mut a, dst, y);
                a.jcc(Cc::O, deopt);
            }
            Opcode::FloatMul | Opcode::FloatAdd | Opcode::FloatSub => {
                // The float second specialization: unpack both operands into XMM
                // (constants coerced by contagion, variables guarded single-float),
                // compute with a scalar-single SSE op, and repack the tagged result
                // into rax. No overflow trap — IEEE float has none.
                let dst = framed_alloc(&mut reg, &mut pool, data.results[0])?;
                float_operand_to_xmm(&mut a, 0, data.args[0], &reg, &consts, &float_consts, deopt)?;
                float_operand_to_xmm(&mut a, 1, data.args[1], &reg, &consts, &float_consts, deopt)?;
                let sub = match data.opcode {
                    Opcode::FloatAdd => 0x58,
                    Opcode::FloatMul => 0x59,
                    Opcode::FloatSub => 0x5C,
                    _ => unreachable!(),
                };
                ss_op(&mut a, sub, 0, 1); // xmm0 = xmm0 <op> xmm1
                movd_r32_xmm(&mut a, 0, 0); // eax = f32 bits of xmm0 (zero-extended)
                shl_imm(&mut a, 0, 32); // rax = bits << 32
                or_imm8(&mut a, 0, 4); // tag single-float (0b100)
                if dst != 0 {
                    mov_rr(&mut a, dst, 0); // if the result isn't the return value
                }
            }
            other if other.is_terminator() => break, // handled below
            other => return Err(EmitError::UnsupportedOp(op_tag(other))),
        }
    }

    // Return: make sure the value is in rax — a bare constant is materialised
    // there directly, otherwise it is already in rax (pre-assigned) or moved in.
    if let Some(rv) = ret_val {
        if let Some(&c) = consts.get(&rv) {
            mov_imm64(&mut a, 0, bliss_rt::value::BlissVal::from_fixnum(c).0 as i64);
        } else {
            let r = framed_mat(&mut a, &mut reg, &mut pool, &consts, rv)?;
            if r != 0 {
                mov_rr(&mut a, 0, r); // mov rax, r
            }
        }
    }
    a.push(0xC3); // ret — no frame to tear down

    // Deopt stub: align rsp (entry rsp%16==8; the fast path pushed nothing), call
    // c2i_deopt, unwind the alignment, return (value ignored on deopt).
    a.bind(deopt);
    a.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8
    mov_imm64(&mut a, 0, c2i_deopt_addr as i64); // mov rax, c2i_deopt
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    a.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
    a.push(0xC3); // ret

    let code = a.finish().ok_or(EmitError::BadBranch)?;
    Ok(FramedCode { code, compiled_entry })
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
        let code = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit_framed").code;
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

    /// Build post-speculation IR for `(x) -> x * c` where the param `x` carries
    /// the given inferred range — i.e. what inlining/the caller ABI would supply.
    fn build_mul_ranged(c: i64, range: Option<crate::t2::ir::Range>) -> crate::t2::ir::Function {
        use crate::t2::ir::{AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation};
        let mut f = Function::new("m");
        let entry = f.entry();
        let xty = IRType { bits: TypeBits::FIXNUM, range, class_id: None };
        let x = f.add_block_param(entry, xty, ValueRepresentation::Tagged);
        let (_ci, cres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(c),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let (_mi, mres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::FixnumMul,
                args: vec![x, cres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags { guard: true, effectful: true, ..Default::default() },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![mres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// Range-proven-safe `* 5` strength-reduces to a single `lea` with no overflow
    /// deopt — matching SBCL's optimised form — and still executes to x*5.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn strength_reduces_to_lea_when_range_is_safe() {
        use bliss_rt::value::BlissVal;
        let f = build_mul_ranged(5, Some(crate::t2::ir::Range { lo: 0, hi: 100 }));
        let code = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit").code;
        // `lea rax,[rcx+rcx*4]` = 48 8D 04 89 ; and NO overflow branch (0F 80).
        assert!(contains(&code, &[0x48, 0x8D, 0x04, 0x89]), "must emit lea for a range-safe *5");
        assert!(!contains(&code, &[0x0F, 0x80]), "range proves no overflow → no jo deopt");
        let buf = bliss_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let mut frame = [BlissVal::from_fixnum(7).0, 0u64, 0u64];
        assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 35, "lea *5 of 7 = 35");
    }

    /// Without a range, the overflow-detecting `imul + jo` is kept (correctness
    /// over speed — an unbounded fixnum could overflow to a bignum).
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn keeps_imul_and_overflow_check_when_range_unknown() {
        use bliss_rt::value::BlissVal;
        let f = build_mul_ranged(5, None);
        let code = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit").code;
        assert!(contains(&code, &[0x48, 0x69, 0xC1]), "unknown range → imul rax,rcx,5");
        assert!(contains(&code, &[0x0F, 0x80]), "unknown range → keep the jo overflow deopt");
        let buf = bliss_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let mut frame = [BlissVal::from_fixnum(7).0, 0u64, 0u64];
        assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 35, "imul *5 of 7 = 35");
    }

    /// Compiled-caller ABI: a compiled caller places args in registers and calls
    /// the `compiled_entry`, skipping the frame load. Here inline asm invokes it
    /// directly — arg0 in rcx, result in rax — proving the register entry runs the
    /// body with no interpreter frame (the memory load `mov rcx,[rdi]` is gone).
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn compiled_entry_takes_register_args() {
        use bliss_rt::value::BlissVal;
        let f = build_mul_ranged(5, Some(crate::t2::ir::Range { lo: 0, hi: 100 }));
        let framed = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit");
        assert!(framed.compiled_entry > 0, "a register entry must sit past the frame-load prologue");
        let buf = bliss_rt::jit::JitBuffer::new(&framed.code).unwrap();
        let entry = buf.as_ptr() as usize + framed.compiled_entry;
        let x = BlissVal::from_fixnum(7).0;
        let result: u64;
        // SAFETY: the compiled entry is a leaf reading arg0 from rcx and writing
        // rax; every caller-saved GPR it may touch is marked clobbered.
        unsafe {
            core::arch::asm!(
                "call {e}",
                e = in(reg) entry,
                inout("rcx") x => _,
                lateout("rax") result,
                lateout("rdx") _,
                lateout("rsi") _,
                lateout("rdi") _,
                lateout("r8") _,
                lateout("r9") _,
                lateout("r10") _,
                lateout("r11") _,
            );
        }
        assert_eq!(BlissVal(result).as_fixnum(), 35, "compiled entry: 7*5 = 35 from a register arg");
    }

    /// Build `(x) -> x <op> c` as a float-speculated op: single-float param `x`,
    /// a fixnum constant `c` (coerced by contagion), guard-flagged float op.
    fn build_float_arith(op: crate::t2::ir::Opcode, c: i64) -> crate::t2::ir::Function {
        use crate::t2::ir::{AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation};
        let mut f = Function::new("m");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::of(TypeBits::SINGLE_FLOAT), ValueRepresentation::Tagged);
        let (_c, cres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(c),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let (_m, mres) = f.push_inst(
            entry,
            InstData {
                opcode: op,
                args: vec![x, cres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags { guard: true, effectful: true, ..Default::default() },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::SINGLE_FLOAT), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![mres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    /// The float second specialization: a float-speculated `(* x 5)` runs the XMM
    /// path — a single-float arg gives `x*5.0` (the fixnum constant coerced), and a
    /// fixnum arg trips the single-float guard and deopts.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn float_mul_runs_and_deopts() {
        use bliss_rt::value::BlissVal;
        let f = build_float_arith(crate::t2::ir::Opcode::FloatMul, 5);
        let code = emit_framed(&f, mock_c2i_deopt as usize as u64).expect("emit").code;
        let buf = bliss_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

        let mut frame = [BlissVal::from_single_float(2.0).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let r = func(frame.as_mut_ptr());
        assert!(!DEOPTED.load(Ordering::SeqCst), "a single-float arg must not deopt");
        assert_eq!(BlissVal(r).as_single_float(), 10.0, "2.0 * 5 = 10.0 (fixnum coerced)");

        let mut frame = [BlissVal::from_fixnum(2).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let _ = func(frame.as_mut_ptr());
        assert!(DEOPTED.load(Ordering::SeqCst), "a fixnum operand must deopt (guard is single-float)");
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
