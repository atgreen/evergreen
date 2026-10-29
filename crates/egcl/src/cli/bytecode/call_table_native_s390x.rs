// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native entries for System Z linkage slots. The entry binds its own precise
//! activation and calls the installed body without Rust dispatch.
//!
//! This is the System Z counterpart of call_table_native.rs, and it is much
//! smaller for two reasons. It publishes only the slice form, because that is
//! the form emit_s390x calls through a cell; the register slot keeps the Rust
//! bridge it already had. And it bakes no callable into the instruction
//! stream -- the frame's `function` word is NIL, as the inline direct-call
//! path already leaves it -- so the version holds no Lisp value and has
//! nothing to trace.
//!
//! The machine code itself belongs to the compiler crate
//! ([`egcl_compiler::t2::emit_s390x::emit_native_call_entry`]), next to the
//! assembler and the tests that execute it, exactly as the x86 call bridges
//! come from `emit::emit_native_call_bridge`.

use super::*;
use egcl_compiler::t2::emit_s390x::{NativeCallEntry, emit_native_call_entry};
use std::cell::Cell;

pub(super) struct NativeEntry {
    /// Retains the exact installed code these bytes enter.
    code: Rc<NativeCode>,
    /// Nonzero while an activation of this entry is live. Its address is
    /// baked into the emitted code, so the box must not move.
    active: Cell<usize>,
    /// The register-form bridge this entry does not replace.
    register: usize,
    slice: Option<egcl_rt::jit::JitBuffer>,
}

impl NativeEntry {
    /// Build the entry, or decline. The admission test is the one
    /// `direct_native_targets` already applies before entering a callee with
    /// a generated frame -- fixed arity, no declared parameter types, no
    /// captured environment or closure control, no replacement, a compatible
    /// transfer ABI, and T2 code whose deopt resumes locally.
    pub(super) fn new(
        symbol: u32,
        function: EgclVal,
        code: Rc<NativeCode>,
        fallback: [usize; 2],
    ) -> Option<Box<Self>> {
        let body = code.body.as_ref()?;
        if !code.is_t2
            || !native_transfer_abi_compatible(&code)
            || body.variadic
            || body.has_env
            || body.max_args != Some(body.min_args)
            || body.min_args != body.arity
            || body
                .param_types
                .iter()
                .any(|ty| !matches!(ty, DeclaredType::Any))
            || replacement_function(symbol).is_some()
            || closure_captured_env(function).is_some()
            || closure_controls().borrow().contains_key(&symbol)
        {
            return None;
        }
        // Every parameter must name an activation slot the entry can write.
        let params: Option<Vec<u16>> = body
            .param_layout
            .iter()
            .map(|(_, location)| match location {
                VarLoc::Slot(slot) if *slot < body.n_locals => Some(*slot),
                _ => None,
            })
            .collect();
        let params = params?;
        if params.len() != usize::from(body.arity) {
            return None;
        }
        let depth_cap = u16::try_from(native_depth_cap()).ok()?;
        let mut entry = Box::new(Self {
            code,
            active: Cell::new(0),
            register: fallback[0],
            slice: None,
        });
        let spec = NativeCallEntry {
            stack: egcl_rt::current_stack() as *const egcl_rt::EgclStack as u64,
            depth: NATIVE_DEPTH.with(|slot| slot.as_ptr() as u64),
            depth_cap,
            active: entry.active.as_ptr() as u64,
            entry: entry.code.entry as u64,
            code_info: entry.code.code_info as *const CodeInfo as u64,
            frame_flags: FLAG_CALL,
            num_slots: entry.code.num_slots,
            params,
            // Prefer the register entry when this callee published one: its
            // presence certifies a body that needs no activation, so the stub
            // can pass the arguments in registers instead of building a frame
            // and copying them into slots (bliss-9va4r). Zero keeps the framed
            // path, which is every callee that did not qualify.
            register_entry: if entry.code.compiled_entry != 0 {
                entry.code.entry as u64 + entry.code.compiled_entry as u64
            } else {
                0
            },
            clear_mv: c2i_clear_mv as extern "C" fn() as usize as u64,
            fallback: fallback[1] as u64,
        };
        let bytes = emit_native_call_entry(&spec).ok()?;
        entry.slice = Some(egcl_rt::jit::JitBuffer::new(&bytes)?);
        Some(entry)
    }

    pub(super) fn entries(&self) -> [usize; 2] {
        [
            self.register,
            self.slice.as_ref().map_or(self.register, |buffer| {
                buffer.as_ptr() as usize
            }),
        ]
    }

    /// Retire the version only once nothing can enter or is inside it. A
    /// foreign execution may redirect the cell, but cannot run this code.
    pub(super) fn keep(&self, cell: &egcl_rt::call_table::CallCell) -> bool {
        self.active.get() != 0
            || unsafe { &*cell.checked_entry_address(true) }
                .load(std::sync::atomic::Ordering::Acquire)
                == self.entries()[1]
    }

    /// The emitted frame stores NIL where x86 stores the callable, so this
    /// version owns no Lisp value for the collector to visit.
    pub(super) fn trace(&mut self, _visit: &mut dyn FnMut(*mut EgclVal)) {}
}
