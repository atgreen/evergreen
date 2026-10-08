// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! IBM Z instruction encoding for the native compiler.

/// A halfword-aligned branch destination in one assembler.
#[derive(Clone, Copy, Debug)]
pub struct Label(usize);

/// Fixed-width encodings with deferred BRCL displacements. Instructions and
/// immediates are big endian, independently of the compiler host's byte order.
#[derive(Default)]
pub struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    branches: Vec<(usize, Label)>,
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

    fn rre(&mut self, opcode: u16, dst: u8, src: u8) {
        assert!(dst < 16 && src < 16);
        self.code.extend_from_slice(&opcode.to_be_bytes());
        self.code.extend_from_slice(&[0, dst << 4 | src]);
    }

    pub fn mov(&mut self, dst: u8, src: u8) {
        self.rre(0xb904, dst, src);
    }

    pub fn compare(&mut self, lhs: u8, rhs: u8) {
        self.rre(0xb920, lhs, rhs);
    }

    pub fn add(&mut self, dst: u8, src: u8) {
        self.rre(0xb908, dst, src);
    }

    pub fn sub(&mut self, dst: u8, src: u8) {
        self.rre(0xb909, dst, src);
    }

    pub fn and(&mut self, dst: u8, src: u8) {
        self.rre(0xb980, dst, src);
    }

    pub fn or(&mut self, dst: u8, src: u8) {
        self.rre(0xb981, dst, src);
    }

    pub fn xor(&mut self, dst: u8, src: u8) {
        self.rre(0xb982, dst, src);
    }

    /// Multiply the unsigned low half (even + 1) by src into an even/odd pair.
    pub fn multiply_unsigned_wide(&mut self, even: u8, src: u8) {
        assert!(even < 15 && even % 2 == 0);
        self.rre(0xb986, even, src);
    }

    pub fn load_float_bits(&mut self, float_dst: u8, general_src: u8) {
        self.rre(0xb3c1, float_dst, general_src);
    }

    pub fn store_float_bits(&mut self, general_dst: u8, float_src: u8) {
        self.rre(0xb3cd, general_dst, float_src);
    }

    pub fn add_single(&mut self, dst: u8, src: u8) {
        self.rre(0xb30a, dst, src);
    }

    pub fn sub_single(&mut self, dst: u8, src: u8) {
        self.rre(0xb30b, dst, src);
    }

    pub fn multiply_single(&mut self, dst: u8, src: u8) {
        self.rre(0xb317, dst, src);
    }

    pub fn shift_right_signed(&mut self, dst: u8, src: u8, bits: u8) {
        assert!(dst < 16 && src < 16 && bits < 64);
        self.code
            .extend_from_slice(&[0xeb, dst << 4 | src, 0, bits, 0, 0x0a]);
    }

    /// SLLG: a 64-bit logical left shift by a constant, like
    /// [`Self::shift_right_signed`] with the SRAG opcode byte swapped for SLLG's.
    pub fn shift_left(&mut self, dst: u8, src: u8, bits: u8) {
        assert!(dst < 16 && src < 16 && bits < 64);
        self.code
            .extend_from_slice(&[0xeb, dst << 4 | src, 0, bits, 0, 0x0d]);
    }

    pub fn address(&mut self, dst: u8, base: u8, disp: i32) {
        assert!(dst < 16);
        self.memory(0xe3, 0x71, dst << 4, base, disp);
    }

    fn memory(&mut self, prefix: u8, opcode: u8, regs: u8, base: u8, disp: i32) {
        assert!(
            base > 0 && base < 16,
            "r0 means no base in address operands"
        );
        assert!((-524288..=524287).contains(&disp));
        let d = disp as u32;
        self.code.extend_from_slice(&[
            prefix,
            regs,
            base << 4 | ((d >> 8) & 15) as u8,
            d as u8,
            (d >> 12) as u8,
            opcode,
        ]);
    }

    pub fn load(&mut self, dst: u8, base: u8, disp: i32) {
        assert!(dst < 16);
        self.memory(0xe3, 0x04, dst << 4, base, disp);
    }

    pub fn store(&mut self, src: u8, base: u8, disp: i32) {
        assert!(src < 16);
        self.memory(0xe3, 0x24, src << 4, base, disp);
    }

    /// LLGC: zero-extend one byte into a 64-bit register.
    pub fn load_u8(&mut self, dst: u8, base: u8, disp: i32) {
        assert!(dst < 16);
        self.memory(0xe3, 0x90, dst << 4, base, disp);
    }

    pub fn load_u32(&mut self, dst: u8, base: u8, disp: i32) {
        assert!(dst < 16);
        self.memory(0xe3, 0x16, dst << 4, base, disp);
    }

    /// STHY: store the low 16 bits of a register.
    pub fn store_u16(&mut self, src: u8, base: u8, disp: i32) {
        assert!(src < 16);
        self.memory(0xe3, 0x70, src << 4, base, disp);
    }

    pub fn store_u32(&mut self, src: u8, base: u8, disp: i32) {
        assert!(src < 16);
        self.memory(0xe3, 0x50, src << 4, base, disp);
    }

    pub fn add_imm(&mut self, reg: u8, value: i16) {
        assert!(reg < 16);
        self.code.extend_from_slice(&[0xa7, reg << 4 | 0x0b]);
        self.code.extend_from_slice(&value.to_be_bytes());
    }

    /// IIHF followed by IILF defines all 64 bits without a literal pool.
    pub fn imm64(&mut self, reg: u8, value: u64) {
        assert!(reg < 16);
        self.code.extend_from_slice(&[0xc0, reg << 4 | 8]);
        self.code
            .extend_from_slice(&((value >> 32) as u32).to_be_bytes());
        self.code.extend_from_slice(&[0xc0, reg << 4 | 9]);
        self.code.extend_from_slice(&(value as u32).to_be_bytes());
    }

    pub fn call_reg(&mut self, reg: u8) {
        assert!(reg > 0 && reg < 16);
        self.code.extend_from_slice(&[0x0d, 0xe0 | reg]);
    }

    pub fn ret(&mut self) {
        self.code.extend_from_slice(&[0x07, 0xfe]);
    }

    /// Save r6-r15 in the caller's ABI save area and reserve the 160-byte
    /// outgoing save/argument area. Tagged Lisp roots live in EgclStack, never
    /// in this native save area across an allocating call.
    pub fn prologue(&mut self) {
        self.memory(0xeb, 0x24, 0x6f, 15, 48);
        self.add_imm(15, -160);
    }

    pub fn epilogue(&mut self) {
        self.memory(0xeb, 0x04, 0x6f, 15, 208);
        self.ret();
    }

    /// BRASL %r14,target: a relative call to a label in this same code. The
    /// displacement is patched by [`Self::finish`] exactly like a branch's.
    pub fn call_label(&mut self, target: Label) {
        self.branches.push((self.here(), target));
        self.code.extend_from_slice(&[0xc0, 0xe5, 0, 0, 0, 0]);
    }

    /// Branch on an architectural condition-code mask (8=equal, 15=always).
    pub fn branch(&mut self, mask: u8, target: Label) {
        assert!(mask < 16);
        self.branches.push((self.here(), target));
        self.code
            .extend_from_slice(&[0xc0, mask << 4 | 4, 0, 0, 0, 0]);
    }

    pub fn finish(mut self) -> Option<Vec<u8>> {
        for &(site, target) in &self.branches {
            let target = i64::try_from(self.label_offset(target)?).ok()?;
            let displacement = target.checked_sub(i64::try_from(site).ok()?)?;
            if displacement % 2 != 0 {
                return None;
            }
            let halfwords = i32::try_from(displacement / 2).ok()?;
            self.code[site + 2..site + 6].copy_from_slice(&halfwords.to_be_bytes());
        }
        Some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_arch = "s390x", unix))]
    #[test]
    fn executes_generated_code_across_rust_call() {
        extern "C" fn add_seven(value: u64) -> u64 {
            value.wrapping_add(7)
        }
        let mut a = Asm::new();
        a.prologue();
        a.mov(8, 2); // retain the caller's output pointer across the Rust call
        a.imm64(2, 0x123456789abcdef0);
        a.imm64(1, add_seven as *const () as u64);
        a.call_reg(1);
        a.store(2, 8, 0);
        a.epilogue();
        let code = crate::jit::JitBuffer::new(&a.finish().unwrap()).unwrap();
        // SAFETY: the generated System Z ABI function writes one u64 through
        // its first argument and returns the callback's u64 in r2.
        let entry: unsafe extern "C" fn(*mut u64) -> u64 =
            unsafe { core::mem::transmute(code.as_ptr()) };
        let mut output = 0;
        assert_eq!(unsafe { entry(&mut output) }, 0x123456789abcdef7);
        assert_eq!(output, 0x123456789abcdef7);
    }

    #[test]
    fn encodings_match_llvm_systemz() {
        let mut a = Asm::new();
        a.mov(8, 2);
        a.load(2, 9, -8);
        a.store(2, 8, 524287);
        a.add_imm(9, -8);
        a.imm64(2, 0x123456789abcdef0);
        a.call_reg(1);
        a.ret();
        assert_eq!(
            a.finish().unwrap(),
            [
                0xb9, 0x04, 0x00, 0x82, 0xe3, 0x20, 0x9f, 0xf8, 0xff, 0x04, 0xe3, 0x20, 0x8f, 0xff,
                0x7f, 0x24, 0xa7, 0x9b, 0xff, 0xf8, 0xc0, 0x28, 0x12, 0x34, 0x56, 0x78, 0xc0, 0x29,
                0x9a, 0xbc, 0xde, 0xf0, 0x0d, 0xe1, 0x07, 0xfe,
            ]
        );
    }

    #[test]
    fn arithmetic_and_addresses_match_llvm_systemz() {
        let mut a = Asm::new();
        a.compare(2, 3);
        a.add(2, 3);
        a.sub(2, 3);
        a.and(4, 0);
        a.shift_right_signed(3, 3, 3);
        a.address(9, 8, 524280);
        a.load_u32(2, 1, 0);
        a.store_u32(2, 1, 0);
        assert_eq!(
            a.finish().unwrap(),
            [
                0xb9, 0x20, 0, 0x23, 0xb9, 0x08, 0, 0x23, 0xb9, 0x09, 0, 0x23, 0xb9, 0x80, 0, 0x40,
                0xeb, 0x33, 0, 3, 0, 0x0a, 0xe3, 0x90, 0x8f, 0xf8, 0x7f, 0x71, 0xe3, 0x20, 0x10, 0,
                0, 0x16, 0xe3, 0x20, 0x10, 0, 0, 0x50,
            ]
        );
    }

    #[test]
    fn halfword_store_encoding_matches_llvm_systemz() {
        // llvm-mc: sthy %r1,-2(%r2) => e3 10 2f fe ff 70
        let mut a = Asm::new();
        a.store_u16(1, 2, -2);
        assert_eq!(a.finish().unwrap(), [0xe3, 0x10, 0x2f, 0xfe, 0xff, 0x70]);
    }

    #[test]
    fn byte_load_encoding_matches_llvm_systemz() {
        // llvm-mc: llgc %r3,7(%r4) => e3 30 40 07 00 90
        let mut a = Asm::new();
        a.load_u8(3, 4, 7);
        assert_eq!(a.finish().unwrap(), [0xe3, 0x30, 0x40, 0x07, 0x00, 0x90]);
    }

    #[test]
    fn logical_and_left_shift_encodings_match_llvm_systemz() {
        // llvm-mc -triple=s390x-linux-gnu --show-encoding:
        //   ogr %r2,%r3        b9 81 00 23
        //   xgr %r2,%r3        b9 82 00 23
        //   sllg %r3,%r2,5     eb 32 00 05 00 0d
        let mut a = Asm::new();
        a.or(2, 3);
        a.xor(2, 3);
        a.shift_left(3, 2, 5);
        assert_eq!(
            a.finish().unwrap(),
            [
                0xb9, 0x81, 0, 0x23, 0xb9, 0x82, 0, 0x23, 0xeb, 0x32, 0, 5, 0, 0x0d
            ]
        );
    }

    #[test]
    fn wide_multiply_and_single_float_encodings_match_llvm_z10() {
        let mut a = Asm::new();
        a.multiply_unsigned_wide(2, 4);
        a.load_float_bits(0, 2);
        a.load_float_bits(2, 3);
        a.store_float_bits(2, 0);
        a.add_single(0, 2);
        a.sub_single(0, 2);
        a.multiply_single(0, 2);
        assert_eq!(
            a.finish().unwrap(),
            [
                0xb9, 0x86, 0, 0x24, 0xb3, 0xc1, 0, 0x02, 0xb3, 0xc1, 0, 0x23, 0xb3, 0xcd, 0, 0x20,
                0xb3, 0x0a, 0, 0x02, 0xb3, 0x0b, 0, 0x02, 0xb3, 0x17, 0, 0x02,
            ]
        );
    }

    #[test]
    fn branches_use_signed_halfwords_from_instruction_start() {
        let mut a = Asm::new();
        let start = a.label();
        let end = a.label();
        a.bind(start);
        a.branch(8, end);
        a.branch(15, start);
        a.bind(end);
        a.ret();
        assert_eq!(
            a.finish().unwrap(),
            [
                0xc0, 0x84, 0, 0, 0, 6, 0xc0, 0xf4, 0xff, 0xff, 0xff, 0xfd, 0x07, 0xfe,
            ]
        );
    }

    #[test]
    fn relative_call_uses_signed_halfwords_from_instruction_start() {
        // llvm-mc: brasl %r14,.+6 => c0 e5 00 00 00 03
        let mut a = Asm::new();
        let target = a.label();
        a.call_label(target);
        a.bind(target);
        a.ret();
        assert_eq!(a.finish().unwrap(), [0xc0, 0xe5, 0, 0, 0, 3, 0x07, 0xfe]);
    }

    #[test]
    fn unresolved_branch_rejects_code() {
        let mut a = Asm::new();
        let target = a.label();
        a.branch(15, target);
        assert!(a.finish().is_none());
    }

    #[test]
    fn abi_frame_preserves_callee_saved_registers() {
        let mut a = Asm::new();
        a.prologue();
        a.epilogue();
        assert_eq!(
            a.finish().unwrap(),
            [
                0xeb, 0x6f, 0xf0, 0x30, 0x00, 0x24, 0xa7, 0xfb, 0xff, 0x60, 0xeb, 0x6f, 0xf0, 0xd0,
                0x00, 0x04, 0x07, 0xfe,
            ]
        );
    }
}
