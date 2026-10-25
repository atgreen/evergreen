// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Private semantic operations called through the ordinary Lisp ABI. Their
//! permanently interned identities own stable call-table slots. Dispatch selects
//! their semantics directly, independently of public function-cell bindings.

use super::*;
use egcl_compiler::t2::emit::OrdinaryPrimitive;

static SYMBOLS: std::sync::OnceLock<Vec<(OrdinaryPrimitive, u32)>> = std::sync::OnceLock::new();

fn symbols() -> &'static Vec<(OrdinaryPrimitive, u32)> {
    SYMBOLS.get_or_init(|| {
        OrdinaryPrimitive::ALL
            .into_iter()
            .map(|primitive| {
                (primitive, egcl_rt::symbols::intern(&primitive.adapter_name()))
            })
            .collect()
    })
}

pub(super) fn lower(
    ir: &mut egcl_compiler::t2::ir::Function,
    cells: &mut Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
) -> Option<()> {
    egcl_compiler::t2::emit::lower_ordinary_primitives(ir, |primitive| {
        let symbol = symbols().iter().find(|(kind, _)| *kind == primitive)?.1;
        if !cells.iter().any(|(existing, _)| *existing == symbol) {
            cells.push((symbol, super::super::call_table::resolve(symbol)?));
        }
        Some(symbol)
    })
    .ok()?;
    Some(())
}

pub(in crate::cli::bytecode) fn is_private(symbol: u32) -> bool {
    SYMBOLS
        .get()
        .is_some_and(|symbols| symbols.iter().any(|(_, id)| *id == symbol))
}

pub(in crate::cli::bytecode) fn invoke(
    symbol: u32,
    args: &[EgclVal],
) -> Option<Result<EgclVal, EgclError>> {
    // Do not initialize private symbols during normal named-call dispatch.
    // Lowering creates them before it publishes any primitive call sites.
    let primitive = SYMBOLS.get()?.iter().find(|(_, id)| *id == symbol)?.0;
    Some(guard_c2i(|| {
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        unsafe {
            (*env).clear_mv();
        }
        use OrdinaryPrimitive::*;
        let expected = match primitive {
            LoadGlobal | LoadFunction => 1,
            _ => 2,
        };
        if args.len() != expected {
            return Err(invalid_capture());
        }
        let result = match primitive {
            LoadGlobal => EgclVal(c2i_load_global(u64::from(args[0].as_symbol_index()))),
            LoadFunction => EgclVal(c2i_load_function(u64::from(args[0].as_symbol_index()))),
            StoreGlobal => {
                c2i_store_global(u64::from(args[0].as_symbol_index()), args[1].to_raw());
                NIL
            }
            Cons => EgclVal(c2i_alloc_cons(args[0].to_raw(), args[1].to_raw())),
            _ => {
                let name = match primitive {
                    Add => "+",
                    Subtract => "-",
                    Multiply => "*",
                    Divide => "/",
                    Equal => "EQUAL",
                    _ => unreachable!(),
                };
                let slot = super::super::super::direct_builtin_table()
                    .iter()
                    .position(|candidate| *candidate == name)
                    .ok_or_else(invalid_capture)?;
                super::super::super::call_direct_builtin(slot as u32, args, unsafe { &mut *env })
                    .ok_or_else(invalid_capture)??
            }
        };
        Ok(result)
    }))
}
