// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use crate::t2::frame_state::FrameScope;
use crate::t2::ir::{AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits};
use egcl_rt::value::EgclVal;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNT: AtomicU64 = AtomicU64::new(0);
static CONTROL: AtomicU64 = AtomicU64::new(0);
static WORDS: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
extern "C" fn capture(request: *const OrdinaryGuardRequest) -> u64 {
    let request = unsafe { &*request };
    COUNT.store(request.n_words, Ordering::Relaxed);
    CONTROL.store(request.control as usize as u64, Ordering::Relaxed);
    if request.n_words <= 32 {
        for (slot, &word) in WORDS
            .iter()
            .zip(unsafe { std::slice::from_raw_parts(request.words, request.n_words as usize) })
        {
            slot.store(word, Ordering::Relaxed);
        }
    }
    EgclVal::from_fixnum(777).0
}
fn epilogue(a: &mut Asm) {
    a.extend_from_slice(&[0x48, 0x8d, 0x64, 0x24, 40, 0x5b, 0x5d]);
}

#[test]
fn shared_serializer_executes_mixed_homes_and_preserves_exact_call_maps() {
    let mut a = Asm::new();
    // SysV entry is 8 mod16; two saved GPRs, 8 padding, 32 body bytes.
    a.extend_from_slice(&[0x55, 0x53, 0x48, 0x83, 0xec, 40]);
    mov_imm64(&mut a, RAX, 0x555);
    store_to_rsp(&mut a, RAX, 0);
    mov_imm64(&mut a, RAX, EgclVal::from_fixnum(37).0 as i64);
    store_to_rsp(&mut a, RAX, 8);
    let raw_double = 2.25f64.to_bits();
    mov_imm64(&mut a, RAX, raw_double as i64);
    store_to_rsp(&mut a, RAX, 16);
    let first = a.label();
    let second = a.label();
    alu_r_imm(&mut a, 7, 7, 0);
    a.jcc(Cc::E, first);
    a.jmp(second);
    let small = vec![
        Source::Imm(100),
        Source::Imm(7),
        Source::Imm(3),
        Source::Imm(0),
        Source::Home(FramedHome::Reg(6), false),
        Source::Home(FramedHome::Stack(1), false),
        Source::Home(FramedHome::Stack(2), false),
    ];
    let mut large = vec![
        Source::Imm(100),
        Source::Imm(9),
        Source::Imm(18),
        Source::Imm(0),
        Source::Home(FramedHome::Reg(6), true),
        Source::Home(FramedHome::Stack(1), false),
        Source::Home(FramedHome::Stack(2), false),
    ];
    large.extend(std::iter::repeat_n(
        Source::Imm(EgclVal::from_fixnum(3).0),
        15,
    ));
    let sites = [
        Site {
            label: first,
            frame_state: FrameStateId(0),
            n_scopes: 1,
            sources: small,
            f64_slots: vec![2].into_boxed_slice(),
        },
        Site {
            label: second,
            frame_state: FrameStateId(1),
            n_scopes: 1,
            sources: large,
            f64_slots: vec![2].into_boxed_slice(),
        },
    ];
    let candidate = emit_candidate(
        a,
        &sites,
        4,
        FramedHome::Stack(0),
        capture as *const () as usize as u64,
        epilogue,
    )
    .unwrap();
    assert_eq!(candidate.calls.len(), 2);
    assert_ne!(
        candidate.calls[0].return_offset,
        candidate.calls[1].return_offset
    );
    assert_eq!(
        candidate.calls[0].stack_adjust, candidate.calls[1].stack_adjust,
        "both FS sizes share fixed maximum A"
    );
    for (index, site) in candidate.calls.iter().enumerate() {
        assert_eq!(
            site.origin,
            NativeCallOrigin::Deopt(FrameStateId(index as u32))
        );
        assert!(
            matches!(&candidate.values[&site.return_offset], NativeCallValues::Deoptimizing { native_slots: 4, f64_slots } if f64_slots.as_ref() == [2])
        );
    }
    let helper = (candidate.helper_start, candidate.helper_end);
    let bytes = candidate.asm.finish().unwrap();
    for instruction in iced_x86::Decoder::new(
        64,
        &bytes[helper.0..helper.1],
        iced_x86::DecoderOptions::NONE,
    ) {
        assert_ne!(
            instruction.mnemonic(),
            iced_x86::Mnemonic::Call,
            "serializer must not collect or introduce a call PC"
        );
        assert!(
            !matches!(
                instruction.op0_register(),
                iced_x86::Register::R13 | iced_x86::Register::RBP
            ),
            "dedicated execution/frame registers stay intact"
        );
    }
    let buffer = egcl_rt::jit::JitBuffer::new(&bytes).unwrap();
    let function: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    for (which, input, count, bcp) in [(0, EgclVal::from_fixnum(7).0, 7, 7), (1, 7, 22, 9)] {
        assert_eq!(EgclVal(function(which, input)).as_fixnum(), 777);
        assert_eq!(COUNT.load(Ordering::Relaxed), count);
        assert_eq!(CONTROL.load(Ordering::Relaxed), 0x555);
        assert_eq!(WORDS[1].load(Ordering::Relaxed), bcp);
        assert_eq!(WORDS[4].load(Ordering::Relaxed), EgclVal::from_fixnum(7).0);
        assert_eq!(WORDS[5].load(Ordering::Relaxed), EgclVal::from_fixnum(37).0);
        assert_eq!(WORDS[6].load(Ordering::Relaxed), raw_double);
    }
}

