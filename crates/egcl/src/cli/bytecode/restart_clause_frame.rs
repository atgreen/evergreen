// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Discard only the conservatively inherited frame of a fully lowered literal
//! restart clause. Capture discovery still happens before macro expansion.

use super::{BytecodeFunction, Instr};

pub(super) fn elide_unused_literal_frame(function: &mut BytecodeFunction) {
    if literal_without_frame(function) {
        function.has_env = false;
    }
}

fn literal_without_frame(function: &BytecodeFunction) -> bool {
    // No binder may evaluate a deferred default or bind a SPECIAL parameter.
    // In particular, an in-range local index alone would not prove that a slot
    // is independent of the surrounding lexical frame.
    if function.variadic
        || function.arity != 0
        || function.min_args != 0
        || function.max_args != Some(0)
        || !function.params_form.is_nil()
        || !function.param_layout.is_empty()
        || !function.param_types.is_empty()
        || function.n_locals != 0
        || !function.names.is_empty()
        || !function.load_time_values.is_empty()
        || !function.nested_functions.is_empty()
        || !function.handler_cases.is_empty()
        || !function.handler_binds.is_empty()
        || !function.restart_cases.is_empty()
    {
        return false;
    }
    // This small straight-line language has no name lookup, call, external
    // control or delayed evaluation. Constants retain their existing precise
    // roots and identity, including quoted heap data and literal option forms.
    let mut depth = 0usize;
    for (pc, instruction) in function.code.iter().enumerate() {
        match instruction {
            Instr::Const(index) if (*index as usize) < function.constants.len() => depth += 1,
            Instr::Pop if depth != 0 => depth -= 1,
            Instr::ClearMv => {}
            Instr::Return => return depth == 1 && pc + 1 == function.code.len(),
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests;
