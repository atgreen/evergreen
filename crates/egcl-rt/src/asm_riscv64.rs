// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! RV64I instruction encoding for the native compiler.
//!
//! Every instruction is a 4-byte little-endian word (no compressed forms, so
//! a label is always 4-byte aligned). 64-bit constants come from a literal
//! pool appended after the code, reached with `auipc`+`ld`, which keeps each
//! constant load at two instructions instead of the six-to-eight an inline
//! `li` sequence needs. Branches are the inverted-condition-over-`jal` idiom,
//! so a conditional branch reaches +-1 MiB rather than the +-4 KiB of a bare
//! B-type instruction; the pool and all branch fixups are resolved by
//! [`Asm::finish`].

/// A 4-byte-aligned branch destination in one assembler.
#[derive(Clone, Copy, Debug)]
pub struct Label(usize);

/// ABI register numbers used by the emitters. Integer registers only.
pub mod reg {
    pub const ZERO: u8 = 0;
    pub const RA: u8 = 1;
    pub const SP: u8 = 2;
    pub const T0: u8 = 5;
    pub const T1: u8 = 6;
    pub const T2: u8 = 7;
    pub const A0: u8 = 10;
    pub const A1: u8 = 11;
    pub const A2: u8 = 12;
    pub const A3: u8 = 13;
    pub const S2: u8 = 18;
    pub const S3: u8 = 19;
    pub const S4: u8 = 20;
    pub const S5: u8 = 21;
    pub const T3: u8 = 28;
    pub const T4: u8 = 29;
    pub const T5: u8 = 30;
    /// Reserved for the assembler's own address arithmetic (wide offsets).
    pub const T6: u8 = 31;
}

/// Conditions for [`Asm::branch`], evaluated as `lhs cond rhs` on signed
/// 64-bit values (the tagged representation compares as signed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

impl Cond {
    fn inverse(self) -> Cond {
        match self {
            Cond::Eq => Cond::Ne,
            Cond::Ne => Cond::Eq,
            Cond::Lt => Cond::Ge,
            Cond::Ge => Cond::Lt,
            Cond::Gt => Cond::Le,
            Cond::Le => Cond::Gt,
        }
    }

    /// B-type funct3 plus whether the operands must be swapped: the ISA has
    /// only `blt`/`bge`, so `a > b` is `b < a` and `a <= b` is `b >= a`.
    fn funct3(self) -> (u32, bool) {
        match self {
            Cond::Eq => (0b000, false),
            Cond::Ne => (0b001, false),
            Cond::Lt => (0b100, false),
            Cond::Ge => (0b101, false),
            Cond::Gt => (0b100, true),
            Cond::Le => (0b101, true),
        }
    }
}

const OP_LOAD: u32 = 0x03;
const OP_IMM: u32 = 0x13;
const OP_AUIPC: u32 = 0x17;
const OP_IMM32: u32 = 0x1b;
const OP_STORE: u32 = 0x23;
const OP_REG: u32 = 0x33;
const OP_LUI: u32 = 0x37;
const OP_BRANCH: u32 = 0x63;
const OP_JALR: u32 = 0x67;
const OP_JAL: u32 = 0x6f;

