// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Standard CL primitives have compiler-known semantics. Implementation
//! primitives retain exact mutable-binding guards.

use super::frame_state::FrameStateId;
use super::ir::{AuxData, Block, Function, InstData, InstFlags, Opcode};

/// Whether an admitted expansion needs a mutable function-cell check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    FixedStandard,
    Guarded { address: usize, expected: u64 },
}

/// Only exact canonical identities of currently implemented CL templates are
/// fixed. Package bootstrap seeds these ANSI symbols under bare registry keys;
/// package-local/shadowed symbols instead have distinct qualified keys. Never
/// use SYMBOL-NAME here: an uninterned or shadowed symbol may print identically.
fn fixed_standard_primitive(symbol: u32) -> bool {
    let Some(key) = egcl_rt::symbols::registry_key(symbol) else {
        return false;
    };
    matches!(
        key.as_str(),
        "+" | "-"
            | "*"
            | "<"
            | ">"
            | "<="
            | ">="
            | "="
            | "1+"
            | "1-"
            | "LOGAND"
            | "LOGIOR"
            | "LOGXOR"
            | "LOGNOT"
            | "MOD"
            | "REM"
            | "FLOOR"
            | "TRUNCATE"
            | "MIN"
            | "MAX"
            | "ASH"
            | "NOT"
            | "NULL"
            | "EQ"
            | "CAR"
            | "CDR"
            | "FIRST"
            | "REST"
            | "SYMBOLP"
            | "INTEGERP"
            | "TYPEP"
            | "STRINGP"
            | "ZEROP"
            | "PLUSP"
            | "MINUSP"
            | "EVENP"
            | "ODDP"
    ) && egcl_rt::symbols::find_index(&key) == Some(symbol)
}

/// Standard CL redefinition is unsupported, so its canonical primitives may
/// be open-coded without reading the function cell. Other expansions retain
/// the exact absent-binding snapshot; installed overrides remain normal calls.
pub fn snapshot(symbol: u32) -> Option<Binding> {
    if fixed_standard_primitive(symbol) {
        return Some(Binding::FixedStandard);
    }
    if egcl_rt::symbols::is_uninterned(symbol) {
        return None;
    }
    let binding = egcl_rt::symbols::symbol_function(symbol)?;
    if binding != egcl_rt::value::UNBOUND && binding != egcl_rt::value::NIL {
        return None;
    }
    let object = egcl_rt::symbols::symbol_object_ptr(symbol)?;
    Some(Binding::Guarded {
        address: object + core::mem::offset_of!(egcl_rt::object::SymbolData, function),
        expected: binding.to_raw(),
    })
}

/// Keep the pre-call state and an ordered guard for mutable primitives even
/// if the replacement operation is pure or constant-folded. Standard primitive
/// selection does not remove the operation's own type/overflow or MV effects.
pub fn insert(
    function: &mut Function,
    block: Block,
    position: usize,
    state: FrameStateId,
    binding: Binding,
) {
    let Binding::Guarded { address, expected } = binding else {
        return;
    };
    // Operand-guard materialization rewrites the later operation's state.
    // A private snapshot prevents those new values from leaking backward into
    // this earlier binding check before their defining guards have executed.
    let state = function
        .frame_states
        .add(function.frame_states.get(state).clone());
    let (guard, _) = function.push_inst(
        block,
        InstData {
            opcode: Opcode::Guard,
            args: vec![],
            results: vec![],
            aux: AuxData::BuiltinBinding { address, expected },
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: Some(state),
            source_pos: 0,
        },
        &[],
    );
    let instructions = &mut function.block_mut(block).insts;
    assert_eq!(instructions.pop(), Some(guard));
    instructions.insert(position, guard);
}
