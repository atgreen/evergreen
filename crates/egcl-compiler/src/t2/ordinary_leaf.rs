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
        None,
    )
}

/// Bound the leaf frame and conservative parallel-move snapshot headroom.
/// The largest block parameter list bounds both entry moves and phi edges.
pub(super) fn stack_requirement(frame_bytes: usize, move_values: usize) -> Option<usize> {
    let snapshot_bytes = move_values.checked_mul(8)?.checked_add(15)? & !15;
    let required = frame_bytes.checked_add(snapshot_bytes)?;
    (required <= isize::MAX as usize).then_some(required)
}

#[cfg(test)]
mod tests {
    use super::super::{
        FramedHome, HomeMoveSrc, mov_rr, parallel_home_move, pop_reg, push_reg, store_mem64_disp,
    };
    use super::*;
    use egcl_rt::{asm::Asm, jit::JitBuffer};
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};

    #[test]
    fn ordinary_stack_bound_covers_an_executed_parallel_phi_cycle() {
        let mut a = Asm::new();
        push_reg(&mut a, 5);
        mov_rr(&mut a, 5, 4);
        mov_rr(&mut a, 1, 7);
        mov_rr(&mut a, 8, 6);
        parallel_home_move(
            &mut a,
            &[
                (FramedHome::Reg(1), HomeMoveSrc::Home(FramedHome::Reg(8))),
                (FramedHome::Reg(8), HomeMoveSrc::Home(FramedHome::Reg(1))),
            ],
        );
        store_mem64_disp(&mut a, 2, 0, 1);
        store_mem64_disp(&mut a, 2, 8, 8);
        pop_reg(&mut a, 5);
        a.push(0xc3);
        let code = a.finish().unwrap();
        let buffer = JitBuffer::new(&code).unwrap();
        let run: extern "C" fn(u64, u64, *mut u64) =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        let mut swapped = [0; 2];
        run(11, 22, swapped.as_mut_ptr());
        assert_eq!(swapped, [22, 11]);

        let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
        let mut depth = 0usize;
        let mut maximum = 0usize;
        while decoder.can_decode() {
            let instruction = decoder.decode();
            match instruction.mnemonic() {
                Mnemonic::Push => depth += 8,
                Mnemonic::Pop => depth -= 8,
                Mnemonic::Sub if instruction.op0_register() == Register::RSP => {
                    depth += instruction.immediate32() as usize
                }
                Mnemonic::Add if instruction.op0_register() == Register::RSP => {
                    depth -= instruction.immediate32() as usize
                }
                _ => {}
            }
            maximum = maximum.max(depth);
        }
        assert_eq!(depth, 0);
        assert_eq!(maximum, 24, "RBP plus the actual cycle snapshot");
        assert_eq!(stack_requirement(8, 2), Some(maximum));
        assert_eq!(stack_requirement(usize::MAX, 2), None);
        assert_eq!(stack_requirement(8, usize::MAX), None);
    }
}
