// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Builtin semantics are valid only while the symbol has no installed override.

use super::frame_state::FrameStateId;
use super::ir::{AuxData, Block, Function, InstData, InstFlags, Opcode};

/// Capture the exact absent binding before selecting a builtin expansion.
/// A callable already installed in the cell must remain a normal call.
pub fn snapshot(symbol: u32) -> Option<AuxData> {
    if egcl_rt::symbols::is_uninterned(symbol) {
        return None;
    }
    let binding = egcl_rt::symbols::symbol_function(symbol)?;
    if binding != egcl_rt::value::UNBOUND && binding != egcl_rt::value::NIL {
        return None;
    }
    let object = egcl_rt::symbols::symbol_object_ptr(symbol)?;
    Some(AuxData::BuiltinBinding {
        address: object + core::mem::offset_of!(egcl_rt::object::SymbolData, function),
        expected: binding.to_raw(),
    })
}

/// Keep the pre-call state and an ordered guard even if the replacement
/// operation is pure or constant-folded. No earlier effects may be replayed.
pub fn insert(
    function: &mut Function,
    block: Block,
    position: usize,
    state: FrameStateId,
    binding: AuxData,
) {
    assert!(matches!(binding, AuxData::BuiltinBinding { .. }));
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
            aux: binding,
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
