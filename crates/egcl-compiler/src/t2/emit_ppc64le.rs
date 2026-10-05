// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! ppc64le (ELFv2) machine-code emission for the T2 framed pipeline.
//!
//! # Purpose
//!
//! Turns an optimised SSA [`Function`] into ELFv2 code over the shared
//! activation contract: an `EgclStack` activation of `activation_slots` tagged
//! words, extended by the shadow-root slots needed at runtime calls. The module
//! owns instruction selection for the opcode subset below, the frame layout,
//! GC-root synchronisation around runtime calls, sampled back-edge polls, OSR
//! entry prologues and guard-failure deopt exits. Native transfer dispatch is
//! not here: the ppc64le transfer stubs live in native_transfer_ppc64le.rs,
//! and the ppc64le native-segment entry uses `emit_framed` (no adapters) for
//! allocation-free, scope-free, deopt-free bodies only.
//!
//! The fourth backend of this shape, structured like the other three and
//! sharing [`RuntimeCalls`] and `rematerialization_order` with System Z.
//!
//! # Design
//!
//! Register allocation runs (`allocate_framed_ppc64le`, pool r20–r29, all
//! nonvolatile), but emission is home-based: a value whose every
//! `value_locations` range agrees keeps that register or frame slot as its
//! *home*; a split value gets a fresh stable frame slot. Each instruction loads
//! operands into fixed working registers, operates, and stores back.
//!
//! ELFv2 passes the first arguments from r3 and returns in r3, so the working
//! registers double as argument registers as on the other two targets:
//!
//! | role                                          | System Z | AArch64 | ppc64le |
//! |-----------------------------------------------|----------|---------|---------|
//! | primary working / first argument / result     | r2       | x0      | r3      |
//! | secondary working / second argument           | r3       | x1      | r4      |
//! | temporaries / third and fourth arguments      | r4, r5   | x2, x3  | r5, r6  |
//! | address scratch                               | r1       | x16     | r11     |
//! | activation (frame-slots) pointer, whole body  | r13      | x28     | r14     |
//! | stack pointer                                 | r15      | sp      | r1      |
//!
//! Two things POWER forces that the others do not. An indirect call goes
//! through the count register and clobbers the link register, and the ABI
//! expects the target in r12 so a callee entered at its global entry point can
//! compute its own TOC, which is why r2 is saved and restored around every call
//! (`emit_call`). And overflow cannot use `XER[SO]`: it is sticky, and the
//! instruction that cheaply cleared it no longer exists at this ISA level
//! (bliss-lpgl1), so add/sub test operand signs explicitly and multiply
//! compares `mulhd` against the low half's sign.
//!
//! # Frame
//!
//! The link register is saved in the caller's frame at `frame::LINK_SLOT`
//! before `stdu` claims this one, as the ABI prescribes; `frame::SAVED`
//! (r14–r16, r20–r29) go at the top of the frame. From `frame::LOCALS_BASE`
//! upward: spill slots (regalloc2's plus stable homes for split values), the
//! edge parallel-copy buffer, the deopt serialisation buffer and the remat
//! buffer; the call result and poll countdown use the fixed
//! `frame::SCRATCH_SLOT` and `frame::POLL_SLOT`. Every access uses a signed
//! 16-bit displacement, so a frame over 32752 bytes declines.
//!
//! The entry prologue imports the entry block's parameters from activation
//! slots `0..n`. OSR entries are extra prologues for each `osr_entries` record
//! whose frame state is single-scope, stack-empty and all-tagged and whose
//! every live home at the loop header is importable from a slot; they are
//! published as `(bcp, offset)` in [`FramedCode`].
//!
//! # Runtime calls and GC roots
//!
//! `Call`, `SymbolValue`, `SymbolFunction`, `SetSymbolValue` and
//! `TakeValuesToLocals` go through [`RuntimeCalls`] adapters. `ClearMv` is a
//! local no-op on this target: a direct native segment enters with the
//! caller's multiple-value state already cleared, and routing the marker
//! through an adapter would make otherwise leaf bodies ineligible for the
//! adapter-free emitter. Around each call `runtime_call` clears every shadow
//! slot to NIL, writes each live tagged value into the shadow area after
//! `activation_slots` (roots, then arguments), calls, and reloads each root
//! from its shadow before assigning results. The live set is derived from the
//! regalloc2 ranges covering the instruction's Late point plus its uses and
//! edge args, minus its defs. Each site is a [`RootSyncSite`] and installation
//! checks the count against `emitted_safepoints`. A nonzero `transfer_pending`
//! is consulted after each call and a nonzero answer takes the shared NIL exit
//! (the checked legacy ABI). Back-edge polls decrement a 256-count frame word
//! and call `poll` through the same path at zero; a cycle without a `poll`
//! adapter declines.
//!
//! # Supported opcodes
//!
//! Constants (`Const*` folded; heap literals loaded through their rooted
//! constant-pool slot), `Return`, `Jump`, two-way `Brif`, `Guard` for the
//! FIXNUM and SINGLE_FLOAT tags, `FloatAdd/Sub/Mul` on tagged single-floats
//! (unboxed through the direct GPR↔FPR move), `FixnumAdd/Sub/Mul/Neg` and the
//! five comparisons (branchless via `isel`), all tag-guarded and deopting on
//! overflow. Anything else returns `UnsupportedOp(0x9C)`.
//!
//! # Deopt exits
//!
//! Each guard gets a cold label. The exit evaluates remat recipes in
//! dependency order into the remat buffer, serialises each scope as
//! `(function, bcp, nlocals, nstack)` followed by its slot words, and calls
//! `deopt_t2` with `(nscopes, nwords, buffer, 0)`; no home is written before
//! an instruction's guards pass. The adapter must root the stream before it
//! allocates.

