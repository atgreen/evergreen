// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bounded packed-octet allocation in front of the unchanged collecting adapter.
//! Every decline precedes cursor publication. The committed path has no call,
//! safepoint or failure edge and initializes the entire footprint, padding too.
use super::inline_alloc::{fs_mem, load_value, window_fields};
use super::*;
use crate::t2::ir::{AuxData, Function, InstData, Opcode, Value, ValueRepresentation};
use std::collections::HashMap;

const MAX_LENGTH: i32 = 256;
const R11: u8 = 11;

fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_INLINE_U8_ALLOCATION").is_some())
}

fn shape(f: &Function, data: &InstData) -> Option<ordinary_primitives::PackedU8AdapterSymbols> {
    let AuxData::CallTarget(target) = data.aux else {
        return None;
    };
    if disabled()
        || !matches!(data.opcode, Opcode::Call | Opcode::Invoke)
        || data.args.len() != 3
        || data.results.len() != 1
        || !data.flags.call
        || !data.flags.effectful
        || !data.flags.safepoint
        || data.frame_state.is_none()
        || data
            .args
            .iter()
            .chain(&data.results)
            .any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
    {
        return None;
    }
    ordinary_primitives::packed_u8_adapter(target)
}

pub(super) fn eligible(f: &Function, data: &InstData) -> bool {
    shape(f, data).is_some()
}

fn log_size(f: &Function, bytes: usize) {
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("EGCL_INLINE_U8_LOG").is_some()) {
        eprintln!(
            "[inline-u8] {}: fast allocation bytes={bytes} max_length={MAX_LENGTH}",
            f.name()
        );
    }
}

pub(super) fn emit_ordinary(
    a: &mut Asm,
    f: &Function,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_empty: bool,
) -> Option<egcl_rt::asm::Label> {
    if !mv_empty {
        return None;
    }
    let slow = a.label();
    let done = a.label();
    let start = a.here();
    emit_bump(a, f, data, homes, constants, slow)?;
    a.push(0xf8); // CF clear promises the already-empty ordinary MV buffer.
    a.jmp(done);
    a.bind(slow);
    log_size(f, a.here() - start);
    Some(done)
}

/// Pair only the next instruction in this block. In particular, no implicit
/// poll, edge projection, MV reset or other operation may be bypassed.
pub(super) fn adjacent_layout(
    f: &Function,
    block: crate::t2::ir::Block,
    call: crate::t2::ir::Inst,
    straight_polls: &std::collections::HashSet<crate::t2::ir::Inst>,
    tail: bool,
) -> Option<(crate::t2::ir::Inst, Value)> {
    let data = f.inst(call);
    if tail || data.opcode != Opcode::Call || shape(f, data).is_none() {
        return None;
    }
    let instructions = &f.block(block).insts;
    let position = instructions.iter().position(|&inst| inst == call)?;
    let probe = *instructions.get(position + 1)?;
    let layout = f.inst(probe);
    if straight_polls.contains(&probe)
        || !matches!(layout.opcode, Opcode::PackedU8Layout | Opcode::PackedU8AllocationLayout)
        || (if layout.opcode == Opcode::PackedU8Layout {
            layout.args.as_slice() != data.results.as_slice()
        } else {
            layout.args.len() != 2 || layout.args[0] != data.results[0]
                || layout.args[1] != data.results[0]
        })
        || layout.results.len() != 1
        || layout.flags != crate::t2::ir::InstFlags::default()
        || !matches!(layout.aux, AuxData::None)
        || layout.frame_state.is_some()
        || !layout.targets.is_empty()
        || f.value(layout.results[0]).repr != ValueRepresentation::Tagged
        || f.value(layout.results[0]).ty.bits
            != crate::t2::ir::TypeBits::FIXNUM.join(crate::t2::ir::TypeBits::NULL)
    {
        return None;
    }
    Some((probe, layout.results[0]))
}

/// An explicit producer can remain separated by an observable MV clear. Keep
/// that instruction on BOTH execution paths; never jump over a poll or effect.
#[derive(Clone, Copy)]
pub(super) struct AllocationLayout {
    pub probe: crate::t2::ir::Inst,
    pub width: Value,
    pub delayed: bool,
    pub extent: Option<(crate::t2::ir::Inst, Value)>,
}

