//! AArch64 instruction encoding.
//!
//! `asm.rs` owns labels and branch fixups; the instructions themselves are the
//! caller's business. On x86-64 the callers spell them as raw byte slices with a
//! mnemonic in a comment, which works because an x86 instruction is a byte
//! string anyway. A64 instructions are fixed 32-bit words whose operands are
//! scattered across bitfields, so hand-written literals would be unreadable and
//! unreviewable. These are the encoders T1 and T2 emit through instead.
//!
//! Every function returns the instruction word (or `None` when the operand does
//! not fit the field, which is the emitter's cue to synthesise a longer form).
//! All operations are 64-bit unless the name says otherwise: TorCL values are
//! word-sized, so the 32-bit forms are not needed yet.
//!
//! Encodings are per the Arm ARM (DDI 0487, C4.1 "A64 instruction set
//! encoding"), and every one below is pinned by a test against a known-good
//! assembler encoding.

use super::Cc;

/// A general-purpose register number. 31 is the zero register in most
/// instructions and the stack pointer in the load/store and add/sub-immediate
/// forms — the instruction decides, not the encoding.
pub type Reg = u8;

/// A SIMD/FP register number, used here only as a double.
pub type Fpr = u8;

pub const XZR: Reg = 31;
pub const SP: Reg = 31;
/// The link register: `BL`/`BLR` write it and `RET` reads it.
pub const LR: Reg = 30;
/// The frame pointer.
pub const FP: Reg = 29;

// ---------------------------------------------------------------------------
// Moves

/// `MOV Xd, Xm` — architecturally `ORR Xd, XZR, Xm`.
pub fn mov(d: Reg, m: Reg) -> u32 {
    orr(d, XZR, m)
}

/// `MOVZ Xd, #imm16, LSL #(16*hw)`.
pub fn movz(d: Reg, imm16: u16, hw: u32) -> u32 {
    debug_assert!(hw < 4);
    0xD280_0000 | (hw << 21) | ((imm16 as u32) << 5) | d as u32
}

/// `MOVK Xd, #imm16, LSL #(16*hw)` — insert, leaving the other halves alone.
pub fn movk(d: Reg, imm16: u16, hw: u32) -> u32 {
    debug_assert!(hw < 4);
    0xF280_0000 | (hw << 21) | ((imm16 as u32) << 5) | d as u32
}

/// `MOVN Xd, #imm16, LSL #(16*hw)` — sets `Xd` to the *inverse*, so every other
/// half becomes `0xffff`.
pub fn movn(d: Reg, imm16: u16, hw: u32) -> u32 {
    debug_assert!(hw < 4);
    0x9280_0000 | (hw << 21) | ((imm16 as u32) << 5) | d as u32
}

