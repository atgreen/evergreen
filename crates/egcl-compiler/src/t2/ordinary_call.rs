// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bounded ordinary calls that forward the complete callee result immediately.
use super::*;
use crate::t2::ir::{AuxData, InstData, Opcode, Value, ValueRepresentation};
use egcl_rt::call_table::{
    ORDINARY_ENV_NONE, ORDINARY_X64_LISP_KIND, ORDINARY_X64_LISP_VERSION, OrdinaryEntryDescriptor,
};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct OrdinaryCallTarget {
    pub symbol: u32,
    pub descriptor_slot: u64,
    pub revision_address: u64,
}

/// Process-wide atomic cells, independent of each target's cell revision.
#[derive(Clone, Copy)]
pub struct OrdinaryCallFlags {
    pub gc_requested: u64,
    pub sigusr1_pending: u64,
    pub posted_signal: u64,
    pub native_errors: u64,
    pub signal_epoch: u64,
}

/// The cold veneer publishes the native caller before collecting or unwinding.
/// Arguments remain in the caller's exact mapped slice. The veneer returns a
/// completed ordinary result in RAX/RDX/RCX/CF; it must not request a retry.
#[repr(C)]
pub struct OrdinaryColdRequest {
    pub symbol: u64,
    pub nargs: usize,
    pub args: *mut egcl_rt::value::EgclVal,
    pub closure: egcl_rt::value::EgclVal,
    pub control: *mut NativeRecursiveControl,
    pub descriptor_slot: u64,
    pub revision_address: u64,
    pub reserved: u64,
}

const _: () = {
    assert!(std::mem::size_of::<OrdinaryColdRequest>() == 64);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, closure) == 24);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, control) == 32);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, revision_address) == 48);
};

#[derive(Clone, Copy)]
pub(super) struct OrdinaryCallEmission {
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
}

/// Emit a single named ordinary call followed immediately by returning its
/// complete result. Only tagged fixed 0–4 argument, environment-free callers
/// with immediate constants are accepted. This artifact has ONLY an ordinary
/// entry; offset zero traps. A retained segment, rooted arity veneer and valid
/// R13 execution pointer are required before invocation. This does not install
/// or publish code. Cold helpers must implement `OrdinaryColdRequest` above.
pub fn emit_framed_ordinary_call(
    f: &Function,
    entry: OrdinaryLispEntryOptions,
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
) -> Result<FramedCode, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    if f.block_order().len() != 1
        || f.is_variadic()
        || !f.osr_entries.is_empty()
        || !f.handler_cases.is_empty()
        || entry.arity > 4
        || entry.arity_error_veneer == 0
        || f.block(f.entry()).params.len() != entry.arity
        || f.block(f.entry())
            .params
            .iter()
            .any(|&v| f.is_entry_param_checked(v))
        || [
            target.descriptor_slot,
            target.revision_address,
            cold,
            flags.gc_requested,
            flags.sigusr1_pending,
            flags.posted_signal,
            flags.native_errors,
            flags.signal_epoch,
        ]
        .contains(&0)
        || crate::t2::verify::verify(f).is_err()
        || (0..f.num_values()).any(|v| f.value(Value(v as u32)).repr != ValueRepresentation::Tagged)
    {
        return Err(bad());
    }
    let insts = &f.block(f.entry()).insts;
    if insts.len() < 2 || insts.len() > 64 {
        return Err(bad());
    }
    let call = f.inst(insts[insts.len() - 2]);
    let ret = f.inst(insts[insts.len() - 1]);
    if call.opcode != Opcode::Call
        || call.args.len() > 4
        || call.results.len() != 1
        || !matches!(call.aux, AuxData::CallTarget(symbol) if symbol == target.symbol)
        || !call.flags.call
        || !call.flags.safepoint
        || ret.opcode != Opcode::Return
        || ret.args != call.results
        || insts[..insts.len() - 2].iter().any(|&i| {
            !matches!(
                f.inst(i).opcode,
                Opcode::ConstFixnum
                    | Opcode::ConstNil
                    | Opcode::ConstT
                    | Opcode::ConstSymbol
                    | Opcode::ConstChar
            )
        })
    {
        return Err(bad());
    }
    super::emit_framed_inner(
        f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        Some(entry.arity as u16),
        None,
        &[],
        0,
        EnvironmentCalls::default(),
        None,
        Some(entry),
        Some(OrdinaryCallEmission {
            target,
            flags,
            cold,
        }),
    )
}