pub(super) fn allocation_layout(
    f: &Function,
    block: crate::t2::ir::Block,
    call: crate::t2::ir::Inst,
    straight_polls: &std::collections::HashSet<crate::t2::ir::Inst>,
    tail: bool,
) -> Option<AllocationLayout> {
    if let Some((probe, width)) = adjacent_layout(f, block, call, straight_polls, tail) {
        let extent = crate::t2::allocation_extent::adjacent_completion(f, block, probe, straight_polls);
        return Some(AllocationLayout { probe, width, delayed: false, extent });
    }
    if tail || f.inst(call).opcode != Opcode::Call || shape(f, f.inst(call)).is_none() {
        return None;
    }
    let instructions = &f.block(block).insts;
    let position = instructions.iter().position(|&i| i == call)?;
    let mut index = position + 1;
    while let Some(&inst) = instructions.get(index) {
        let d = f.inst(inst);
        if straight_polls.contains(&inst) { return None; }
        if d.opcode != Opcode::ClearMv { break; }
        // This gap has no values/homes to overwrite the early scalar evidence.
        // ClearMv remains emitted, even when the incoming MV state is empty.
        if !d.args.is_empty() || !d.results.is_empty() || !d.targets.is_empty()
            || d.flags.call || d.flags.safepoint || d.flags.terminator || d.flags.guard
        { return None; }
        index += 1;
    }
    let probe = *instructions.get(index)?;
    if straight_polls.contains(&probe) || f.inst(probe).opcode != Opcode::PackedU8AllocationLayout {
        return None;
    }
    let facts = crate::t2::value_facts::StructuralFacts::analyze(f);
    if crate::t2::allocation_facts::allocation_site(f, probe, &facts) != Some(call) {
        return None;
    }
    Some(AllocationLayout { probe, width: f.inst(probe).results[0], delayed: index != position + 1, extent: None })
}

/// Consume an early scalar completion after a retained MV clear. The scalar is
/// initialized on both call outcomes; NIL requests the real slow-result probe.
/// Original tagged array homes, not an allocation/interior pointer, are reloaded.
pub(super) fn emit_delayed_layout(
    a: &mut Asm,
    f: &Function,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
) -> Result<(), EmitError> {
    let done = a.label();
    let width = *data.results.first().ok_or(EmitError::UnsupportedOp(0xf2))?;
    load_home(a, RAX, *homes.get(&width).ok_or(EmitError::UnsupportedOp(0xf2))?, 0);
    alu_r_imm(a, 7, RAX, egcl_rt::value::NIL.to_raw() as i32);
    a.jcc(Cc::Ne, done);
    super::octet_word_store::emit_layout(a, f, data, homes, constants)?;
    a.bind(done);
    Ok(())
}

/// Only the runtime-validated bump success supplies this width. Every slow
/// branch still enters the original collecting call and the real probe.
#[cfg(test)]
pub(super) fn emit_ordinary_layout(
    a: &mut Asm, f: &Function, data: &InstData, homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>, mv_empty: bool, fusion: (Value, egcl_rt::asm::Label),
) -> Option<()> {
    emit_ordinary_layout_extent(a, f, data, homes, constants, mv_empty, (fusion.0, fusion.1, None))
}

pub(super) fn emit_ordinary_layout_extent(
    a: &mut Asm,
    f: &Function,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_empty: bool,
    fusion: (Value, egcl_rt::asm::Label, Option<Value>),
) -> Option<()> {
    if !mv_empty {
        return None;
    }
    let result = *homes.get(data.results.first()?)?;
    let width = *homes.get(&fusion.0)?;
    let valid_home = |home| match home {
        FramedHome::Reg(reg) => crate::t2::x64_frame::FRAME_GPRS
            .iter()
            .any(|&index| crate::t2::x64_frame::GPR_X86[index] == reg),
        FramedHome::Stack(slot) => slot <= i32::MAX as u32 / 8,
    };
    if result == width || !valid_home(result) || !valid_home(width) {
        return None;
    }
    let extent = match fusion.2 {
        Some(value) => {
            let home = *homes.get(&value)?;
            if home == result || home == width || !valid_home(home) { return None; }
            Some(home)
        },
        None => None,
    };
    let slow = a.label();
    emit_bump(a, f, data, homes, constants, slow)?;
    publish_layout_extent_completion(a, result, width, extent, fusion.1);
    a.bind(slow);
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("EGCL_ARRAY_LAYOUT_LOG").is_some()) {
        eprintln!(
            "[array-layout] {}: inline allocation witness reused",
            f.name()
        );
    }
    Some(())
}