/// Materialise an arbitrary 64-bit constant into `d`, appending 1–4 words.
///
/// Starts from whichever of `MOVZ` (other halves zero) or `MOVN` (other halves
/// ones) leaves fewer halves to fix up, then `MOVK`s the rest. A tagged TorCL
/// immediate is usually two words this way and a small fixnum one.
pub fn mov_imm64(d: Reg, value: u64, out: &mut Vec<u32>) {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let ones = halves.iter().filter(|h| **h == 0xffff).count();
    let zeros = halves.iter().filter(|h| **h == 0).count();
    // A constant of all-zero or all-one halves needs only the leading move, and
    // `position` finding nothing is exactly that case.
    if ones > zeros {
        let first = halves.iter().position(|h| *h != 0xffff).unwrap_or(0);
        out.push(movn(d, !halves[first], first as u32));
        for (hw, half) in halves.iter().enumerate() {
            if hw != first && *half != 0xffff {
                out.push(movk(d, *half, hw as u32));
            }
        }
    } else {
        let first = halves.iter().position(|h| *h != 0).unwrap_or(0);
        out.push(movz(d, halves[first], first as u32));
        for (hw, half) in halves.iter().enumerate() {
            if hw != first && *half != 0 {
                out.push(movk(d, *half, hw as u32));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logical, register operand

/// The shift applied to the second register operand of a logical or add/sub.
#[derive(Clone, Copy)]
pub enum Shift {
    Lsl(u32),
    Lsr(u32),
    Asr(u32),
}

impl Shift {
    const NONE: Shift = Shift::Lsl(0);

    fn bits(self) -> u32 {
        let (kind, amount) = match self {
            Shift::Lsl(n) => (0, n),
            Shift::Lsr(n) => (1, n),
            Shift::Asr(n) => (2, n),
        };
        debug_assert!(amount < 64);
        (kind << 22) | (amount << 10)
    }
}

fn logical_reg(base: u32, d: Reg, n: Reg, m: Reg, shift: Shift) -> u32 {
    base | shift.bits() | ((m as u32) << 16) | ((n as u32) << 5) | d as u32
}

/// `AND Xd, Xn, Xm`.
pub fn and(d: Reg, n: Reg, m: Reg) -> u32 {
    logical_reg(0x8A00_0000, d, n, m, Shift::NONE)
}

/// `AND Xd, Xn, Xm, <shift>` — the shift is free, so tag extraction is one
/// instruction where x86 needs a separate shift.
pub fn and_shifted(d: Reg, n: Reg, m: Reg, shift: Shift) -> u32 {
    logical_reg(0x8A00_0000, d, n, m, shift)
}

/// `ORR Xd, Xn, Xm`.
pub fn orr(d: Reg, n: Reg, m: Reg) -> u32 {
    logical_reg(0xAA00_0000, d, n, m, Shift::NONE)
}

/// `EOR Xd, Xn, Xm`.
pub fn eor(d: Reg, n: Reg, m: Reg) -> u32 {
    logical_reg(0xCA00_0000, d, n, m, Shift::NONE)
}

/// `ANDS Xd, Xn, Xm` — AND, setting the flags.
pub fn ands(d: Reg, n: Reg, m: Reg) -> u32 {
    logical_reg(0xEA00_0000, d, n, m, Shift::NONE)
}

/// `TST Xn, Xm` — `ANDS` discarding the result, i.e. x86's `test`.
pub fn tst(n: Reg, m: Reg) -> u32 {
    ands(XZR, n, m)
}

/// `MVN Xd, Xm` — `ORN Xd, XZR, Xm`, i.e. x86's `not`.
pub fn mvn(d: Reg, m: Reg) -> u32 {
    logical_reg(0xAA20_0000, d, XZR, m, Shift::NONE)
}

// ---------------------------------------------------------------------------
// Logical, immediate operand

/// Encode `value` as an A64 logical immediate: `(N, immr, imms)`.
///
/// The field holds a *rotated run of ones replicated to fill the register*, so
/// masks like `0xff` or `0xfffffffffffffff8` are free but arbitrary constants
/// are not (`0xc3` is not encodable — as two runs it is only a valid pattern at
/// element size 8, and replicating it there would give `0xc3c3…c3`, not `0xc3`).
/// `None` means the caller must materialise the constant into a register.
pub fn logical_imm(value: u64) -> Option<(u32, u32, u32)> {
    // All-zeros and all-ones are unencodable: the field cannot express a run of
    // zero ones, and a full run would need `imms` one larger than it holds.
    if value == 0 || value == u64::MAX {
        return None;
    }
    for log_esize in 1..=6u32 {
        let esize = 1u32 << log_esize;
        let element = value & mask(esize);
        // Every element must be identical for the replication to reproduce it.
        if (0..64 / esize).any(|i| (value >> (i * esize)) & mask(esize) != element) {
            continue;
        }
        // `element` must be `Ones(s + 1)` rotated right by `r`; equivalently,
        // rotating it left by `r` yields a mask anchored at bit 0.
        for r in 0..esize {
            let rotated = rotate_left(element, r, esize);
            if (rotated + 1) & rotated == 0 {
                let s = rotated.count_ones() - 1;
                let n = u32::from(esize == 64);
                let imms = ((!(esize - 1)) << 1) & 0x3f | s;
                return Some((n, r, imms));
            }
        }
    }
    None
}

fn mask(esize: u32) -> u64 {
    if esize == 64 {
        u64::MAX
    } else {
        (1u64 << esize) - 1
    }
}

fn rotate_left(element: u64, amount: u32, esize: u32) -> u64 {
    if amount == 0 {
        element
    } else {
        ((element << amount) | (element >> (esize - amount))) & mask(esize)
    }
}

fn logical_immediate(base: u32, d: Reg, n: Reg, value: u64) -> Option<u32> {
    let (bit_n, immr, imms) = logical_imm(value)?;
    Some(base | (bit_n << 22) | (immr << 16) | (imms << 10) | ((n as u32) << 5) | d as u32)
}

/// `AND Xd, Xn, #value`, or `None` if `value` is not a logical immediate.
pub fn and_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    logical_immediate(0x9200_0000, d, n, value)
}

/// `ORR Xd, Xn, #value`.
pub fn orr_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    logical_immediate(0xB200_0000, d, n, value)
}

/// `EOR Xd, Xn, #value`.
pub fn eor_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    logical_immediate(0xD200_0000, d, n, value)
}

/// `TST Xn, #value` — the tag check, and the reason this encoder exists.
pub fn tst_imm(n: Reg, value: u64) -> Option<u32> {
    logical_immediate(0xF200_0000, XZR, n, value)
}

/// `UXTB Xd, Wn` — zero-extend the low byte, as a mask.
pub fn uxtb(d: Reg, n: Reg) -> u32 {
    and_imm(d, n, 0xff).expect("0xff is a logical immediate")
}

/// `UXTH Xd, Wn` — zero-extend the low halfword.
pub fn uxth(d: Reg, n: Reg) -> u32 {
    and_imm(d, n, 0xffff).expect("0xffff is a logical immediate")
}

// ---------------------------------------------------------------------------
// Add and subtract

fn addsub_reg(base: u32, d: Reg, n: Reg, m: Reg, shift: Shift) -> u32 {
    base | shift.bits() | ((m as u32) << 16) | ((n as u32) << 5) | d as u32
}

/// `ADD Xd, Xn, Xm`.
pub fn add(d: Reg, n: Reg, m: Reg) -> u32 {
    addsub_reg(0x8B00_0000, d, n, m, Shift::NONE)
}

/// `ADD Xd, Xn, Xm, <shift>` — the addressing-arithmetic workhorse.
pub fn add_shifted(d: Reg, n: Reg, m: Reg, shift: Shift) -> u32 {
    addsub_reg(0x8B00_0000, d, n, m, shift)
}

/// `ADDS Xd, Xn, Xm` — add, setting the flags. Paired with a `Cc::O` branch this
/// is the fixnum overflow check.
pub fn adds(d: Reg, n: Reg, m: Reg) -> u32 {
    addsub_reg(0xAB00_0000, d, n, m, Shift::NONE)
}

/// `SUB Xd, Xn, Xm`.
pub fn sub(d: Reg, n: Reg, m: Reg) -> u32 {
    addsub_reg(0xCB00_0000, d, n, m, Shift::NONE)
}

/// `SUBS Xd, Xn, Xm` — subtract, setting the flags.
pub fn subs(d: Reg, n: Reg, m: Reg) -> u32 {
    addsub_reg(0xEB00_0000, d, n, m, Shift::NONE)
}

/// `CMP Xn, Xm` — `SUBS` discarding the result.
pub fn cmp(n: Reg, m: Reg) -> u32 {
    subs(XZR, n, m)
}

/// `NEG Xd, Xm` — `SUB Xd, XZR, Xm`.
pub fn neg(d: Reg, m: Reg) -> u32 {
    sub(d, XZR, m)
}

/// The add/sub immediate field is 12 bits, optionally shifted left by 12 — so
/// either a small offset or a multiple of 4096, and nothing in between.
fn addsub_imm(base: u32, d: Reg, n: Reg, value: u64) -> Option<u32> {
    let (shifted, imm12) = if value < 0x1000 {
        (0, value as u32)
    } else if value < 0x100_0000 && value.trailing_zeros() >= 12 {
        (1, (value >> 12) as u32)
    } else {
        return None;
    };
    Some(base | (shifted << 22) | (imm12 << 10) | ((n as u32) << 5) | d as u32)
}

/// `ADD Xd, Xn, #value`, or `None` if the immediate does not fit.
pub fn add_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    addsub_imm(0x9100_0000, d, n, value)
}

