// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native low-bit extraction. No helper call, allocation or safepoint occurs.
//! The low bits of -magnitude are simply the low bits of its negation, even
//! for multi-limb bignums: higher limbs are multiples of 2^64.

use super::*;

pub(super) fn emit(
    a: &mut Asm,
    f: &Function,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode, ValueDef, ValueRepresentation as Repr};
    let AuxData::FixnumImm(mask) = data.aux else {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    };
    if data.opcode != Opcode::IntegerLowBits
        || data.args.len() != 1
        || data.results.len() != 1
        || !(0..=FIXNUM_MAX).contains(&mask)
        || !data.flags.guard
        || data.frame_state.is_none()
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let input = data.args[0];
    let result = data.results[0];
    let input_repr = f.value(input).repr;
    let result_repr = f.value(result).repr;
    if !matches!(input_repr, Repr::Tagged | Repr::UnboxedFixnum)
        || !matches!(result_repr, Repr::Tagged | Repr::UnboxedFixnum)
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    if let Some(&source) = reg.get(&input) {
        mov_rr(a, RAX, source);
    } else if let Some(&bits) = constants.get(&input) {
        if input_repr == Repr::UnboxedFixnum {
            let ValueDef::Result { inst, .. } = f.value(input).def else {
                return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
            };
            let AuxData::FixnumImm(value) = f.inst(inst).aux else {
                return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
            };
            mov_imm64(a, RAX, value);
        } else {
            mov_imm64(a, RAX, bits as i64);
        }
    } else {
        return Err(EmitError::UnsupportedOp(0xF2));
    }

    let mask_result = a.label();
    if input_repr == Repr::Tagged {
        let fixnum = a.label();
        let positive = a.label();
        mov_rr(a, SCRATCH, RAX);
        alu_r_imm(a, 4, SCRATCH, 7);
        alu_r_imm(a, 7, SCRATCH, 0);
        a.jcc(Cc::E, fixnum);
        alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
        a.jcc(Cc::Ne, deopt);
        alu_r_imm(a, 4, RAX, -8);
        load_mem64_disp(a, SCRATCH, RAX, 0);
        shr_imm(a, SCRATCH, 56);
        alu_r_imm(a, 7, SCRATCH, egcl_rt::object::type_id::BIGNUM as i32);
        a.jcc(Cc::Ne, deopt);
        // A canonical bignum has at least one limb. Zero is a fixnum.
        a.extend_from_slice(&[0x83, 0x78, 0x0c, 0x00]); // cmp dword [rax+12],0
        a.jcc(Cc::E, deopt);
        a.extend_from_slice(&[0x83, 0x78, 0x08, 0x01]); // cmp sign,1
        a.jcc(Cc::E, positive);
        a.extend_from_slice(&[0x83, 0x78, 0x08, 0xff]); // cmp sign,-1
        a.jcc(Cc::Ne, deopt);
        load_mem64_disp(a, RAX, RAX, 16);
        neg_r(a, RAX);
        a.jmp(mask_result);
        a.bind(positive);
        load_mem64_disp(a, RAX, RAX, 16);
        a.jmp(mask_result);
        a.bind(fixnum);
        sar_imm(a, RAX, 3);
    }
    a.bind(mask_result);
    mov_imm64(a, SCRATCH, mask);
    alu_rr(a, 0x21, RAX, SCRATCH); // and rax,rdx
    if result_repr == Repr::Tagged {
        shl_imm(a, RAX, 3);
    }
    let dst = framed_alloc(reg, pool, result)?;
    mov_rr(a, dst, RAX);
    Ok(())
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameState;
    use crate::t2::ir::{
        AuxData, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation as Repr,
    };
    use egcl_rt::object::{ObjectHeader, type_id};
    use egcl_rt::value::EgclVal;
    use std::collections::HashMap;

    const DEOPT: u64 = u64::MAX;

    fn kernel(width: u32, input_repr: Repr, result_repr: Repr) -> egcl_rt::jit::JitBuffer {
        let mut f = Function::new("integer-low-bits-machine-test");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, input_repr);
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (inst, output) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::IntegerLowBits,
                args: vec![input],
                results: vec![],
                aux: AuxData::FixnumImm(((1u64 << width) - 1) as i64),
                flags: InstFlags {
                    effectful: true,
                    guard: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), result_repr)],
        );
        let mut asm = Asm::new();
        let deopt = asm.label();
        // SysV leaf: RDI is the argument, RAX the result; the template's RDX
        // scratch is caller-save. No generated path calls or allocates.
        let mut regs = HashMap::from([(input, 7), (output[0], RAX)]);
        super::emit(
            &mut asm,
            &f,
            f.inst(inst),
            &mut regs,
            &mut vec![],
            &HashMap::new(),
            deopt,
        )
        .expect("emit extraction template");
        asm.extend_from_slice(&[0xc3]);
        asm.bind(deopt);
        mov_imm64(&mut asm, RAX, DEOPT as i64);
        asm.extend_from_slice(&[0xc3]);
        egcl_rt::jit::JitBuffer::new(&asm.finish().expect("resolve labels"))
            .expect("executable leaf")
    }

    fn run(code: &egcl_rt::jit::JitBuffer, input: u64) -> u64 {
        let leaf: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
        leaf(input)
    }

    fn expected(bits: u64, width: u32, repr: Repr) -> u64 {
        let result = bits & ((1u64 << width) - 1);
        if repr == Repr::Tagged {
            result << 3
        } else {
            result
        }
    }

    #[test]
    fn fixnum_machine_extraction_handles_all_representation_pairs_and_limits() {
        for width in [0, 8, 60] {
            for input_repr in [Repr::Tagged, Repr::UnboxedFixnum] {
                for output_repr in [Repr::Tagged, Repr::UnboxedFixnum] {
                    let code = kernel(width, input_repr, output_repr);
                    for integer in [
                        egcl_rt::bignum::FIXNUM_MIN,
                        -257,
                        -256,
                        -1,
                        0,
                        1,
                        255,
                        256,
                        egcl_rt::bignum::FIXNUM_MAX,
                    ] {
                        let input = if input_repr == Repr::Tagged {
                            EgclVal::from_fixnum(integer).to_raw()
                        } else {
                            integer as u64
                        };
                        assert_eq!(
                            run(&code, input),
                            expected(integer as u64, width, output_repr),
                            "width={width} in={input_repr:?} out={output_repr:?} integer={integer}"
                        );
                    }
                }
            }
        }
    }

    // Owned, aligned storage uses the real object header and canonical
    // bignum layout (header/sign/count/little-endian limbs). It never enters
    // the GC heap: no allocation or safepoint occurs while its pointer is
    // consumed, and moving-GC/root publication is exercised by integration.
    #[repr(C, align(8))]
    struct BignumFixture {
        header: ObjectHeader,
        sign: i32,
        n_limbs: u32,
        limbs: [u64; 3],
    }

    impl BignumFixture {
        fn new(sign: i32, low: u64) -> Self {
            Self {
                header: ObjectHeader::new(type_id::BIGNUM, 5),
                sign,
                n_limbs: 3,
                limbs: [low, 0x8000_0000_0000_0001, 1],
            }
        }
        fn tagged(&mut self) -> u64 {
            // The aligned fixture outlives the synchronous noncollecting leaf.
            unsafe { EgclVal::from_heap_ptr((self as *mut Self).cast()).to_raw() }
        }
    }

    #[test]
    fn signed_multilimb_machine_extraction_uses_low_limb_with_twos_complement_sign() {
        assert_eq!(std::mem::offset_of!(BignumFixture, sign), 8);
        assert_eq!(std::mem::offset_of!(BignumFixture, n_limbs), 12);
        assert_eq!(std::mem::offset_of!(BignumFixture, limbs), 16);
        for width in [0, 8, 60] {
            for output_repr in [Repr::Tagged, Repr::UnboxedFixnum] {
                let code = kernel(width, Repr::Tagged, output_repr);
                for low in [0, 1, 0x0123_4567_89ab_cdef, 1u64 << 63, u64::MAX] {
                    for sign in [-1, 1] {
                        let mut object = BignumFixture::new(sign, low);
                        let bits = if sign == -1 { low.wrapping_neg() } else { low };
                        assert_eq!(
                            run(&code, object.tagged()),
                            expected(bits, width, output_repr),
                            "width={width} out={output_repr:?} sign={sign} low={low:#x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_tagged_inputs_deopt_before_mask_even_for_width_zero() {
        for width in [0, 8, 60] {
            for output_repr in [Repr::Tagged, Repr::UnboxedFixnum] {
                let code = kernel(width, Repr::Tagged, output_repr);
                for input in [
                    egcl_rt::value::NIL.to_raw(),
                    EgclVal::from_char('x').to_raw(),
                    EgclVal::from_single_float(1.5).to_raw(),
                    EgclVal::from_symbol_index(42).to_raw(),
                ] {
                    assert_eq!(run(&code, input), DEOPT, "width={width} input={input:#x}");
                }
                let mut wrong_type = BignumFixture::new(1, 17);
                wrong_type.header = ObjectHeader::new(type_id::DOUBLE_FLOAT, 5);
                assert_eq!(run(&code, wrong_type.tagged()), DEOPT);
                let mut invalid_sign = BignumFixture::new(2, 17);
                assert_eq!(run(&code, invalid_sign.tagged()), DEOPT);
                let mut no_limbs = BignumFixture::new(1, 17);
                no_limbs.n_limbs = 0;
                assert_eq!(run(&code, no_limbs.tagged()), DEOPT);
            }
        }
    }
}
