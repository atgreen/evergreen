// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::t2::ir::Opcode;

fn arithmetic(opcode: Opcode, slots: [u32; 3], memory: bool) -> egcl_rt::jit::JitBuffer {
    let mut a = Asm::new();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // ENDBR64
    alu_r_imm(&mut a, 5, 4, 288); // SUB RSP, frame bytes
    for (offset, slot) in [(0, 0), (8, 33)] {
        load_mem64_disp(&mut a, RAX, 7, offset);
        store_home(&mut a, FramedHome::Stack(slot), RAX, 0);
    }
    if memory {
        let start = a.here();
        emit_stack_float(&mut a, opcode, slots, false);
        assert_eq!(
            a.here() - start,
            27,
            "three SSE2 instructions with disp32 homes"
        );
    } else {
        load_home(&mut a, RAX, FramedHome::Stack(slots[0]), 0);
        load_home(&mut a, 2, FramedHome::Stack(slots[1]), 0);
        a.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0]); // MOVQ XMM0,RAX
        a.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xca]); // MOVQ XMM1,RDX
        a.extend_from_slice(&[
            0xf2,
            0x0f,
            if opcode == Opcode::FloatAdd {
                0x58
            } else {
                0x59
            },
            0xc1,
        ]);
        a.extend_from_slice(&[0x66, 0x48, 0x0f, 0x7e, 0xc0]); // MOVQ RAX,XMM0
        store_home(&mut a, FramedHome::Stack(slots[2]), RAX, 0);
    }
    load_home(&mut a, RAX, FramedHome::Stack(slots[2]), 0);
    alu_r_imm(&mut a, 0, 4, 288); // ADD RSP, frame bytes
    a.push(0xc3);
    egcl_rt::jit::JitBuffer::new(&a.finish().unwrap()).unwrap()
}

#[test]
fn stack_float_matches_register_sse_for_aliases_and_special_values() {
    let values = [
        0,
        (-0.0f64).to_bits(),
        1.25f64.to_bits(),
        (-2.5f64).to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        1,
        0x7ff8_0000_0000_0042,
        0xfff8_0000_0000_0071,
        0x7ff0_0000_0000_0001,
    ];
    for opcode in [Opcode::FloatAdd, Opcode::FloatMul] {
        for slots in [[0, 33, 34], [0, 33, 0], [0, 33, 33], [0, 0, 34], [0, 0, 0]] {
            let memory = arithmetic(opcode, slots, true);
            let register = arithmetic(opcode, slots, false);
            let candidate: unsafe extern "C" fn(*const u64) -> u64 =
                unsafe { std::mem::transmute(memory.as_ptr()) };
            let reference: unsafe extern "C" fn(*const u64) -> u64 =
                unsafe { std::mem::transmute(register.as_ptr()) };
            for lhs in values {
                for rhs in values {
                    let args = [lhs, rhs];
                    assert_eq!(
                        unsafe { candidate(args.as_ptr()) },
                        unsafe { reference(args.as_ptr()) },
                        "{opcode:?} {slots:?} {lhs:x} {rhs:x}"
                    );
                }
            }
        }
    }
}
