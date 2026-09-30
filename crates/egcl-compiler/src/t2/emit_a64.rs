// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AArch64 emission for the optimized SSA pipeline.
//!
//! Structured as the System Z emitter is, and for the same reasons: register
//! allocation assigns each SSA value a *home* (a callee-saved register or a frame
//! slot), and emission then loads operands into fixed working registers, operates,
//! and stores the result back to its home. That keeps instruction selection
//! readable and confines the architecture to this file, at the cost of some moves
//! a fully allocation-driven emitter would avoid.
//!
//! Register roles map one-to-one onto the System Z emitter's, which is why the two
//! read alike. AAPCS64 passes the first arguments in x0–x7 and returns in x0, and
//! System Z uses r2–r6 and returns in r2, so the working registers double as
//! argument registers on both:
//!
//! | role                                  | System Z | AArch64 |
//! |---------------------------------------|----------|---------|
//! | primary working / first argument      | r2       | x0      |
//! | secondary working / second argument   | r3       | x1      |
//! | temporaries / third and fourth args   | r4, r5   | x2, x3  |
//! | address scratch                       | r1       | x16     |
//! | frame-slots pointer, whole body       | r13      | x28     |
//! | stack pointer                         | r15      | sp      |
//!
//! Homes come from [`super::regalloc::allocate_framed_a64`], whose pool is x19–x27
//! — callee-saved, so a value survives a runtime call without the emitter spilling
//! it. Unlike x86, `PhysReg.encoding` here is the architectural register number.

use super::emit::{EmitError, FramedCode, RootSyncSite};
use super::emit_s390x::rematerialization_order;
use super::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, Inst, InstData, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use super::mach::{Location, RegClass};
use std::collections::{HashMap, HashSet};
use egcl_rt::asm::{Asm, Cc, a64};
use egcl_rt::value::{NIL, T, EgclVal, UNBOUND};

/// Primary working register, and the C ABI's first argument and return register.
const W0: a64::Reg = 0;
/// Secondary working register / second argument.
const W1: a64::Reg = 1;
/// Temporaries, and the third and fourth arguments.
const T0: a64::Reg = 2;
const T1: a64::Reg = 3;
/// IP0: the architecture already reserves it as call-clobbered scratch, so no
/// allocated home can live here.
const ADDR: a64::Reg = 16;
/// Holds the frame-slots pointer for the whole body.
const SLOTS: a64::Reg = 28;
/// Working float registers.
const F0: a64::Fpr = 0;
const F1: a64::Fpr = 1;

/// Bytes the prologue's save area occupies. Shared with T1 and with the SIGSEGV
/// recovery epilogue, which must be able to unwind either tier.
const SAVE_BYTES: i32 = a64::JIT_SAVE_BYTES;

/// Back-edge polls run one in this many iterations. Matching the System Z
/// emitter: frequent enough that a hot loop reaches a collection promptly,
/// rare enough that the counter is not the loop's cost.
const POLL_PERIOD: u64 = 256;

/// C-ABI runtime adapters used by optimized code, as the System Z emitter defines
/// them. Re-exported rather than redefined so one call site fills both.
pub use super::emit_s390x::RuntimeCalls;

fn calls_runtime(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Call
            | Opcode::SymbolValue
            | Opcode::SymbolFunction
            | Opcode::SetSymbolValue
            | Opcode::ClearMv
            | Opcode::TakeValuesToLocals
    )
}

#[derive(Clone, Copy)]
enum Home {
    Register(u8),
    Stack(u32),
}

struct Emitter<'a> {
    function: &'a Function,
    asm: Asm,
    homes: HashMap<Value, Home>,
    constants: HashMap<Value, u64>,
    heap_constants: HashMap<Value, usize>,
    blocks: HashMap<Block, egcl_rt::asm::Label>,
    deopts: Vec<(FrameStateId, egcl_rt::asm::Label)>,
    frame_bytes: i32,
    edge_base: i32,
    deopt_base: i32,
    remat_base: i32,
    runtime: RuntimeCalls,
    activation_slots: u16,
    root_slots: u16,
    argument_slots: u16,
    roots: HashMap<Inst, Vec<Value>>,
    root_sites: Vec<RootSyncSite>,
    result_offset: i32,
    poll_offset: i32,
    polls: HashSet<Inst>,
    transfer_exit: Option<egcl_rt::asm::Label>,
}

fn unsupported() -> EmitError {
    EmitError::UnsupportedOp(0xA64)
}

