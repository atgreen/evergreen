// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary-only, noncollecting recovery serialization. Every runtime CALL
//! retains its distinct exact-PC map; only the preceding machine work is shared.
use super::*;
use crate::t2::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use crate::t2::ir::{Value, ValueRepresentation};
use crate::t2::x64_calls::{NativeCallOrigin, NativeCallSite, NativeCallValues, NativeStackBase};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub(super) enum Source {
    Home(FramedHome, bool),
    Imm(u64),
}
pub(super) struct Site {
    pub label: egcl_rt::asm::Label,
    pub frame_state: FrameStateId,
    pub n_scopes: u32,
    pub sources: Vec<Source>,
    pub f64_slots: Box<[u32]>,
}
pub(super) struct Candidate {
    pub asm: Asm,
    pub calls: Vec<NativeCallSite>,
    pub values: HashMap<usize, NativeCallValues>,
    pub helper_start: usize,
    pub helper_end: usize,
    pub data_offset: usize,
    pub data_bytes: usize,
}

pub(super) fn enabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    !*DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_SHARED_DEOPT_RECOVERY").is_some())
}

pub(super) fn source(
    input: &ValueSource,
    fs: &FrameState,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    depth: usize,
) -> Option<Source> {
    if depth > 64 {
        return None;
    }
    match input {
        ValueSource::Value { value, repr } => {
            if !matches!(
                repr,
                ValueRepresentation::Tagged | ValueRepresentation::UnboxedFixnum
            ) {
                return None;
            }
            let shifted = *repr == ValueRepresentation::UnboxedFixnum;
            if let Some(&bits) = constants.get(value) {
                Some(Source::Imm(if shifted { bits << 3 } else { bits }))
            } else {
                Some(Source::Home(*homes.get(value)?, shifted))
            }
        }
        ValueSource::Const(value) if !is_moving_gc_reference(*value) => Some(Source::Imm(value.0)),
        ValueSource::Const(_) => None,
        ValueSource::Unbound => Some(Source::Imm(egcl_rt::value::UNBOUND.0)),
        ValueSource::Remat(id) => {
            let recipe = fs.remat.get(id.0 as usize)?;
            if !matches!(
                recipe.op,
                RematOp::Const
                    | RematOp::BoxFixnum
                    | RematOp::UnboxFixnum
                    | RematOp::BoxFloat
                    | RematOp::UnboxFloat
            ) {
                return None;
            }
            source(recipe.inputs.first()?, fs, homes, constants, depth + 1)
        }
    }
}

fn add_register(registers: &mut Vec<u8>, home: FramedHome) -> Option<()> {
    if let FramedHome::Reg(reg) = home {
        if matches!(reg, 0 | 2 | 4 | 5 | 11) || reg >= 16 {
            return None;
        }
        if !registers.contains(&reg) {
            registers.push(reg);
        }
    }
    Some(())
}
fn home_offset(
    home: FramedHome,
    registers: &[u8],
    snapshot: usize,
    allocation: usize,
) -> Option<u32> {
    let offset = match home {
        FramedHome::Reg(reg) => {
            snapshot.checked_add(registers.iter().position(|&r| r == reg)?.checked_mul(8)?)?
        }
        FramedHome::Stack(slot) => allocation.checked_add((slot as usize).checked_mul(8)?)?,
    };
    let offset = u32::try_from(offset).ok()?;
    (offset < (1 << 29) && offset % 8 == 0).then_some(offset)
}
fn narrow_recipe(word: u32) -> Option<u16> {
    let kind = word >> 29;
    let payload = word & 0x1fff_ffff;
    let payload = match kind {
        0 | 1 if payload % 8 == 0 => payload / 8,
        2 => payload,
        _ => return None,
    };
    (payload < (1 << 14)).then_some(((kind << 14) | payload) as u16)
}

fn load_dword(a: &mut Asm, dst: u8, base: u8, displacement: i32) {
    assert!(!matches!(base & 7, 4));
    a.push(0x40 | if dst >= 8 { 4 } else { 0 } | if base >= 8 { 1 } else { 0 });
    a.extend_from_slice(&[0x8b, 0x80 | ((dst & 7) << 3) | (base & 7)]);
    a.extend_from_slice(&displacement.to_le_bytes());
}