/// `SUB Xd, Xn, #value`.
pub fn sub_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    addsub_imm(0xD100_0000, d, n, value)
}

/// `SUBS Xd, Xn, #value`.
pub fn subs_imm(d: Reg, n: Reg, value: u64) -> Option<u32> {
    addsub_imm(0xF100_0000, d, n, value)
}

/// `CMP Xn, #value`.
pub fn cmp_imm(n: Reg, value: u64) -> Option<u32> {
    subs_imm(XZR, n, value)
}

// ---------------------------------------------------------------------------
// Multiply

/// `MADD Xd, Xn, Xm, Xa` — `Xd = Xa + Xn * Xm`.
pub fn madd(d: Reg, n: Reg, m: Reg, a: Reg) -> u32 {
    0x9B00_0000 | ((m as u32) << 16) | ((a as u32) << 10) | ((n as u32) << 5) | d as u32
}

/// `MUL Xd, Xn, Xm` — `MADD` accumulating into the zero register.
pub fn mul(d: Reg, n: Reg, m: Reg) -> u32 {
    madd(d, n, m, XZR)
}

// ---------------------------------------------------------------------------
// Loads and stores

fn load_store_scaled(base: u32, t: Reg, n: Reg, offset: u64) -> Option<u32> {
    // The unsigned-offset form scales by the access size, so only multiples of
    // eight are reachable — an unaligned or negative offset needs the unscaled
    // form instead.
    if offset % 8 != 0 || offset / 8 >= 0x1000 {
        return None;
    }
    Some(base | (((offset / 8) as u32) << 10) | ((n as u32) << 5) | t as u32)
}

/// `STR Xt, [Xn, #offset]` — scaled unsigned offset, so `None` unless `offset`
/// is a non-negative multiple of 8 below 32 KiB.
pub fn str_imm(t: Reg, n: Reg, offset: u64) -> Option<u32> {
    load_store_scaled(0xF900_0000, t, n, offset)
}

/// `LDR Xt, [Xn, #offset]`.
pub fn ldr_imm(t: Reg, n: Reg, offset: u64) -> Option<u32> {
    load_store_scaled(0xF940_0000, t, n, offset)
}

