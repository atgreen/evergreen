// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, OsrEntry, Range, TypeBits,
    ValueRepresentation as Repr,
};
use crate::t2::x64_calls::{NativeCallOrigin, NativeCallValues};
use egcl_rt::value::EgclVal;
use std::sync::atomic::{AtomicU64, Ordering};

static RECOVERED_BCP: AtomicU64 = AtomicU64::new(u64::MAX);
static RECOVERED_VALUE: AtomicU64 = AtomicU64::new(0);
static MV_RESETS: AtomicU64 = AtomicU64::new(0);

extern "C" fn reset_mv(_: u64, _: u64, _: u64) -> u64 {
    MV_RESETS.fetch_add(1, Ordering::Relaxed);
    0
}

extern "C" fn reconstruct(scopes: u64, count: u64, words: *const u64, _: u64) -> u64 {
    if scopes != 1 || count != 5 {
        return EgclVal::from_fixnum(-1).0;
    }
    let words = unsafe { std::slice::from_raw_parts(words, count as usize) };
    RECOVERED_BCP.store(words[1], Ordering::Relaxed);
    RECOVERED_VALUE.store(words[4], Ordering::Relaxed);
    EgclVal::from_fixnum(99).0
}

fn state(
    f: &mut Function,
    x: crate::t2::ir::Value,
    bcp: u32,
) -> crate::t2::frame_state::FrameStateId {
    f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp,
            locals: vec![ValueSource::Value {
                value: x,
                repr: Repr::Tagged,
            }],
            stack: vec![],
        }],
        remat: vec![],
    })
}

fn multiply(range: Option<Range>) -> Function {
    let mut f = Function::new("unused-overflow-stub");
    let b = f.entry();
    let x = f.add_block_param(b, IRType::TOP, Repr::Tagged);
    // Pure framed bodies intentionally use whole-function restart recovery.
    // A real MV-reset leaf call selects precise per-instruction recovery.
    f.push_inst(
        b,
        InstData {
            opcode: Opcode::ClearMv,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[],
    );
    let guard_state = state(&mut f, x, 3);
    let ty = IRType {
        bits: TypeBits::FIXNUM,
        range,
        class_id: None,
    };
    let (_, guarded) = f.push_inst(
        b,
        InstData {
            opcode: Opcode::Guard,
            args: vec![x],
            results: vec![],
            aux: AuxData::TypeTag(ty),
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(guard_state),
            source_pos: 0,
        },
        &[(ty, Repr::Tagged)],
    );
    let (_, five) = f.push_inst(
        b,
        InstData {
            opcode: Opcode::ConstFixnum,
            args: vec![],
            results: vec![],
            aux: AuxData::FixnumImm(5),
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)],
    );
    let multiply_state = state(&mut f, x, 7);
    let (_, result) = f.push_inst(
        b,
        InstData {
            opcode: Opcode::FixnumMul,
            args: vec![guarded[0], five[0]],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(multiply_state),
            source_pos: 0,
        },
        &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)],
    );
    f.set_terminator(
        b,
        InstData {
            opcode: Opcode::Return,
            args: vec![result[0]],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
    );
    crate::t2::verify::verify(&f).unwrap();
    f
}

fn emit(f: &Function) -> FramedCode {
    emit_framed(
        f,
        0,
        reconstruct as *const () as usize as u64,
        0,
        0,
        0,
        0,
        0,
        reset_mv as *const () as usize as u64,
        None,
    )
    .unwrap()
}