fn guarded_function(count: u32) -> Function {
    let mut f = Function::new("shared-recovery-profitability");
    let block = f.entry();
    let x = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
    let (_, one) = f.push_inst(
        block,
        InstData {
            opcode: Opcode::ConstFixnum,
            args: vec![],
            results: vec![],
            aux: AuxData::FixnumImm(1),
            flags: InstFlags::default(),
            frame_state: None,
            targets: vec![],
            source_pos: 0,
        },
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    let mut last = x;
    for bcp in 0..count {
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp,
                locals: vec![
                    ValueSource::Value {
                        value: x,
                        repr: ValueRepresentation::Tagged
                    };
                    if bcp % 2 == 0 { 12 } else { 24 }
                ],
                stack: vec![],
            }],
            remat: vec![],
        });
        let (_, value) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::FixnumAdd,
                args: vec![x, one[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                },
                frame_state: Some(state),
                targets: vec![],
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        last = value[0];
    }
    f.set_terminator(
        block,
        InstData {
            opcode: Opcode::Return,
            args: vec![last],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            frame_state: None,
            targets: vec![],
            source_pos: 0,
        },
    );
    crate::t2::verify::verify(&f).unwrap();
    f
}
fn ordinary(function: &Function) -> FramedCode {
    emit_framed_ordinary_function(
        function,
        OrdinaryLispEntryOptions {
            arity: 1,
            arity_error_veneer: 1,
            binding: None,
        },
        &[],
        OrdinaryCallFlags {
            gc_requested: 1,
            sigusr1_pending: 1,
            posted_signal: 1,
            native_errors: 1,
            signal_epoch: 1,
        },
        1,
        OrdinaryGuardOptions {
            veneer: 1,
            multiple_values: 1,
        },
    )
    .unwrap()
}
#[test]
fn shared_recovery_is_profitable_or_keeps_identical_legacy_artifacts() {
    const CASE: &str = "EGCL_TEST_SHARED_RECOVERY_MODE";
    let Ok(_) = std::env::var(CASE) else {
        let mut records = Vec::new();
        for mode in ["on", "off"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "t2::emit::shared_deopt::tests::shared_recovery_is_profitable_or_keeps_identical_legacy_artifacts", "--nocapture", "--test-threads=1"]).env(CASE, mode);
            if mode == "off" {
                command.env("EGCL_DISABLE_SHARED_DEOPT_RECOVERY", "1");
            } else {
                command.env_remove("EGCL_DISABLE_SHARED_DEOPT_RECOVERY");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            records.push(
                stdout
                    .lines()
                    .find_map(|line| {
                        line.split_once("SHARED-MEASUREMENT ")
                            .map(|(_, value)| value)
                    })
                    .unwrap()
                    .to_string(),
            );
        }
        let on: Vec<_> = records[0].split_whitespace().collect();
        let off: Vec<_> = records[1].split_whitespace().collect();
        assert_eq!(
            on[0], off[0],
            "unprofitable single-site code stays byte-identical"
        );
        assert!(
            on[1].parse::<usize>().unwrap() < off[1].parse::<usize>().unwrap(),
            "all embedded table/helper bytes count toward profit: {records:?}"
        );
        assert!(
            on[2].parse::<u32>().unwrap() > off[2].parse::<u32>().unwrap(),
            "shared max A includes the new saved-register/descriptor extent"
        );
        return;
    };
    let small = ordinary(&guarded_function(1));
    let large = ordinary(&guarded_function(32));
    let fingerprint = small
        .code
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    let calls = large.native_calls.as_ref().unwrap();
    let guards: Vec<_> = calls
        .iter()
        .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
        .collect();
    assert_eq!(guards.len(), 32);
    let max_a = guards.iter().map(|site| site.stack_adjust).max().unwrap();
    let frame = calls.unwind_recipe().unwrap().frame_bytes() as usize;
    assert!(
        large.ordinary_stack_required.unwrap() >= frame + max_a as usize + 8,
        "entry stack admission must cover the actual recovery extent"
    );
    for site in guards {
        assert!(matches!(
            calls.value_map(site.return_offset),
            Some(NativeCallValues::Deoptimizing { .. })
        ));
    }
    println!(
        "SHARED-MEASUREMENT {fingerprint:016x} {} {max_a}",
        large.code.len()
    );
}