/// Enter only after emit_bump committed and initialized a small packed-U8
/// allocation in RAX. Caller preflighted distinct final homes and empty MV.
/// No allocation pointer is retained: publish tagged array and tagged width.
#[cfg(test)]
fn publish_layout_completion(a: &mut Asm, result: FramedHome, width: FramedHome,
    done: egcl_rt::asm::Label) {
    publish_layout_extent_completion(a, result, width, None, done)
}

fn publish_layout_extent_completion(
    a: &mut Asm,
    result: FramedHome,
    width: FramedHome,
    extent: Option<FramedHome>,
    done: egcl_rt::asm::Label,
) {
    store_home(a, result, RAX, 0);
    if let Some(extent) = extent {
        // This is precisely the scalar decoded before cursor commit, already
        // written into the fresh object's immutable length. Never reread CAR.
        load_mem64_disp(a, SCRATCH, RAX, 16 - egcl_rt::value::TAG_HEAP_OBJECT as i32);
        shl_imm(a, SCRATCH, 3);
        store_home(a, extent, SCRATCH, 0);
    }
    mov_imm32(a, SCRATCH, 8 << 3);
    store_home(a, width, SCRATCH, 0);
    a.push(0xf8); // incoming MV was proven empty; fast allocation leaves it so
    a.jmp(done);
}

pub(super) fn emit_framed(
    a: &mut Asm,
    f: &Function,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_unobservable: bool,
) -> Option<egcl_rt::asm::Label> {
    if !mv_unobservable {
        return None;
    }
    let result = *homes.get(data.results.first()?)?;
    let slow = a.label();
    let done = a.label();
    let start = a.here();
    emit_bump(a, f, data, homes, constants, slow)?;
    store_home(a, result, RAX, 0);
    a.jmp(done);
    a.bind(slow);
    log_size(f, a.here() - start);
    Some(done)
}