fn precise_sites(code: &FramedCode) -> Vec<crate::t2::frame_state::FrameStateId> {
    let calls = code.native_calls.as_ref().unwrap();
    calls
        .iter()
        .filter_map(|site| {
            if let NativeCallOrigin::Deopt(id) = site.origin {
                assert!(matches!(
                    calls.value_map(site.return_offset),
                    Some(NativeCallValues::Deoptimizing { .. })
                ));
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

fn run(code: &FramedCode, offset: usize, input: EgclVal) -> EgclVal {
    let buffer = egcl_rt::jit::JitBuffer::new(&code.code).unwrap();
    let entry: extern "C" fn(*mut u64) -> u64 =
        unsafe { std::mem::transmute(buffer.as_ptr().add(offset)) };
    let mut slots = [input.0, 0, 0, 0];
    EgclVal(entry(slots.as_mut_ptr()))
}

#[test]
fn unused_deopt_elision_preserves_referenced_recovery_and_future_osr() {
    const CASE: &str = "EGCL_TEST_UNUSED_DEOPT_MODE";
    let Ok(mode) = std::env::var(CASE) else {
        let mut lengths = Vec::new();
        for mode in ["elide", "keep"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "t2::emit::unused_deopt_tests::unused_deopt_elision_preserves_referenced_recovery_and_future_osr", "--nocapture", "--test-threads=1"])
                .env(CASE, mode);
            if mode == "keep" {
                command.env("EGCL_DISABLE_UNUSED_DEOPT_STUB_ELISION", "1");
            } else {
                command.env_remove("EGCL_DISABLE_UNUSED_DEOPT_STUB_ELISION");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{mode}: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let length = stdout
                .lines()
                .find_map(|line| line.split_once("PROVEN-CODE-BYTES ").map(|(_, value)| value))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            lengths.push(length);
        }
        assert!(
            lengths[0] < lengths[1],
            "elision removes emitted code, not only metadata: {lengths:?}"
        );
        return;
    };
    let proven = emit(&multiply(Some(Range { lo: 0, hi: 100 })));
    let sites = precise_sites(&proven);
    assert_eq!(sites.len(), if mode == "keep" { 2 } else { 1 });
    assert!(proven.has_deopt, "type/range guard remains referenced");
    MV_RESETS.store(0, Ordering::Relaxed);
    assert_eq!(run(&proven, 0, EgclVal::from_fixnum(7)).as_fixnum(), 35);
    assert_eq!(
        MV_RESETS.load(Ordering::Relaxed),
        1,
        "the call-capable fixture executes its real MV reset"
    );
    let wrong_type = EgclVal::from_single_float(2.0);
    assert_eq!(run(&proven, 0, wrong_type).as_fixnum(), 99);
    assert_eq!(RECOVERED_BCP.load(Ordering::Relaxed), 3);
    assert_eq!(RECOVERED_VALUE.load(Ordering::Relaxed), wrong_type.0);
    assert_eq!(run(&proven, 0, EgclVal::from_fixnum(101)).as_fixnum(), 99);
    assert_eq!(RECOVERED_BCP.load(Ordering::Relaxed), 3);
    assert_eq!(
        RECOVERED_VALUE.load(Ordering::Relaxed),
        EgclVal::from_fixnum(101).0
    );
    println!("PROVEN-CODE-BYTES {}", proven.code.len());

    let unproven = emit(&multiply(None));
    assert_eq!(
        precise_sites(&unproven).len(),
        2,
        "referenced overflow and type stubs stay"
    );
    let maximum = EgclVal::from_fixnum((1i64 << 60) - 1);
    assert_eq!(run(&unproven, 0, maximum).as_fixnum(), 99);
    assert_eq!(RECOVERED_BCP.load(Ordering::Relaxed), 7);
    assert_eq!(RECOVERED_VALUE.load(Ordering::Relaxed), maximum.0);

    let mut osr = Function::new("future-osr-guard");
    let b = osr.entry();
    let x = osr.add_block_param(b, IRType::TOP, Repr::Tagged);
    let fs = state(&mut osr, x, 5);
    osr.set_terminator(
        b,
        InstData {
            opcode: Opcode::Return,
            args: vec![x],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
    );
    osr.osr_entries.push(OsrEntry {
        control_scopes: vec![],
        bcp: 5,
        block: b,
        frame_state: fs,
        checks: vec![(x, IRType::of(TypeBits::FIXNUM))],
    });
    crate::t2::verify::verify(&osr).unwrap();
    let osr = emit(&osr);
    assert_eq!(
        precise_sites(&osr).len(),
        1,
        "OSR branch is emitted after the stub loop"
    );
    assert!(osr.has_deopt);
    let offset = osr.osr_entries[0].1;
    assert_eq!(run(&osr, offset, EgclVal::from_fixnum(7)).as_fixnum(), 7);
    assert_eq!(run(&osr, offset, wrong_type).as_fixnum(), 99);
    assert_eq!(RECOVERED_BCP.load(Ordering::Relaxed), 5);
    assert_eq!(RECOVERED_VALUE.load(Ordering::Relaxed), wrong_type.0);
}