/// The caller owns a clone of the unchanged hot prefix and commits this result
/// only after comparing its ENTIRE suffix (including embedded data) with legacy.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_candidate(
    mut a: Asm,
    sites: &[Site],
    native_slots: u32,
    control: FramedHome,
    veneer: u64,
    epilogue: impl Fn(&mut Asm),
) -> Option<Candidate> {
    if sites.len() < 2 {
        return None;
    }
    let mut registers = Vec::new();
    add_register(&mut registers, control)?;
    for site in sites {
        if site.n_scopes == 0 || site.sources.is_empty() {
            return None;
        }
        for source in &site.sources {
            if let Source::Home(home, _) = source {
                add_register(&mut registers, *home)?;
            }
        }
    }
    let payload = sites
        .iter()
        .map(|s| s.sources.len())
        .max()?
        .checked_mul(8)?;
    let descriptor_slot = payload.checked_add(32)?;
    let snapshot = descriptor_slot.checked_add(8)?;
    let allocation = snapshot
        .checked_add(registers.len().checked_mul(8)?)?
        .checked_add(15)?
        & !15;
    if allocation > i32::MAX as usize {
        return None;
    }
    let control_offset = home_offset(control, &registers, snapshot, allocation)?;
    let mut pool = Vec::<u64>::new();
    let mut recipes = Vec::new();
    for site in sites {
        let mut words = Vec::new();
        for source in &site.sources {
            let word = match source {
                Source::Home(home, shift) => {
                    home_offset(*home, &registers, snapshot, allocation)?
                        | if *shift { 1 << 29 } else { 0 }
                }
                Source::Imm(bits) => {
                    let index = match pool.iter().position(|word| word == bits) {
                        Some(i) => i,
                        None => {
                            pool.push(*bits);
                            pool.len() - 1
                        }
                    };
                    let index = u32::try_from(index).ok()?;
                    if index >= 1 << 29 {
                        return None;
                    }
                    index | (2 << 29)
                }
            };
            words.push(word);
        }
        recipes.push(words);
    }
    // Narrow homes encode aligned stack offsets in words; immediate recipes
    // encode pool indices. Choose once, so decoding adds no per-word width test.
    let narrow = recipes
        .iter()
        .flatten()
        .all(|word| narrow_recipe(*word).is_some());
    let serializer = a.label();
    let descriptors: Vec<_> = sites.iter().map(|_| a.label()).collect();
    let continuations: Vec<_> = sites.iter().map(|_| a.label()).collect();
    let pool_label = a.label();
    for (site, descriptor) in sites.iter().zip(&descriptors) {
        a.bind(site.label);
        a.lea_rip_label(11, *descriptor);
        a.jmp(serializer);
    }
    a.bind(serializer);
    let helper_start = a.here();
    ordinary_call::reserve_ordinary_stack(&mut a, allocation);
    // Save all potential value/control homes BEFORE borrowing any of their GPRs.
    // R13 (execution), RBP, and RSP are never used as loop scratch.
    for (index, &reg) in registers.iter().enumerate() {
        store_to_rsp(&mut a, reg, (snapshot + index * 8) as i32);
    }
    store_to_rsp(&mut a, 11, descriptor_slot as i32);
    a.lea_rip_label(10, pool_label);
    load_dword(&mut a, 9, 11, 4); // remaining words
    mov_rr(&mut a, 1, 11);
    alu_r_imm(&mut a, 0, 1, 12); // recipe pointer
    mov_rr(&mut a, 8, 4); // payload destination
    let again = a.label();
    let immediate = a.label();
    let store = a.label();
    a.bind(again);
    if narrow {
        a.extend_from_slice(&[0x0f, 0xb7, 0x11]); // movzx edx,word [rcx]
    } else {
        a.extend_from_slice(&[0x8b, 0x11]); // mov edx,dword [rcx]
    }
    a.extend_from_slice(&[0x89, 0xd0, 0x25]); // eax=edx; and eax,mask
    a.extend_from_slice(&(if narrow { 0x3fffu32 } else { 0x1fff_ffff }).to_le_bytes());
    a.extend_from_slice(&[0xc1, 0xea, if narrow { 14 } else { 29 }, 0x83, 0xfa, 2]);
    a.jcc(Cc::E, immediate);
    if narrow {
        shl_imm(&mut a, RAX, 3); // normalized home word offset -> bytes
    }
    a.extend_from_slice(&[0x48, 0x8b, 0x04, 0x04]); // rax=[rsp+rax]
    a.extend_from_slice(&[0x83, 0xfa, 1]);
    a.jcc(Cc::Ne, store);
    shl_imm(&mut a, RAX, 3);
    a.jmp(store);
    a.bind(immediate);
    a.extend_from_slice(&[0x49, 0x8b, 0x04, 0xc2]); // rax=[r10+rax*8]
    a.bind(store);
    a.extend_from_slice(&[
        0x49,
        0x89,
        0x00,
        0x49,
        0x83,
        0xc0,
        8,
        0x48,
        0x83,
        0xc1,
        if narrow { 2 } else { 4 },
        0x49,
        0x83,
        0xe9,
        1,
    ]);
    a.jcc(Cc::Ne, again);
    load_from_rsp(&mut a, 11, descriptor_slot as i32);
    load_dword(&mut a, RAX, 11, 0);
    store_to_rsp(&mut a, RAX, payload as i32);
    load_dword(&mut a, RAX, 11, 4);
    store_to_rsp(&mut a, RAX, (payload + 8) as i32);
    mov_rr(&mut a, RAX, 4);
    store_to_rsp(&mut a, RAX, (payload + 16) as i32);
    load_from_rsp(&mut a, RAX, control_offset as i32);
    store_to_rsp(&mut a, RAX, (payload + 24) as i32);
    mov_rr(&mut a, 7, 4);
    alu_r_imm(&mut a, 0, 7, payload as i32);
    // All value homes have already been serialized. RAX is now the shared
    // veneer target, retained through the descriptor dispatch in RCX.
    mov_imm64(&mut a, RAX, veneer as i64);
    a.extend_from_slice(&[0x49, 0x63, 0x4b, 8, 0x4c, 0x01, 0xd9, 0xff, 0xe1]); // rcx=signext[desc+8]+desc; jmp rcx
    let helper_end = a.here();
    let mut calls = Vec::new();
    let mut values = HashMap::new();
    let returned = a.label();
    for (site, &continuation) in sites.iter().zip(&continuations) {
        a.bind(continuation);
        let mut call = emit_runtime_helper_call(&mut a, 0, None);
        call.stack_adjust = u32::try_from(allocation).ok()?;
        values.insert(
            call.return_offset,
            NativeCallValues::Deoptimizing {
                native_slots,
                f64_slots: site.f64_slots.clone(),
            },
        );
        calls.push(call.site(
            NativeCallOrigin::Deopt(site.frame_state),
            NativeStackBase::Body,
        ));
        // Keep every exact CALL return PC distinct. The jump and shared LEA
        // preserve native return values and CF while discarding stale snapshots.
        a.jmp(returned);
    }
    a.bind(returned);
    a.extend_from_slice(&[0x48, 0x8d, 0xa4, 0x24]);
    a.extend_from_slice(&(allocation as i32).to_le_bytes());
    epilogue(&mut a);
    a.push(0xc3);
    let data_offset = a.here();
    while a.here() % 8 != 0 {
        a.push(0);
    }
    for (((site, recipe), &descriptor), &continuation) in sites
        .iter()
        .zip(&recipes)
        .zip(&descriptors)
        .zip(&continuations)
    {
        a.bind(descriptor);
        a.extend_from_slice(&site.n_scopes.to_le_bytes());
        a.extend_from_slice(&u32::try_from(recipe.len()).ok()?.to_le_bytes());
        a.relative_i32(continuation, descriptor);
        for word in recipe {
            if narrow {
                let encoded = narrow_recipe(*word)?;
                a.extend_from_slice(&encoded.to_le_bytes());
            } else {
                a.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    while a.here() % 8 != 0 {
        a.push(0);
    }
    a.bind(pool_label);
    for word in pool {
        a.extend_from_slice(&word.to_le_bytes());
    }
    if a.here() > i32::MAX as usize {
        return None;
    }
    let data_bytes = a.here() - data_offset;
    Some(Candidate {
        asm: a,
        calls,
        values,
        helper_start,
        helper_end,
        data_offset,
        data_bytes,
    })
}

#[cfg(test)]
#[path = "shared_deopt_tests.rs"]
mod tests;