/// Fixed-width RV64I encodings with a deferred literal pool and branch fixups.
#[derive(Default)]
pub struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// `(jal site, target)`: the J-type immediate is patched at finish.
    jumps: Vec<(usize, Label)>,
    /// `(auipc site, constant)`: the auipc/ld pair is patched to reach the
    /// pool slot that holds `constant`.
    pool_uses: Vec<(usize, u64)>,
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn here(&self) -> usize {
        self.code.len()
    }

    pub fn label(&mut self) -> Label {
        let label = Label(self.labels.len());
        self.labels.push(None);
        label
    }

    pub fn bind(&mut self, label: Label) {
        assert!(self.labels[label.0].is_none(), "label already bound");
        self.labels[label.0] = Some(self.here());
    }

    pub fn label_offset(&self, label: Label) -> Option<usize> {
        self.labels.get(label.0).copied().flatten()
    }

    fn word(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }

    fn r_type(&mut self, opcode: u32, funct3: u32, funct7: u32, rd: u8, rs1: u8, rs2: u8) {
        assert!(rd < 32 && rs1 < 32 && rs2 < 32);
        self.word(
            funct7 << 25
                | u32::from(rs2) << 20
                | u32::from(rs1) << 15
                | funct3 << 12
                | u32::from(rd) << 7
                | opcode,
        );
    }

    fn i_type(&mut self, opcode: u32, funct3: u32, rd: u8, rs1: u8, imm: i32) {
        assert!(rd < 32 && rs1 < 32);
        assert!(
            (-2048..=2047).contains(&imm),
            "I-type immediate out of range"
        );
        self.word(
            ((imm as u32) & 0xfff) << 20
                | u32::from(rs1) << 15
                | funct3 << 12
                | u32::from(rd) << 7
                | opcode,
        );
    }

    fn s_type(&mut self, funct3: u32, rs1: u8, rs2: u8, imm: i32) {
        assert!(rs1 < 32 && rs2 < 32);
        assert!(
            (-2048..=2047).contains(&imm),
            "S-type immediate out of range"
        );
        let imm = imm as u32;
        self.word(
            ((imm >> 5) & 0x7f) << 25
                | u32::from(rs2) << 20
                | u32::from(rs1) << 15
                | funct3 << 12
                | (imm & 0x1f) << 7
                | OP_STORE,
        );
    }

    fn u_type(&mut self, opcode: u32, rd: u8, imm20: u32) {
        assert!(rd < 32);
        self.word((imm20 & 0xfffff) << 12 | u32::from(rd) << 7 | opcode);
    }

    fn b_type_word(funct3: u32, rs1: u8, rs2: u8, offset: i32) -> u32 {
        assert!(offset % 2 == 0 && (-4096..=4095).contains(&offset));
        let imm = offset as u32;
        ((imm >> 12) & 1) << 31
            | ((imm >> 5) & 0x3f) << 25
            | u32::from(rs2) << 20
            | u32::from(rs1) << 15
            | funct3 << 12
            | ((imm >> 1) & 0xf) << 8
            | ((imm >> 11) & 1) << 7
            | OP_BRANCH
    }

    fn j_type_word(rd: u8, offset: i32) -> u32 {
        assert!(offset % 2 == 0 && (-(1 << 20)..(1 << 20)).contains(&offset));
        let imm = offset as u32;
        ((imm >> 20) & 1) << 31
            | ((imm >> 1) & 0x3ff) << 21
            | ((imm >> 11) & 1) << 20
            | ((imm >> 12) & 0xff) << 12
            | u32::from(rd) << 7
            | OP_JAL
    }

    // ── Register arithmetic ────────────────────────────────────────────

    pub fn mov(&mut self, dst: u8, src: u8) {
        self.add_imm(dst, src, 0);
    }

    pub fn add(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b000, 0, dst, lhs, rhs);
    }

    pub fn sub(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b000, 0x20, dst, lhs, rhs);
    }

    pub fn and(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b111, 0, dst, lhs, rhs);
    }

    pub fn or(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b110, 0, dst, lhs, rhs);
    }

    pub fn xor(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b100, 0, dst, lhs, rhs);
    }

    /// `dst = src + imm` (ADDI), imm within +-2 KiB.
    pub fn add_imm(&mut self, dst: u8, src: u8, imm: i32) {
        self.i_type(OP_IMM, 0b000, dst, src, imm);
    }

    pub fn and_imm(&mut self, dst: u8, src: u8, imm: i32) {
        self.i_type(OP_IMM, 0b111, dst, src, imm);
    }

    pub fn shift_left(&mut self, dst: u8, src: u8, bits: u8) {
        assert!(bits < 64);
        self.i_type(OP_IMM, 0b001, dst, src, i32::from(bits));
    }

    pub fn shift_right_signed(&mut self, dst: u8, src: u8, bits: u8) {
        assert!(bits < 64);
        self.i_type(OP_IMM, 0b101, dst, src, 0x400 | i32::from(bits));
    }

    pub fn shift_right(&mut self, dst: u8, src: u8, bits: u8) {
        assert!(bits < 64);
        self.i_type(OP_IMM, 0b101, dst, src, i32::from(bits));
    }

    /// `dst = (lhs < rhs)` as 0 or 1, signed.
    pub fn set_less_than(&mut self, dst: u8, lhs: u8, rhs: u8) {
        self.r_type(OP_REG, 0b010, 0, dst, lhs, rhs);
    }

    // ── Memory ─────────────────────────────────────────────────────────

    /// Materialize `base + disp` into T6 when `disp` does not fit a 12-bit
    /// displacement, returning the register and displacement to use.
    fn wide(&mut self, base: u8, disp: i32) -> (u8, i32) {
        if (-2048..=2047).contains(&disp) {
            return (base, disp);
        }
        assert!(base != reg::T6, "T6 is the assembler's scratch register");
        self.imm32(reg::T6, disp);
        self.add(reg::T6, reg::T6, base);
        (reg::T6, 0)
    }

    /// `dst = base + disp` for any 32-bit displacement.
    pub fn address(&mut self, dst: u8, base: u8, disp: i32) {
        let (base, disp) = self.wide(base, disp);
        self.add_imm(dst, base, disp);
    }

    pub fn load(&mut self, dst: u8, base: u8, disp: i32) {
        let (base, disp) = self.wide(base, disp);
        self.i_type(OP_LOAD, 0b011, dst, base, disp);
    }

    pub fn store(&mut self, src: u8, base: u8, disp: i32) {
        let (base, disp) = self.wide(base, disp);
        self.s_type(0b011, base, src, disp);
    }

    /// LWU: zero-extending 32-bit load (an `AtomicU32` counter).
    pub fn load_u32(&mut self, dst: u8, base: u8, disp: i32) {
        let (base, disp) = self.wide(base, disp);
        self.i_type(OP_LOAD, 0b110, dst, base, disp);
    }

    pub fn store_u32(&mut self, src: u8, base: u8, disp: i32) {
        let (base, disp) = self.wide(base, disp);
        self.s_type(0b010, base, src, disp);
    }

    // ── Constants ──────────────────────────────────────────────────────

    /// A sign-extended 32-bit constant through LUI+ADDIW, two instructions.
    pub fn imm32(&mut self, dst: u8, value: i32) {
        let lo = ((value << 20) >> 20) as i32; // sign-extended low 12 bits
        let hi = (value.wrapping_sub(lo) as u32) >> 12;
        if hi == 0 {
            self.add_imm(dst, reg::ZERO, lo);
            return;
        }
        self.u_type(OP_LUI, dst, hi);
        if lo != 0 {
            self.i_type(OP_IMM32, 0b000, dst, dst, lo);
        }
    }

    /// Any 64-bit constant: small ones inline, the rest from the literal pool
    /// (`auipc dst, hi; ld dst, lo(dst)`, resolved by [`Self::finish`]).
    pub fn imm64(&mut self, dst: u8, value: u64) {
        if let Ok(small) = i32::try_from(value as i64) {
            self.imm32(dst, small);
            return;
        }
        self.pool_uses.push((self.here(), value));
        self.u_type(OP_AUIPC, dst, 0);
        self.i_type(OP_LOAD, 0b011, dst, dst, 0);
    }

    // ── Control flow ───────────────────────────────────────────────────

    /// `jalr ra, reg, 0`.
    pub fn call_reg(&mut self, reg: u8) {
        self.i_type(OP_JALR, 0b000, reg::RA, reg, 0);
    }

    pub fn ret(&mut self) {
        self.i_type(OP_JALR, 0b000, reg::ZERO, reg::RA, 0);
    }

    /// Save RA and S2-S5 (the emitters' callee-saved state) in a 48-byte
    /// frame, keeping the 16-byte stack alignment the ABI requires.
    pub fn prologue(&mut self) {
        self.add_imm(reg::SP, reg::SP, -48);
        self.store(reg::RA, reg::SP, 40);
        self.store(reg::S2, reg::SP, 32);
        self.store(reg::S3, reg::SP, 24);
        self.store(reg::S4, reg::SP, 16);
        self.store(reg::S5, reg::SP, 8);
    }

    pub fn epilogue(&mut self) {
        self.load(reg::RA, reg::SP, 40);
        self.load(reg::S2, reg::SP, 32);
        self.load(reg::S3, reg::SP, 24);
        self.load(reg::S4, reg::SP, 16);
        self.load(reg::S5, reg::SP, 8);
        self.add_imm(reg::SP, reg::SP, 48);
        self.ret();
    }

    /// Unconditional `jal zero, target` (+-1 MiB).
    pub fn jump(&mut self, target: Label) {
        self.jumps.push((self.here(), target));
        self.word(Self::j_type_word(reg::ZERO, 0));
    }

    /// `if lhs cond rhs goto target`, as an inverted B-type over a `jal` so
    /// the reach is the jump's +-1 MiB.
    pub fn branch(&mut self, cond: Cond, lhs: u8, rhs: u8, target: Label) {
        let (funct3, swap) = cond.inverse().funct3();
        let (rs1, rs2) = if swap { (rhs, lhs) } else { (lhs, rhs) };
        self.word(Self::b_type_word(funct3, rs1, rs2, 8));
        self.jump(target);
    }

    /// Resolve branches and append the literal pool. `None` if a label is
    /// unbound or a jump is out of range, in which case the function stays at
    /// the tier below.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for &(site, target) in &self.jumps {
            let target = i64::try_from(self.label_offset(target)?).ok()?;
            let offset = i32::try_from(target.checked_sub(i64::try_from(site).ok()?)?).ok()?;
            if !(-(1 << 20)..(1 << 20)).contains(&offset) {
                return None;
            }
            let word = Self::j_type_word(reg::ZERO, offset);
            self.code[site..site + 4].copy_from_slice(&word.to_le_bytes());
        }
        if self.pool_uses.is_empty() {
            return Some(self.code);
        }
        while self.code.len() % 8 != 0 {
            self.code.extend_from_slice(&[0, 0, 0, 0]); // padding never executes
        }
        let mut slots: Vec<u64> = Vec::new();
        for &(site, value) in &self.pool_uses {
            let index = match slots.iter().position(|&v| v == value) {
                Some(index) => index,
                None => {
                    slots.push(value);
                    slots.len() - 1
                }
            };
            let pool_base = self.code.len();
            let offset = i32::try_from(pool_base + index * 8).ok()? - i32::try_from(site).ok()?;
            let lo = (offset << 20) >> 20;
            let hi = (offset.wrapping_sub(lo) as u32) >> 12;
            let rd = u8::try_from(
                (u32::from_le_bytes(self.code[site..site + 4].try_into().ok()?) >> 7) & 0x1f,
            )
            .ok()?;
            let auipc = (hi & 0xfffff) << 12 | u32::from(rd) << 7 | OP_AUIPC;
            let ld = ((lo as u32) & 0xfff) << 20
                | u32::from(rd) << 15
                | 0b011 << 12
                | u32::from(rd) << 7
                | OP_LOAD;
            self.code[site..site + 4].copy_from_slice(&auipc.to_le_bytes());
            self.code[site + 4..site + 8].copy_from_slice(&ld.to_le_bytes());
        }
        for value in slots {
            self.code.extend_from_slice(&value.to_le_bytes());
        }
        Some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::reg::*;
    use super::*;

    fn words(code: &[u8]) -> Vec<u32> {
        code.chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[cfg(all(target_arch = "riscv64", unix))]
    #[test]
    fn executes_generated_code_across_rust_call() {
        extern "C" fn add_seven(value: u64) -> u64 {
            value.wrapping_add(7)
        }
        let mut a = Asm::new();
        a.prologue();
        a.mov(S2, A0); // retain the caller's output pointer across the Rust call
        a.imm64(A0, 0x123456789abcdef0);
        a.imm64(T0, add_seven as *const () as u64);
        a.call_reg(T0);
        a.store(A0, S2, 0);
        a.epilogue();
        let code = crate::jit::JitBuffer::new(&a.finish().unwrap()).unwrap();
        // SAFETY: the generated LP64 function writes one u64 through its
        // first argument and returns the callback's u64 in a0.
        let entry: unsafe extern "C" fn(*mut u64) -> u64 =
            unsafe { core::mem::transmute(code.as_ptr()) };
        let mut output = 0;
        assert_eq!(unsafe { entry(&mut output) }, 0x123456789abcdef7);
        assert_eq!(output, 0x123456789abcdef7);
    }

    #[cfg(all(target_arch = "riscv64", unix))]
    #[test]
    fn executes_branches_and_pool_constants() {
        // Returns 1 if a0 < a1 (signed) else the 64-bit pool constant.
        let mut a = Asm::new();
        let less = a.label();
        let end = a.label();
        a.branch(Cond::Lt, A0, A1, less);
        a.imm64(A0, 0x8000_0000_0000_0001);
        a.jump(end);
        a.bind(less);
        a.add_imm(A0, ZERO, 1);
        a.bind(end);
        a.ret();
        let code = crate::jit::JitBuffer::new(&a.finish().unwrap()).unwrap();
        // SAFETY: a leaf function of two integer arguments returning in a0.
        let entry: unsafe extern "C" fn(u64, u64) -> u64 =
            unsafe { core::mem::transmute(code.as_ptr()) };
        assert_eq!(unsafe { entry(-5i64 as u64, 3) }, 1);
        assert_eq!(unsafe { entry(3, -5i64 as u64) }, 0x8000_0000_0000_0001);
    }

    // Expected words below were produced by GNU as (binutils 2.44,
    // riscv64-linux-gnu) from the equivalent mnemonics.
    #[test]
    fn register_and_immediate_encodings_match_gnu_as() {
        let mut a = Asm::new();
        a.mov(S2, A0); // addi s2, a0, 0
        a.add(A0, A0, A1); // add a0, a0, a1
        a.sub(A0, A0, A1); // sub a0, a0, a1
        a.and(T0, A0, T1); // and t0, a0, t1
        a.or(T0, A0, T1); // or t0, a0, t1
        a.xor(T0, A0, T1); // xor t0, a0, t1
        a.add_imm(S3, S3, -8); // addi s3, s3, -8
        a.and_imm(T0, A0, 7); // andi t0, a0, 7
        a.shift_left(A0, A0, 3); // slli a0, a0, 3
        a.shift_right_signed(A1, A1, 3); // srai a1, a1, 3
        a.shift_right(A1, A1, 32); // srli a1, a1, 32
        a.set_less_than(T0, A0, A1); // slt t0, a0, a1
        assert_eq!(
            words(&a.finish().unwrap()),
            [
                0x00050913, 0x00b50533, 0x40b50533, 0x006572b3, 0x006562b3, 0x006542b3, 0xff898993,
                0x00757293, 0x00351513, 0x4035d593, 0x0205d593, 0x00b522b3,
            ]
        );
    }

    #[test]
    fn memory_encodings_match_gnu_as() {
        let mut a = Asm::new();
        a.load(A0, S3, -8); // ld a0, -8(s3)
        a.store(A0, S2, 2040); // sd a0, 2040(s2)
        a.load_u32(A0, T0, 0); // lwu a0, 0(t0)
        a.store_u32(A0, T0, 4); // sw a0, 4(t0)
        a.address(S3, S2, 16); // addi s3, s2, 16
        assert_eq!(
            words(&a.finish().unwrap()),
            [0xff89b503, 0x7ea93c23, 0x0002e503, 0x00a2a223, 0x01090993]
        );
    }

    #[test]
    fn wide_displacements_go_through_t6() {
        let mut a = Asm::new();
        a.load(A0, S2, 4096); // lui t6, 1; add t6, t6, s2; ld a0, 0(t6)
        a.store(A1, S2, -4100); // lui t6, 0xfffff; addiw t6, t6, -4; add; sd a1, 0(t6)
        assert_eq!(
            words(&a.finish().unwrap()),
            [
                0x00001fb7, 0x012f8fb3, 0x000fb503, 0xffffffb7, 0xffcf8f9b, 0x012f8fb3, 0x00bfb023,
            ]
        );
    }

    #[test]
    fn constants_use_lui_addiw_or_the_pool() {
        let mut a = Asm::new();
        a.imm64(A0, 0); // addi a0, zero, 0
        a.imm64(A0, 2047); // addi a0, zero, 2047
        a.imm64(A0, 2048); // lui a0, 1; addiw a0, a0, -2048
        a.imm64(A0, 0xdead_beef_u64); // > i32::MAX: auipc a0, 0; ld a0, 24(a0)
        a.imm64(A1, 0xdead_beef_u64); // same pool slot: auipc a1, 0; ld a1, 16(a1)
        a.ret();
        let code = a.finish().unwrap();
        // 36 bytes of code, padded to 40, then the one 8-byte pool slot.
        assert_eq!(code.len(), 48);
        assert_eq!(
            words(&code[..40]),
            [
                0x00000513, 0x7ff00513, 0x00001537, 0x8005051b, 0x00000517, 0x01853503, 0x00000597,
                0x0105b583, 0x00008067, 0,
            ]
        );
        assert_eq!(&code[40..48], &0xdead_beef_u64.to_le_bytes());
    }

    #[test]
    fn branches_invert_over_a_jump() {
        let mut a = Asm::new();
        let start = a.label();
        let end = a.label();
        a.bind(start);
        a.branch(Cond::Eq, A0, A1, end); // bne a0, a1, +8; jal zero, +16
        a.branch(Cond::Gt, A0, A1, start); // !(a0 > a1) is a1 >= a0: bge a1, a0, +8; jal
        a.jump(start);
        a.bind(end);
        a.ret();
        assert_eq!(
            words(&a.finish().unwrap()),
            [
                0x00b51463, 0x0100006f, 0x00a5d463, 0xff5ff06f, 0xff1ff06f, 0x00008067,
            ]
        );
    }

    #[test]
    fn unresolved_jump_rejects_code() {
        let mut a = Asm::new();
        let target = a.label();
        a.jump(target);
        assert!(a.finish().is_none());
    }

    #[test]
    fn abi_frame_saves_ra_and_s2_to_s5() {
        let mut a = Asm::new();
        a.prologue();
        a.epilogue();
        assert_eq!(
            words(&a.finish().unwrap()),
            [
                0xfd010113, 0x02113423, 0x03213023, 0x01313c23, 0x01413823, 0x01513423, 0x02813083,
                0x02013903, 0x01813983, 0x01013a03, 0x00813a83, 0x03010113, 0x00008067,
            ]
        );
    }
}
