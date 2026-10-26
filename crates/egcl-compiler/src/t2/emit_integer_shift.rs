// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Nonallocating one-limb ASH probe. No pointer or borrow escapes the template.
use super::*;

pub(super) fn emit(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    registers: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode};
    let AuxData::FixnumImm(bits) = data.aux else {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    };
    if data.opcode != Opcode::TryIntegerShr
        || data.args.len() != 1
        || data.results.len() != 2
        || bits <= 0
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let input = data.args[0];
    if let Some(&word) = constants.get(&input) {
        mov_imm64(a, RAX, word as i64);
    } else {
        mov_rr(
            a,
            RAX,
            *registers
                .get(&input)
                .ok_or(EmitError::UnsupportedOp(0xF2))?,
        );
    }
    let failed = a.label();
    let fixnum = a.label();
    let positive = a.label();
    let success = a.label();
    let done = a.label();
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, 0);
    a.jcc(Cc::E, fixnum);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 4, RAX, -8);
    load_mem64_disp(a, SCRATCH, RAX, 0);
    shr_imm(a, SCRATCH, 56);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::object::type_id::BIGNUM as i32);
    a.jcc(Cc::Ne, failed);
    a.extend_from_slice(&[0x83, 0x78, 0x0c, 0x01]); // cmp n_limbs,1
    a.jcc(Cc::Ne, failed);
    // RDI retains sign, RDX discarded bits, RAX the magnitude/answer.
    a.extend_from_slice(&[0x48, 0x63, 0x78, 0x08]); // movsxd rdi,[rax+8]
    load_mem64_disp(a, RAX, RAX, 16);
    alu_r_imm(a, 7, RAX, 0);
    a.jcc(Cc::E, failed);
    alu_r_imm(a, 7, 7, 1);
    a.jcc(Cc::E, positive);
    alu_r_imm(a, 7, 7, -1);
    a.jcc(Cc::Ne, failed);
    if bits >= 64 {
        mov_imm64(a, RAX, -1);
    } else {
        mov_imm64(a, SCRATCH, ((1u64 << bits) - 1) as i64);
        alu_rr(a, 0x21, SCRATCH, RAX);
        shr_imm(a, RAX, bits as u8);
        neg_r(a, RAX);
        let exact = a.label();
        alu_r_imm(a, 7, SCRATCH, 0);
        a.jcc(Cc::E, exact);
        alu_r_imm(a, 5, RAX, 1); // sub rax,1: negative division rounds down
        a.bind(exact);
        if bits < 4 {
            mov_imm64(a, SCRATCH, FIXNUM_MIN);
            cmp_rr(a, RAX, SCRATCH);
            a.jcc(Cc::L, failed);
        }
    }
    a.jmp(success);
    a.bind(positive);
    if bits >= 64 {
        mov_imm64(a, RAX, 0);
    } else {
        shr_imm(a, RAX, bits as u8);
        if bits < 4 {
            mov_imm64(a, SCRATCH, FIXNUM_MAX);
            cmp_rr(a, RAX, SCRATCH);
            a.jcc(Cc::G, failed);
        }
    }
    a.jmp(success);
    a.bind(fixnum);
    sar_imm(a, RAX, 3);
    sar_imm(a, RAX, bits.min(63) as u8);
    a.bind(success);
    shl_imm(a, RAX, 3);
    mov_imm64(a, SCRATCH, egcl_rt::value::T.0 as i64);
    a.jmp(done);
    a.bind(failed);
    mov_imm64(a, RAX, 0);
    mov_imm64(a, SCRATCH, egcl_rt::value::NIL.0 as i64);
    a.bind(done);
    mov_rr(a, registers[&data.results[1]], RAX);
    mov_rr(a, registers[&data.results[0]], SCRATCH);
    Ok(())
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::t2::ir::{
        AuxData, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation as Repr,
    };
    use egcl_rt::value::EgclVal;
    use std::collections::HashMap;

    fn kernel(bits: i64) -> egcl_rt::jit::JitBuffer {
        let mut f = Function::new("try-integer-right-shift-kernel");
        let block = f.entry();
        let input = f.add_block_param(block, IRType::TOP, Repr::Tagged);
        let (inst, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TryIntegerShr,
                args: vec![input],
                results: vec![],
                aux: AuxData::FixnumImm(bits),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    Repr::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), Repr::Tagged),
            ],
        );
        let registers = HashMap::from([(input, 7), (result[0], 8), (result[1], RAX)]);
        let mut a = Asm::new();
        super::emit(&mut a, f.inst(inst), &registers, &HashMap::new()).unwrap();
        // SysV second argument RSI points to the success flag, separate from
        // the returned value. The probe has no calls or safepoints.
        store_mem64_disp(&mut a, 6, 0, 8);
        a.extend_from_slice(&[0xc3]);
        let code = a.finish().unwrap();
        assert!(
            iced_x86::Decoder::new(64, &code, iced_x86::DecoderOptions::NONE)
                .into_iter()
                .all(|instruction| instruction.mnemonic() != iced_x86::Mnemonic::Call),
            "the shift probe must not call an allocating/signaling helper"
        );
        egcl_rt::jit::JitBuffer::new(&code).unwrap()
    }
    fn run(code: &egcl_rt::jit::JitBuffer, value: EgclVal) -> Option<EgclVal> {
        let f: extern "C" fn(u64, *mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
        let mut success = 0;
        let result = EgclVal(f(value.0, &mut success));
        if success == egcl_rt::value::NIL.0 {
            assert_eq!(
                result,
                EgclVal::from_fixnum(0),
                "failure still defines a real fixnum"
            );
            None
        } else {
            assert_eq!(success, egcl_rt::value::T.0);
            Some(result)
        }
    }
    #[test]
    fn native_shift_probe_executes_fixnum_and_full_unsigned_limb_boundaries() {
        use egcl_rt::bignum::{BigInt, FIXNUM_MAX, FIXNUM_MIN};
        for bits in [1i64, 2, 3, 4, 8, 60, 63, 64, 65, 1 << 60] {
            let code = kernel(bits);
            for integer in [FIXNUM_MIN, -257, -1, 0, 1, 257, FIXNUM_MAX] {
                assert_eq!(
                    run(&code, EgclVal::from_fixnum(integer))
                        .unwrap()
                        .as_fixnum(),
                    integer >> bits.min(63)
                );
            }
            for magnitude in [1u64 << 60, (1u64 << 63) - 1, 1u64 << 63, u64::MAX] {
                for sign in [-1i8, 1] {
                    egcl_rt::rooted!(value = BigInt::from_mag(sign, vec![magnitude]).to_val());
                    let expected = (i128::from(magnitude) * i128::from(sign)) >> bits.min(127);
                    let actual = run(&code, *value);
                    if (i128::from(FIXNUM_MIN)..=i128::from(FIXNUM_MAX)).contains(&expected) {
                        assert_eq!(actual.unwrap().as_fixnum() as i128, expected);
                    } else {
                        assert!(actual.is_none(), "wide result must take the generic branch");
                    }
                }
            }
        }
    }
    #[test]
    fn native_shift_probe_rejects_other_layouts_without_allocating_or_signaling() {
        let code = kernel(8);
        for value in [
            egcl_rt::value::NIL,
            EgclVal::from_char('x'),
            EgclVal::from_single_float(3.0),
        ] {
            assert!(run(&code, value).is_none());
        }
        egcl_rt::rooted!(large = egcl_rt::bignum::BigInt::from_mag(1, vec![0, 1]).to_val());
        assert!(run(&code, *large).is_none());
        egcl_rt::rooted!(not_integer = egcl_rt::gc::alloc_double_float(5.0));
        assert!(run(&code, *not_integer).is_none());
        egcl_rt::rooted!(bad_sign = egcl_rt::bignum::alloc_bignum_cli(0, &[1]));
        assert!(run(&code, *bad_sign).is_none());
        egcl_rt::rooted!(bad_zero = egcl_rt::bignum::alloc_bignum_cli(1, &[0]));
        assert!(run(&code, *bad_zero).is_none());
        for _ in 0..100 {
            assert!(run(&code, *large).is_none());
        }
    }
}
