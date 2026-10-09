// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native entries for execution-owned linkage slots. The entry binds its own
//! precise activation and calls the installed body without Rust dispatch.

use super::*;
use std::cell::Cell;

pub(super) struct NativeEntry {
    function: EgclVal,
    code: Rc<NativeCode>,
    active: Cell<usize>,
    dynamic: bool,
    capture_context: bool,
    buffers: Vec<egcl_rt::jit::JitBuffer>,
}

impl NativeEntry {
    pub(super) fn new(
        symbol: u32,
        function: EgclVal,
        code: Rc<NativeCode>,
        fallback: [usize; 2],
    ) -> Option<Box<Self>> {
        Self::build(symbol, function, code, fallback, false)
    }

    pub(super) fn new_dynamic(
        symbol: u32,
        function: EgclVal,
        code: Rc<NativeCode>,
        fallback: [usize; 2],
    ) -> Option<Box<Self>> {
        Self::build(symbol, function, code, fallback, true)
    }

    fn build(
        symbol: u32,
        function: EgclVal,
        code: Rc<NativeCode>,
        fallback: [usize; 2],
        dynamic: bool,
    ) -> Option<Box<Self>> {
        let body = code.body.as_ref()?;
        // When every optional argument is supplied, binding is positional too.
        // A finite maximum rules out &REST/&KEY. Exactly one slot per argument
        // rules out supplied-p and &AUX bindings, which need a richer entry.
        // Default forms that require evaluation still use the binder bridge.
        let entry_arity = body.max_args?;
        if !code.is_t2
            || !native_transfer_abi_compatible(&code)
            || (body.has_env && !dynamic)
            || body.min_args != body.arity
            || (!body.variadic && entry_arity != body.arity)
            || body.param_layout.len() != entry_arity as usize
            || body
                .param_types
                .iter()
                .any(|ty| !matches!(ty, DeclaredType::Any))
            || (!dynamic
                && (closure_envs().borrow().contains_key(&symbol)
                    || closure_controls().borrow().contains_key(&symbol)))
        {
            return None;
        }
        // Capturing bodies set has_env even when all bindings belong to their
        // parent. Dynamic entries can use that captured frame directly: slot
        // parameters and this whitelist exclude any new heap-local bindings.
        if body.code.iter().any(|op| {
            if dynamic && matches!(op, Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_)) {
                return false;
            }
            !matches!(
                op,
                Instr::Const(_)
                    | Instr::LoadLocal(_)
                    | Instr::StoreLocal(_)
                    | Instr::LoadGlobal(_)
                    | Instr::StoreGlobal(_)
                    | Instr::LoadFunction(_)
                    | Instr::ClearMv
                    | Instr::TakeValuesToLocals { .. }
                    | Instr::Pop
                    | Instr::Dup
                    | Instr::Br(_)
                    | Instr::BrIfFalse(_)
                    | Instr::BrIfTrue(_)
                    | Instr::CallNamed { .. }
                    | Instr::TypeP(_)
                    | Instr::MemoryFence(_)
                    | Instr::Return
                    | Instr::PushBlock {
                        register: false,
                        ..
                    }
                    | Instr::PopHandler
                    | Instr::ReturnFrom { .. }
                    | Instr::PushTag { .. }
                    | Instr::Go { .. }
            )
        }) {
            return None;
        }
        let slots: Option<Vec<u16>> = body
            .param_layout
            .iter()
            .map(|(_, location)| match location {
                VarLoc::Slot(slot) if *slot < body.n_locals => Some(*slot),
                _ => None,
            })
            .collect();
        let slots = slots?;
        let defaults = literal_defaults(body);
        let capture_context = dynamic
            && (egcl_rt::symbols::is_uninterned(symbol)
                || closure_envs().borrow().contains_key(&symbol)
                || closure_controls().borrow().contains_key(&symbol));
        let mut entry = Box::new(Self {
            function: if dynamic { NIL } else { function },
            dynamic,
            capture_context,
            code,
            active: Cell::new(0),
            buffers: Vec::new(),
        });
        for (slice, fallback) in [false, true].into_iter().zip(fallback) {
            let bytes = entry.emit(&slots, &defaults, slice, fallback)?;
            entry.buffers.push(egcl_rt::jit::JitBuffer::new(&bytes)?);
        }
        Some(entry)
    }

    pub(super) fn owns(&self, code: &Rc<NativeCode>) -> bool {
        Rc::ptr_eq(&self.code, code)
    }
    pub(super) fn is_active(&self) -> bool {
        self.active.get() != 0
    }

    pub(super) fn entries(&self) -> [usize; 2] {
        std::array::from_fn(|i| self.buffers[i].as_ptr() as usize)
    }

    pub(super) fn keep(&self, cell: &egcl_rt::call_table::CallCell) -> bool {
        self.active.get() != 0
            || self.entries().into_iter().enumerate().any(|(i, entry)| {
                // Only the owning execution reclaims versions. Foreign executions
                // can redirect these atomic words, but cannot enter this code.
                unsafe { &*cell.checked_entry_address(i != 0) }.load(std::sync::atomic::Ordering::Acquire)
                    == entry
            })
    }

    pub(super) fn trace(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
    }

    fn emit(
        &self,
        params: &[u16],
        defaults: &[Option<EgclVal>],
        slice: bool,
        fallback: usize,
    ) -> Option<Vec<u8>> {
        const RAX: u8 = 0;
        const RCX: u8 = 1;
        const RDX: u8 = 2;
        const RSP: u8 = 4;
        const RSI: u8 = 6;
        const RDI: u8 = 7;
        const R10: u8 = 10;
        const R11: u8 = 11;
        // Six incoming words, two saved EgclStack pointers; 72 aligns RSP.
        let save_bytes = 72_u32
            + if self.capture_context {
                std::mem::size_of::<super::call_table_funcall::Context>().next_multiple_of(16)
                    as u32
            } else {
                0
            };
        const OLD_FP: i32 = 48;
        const OLD_SP: i32 = 56;
        let stack = egcl_rt::current_stack() as *const egcl_rt::EgclStack as u64;
        let depth = NATIVE_DEPTH.with(|slot| slot.as_ptr() as u64);
        let active = self.active.as_ptr() as u64;
        let bs_base = egcl_rt::EgclStack::OFFSET_BASE as i32;
        let bs_sp = egcl_rt::EgclStack::OFFSET_SP_OFFSET as i32;
        let bs_fp = egcl_rt::EgclStack::OFFSET_FP as i32;
        let bs_cap = egcl_rt::EgclStack::OFFSET_CAPACITY as i32;
        let header = std::mem::size_of::<Frame>() as i32;
        let frame_bytes = header + i32::from(self.code.num_slots) * 8;
        let mut a = Asm::new();
        let slow = a.label();
        let restore_slow = a.label();
        // Every required argument and every nonliteral default must be supplied.
        // This also prevents reading beyond a short register/slice payload.
        let minimum = defaults
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |i| i + 1);
        compare_count(&mut a, minimum);
        a.jcc(Cc::L, slow);
        compare_count(&mut a, params.len());
        a.jcc(Cc::G, slow);
        if !slice {
            compare_count(&mut a, 3);
            a.jcc(Cc::G, slow);
        }
        a.extend_from_slice(&[0x48, 0x81, 0xec]);
        a.extend_from_slice(&save_bytes.to_le_bytes());
        for (i, register) in [RDI, RSI, RDX, RCX, 8, 9].into_iter().enumerate() {
            memory(&mut a, 0x89, register, RSP, i as i32 * 8);
        }
        immediate(&mut a, R11, depth);
        a.extend_from_slice(&[0x41, 0x8b, 0x03]); // mov eax, [r11], zero-extending u32
        immediate(&mut a, R10, u64::from(native_depth_cap()));
        a.extend_from_slice(&[0x4c, 0x39, 0xd0]); // cmp rax, r10: both nonnegative u32
        a.jcc(Cc::Ge, restore_slow);
        immediate(&mut a, R10, stack);
        memory(&mut a, 0x8b, RDX, R10, bs_sp);
        memory(&mut a, 0x8d, RCX, RDX, frame_bytes);
        memory(&mut a, 0x3b, RCX, R10, bs_cap);
        a.jcc(Cc::G, restore_slow);
        memory(&mut a, 0x89, RDX, RSP, OLD_SP);
        memory(&mut a, 0x8b, RAX, R10, bs_fp);
        memory(&mut a, 0x89, RAX, RSP, OLD_FP);
        memory(&mut a, 0x8b, RAX, R10, bs_base);
        a.extend_from_slice(&[0x48, 0x01, 0xd0]); // add rax, rdx
        memory(&mut a, 0x8b, R11, RSP, OLD_FP);
        memory(
            &mut a,
            0x89,
            R11,
            RAX,
            std::mem::offset_of!(Frame, prev_fp) as i32,
        );
        immediate(&mut a, R11, 0);
        memory(
            &mut a,
            0x89,
            R11,
            RAX,
            std::mem::offset_of!(Frame, return_pc) as i32,
        );
        if self.dynamic {
            memory(&mut a, 0x8b, R11, RSP, 0);
        } else {
            immediate(&mut a, R11, self.function.0);
        }
        memory(
            &mut a,
            0x89,
            R11,
            RAX,
            std::mem::offset_of!(Frame, function) as i32,
        );
        immediate(&mut a, R11, self.code.code_info as *const CodeInfo as u64);
        memory(
            &mut a,
            0x89,
            R11,
            RAX,
            std::mem::offset_of!(Frame, code_info) as i32,
        );
        // flags/u16 num_locals/u16 padding occupy one fixed-layout word.
        immediate(
            &mut a,
            R11,
            u64::from(FLAG_CALL) | (u64::from(self.code.num_slots) << 32),
        );
        memory(
            &mut a,
            0x89,
            R11,
            RAX,
            std::mem::offset_of!(Frame, flags) as i32,
        );
        immediate(&mut a, R11, NIL.0);
        for slot in 0..self.code.num_slots {
            memory(&mut a, 0x89, R11, RAX, header + i32::from(slot) * 8);
        }
        if slice {
            memory(&mut a, 0x8b, RDX, RSP, 16);
        }
        for (arg, &slot) in params.iter().enumerate() {
            let bound = a.label();
            if let Some(value) = defaults[arg] {
                let supplied = a.label();
                compare_count(&mut a, arg + 1);
                a.jcc(Cc::Ge, supplied);
                immediate(&mut a, R11, value.0);
                a.jmp(bound);
                a.bind(supplied);
            }
            if slice {
                memory(&mut a, 0x8b, R11, RDX, arg as i32 * 8);
            } else {
                memory(&mut a, 0x8b, R11, RSP, 16 + arg as i32 * 8);
            }
            a.bind(bound);
            memory(&mut a, 0x89, R11, RAX, header + i32::from(slot) * 8);
        }
        // Publish a fully initialized, precisely scanned frame before any call.
        memory(&mut a, 0x89, RAX, R10, bs_fp);
        memory(&mut a, 0x89, RCX, R10, bs_sp);
        immediate(&mut a, R11, active);
        a.extend_from_slice(&[0x49, 0xff, 0x03]); // inc qword [r11]
        immediate(&mut a, R11, depth);
        a.extend_from_slice(&[0x41, 0xff, 0x03]); // inc dword [r11]
        if !profiling_disabled() {
            // The retained callable is a pinned, interpreted-function object.
            let offset = std::mem::offset_of!(egcl_rt::object::FunctionData, invoke_count);
            if self.dynamic {
                memory(&mut a, 0x8b, R11, RSP, 0);
                a.extend_from_slice(&[0x49, 0x83, 0xe3, 0xf8]); // clear value tag
                memory(&mut a, 0x8d, R11, R11, offset as i32);
            } else {
                let count = unsafe { self.function.as_ptr() } as usize + offset;
                immediate(&mut a, R11, count as u64);
            }
            a.extend_from_slice(&[0xf0, 0x41, 0xff, 0x03]); // lock inc dword [r11]
        }
        // This leaf helper neither allocates nor yields. It clears the real MV
        // buffer, without exposing Rust Vec internals to generated code.
        immediate(&mut a, RAX, c2i_clear_mv as *const () as u64);
        a.extend_from_slice(&[0xff, 0xd0]);
        if self.capture_context {
            memory(&mut a, 0x8d, RDI, RSP, 72);
            memory(&mut a, 0x8b, RSI, RSP, 0);
            immediate(
                &mut a,
                RAX,
                super::call_table_funcall::enter_context as *const () as u64,
            );
            a.extend_from_slice(&[0xff, 0xd0]);
        }
        immediate(&mut a, RSI, stack);
        memory(&mut a, 0x8b, RDI, RSI, bs_fp);
        memory(&mut a, 0x8d, RDI, RDI, header);
        immediate(&mut a, RAX, self.code.entry as u64);
        a.extend_from_slice(&[0xff, 0xd0]); // native body returns its final value
        if self.capture_context {
            memory(&mut a, 0x89, RAX, RSP, 64);
            memory(&mut a, 0x8d, RDI, RSP, 72);
            immediate(
                &mut a,
                RAX,
                super::call_table_funcall::leave_context as *const () as u64,
            );
            a.extend_from_slice(&[0xff, 0xd0]);
            memory(&mut a, 0x8b, RAX, RSP, 64);
        }
        immediate(&mut a, R10, stack);
        memory(&mut a, 0x8b, RCX, RSP, OLD_FP);
        memory(&mut a, 0x89, RCX, R10, bs_fp);
        memory(&mut a, 0x8b, RCX, RSP, OLD_SP);
        memory(&mut a, 0x89, RCX, R10, bs_sp);
        immediate(&mut a, R11, depth);
        a.extend_from_slice(&[0x41, 0xff, 0x0b]); // dec dword [r11]
        immediate(&mut a, R11, active);
        a.extend_from_slice(&[0x49, 0xff, 0x0b]); // dec qword [r11]
        // No safepoint between releasing the active version and leaving it.
        a.extend_from_slice(&[0x48, 0x81, 0xc4]);
        a.extend_from_slice(&save_bytes.to_le_bytes());
        a.push(0xc3);
        a.bind(restore_slow);
        for (i, register) in [RDI, RSI, RDX, RCX, 8, 9].into_iter().enumerate() {
            memory(&mut a, 0x8b, register, RSP, i as i32 * 8);
        }
        a.extend_from_slice(&[0x48, 0x81, 0xc4]);
        a.extend_from_slice(&save_bytes.to_le_bytes());
        a.bind(slow);
        immediate(&mut a, RAX, fallback as u64);
        a.extend_from_slice(&[0xff, 0xe0]); // tail-enter the interpreter/legacy bridge
        a.finish()
    }
}