fn unsigned_branch(a: &mut Asm, inclusive: bool, target: egcl_rt::asm::Label) {
    let at = a.here();
    a.jcc(Cc::E, target);
    // Shared Cc deliberately only models portable signed conditions. x86
    // admission uses unsigned addresses and explicit borrow tests.
    a.patch_u8(at + 1, if inclusive { 0x86 } else { 0x82 }); // JBE / JB
}

fn retire(a: &mut Asm, bytes: u32) {
    if bytes != 0 {
        a.extend_from_slice(&[0x48, 0x8d, 0xa4, 0x24]);
        a.extend_from_slice(&bytes.to_le_bytes()); // lea rsp,[rsp+bytes], preserve CF
    }
}

impl OrdinaryCallEmission {
    pub(super) fn emit(
        self,
        a: &mut Asm,
        data: &InstData,
        homes: &HashMap<Value, FramedHome>,
        constants: &HashMap<Value, u64>,
        outgoing: NativeValueArea,
        control_home: FramedHome,
    ) -> Result<Vec<CallReturn>, EmitError> {
        let NativeValueArea::NativeStack(base) = outgoing else {
            return Err(EmitError::UnsupportedOp(0xFD));
        };
        let cold = a.label();
        let done = a.label();
        let overflow = if data.args.len() == 4 { 16 } else { 0 };
        for (i, value) in data.args.iter().enumerate() {
            if let Some(&bits) = constants.get(value) {
                mov_imm64(a, RAX, bits as i64);
            } else {
                load_home(a, RAX, homes[value], 0);
            }
            store_home(a, FramedHome::Stack(base + i as u32), RAX, 0);
        }
        load_home(a, 7, control_home, 0);
        alu_r_imm(a, 7, 7, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(a, RAX, 7, 0);
        cmp_rr(a, RAX, 13);
        a.jcc(Cc::Ne, cold);
        for (address, byte) in [
            (self.flags.gc_requested, true),
            (self.flags.sigusr1_pending, true),
            (self.flags.posted_signal, false),
            (self.flags.native_errors, false),
        ] {
            mov_imm64(a, 11, address as i64);
            if byte {
                movzx_mem8(a, RAX, 11);
            } else {
                load_mem64_disp(a, RAX, 11, 0);
            }
            alu_r_imm(a, 7, RAX, 0);
            a.jcc(Cc::Ne, cold);
        }
        mov_imm64(a, 11, self.flags.signal_epoch as i64);
        load_mem64_disp(a, RAX, 11, 0);
        cmp_mem64_disp8(a, RAX, 7, 48);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            13,
            egcl_rt::exec_context::offsets::CARRIER_ID as i32,
        );
        cmp_mem64_disp8(a, RAX, 7, 40);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            13,
            egcl_rt::exec_context::offsets::YIELD_REQUESTED as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        mov_rr(a, 11, 7);
        alu_r_imm(a, 0, 11, 64);
        movzx_mem8(a, RAX, 11);
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(a, 11, 7, 24);
        movzx_mem8(a, RAX, 11);
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(a, 11, 7, 8);
        a.extend_from_slice(&[0x41, 0x8b, 0x03, 0x8b, 0x57, 56]); // eax=*depth; edx=limit
        cmp_rr(a, RAX, SCRATCH);
        a.jcc(Cc::Ge, cold);

        mov_imm64(a, 11, self.target.descriptor_slot as i64);
        load_mem64_disp(a, 11, 11, 0);
        alu_r_imm(a, 7, 11, 0);
        a.jcc(Cc::E, cold);
        for (offset, expected) in [
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, kind),
                ORDINARY_X64_LISP_KIND,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, version),
                ORDINARY_X64_LISP_VERSION,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, arity),
                data.args.len() as u64,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, environment),
                ORDINARY_ENV_NONE,
            ),
        ] {
            load_mem64_disp(a, RAX, 11, offset as i32);
            mov_imm64(a, SCRATCH, expected as i64);
            cmp_rr(a, RAX, SCRATCH);
            a.jcc(Cc::Ne, cold);
        }
        mov_imm64(a, SCRATCH, self.target.revision_address as i64);
        load_mem64_disp(a, RAX, SCRATCH, 0);
        cmp_mem64_disp8(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, revision) as u8,
        );
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, owner) as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_start) as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_len) as i32,
        );
        alu_rr(a, 0x01, RAX, SCRATCH);
        unsigned_branch(a, false, cold); // reject range overflow
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, entry) as i32,
        );
        cmp_rr(a, RAX, SCRATCH);
        unsigned_branch(a, true, cold); // entry >= end
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_start) as i32,
        );
        cmp_rr(a, SCRATCH, RAX);
        unsigned_branch(a, false, cold); // entry < start
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, stack_required) as i32,
        );
        alu_r_imm(a, 7, SCRATCH, 8);
        a.jcc(Cc::L, cold); // also rejects high-bit bounds
        mov_rr(a, RAX, 4);
        alu_r_imm(a, 5, RAX, overflow + 8);
        unsigned_branch(a, false, cold);
        alu_rr(a, 0x29, RAX, SCRATCH);
        unsigned_branch(a, false, cold);
        load_mem64_disp(a, SCRATCH, 7, 88);
        cmp_rr(a, RAX, SCRATCH);
        unsigned_branch(a, true, cold);
        load_mem64_disp(a, SCRATCH, 7, 8);
        a.extend_from_slice(&[0xff, 0x02]); // inc dword[rdx]
        load_mem64_disp(
            a,
            11,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, entry) as i32,
        );
        if overflow != 0 {
            alu_r_imm(a, 5, 4, overflow);
            load_home(a, RAX, FramedHome::Stack(base + 3), overflow);
            store_to_rsp(a, RAX, 0);
        }
        for (index, reg) in [7, 6, 2].into_iter().take(data.args.len()).enumerate() {
            load_home(a, reg, FramedHome::Stack(base + index as u32), overflow);
        }
        mov_imm64(a, 1, data.args.len() as i64);
        mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
        a.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11, genuine ordinary ABI
        let direct = CallReturn {
            return_offset: a.here(),
            stack_adjust: overflow as u32,
        };
        load_home(a, 11, control_home, overflow);
        load_mem64_disp(a, 11, 11, 8);
        a.extend_from_slice(&[0x41, 0xff, 0x0b]); // DEC preserves CF and all result registers
        retire(a, overflow as u32);
        a.jmp(done);

        a.bind(cold);
        alu_r_imm(a, 5, 4, 64);
        for (offset, bits) in [
            (0, self.target.symbol as u64),
            (8, data.args.len() as u64),
            (24, egcl_rt::value::NIL.0),
            (40, self.target.descriptor_slot),
            (48, self.target.revision_address),
            (56, 0),
        ] {
            mov_imm64(a, RAX, bits as i64);
            store_to_rsp(a, RAX, offset);
        }
        mov_rr(a, RAX, 4);
        alu_r_imm(a, 0, RAX, (base * 8 + 64) as i32);
        store_to_rsp(a, RAX, 16);
        load_home(a, RAX, control_home, 64);
        store_to_rsp(a, RAX, 32);
        mov_rr(a, 7, 4);
        mov_imm64(a, 11, self.cold as i64);
        a.extend_from_slice(&[0x41, 0xff, 0xd3]);
        let slow = CallReturn {
            return_offset: a.here(),
            stack_adjust: 64,
        };
        retire(a, 64);
        a.bind(done);
        Ok(vec![direct, slow])
    }
}