impl Emitter<'_> {
    // ── the instruction vocabulary ───────────────────────────────
    //
    // Thin semantic wrappers over the encoders in `egcl_rt::asm::a64`, so the
    // emission logic below reads as operations rather than as bit fields. Each
    // bails rather than truncating when an operand does not fit its field.

    fn imm(&mut self, register: a64::Reg, value: u64) {
        let mut words = Vec::new();
        a64::mov_imm64(register, value, &mut words);
        self.asm.words(&words);
    }

    fn mov(&mut self, destination: a64::Reg, source: a64::Reg) {
        if destination != source {
            self.asm.word(a64::mov(destination, source));
        }
    }

    /// Load from `[base + offset]`, materialising the offset when it is beyond the
    /// scaled field rather than silently emitting the wrong access.
    /// Load from `[base + offset]`. Beyond the scaled immediate field this falls
    /// back to the *indexed* form with the offset in a register, never to a
    /// shifted-register `add`: register 31 reads as SP in a load or store but as
    /// XZR in an `add`, so computing the address first would quietly address zero
    /// whenever `base` is the stack pointer.
    fn load_at(
        &mut self,
        register: a64::Reg,
        base: a64::Reg,
        offset: i32,
    ) -> Result<(), EmitError> {
        let scaled = u64::try_from(offset).map_err(|_| unsupported())?;
        match a64::ldr_imm(register, base, scaled) {
            Some(word) => self.asm.word(word),
            None => {
                if scaled % 8 != 0 {
                    return Err(unsupported());
                }
                self.imm(ADDR, scaled / 8);
                self.asm.word(a64::ldr_indexed(register, base, ADDR));
            }
        }
        Ok(())
    }

    fn store_at(
        &mut self,
        register: a64::Reg,
        base: a64::Reg,
        offset: i32,
    ) -> Result<(), EmitError> {
        let scaled = u64::try_from(offset).map_err(|_| unsupported())?;
        match a64::str_imm(register, base, scaled) {
            Some(word) => self.asm.word(word),
            None => {
                if scaled % 8 != 0 {
                    return Err(unsupported());
                }
                self.imm(ADDR, scaled / 8);
                self.asm.word(a64::str_indexed(register, base, ADDR));
            }
        }
        Ok(())
    }

    /// `register = base + offset`, for handing the runtime the address of a slot.
    /// Only the add/sub *immediate* form reads register 31 as the stack pointer, so
    /// this must stay on that form; an offset it cannot hold is a decline.
    fn address(
        &mut self,
        register: a64::Reg,
        base: a64::Reg,
        offset: i32,
    ) -> Result<(), EmitError> {
        let value = u64::try_from(offset).map_err(|_| unsupported())?;
        let word = a64::add_imm(register, base, value).ok_or_else(unsupported)?;
        self.asm.word(word);
        Ok(())
    }

    /// Move the stack pointer by `bytes`, which must be a multiple of 16 so it stays
    /// aligned as AAPCS64 requires at every call. Emitted as add/sub-immediate — the
    /// only form that reads register 31 as SP — in up to two steps so a frame larger
    /// than the 12-bit field still works.
    fn adjust_stack(&mut self, bytes: i32, down: bool) -> Result<(), EmitError> {
        debug_assert_eq!(bytes % 16, 0, "the stack pointer must stay 16-byte aligned");
        let mut remaining = u64::try_from(bytes).map_err(|_| unsupported())?;
        let mut steps = 0;
        while remaining > 0 {
            steps += 1;
            if steps > 2 {
                return Err(unsupported());
            }
            let step = if remaining >= 0x1000 {
                remaining & !0xfff
            } else {
                remaining
            };
            let word = if down {
                a64::sub_imm(a64::SP, a64::SP, step)
            } else {
                a64::add_imm(a64::SP, a64::SP, step)
            }
            .ok_or_else(unsupported)?;
            self.asm.word(word);
            remaining -= step;
        }
        Ok(())
    }

    fn compare(&mut self, left: a64::Reg, right: a64::Reg) {
        self.asm.word(a64::cmp(left, right));
    }

    /// Compare against a constant, through a register when it does not fit the
    /// immediate field — which tagged values generally do not.
    fn compare_imm(&mut self, register: a64::Reg, value: u64) {
        match a64::cmp_imm(register, value) {
            Some(word) => self.asm.word(word),
            None => {
                self.imm(ADDR, value);
                self.asm.word(a64::cmp(register, ADDR));
            }
        }
    }

    fn call_register(&mut self, register: a64::Reg) {
        self.asm.word(a64::blr(register));
    }

    // ── values and their homes ───────────────────────────────────

    fn load(&mut self, value: Value, register: a64::Reg) -> Result<(), EmitError> {
        if let Some(&bits) = self.constants.get(&value) {
            self.imm(register, bits);
        } else if let Some(&slot) = self.heap_constants.get(&value) {
            // A movable heap constant is read through its stable constants slot,
            // never baked in: the collector rewrites that slot and cannot patch
            // machine code.
            self.imm(ADDR, slot as u64);
            self.asm
                .word(a64::ldr_imm(register, ADDR, 0).ok_or_else(unsupported)?);
        } else {
            match *self.homes.get(&value).ok_or_else(unsupported)? {
                Home::Register(source) => self.mov(register, source),
                Home::Stack(slot) => self.load_at(register, a64::SP, slot as i32 * 8)?,
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: a64::Reg) -> Result<(), EmitError> {
        match *self.homes.get(&value).ok_or_else(unsupported)? {
            Home::Register(destination) => self.mov(destination, register),
            Home::Stack(slot) => self.store_at(register, a64::SP, slot as i32 * 8)?,
        }
        Ok(())
    }

    // ── frame ────────────────────────────────────────────────────

    fn prologue(&mut self) -> Result<(), EmitError> {
        // One save area holding the frame record and every register the pool may
        // hand out, so the epilogue can restore the stack pointer from x29 alone
        // rather than trusting the body to have left it balanced.
        self.asm
            .word(a64::stp_pre(a64::FP, a64::LR, a64::SP, -SAVE_BYTES).ok_or_else(unsupported)?);
        self.asm.word(a64::mov_from_sp(a64::FP));
        for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
            self.asm.word(
                a64::stp(first, second, a64::SP, 16 + index as i32 * 16).ok_or_else(unsupported)?,
            );
        }
        let frame = self.frame_bytes;
        self.adjust_stack(frame, true)?;
        // The first argument is the frame-slots pointer, and it must survive the
        // whole body, so it moves out of the working registers immediately.
        self.mov(SLOTS, W0);
        self.imm(W0, POLL_PERIOD);
        let offset = self.poll_offset;
        self.store_at(W0, a64::SP, offset)
    }

    fn epilogue(&mut self) -> Result<(), EmitError> {
        self.asm.word(a64::mov_to_sp(a64::FP));
        for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
            self.asm.word(
                a64::ldp(first, second, a64::SP, 16 + index as i32 * 16).ok_or_else(unsupported)?,
            );
        }
        self.asm
            .word(a64::ldp_post(a64::FP, a64::LR, a64::SP, SAVE_BYTES).ok_or_else(unsupported)?);
        self.asm.word(a64::ret());
        Ok(())
    }

    // ── guards ───────────────────────────────────────────────────

    fn deopt_label(&mut self, data: &InstData) -> Result<egcl_rt::asm::Label, EmitError> {
        let state = data.frame_state.ok_or_else(unsupported)?;
        if self.function.frame_states.get(state).scopes.is_empty() {
            return Err(unsupported());
        }
        let label = self.asm.label();
        self.deopts.push((state, label));
        Ok(label)
    }

    /// A fixnum's low three tag bits are zero, which `TST` tests without needing a
    /// temporary or disturbing the value.
    fn guard_fixnum(&mut self, register: a64::Reg, deopt: egcl_rt::asm::Label) {
        self.asm
            .word(a64::tst_imm(register, 7).expect("7 is a logical immediate"));
        self.asm.jcc(Cc::Ne, deopt);
    }

    /// A tagged single-float carries tag 4 and its f32 bits in the high half.
    fn guard_single_float(&mut self, register: a64::Reg, deopt: egcl_rt::asm::Label) {
        self.asm
            .word(a64::and_imm(T0, register, 7).expect("7 is a logical immediate"));
        self.asm
            .word(a64::cmp_imm(T0, 4).expect("4 is an add-immediate"));
        self.asm.jcc(Cc::Ne, deopt);
    }

    /// A tagged cons carries tag 1 in its low three bits. Mirrors emit.rs's
    /// `emit_cons_guard`; the guard passes the value through, so the caller
    /// stores the unchanged operand as the guard's result.
    fn guard_cons(&mut self, register: a64::Reg, deopt: egcl_rt::asm::Label) {
        self.asm
            .word(a64::and_imm(T0, register, 7).expect("7 is a logical immediate"));
        self.asm.word(
            a64::cmp_imm(T0, egcl_rt::value::TAG_CONS).expect("1 is an add-immediate"),
        );
        self.asm.jcc(Cc::Ne, deopt);
    }

    fn float_operand(
        &mut self,
        value: Value,
        register: a64::Reg,
        deopt: egcl_rt::asm::Label,
    ) -> Result<(), EmitError> {
        if let Some(&bits) = self.constants.get(&value) {
            let constant = EgclVal(bits);
            if constant.is_fixnum() {
                self.imm(
                    register,
                    EgclVal::from_single_float(constant.as_fixnum() as f32).0,
                );
                return Ok(());
            }
        }
        self.load(value, register)?;
        self.guard_single_float(register, deopt);
        Ok(())
    }

    /// Unbox a tagged single-float from `gpr` into `fpr`.
    fn unbox_single(&mut self, fpr: a64::Fpr, gpr: a64::Reg) {
        self.asm.word(a64::lsr_imm(T1, gpr, 32));
        self.asm.word(a64::fmov_from_gpr_single(fpr, T1));
    }

    /// Re-tag the f32 in `fpr` as a EGCL single-float in `W0`.
    fn box_single(&mut self, fpr: a64::Fpr) {
        self.asm.word(a64::fmov_to_gpr_single(T1, fpr));
        self.asm.word(a64::lsl_imm(W0, T1, 32));
        self.asm
            .word(a64::orr_imm(W0, W0, 4).expect("4 is a logical immediate"));
    }

    /// Multiply the tagged fixnums in `W0` and `W1`, leaving the tagged product in
    /// `W0` and deopting on overflow.
    ///
    /// AArch64 has no flag-setting multiply, so unlike `ADDS` there is nothing for
    /// a `Cc::O` branch to read. Untag one operand so the product is tagged once,
    /// then require the high half of the signed 128-bit product to be exactly the
    /// low half's sign extension. No allocated home is written before the exit, so
    /// a deopt still sees both inputs.
    fn multiply_fixnums(&mut self, deopt: egcl_rt::asm::Label) {
        self.asm.word(a64::asr_imm(T0, W0, 3));
        self.asm.word(a64::smulh(T1, T0, W1));
        self.asm.word(a64::mul(W0, T0, W1));
        self.asm.word(a64::cmp_shifted(T1, W0, a64::Shift::Asr(63)));
        self.asm.jcc(Cc::Ne, deopt);
    }

    // ── control flow ─────────────────────────────────────────────

    fn edge(&mut self, edge: &BlockCall) -> Result<(), EmitError> {
        let parameters = self.function.block(edge.block).params.clone();
        if parameters.len() != edge.args.len() {
            return Err(unsupported());
        }
        // A parallel copy through private frame slots handles cycles and
        // overlapping register/spill homes without clobbering an edge input.
        let base = self.edge_base;
        for (index, &source) in edge.args.iter().enumerate() {
            self.load(source, W0)?;
            self.store_at(W0, a64::SP, base + index as i32 * 8)?;
        }
        for (index, destination) in parameters.into_iter().enumerate() {
            self.load_at(W0, a64::SP, base + index as i32 * 8)?;
            self.store(destination, W0)?;
        }
        self.asm.jmp(self.blocks[&edge.block]);
        Ok(())
    }
}

