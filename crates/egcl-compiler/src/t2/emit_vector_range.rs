// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A noncollecting small simple-vector range test. The temporary stack holds
//! tagged snapshots only while this call-free template runs; no interior
//! pointer escapes and no input register is modified.

use super::*;

pub(super) fn emit(
    a: &mut Asm,
    f: &Function,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode, ValueRepresentation as Repr};
    const RSP: u8 = 4;
    if data.opcode != Opcode::SmallVectorRangeP
        || data.args.len() != 3
        || data.results.len() != 1
        || !matches!(data.aux, AuxData::None)
        || data.flags != crate::t2::ir::InstFlags::default()
        || data.frame_state.is_some()
        || !data.targets.is_empty()
        || data
            .args
            .iter()
            .chain(&data.results)
            .any(|&v| f.value(v).repr != Repr::Tagged)
        || data.args.iter().any(|v| {
            if let Some(&r) = reg.get(v) {
                matches!(r, RAX | SCRATCH | RSP)
            } else {
                !constants.contains_key(v)
            }
        })
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let dst = framed_alloc(reg, pool, data.results[0])?;
    let failed = a.label();
    let done = a.label();
    // Snapshot before clobbering the output, which may share an
    // input's home. After these pushes: end [rsp], start [rsp+8], vector [rsp+16].
    for input in &data.args {
        if let Some(&source) = reg.get(input) {
            mov_rr(a, RAX, source);
        } else {
            mov_imm64(a, RAX, constants[input] as i64);
        }
        push_reg(a, RAX);
    }
    load_from_rsp(a, RAX, 16);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 4, RAX, -8);
    // A valid heap object can be read only after checking its tag. The small
    // layout has raw length at +8 and elements at +16; large headers differ.
    a.extend_from_slice(&[0x80, 0x78, 0x07, egcl_rt::object::type_id::SIMPLE_VECTOR]);
    a.jcc(Cc::Ne, failed);
    a.extend_from_slice(&[0x66, 0x81, 0x38, 0xff, 0xff]); // cmp word [rax],0xffff
    a.jcc(Cc::E, failed);

    load_from_rsp(a, RAX, 8);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    a.jcc(Cc::Ne, failed);
    alu_r_imm(a, 7, RAX, 0);
    a.jcc(Cc::L, failed);
    load_from_rsp(a, RAX, 0);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    a.jcc(Cc::Ne, failed);
    // Both endpoints are fixnums. Tagged order is signed integer order;
    // end >= start >= 0 also proves end nonnegative.
    load_from_rsp(a, SCRATCH, 8);
    cmp_rr(a, RAX, SCRATCH);
    a.jcc(Cc::L, failed);
    sar_imm(a, RAX, 3);
    load_from_rsp(a, SCRATCH, 16);
    alu_r_imm(a, 4, SCRATCH, -8);
    // Small-object length is bounded by the u16 header, hence signed-safe.
    cmp_mem64_disp8(a, RAX, SCRATCH, 8);
    a.jcc(Cc::G, failed);
    mov_imm64(a, RAX, egcl_rt::value::T.to_raw() as i64);
    a.jmp(done);
    a.bind(failed);
    mov_imm64(a, RAX, egcl_rt::value::NIL.to_raw() as i64);
    a.bind(done);
    alu_r_imm(a, 0, RSP, 24);
    mov_rr(a, dst, RAX);
    Ok(())
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::t2::ir::{AuxData, IRType, InstData, InstFlags, Opcode, ValueRepresentation};
    use egcl_rt::object::{type_id, ObjectHeader};
    use egcl_rt::value::{EgclVal, NIL, T};
    use std::collections::HashMap;

    fn kernel(constant_start: Option<i64>) -> egcl_rt::jit::JitBuffer {
        let mut f = Function::new("small-vector-range-machine-test");
        let entry = f.entry();
        let args: Vec<_> = (0..3)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let (inst, result) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::SmallVectorRangeP,
                args: args.clone(),
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let mut a = Asm::new();
        // SysV's third argument arrives in RDX, which the template reserves.
        mov_rr(&mut a, 1, SCRATCH);
        for r in [7, 6, 1] {
            push_reg(&mut a, r);
        }
        let mut regs = HashMap::from([(args[0], 7), (args[1], 6), (args[2], 1), (result[0], RAX)]);
        let mut constants = HashMap::new();
        if let Some(start) = constant_start {
            regs.remove(&args[1]);
            constants.insert(args[1], EgclVal::from_fixnum(start).to_raw());
        }
        super::emit(&mut a, &f, f.inst(inst), &mut regs, &mut vec![], &constants)
            .expect("emit range predicate");
        // Verify all non-scratch input registers survived and the temporary
        // stack is balanced, including every failing path.
        let broken = a.label();
        let finish = a.label();
        for (r, offset) in [(1, 0), (6, 8), (7, 16)] {
            load_from_rsp(&mut a, SCRATCH, i32::from(offset));
            cmp_rr(&mut a, r, SCRATCH);
            a.jcc(Cc::Ne, broken);
        }
        a.jmp(finish);
        a.bind(broken);
        mov_imm64(&mut a, RAX, -1);
        a.bind(finish);
        alu_r_imm(&mut a, 0, 4, 24);
        a.extend_from_slice(&[0xc3]);
        egcl_rt::jit::JitBuffer::new(&a.finish().expect("labels")).expect("executable kernel")
    }

    fn run(code: &egcl_rt::jit::JitBuffer, vector: u64, start: u64, end: u64) -> u64 {
        let leaf: extern "C" fn(u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr()) };
        leaf(vector, start, end)
    }

    // These fixtures are owned aligned objects, never GC allocations. The
    // native predicate reads only the header and length, never elements.
    #[repr(C, align(8))]
    struct VectorFixture {
        header: ObjectHeader,
        length: u64,
    }
    impl VectorFixture {
        fn tagged(&mut self) -> u64 {
            unsafe { EgclVal::from_heap_ptr((self as *mut Self).cast()).to_raw() }
        }
    }
    fn fx(n: i64) -> u64 {
        EgclVal::from_fixnum(n).to_raw()
    }

    #[test]
    fn native_small_vector_ranges_cover_endpoints_and_header_boundary() {
        let code = kernel(None);
        for length in [0, 1, 8, 65_532] {
            let mut vector = VectorFixture {
                header: ObjectHeader::new(type_id::SIMPLE_VECTOR, (length + 2) as u16),
                length,
            };
            let vector = vector.tagged();
            for (start, end, expected) in [
                (0, 0, true),
                (0, length as i64, true),
                (length as i64, length as i64, true),
                (-1, 0, false),
                (0, -1, false),
                (1, 0, false),
                (0, length as i64 + 1, false),
                (length as i64 + 1, length as i64 + 1, false),
                (0, egcl_rt::bignum::FIXNUM_MAX, false),
                (egcl_rt::bignum::FIXNUM_MIN, 0, false),
            ] {
                assert_eq!(
                    run(&code, vector, fx(start), fx(end)),
                    if expected { T.to_raw() } else { NIL.to_raw() },
                    "length={length}, start={start}, end={end}"
                );
            }
        }
        let code = kernel(Some(0));
        let mut vector = VectorFixture {
            header: ObjectHeader::new(type_id::SIMPLE_VECTOR, 10),
            length: 8,
        };
        assert_eq!(run(&code, vector.tagged(), NIL.to_raw(), fx(8)), T.to_raw());
    }

    #[test]
    fn native_small_vector_range_rejects_types_and_large_header() {
        let code = kernel(None);
        let mut vector = VectorFixture {
            header: ObjectHeader::new(type_id::SIMPLE_VECTOR, 10),
            length: 8,
        };
        let vector_bits = vector.tagged();
        for wrong in [
            NIL.to_raw(),
            T.to_raw(),
            fx(0),
            EgclVal::from_char('x').to_raw(),
            EgclVal::from_single_float(1.0).to_raw(),
        ] {
            assert_eq!(run(&code, wrong, fx(0), fx(0)), NIL.to_raw());
            assert_eq!(run(&code, vector_bits, wrong | 1, fx(8)), NIL.to_raw());
            assert_eq!(run(&code, vector_bits, fx(0), wrong | 1), NIL.to_raw());
        }
        for object_type in [
            type_id::DOUBLE_FLOAT,
            type_id::BIGNUM,
            type_id::SIMPLE_ARRAY,
        ] {
            let mut wrong = VectorFixture {
                header: ObjectHeader::new(object_type, 10),
                length: 8,
            };
            assert_eq!(run(&code, wrong.tagged(), fx(0), fx(0)), NIL.to_raw());
        }
        // Reject the large sentinel without reading an extended body/length.
        vector.header = ObjectHeader::new(type_id::SIMPLE_VECTOR, 0xffff);
        vector.length = 0;
        assert_eq!(run(&code, vector.tagged(), fx(0), fx(0)), NIL.to_raw());
    }
}
