// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Inline special-variable reads and assignments (bliss-csc8v).
//!
//! A global read or assignment keeps its IR identity and its runtime call:
//! the c2i helper in mapped code, or the private ordinary-ABI adapter in
//! ordinary code. In front of that call the emitter places a fast path that
//! accesses the symbol's value cell directly when the symbol's
//! `binding_guard` word allows it (see `egcl_rt::symbols`):
//!
//! - a read needs no execution to hold a dynamic binding (the low 31 bits are
//!   zero) and a bound cell; an unbound cell falls through so the call signals
//!   UNBOUND-VARIABLE;
//! - an assignment needs the whole word to be zero, which also excludes
//!   constants, whose assignment the call rejects.
//!
//! Otherwise the fast path falls through into the unchanged call. The fast
//! path neither allocates nor polls, so it needs no safepoint map, and it
//! leaves the multiple-value state untouched. A registry symbol's value cell
//! is a root of every collection, so the store needs no write barrier.
//! Only interned symbols qualify: their objects are pinned and never freed.

use super::{
    Asm, Cc, FramedHome, RAX, SCRATCH, load_home, mov_imm64, store_home, store_mem64_disp,
};
use crate::t2::ir::{AuxData, InstData, Value};
use std::collections::HashMap;

/// `mov rax, object` followed by a test of its guard word against `mask`,
/// branching to `slow` when any masked bit is set.
fn emit_guard(a: &mut Asm, object: u64, mask: u32, slow: egcl_rt::asm::Label) {
    let guard = u8::try_from(egcl_rt::symbols::SYMBOL_BINDING_GUARD_OFFSET).unwrap();
    mov_imm64(a, RAX, object as i64);
    a.extend_from_slice(&[0xF7, 0x40, guard]); // test dword [rax+guard], imm32
    a.extend_from_slice(&mask.to_le_bytes());
    a.jcc(Cc::Ne, slow);
}

/// Read: RAX = value cell, or branch to `slow`. Writes only RAX.
fn emit_load(a: &mut Asm, object: u64, slow: egcl_rt::asm::Label) {
    emit_guard(a, object, egcl_rt::symbols::BINDING_GUARD_COUNT_MASK, slow);
    let value = egcl_rt::symbols::SYMBOL_VALUE_OFFSET;
    super::load_mem64_disp(a, RAX, RAX, value);
    a.extend_from_slice(&[0x48, 0x83, 0xF8]); // cmp rax, imm8 (UNBOUND)
    a.push(u8::try_from(egcl_rt::value::UNBOUND_BITS).unwrap());
    a.jcc(Cc::E, slow);
}

/// Assignment of `src` (not RAX) to the value cell, or branch to `slow`.
/// Writes only RAX.
fn emit_store(a: &mut Asm, object: u64, src: u8, slow: egcl_rt::asm::Label) {
    debug_assert_ne!(src, RAX);
    emit_guard(a, object, u32::MAX, slow);
    store_mem64_disp(a, RAX, egcl_rt::symbols::SYMBOL_VALUE_OFFSET, src);
}

/// The symbol object for an inline access to `symbol`, if allowed.
fn object(symbol: u32) -> Option<u64> {
    egcl_rt::symbols::inline_access_object(symbol).map(|object| object as u64)
}

