// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Checked, noncollecting eight-octet store. No call, poll, allocation or raw
//! pointer escapes this template. The sole payload write follows every check.

use super::*;

fn invalid_shape(f: &Function, data: &crate::t2::ir::InstData) -> bool {
    use crate::t2::ir::{AuxData, InstFlags, Opcode, ValueRepresentation as Repr};
    data.opcode != Opcode::TryStoreIntegerOctets8
        || data.args.len() != 4
        || data.results.len() != 2
        || !matches!(data.aux, AuxData::None)
        || data.flags
            != (InstFlags {
                effectful: true,
                ..Default::default()
            })
        || data.frame_state.is_some()
        || !data.targets.is_empty()
        || data
            .args
            .iter()
            .chain(&data.results)
            .any(|&v| f.value(v).repr != Repr::Tagged)
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) fn emit(
    a: &mut Asm,
    f: &Function,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    const RSP: u8 = 4;
    if invalid_shape(f, data)
        || data.args.iter().any(|v| {
            !constants.contains_key(v)
                && match reg.get(v) {
                    Some(&r) => matches!(r, RAX | SCRATCH | RSP),
                    None => true,
                }
        })
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let success_dst = framed_alloc(reg, pool, data.results[0])?;
    let final_dst = framed_alloc(reg, pool, data.results[1])?;
    if success_dst == final_dst || matches!(success_dst, RSP) || matches!(final_dst, RSP) {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    // Snapshot all operands before writing either result: an output may alias
    // any input home. Framed inputs never occupy the reserved RAX/RDX scratch.
    for input in &data.args {
        if let Some(&bits) = constants.get(input) {
            mov_imm64(a, RAX, bits as i64);
            push_reg(a, RAX);
        } else {
            push_reg(a, reg[input]);
        }
    }
    emit_body(a, success_dst, final_dst)
}

/// Materialize operands directly from canonical native homes. Four spilled
/// operands exceed the generic instruction adapter's three scratch registers.
/// Snapshot them before writing either result; stack loads compensate for each
/// preceding push, and dead results need no home.
pub(super) fn emit_framed(
    a: &mut Asm,
    f: &Function,
    data: &crate::t2::ir::InstData,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    if invalid_shape(f, data) {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    for (index, input) in data.args.iter().enumerate() {
        if let Some(&bits) = constants.get(input) {
            mov_imm64(a, RAX, bits as i64);
        } else {
            let home = *homes.get(input).ok_or(EmitError::UnsupportedOp(0xF2))?;
            load_home(a, RAX, home, (index * 8) as i32);
        }
        push_reg(a, RAX);
    }
    emit_body(a, RAX, SCRATCH)?;
    for (&value, source) in data.results.iter().zip([RAX, SCRATCH]) {
        if let Some(&home) = homes.get(&value) {
            store_home(a, home, source, 0);
        }
    }
    Ok(())
}

fn emit_body(a: &mut Asm, success_dst: u8, final_dst: u8) -> Result<(), EmitError> {
    const RSP: u8 = 4;
    alu_r_imm(a, 5, RSP, 24);
    // Fixed stack: raw word/result flag +0, final FX +8, body/slot +16,
    // integer +24, end +32, start +40, array +48. Every exit restores 56 bytes.
    let failed = a.label();
    let fixnum = a.label();
    let positive = a.label();
    let negative = a.label();
    let decoded = a.label();
    let done = a.label();
    load_from_rsp(a, RAX, 24);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, 0);
    a.jcc(Cc::E, fixnum);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 4, RAX, -8);
    a.extend_from_slice(&[0x80, 0x78, 0x07, egcl_rt::object::type_id::BIGNUM]);
    a.jcc(Cc::Ne, failed);
    a.extend_from_slice(&[0x83, 0x78, 0x0c, 0x01]); // cmp n_limbs,1
    a.jcc(Cc::Ne, failed);
    a.extend_from_slice(&[0x83, 0x78, 0x08, 0x01]); // cmp sign,1
    a.jcc(Cc::E, positive);
    a.extend_from_slice(&[0x83, 0x78, 0x08, 0xff]); // cmp sign,-1
    a.jcc(Cc::Ne, failed);
    load_mem64_disp(a, RAX, RAX, 16);
    // Canonical negative bignums have magnitude > abs(MIN-FIXNUM).
    // Shift before comparing so full unsigned magnitudes remain signed-safe.
    mov_rr(a, SCRATCH, RAX);
    shr_imm(a, SCRATCH, 60);
    alu_r_imm(a, 7, SCRATCH, 1);
    a.jcc(Cc::L, failed);
    a.jcc(Cc::G, negative);
    mov_imm64(a, SCRATCH, 1i64 << 60);
    cmp_rr(a, RAX, SCRATCH);
    a.jcc(Cc::E, failed);
    a.bind(negative);
    neg_r(a, RAX); // modulo 2^64 gives the low two's-complement octets
    mov_imm64(a, SCRATCH, -8); // ASH of the original negative integer by -64
    a.jmp(decoded);
    a.bind(positive);
    load_mem64_disp(a, RAX, RAX, 16);
    mov_rr(a, SCRATCH, RAX);
    shr_imm(a, SCRATCH, 60);
    alu_r_imm(a, 7, SCRATCH, 0); // canonical positive magnitude >= 2^60
    a.jcc(Cc::E, failed);
    mov_imm64(a, SCRATCH, 0);
    a.jmp(decoded);
    a.bind(fixnum);
    sar_imm(a, RAX, 3);
    mov_rr(a, SCRATCH, RAX);
    sar_imm(a, SCRATCH, 63);
    shl_imm(a, SCRATCH, 3);
    a.bind(decoded);
    store_to_rsp(a, RAX, 0);
    store_to_rsp(a, SCRATCH, 8);

    load_from_rsp(a, RAX, 48);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 4, RAX, -8);
    a.extend_from_slice(&[0x80, 0x78, 0x07, egcl_rt::object::type_id::SIMPLE_ARRAY]);
    a.jcc(Cc::Ne, failed);
    let small = a.label();
    let body = a.label();
    a.extend_from_slice(&[0x66, 0x81, 0x38, 0xff, 0xff]);
    a.jcc(Cc::Ne, small);
    alu_r_imm(a, 0, RAX, 16);
    a.jmp(body);
    a.bind(small);
    alu_r_imm(a, 0, RAX, 8);
    a.bind(body);
    a.extend_from_slice(&[0x80, 0x38, egcl_rt::object::ElementTypeTag::U8 as u8]);
    a.jcc(Cc::Ne, failed);
    store_to_rsp(a, RAX, 16);
    load_from_rsp(a, RAX, 40);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 7, RAX, 0);
    a.jcc(Cc::L, failed);
    load_from_rsp(a, RAX, 32);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    a.jcc(Cc::Ne, failed);
    load_from_rsp(a, SCRATCH, 40);
    alu_r_imm(a, 0, SCRATCH, 8 << 3);
    a.jcc(Cc::O, failed);
    cmp_rr(a, RAX, SCRATCH);
    a.jcc(Cc::Ne, failed);
    sar_imm(a, RAX, 3);
    load_from_rsp(a, SCRATCH, 16);
    // Runtime packed allocation bounds length below isize::MAX. Equal end is
    // permitted: it denotes the byte just after the eight-byte store.
    cmp_mem64_disp8(a, RAX, SCRATCH, 8);
    a.jcc(Cc::G, failed);
    load_from_rsp(a, RAX, 40);
    sar_imm(a, RAX, 3);
    alu_rr(a, 0x01, SCRATCH, RAX);
    alu_r_imm(a, 0, SCRATCH, 16);
    load_from_rsp(a, RAX, 0);
    // x86 permits unaligned stores and is little-endian. No failure edge or
    // safepoint remains after this sole write to the untraced byte payload.
    store_mem64_disp(a, SCRATCH, 0, RAX);
    mov_imm64(a, RAX, egcl_rt::value::T.to_raw() as i64);
    store_to_rsp(a, RAX, 0);
    a.jmp(done);
    a.bind(failed);
    mov_imm64(a, RAX, egcl_rt::value::NIL.to_raw() as i64);
    store_to_rsp(a, RAX, 0);
    mov_imm64(a, RAX, 0);
    store_to_rsp(a, RAX, 8);
    a.bind(done);
    mov_imm64(a, SCRATCH, 0); // no interior pointer escapes in a scratch register
    load_from_rsp(a, final_dst, 8);
    load_from_rsp(a, success_dst, 0);
    alu_r_imm(a, 0, RSP, 56);
    Ok(())
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::t2::ir::{
        AuxData, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
    };
    use egcl_rt::object::{ElementTypeTag, ObjectHeader, type_id};
    use egcl_rt::value::{EgclVal, NIL, T};
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, Register};
    use std::collections::HashMap;

    fn kernel(outputs: [u8; 2], constants: [Option<u64>; 4]) -> egcl_rt::jit::JitBuffer {
        let mut f = Function::new("try-store-eight-octets-kernel");
        let block = f.entry();
        let args: Vec<_> = (0..4)
            .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let (inst, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TryStoreIntegerOctets8,
                args: args.clone(),
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged),
            ],
        );
        let mut a = Asm::new();
        // SysV argument 3 arrives in reserved RDX. R10 retains the out pointer,
        // while R9 carries end; every argument home is otherwise caller-save.
        mov_rr(&mut a, 10, 8);
        mov_rr(&mut a, 9, SCRATCH);
        let input_homes = [7, 6, 9, 1];
        for r in input_homes {
            push_reg(&mut a, r);
        }
        let mut registers: HashMap<_, _> = args.iter().copied().zip(input_homes).collect();
        let constants: HashMap<_, _> = args
            .iter()
            .copied()
            .zip(constants)
            .filter_map(|(v, c)| c.map(|bits| (v, bits)))
            .collect();
        for v in constants.keys() {
            registers.remove(v);
        }
        registers.insert(result[0], outputs[0]);
        registers.insert(result[1], outputs[1]);
        super::emit(
            &mut a,
            &f,
            f.inst(inst),
            &mut registers,
            &mut vec![],
            &constants,
        )
        .unwrap();
        store_mem64_disp(&mut a, 10, 0, outputs[0]);
        store_mem64_disp(&mut a, 10, 8, outputs[1]);
        let broken = a.label();
        let finish = a.label();
        for (i, r) in input_homes.into_iter().enumerate() {
            if !outputs.contains(&r) {
                load_from_rsp(&mut a, SCRATCH, ((3 - i) * 8) as i32);
                cmp_rr(&mut a, r, SCRATCH);
                a.jcc(Cc::Ne, broken);
            }
        }
        mov_imm64(&mut a, RAX, 1);
        a.jmp(finish);
        a.bind(broken);
        mov_imm64(&mut a, RAX, 0);
        a.bind(finish);
        store_mem64_disp(&mut a, 10, 16, RAX);
        alu_r_imm(&mut a, 0, 4, 32);
        a.extend_from_slice(&[0xc3]);
        let bytes = a.finish().unwrap();
        let decoded: Vec<_> = Decoder::new(64, &bytes, DecoderOptions::NONE)
            .into_iter()
            .collect();
        assert!(
            decoded.len() <= 256,
            "bounded straight-line word kernel: {} instructions",
            decoded.len()
        );
        assert!(
            decoded
                .iter()
                .all(|i| i.mnemonic() != Mnemonic::Call && !i.is_invalid())
        );
        assert!(
            decoded
                .iter()
                .filter(|i| matches!(
                    i.op0_kind(),
                    OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
                ))
                .all(|i| i.near_branch_target() > i.ip()),
            "the word kernel must not hide a byte loop"
        );
        assert_eq!(
            decoded
                .iter()
                .filter(|i| i.op0_kind() == OpKind::Memory && i.memory_base() == Register::RDX)
                .count(),
            1,
            "exactly one payload store"
        );
        egcl_rt::jit::JitBuffer::new(&bytes).unwrap()
    }

    fn run(code: &egcl_rt::jit::JitBuffer, args: [u64; 4]) -> [u64; 2] {
        let entry: extern "C" fn(u64, u64, u64, u64, *mut u64) =
            unsafe { std::mem::transmute(code.as_ptr()) };
        let mut output = [0; 3];
        entry(args[0], args[1], args[2], args[3], output.as_mut_ptr());
        assert_eq!(
            output[2], 1,
            "stack balance/non-output input homes must survive"
        );
        [output[0], output[1]]
    }
    fn fx(n: i64) -> u64 {
        EgclVal::from_fixnum(n).to_raw()
    }

    // Owned aligned fixtures use the exact runtime layout. They never enter
    // the moving heap, and no call/GC occurs while the native leaf reads them.
    struct Octets {
        words: Vec<u64>,
        data_offset: usize,
        length: usize,
    }
    impl Octets {
        fn new(length: usize, large: bool) -> Self {
            let header = if large { 16 } else { 8 };
            let bytes = header + 16 + length;
            let mut words = vec![0xa5a5_a5a5_a5a5_a5a5; bytes.div_ceil(8)];
            words[0] = ObjectHeader::new(
                type_id::SIMPLE_ARRAY,
                if large { 0xffff } else { words.len() as u16 },
            )
            .0;
            if large {
                words[1] = (words.len() * 8) as u64;
            }
            words[header / 8] = ElementTypeTag::U8 as u64;
            words[header / 8 + 1] = length as u64;
            Self {
                words,
                data_offset: header + 16,
                length,
            }
        }
        fn tagged(&mut self) -> u64 {
            unsafe { EgclVal::from_heap_ptr(self.words.as_mut_ptr().cast()).to_raw() }
        }
        fn image_with(&self, start: usize, raw: u64) -> Vec<u64> {
            let mut expected = self.words.clone();
            unsafe {
                std::slice::from_raw_parts_mut(
                    expected.as_mut_ptr().cast::<u8>(),
                    expected.len() * 8,
                )[self.data_offset + start..self.data_offset + start + 8]
                    .copy_from_slice(&raw.to_le_bytes());
            }
            expected
        }
    }
    #[repr(C, align(8))]
    struct Integer {
        header: ObjectHeader,
        sign: i32,
        count: u32,
        limbs: [u64; 2],
    }
    impl Integer {
        fn new(sign: i32, magnitude: u64) -> Self {
            Self {
                header: ObjectHeader::new(type_id::BIGNUM, 3),
                sign,
                count: 1,
                limbs: [magnitude, 1],
            }
        }
        fn tagged(&mut self) -> u64 {
            unsafe { EgclVal::from_heap_ptr((self as *mut Self).cast()).to_raw() }
        }
    }

    #[test]
    fn native_octet_word_store_handles_fixnums_unsigned_limbs_and_aliases() {
        for outputs in [[8, RAX], [7, 6], [1, 9], [RAX, SCRATCH], [SCRATCH, RAX]] {
            let code = kernel(outputs, [None; 4]);
            for (length, large) in [
                (8, false),
                (32, false),
                (524_248, false),
                (524_249, true),
                (600_000, true),
            ] {
                let mut octets = Octets::new(length, large);
                let mut starts = vec![0, octets.length - 8];
                if length > 8 {
                    starts.push(1);
                }
                for start in starts {
                    for integer in [
                        egcl_rt::bignum::FIXNUM_MIN,
                        -257,
                        -1,
                        0,
                        1,
                        257,
                        egcl_rt::bignum::FIXNUM_MAX,
                    ] {
                        let expected = octets.image_with(start, integer as u64);
                        let result = run(
                            &code,
                            [
                                octets.tagged(),
                                fx(start as i64),
                                fx(start as i64 + 8),
                                fx(integer),
                            ],
                        );
                        assert_eq!(result, [T.to_raw(), fx(if integer < 0 { -1 } else { 0 })]);
                        assert_eq!(
                            octets.words, expected,
                            "exact eight bytes, prefix/suffix/header unchanged"
                        );
                    }
                    for magnitude in [
                        1u64 << 60,
                        (1u64 << 60) + 1,
                        (1u64 << 63) - 1,
                        1u64 << 63,
                        u64::MAX,
                    ] {
                        for sign in [-1, 1] {
                            if sign < 0 && magnitude == 1u64 << 60 {
                                continue;
                            }
                            let mut integer = Integer::new(sign, magnitude);
                            let raw = if sign < 0 {
                                magnitude.wrapping_neg()
                            } else {
                                magnitude
                            };
                            let expected = octets.image_with(start, raw);
                            let result = run(
                                &code,
                                [
                                    octets.tagged(),
                                    fx(start as i64),
                                    fx(start as i64 + 8),
                                    integer.tagged(),
                                ],
                            );
                            assert_eq!(result, [T.to_raw(), fx(if sign < 0 { -1 } else { 0 })]);
                            assert_eq!(octets.words, expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn native_octet_word_store_rejects_partial_out_of_bounds_and_wrong_types_without_writes() {
        let code = kernel([8, RAX], [None; 4]);
        for (length, large) in [(0, false), (7, false), (16, false), (600_000, true)] {
            let mut octets = Octets::new(length, large);
            let before = octets.words.clone();
            for (start, end) in [
                (fx(0), fx(7)),
                (fx(0), fx(9)),
                (fx(1), fx(8)),
                (fx(-1), fx(7)),
                (fx(length as i64), fx(length as i64 + 8)),
                (fx(length as i64 - 7), fx(length as i64 + 1)),
                (
                    fx(egcl_rt::bignum::FIXNUM_MAX),
                    fx(egcl_rt::bignum::FIXNUM_MAX),
                ),
                (NIL.to_raw(), fx(8)),
                (fx(0), NIL.to_raw()),
            ] {
                assert_eq!(
                    run(&code, [octets.tagged(), start, end, fx(-1)]),
                    [NIL.to_raw(), fx(0)]
                );
                assert_eq!(octets.words, before);
            }
        }
        let mut octets = Octets::new(16, false);
        let before = octets.words.clone();
        for integer in [
            NIL.to_raw(),
            T.to_raw(),
            EgclVal::from_char('x').to_raw(),
            octets.tagged(),
        ] {
            assert_eq!(
                run(&code, [octets.tagged(), fx(0), fx(8), integer]),
                [NIL.to_raw(), fx(0)]
            );
            assert_eq!(octets.words, before);
        }
        for (sign, magnitude, count) in [
            (0, u64::MAX, 1),
            (2, u64::MAX, 1),
            (1, 0, 1),
            (1, 17, 1),
            (-1, 17, 1),
            (-1, 1u64 << 60, 1),
            (1, u64::MAX, 0),
            (1, u64::MAX, 2),
        ] {
            let mut integer = Integer::new(sign, magnitude);
            integer.count = count;
            assert_eq!(
                run(&code, [octets.tagged(), fx(0), fx(8), integer.tagged()]),
                [NIL.to_raw(), fx(0)]
            );
            assert_eq!(octets.words, before);
        }
        for array in [NIL.to_raw(), fx(0), EgclVal::from_char('x').to_raw()] {
            assert_eq!(
                run(&code, [array, fx(0), fx(8), fx(1)]),
                [NIL.to_raw(), fx(0)]
            );
        }
        for wrong_type in [type_id::SIMPLE_VECTOR, type_id::BIGNUM] {
            octets.words[0] = ObjectHeader::new(wrong_type, 5).0;
            let before = octets.words.clone();
            assert_eq!(
                run(&code, [octets.tagged(), fx(0), fx(8), fx(1)]),
                [NIL.to_raw(), fx(0)]
            );
            assert_eq!(octets.words, before);
        }
        octets.words[0] = ObjectHeader::new(type_id::SIMPLE_ARRAY, 5).0;
        octets.words[1] = ElementTypeTag::DoubleFloat as u64;
        let before = octets.words.clone();
        assert_eq!(
            run(&code, [octets.tagged(), fx(0), fx(8), fx(1)]),
            [NIL.to_raw(), fx(0)]
        );
        assert_eq!(octets.words, before);
    }

    #[test]
    fn native_octet_word_store_materializes_constants_and_preserves_aliases_on_failure() {
        let mut octets = Octets::new(24, false);
        let array = octets.tagged();
        for constants in [
            [None, Some(fx(1)), Some(fx(9)), Some(fx(-257))],
            [Some(array), Some(fx(1)), Some(fx(9)), Some(fx(-257))],
        ] {
            let code = kernel([7, 6], constants);
            let expected = octets.image_with(1, -257i64 as u64);
            let passed_array = if constants[0].is_some() {
                NIL.to_raw()
            } else {
                array
            };
            assert_eq!(
                run(
                    &code,
                    [passed_array, NIL.to_raw(), NIL.to_raw(), NIL.to_raw()]
                ),
                [T.to_raw(), fx(-1)]
            );
            assert_eq!(octets.words, expected);
        }
        let code = kernel([7, 6], [None, Some(fx(1)), Some(fx(8)), None]);
        let before = octets.words.clone();
        assert_eq!(
            run(&code, [array, fx(1), fx(8), fx(-257)]),
            [NIL.to_raw(), fx(0)]
        );
        assert_eq!(octets.words, before);
    }
    fn framed_kernel(
        constants: [Option<u64>; 4],
        aliases: bool,
        dead_final: bool,
    ) -> egcl_rt::jit::JitBuffer {
        let mut f = Function::new("octet-word-all-spilled-native-homes");
        let block = f.entry();
        let args: Vec<_> = (0..4)
            .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let (inst, results) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TryStoreIntegerOctets8,
                args: args.clone(),
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged),
            ],
        );
        let mut homes: HashMap<_, _> = args
            .iter()
            .copied()
            .enumerate()
            .map(|(i, v)| (v, FramedHome::Stack(i as u32)))
            .collect();
        let constants: HashMap<_, _> = args
            .iter()
            .copied()
            .zip(constants)
            .filter_map(|(v, c)| c.map(|bits| (v, bits)))
            .collect();
        for v in constants.keys() {
            homes.remove(v);
        }
        let success_home = FramedHome::Stack(if aliases { 0 } else { 4 });
        let final_home = FramedHome::Stack(if aliases { 3 } else { 5 });
        homes.insert(results[0], success_home);
        if !dead_final {
            homes.insert(results[1], final_home);
        }
        let mut a = Asm::new();
        mov_rr(&mut a, 10, 8); // out pointer
        mov_rr(&mut a, 9, SCRATCH); // preserve original end outside scratch
        alu_r_imm(&mut a, 5, 4, 96);
        let original = [7, 6, 9, 1];
        for (i, r) in original.into_iter().enumerate() {
            store_home(&mut a, FramedHome::Stack(i as u32), r, 0);
        }
        mov_imm64(&mut a, RAX, fx(777) as i64);
        store_home(&mut a, FramedHome::Stack(5), RAX, 0);
        mov_imm64(&mut a, RAX, fx(778) as i64);
        store_home(&mut a, FramedHome::Stack(11), RAX, 0);
        // Exercise load_home's immediately-preceding-store forwarding on
        // the first argument. Each later push must invalidate it by position.
        store_home(&mut a, FramedHome::Stack(0), 7, 0);
        super::emit_framed(&mut a, &f, f.inst(inst), &homes, &constants).unwrap();
        load_home(&mut a, RAX, success_home, 0);
        store_mem64_disp(&mut a, 10, 0, RAX);
        if dead_final {
            load_home(&mut a, RAX, FramedHome::Stack(5), 0);
        } else {
            load_home(&mut a, RAX, final_home, 0);
        }
        store_mem64_disp(&mut a, 10, 8, RAX);
        let broken = a.label();
        let finish = a.label();
        for (i, r) in original.into_iter().enumerate() {
            let home = FramedHome::Stack(i as u32);
            if home != success_home && (dead_final || home != final_home) {
                load_home(&mut a, SCRATCH, home, 0);
                cmp_rr(&mut a, r, SCRATCH);
                a.jcc(Cc::Ne, broken);
            }
        }
        load_home(&mut a, SCRATCH, FramedHome::Stack(11), 0);
        mov_imm64(&mut a, RAX, fx(778) as i64);
        cmp_rr(&mut a, RAX, SCRATCH);
        a.jcc(Cc::Ne, broken);
        mov_imm64(&mut a, RAX, 1);
        a.jmp(finish);
        a.bind(broken);
        mov_imm64(&mut a, RAX, 0);
        a.bind(finish);
        store_mem64_disp(&mut a, 10, 16, RAX);
        alu_r_imm(&mut a, 0, 4, 96);
        a.extend_from_slice(&[0xc3]);
        let bytes = a.finish().unwrap();
        assert!(
            Decoder::new(64, &bytes, DecoderOptions::NONE)
                .into_iter()
                .all(|i| i.mnemonic() != Mnemonic::Call && !i.is_invalid())
        );
        egcl_rt::jit::JitBuffer::new(&bytes).unwrap()
    }

    #[test]
    fn native_octet_word_direct_homes_cover_four_spills_aliases_constants_and_dead_final() {
        for aliases in [false, true] {
            for dead_final in [false, true] {
                for constants in [[None; 4], [None, Some(fx(1)), None, Some(fx(-257))]] {
                    let code = framed_kernel(constants, aliases, dead_final);
                    let mut octets = Octets::new(24, false);
                    // The mixed case deliberately passes invalid placeholders
                    // for constants: the native home materializer must ignore them.
                    let args = [
                        octets.tagged(),
                        if constants[1].is_some() {
                            NIL.to_raw()
                        } else {
                            fx(1)
                        },
                        fx(9),
                        if constants[3].is_some() {
                            NIL.to_raw()
                        } else {
                            fx(-257)
                        },
                    ];
                    let expected = octets.image_with(1, -257i64 as u64);
                    assert_eq!(
                        run(&code, args),
                        [T.to_raw(), if dead_final { fx(777) } else { fx(-1) }]
                    );
                    assert_eq!(octets.words, expected);
                    let before = octets.words.clone();
                    let mut bad = args;
                    bad[2] = fx(8); // partial span; no byte may be committed
                    assert_eq!(
                        run(&code, bad),
                        [NIL.to_raw(), if dead_final { fx(777) } else { fx(0) }]
                    );
                    assert_eq!(octets.words, before);
                }
            }
        }
    }
}
