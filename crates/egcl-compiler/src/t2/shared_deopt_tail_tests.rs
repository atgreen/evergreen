// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

static RETURNED: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

#[test]
fn shared_return_preserves_result_registers_carry_and_unique_call_maps() {
    // This noncollecting machine veneer deliberately sets every ordinary
    // return-channel register and CF, independent of the host C ABI's flags.
    let mut veneer = Asm::new();
    mov_imm64(&mut veneer, RAX, 777);
    mov_imm64(&mut veneer, 2, 0x1234_5678_9abc_def0);
    mov_imm64(&mut veneer, 1, 3);
    veneer.extend_from_slice(&[0xf9, 0xc3]); // STC; RET
    let veneer_bytes = veneer.finish().unwrap();
    let veneer_buffer = egcl_rt::jit::JitBuffer::new(&veneer_bytes).unwrap();
    let veneer_address = veneer_buffer.as_ptr() as usize as u64;
    let mut a = Asm::new();
    a.extend_from_slice(&[0x55, 0x53, 0x48, 0x83, 0xec, 8]);
    mov_imm64(&mut a, RAX, 0x555);
    store_to_rsp(&mut a, RAX, 0);
    let first = a.label();
    let second = a.label();
    alu_r_imm(&mut a, 7, 7, 0);
    a.jcc(Cc::E, first);
    a.jmp(second);
    let sites: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, label)| Site {
            label,
            frame_state: FrameStateId(index as u32),
            n_scopes: 1,
            sources: vec![Source::Imm(index as u64)],
            f64_slots: Box::new([]),
        })
        .collect();
    let candidate = emit_candidate(a, &sites, 1, FramedHome::Stack(0), veneer_address, |a| {
        // Capture CF before any flag-writing instruction. All subsequent MOVs,
        // MOVZX, LEA and POP preserve the original CF and return registers.
        a.extend_from_slice(&[0x41, 0x0f, 0x92, 0xc2, 0x4d, 0x0f, 0xb6, 0xd2]);
        for (index, store) in [
            [0x49, 0x89, 0x03],
            [0x49, 0x89, 0x13],
            [0x49, 0x89, 0x0b],
            [0x4d, 0x89, 0x13],
        ]
        .into_iter()
        .enumerate()
        {
            mov_imm64(a, 11, &RETURNED[index] as *const AtomicU64 as usize as i64);
            a.extend_from_slice(&store);
        }
        a.extend_from_slice(&[0x48, 0x8d, 0x64, 0x24, 8, 0x5b, 0x5d]);
    })
    .unwrap();
    let returns: Vec<_> = candidate
        .calls
        .iter()
        .map(|call| call.return_offset)
        .collect();
    assert_eq!(returns.len(), 2);
    assert_ne!(returns[0], returns[1]);
    for (index, call) in candidate.calls.iter().enumerate() {
        assert_eq!(
            call.origin,
            NativeCallOrigin::Deopt(FrameStateId(index as u32))
        );
        assert_eq!(call.stack_adjust, 48);
        assert!(
            matches!(&candidate.values[&call.return_offset], NativeCallValues::Deoptimizing { native_slots: 1, f64_slots } if f64_slots.is_empty())
        );
    }
    let helper = candidate.helper_start..candidate.helper_end;
    let data_offset = candidate.data_offset;
    let bytes = candidate.asm.finish().unwrap();
    let instructions: Vec<_> =
        iced_x86::Decoder::new(64, &bytes[..data_offset], iced_x86::DecoderOptions::NONE)
            .into_iter()
            .collect();
    assert_eq!(
        instructions
            .iter()
            .filter(|i| i.mnemonic() == iced_x86::Mnemonic::Call)
            .count(),
        2
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|i| i.mnemonic() == iced_x86::Mnemonic::Ret)
            .count(),
        1,
        "one shared return block"
    );
    let helper_instructions: Vec<_> =
        iced_x86::Decoder::new(64, &bytes[helper], iced_x86::DecoderOptions::NONE)
            .into_iter()
            .collect();
    assert!(
        !helper_instructions
            .iter()
            .any(|i| i.mnemonic() == iced_x86::Mnemonic::Call)
    );
    assert_eq!(
        helper_instructions
            .iter()
            .filter(|i| i.mnemonic() == iced_x86::Mnemonic::Mov
                && i.op0_register() == iced_x86::Register::RAX
                && i.op1_kind() == iced_x86::OpKind::Immediate64
                && i.immediate64() == veneer_address)
            .count(),
        1
    );
    let mut common_target = None;
    for offset in returns {
        let instruction = iced_x86::Decoder::with_ip(
            64,
            &bytes[offset..],
            offset as u64,
            iced_x86::DecoderOptions::NONE,
        )
        .decode();
        assert_eq!(instruction.code(), iced_x86::Code::Jmp_rel32_64);
        let target = instruction.near_branch_target();
        assert!(target < data_offset as u64);
        assert_eq!(*common_target.get_or_insert(target), target);
    }
    let buffer = egcl_rt::jit::JitBuffer::new(&bytes).unwrap();
    let function: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    for path in [0, 1] {
        assert_eq!(function(path), 777);
        assert_eq!(
            RETURNED.each_ref().map(|v| v.load(Ordering::Relaxed)),
            [777, 0x1234_5678_9abc_def0, 3, 1]
        );
    }
}