fn load_store_unscaled(base: u32, t: Reg, n: Reg, offset: i32) -> Option<u32> {
    if !(-256..256).contains(&offset) {
        return None;
    }
    Some(base | (((offset as u32) & 0x1ff) << 12) | ((n as u32) << 5) | t as u32)
}

/// `STUR Xt, [Xn, #offset]` — unscaled, signed 9-bit offset. This is the form
/// for a negative displacement, e.g. reaching below a frame pointer.
pub fn stur(t: Reg, n: Reg, offset: i32) -> Option<u32> {
    load_store_unscaled(0xF800_0000, t, n, offset)
}

/// `LDUR Xt, [Xn, #offset]`.
pub fn ldur(t: Reg, n: Reg, offset: i32) -> Option<u32> {
    load_store_unscaled(0xF840_0000, t, n, offset)
}

/// `STR Xt, [Xn, #offset]!` — pre-index: adjust the base, then store. With
/// `Xn = SP` this is x86's `push`.
pub fn str_pre(t: Reg, n: Reg, offset: i32) -> Option<u32> {
    load_store_unscaled(0xF800_0C00, t, n, offset)
}

/// `LDR Xt, [Xn], #offset` — post-index: load, then adjust the base. With
/// `Xn = SP` this is x86's `pop`.
pub fn ldr_post(t: Reg, n: Reg, offset: i32) -> Option<u32> {
    load_store_unscaled(0xF840_0400, t, n, offset)
}

/// `LDR Xt, [Xn, Xm, LSL #3]` — indexed by a word-scaled register, the shape of
/// a stack-slot or vector element access.
pub fn ldr_indexed(t: Reg, n: Reg, m: Reg) -> u32 {
    0xF860_7800 | ((m as u32) << 16) | ((n as u32) << 5) | t as u32
}

fn pair(base: u32, t1: Reg, t2: Reg, n: Reg, offset: i32) -> Option<u32> {
    // imm7 counts eightbytes, signed.
    if offset % 8 != 0 {
        return None;
    }
    let scaled = offset / 8;
    if !(-64..64).contains(&scaled) {
        return None;
    }
    Some(
        base | (((scaled as u32) & 0x7f) << 15)
            | ((t2 as u32) << 10)
            | ((n as u32) << 5)
            | t1 as u32,
    )
}

/// `STP Xt1, Xt2, [Xn, #offset]!` — pre-index. The frame prologue.
pub fn stp_pre(t1: Reg, t2: Reg, n: Reg, offset: i32) -> Option<u32> {
    pair(0xA980_0000, t1, t2, n, offset)
}

/// `LDP Xt1, Xt2, [Xn], #offset` — post-index. The frame epilogue.
pub fn ldp_post(t1: Reg, t2: Reg, n: Reg, offset: i32) -> Option<u32> {
    pair(0xA8C0_0000, t1, t2, n, offset)
}

/// `STP Xt1, Xt2, [Xn, #offset]` — signed offset, base unchanged.
pub fn stp(t1: Reg, t2: Reg, n: Reg, offset: i32) -> Option<u32> {
    pair(0xA900_0000, t1, t2, n, offset)
}

/// `LDP Xt1, Xt2, [Xn, #offset]`.
pub fn ldp(t1: Reg, t2: Reg, n: Reg, offset: i32) -> Option<u32> {
    pair(0xA940_0000, t1, t2, n, offset)
}

// ---------------------------------------------------------------------------
// Bitfield moves and shifts
//
// A64 has no shift-by-immediate instruction: `LSL`/`LSR`/`ASR` by a constant are
// aliases of the bitfield moves, and the assembler does this arithmetic.

fn bfm(base: u32, d: Reg, n: Reg, immr: u32, imms: u32) -> u32 {
    base | (immr << 16) | (imms << 10) | ((n as u32) << 5) | d as u32
}

/// `LSL Xd, Xn, #amount` — `UBFM Xd, Xn, #(-amount mod 64), #(63 - amount)`.
pub fn lsl_imm(d: Reg, n: Reg, amount: u32) -> u32 {
    debug_assert!(amount < 64);
    bfm(0xD340_0000, d, n, (64 - amount) & 63, 63 - amount)
}

/// `LSR Xd, Xn, #amount`.
pub fn lsr_imm(d: Reg, n: Reg, amount: u32) -> u32 {
    debug_assert!(amount < 64);
    bfm(0xD340_0000, d, n, amount, 63)
}

/// `ASR Xd, Xn, #amount` — the arithmetic shift that untags a fixnum.
pub fn asr_imm(d: Reg, n: Reg, amount: u32) -> u32 {
    debug_assert!(amount < 64);
    bfm(0x9340_0000, d, n, amount, 63)
}

/// `SXTB Xd, Wn` — sign-extend the low byte.
pub fn sxtb(d: Reg, n: Reg) -> u32 {
    bfm(0x9340_0000, d, n, 0, 7)
}