use super::emit::{EmitError, FramedCode, RootSyncSite};
use super::emit_s390x::rematerialization_order;

/// The C-ABI runtime adapters, as the System Z emitter defines them. Re-exported
/// rather than redefined so one call site fills either backend's.
pub use super::emit_s390x::RuntimeCalls;
use super::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, Inst, InstData, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use super::mach::{Location, RegClass};
use std::collections::{HashMap, HashSet};
use egcl_rt::asm::Cc;
use egcl_rt::asm_ppc64le::{Asm, Label, frame};
use egcl_rt::value::{NIL, T, EgclVal, UNBOUND};

/// Primary working register, and the ABI's first argument and return register.
const W0: u8 = 3;
/// Secondary working register / second argument.
const W1: u8 = 4;
/// Temporaries, and the third and fourth arguments.
const T0: u8 = 5;
const T1: u8 = 6;
/// Address scratch, and the call target — the ABI expects the latter in r12.
const ADDR: u8 = 11;
const TARGET: u8 = 12;
/// Holds the frame-slots pointer for the whole body.
const SLOTS: u8 = 14;
/// The stack pointer and the TOC.
const SP: u8 = 1;
const TOC: u8 = 2;
/// Used only for moving the link register, where r0's literal-zero reading in some
/// forms cannot matter.
const LINK: u8 = 0;
/// Working float registers, from the call-clobbered range.
const F0: u8 = 0;
const F1: u8 = 1;

/// Back-edge polls run one in this many iterations, matching the other emitters.
const POLL_PERIOD: u64 = 256;

fn calls_runtime(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Call
            | Opcode::SymbolValue
            | Opcode::SymbolFunction
            | Opcode::SetSymbolValue
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
    blocks: HashMap<Block, Label>,
    deopts: Vec<(FrameStateId, Label)>,
    frame_bytes: i32,
    spill_base: i32,
    edge_base: i32,
    deopt_base: i32,
    remat_base: i32,
    runtime: RuntimeCalls,
    activation_slots: u16,
    root_slots: u16,
    argument_slots: u16,
    roots: HashMap<Inst, Vec<Value>>,
    root_sites: Vec<RootSyncSite>,
    polls: HashSet<Inst>,
    transfer_exit: Option<Label>,
}

fn unsupported() -> EmitError {
    EmitError::UnsupportedOp(0x9C)
}

