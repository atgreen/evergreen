// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};

fn execute(moves: &[(FramedHome, HomeMoveSrc)], homes: &[FramedHome], initial: &[u64]) {
    assert_eq!(homes.len(), initial.len());
    let expected: Vec<_> = homes
        .iter()
        .map(|home| {
            moves.iter().find(|(dst, _)| dst == home).map_or(
                initial[homes.iter().position(|h| h == home).unwrap()],
                |(_, src)| match src {
                    HomeMoveSrc::Const(bits) => *bits,
                    HomeMoveSrc::Home(src) => initial[homes.iter().position(|h| h == src).unwrap()],
                },
            )
        })
        .collect();
    let mut a = Asm::new();
    mov_rr(&mut a, 2, 7); // retain output pointer in RDX, not a value home
    a.extend_from_slice(&[0x48, 0x83, 0xec, 72]);
    for (&home, &bits) in homes.iter().zip(initial) {
        mov_imm64(&mut a, RAX, bits as i64);
        store_home(&mut a, home, RAX, 0);
    }
    a.push(0xf9); // STC: every optimized edge move must preserve carry
    let start = a.here();
    parallel_home_move(&mut a, moves);
    let end = a.here();
    let prefix = a.clone().finish().unwrap();
    for instruction in Decoder::new(64, &prefix[start..end], DecoderOptions::NONE) {
        assert!(!instruction.is_invalid());
        assert!(
            !matches!(
                instruction.mnemonic(),
                Mnemonic::Call | Mnemonic::Sub | Mnemonic::Add | Mnemonic::Push | Mnemonic::Pop
            ),
            "eligible moves must not reserve a snapshot or call"
        );
    }
    for (index, &home) in homes.iter().enumerate() {
        load_home(&mut a, RAX, home, 0);
        store_mem64_disp(&mut a, 2, (index * 8) as i32, RAX);
    }
    // SETC byte [rdx+disp32] records carry without changing it.
    a.extend_from_slice(&[0x0f, 0x92, 0x82]);
    a.extend_from_slice(&((homes.len() * 8) as i32).to_le_bytes());
    a.extend_from_slice(&[0x48, 0x8d, 0x64, 0x24, 72, 0xc3]);
    let code = a.finish().unwrap();
    let executable = egcl_rt::jit::JitBuffer::new(&code).unwrap();
    let function: extern "C" fn(*mut u64) = unsafe { std::mem::transmute(executable.as_ptr()) };
    let mut result = vec![0; homes.len() + 1];
    function(result.as_mut_ptr());
    assert_eq!(&result[..homes.len()], expected);
    assert_eq!(result[homes.len()], 1, "CF must survive all moves");
}

#[test]
fn phi_cycles_preserve_native_bits_carry_and_fanout_without_stack_snapshots() {
    use FramedHome::{Reg as R, Stack as S};
    use HomeMoveSrc::{Const as C, Home as H};
    let bits = [
        0x7ff8_1234_5678_9abc,
        (-7i64) as u64,
        0xfedc_ba98_7654_3210,
        0x1234_5678_9abc_def0,
    ];
    execute(
        &[(R(1), H(R(8))), (R(8), H(R(1)))],
        &[R(1), R(8)],
        &bits[..2],
    );
    execute(
        &[(R(1), H(S(0))), (S(0), H(R(8))), (R(8), H(R(1)))],
        &[R(1), S(0), R(8)],
        &bits[..3],
    );
    execute(
        &[
            (R(1), H(R(8))),
            (R(8), H(R(1))),
            (S(0), H(S(1))),
            (S(1), H(S(0))),
        ],
        &[R(1), R(8), S(0), S(1)],
        &bits,
    );
    execute(
        &[(S(0), H(R(1))), (R(1), H(R(8))), (R(8), H(R(1)))],
        &[R(1), R(8), S(0)],
        &bits[..3],
    );
    execute(
        &[
            (S(0), C(bits[3])),
            (R(6), H(R(6))),
            (R(1), H(R(8))),
            (R(8), H(R(1))),
        ],
        &[R(1), R(8), S(0), R(6)],
        &bits,
    );
    // Equal referent bits do not collapse distinct physical homes.
    execute(
        &[(R(1), H(S(0))), (S(0), H(R(1)))],
        &[R(1), S(0)],
        &[bits[0]; 2],
    );
}

#[test]
fn unsupported_cycles_leave_assembler_untouched_for_snapshot_fallback() {
    use FramedHome::{Reg as R, Stack as S};
    use HomeMoveSrc::{Const as C, Home as H};
    for moves in [
        vec![(R(1), H(R(8))), (R(1), H(R(1)))], // duplicate destination
        vec![(R(1), H(R(8))), (S(0), H(R(8)))], // duplicate source
        vec![(R(1), H(R(8)))],                  // not a closed permutation
        vec![(R(1), C(8))],
        vec![(R(7), H(R(1))), (R(1), H(R(7)))], // reserved cycle scratch
        vec![(R(0), H(R(1))), (R(1), H(R(0)))], // ordinary move scratch
        vec![(S(u32::MAX), H(R(1))), (R(1), H(S(u32::MAX)))],
    ] {
        let mut a = Asm::new();
        a.push(0x90);
        assert!(!parallel_home_move_cycles(&mut a, &moves));
        assert_eq!(a.finish().unwrap(), [0x90]);
    }
    let mut snapshot = Asm::new();
    parallel_home_move_snapshot(&mut snapshot, &[(R(1), H(R(8))), (R(8), H(R(1)))]);
    assert!(
        snapshot.here() > 9,
        "old snapshot remains available and costs more than three register MOVs"
    );
}