/// `SXTH Xd, Wn` — sign-extend the low halfword.
pub fn sxth(d: Reg, n: Reg) -> u32 {
    bfm(0x9340_0000, d, n, 0, 15)
}

/// `SXTW Xd, Wn` — sign-extend the low word.
pub fn sxtw(d: Reg, n: Reg) -> u32 {
    bfm(0x9340_0000, d, n, 0, 31)
}

fn shift_reg(base: u32, d: Reg, n: Reg, m: Reg) -> u32 {
    base | ((m as u32) << 16) | ((n as u32) << 5) | d as u32
}

/// `LSL Xd, Xn, Xm` — shift by a register. Unlike x86 this needs no fixed
/// register and does not disturb the flags.
pub fn lsl_reg(d: Reg, n: Reg, m: Reg) -> u32 {
    shift_reg(0x9AC0_2000, d, n, m)
}

/// `LSR Xd, Xn, Xm`.
pub fn lsr_reg(d: Reg, n: Reg, m: Reg) -> u32 {
    shift_reg(0x9AC0_2400, d, n, m)
}

/// `ASR Xd, Xn, Xm`.
pub fn asr_reg(d: Reg, n: Reg, m: Reg) -> u32 {
    shift_reg(0x9AC0_2800, d, n, m)
}

// ---------------------------------------------------------------------------
// Conditional selection

/// `CSEL Xd, Xn, Xm, <cc>` — `Xd = if cc { Xn } else { Xm }`, x86's `cmovcc`
/// without the read-modify-write.
pub fn csel(d: Reg, n: Reg, m: Reg, cc: Cc) -> u32 {
    0x9A80_0000 | ((m as u32) << 16) | (cc.a64_cond() << 12) | ((n as u32) << 5) | d as u32
}

/// `CSET Xd, <cc>` — materialise the condition as 0 or 1, x86's `setcc`.
/// Architecturally `CSINC Xd, XZR, XZR, <inverse cc>`.
pub fn cset(d: Reg, cc: Cc) -> u32 {
    0x9A80_0400
        | ((XZR as u32) << 16)
        | (cc.inverse().a64_cond() << 12)
        | ((XZR as u32) << 5)
        | d as u32
}

// ---------------------------------------------------------------------------
// Branches to a register, and the no-op

/// `BR Xn` — an indirect jump, for a tail call or a jump table.
pub fn br(n: Reg) -> u32 {
    0xD61F_0000 | ((n as u32) << 5)
}

/// `BLR Xn` — an indirect call.
pub fn blr(n: Reg) -> u32 {
    0xD63F_0000 | ((n as u32) << 5)
}

/// `RET` — return to the link register.
pub fn ret() -> u32 {
    0xD65F_0000 | ((LR as u32) << 5)
}

/// `NOP`. Padding must still be instruction-shaped: a zero word is `UDF`.
pub fn nop() -> u32 {
    0xD503_201F
}

// ---------------------------------------------------------------------------
// Double-precision floating point

/// `FMOV Xd, Dn` — move the bit pattern out of an FP register.
pub fn fmov_to_gpr(d: Reg, n: Fpr) -> u32 {
    0x9E66_0000 | ((n as u32) << 5) | d as u32
}

/// `FMOV Dd, Xn` — move a bit pattern in. Unboxing a double is this, not a load.
pub fn fmov_from_gpr(d: Fpr, n: Reg) -> u32 {
    0x9E67_0000 | ((n as u32) << 5) | d as u32
}

/// `FMOV Dd, Dn`.
pub fn fmov(d: Fpr, n: Fpr) -> u32 {
    0x1E60_4000 | ((n as u32) << 5) | d as u32
}

fn float_arith(base: u32, d: Fpr, n: Fpr, m: Fpr) -> u32 {
    base | ((m as u32) << 16) | ((n as u32) << 5) | d as u32
}

/// `FADD Dd, Dn, Dm`.
pub fn fadd(d: Fpr, n: Fpr, m: Fpr) -> u32 {
    float_arith(0x1E60_2800, d, n, m)
}

/// `FSUB Dd, Dn, Dm`.
pub fn fsub(d: Fpr, n: Fpr, m: Fpr) -> u32 {
    float_arith(0x1E60_3800, d, n, m)
}

/// `FMUL Dd, Dn, Dm`.
pub fn fmul(d: Fpr, n: Fpr, m: Fpr) -> u32 {
    float_arith(0x1E60_0800, d, n, m)
}

/// `FDIV Dd, Dn, Dm`.
pub fn fdiv(d: Fpr, n: Fpr, m: Fpr) -> u32 {
    float_arith(0x1E60_1800, d, n, m)
}

/// `FNEG Dd, Dn`.
pub fn fneg(d: Fpr, n: Fpr) -> u32 {
    0x1E61_4000 | ((n as u32) << 5) | d as u32
}