/// Mapped `SymbolValue`: RAX = the value and jump to the returned label,
/// which the caller binds where the helper's result arrives in RAX.
/// Otherwise fall through to the helper call.
pub(super) fn emit_mapped_load(a: &mut Asm, symbol: u32) -> Option<egcl_rt::asm::Label> {
    let object = object(symbol)?;
    let slow = a.label();
    let done = a.label();
    emit_load(a, object, slow);
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// Mapped `SetSymbolValue` with the value already in `src`: assign and jump
/// to the returned label, which the caller binds after the helper call.
/// Otherwise fall through to the helper call, `src` intact.
pub(super) fn emit_mapped_store(a: &mut Asm, symbol: u32, src: u8) -> Option<egcl_rt::asm::Label> {
    if src == RAX {
        return None;
    }
    let object = object(symbol)?;
    let slow = a.label();
    let done = a.label();
    emit_store(a, object, src, slow);
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// Which private adapter, if any, an ordinary call targets: `Some(false)` for
/// the global read and `Some(true)` for the assignment.
fn ordinary_adapter(target: u32) -> Option<bool> {
    use super::OrdinaryPrimitive;
    [
        (OrdinaryPrimitive::LoadGlobal, false),
        (OrdinaryPrimitive::StoreGlobal, true),
    ]
    .into_iter()
    .find(|(primitive, _)| egcl_rt::symbols::find_index(&primitive.adapter_name()) == Some(target))
    .map(|(_, store)| store)
}

/// The `(store, symbol)` of an ordinary call to the global read or assignment
/// adapter whose symbol operand is a constant.
fn ordinary_access(data: &InstData, constants: &HashMap<Value, u64>) -> Option<(bool, u32)> {
    let AuxData::CallTarget(target) = data.aux else {
        return None;
    };
    let store = ordinary_adapter(target)?;
    if data.args.len() != if store { 2 } else { 1 } || data.results.len() > 1 {
        return None;
    }
    let symbol = egcl_rt::value::EgclVal(*constants.get(&data.args[0])?);
    if !symbol.is_symbol() || symbol == egcl_rt::value::NIL || symbol == egcl_rt::value::T {
        return None;
    }
    Some((store, symbol.as_symbol_index()))
}

/// Ordinary call to a global read/assignment adapter: on success leave the
/// primary result in RAX (the value read, or NIL after an assignment, as the
/// adapter returns) with CF clear and jump to the returned label, which the
/// caller binds where the call's own result arrives. On failure fall through
/// to the call. Writes only RAX and RDX (neither is a value home).
///
/// `mv_empty` says the multiple-value buffer is known to be empty on entry:
/// the adapter empties it, and a CF-clear ordinary return promises an empty
/// buffer, so without it nothing is emitted.
pub(super) fn emit_ordinary_fast_path(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_empty: bool,
) -> Option<egcl_rt::asm::Label> {
    if !mv_empty {
        return None;
    }
    let (store, symbol) = ordinary_access(data, constants)?;
    let object = object(symbol)?;
    // Resolve the stored operand before emitting anything.
    let source = if store {
        let value = data.args[1];
        Some(match constants.get(&value) {
            Some(&bits) => Err(bits),
            None => Ok(*homes.get(&value)?),
        })
    } else {
        None
    };
    let slow = a.label();
    let done = a.label();
    if let Some(source) = source {
        match source {
            Err(bits) => mov_imm64(a, SCRATCH, bits as i64),
            Ok(home) => load_home(a, SCRATCH, home, 0),
        }
        emit_store(a, object, SCRATCH, slow);
        mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
    } else {
        emit_load(a, object, slow);
    }
    a.push(0xf8); // clc: one value, MV buffer untouched (and empty)
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// Mapped transfer-protocol global access (an `Invoke` carrying
/// `GlobalValue`): on success store the result in its home (the value read,
/// or NIL after an assignment, as the transfer returns) and jump to the
/// returned label, which the caller binds after the whole call sequence, its
/// root restoration included; nothing moved, so the homes the call would have
/// reloaded are already current. On failure fall through to the call. Writes
/// only RAX and RDX. The transfer leaves the multiple values alone, as does
/// this path.
///
/// The transfer resolves the symbol through the evaluator, which consults a
/// lexical binding of a symbol that is not special. Only a symbol that is
/// already globally special qualifies: it reads the binding overlay or the
/// value cell, and a proclamation is never withdrawn.
pub(super) fn emit_transfer_fast_path(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
) -> Option<egcl_rt::asm::Label> {
    let AuxData::GlobalValue { symbol, store } = data.aux else {
        return None;
    };
    if data.args.len() != usize::from(store)
        || data.results.len() > 1
        || !super::is_global_special(symbol)
    {
        return None;
    }
    let object = object(symbol)?;
    let result = match data.results.first() {
        Some(value) => Some(*homes.get(value)?),
        None => None,
    };
    let source = if store {
        let value = data.args[0];
        Some(match constants.get(&value) {
            Some(&bits) => Err(bits),
            None => Ok(*homes.get(&value)?),
        })
    } else {
        None
    };
    let slow = a.label();
    let done = a.label();
    if let Some(source) = source {
        match source {
            Err(bits) => mov_imm64(a, SCRATCH, bits as i64),
            Ok(home) => load_home(a, SCRATCH, home, 0),
        }
        emit_store(a, object, SCRATCH, slow);
        mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
    } else {
        emit_load(a, object, slow);
    }
    if let Some(result) = result {
        store_home(a, result, RAX, 0);
    }
    a.jmp(done);
    a.bind(slow);
    Some(done)
}