impl Emitter<'_> {
    /// Call a runtime adapter, synchronising GC roots around it.
    ///
    /// This is the GC contract, and the reason the sequence looks heavy: the
    /// collector moves objects, and it can only find and update roots it knows
    /// about. So every live value is written to a shadow slot in the owning
    /// EgclStack activation before the call and read back afterwards — its home
    /// register or spill slot is invisible to the collector, the shadow slot is
    /// not. `RootSyncSite` records where each of these happens so installation can
    /// verify the count against the emitted safepoints.
    fn runtime_call(
        &mut self,
        instruction: Inst,
        data: Option<&InstData>,
    ) -> Result<(), EmitError> {
        let roots = self
            .roots
            .get(&instruction)
            .ok_or_else(unsupported)?
            .clone();
        let root_base = i32::from(self.activation_slots) * 8;
        let argument_base = root_base + i32::from(self.root_slots) * 8;
        // Clear the unused shadows as well: an earlier call may have had more live
        // roots or arguments, and dead objects need not be retained indefinitely.
        self.imm(W0, NIL.0);
        for index in 0..u32::from(self.root_slots) + u32::from(self.argument_slots) {
            self.store_at(W0, SLOTS, root_base + index as i32 * 8)?;
        }
        for (index, &value) in roots.iter().enumerate() {
            self.load(value, W0)?;
            self.store_at(W0, SLOTS, root_base + index as i32 * 8)?;
        }
        let helper = if let Some(data) = data {
            match data.opcode {
                Opcode::Call => {
                    let AuxData::CallTarget(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    for (index, &arg) in data.args.iter().enumerate() {
                        self.load(arg, W0)?;
                        self.store_at(W0, SLOTS, argument_base + index as i32 * 8)?;
                    }
                    self.imm(W0, u64::from(symbol));
                    self.imm(W1, data.args.len() as u64);
                    self.address(T0, SLOTS, argument_base)?;
                    self.imm(T1, 0);
                    self.runtime.call_slice
                }
                Opcode::SymbolValue | Opcode::SymbolFunction | Opcode::SetSymbolValue => {
                    let AuxData::SymbolRef(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    if data.opcode == Opcode::SetSymbolValue {
                        self.load(*data.args.first().ok_or_else(unsupported)?, W1)?;
                    }
                    self.imm(W0, u64::from(symbol));
                    match data.opcode {
                        Opcode::SymbolValue => self.runtime.load_global,
                        Opcode::SymbolFunction => self.runtime.load_function,
                        _ => self.runtime.store_global,
                    }
                }
                Opcode::ClearMv => {
                    self.imm(W0, NIL.0);
                    self.imm(W1, 0);
                    self.imm(T0, 0);
                    self.runtime.multiple_values
                }
                Opcode::TakeValuesToLocals => {
                    let AuxData::ValuesLocals { nvars, slot_base } = data.aux else {
                        return Err(unsupported());
                    };
                    if usize::from(nvars) != data.results.len()
                        || slot_base
                            .checked_add(nvars)
                            .is_none_or(|end| end > self.activation_slots)
                    {
                        return Err(unsupported());
                    }
                    self.load(*data.args.first().ok_or_else(unsupported)?, W0)?;
                    self.address(W1, SLOTS, i32::from(slot_base) * 8)?;
                    self.imm(T0, u64::from(nvars));
                    self.runtime.multiple_values
                }
                _ => return Err(unsupported()),
            }
        } else {
            self.runtime.poll
        };
        if helper == 0 {
            return Err(unsupported());
        }
        let offset = self.asm.here();
        self.imm(ADDR, helper);
        self.call_register(ADDR);
        let result_offset = self.result_offset;
        self.store_at(W0, a64::SP, result_offset)?;
        if data.is_none() || self.runtime.transfer_pending != 0 {
            if data.is_some() {
                self.imm(ADDR, self.runtime.transfer_pending);
                self.call_register(ADDR);
            }
            self.compare_imm(W0, 0);
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.jcc(Cc::Ne, exit);
        }
        // Restore roots before assigning results: a dying argument may share its
        // home with the call result, and must never overwrite that result.
        for (index, &value) in roots.iter().enumerate() {
            self.load_at(W0, SLOTS, root_base + index as i32 * 8)?;
            self.store(value, W0)?;
        }
        if let Some(data) = data {
            if let AuxData::ValuesLocals { slot_base, .. } = data.aux {
                for (index, &value) in data.results.iter().enumerate() {
                    self.load_at(W0, SLOTS, (i32::from(slot_base) + index as i32) * 8)?;
                    self.store(value, W0)?;
                }
            } else if let Some(&result) = data.results.first() {
                self.load_at(W0, a64::SP, result_offset)?;
                self.store(result, W0)?;
            }
        }
        let register_roots = roots
            .iter()
            .filter(|value| matches!(self.homes.get(value), Some(Home::Register(_))))
            .count();
        self.root_sites.push(RootSyncSite {
            code_offset: u32::try_from(offset).map_err(|_| unsupported())?,
            live_roots: roots.len() as u16,
            register_roots: register_roots as u16,
            spill_roots: (roots.len() - register_roots) as u16,
        });
        Ok(())
    }

    fn instruction(&mut self, instruction: Inst, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
        if self.polls.contains(&instruction) {
            // Sampled back-edge safepoint: a hot loop must still reach GC
            // stop-the-world and still be interruptible.
            let skip = self.asm.label();
            let poll_offset = self.poll_offset;
            self.load_at(W0, a64::SP, poll_offset)?;
            self.asm
                .word(a64::subs_imm(W0, W0, 1).expect("one is an add-immediate"));
            self.store_at(W0, a64::SP, poll_offset)?;
            self.asm.jcc(Cc::Ne, skip);
            self.imm(W0, POLL_PERIOD);
            self.store_at(W0, a64::SP, poll_offset)?;
            self.runtime_call(instruction, None)?;
            self.asm.bind(skip);
        }
        if calls_runtime(data.opcode) {
            return self.runtime_call(instruction, Some(data));
        }
        if data
            .results
            .first()
            .is_some_and(|v| self.constants.contains_key(v) || self.heap_constants.contains_key(v))
        {
            return Ok(());
        }
        match data.opcode {
            Return => {
                if let Some(&value) = data.args.first() {
                    self.load(value, W0)?;
                } else {
                    self.imm(W0, NIL.0);
                }
                return self.epilogue();
            }
            Jump if data.targets.len() == 1 => return self.edge(&data.targets[0]),
            Brif if data.args.len() == 1 && data.targets.len() == 2 => {
                let otherwise = self.asm.label();
                self.load(data.args[0], W0)?;
                self.compare_imm(W0, NIL.0);
                self.asm.jcc(Cc::E, otherwise);
                self.edge(&data.targets[0])?;
                self.asm.bind(otherwise);
                return self.edge(&data.targets[1]);
            }
            _ => {}
        }
        let result = *data.results.first().ok_or_else(unsupported)?;
        let first = *data.args.first().ok_or_else(unsupported)?;
        self.load(first, W0)?;
        match data.opcode {
            Guard if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM || ty.bits == TypeBits::SINGLE_FLOAT || ty.bits == TypeBits::CONS) =>
            {
                // CONS matters as much as the numeric tags: build.rs emits a
                // separate Guard(TypeTag(CONS)) ahead of every Car/Cdr, so
                // refusing it cost the whole function its T2 code for any use of
                // CAR or CDR -- which is most Lisp code (bliss-2yews).
                let deopt = self.deopt_label(data)?;
                match &data.aux {
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM => {
                        self.guard_fixnum(W0, deopt)
                    }
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::CONS => {
                        self.guard_cons(W0, deopt)
                    }
                    _ => self.guard_single_float(W0, deopt),
                }
            }
            FloatAdd | FloatSub | FloatMul => {
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, W0, deopt)?;
                self.float_operand(*data.args.get(1).ok_or_else(unsupported)?, W1, deopt)?;
                self.unbox_single(F0, W0);
                self.unbox_single(F1, W1);
                match data.opcode {
                    FloatAdd => self.asm.word(a64::fadd_single(F0, F0, F1)),
                    FloatSub => self.asm.word(a64::fsub_single(F0, F0, F1)),
                    _ => self.asm.word(a64::fmul_single(F0, F0, F1)),
                }
                self.box_single(F0);
            }
            FixnumAdd | FixnumSub | FixnumMul | FixnumNeg | FixnumCmpEq | FixnumCmpLt
            | FixnumCmpLe | FixnumCmpGt | FixnumCmpGe => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(W0, deopt);
                if data.opcode != FixnumNeg {
                    self.load(*data.args.get(1).ok_or_else(unsupported)?, W1)?;
                    self.guard_fixnum(W1, deopt);
                }
                match data.opcode {
                    // Both operands are tagged by the same shift, so the tagged sum
                    // is the sum's tagging. Overflow in the tagged domain is
                    // detected three bits early, which only deopts sooner than
                    // strictly necessary — never later.
                    FixnumAdd => self.asm.word(a64::adds(W0, W0, W1)),
                    FixnumSub => self.asm.word(a64::subs(W0, W0, W1)),
                    FixnumMul => {
                        self.multiply_fixnums(deopt);
                        return self.store(result, W0);
                    }
                    FixnumNeg => self.asm.word(a64::subs(W0, a64::XZR, W0)),
                    comparison => {
                        // Branchless: materialise both answers and select, rather
                        // than branching around two constant loads.
                        self.compare(W0, W1);
                        self.imm(T0, NIL.0);
                        self.imm(T1, T.0);
                        let cc = match comparison {
                            FixnumCmpEq => Cc::E,
                            FixnumCmpLt => Cc::L,
                            FixnumCmpLe => Cc::Le,
                            FixnumCmpGt => Cc::G,
                            FixnumCmpGe => Cc::Ge,
                            _ => unreachable!(),
                        };
                        self.asm.word(a64::csel(W0, T1, T0, cc));
                        return self.store(result, W0);
                    }
                }
                self.asm.jcc(Cc::O, deopt);
            }
            Car | Cdr => {
                // A cons cell is HEADERLESS and its tagged pointer differs from
                // the cell address only in the low three bits, so masking them
                // off gives the base and the two slots sit at +0 and +8. Mirrors
                // emit.rs, including its refusal of the guarded form: this is the
                // fast path for a cons whose type the IR has already proven, and
                // a Car still carrying a guard, an effect or a FrameState needs
                // the general path instead.
                if data.flags.guard || data.flags.effectful || data.frame_state.is_some() {
                    return Err(unsupported());
                }
                self.asm
                    .word(a64::and_imm(W0, W0, !7u64).ok_or_else(unsupported)?);
                let offset = if data.opcode == Car { 0 } else { 8 };
                self.load_at(W0, W0, offset)?;
            }
            LogAnd | LogOr | LogXor => {
                // Exact on the tagged representation, exactly as the x86 emitter
                // does it: both operands carry the same zero tag, so
                // tagged(a) OP tagged(b) = (a OP b)<<3 = tagged(a OP b). No untag,
                // no retag, no overflow.
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(W0, deopt);
                self.load(*data.args.get(1).ok_or_else(unsupported)?, W1)?;
                self.guard_fixnum(W1, deopt);
                match data.opcode {
                    LogAnd => self.asm.word(a64::and(W0, W0, W1)),
                    LogOr => self.asm.word(a64::orr(W0, W0, W1)),
                    _ => self.asm.word(a64::eor(W0, W0, W1)),
                }
            }
            LogNot => {
                // NOT leaves the tag bits set, so they must be cleared again:
                // ~(x<<3) has its low three bits all 1, and masking them off
                // yields ~(x<<3) - 7 = (~x)<<3, which is tagged(~x).
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(W0, deopt);
                self.asm.word(a64::mvn(W0, W0));
                self.asm
                    .word(a64::and_imm(W0, W0, !7u64).ok_or_else(unsupported)?);
            }
            FixnumShl | FixnumShr => {
                // (ash x n) for a CONSTANT n; a variable amount declines. The
                // constants map holds TAGGED bits, so the shift count is
                // recovered by untagging it.
                let deopt = self.deopt_label(data)?;
                let amount = *data.args.get(1).ok_or_else(unsupported)?;
                let tagged = *self.constants.get(&amount).ok_or_else(unsupported)?;
                let n = (tagged as i64) >> 3;
                self.guard_fixnum(W0, deopt);
                // DIRECTION, mirroring emit.rs and easy to get backwards: on
                // FixnumShl a NEGATIVE constant is a RIGHT shift, because that is
                // how `(ash x -2)` lowers -- ASH becomes FixnumShl whatever the
                // sign, and the emitter reads the constant to choose. FixnumShr is
                // always a right shift. Inverting this is not a crash, it is a
                // wrong number: it made `(ash -17 -2)` answer -68 (a left shift by
                // two) instead of -5, and only differential testing against x86-64
                // showed it.
                let shift_right = if data.opcode == FixnumShr {
                    Some(n.max(0))
                } else if n < 0 {
                    Some(-n)
                } else {
                    None
                };
                if let Some(right) = shift_right {
                    // Right shift: untag, shift arithmetically, retag. Cannot
                    // overflow. Saturated at 60 because shifting a 61-bit payload
                    // further only ever yields 0 or -1, which the shift already
                    // gives, and AArch64 requires the amount below the width.
                    let k = right.min(60) as u32;
                    self.asm.word(a64::asr_imm(W0, W0, 3 + k));
                    self.asm.word(a64::lsl_imm(W0, W0, 3));
                } else {
                    // Left shift by n: tagged(x)<<n = tagged(x<<n), but it can
                    // leave fixnum range, and unlike add/sub there is no flag for
                    // that. Shift back and compare: if the value does not survive
                    // the round trip it overflowed, so deopt and let the generic
                    // path produce a bignum.
                    let k = n as u32;
                    if k > 60 {
                        return Err(unsupported());
                    }
                    self.asm.word(a64::lsl_imm(T0, W0, k));
                    self.asm.word(a64::asr_imm(T1, T0, k));
                    self.asm.word(a64::cmp(T1, W0));
                    self.asm.jcc(Cc::Ne, deopt);
                    self.asm.word(a64::mov(W0, T0));
                }
            }
            _ => return Err(unsupported()),
        }
        // Do not overwrite any allocated home until every guard has passed: a
        // failing instruction must reconstruct its pre-instruction state.
        self.store(result, W0)
    }

    fn deopt_source(&mut self, source: &ValueSource, state: &FrameState) -> Result<(), EmitError> {
        match source {
            ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            } => self.load(*value, W0),
            ValueSource::Const(value)
                if !value.is_heap_object() && !value.is_cons() && !value.is_function() =>
            {
                self.imm(W0, value.0);
                Ok(())
            }
            ValueSource::Unbound => {
                self.imm(W0, UNBOUND.0);
                Ok(())
            }
            ValueSource::Remat(id) if (id.0 as usize) < state.remat.len() => {
                let offset = self.remat_base + id.0 as i32 * 8;
                self.load_at(W0, a64::SP, offset)
            }
            _ => Err(unsupported()),
        }
    }

    /// The cold path behind a failed guard: rebuild the interpreter's view of this
    /// activation into frame slots, then hand it to the deopt callback.
    fn deopt(&mut self, state: &FrameState, callback: u64) -> Result<(), EmitError> {
        for index in rematerialization_order(state)? {
            let recipe = &state.remat[index];
            if recipe.result_repr != ValueRepresentation::Tagged {
                return Err(unsupported());
            }
            match recipe.op {
                RematOp::Const
                | RematOp::BoxFixnum
                | RematOp::UnboxFixnum
                | RematOp::BoxFloat
                | RematOp::UnboxFloat => {
                    if recipe.inputs.len() != 1 {
                        return Err(unsupported());
                    }
                    let input = recipe.inputs[0].clone();
                    self.deopt_source(&input, state)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(unsupported());
                    }
                    let (left, right) = (recipe.inputs[0].clone(), recipe.inputs[1].clone());
                    self.deopt_source(&left, state)?;
                    self.mov(W1, W0);
                    self.deopt_source(&right, state)?;
                    if recipe.op == RematOp::FixnumAdd {
                        self.asm.word(a64::add(W0, W1, W0));
                    } else {
                        self.asm.word(a64::sub(W0, W1, W0));
                    }
                }
            }
            let offset = self.remat_base + index as i32 * 8;
            self.store_at(W0, a64::SP, offset)?;
        }
        let mut words = 0;
        let scopes = state.scopes.clone();
        for scope in &scopes {
            for header in [
                scope.function as u64,
                scope.bcp as u64,
                scope.locals.len() as u64,
                scope.stack.len() as u64,
            ] {
                self.imm(W0, header);
                let offset = self.deopt_base + words * 8;
                self.store_at(W0, a64::SP, offset)?;
                words += 1;
            }
            for source in scope.locals.iter().chain(&scope.stack) {
                self.deopt_source(source, state)?;
                let offset = self.deopt_base + words * 8;
                self.store_at(W0, a64::SP, offset)?;
                words += 1;
            }
        }
        // The T0 resume adapter's ABI: scope count, total words, the buffer, and a
        // reserved zero. It must root the stream before it allocates.
        self.imm(W0, scopes.len() as u64);
        self.imm(W1, words as u64);
        let deopt_base = self.deopt_base;
        self.address(T0, a64::SP, deopt_base)?;
        self.imm(T1, 0);
        self.imm(ADDR, callback);
        self.call_register(ADDR);
        self.imm(W0, NIL.0);
        self.epilogue()
    }
}