/// Only immediate self-evaluating defaults can be embedded without evaluation
/// or movable constant roots. Calls, variable references and heap literals use
/// the ordinary binder when omitted; supplied arguments always bypass it.
fn literal_defaults(body: &BytecodeFunction) -> Vec<Option<EgclVal>> {
    let mut defaults = vec![None; body.param_layout.len()];
    if !body.variadic {
        return defaults;
    }
    let mut tail = body.params_form;
    let mut optional = false;
    let mut index = 0;
    while tail.is_cons() {
        let (parameter, rest) = cp(tail);
        tail = rest;
        if parameter.is_symbol() {
            let name = super::super::sym_name_rc(parameter);
            if name.starts_with('&') {
                if &*name == "&OPTIONAL" {
                    optional = true;
                    continue;
                }
                // The eligibility test excludes binding-bearing &AUX/&REST/
                // &KEY lists. Treat any other keyword conservatively too.
                return vec![None; defaults.len()];
            }
        }
        if optional && index < defaults.len() {
            let value = if parameter.is_cons() {
                let (_, spec) = cp(parameter);
                if spec.is_cons() { cp(spec).0 } else { NIL }
            } else {
                NIL
            };
            if value.is_nil()
                || value == egcl_rt::value::T
                || value.is_fixnum()
                || value.is_character()
                || value.is_single_float()
            {
                defaults[index] = Some(value);
            }
        }
        index += 1;
    }
    if !tail.is_nil() || index != defaults.len() {
        defaults.fill(None);
    }
    defaults
}

fn compare_count(a: &mut Asm, count: usize) {
    a.extend_from_slice(&[0x48, 0x81, 0xfe]); // cmp rsi, immediate
    a.extend_from_slice(&(count as u32).to_le_bytes());
}

pub(super) fn immediate(a: &mut Asm, register: u8, value: u64) {
    a.extend_from_slice(&[
        if register >= 8 { 0x49 } else { 0x48 },
        0xb8 + (register & 7),
    ]);
    a.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn memory(a: &mut Asm, opcode: u8, register: u8, base: u8, displacement: i32) {
    a.push(0x48 | if register >= 8 { 4 } else { 0 } | if base >= 8 { 1 } else { 0 });
    a.push(opcode);
    a.push(0x80 | ((register & 7) << 3) | (base & 7));
    if base & 7 == 4 {
        a.push(0x24);
    }
    a.extend_from_slice(&displacement.to_le_bytes());
}