fn emit_bump(
    a: &mut Asm,
    f: &Function,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    slow: egcl_rt::asm::Label,
) -> Option<()> {
    let symbols = shape(f, data)?;
    let window = egcl_rt::gc::native_alloc_layout()?;
    let (cursor, limit, epoch) = window_fields(&window)?;
    let layout = egcl_rt::gc::native_packed_u8_layout();
    // The compact byte/word metadata stores below are for this LE small-header
    // encoding. A changed runtime representation conservatively keeps the call.
    if layout.header_bytes != 8
        || layout.alignment != 16
        || layout.length_offset != 8
        || layout.data_offset != 16
        || layout.body_size_shift != 16
        || layout.size_units_one != 1
        || layout.size_units_mask != 0xffff
        || layout.body_size_mask != 0x0000_ffff_ffff_0000
        || layout.header_base & 0x00ff_ffff_ffff_ffff != 0
        || !data.args.iter().all(|value| {
            constants.contains_key(value)
                || homes
                    .get(value)
                    .is_some_and(|home| !matches!(home, FramedHome::Reg(0 | 2 | 11)))
        })
    {
        return None;
    }
    let load = |a: &mut Asm, dst, value| {
        load_value(a, dst, value, homes, constants).expect("all operands preflighted");
    };
    // No emitted failure after this point is a compiler-time decline.
    // A caller reaching slow retains every input home and the original FS.
    load(a, RAX, data.args[0]);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    let checked_later = a.label();
    a.jcc(Cc::Ne, checked_later);
    alu_r_imm(a, 7, RAX, MAX_LENGTH << 3);
    let unsigned = a.here();
    a.jcc(Cc::G, slow);
    a.patch_u8(unsigned + 1, 0x87); // JA rejects negative FX as well as > cap.
    a.bind(checked_later);

    let keyword = egcl_rt::value::EgclVal::from_symbol_index(symbols.element_type).to_raw();
    if constants.get(&data.args[1]).copied() != Some(keyword) {
        load(a, RAX, data.args[1]);
        mov_imm64(a, R11, keyword as i64);
        cmp_rr(a, RAX, R11);
        a.jcc(Cc::Ne, slow);
    }
    load(a, RAX, data.args[2]);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_CONS as i32);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, SCRATCH, RAX, -1);
    mov_imm64(
        a,
        R11,
        egcl_rt::value::EgclVal::from_symbol_index(symbols.unsigned_byte).to_raw() as i64,
    );
    cmp_rr(a, SCRATCH, R11);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, RAX, RAX, 7);
    mov_rr(a, SCRATCH, RAX);
    alu_r_imm(a, 4, SCRATCH, 7);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_CONS as i32);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, SCRATCH, RAX, -1);
    alu_r_imm(a, 7, SCRATCH, 8 << 3);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, SCRATCH, RAX, 7);
    mov_imm64(a, R11, egcl_rt::value::NIL.to_raw() as i64);
    cmp_rr(a, SCRATCH, R11);
    a.jcc(Cc::Ne, slow);

    // Decode either a direct fixnum or a proper singleton dimension list.
    // Read the mutable car ONCE. R11 holds this immutable scalar through the
    // entire allocation; neither list cell nor any argument is read again.
    load(a, R11, data.args[0]);
    mov_rr(a, SCRATCH, R11);
    alu_r_imm(a, 4, SCRATCH, 7);
    let scalar = a.label();
    a.jcc(Cc::E, scalar);
    alu_r_imm(a, 7, SCRATCH, egcl_rt::value::TAG_CONS as i32);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, SCRATCH, R11, 7);
    mov_imm64(a, RAX, egcl_rt::value::NIL.to_raw() as i64);
    cmp_rr(a, SCRATCH, RAX);
    a.jcc(Cc::Ne, slow);
    load_mem64_disp(a, R11, R11, -1);
    mov_rr(a, SCRATCH, R11);
    alu_r_imm(a, 4, SCRATCH, 7);
    a.jcc(Cc::Ne, slow);
    a.bind(scalar);
    alu_r_imm(a, 7, R11, MAX_LENGTH << 3);
    let unsigned = a.here();
    a.jcc(Cc::G, slow);
    a.patch_u8(unsigned + 1, 0x87); // JA: includes negative scalar lengths.
    shr_imm(a, R11, 3);

    // RDX = next cursor; R11 remains the decoded raw length. No overflow is
    // possible within an initialized canonical userspace window at this cap.
    mov_rr(a, SCRATCH, R11);
    alu_r_imm(a, 0, SCRATCH, 39);
    alu_r_imm(a, 4, SCRATCH, -16);
    fs_mem(a, 0x8b, RAX, cursor);
    alu_rr(a, 0x01, SCRATCH, RAX);
    fs_mem(a, 0x3b, SCRATCH, limit);
    a.jcc(Cc::G, slow);
    mov_imm64(a, RAX, window.epoch_address as i64);
    load_mem64_disp(a, RAX, RAX, 0);
    fs_mem(a, 0x3b, RAX, epoch);
    a.jcc(Cc::Ne, slow);
    fs_mem(a, 0x8b, RAX, cursor);
    fs_mem(a, 0x89, SCRATCH, cursor); // COMMIT: no failures/calls/polls below.

    // Initialize exact header, raw length and metadata padding using ONLY the
    // captured scalar. R11 can be reused after these writes; the mutable list
    // is never reread, including when its car aliases another live argument.
    alu_rr(a, 0x31, SCRATCH, SCRATCH);
    store_mem64_disp(a, RAX, 0, SCRATCH);
    store_mem64_disp(a, RAX, 8, SCRATCH);
    store_mem64_disp(a, RAX, 16, R11);
    a.extend_from_slice(&[0x41, 0x83, 0xc3, 16]); // add r11d,16
    a.extend_from_slice(&[0x44, 0x89, 0x58, 0x02]); // body-byte hash at header+2
    a.extend_from_slice(&[0x41, 0x83, 0xc3, 23]);
    a.extend_from_slice(&[0x41, 0x83, 0xe3, 0xf0]);
    a.extend_from_slice(&[0x41, 0xc1, 0xeb, 3]);
    a.extend_from_slice(&[0x66, 0x44, 0x89, 0x18]); // footprint/8 -> size word
    a.extend_from_slice(&[0xc6, 0x40, 0x07, (layout.header_base >> 56) as u8]);
    a.extend_from_slice(&[0xc6, 0x40, 0x08, layout.element_tag]);

    // One compact backwards qword loop clears payload AND alignment padding.
    // Even n=0 has eight padding bytes here, so there are 1..33 iterations.
    fs_mem(a, 0x8b, R11, cursor);
    alu_r_imm(a, 0, RAX, 24);
    let zero = a.label();
    a.bind(zero);
    a.extend_from_slice(&[0x49, 0x83, 0xeb, 0x08]); // sub r11,8
    a.extend_from_slice(&[0x49, 0x89, 0x13]); // mov [r11],rdx (zero)
    cmp_rr(a, R11, RAX);
    a.jcc(Cc::Ne, zero);
    alu_r_imm(a, 5, RAX, 24);
    alu_r_imm(a, 1, RAX, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{BlockCall, IRType, InstFlags};

    fn fixture() -> (Function, InstData) {
        let mut f = Function::new("inline-packed-u8-contract");
        let entry = f.entry();
        let args: Vec<_> = (0..3)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 1,
                bcp: 2,
                locals: args
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        });
        let symbols = ordinary_primitives::PackedU8AdapterSymbols {
            element_type: 0xf001,
            unsigned_byte: 0xf002,
        };
        assert!(ordinary_primitives::register_packed_u8_adapter(
            0xf003, symbols
        ));
        let (site, results) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::Call,
                args,
                results: vec![],
                aux: AuxData::CallTarget(0xf003),
                flags: InstFlags {
                    call: true,
                    safepoint: true,
                    effectful: true,
                    ..Default::default()
                },
                frame_state: Some(state),
                targets: vec![],
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: results,
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                frame_state: None,
                targets: Vec::<BlockCall>::new(),
                source_pos: 0,
            },
        );
        let data = f.inst(site).clone();
        (f, data)
    }

    #[test]
    fn adapter_identity_and_tagged_operand_contract_exclude_opaque_calls() {
        let (mut f, mut data) = fixture();
        assert!(eligible(&f, &data));
        let symbols = ordinary_primitives::packed_u8_adapter(0xf003).unwrap();
        assert!(ordinary_primitives::register_packed_u8_adapter(
            0xf013, symbols
        ));
        assert!(!ordinary_primitives::register_packed_u8_adapter(
            0xf013,
            ordinary_primitives::PackedU8AdapterSymbols {
                unsigned_byte: 0xf014,
                ..symbols
            }
        ));
        assert_eq!(ordinary_primitives::packed_u8_adapter(0xf013), None);
        assert!(!ordinary_primitives::register_packed_u8_adapter(
            0xf013, symbols
        ));
        data.aux = AuxData::OpaqueCallTarget(0xf003);
        assert!(!eligible(&f, &data));
        data.aux = AuxData::CallTarget(0xf004);
        assert!(!eligible(&f, &data));
        data.aux = AuxData::CallTarget(0xf003);
        f.set_repr(data.args[0], ValueRepresentation::UnboxedFixnum);
        assert!(!eligible(&f, &data));
    }

    #[test]
    fn preflight_declines_without_bytes_and_inline_code_has_a_bounded_size() {
        let (f, data) = fixture();
        let homes = HashMap::from([
            (data.args[0], FramedHome::Stack(0)),
            (data.args[1], FramedHome::Stack(1)),
            (data.args[2], FramedHome::Stack(2)),
            (data.results[0], FramedHome::Stack(3)),
        ]);
        let mut a = Asm::new();
        assert!(emit_ordinary(&mut a, &f, &data, &homes, &HashMap::new(), false).is_none());
        assert!(emit_ordinary(&mut a, &f, &data, &HashMap::new(), &HashMap::new(), true).is_none());
        let mut reserved = homes.clone();
        reserved.insert(data.args[2], FramedHome::Reg(RAX));
        assert!(emit_ordinary(&mut a, &f, &data, &reserved, &HashMap::new(), true).is_none());
        assert_eq!(a.here(), 0);
        let done = emit_ordinary(&mut a, &f, &data, &homes, &HashMap::new(), true).unwrap();
        let bytes = a.here();
        a.bind(done);
        let code = a.finish().unwrap();
        assert_eq!(bytes, code.len());
        assert!(bytes <= 544, "bounded template grew to {bytes} bytes");
        eprintln!("inline packed-U8 worst-home ordinary bytes={bytes}");
        // This budget excludes unrolling a store for every possible element.
    }
    fn paired_fixture() -> (Function, crate::t2::ir::Inst, crate::t2::ir::Inst, Value) {
        let (mut f, call_data) = fixture();
        let block = f.entry();
        let call = f.block(block).insts[0];
        let (probe, results) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::PackedU8Layout,
                args: call_data.results,
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                frame_state: None,
                targets: vec![],
                source_pos: 0,
            },
            &[(
                IRType::of(crate::t2::ir::TypeBits::FIXNUM.join(crate::t2::ir::TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        assert_eq!(f.block_mut(block).insts.pop(), Some(probe));
        f.block_mut(block).insts.insert(1, probe);
        (f, call, probe, results[0])
    }
    #[test]
    fn allocation_layout_pair_requires_exact_adjacent_array_without_poll_or_tail() {
        let (mut f, call, probe, width) = paired_fixture();
        let block = f.entry();
        let mut polls = std::collections::HashSet::new();
        assert_eq!(
            adjacent_layout(&f, block, call, &polls, false),
            Some((probe, width))
        );
        assert_eq!(adjacent_layout(&f, block, call, &polls, true), None);
        polls.insert(probe);
        assert_eq!(adjacent_layout(&f, block, call, &polls, false), None);
        polls.clear();
        let original = f.inst(probe).args[0];
        f.inst_mut(probe).args[0] = f.inst(call).args[0];
        assert_eq!(adjacent_layout(&f, block, call, &polls, false), None);
        f.inst_mut(probe).args[0] = original;
        let (reset, _) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::ClearMv,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..Default::default()
                },
                frame_state: None,
                targets: vec![],
                source_pos: 0,
            },
            &[],
        );
        assert_eq!(f.block_mut(block).insts.pop(), Some(reset));
        f.block_mut(block).insts.insert(1, reset);
        assert_eq!(adjacent_layout(&f, block, call, &polls, false), None);
        f.block_mut(block).insts.retain(|&inst| inst != reset);
        f.inst_mut(call).aux = AuxData::CallTarget(0xf004);
        assert_eq!(adjacent_layout(&f, block, call, &polls, false), None);
    }
    #[test]
    fn allocation_layout_fast_exit_requires_empty_mv_and_distinct_publishable_homes() {
        let (f, call, _, width) = paired_fixture();
        let data = f.inst(call);
        let homes = HashMap::from([
            (data.args[0], FramedHome::Stack(0)),
            (data.args[1], FramedHome::Stack(1)),
            (data.args[2], FramedHome::Stack(2)),
            (data.results[0], FramedHome::Reg(1)),
            (width, FramedHome::Stack(3)),
        ]);
        let mut a = Asm::new();
        let done = a.label();
        assert!(
            emit_ordinary_layout(
                &mut a,
                &f,
                data,
                &homes,
                &HashMap::new(),
                false,
                (width, done)
            )
            .is_none()
        );
        let mut aliases = homes.clone();
        aliases.insert(width, FramedHome::Reg(1));
        assert!(
            emit_ordinary_layout(
                &mut a,
                &f,
                data,
                &aliases,
                &HashMap::new(),
                true,
                (width, done)
            )
            .is_none()
        );
        assert_eq!(a.here(), 0);
        assert!(
            emit_ordinary_layout(
                &mut a,
                &f,
                data,
                &homes,
                &HashMap::new(),
                true,
                (width, done)
            )
            .is_some()
        );
        // Original slow call/probe emission belongs here; the fast branch must
        // cross it, with both homes published and CF cleared beforehand.
        let slow = a.here();
        a.push(0x90);
        a.bind(done);
        let code = a.finish().unwrap();
        assert_eq!(&code[slow - 6..slow - 5], &[0xf8]);
        assert_eq!(code[slow - 5], 0xe9);
        assert_eq!(
            i32::from_le_bytes(code[slow - 4..slow].try_into().unwrap()),
            1
        );
        assert!(
            code[..slow]
                .windows(5)
                .any(|bytes| bytes == [0xba, 64, 0, 0, 0]),
            "tagged small-header width uses flag-preserving short MOV32"
        );
    }
    #[test]
    fn native_layout_completion_publishes_homes_and_skips_probe_without_stack_or_carry_damage() {
        // Exercise the actual production tail after a phantom successful bump.
        // The mock tagged bits are never dereferenced: this is native control /
        // publication evidence, NOT an allocation or moving-GC claim.
        let mock_array = 0x1234_5002u64;
        let unrelated = 0xfeed_1234_5678_9abcu64;
        for (array_home, width_home) in [
            (FramedHome::Reg(1), FramedHome::Stack(0)),
            (FramedHome::Stack(0), FramedHome::Reg(1)),
            (FramedHome::Stack(0), FramedHome::Stack(1)),
        ] {
            let mut a = Asm::new();
            mov_rr(&mut a, 8, 7); // output pointer: independent of final homes
            mov_rr(&mut a, RAX, 4);
            store_mem64_disp(&mut a, 8, 24, RAX); // entry RSP
            a.extend_from_slice(&[0x48, 0x8d, 0x64, 0x24, 0xe8]); // lea rsp,[rsp-24]
            mov_imm64(&mut a, 6, unrelated as i64);
            mov_imm64(&mut a, RAX, mock_array as i64);
            a.push(0xf9); // STC must be cleared by the production tail
            let done = a.label();
            publish_layout_completion(&mut a, array_home, width_home, done);
            // This represents the skipped slow-call / actual layout-probe path.
            a.extend_from_slice(&[0x41, 0xc6, 0x40, 16, 123]);
            a.bind(done);
            load_home(&mut a, RAX, array_home, 0);
            store_mem64_disp(&mut a, 8, 0, RAX);
            load_home(&mut a, RAX, width_home, 0);
            store_mem64_disp(&mut a, 8, 8, RAX);
            a.extend_from_slice(&[0x41, 0x0f, 0x92, 0x40, 32]); // setc byte [r8+32]
            store_mem64_disp(&mut a, 8, 40, 6);
            a.extend_from_slice(&[0x48, 0x8d, 0x64, 0x24, 24]);
            mov_rr(&mut a, RAX, 4);
            store_mem64_disp(&mut a, 8, 48, RAX);
            a.push(0xc3);
            let bytes = a.finish().unwrap();
            let code = egcl_rt::jit::JitBuffer::new(&bytes).unwrap();
            let entry: extern "C" fn(*mut u64) = unsafe { std::mem::transmute(code.as_ptr()) };
            let mut observed = [0u64; 7];
            entry(observed.as_mut_ptr());
            assert_eq!(observed[0], mock_array);
            assert_eq!(observed[1], 64);
            assert_eq!(observed[2], 0, "the slow probe sentinel must be bypassed");
            assert_eq!(observed[4], 0, "CLC supplies the ordinary empty-MV promise");
            assert_eq!(observed[5], unrelated, "unrelated live RSI home survives");
            assert_eq!(observed[3], observed[6], "native stack remains balanced");
        }
    }
    #[test]
    fn explicit_allocation_evidence_crosses_retained_clear_but_not_poll_or_effect() {
        let (mut f, call, probe, width) = paired_fixture();
        assert_eq!(crate::t2::allocation_facts::specialize(&mut f), 1);
        let block = f.entry();
        let (clear, _) = f.push_inst(block, InstData {
            opcode: Opcode::ClearMv, args: vec![], results: vec![], aux: AuxData::None,
            flags: InstFlags { effectful: true, ..Default::default() },
            frame_state: None, targets: vec![], source_pos: 0,
        }, &[]);
        assert_eq!(f.block_mut(block).insts.pop(), Some(clear));
        f.block_mut(block).insts.insert(1, clear);
        let mut polls = std::collections::HashSet::new();
        let selected = allocation_layout(&f, block, call, &polls, false).unwrap();
        assert_eq!(selected.probe, probe);
        assert_eq!(selected.width, width);
        assert!(selected.delayed);
        assert!(allocation_layout(&f, block, call, &polls, true).is_none());
        polls.insert(clear);
        assert!(allocation_layout(&f, block, call, &polls, false).is_none());
        polls.clear(); polls.insert(probe);
        assert!(allocation_layout(&f, block, call, &polls, false).is_none());
        polls.clear();
        f.inst_mut(clear).flags.safepoint = true;
        assert!(allocation_layout(&f, block, call, &polls, false).is_none());
        f.inst_mut(clear).flags.safepoint = false;
        f.inst_mut(clear).opcode = Opcode::Store;
        assert!(allocation_layout(&f, block, call, &polls, false).is_none());
    }

    #[test]
    fn explicit_allocation_native_consumer_uses_fast_scalar_or_real_slow_probe() {
        let (mut f, _, probe, width) = paired_fixture();
        assert_eq!(crate::t2::allocation_facts::specialize(&mut f), 1);
        let input = f.inst(probe).args[0];
        // Bare-leaf ABI: RDI holds the tagged object, RSI the prepublished
        // completion. No pointer is retained and this leaf cannot collect.
        let homes = HashMap::from([(input, FramedHome::Reg(7)), (width, FramedHome::Reg(6))]);
        let mut a = Asm::new();
        emit_delayed_layout(&mut a, &f, f.inst(probe), &homes, &HashMap::new()).unwrap();
        mov_rr(&mut a, RAX, 6);
        a.push(0xc3);
        let code = a.finish().unwrap();
        let jit = egcl_rt::jit::JitBuffer::new(&code).unwrap();
        let run: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(jit.as_ptr()) };
        egcl_rt::rooted!(array = egcl_rt::packed_u8::make(8, 0).unwrap());
        let nil = egcl_rt::value::NIL.to_raw();
        assert_eq!(run((*array).to_raw(), 8 << 3), 8 << 3, "validated fast completion");
        assert_eq!(run((*array).to_raw(), nil), 8 << 3, "slow UB8 result is probed");
        assert_eq!(run(egcl_rt::value::T.to_raw(), nil), nil,
            "generic slow return has no unconditional allocation layout");
        // Emission-only construction plus bare native execution checks the
        // transport/consumer, not a moving collection or actual TLAB allocation.
    }

    #[test]
    fn allocation_extent_fast_completion_captures_decoded_length_and_preserves_cf() {
        // Bare native tail over an aligned nonmoving fixture. This proves the
        // production completion's transport, not moving-GC/TLAB admission.
        let object = [0u64, 0, 136, 0];
        let tagged = unsafe { egcl_rt::value::EgclVal::from_heap_ptr(object.as_ptr().cast_mut().cast()).to_raw() };
        let mut a = Asm::new(); mov_rr(&mut a, 8, 7);
        mov_rr(&mut a, RAX, 4); store_mem64_disp(&mut a, 8, 48, RAX);
        alu_r_imm(&mut a, 5, 4, 32);
        mov_imm64(&mut a, 6, 0x1234); mov_imm64(&mut a, RAX, tagged as i64); a.push(0xf9);
        let done = a.label();
        publish_layout_extent_completion(&mut a, FramedHome::Stack(0), FramedHome::Stack(1),
            Some(FramedHome::Stack(2)), done);
        // Both the slow layout probe and slow capacity load are bypassed.
        mov_imm64(&mut a, RAX, 999); store_mem64_disp(&mut a, 8, 24, RAX);
        a.bind(done);
        for i in 0..3 { load_home(&mut a, RAX, FramedHome::Stack(i), 0); store_mem64_disp(&mut a, 8, i as i32 * 8, RAX); }
        a.extend_from_slice(&[0x41, 0x0f, 0x92, 0x40, 32]);
        store_mem64_disp(&mut a, 8, 40, 6);
        alu_r_imm(&mut a, 0, 4, 32); mov_rr(&mut a, RAX, 4); store_mem64_disp(&mut a, 8, 56, RAX); a.push(0xc3);
        let jit = egcl_rt::jit::JitBuffer::new(&a.finish().unwrap()).unwrap();
        let run: extern "C" fn(*mut u64) = unsafe { std::mem::transmute(jit.as_ptr()) };
        let mut out = [0u64; 8]; run(out.as_mut_ptr());
        assert_eq!(&out[..3], &[tagged, 8 << 3, 136 << 3]);
        assert_eq!(out[3], 0); assert_eq!(out[4], 0, "CLC remains the ordinary empty-MV promise");
        assert_eq!(out[5], 0x1234); assert_eq!(out[6], out[7]);
    }

    #[test]
    fn allocation_extent_completion_rejects_overlapping_homes_before_emission() {
        let (f, call, _, width) = paired_fixture(); let data = f.inst(call);
        let homes = HashMap::from([(data.args[0], FramedHome::Stack(0)),
            (data.args[1], FramedHome::Stack(1)), (data.args[2], FramedHome::Stack(2)),
            (data.results[0], FramedHome::Stack(3)), (width, FramedHome::Stack(4))]);
        let mut a = Asm::new(); let done = a.label();
        for alias in [data.results[0], width] {
            assert!(emit_ordinary_layout_extent(&mut a, &f, data, &homes, &HashMap::new(),
                true, (width, done, Some(alias))).is_none());
            assert_eq!(a.here(), 0, "preflight must precede bump emission or cursor commit");
        }
    }

}