/// Emit without ordinary runtime adapters. Calls decline compilation; guards can
/// still serialize virtual scopes for the precise T0 resume adapter.
pub fn emit_framed(
    function: &Function,
    deopt_t2: u64,
    activation_slots: u16,
) -> Result<FramedCode, EmitError> {
    emit_framed_with_runtime(
        function,
        deopt_t2,
        activation_slots,
        RuntimeCalls::default(),
    )
}

/// Emit optimized AArch64 code with native roots synchronized through extra
/// activation slots at runtime calls. Every cycle crosses a sampled GC/signal poll
/// before its edge transfers, so a hot loop stays interruptible and reachable by
/// stop-the-world collection.
pub fn emit_framed_with_runtime(
    function: &Function,
    deopt_t2: u64,
    activation_slots: u16,
    runtime: RuntimeCalls,
) -> Result<FramedCode, EmitError> {
    let positions: HashMap<Block, usize> = function
        .block_order()
        .iter()
        .enumerate()
        .map(|(index, &block)| (block, index))
        .collect();
    // Any edge that can close a cycle must carry a poll, or a hot loop would spin
    // without ever reaching a safepoint.
    let mut polls = HashSet::new();
    for (index, &block) in function.block_order().iter().enumerate() {
        let terminator = function
            .block(block)
            .insts
            .last()
            .copied()
            .ok_or_else(unsupported)?;
        if function
            .inst(terminator)
            .targets
            .iter()
            .any(|target| positions.get(&target.block).is_none_or(|&t| t <= index))
        {
            if runtime.poll == 0 {
                return Err(unsupported());
            }
            polls.insert(terminator);
        }
    }

    let mut machine = super::lower::lower(function);
    super::regalloc::allocate_framed_a64(&mut machine)
        .map_err(|error| EmitError::RegAlloc(format!("{error:?}")))?;

    let mut emitter = Emitter {
        function,
        asm: Asm::new(),
        homes: HashMap::new(),
        constants: HashMap::new(),
        heap_constants: HashMap::new(),
        blocks: HashMap::new(),
        deopts: Vec::new(),
        frame_bytes: 0,
        edge_base: 0,
        deopt_base: 0,
        remat_base: 0,
        runtime,
        activation_slots,
        root_slots: 0,
        argument_slots: 0,
        roots: HashMap::new(),
        root_sites: Vec::new(),
        result_offset: 0,
        poll_offset: 0,
        polls,
        transfer_exit: None,
    };

    // Fold constants up front: they are rematerialised at each use rather than
    // occupying a home, and a movable heap literal is remembered by its slot.
    for &block in function.block_order() {
        let label = emitter.asm.label();
        emitter.blocks.insert(block, label);
        for &inst in &function.block(block).insts {
            let data = function.inst(inst);
            let Some(&result) = data.results.first() else {
                continue;
            };
            let bits = match (&data.opcode, &data.aux) {
                (Opcode::ConstFixnum, AuxData::FixnumImm(value)) => EgclVal::from_fixnum(*value).0,
                (Opcode::ConstFloat, AuxData::FloatImm(value)) => {
                    ((value.to_bits() as u64) << 32) | 4
                }
                (Opcode::ConstChar, AuxData::CharImm(value)) => EgclVal::from_char(*value).0,
                (Opcode::ConstSymbol, AuxData::SymbolRef(value)) => {
                    EgclVal::from_symbol_index(*value).0
                }
                (Opcode::ConstNil, _) => NIL.0,
                (Opcode::ConstT, _) => T.0,
                (Opcode::ConstHeapObj, AuxData::HeapLiteral { slot }) => {
                    emitter.heap_constants.insert(result, *slot);
                    continue;
                }
                _ => continue,
            };
            emitter.constants.insert(result, bits);
        }
    }

    // A value with one location for its whole reported live range keeps that
    // location; a split range gets a stable frame slot instead.
    let mut spill_slots = machine.num_spill_slots;
    for index in 0..function.num_values() {
        let value = Value(index as u32);
        if function.value(value).repr != ValueRepresentation::Tagged {
            return Err(unsupported());
        }
        if emitter.constants.contains_key(&value) || emitter.heap_constants.contains_key(&value) {
            continue;
        }
        let mut ranges = machine
            .value_locations
            .iter()
            .filter(|range| range.vreg.class == RegClass::Gpr && range.vreg.num == value.0);
        let stable = ranges
            .next()
            .map(|range| range.location)
            .filter(|first| ranges.all(|range| range.location == *first));
        let home = match stable {
            Some(Location::Register(register)) => Home::Register(register.encoding),
            Some(Location::Stack(slot)) => Home::Stack(slot.0),
            None => {
                let slot = spill_slots;
                spill_slots += 1;
                Home::Stack(slot)
            }
        };
        emitter.homes.insert(value, home);
    }

    // The same precise regalloc2 ranges that chose homes determine the live roots
    // at each runtime call. Include dying arguments, and exclude results that do
    // not exist until the callback returns.
    for (index, inst) in machine.insts.iter().enumerate() {
        let Some(source) = inst.source_inst else {
            continue;
        };
        if !calls_runtime(function.inst(source).opcode) {
            continue;
        }
        let after = u32::try_from(index).map_err(|_| unsupported())? * 2 + 1;
        let data = function.inst(source);
        let mut roots: Vec<Value> = machine
            .value_locations
            .iter()
            .filter(|range| {
                range.vreg.class == RegClass::Gpr && range.start <= after && after < range.end
            })
            .map(|range| Value(range.vreg.num))
            .chain(data.args.iter().copied())
            .chain(
                data.targets
                    .iter()
                    .flat_map(|target| target.args.iter().copied()),
            )
            .filter(|value| emitter.homes.contains_key(value))
            .collect();
        roots.sort_by_key(|value| value.0);
        roots.dedup();
        emitter.roots.insert(source, roots);
    }
    emitter.root_slots = u16::try_from(emitter.roots.values().map(Vec::len).max().unwrap_or(0))
        .map_err(|_| unsupported())?;
    emitter.argument_slots = u16::try_from(
        function
            .block_order()
            .iter()
            .flat_map(|&block| &function.block(block).insts)
            .map(|&inst| function.inst(inst))
            .filter(|data| data.opcode == Opcode::Call)
            .map(|data| data.args.len())
            .max()
            .unwrap_or(0),
    )
    .map_err(|_| unsupported())?;
    let shadow_root_slots = emitter
        .root_slots
        .checked_add(emitter.argument_slots)
        .ok_or_else(unsupported)?;
    activation_slots
        .checked_add(shadow_root_slots)
        .ok_or_else(unsupported)?;

    let edge_words = function
        .block_order()
        .iter()
        .map(|&b| function.block(b).params.len())
        .max()
        .unwrap_or(0);
    let deopt_words = function
        .frame_states
        .iter()
        .map(|(_, state)| {
            state
                .scopes
                .iter()
                .map(|scope| 4 + scope.locals.len() + scope.stack.len())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let remat_words = function
        .frame_states
        .iter()
        .map(|(_, state)| state.remat.len())
        .max()
        .unwrap_or(0);
    // Two extra words hold the call result and the poll counter. Round to a pair
    // so the stack pointer stays 16-byte aligned, which AAPCS64 requires at a call.
    let frame_words = (spill_slots as usize + edge_words + deopt_words + remat_words + 2 + 1) & !1;
    // Every frame access must remain reachable; beyond this the emitter would be
    // materialising an address for each one.
    if frame_words > 0x1000 {
        return Err(unsupported());
    }
    emitter.frame_bytes = (frame_words * 8) as i32;
    emitter.edge_base = spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    emitter.remat_base = emitter.deopt_base + deopt_words as i32 * 8;
    emitter.result_offset = (frame_words as i32 - 2) * 8;
    emitter.poll_offset = emitter.result_offset + 8;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported());
    }

    emitter.prologue()?;
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.load_at(W0, SLOTS, index as i32 * 8)?;
        emitter.store(parameter, W0)?;
    }
    emitter.asm.jmp(emitter.blocks[&function.entry()]);

    let mut bcp_offsets = Vec::new();
    for &block in function.block_order() {
        let label = emitter.blocks[&block];
        emitter.asm.bind(label);
        for &instruction in &function.block(block).insts {
            let data = function.inst(instruction);
            if let Some(state) = data.frame_state {
                if let Some(scope) = function.frame_states.get(state).scopes.first() {
                    bcp_offsets.resize(bcp_offsets.len().max(scope.bcp as usize + 1), u32::MAX);
                    let offset = &mut bcp_offsets[scope.bcp as usize];
                    *offset = (*offset).min(emitter.asm.here() as u32);
                }
            }
            emitter.instruction(instruction, data)?;
        }
    }

    // OSR entries: an alternate prologue that imports the live values from frame
    // slots and jumps into the loop, rather than restarting the function.
    let mut osr_entries = Vec::new();
    for osr in &function.osr_entries {
        let state = function.frame_states.get(osr.frame_state);
        let Some(scope) = state.scopes.first() else {
            continue;
        };
        if state.scopes.len() != 1
            || !scope.stack.is_empty()
            || scope.locals.len() > activation_slots as usize
        {
            continue;
        }
        let specs = super::slot_map::slot_specs(scope, state);
        if specs
            .iter()
            .any(|spec| spec.repr != ValueRepresentation::Tagged)
        {
            continue;
        }
        let Some(&block_index) = positions.get(&osr.block) else {
            continue;
        };
        let point = machine.blocks[block_index].start as u32 * 2;
        let live: HashSet<_> = machine
            .value_locations
            .iter()
            .filter(|range| {
                range.vreg.class == RegClass::Gpr && range.start <= point && point < range.end
            })
            .map(|range| Value(range.vreg.num))
            .filter(|value| emitter.homes.contains_key(value))
            .collect();
        let imports: Vec<_> = specs
            .iter()
            .filter_map(|spec| match spec.source {
                ValueSource::Value { value, .. } if live.contains(&value) => {
                    Some((spec.index, value))
                }
                _ => None,
            })
            .collect();
        // An entry must reconstruct every live home, including values defined above
        // the loop. Constants are rematerialised by `load`.
        if live
            .iter()
            .any(|value| !imports.iter().any(|(_, imported)| imported == value))
        {
            continue;
        }
        let offset = emitter.asm.here();
        emitter.prologue()?;
        for (slot, value) in imports {
            emitter.load_at(W0, SLOTS, slot as i32 * 8)?;
            emitter.store(value, W0)?;
        }
        emitter.asm.jmp(emitter.blocks[&osr.block]);
        osr_entries.push((osr.bcp, offset));
    }

    let has_deopt = !emitter.deopts.is_empty();
    if let Some(exit) = emitter.transfer_exit {
        emitter.asm.bind(exit);
        emitter.imm(W0, NIL.0);
        emitter.epilogue()?;
    }
    for (state, label) in std::mem::take(&mut emitter.deopts) {
        emitter.asm.bind(label);
        emitter.deopt(function.frame_states.get(state), deopt_t2)?;
    }

    Ok(FramedCode {
        #[cfg(all(target_arch = "x86_64", windows))]
        windows_unwind: Vec::new(),
        code: emitter.asm.finish().ok_or(EmitError::BadBranch)?,
        compiled_entry: 0,
        osr_entries,
        bcp_offsets,
        native_spill_slots: frame_words as u32,
        regalloc_spill_slots: machine.num_spill_slots,
        allocation_edits: machine.allocation_edits.len(),
        shadow_root_slots,
        emitted_safepoints: emitter.root_sites.len(),
        root_sync_sites: emitter.root_sites,
        heap_constant_slots: emitter.heap_constants.values().copied().collect(),
        has_deopt,
    })
}