impl Emitter<'_> {
    // ── values and their homes ───────────────────────────────────

    fn load(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        if let Some(&bits) = self.constants.get(&value) {
            self.asm.imm64(register, bits);
        } else if let Some(&slot) = self.heap_constants.get(&value) {
            // A movable heap constant is read through its stable constants slot,
            // never baked in: the collector rewrites that slot and cannot patch
            // machine code.
            self.asm.imm64(ADDR, slot as u64);
            self.asm.load(register, ADDR, 0).ok_or_else(unsupported)?;
        } else {
            match *self.homes.get(&value).ok_or_else(unsupported)? {
                Home::Register(source) => self.asm.mov(register, source),
                Home::Stack(slot) => {
                    let offset = self.spill_base + slot as i32 * 8;
                    self.frame_load(register, offset)?;
                }
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        match *self.homes.get(&value).ok_or_else(unsupported)? {
            Home::Register(destination) => self.asm.mov(destination, register),
            Home::Stack(slot) => {
                let offset = self.spill_base + slot as i32 * 8;
                self.frame_store(register, offset)?;
            }
        }
        Ok(())
    }

    /// Load from this frame. The displacement field is 16 bits and every frame this
    /// emitter builds fits, so an overflow is a decline rather than a fallback.
    fn frame_load(&mut self, register: u8, offset: i32) -> Result<(), EmitError> {
        self.asm.load(register, SP, offset).ok_or_else(unsupported)
    }

    fn frame_store(&mut self, register: u8, offset: i32) -> Result<(), EmitError> {
        self.asm.store(register, SP, offset).ok_or_else(unsupported)
    }

    /// Load from the activation, whose pointer is held for the whole body.
    fn slots_load(&mut self, register: u8, offset: i32) -> Result<(), EmitError> {
        self.asm
            .load(register, SLOTS, offset)
            .ok_or_else(unsupported)
    }

    fn slots_store(&mut self, register: u8, offset: i32) -> Result<(), EmitError> {
        self.asm
            .store(register, SLOTS, offset)
            .ok_or_else(unsupported)
    }

    /// `register = base + displacement`, materialising an offset beyond the
    /// add-immediate field.
    fn address(&mut self, register: u8, base: u8, displacement: i32) {
        match i16::try_from(displacement) {
            Ok(small) => self.asm.addi(register, base, small),
            Err(_) => {
                self.asm.imm64(ADDR, displacement as i64 as u64);
                self.asm.add(register, base, ADDR);
            }
        }
    }

    /// Compare against a constant, through a register when it does not fit the
    /// immediate field — which tagged values generally do not.
    fn compare_imm(&mut self, register: u8, value: u64) {
        match i16::try_from(value as i64) {
            Ok(small) => self.asm.compare_imm(0, register, small),
            Err(_) => {
                self.asm.imm64(ADDR, value);
                self.asm.compare(0, register, ADDR);
            }
        }
    }

    // ── frame ────────────────────────────────────────────────────

    fn prologue(&mut self) -> Result<(), EmitError> {
        // The link register is saved in the CALLER's frame, as the ABI prescribes,
        // and therefore before this frame is claimed.
        self.asm.move_from_link(LINK);
        self.asm
            .store(LINK, SP, frame::LINK_SLOT)
            .ok_or_else(unsupported)?;
        let size = self.frame_bytes;
        self.asm
            .store_update(SP, SP, -size)
            .ok_or_else(unsupported)?;
        for (index, register) in frame::SAVED.into_iter().enumerate() {
            let offset = frame::saved_offset(size, index);
            self.asm
                .store(register, SP, offset)
                .ok_or_else(unsupported)?;
        }
        // The first argument is the frame-slots pointer, and it must survive the
        // whole body, so it moves out of the working registers immediately.
        self.asm.mov(SLOTS, W0);
        self.asm.imm64(W0, POLL_PERIOD);
        self.frame_store(W0, frame::POLL_SLOT)
    }

    fn epilogue(&mut self) -> Result<(), EmitError> {
        let size = self.frame_bytes;
        for (index, register) in frame::SAVED.into_iter().enumerate() {
            let offset = frame::saved_offset(size, index);
            self.asm
                .load(register, SP, offset)
                .ok_or_else(unsupported)?;
        }
        self.address(SP, SP, size);
        self.asm
            .load(LINK, SP, frame::LINK_SLOT)
            .ok_or_else(unsupported)?;
        self.asm.move_to_link(LINK);
        self.asm.ret();
        Ok(())
    }

    // ── guards ───────────────────────────────────────────────────

    fn deopt_label(&mut self, data: &InstData) -> Result<Label, EmitError> {
        let state = data.frame_state.ok_or_else(unsupported)?;
        if self.function.frame_states.get(state).scopes.is_empty() {
            return Err(unsupported());
        }
        let label = self.asm.label();
        self.deopts.push((state, label));
        Ok(label)
    }

    /// A fixnum's low three tag bits are zero.
    fn guard_fixnum(&mut self, register: u8, deopt: Label) {
        self.asm.li(T0, 7);
        self.asm.and(T0, register, T0);
        self.asm.compare_imm(0, T0, 0);
        self.asm.branch(Cc::Ne, 0, deopt);
    }

    /// A tagged single-float carries tag 4 and its f32 bits in the high half.
    fn guard_single_float(&mut self, register: u8, deopt: Label) {
        self.asm.li(T0, 7);
        self.asm.and(T0, register, T0);
        self.asm.compare_imm(0, T0, 4);
        self.asm.branch(Cc::Ne, 0, deopt);
    }

    fn float_operand(&mut self, value: Value, register: u8, deopt: Label) -> Result<(), EmitError> {
        if let Some(&bits) = self.constants.get(&value) {
            let constant = EgclVal(bits);
            if constant.is_fixnum() {
                self.asm.imm64(
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

    /// Unbox a tagged single-float from `gpr` into `fpr`, through the direct
    /// register-file move POWER provides.
    fn unbox_single(&mut self, fpr: u8, gpr: u8) {
        self.asm.srdi(T1, gpr, 32);
        self.asm.sldi(T1, T1, 32);
        self.asm.move_to_float(fpr, T1);
    }

    /// Multiply the tagged fixnums in `W0` and `W1`, leaving the tagged product in
    /// `W0` and deopting on overflow.
    ///
    /// The flag-setting multiply routes through the sticky `XER[SO]`, so it is
    /// unusable for a per-operation guard. Untag one operand so the product is
    /// tagged once, then require the high half of the signed 128-bit product to be
    /// exactly the low half's sign extension — the same shape AArch64 needs `SMULH`
    /// for. No home is written before the exit, so a deopt still sees both inputs.
    fn multiply_fixnums(&mut self, deopt: Label) {
        self.asm.sradi(T0, W0, 3);
        self.asm.mulhd(T1, T0, W1);
        self.asm.mulld(W0, T0, W1);
        self.asm.sradi(ADDR, W0, 63);
        self.asm.compare(0, T1, ADDR);
        self.asm.branch(Cc::Ne, 0, deopt);
    }

    /// Add or subtract tagged fixnums with an explicit sign-overflow test, for the
    /// same reason: the carry and overflow bits are not usable here.
    ///
    /// A signed addition overflows exactly when both operands share a sign that
    /// differs from the sum's, which `(a ^ sum) & (b ^ sum) < 0` detects.
    fn add_or_subtract_fixnums(&mut self, subtract: bool, deopt: Label) {
        if subtract {
            // Negate the right operand so one overflow test serves both.
            self.asm.neg(W1, W1);
        }
        self.asm.add(T0, W0, W1);
        self.asm.xor(T1, W0, T0);
        self.asm.xor(ADDR, W1, T0);
        self.asm.and(T1, T1, ADDR);
        self.asm.compare_imm(0, T1, 0);
        self.asm.mov(W0, T0);
        self.asm.branch(Cc::L, 0, deopt);
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
            self.frame_store(W0, base + index as i32 * 8)?;
        }
        for (index, destination) in parameters.into_iter().enumerate() {
            self.frame_load(W0, base + index as i32 * 8)?;
            self.store(destination, W0)?;
        }
        self.asm.jump(self.blocks[&edge.block]);
        Ok(())
    }

    /// Call a runtime adapter, synchronising GC roots around it.
    ///
    /// The collector moves objects and can only update roots it knows about, and a
    /// home register or spill slot is invisible to it. So every live value is written
    /// to a shadow slot in the owning EgclStack activation before the call and read
    /// back afterwards, with `RootSyncSite` recording each site so installation can
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
        // Clear the unused shadows too: an earlier call may have had more live roots
        // or arguments, and dead objects need not be retained indefinitely.
        self.asm.imm64(W0, NIL.0);
        for index in 0..u32::from(self.root_slots) + u32::from(self.argument_slots) {
            self.slots_store(W0, root_base + index as i32 * 8)?;
        }
        for (index, &value) in roots.iter().enumerate() {
            self.load(value, W0)?;
            self.slots_store(W0, root_base + index as i32 * 8)?;
        }
        let helper = if let Some(data) = data {
            match data.opcode {
                Opcode::Call => {
                    let AuxData::CallTarget(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    for (index, &arg) in data.args.iter().enumerate() {
                        self.load(arg, W0)?;
                        self.slots_store(W0, argument_base + index as i32 * 8)?;
                    }
                    self.asm.imm64(W0, u64::from(symbol));
                    self.asm.imm64(W1, data.args.len() as u64);
                    self.address(T0, SLOTS, argument_base);
                    self.asm.imm64(T1, 0);
                    self.runtime.call_slice
                }
                Opcode::SymbolValue | Opcode::SymbolFunction | Opcode::SetSymbolValue => {
                    let AuxData::SymbolRef(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    if data.opcode == Opcode::SetSymbolValue {
                        self.load(*data.args.first().ok_or_else(unsupported)?, W1)?;
                    }
                    self.asm.imm64(W0, u64::from(symbol));
                    match data.opcode {
                        Opcode::SymbolValue => self.runtime.load_global,
                        Opcode::SymbolFunction => self.runtime.load_function,
                        _ => self.runtime.store_global,
                    }
                }
                Opcode::ClearMv => {
                    self.asm.imm64(W0, NIL.0);
                    self.asm.imm64(W1, 0);
                    self.asm.imm64(T0, 0);
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
                    self.address(W1, SLOTS, i32::from(slot_base) * 8);
                    self.asm.imm64(T0, u64::from(nvars));
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
        self.emit_call(helper)?;
        self.frame_store(W0, frame::SCRATCH_SLOT)?;
        if data.is_none() || self.runtime.transfer_pending != 0 {
            if data.is_some() {
                self.emit_call(self.runtime.transfer_pending)?;
            }
            self.asm.compare_imm(0, W0, 0);
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.branch(Cc::Ne, 0, exit);
        }
        // Restore roots before assigning results: a dying argument may share its
        // home with the call result and must never overwrite it.
        for (index, &value) in roots.iter().enumerate() {
            self.slots_load(W0, root_base + index as i32 * 8)?;
            self.store(value, W0)?;
        }
        if let Some(data) = data {
            if let AuxData::ValuesLocals { slot_base, .. } = data.aux {
                for (index, &value) in data.results.iter().enumerate() {
                    self.slots_load(W0, (i32::from(slot_base) + index as i32) * 8)?;
                    self.store(value, W0)?;
                }
            } else if let Some(&result) = data.results.first() {
                self.frame_load(W0, frame::SCRATCH_SLOT)?;
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

    /// An indirect call: the target goes in r12 so a callee entered at its global
    /// entry point can compute its own TOC, and r2 is preserved across it.
    fn emit_call(&mut self, target: u64) -> Result<(), EmitError> {
        self.asm
            .store(TOC, SP, frame::TOC_SLOT)
            .ok_or_else(unsupported)?;
        self.asm.imm64(TARGET, target);
        self.asm.move_to_count(TARGET);
        self.asm.call_count();
        self.asm
            .load(TOC, SP, frame::TOC_SLOT)
            .ok_or_else(unsupported)
    }

    fn instruction(&mut self, instruction: Inst, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
        // A direct native segment enters with the caller's multiple-value state
        // already cleared.  The narrow no-call/no-handler path cannot produce
        // additional values, so the bytecode ClearMv marker is a local no-op;
        // routing it through a runtime adapter would make otherwise leaf bodies
        // ineligible for the adapter-free emitter.
        if data.opcode == ClearMv {
            return Ok(());
        }
        if self.polls.contains(&instruction) {
            // Sampled back-edge safepoint, so a hot loop still reaches GC
            // stop-the-world and remains interruptible.
            let skip = self.asm.label();
            self.frame_load(W0, frame::POLL_SLOT)?;
            self.asm.addi(W0, W0, -1);
            self.frame_store(W0, frame::POLL_SLOT)?;
            self.asm.compare_imm(0, W0, 0);
            self.asm.branch(Cc::Ne, 0, skip);
            self.asm.imm64(W0, POLL_PERIOD);
            self.frame_store(W0, frame::POLL_SLOT)?;
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
                    self.asm.imm64(W0, NIL.0);
                }
                return self.epilogue();
            }
            Jump if data.targets.len() == 1 => return self.edge(&data.targets[0]),
            Brif if data.args.len() == 1 && data.targets.len() == 2 => {
                let otherwise = self.asm.label();
                self.load(data.args[0], W0)?;
                self.compare_imm(W0, NIL.0);
                self.asm.branch(Cc::E, 0, otherwise);
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
            Guard if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM || ty.bits == TypeBits::SINGLE_FLOAT) =>
            {
                let deopt = self.deopt_label(data)?;
                if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM) {
                    self.guard_fixnum(W0, deopt);
                } else {
                    self.guard_single_float(W0, deopt);
                }
            }
            FloatAdd | FloatSub | FloatMul => {
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, W0, deopt)?;
                self.float_operand(*data.args.get(1).ok_or_else(unsupported)?, W1, deopt)?;
                self.unbox_single(F0, W0);
                self.unbox_single(F1, W1);
                match data.opcode {
                    FloatAdd => self.asm.add_single(F0, F0, F1),
                    FloatSub => self.asm.sub_single(F0, F0, F1),
                    _ => self.asm.multiply_single(F0, F0, F1),
                }
                self.asm.move_from_float(W0, F0);
                self.asm.srdi(W0, W0, 32);
                self.asm.sldi(W0, W0, 32);
                self.asm.li(T0, 4);
                self.asm.or(W0, W0, T0);
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
                    FixnumAdd => self.add_or_subtract_fixnums(false, deopt),
                    FixnumSub => self.add_or_subtract_fixnums(true, deopt),
                    FixnumMul => {
                        self.multiply_fixnums(deopt);
                        return self.store(result, W0);
                    }
                    FixnumNeg => {
                        // Negating the most negative fixnum overflows, which the same
                        // sign test catches with a zero left operand.
                        self.asm.mov(W1, W0);
                        self.asm.li(W0, 0);
                        self.add_or_subtract_fixnums(true, deopt);
                    }
                    comparison => {
                        // Branchless: `isel` picks between the two answers.
                        self.asm.compare(0, W0, W1);
                        self.asm.imm64(T0, T.0);
                        self.asm.imm64(T1, NIL.0);
                        let cc = match comparison {
                            FixnumCmpEq => Cc::E,
                            FixnumCmpLt => Cc::L,
                            FixnumCmpLe => Cc::Le,
                            FixnumCmpGt => Cc::G,
                            FixnumCmpGe => Cc::Ge,
                            _ => unreachable!(),
                        };
                        self.asm.isel(W0, T0, T1, cc, 0);
                        return self.store(result, W0);
                    }
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
                self.asm.imm64(W0, value.0);
                Ok(())
            }
            ValueSource::Unbound => {
                self.asm.imm64(W0, UNBOUND.0);
                Ok(())
            }
            ValueSource::Remat(id) if (id.0 as usize) < state.remat.len() => {
                let offset = self.remat_base + id.0 as i32 * 8;
                self.frame_load(W0, offset)
            }
            _ => Err(unsupported()),
        }
    }

    /// The cold path behind a failed guard: rebuild the interpreter's view of this
    /// activation into frame slots, then hand it to the deopt callback.
    fn deopt(&mut self, state: &FrameState, callback: u64) -> Result<(), EmitError> {
        for index in rematerialization_order(state)? {
            let recipe = state.remat[index].clone();
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
                    self.deopt_source(&recipe.inputs[0], state)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(unsupported());
                    }
                    self.deopt_source(&recipe.inputs[0], state)?;
                    self.asm.mov(W1, W0);
                    self.deopt_source(&recipe.inputs[1], state)?;
                    if recipe.op == RematOp::FixnumAdd {
                        self.asm.add(W0, W1, W0);
                    } else {
                        self.asm.subf(W0, W0, W1);
                    }
                }
            }
            let offset = self.remat_base + index as i32 * 8;
            self.frame_store(W0, offset)?;
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
                self.asm.imm64(W0, header);
                let offset = self.deopt_base + words * 8;
                self.frame_store(W0, offset)?;
                words += 1;
            }
            for source in scope.locals.iter().chain(&scope.stack) {
                self.deopt_source(source, state)?;
                let offset = self.deopt_base + words * 8;
                self.frame_store(W0, offset)?;
                words += 1;
            }
        }
        // The T0 resume adapter's ABI: scope count, total words, the buffer, and a
        // reserved zero. It must root the stream before it allocates.
        self.asm.imm64(W0, scopes.len() as u64);
        self.asm.imm64(W1, words as u64);
        let deopt_base = self.deopt_base;
        self.address(T0, SP, deopt_base);
        self.asm.imm64(T1, 0);
        self.emit_call(callback)?;
        self.asm.imm64(W0, NIL.0);
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

/// Emit optimized ppc64le code with native roots synchronized through extra
/// activation slots at runtime calls. Every cycle crosses a sampled GC/signal poll
/// before its edge transfers.
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
    super::regalloc::allocate_framed_ppc64le(&mut machine)
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
        spill_base: 0,
        edge_base: 0,
        deopt_base: 0,
        remat_base: 0,
        runtime,
        activation_slots,
        root_slots: 0,
        argument_slots: 0,
        roots: HashMap::new(),
        root_sites: Vec::new(),
        polls,
        transfer_exit: None,
    };

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

    emitter.spill_base = frame::LOCALS_BASE;
    emitter.edge_base = emitter.spill_base + spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    emitter.remat_base = emitter.deopt_base + deopt_words as i32 * 8;
    let locals_end = emitter.remat_base + remat_words as i32 * 8;
    // The save area sits at the top, and every frame access uses a signed 16-bit
    // displacement, so a frame beyond that is a decline.
    let size = (locals_end + frame::SAVED_BYTES + 15) & !15;
    if size > 32752 {
        return Err(unsupported());
    }
    emitter.frame_bytes = size;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported());
    }

    emitter.prologue()?;
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.slots_load(W0, index as i32 * 8)?;
        emitter.store(parameter, W0)?;
    }
    emitter.asm.jump(emitter.blocks[&function.entry()]);

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
        if live
            .iter()
            .any(|value| !imports.iter().any(|(_, imported)| imported == value))
        {
            continue;
        }
        let offset = emitter.asm.here();
        emitter.prologue()?;
        for (slot, value) in imports {
            emitter.slots_load(W0, slot as i32 * 8)?;
            emitter.store(value, W0)?;
        }
        emitter.asm.jump(emitter.blocks[&osr.block]);
        osr_entries.push((osr.bcp, offset));
    }

    let has_deopt = !emitter.deopts.is_empty();
    if let Some(exit) = emitter.transfer_exit {
        emitter.asm.bind(exit);
        emitter.asm.imm64(W0, NIL.0);
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
        native_spill_slots: (size / 8) as u32,
        regalloc_spill_slots: machine.num_spill_slots,
        allocation_edits: machine.allocation_edits.len(),
        shadow_root_slots,
        emitted_safepoints: emitter.root_sites.len(),
        root_sync_sites: emitter.root_sites,
        heap_constant_slots: emitter.heap_constants.values().copied().collect(),
        has_deopt,
    })
}
