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
}
