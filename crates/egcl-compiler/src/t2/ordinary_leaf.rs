// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Initial ordinary ABI emission. This bounded, noncollecting subset shares
//! the framed body emitter without borrowing the legacy register-entry ABI.

use super::{EmitError, EnvironmentCalls, FramedCode};
use crate::t2::ir::{Function, Opcode, ValueRepresentation};

#[derive(Clone, Copy, Debug)]
pub struct OrdinaryLispEntryOptions {
    pub arity: usize,
    /// A machine entry using the ordinary Lisp ABI. Wrong arity tail-jumps
    /// here before changing any argument, count, closure, context or frame.
    /// The veneer must publish every actual argument and closure before any
    /// collecting/unwinding helper; it cannot use the rejected fixed arity.
    pub arity_error_veneer: u64,
}

/// Emit an environment-free ordinary entry for a bounded tagged leaf.
///
/// The caller supplies NIL in RAX and an execution pointer in R13, with an
/// inactive multiple-value buffer. The returned artifact is not publication
/// authorization: the runtime must establish segment ownership, entry roots,
/// depth/stack/poll admission and a rooted arity-error veneer first.
///
/// Calls, guards, loops, environment reads, heap literals, OSR, variadic or
/// declared entry checks, unboxed values and bodies over 64 instructions are
/// explicitly declined. Existing managed/register entries retain their own
/// metadata; `ordinary_entry` alone identifies this new calling convention.
pub fn emit_framed_ordinary_leaf(
    f: &Function,
    options: OrdinaryLispEntryOptions,
) -> Result<FramedCode, EmitError> {
    let unsupported = || EmitError::UnsupportedOp(0xFA);
    if options.arity > 4
        || options.arity != f.block(f.entry()).params.len()
        || options.arity_error_veneer == 0
        || f.is_variadic()
        || !f.osr_entries.is_empty()
        || !f.handler_cases.is_empty()
        || f.block(f.entry())
            .params
            .iter()
            .any(|&v| f.is_entry_param_checked(v))
        || crate::t2::verify::verify(f).is_err()
    {
        return Err(unsupported());
    }
    let mut instructions = 0;
    for (index, &block) in f.block_order().iter().enumerate() {
        if f.block(block)
            .params
            .iter()
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
        {
            return Err(unsupported());
        }
        for &inst in &f.block(block).insts {
            instructions += 1;
            let data = f.inst(inst);
            if instructions > 64
                || data.flags.call
                || data.flags.guard
                || data.flags.safepoint
                || data
                    .args
                    .iter()
                    .chain(&data.results)
                    .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
            {
                return Err(unsupported());
            }
            match data.opcode {
                Opcode::ConstFixnum
                | Opcode::ConstNil
                | Opcode::ConstT
                | Opcode::ConstSymbol
                | Opcode::ConstChar => {}
                Opcode::Return if data.args.len() == 1 => {}
                Opcode::Jump
                    if data.targets.len() == 1
                        && f.block_order()[index + 1..].contains(&data.targets[0].block) => {}
                other => return Err(EmitError::UnsupportedOp(super::op_tag(other))),
            }
        }
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
        None,
        None,
        &[],
        0,
        EnvironmentCalls::default(),
        None,
        Some(options),
    )
}