/// `FSQRT Dd, Dn`.
pub fn fsqrt(d: Fpr, n: Fpr) -> u32 {
    0x1E61_C000 | ((n as u32) << 5) | d as u32
}

/// `FCMP Dn, Dm` — sets the flags, so `Cc` branches follow. Note that an
/// unordered compare sets C and V, so the signed conditions do not mean what
/// they mean after an integer compare.
pub fn fcmp(n: Fpr, m: Fpr) -> u32 {
    0x1E60_2000 | ((m as u32) << 16) | ((n as u32) << 5)
}

/// `SCVTF Dd, Xn` — signed integer to double.
pub fn scvtf(d: Fpr, n: Reg) -> u32 {
    0x9E62_0000 | ((n as u32) << 5) | d as u32
}

/// `FCVTZS Xd, Dn` — double to signed integer, truncating toward zero.
pub fn fcvtzs(d: Reg, n: Fpr) -> u32 {
    0x9E78_0000 | ((n as u32) << 5) | d as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expected word here was produced by a known-good assembler
    /// (`llvm-mc -triple=aarch64 -show-encoding`) from the mnemonic in the
    /// label, and read back little-endian. A failure means this encoder drifted
    /// from the architecture, not that the test needs updating.
    fn check(cases: &[(&str, u32, u32)]) {
        let wrong: Vec<_> = cases
            .iter()
            .filter(|(_, expected, got)| expected != got)
            .map(|(asm, expected, got)| {
                format!("  {asm}: expected {expected:#010x}, got {got:#010x}")
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "encodings disagree:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn moves_match_the_assembler() {
        check(&[
            ("mov x3, x7", 0xAA0703E3, mov(3, 7)),
            ("movz x5, #0x1234, lsl #16", 0xD2A24685, movz(5, 0x1234, 1)),
            ("movk x5, #0xbeef", 0xF297DDE5, movk(5, 0xbeef, 0)),
            ("movn x9, #0", 0x92800009, movn(9, 0, 0)),
        ]);
    }

    #[test]
    fn logical_and_arithmetic_registers_match_the_assembler() {
        check(&[
            ("and x1, x2, x3", 0x8A030041, and(1, 2, 3)),
            ("orr x1, x2, x3", 0xAA030041, orr(1, 2, 3)),
            ("eor x1, x2, x3", 0xCA030041, eor(1, 2, 3)),
            ("ands x1, x2, x3", 0xEA030041, ands(1, 2, 3)),
            ("tst x1, x2", 0xEA02003F, tst(1, 2)),
            ("mvn x4, x6", 0xAA2603E4, mvn(4, 6)),
            (
                "and x1, x2, x3, lsl #3",
                0x8A030C41,
                and_shifted(1, 2, 3, Shift::Lsl(3)),
            ),
            ("add x1, x2, x3", 0x8B030041, add(1, 2, 3)),
            ("adds x1, x2, x3", 0xAB030041, adds(1, 2, 3)),
            ("sub x1, x2, x3", 0xCB030041, sub(1, 2, 3)),
            ("subs x1, x2, x3", 0xEB030041, subs(1, 2, 3)),
            ("cmp x2, x3", 0xEB03005F, cmp(2, 3)),
            ("neg x8, x9", 0xCB0903E8, neg(8, 9)),
            (
                "add x1, x2, x3, lsl #2",
                0x8B030841,
                add_shifted(1, 2, 3, Shift::Lsl(2)),
            ),
            ("mul x1, x2, x3", 0x9B037C41, mul(1, 2, 3)),
            ("madd x1, x2, x3, x4", 0x9B031041, madd(1, 2, 3, 4)),
        ]);
    }

    #[test]
    fn immediate_operands_match_the_assembler() {
        check(&[
            ("add x1, x2, #16", 0x91004041, add_imm(1, 2, 16).unwrap()),
            ("sub sp, sp, #32", 0xD10083FF, sub_imm(SP, SP, 32).unwrap()),
            (
                "add x1, x2, #1, lsl #12",
                0x91400441,
                add_imm(1, 2, 4096).unwrap(),
            ),
            ("cmp x1, #5", 0xF100143F, cmp_imm(1, 5).unwrap()),
            (
                "and x10, x11, #0x7",
                0x9240096A,
                and_imm(10, 11, 0x7).unwrap(),
            ),
            (
                "and x10, x11, #0xfffffffffffffff8",
                0x927DF16A,
                and_imm(10, 11, 0xffff_ffff_ffff_fff8).unwrap(),
            ),
            ("and x1, x2, #0xff", 0x92401C41, uxtb(1, 2)),
            ("and x1, x2, #0xffff", 0x92403C41, uxth(1, 2)),
            (
                "and x1, x2, #0x100000001",
                0x92000041,
                and_imm(1, 2, 0x1_0000_0001).unwrap(),
            ),
            (
                "orr x1, x2, #0x100",
                0xB2780041,
                orr_imm(1, 2, 0x100).unwrap(),
            ),
            ("eor x1, x2, #0x1", 0xD2400041, eor_imm(1, 2, 1).unwrap()),
            ("tst x1, #0xf", 0xF2400C3F, tst_imm(1, 0xf).unwrap()),
        ]);
    }

    #[test]
    fn unencodable_immediates_are_refused() {
        // 0xc3 is two runs of ones, so it is a valid pattern only at element
        // size 8 — where replication would give 0xc3c3c3c3c3c3c3c3, not 0xc3.
        // The assembler rejects `and x1, x2, #0xc3` for exactly this reason.
        assert!(and_imm(1, 2, 0xc3).is_none());
        assert!(logical_imm(0).is_none());
        assert!(logical_imm(u64::MAX).is_none());
        // Beyond imm12, and not a multiple of 4096.
        assert!(add_imm(1, 2, 0x1001).is_none());
        assert!(add_imm(1, 2, 4096).is_some());
        // Beyond the shifted imm12 range entirely.
        assert!(add_imm(1, 2, 0x100_0000).is_none());
    }

    /// Every logical immediate this encoder accepts must decode back to the same
    /// value by the architecture's own rule. An encoding that decoded to a
    /// *different* mask would silently corrupt a tag check rather than fail.
    #[test]
    fn accepted_logical_immediates_round_trip() {
        let mut accepted = 0;
        for rotation in 0..64 {
            for width in 1..64u32 {
                let value = ((1u64 << width) - 1).rotate_left(rotation);
                let Some((n, immr, imms)) = logical_imm(value) else {
                    continue;
                };
                accepted += 1;
                assert_eq!(
                    decode_bit_masks(n, immr, imms),
                    Some(value),
                    "{value:#018x} encoded as N={n} immr={immr} imms={imms}"
                );
            }
        }
        // 64 rotations x 63 widths, minus the duplicates a rotation of an
        // all-same-width pattern produces; the point is that it is not a handful.
        assert!(accepted > 1000, "only {accepted} patterns accepted");
    }

    /// `DecodeBitMasks` from the Arm ARM (J1.3, shared pseudocode), for the
    /// immediate (non-`tmask`) case: the element size is the position of the
    /// highest set bit of `N:NOT(imms)`, the run length comes from `imms`, and
    /// `immr` rotates it right before replication.
    fn decode_bit_masks(n: u32, immr: u32, imms: u32) -> Option<u64> {
        let combined = (n << 6) | (!imms & 0x3f);
        if combined == 0 {
            return None;
        }
        let len = 31 - combined.leading_zeros();
        let esize = 1u32 << len;
        let levels = esize - 1;
        let s = imms & levels;
        let r = immr & levels;
        if s == levels {
            return None; // a full run of ones is not encodable
        }
        let element = rotate_right_in((1u64 << (s + 1)) - 1, r, esize);
        Some((0..64 / esize).fold(0u64, |acc, i| acc | (element << (i * esize))))
    }

    fn rotate_right_in(element: u64, amount: u32, esize: u32) -> u64 {
        if amount == 0 {
            element
        } else {
            ((element >> amount) | (element << (esize - amount))) & mask(esize)
        }
    }

    #[test]
    fn loads_and_stores_match_the_assembler() {
        check(&[
            ("str x1, [x2, #24]", 0xF9000C41, str_imm(1, 2, 24).unwrap()),
            ("ldr x1, [x2, #24]", 0xF9400C41, ldr_imm(1, 2, 24).unwrap()),
            ("stur x1, [x2, #-8]", 0xF81F8041, stur(1, 2, -8).unwrap()),
            ("ldur x1, [x2, #-8]", 0xF85F8041, ldur(1, 2, -8).unwrap()),
            (
                "str x1, [sp, #-16]!",
                0xF81F0FE1,
                str_pre(1, SP, -16).unwrap(),
            ),
            (
                "ldr x1, [sp], #16",
                0xF84107E1,
                ldr_post(1, SP, 16).unwrap(),
            ),
            ("ldr x1, [x2, x3, lsl #3]", 0xF8637841, ldr_indexed(1, 2, 3)),
            (
                "stp x29, x30, [sp, #-16]!",
                0xA9BF7BFD,
                stp_pre(FP, LR, SP, -16).unwrap(),
            ),
            (
                "ldp x29, x30, [sp], #16",
                0xA8C17BFD,
                ldp_post(FP, LR, SP, 16).unwrap(),
            ),
            (
                "stp x19, x20, [sp, #16]",
                0xA90153F3,
                stp(19, 20, SP, 16).unwrap(),
            ),
            (
                "ldp x19, x20, [sp, #16]",
                0xA94153F3,
                ldp(19, 20, SP, 16).unwrap(),
            ),
        ]);
    }

    #[test]
    fn out_of_range_displacements_are_refused() {
        // The scaled form reaches only non-negative multiples of eight.
        assert!(str_imm(1, 2, 4).is_none());
        assert!(str_imm(1, 2, 0x8000).is_none());
        assert!(str_imm(1, 2, 0x7ff8).is_some());
        // The unscaled form is a signed 9-bit byte offset.
        assert!(stur(1, 2, -256).is_some());
        assert!(stur(1, 2, -257).is_none());
        assert!(stur(1, 2, 255).is_some());
        // Pairs count eightbytes in a signed 7-bit field.
        assert!(stp_pre(FP, LR, SP, -512).is_some());
        assert!(stp_pre(FP, LR, SP, -520).is_none());
        assert!(stp_pre(FP, LR, SP, -12).is_none());
    }

    #[test]
    fn shifts_and_extensions_match_the_assembler() {
        check(&[
            ("lsl x1, x2, #3", 0xD37DF041, lsl_imm(1, 2, 3)),
            ("lsr x1, x2, #3", 0xD343FC41, lsr_imm(1, 2, 3)),
            ("asr x1, x2, #3", 0x9343FC41, asr_imm(1, 2, 3)),
            ("lsl x1, x2, x3", 0x9AC32041, lsl_reg(1, 2, 3)),
            ("asr x1, x2, x3", 0x9AC32841, asr_reg(1, 2, 3)),
            ("sxtb x1, w2", 0x93401C41, sxtb(1, 2)),
            ("sxth x1, w2", 0x93403C41, sxth(1, 2)),
            ("sxtw x1, w2", 0x93407C41, sxtw(1, 2)),
        ]);
    }

    #[test]
    fn conditionals_and_branches_match_the_assembler() {
        check(&[
            ("csel x1, x2, x3, eq", 0x9A830041, csel(1, 2, 3, Cc::E)),
            ("csel x1, x2, x3, vs", 0x9A836041, csel(1, 2, 3, Cc::O)),
            ("csel x1, x2, x3, le", 0x9A83D041, csel(1, 2, 3, Cc::Le)),
            ("cset x1, ne", 0x9A9F07E1, cset(1, Cc::Ne)),
            ("cset x1, vs", 0x9A9F77E1, cset(1, Cc::O)),
            ("cset x5, lt", 0x9A9FA7E5, cset(5, Cc::L)),
            ("br x16", 0xD61F0200, br(16)),
            ("blr x16", 0xD63F0200, blr(16)),
            ("ret", 0xD65F03C0, ret()),
            ("nop", 0xD503201F, nop()),
        ]);
    }

    #[test]
    fn floating_point_matches_the_assembler() {
        check(&[
            ("fmov x1, d2", 0x9E660041, fmov_to_gpr(1, 2)),
            ("fmov d1, x2", 0x9E670041, fmov_from_gpr(1, 2)),
            ("fmov d0, d1", 0x1E604020, fmov(0, 1)),
            ("fadd d1, d2, d3", 0x1E632841, fadd(1, 2, 3)),
            ("fsub d1, d2, d3", 0x1E633841, fsub(1, 2, 3)),
            ("fmul d1, d2, d3", 0x1E630841, fmul(1, 2, 3)),
            ("fdiv d1, d2, d3", 0x1E631841, fdiv(1, 2, 3)),
            ("fneg d0, d1", 0x1E614020, fneg(0, 1)),
            ("fsqrt d0, d1", 0x1E61C020, fsqrt(0, 1)),
            ("fcmp d2, d3", 0x1E632040, fcmp(2, 3)),
            ("scvtf d1, x2", 0x9E620041, scvtf(1, 2)),
            ("fcvtzs x1, d2", 0x9E780041, fcvtzs(1, 2)),
        ]);
    }

    #[test]
    fn constant_materialisation_picks_the_shortest_sequence() {
        let seq = |value: u64| {
            let mut out = Vec::new();
            mov_imm64(0, value, &mut out);
            out
        };
        // Single-instruction cases, against the assembler's own choice of form.
        check(&[
            ("mov x0, #0x1234", 0xD2824680, seq(0x1234)[0]),
            ("mov x0, #0x12340000", 0xD2A24680, seq(0x1234_0000)[0]),
            ("mov x0, #0", 0xD2800000, seq(0)[0]),
            // MOVN wins: three halves are already 0xffff.
            (
                "mov x0, #0xfffffffffffffffe",
                0x92800020,
                seq(0xffff_ffff_ffff_fffe)[0],
            ),
            (
                "mov x0, #0xffffffff0000ffff",
                0x92BFFFE0,
                seq(0xffff_ffff_0000_ffff)[0],
            ),
        ]);
        for value in [
            0u64,
            1,
            0x1234,
            0x1234_0000,
            u64::MAX,
            0xffff_ffff_0000_ffff,
        ] {
            assert_eq!(seq(value).len(), 1, "{value:#018x} should be one word");
        }
        assert_eq!(seq(0x1234_5678_9abc_def0).len(), 4);
        assert_eq!(seq(0x0000_5678_0000_def0).len(), 2);
    }
}
