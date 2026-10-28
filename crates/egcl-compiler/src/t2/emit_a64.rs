// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AArch64 machine-code emission for the T2 framed pipeline.
//!
//! # Purpose
//!
//! Turns an optimised SSA [`Function`] into AAPCS64 code over the shared
//! activation contract: an `EgclStack` activation of `activation_slots` tagged
//! words, extended by the shadow-root slots needed at runtime calls. The module
//! owns instruction selection for the opcode subset below, the frame layout,
//! GC-root synchronisation around runtime calls, back-edge polls, OSR
//! entry prologues and guard-failure deopt exits. It does not own native
//! transfer dispatch: runtime calls end with the checked `transfer_pending`
//! test, and the AArch64 native-segment entry admits only call-free,
//! scope-free bodies.
//!
//! Structured as the System Z emitter is, for the same reasons, and sharing
//! [`RuntimeCalls`] and `rematerialization_order` with it.
//!
//! # Design
//!
//! Register allocation runs (`allocate_framed_a64`, pool x19–x27, all
//! callee-saved so a value survives a runtime call without the emitter
//! spilling it), but emission is home-based: a value whose every
//! `value_locations` range agrees keeps that register or frame slot as its
//! *home*; a split value gets a fresh stable frame slot. Each instruction loads
//! operands into fixed working registers, operates, and stores back.
//!
//! Register roles map one-to-one onto the System Z emitter's, which is why the
//! two read alike. AAPCS64 passes the first arguments in x0–x7 and returns in
//! x0, so the working registers double as argument registers:
//!
//! | role                                          | System Z | AArch64 |
//! |-----------------------------------------------|----------|---------|
//! | primary working / first argument / result     | r2       | x0      |
//! | secondary working / second argument           | r3       | x1      |
//! | temporaries / third and fourth arguments      | r4, r5   | x2, x3  |
//! | address scratch                               | r1       | x16     |
//! | activation (frame-slots) pointer, whole body  | r13      | x28     |
//! | stack pointer                                 | r15      | sp      |
//!
//! x16 is IP0, already call-clobbered scratch by the architecture, so no home
//! can live there. `PhysReg.encoding` is the architectural register number.
//!
//! # Frame
//!
//! The prologue pushes the frame record (`stp x29, x30`), sets x29, and saves
//! x19–x28 into a `JIT_SAVE_BYTES` area whose layout T1 and the SIGSEGV
//! recovery epilogue share, so one recovery address can unwind either tier.
//! Below that: spill slots (regalloc2's plus stable homes for split values),
//! the edge parallel-copy buffer, the deopt serialisation buffer, the remat
//! buffer, and a word for the saved call result,
//! rounded to a pair so sp stays 16-byte aligned at calls. Frames over 0x1000
//! words decline rather than materialise addresses per access.
//!
//! The entry prologue imports the entry block's parameters from activation
//! slots `0..n`. OSR entries are extra prologues for each `osr_entries` record
//! whose frame state is single-scope, stack-empty and all-tagged and whose
//! every live home at the loop header is importable from a slot; they are
//! published as `(bcp, offset)` in [`FramedCode`].
//!
//! # Runtime calls and GC roots
//!
//! `Call`, `SymbolValue`, `SymbolFunction`, `SetSymbolValue`, `ClearMv`,
//! `TakeValuesToLocals`, `EnvironmentValue` and `SetEnvironmentValue` go
//! through [`RuntimeCalls`] adapters. Because the
//! collector cannot see a home register or native spill slot, `runtime_call`
//! clears every shadow slot to NIL, writes each live tagged value into the
//! shadow area after `activation_slots` (roots, then call arguments), calls,
//! and reloads each root from its shadow before assigning results. The live
//! set is derived from the regalloc2 ranges covering the instruction's Late
//! point plus its uses and edge args, minus its defs. Each site is recorded as
//! a [`RootSyncSite`] and installation checks the count against
//! `emitted_safepoints`. A nonzero `transfer_pending` is consulted after each
//! call and a nonzero answer takes the shared NIL exit (the checked legacy
//! ABI). Each selected backward-edge terminator calls `poll` through the same
//! path before edge moves; a cycle without a `poll` adapter declines.
//!
//! # Supported opcodes
//!
//! The System Z emitter's set apart from `TypeCheck`, plus: `Guard` for the CONS tag
//! (build.rs emits one before every `Car`/`Cdr`, so refusing it cost whole
//! functions their T2 code, bliss-2yews); unguarded, effect-free `Car`/`Cdr`
//! by masking the low three bits of the headerless cons pointer; `LogAnd`,
//! `LogOr`, `LogXor` directly on tagged bits; `LogNot` with the tag re-cleared;
//! `GenericEq` (EQ, NULL, NOT, and the NIL test build.rs puts before CAR/CDR)
//! as a tagged-word compare and `csel`;
//! and `FixnumShl`/`FixnumShr` by a constant amount, where a negative `Shl`
//! constant is a right shift (that is how `(ash x -2)` lowers) and a left
//! shift deopts when the value does not survive the round trip. Fixnum
//! comparisons are branchless (`csel`); add/sub use the overflow flag of
//! `adds`/`subs`; multiply uses `smulh`/`mul` and requires the high half to
//! equal the low half's sign extension. `GenericEq` — which `speculate.rs`
//! also rewrites `NULL` and `NOT` into — is bit-equality plus a `csel`.
//! `TypeCheck` and the simple-string layout guard read the header type-id
//! byte; `MemoryFence` becomes a `dmb` of the matching kind.
//!
//! `FixnumMin`/`FixnumMax` are a tagged compare and a `csel`. The
//! `FixnumDiv`/`FixnumFloor`/`FixnumRem`/`FixnumMod` family divides the tagged
//! operands with `SDIV` -- which cancels the tags, so the quotient needs
//! retagging while the `MSUB` remainder does not -- and tests the divisor and
//! range-checks the quotient explicitly, because this architecture's divide
//! faults on neither. `FloatCmpEq`/`Lt`/`Le`/`Gt`/`Ge` compare two guarded
//! single-floats and `csel` T or NIL, deopting on an unordered compare so the
//! interpreter decides NaN ordering.
//!
//! Anything else declines: a refused opcode carries `op_tag`, and a shape
//! this emitter cannot encode carries [`A64_DECLINE_TAG`] with its line.
//!
//! # Deopt exits
//!
//! Each guard gets a cold label. The exit evaluates remat recipes in
//! dependency order into the remat buffer, serialises each scope as
//! `(function, bcp, nlocals, nstack)` followed by its slot words, and calls
//! `deopt_t2` with `(nscopes, nwords, buffer, 0)`; no home is written before
//! an instruction's guards pass. The adapter must root the stream before it
//! allocates.

use super::emit::{EmitError, FramedCode, RootSyncSite, op_tag};
// `calls_runtime` is the same list the System Z emitter routes -- both
// emitters read the same `RuntimeCalls` struct and reach the same opcodes
// through it -- so it is defined once, there, beside that struct.
use super::emit_s390x::{calls_runtime, rematerialization_order};
use super::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, Inst, InstData, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use super::mach::{Location, RegClass};
use egcl_rt::asm::{Asm, Cc, a64};
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};
use std::collections::{HashMap, HashSet};

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

/// Arguments a register entry accepts, one per ABI argument register its
/// prologue moves home.
///
/// AAPCS64 passes the first eight in x0-x7, and none of x0-x3 is ever a value
/// home here (the pool is x19-x27), so four would be reachable. Three is the
/// count because `direct_self_call` needs one of them, x3, to hold the stack
/// pointer for its limit compare. Raising this means finding that compare
/// another scratch -- x17 is free -- and is a separate change. System Z
/// carries four for the same reason it can: r2-r5 are volatile there and its
/// self-call has r1 to work in.
const REGISTER_ENTRY_ARGS_MAX: usize = 3;

/// C-ABI runtime adapters used by optimized code, as the System Z emitter defines
/// them. Re-exported rather than redefined so one call site fills both.
pub use super::emit_s390x::RuntimeCalls;


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
    /// Where a register-entry body stages a call's arguments. It owns no
    /// activation, so it cannot stage them there.
    self_args_base: i32,
    /// Frame word holding this code's identity, read by every deopt stub.
    /// Bit 63 marks an activation entered through the register entry, which
    /// owns no `EgclStack` frame, so a deopt must push one (bliss-w6aki).
    entry_kind_offset: i32,
    /// The register entry, when this function qualifies for direct self-calls.
    reg_entry: Option<egcl_rt::asm::Label>,
    runtime: RuntimeCalls,
    activation_slots: u16,
    root_slots: u16,
    argument_slots: u16,
    roots: HashMap<Inst, Vec<Value>>,
    root_sites: Vec<RootSyncSite>,
    result_offset: i32,
    polls: HashSet<Inst>,
    transfer_exit: Option<egcl_rt::asm::Label>,
}

/// High bits of a structural decline code; the low 16 bits carry the line.
pub const A64_DECLINE_TAG: u32 = 0x0A64_0000;

/// A structural decline (a shape this emitter does not handle, as opposed to
/// an opcode it refuses, which carries the opcode tag): names the line that
/// declined, so the T2 log can say where. The runtime decodes it as
/// `[aarch64 emitter declined at emit_a64.rs:LINE]`.
///
/// Every site used to share one `UnsupportedOp(0xA64)`, so a decline named
/// neither the arm nor the opcode, and the only way to learn which shape a
/// function tripped over was to bisect the emitter by hand. The System Z
/// emitter solved this the same way (bliss-nlpd4); this is that fix here.
macro_rules! unsupported {
    () => {
        EmitError::UnsupportedOp(A64_DECLINE_TAG | line!())
    };
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
        let scaled = u64::try_from(offset).map_err(|_| unsupported!())?;
        match a64::ldr_imm(register, base, scaled) {
            Some(word) => self.asm.word(word),
            None => {
                if scaled % 8 != 0 {
                    return Err(unsupported!());
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
        let scaled = u64::try_from(offset).map_err(|_| unsupported!())?;
        match a64::str_imm(register, base, scaled) {
            Some(word) => self.asm.word(word),
            None => {
                if scaled % 8 != 0 {
                    return Err(unsupported!());
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
        let value = u64::try_from(offset).map_err(|_| unsupported!())?;
        let word = a64::add_imm(register, base, value).ok_or(unsupported!())?;
        self.asm.word(word);
        Ok(())
    }

    /// Move the stack pointer by `bytes`, which must be a multiple of 16 so it stays
    /// aligned as AAPCS64 requires at every call. Emitted as add/sub-immediate — the
    /// only form that reads register 31 as SP — in up to two steps so a frame larger
    /// than the 12-bit field still works.
    fn adjust_stack(&mut self, bytes: i32, down: bool) -> Result<(), EmitError> {
        debug_assert_eq!(bytes % 16, 0, "the stack pointer must stay 16-byte aligned");
        let mut remaining = u64::try_from(bytes).map_err(|_| unsupported!())?;
        let mut steps = 0;
        while remaining > 0 {
            steps += 1;
            if steps > 2 {
                return Err(unsupported!());
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
            .ok_or(unsupported!())?;
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
                .word(a64::ldr_imm(register, ADDR, 0).ok_or(unsupported!())?);
        } else {
            match *self.homes.get(&value).ok_or(unsupported!())? {
                Home::Register(source) => self.mov(register, source),
                Home::Stack(slot) => self.load_at(register, a64::SP, slot as i32 * 8)?,
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: a64::Reg) -> Result<(), EmitError> {
        match *self.homes.get(&value).ok_or(unsupported!())? {
            Home::Register(destination) => self.mov(destination, register),
            Home::Stack(slot) => self.store_at(register, a64::SP, slot as i32 * 8)?,
        }
        Ok(())
    }

    // ── frame ────────────────────────────────────────────────────

    fn prologue(&mut self) -> Result<(), EmitError> {
        self.prologue_without_activation()?;
        // The first argument is the frame-slots pointer, and it must survive the
        // whole body, so it moves out of the working registers immediately.
        self.mov(SLOTS, W0);
        Ok(())
    }

    /// The frame, without claiming an activation.
    ///
    /// The register entry shares every word of the ordinary prologue but this
    /// last move: x0 is its first ARGUMENT, and x28 keeps pointing at the
    /// caller's activation, which such a body never touches. Keeping one
    /// frame layout is what lets the single epilogue return from either entry.
    fn prologue_without_activation(&mut self) -> Result<(), EmitError> {
        // One save area holding the frame record and every register the pool may
        // hand out, so the epilogue can restore the stack pointer from x29 alone
        // rather than trusting the body to have left it balanced.
        self.asm
            .word(a64::stp_pre(a64::FP, a64::LR, a64::SP, -SAVE_BYTES).ok_or(unsupported!())?);
        self.asm.word(a64::mov_from_sp(a64::FP));
        for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
            self.asm.word(
                a64::stp(first, second, a64::SP, 16 + index as i32 * 16).ok_or(unsupported!())?,
            );
        }
        let frame = self.frame_bytes;
        self.adjust_stack(frame, true)
    }

    fn epilogue(&mut self) -> Result<(), EmitError> {
        self.asm.word(a64::mov_to_sp(a64::FP));
        for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
            self.asm.word(
                a64::ldp(first, second, a64::SP, 16 + index as i32 * 16).ok_or(unsupported!())?,
            );
        }
        self.asm
            .word(a64::ldp_post(a64::FP, a64::LR, a64::SP, SAVE_BYTES).ok_or(unsupported!())?);
        self.asm.word(a64::ret());
        Ok(())
    }

    // ── guards ───────────────────────────────────────────────────

    fn deopt_label(&mut self, data: &InstData) -> Result<egcl_rt::asm::Label, EmitError> {
        let state = data.frame_state.ok_or(unsupported!())?;
        if self.function.frame_states.get(state).scopes.is_empty() {
            return Err(unsupported!());
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
        self.asm
            .word(a64::cmp_imm(T0, egcl_rt::value::TAG_CONS).expect("1 is an add-immediate"));
        self.asm.jcc(Cc::Ne, deopt);
    }

    /// A LIST guard accepts NIL or a cons that cannot be the interpreter's
    /// `(EGCL::CLOSURE . id)` representation. The fixnum-CDR rejection is
    /// conservative: an ordinary dotted cons may deopt, but optimized code
    /// never mistakes a closure for a Common Lisp list.
    fn guard_list(
        &mut self,
        register: a64::Reg,
        deopt: egcl_rt::asm::Label,
    ) -> Result<(), EmitError> {
        let passed = self.asm.label();
        self.asm
            .word(a64::cmp_imm(register, NIL.0).expect("NIL fits an add-immediate"));
        self.asm.jcc(Cc::E, passed);
        self.guard_cons(register, deopt);
        self.asm
            .word(a64::and_imm(T0, register, !7u64).expect("!7 is a logical immediate"));
        self.load_at(T0, T0, 8)?;
        self.asm
            .word(a64::tst_imm(T0, 7).expect("7 is a logical immediate"));
        self.asm.jcc(Cc::E, deopt);
        self.asm.bind(passed);
        Ok(())
    }

    /// Bounds half of a fixnum guard, mirroring emit.rs's `emit_fixnum_guard`.
    /// W0 holds the tagged operand and has already passed the tag check.
    ///
    /// Tagged fixnums preserve signed integer ordering, so the comparison can
    /// be done on the tagged words directly -- the bounds are shifted into
    /// the tagged domain rather than the operand being untagged. The range is
    /// first intersected with what the tag can represent; an empty
    /// intersection can never hold, so it deopts unconditionally.
    fn guard_fixnum_range(
        &mut self,
        ty: &crate::t2::ir::IRType,
        deopt: egcl_rt::asm::Label,
    ) -> Result<(), EmitError> {
        const FIXNUM_MAX: i64 = (1 << 60) - 1;
        const FIXNUM_MIN: i64 = -(1 << 60);
        let Some(range) = ty.range else {
            return Ok(());
        };
        let lo = range.lo.max(FIXNUM_MIN);
        let hi = range.hi.min(FIXNUM_MAX);
        if lo > hi {
            self.asm.jmp(deopt);
            return Ok(());
        }
        if lo > FIXNUM_MIN {
            self.imm(T0, (lo << 3) as u64);
            self.compare(W0, T0);
            self.asm.jcc(Cc::L, deopt);
        }
        if hi < FIXNUM_MAX {
            self.imm(T0, (hi << 3) as u64);
            self.compare(W0, T0);
            self.asm.jcc(Cc::G, deopt);
        }
        Ok(())
    }

    /// Branch to `target` when W0 equals the tagged constant `bits`.
    fn branch_if_equal_imm(&mut self, bits: u64, target: egcl_rt::asm::Label) {
        self.imm(T0, bits);
        self.compare(W0, T0);
        self.asm.jcc(Cc::E, target);
    }

    /// Set the flags from comparing W0's low three bits against `tag`.
    fn tag_equals(&mut self, tag: u64) {
        self.asm
            .word(a64::and_imm(T0, W0, 7).expect("7 is a logical immediate"));
        self.asm
            .word(a64::cmp_imm(T0, tag).expect("a tag is three bits"));
    }

    /// Branch to `target` when W0's low three bits equal `tag`.
    fn branch_if_tag(&mut self, tag: u64, target: egcl_rt::asm::Label) {
        self.tag_equals(tag);
        self.asm.jcc(Cc::E, target);
    }

    /// Branch to `target` unless W0's low three bits equal `tag`.
    fn branch_unless_tag(&mut self, tag: u64, target: egcl_rt::asm::Label) {
        self.tag_equals(tag);
        self.asm.jcc(Cc::Ne, target);
    }

    /// Load the header type id of the heap object in W0 into T0: clear the tag
    /// bits to reach the header word and read its most significant byte.
    /// `ObjectHeader::type_id` occupies bits 63:56, which is byte 7 on a
    /// little-endian target — byte 0 is where big-endian System Z reads it.
    fn load_type_id(&mut self) {
        self.asm
            .word(a64::and_imm(T0, W0, !7u64).expect("!7 is a logical immediate"));
        self.asm
            .word(a64::ldrb_imm(T0, T0, 7).expect("7 fits the byte-form offset"));
    }

    /// Branch to `target` when the type id loaded by [`Self::load_type_id`] is `id`.
    fn branch_if_type_id(&mut self, id: u8, target: egcl_rt::asm::Label) {
        self.asm
            .word(a64::cmp_imm(T0, u64::from(id)).expect("a type id is one byte"));
        self.asm.jcc(Cc::E, target);
    }

    /// A type predicate on the tagged value in W0, leaving T or NIL in W0.
    /// Mirrors emit.rs's `emit_type_check` exactly, including what it DECLINES:
    /// CONS and LIST (an interpreter closure is a cons that is not of type
    /// LIST, bliss-74rl) and the STRING class (a complex string is a
    /// COMPLEX_ARRAY, so the two simple layouts alone would answer NIL where
    /// the other tiers answer T, bliss-c02n). `bits == STRING` is the layout
    /// guard's form and keeps the simple-layout test. Declining keeps the
    /// tiers bit-identical, which is the bliss-x5y.9 rule.
    fn type_check(&mut self, first: Value, data: &InstData) -> Result<(), EmitError> {
        use egcl_rt::bytecode::typep_class;
        use egcl_rt::object::type_id;
        use egcl_rt::value::{TAG_HEAP_OBJECT, TAG_SYMBOL};
        let refused = || EmitError::UnsupportedOp(op_tag(Opcode::TypeCheck));
        // A constant operand should have been folded to T/NIL upstream.
        if self.constants.contains_key(&first) || self.heap_constants.contains_key(&first) {
            return Err(refused());
        }
        let (bits, class) = match &data.aux {
            AuxData::TypeTag(ty) => (ty.bits, None),
            AuxData::TypepClass(class) => (TypeBits::BOTTOM, Some(*class)),
            _ => return Err(refused()),
        };
        let found = self.asm.label();
        let not_found = self.asm.label();
        let done = self.asm.label();
        let integer = TypeBits::FIXNUM.join(TypeBits::BIGNUM);
        if class == Some(typep_class::BOOLEAN) {
            self.branch_if_equal_imm(NIL.0, found);
            self.branch_if_equal_imm(T.0, found);
        } else if class == Some(typep_class::NULL) {
            self.branch_if_equal_imm(NIL.0, found);
        } else if bits == TypeBits::FIXNUM {
            self.branch_if_tag(0, found);
        } else if bits == TypeBits::SYMBOL || class == Some(typep_class::SYMBOL) {
            self.branch_if_tag(TAG_SYMBOL, found);
            self.branch_if_equal_imm(NIL.0, found); // NIL is a symbol
            self.branch_if_equal_imm(T.0, found); // T is a symbol
        } else if bits == integer {
            self.branch_if_tag(0, found);
            self.branch_unless_tag(TAG_HEAP_OBJECT, not_found);
            self.load_type_id();
            self.branch_if_type_id(type_id::BIGNUM, found);
        } else if bits == TypeBits::STRING {
            self.branch_unless_tag(TAG_HEAP_OBJECT, not_found);
            self.load_type_id();
            self.branch_if_type_id(type_id::SIMPLE_BASE_STRING, found);
            self.branch_if_type_id(type_id::SIMPLE_CHARACTER_STRING, found);
        } else if matches!(
            class,
            Some(typep_class::PACKAGE) | Some(typep_class::HASH_TABLE)
        ) {
            self.branch_unless_tag(TAG_HEAP_OBJECT, not_found);
            self.load_type_id();
            let id = if class == Some(typep_class::PACKAGE) {
                type_id::PACKAGE
            } else {
                type_id::HASH_TABLE
            };
            self.branch_if_type_id(id, found);
        } else {
            return Err(refused());
        }
        self.asm.bind(not_found);
        self.imm(W0, NIL.0);
        self.asm.jmp(done);
        self.asm.bind(found);
        self.imm(W0, T.0);
        self.asm.bind(done);
        Ok(())
    }

    /// The simple-string layout guard on the value in W0 (emit.rs's
    /// `guard_simple_string`): a heap object whose header type id is one of
    /// the two simple string layouts, else deopt. W0 is left as it was, since
    /// the guard's result is its operand under a refined identity.
    fn guard_simple_string(&mut self, deopt: egcl_rt::asm::Label) {
        use egcl_rt::object::type_id;
        self.branch_unless_tag(egcl_rt::value::TAG_HEAP_OBJECT, deopt);
        let layout_ok = self.asm.label();
        self.load_type_id();
        self.branch_if_type_id(type_id::SIMPLE_BASE_STRING, layout_ok);
        self.branch_if_type_id(type_id::SIMPLE_CHARACTER_STRING, layout_ok);
        self.asm.jmp(deopt);
        self.asm.bind(layout_ok);
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

    /// Tagged fixnum `MIN`/`MAX`, leaving the chosen operand in `W0`.
    ///
    /// Both operands carry the same tag, so the tagged comparison orders them
    /// exactly as the untagged one would and the selected word is already the
    /// tagged answer. Branchless, like the fixnum comparisons.
    fn select_fixnum(&mut self, maximum: bool) {
        self.compare(W0, W1);
        self.asm
            .word(a64::csel(W0, W0, W1, if maximum { Cc::Ge } else { Cc::Le }));
    }

    /// Divide the tagged fixnum in `W0` by the one in `W1` under the rule
    /// `opcode` names, leaving the tagged answer in `W0`.
    ///
    /// `SDIV` of the two TAGGED operands cancels the tags, so the quotient
    /// comes out UNTAGGED; `MSUB` then recovers `a_t - q * b_t`, which is the
    /// remainder still scaled by the tag, i.e. already TAGGED. A
    /// remainder-producing opcode therefore needs no retagging and a
    /// quotient-producing one does.
    ///
    /// Two checks the hardware will not make for us, because AArch64's divide
    /// reports nothing. `SDIV` by zero yields zero instead of faulting, so the
    /// divisor is tested first -- a division by zero has to reach the
    /// interpreter to signal. And `SDIV` of `MIN-FIXNUM` by -1 yields 2^60, a
    /// perfectly good i64 that is not a fixnum, so the quotient is
    /// range-checked before it is retagged. On x86-64 `IDIV` traps on both and
    /// the emitter there leans on that; here each is an explicit branch.
    fn divide_fixnums(
        &mut self,
        opcode: Opcode,
        deopt: egcl_rt::asm::Label,
    ) -> Result<(), EmitError> {
        use Opcode::*;
        self.compare_imm(W1, 0);
        self.asm.jcc(Cc::E, deopt);
        self.asm.word(a64::sdiv(T0, W0, W1));
        self.asm.word(a64::msub(T1, T0, W1, W0));
        if matches!(opcode, FixnumMod | FixnumFloor) {
            // FLOOR and MOD differ from TRUNCATE and REM only when the
            // division is inexact AND the remainder's sign disagrees with the
            // divisor's: then the floored quotient is one lower and the
            // floored remainder one divisor higher. The dividend in W0 is dead
            // from here, so it serves as the scratch for the sign test.
            let truncated = self.asm.label();
            self.compare_imm(T1, 0);
            self.asm.jcc(Cc::E, truncated);
            self.asm.word(a64::eor(W0, T1, W1));
            self.compare_imm(W0, 0);
            self.asm.jcc(Cc::Ge, truncated);
            self.asm.word(a64::sub_imm(T0, T0, 1).ok_or(unsupported!())?);
            self.asm.word(a64::add(T1, T1, W1));
            self.asm.bind(truncated);
        }
        if matches!(opcode, FixnumMod | FixnumRem) {
            self.mov(W0, T1);
            return Ok(());
        }
        // Only the positive end can overflow: |quotient| <= |dividend|, and
        // MIN-FIXNUM is itself a fixnum, so a one-sided check is exact.
        self.imm(W0, ((1_i64 << 60) - 1) as u64);
        self.compare(T0, W0);
        self.asm.jcc(Cc::G, deopt);
        self.asm.word(a64::lsl_imm(W0, T0, 3));
        Ok(())
    }

    // ── control flow ─────────────────────────────────────────────

    fn edge(&mut self, edge: &BlockCall) -> Result<(), EmitError> {
        let parameters = self.function.block(edge.block).params.clone();
        if parameters.len() != edge.args.len() {
            return Err(unsupported!());
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
        let roots = self.roots.get(&instruction).ok_or(unsupported!())?.clone();
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
                    let Some(symbol) = data.aux.named_call_target() else {
                        return Err(unsupported!());
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
                        return Err(unsupported!());
                    };
                    if data.opcode == Opcode::SetSymbolValue {
                        self.load(*data.args.first().ok_or(unsupported!())?, W1)?;
                    }
                    self.imm(W0, u64::from(symbol));
                    match data.opcode {
                        Opcode::SymbolValue => self.runtime.load_global,
                        Opcode::SymbolFunction => self.runtime.load_function,
                        _ => self.runtime.store_global,
                    }
                }
                Opcode::EnvironmentValue | Opcode::SetEnvironmentValue => {
                    // `(body, name_index) -> value` and
                    // `(body, name_index, value)`: the same C signatures the
                    // x86 framed and System Z emitters call, so AAPCS64 puts
                    // the body in x0, the index in x1 and the stored value in
                    // x2. The value is staged FIRST, as on System Z: nothing
                    // below writes x2, but loading a heap-constant value goes
                    // through the address scratch, and keeping the order
                    // identical across the two ports keeps the reason for it
                    // in one place.
                    //
                    // Refusing these cost a closure that merely READS a
                    // captured variable its whole T2 body, which is most
                    // closures; `build.rs` emits the opcode unconditionally,
                    // so there was no gate to widen, only this arm to add.
                    let AuxData::EnvironmentName(index) = data.aux else {
                        return Err(unsupported!());
                    };
                    let store = data.opcode == Opcode::SetEnvironmentValue;
                    if self.runtime.environment.body == 0 {
                        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
                    }
                    if store {
                        self.load(*data.args.first().ok_or(unsupported!())?, T0)?;
                    }
                    self.imm(W0, self.runtime.environment.body);
                    self.imm(W1, u64::from(index));
                    if store {
                        self.runtime.environment.store
                    } else {
                        self.runtime.environment.load
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
                        return Err(unsupported!());
                    };
                    if usize::from(nvars) != data.results.len()
                        || slot_base
                            .checked_add(nvars)
                            .is_none_or(|end| end > self.activation_slots)
                    {
                        return Err(unsupported!());
                    }
                    self.load(*data.args.first().ok_or(unsupported!())?, W0)?;
                    self.address(W1, SLOTS, i32::from(slot_base) * 8)?;
                    self.imm(T0, u64::from(nvars));
                    self.runtime.multiple_values
                }
                _ => return Err(unsupported!()),
            }
        } else {
            self.runtime.poll
        };
        if helper == 0 {
            return Err(unsupported!());
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
            code_offset: u32::try_from(offset).map_err(|_| unsupported!())?,
            live_roots: roots.len() as u16,
            register_roots: register_roots as u16,
            spill_roots: (roots.len() - register_roots) as u16,
        });
        Ok(())
    }

    /// A call to this very function, entered through the register entry.
    ///
    /// This is the whole point of the register entry: a self-call moves its
    /// arguments into x0-x2 and branches to a label in this same buffer,
    /// skipping the generic dispatch that otherwise costs far more than the
    /// body it surrounds.
    ///
    /// The published native stack limit is compared first. Once the stack
    /// runs low the ordinary `call_slice` path is taken instead, whose
    /// adapter enforces the native depth cap and raises a catchable
    /// STORAGE-CONDITION -- so recursion that outruns the stack still
    /// signals rather than faulting. A zero limit (never published) compares
    /// false, leaving the guard inert.
    fn direct_self_call(&mut self, data: &InstData) -> Result<(), EmitError> {
        let reg_entry = self.reg_entry.ok_or(unsupported!())?;
        let AuxData::CallTarget(symbol) = data.aux else {
            return Err(unsupported!());
        };
        if data.args.len() > REGISTER_ENTRY_ARGS_MAX {
            return Err(unsupported!());
        }
        let slow = self.asm.label();
        let join = self.asm.label();
        // A self-call with the WRONG argument count must reach the generic
        // adapter, which raises PROGRAM-ERROR: the register entry would
        // instead bind whatever happens to be in x0-x2 (bliss-w6aki). Only
        // the slow path is emitted for it.
        let arity_matches =
            data.args.len() == self.function.block(self.function.entry()).params.len();
        if arity_matches {
            self.imm(ADDR, egcl_rt::stack::native_stack_limit_addr());
            self.load_at(ADDR, ADDR, 0)?;
            // Stack addresses are canonical and positive, so a signed compare
            // is right. At or below the limit takes the slow path.
            // T1 is x3, past the three argument registers this entry uses,
            // so it is free here -- the arguments are loaded below.
            self.asm.word(a64::mov_from_sp(T1));
            self.compare(T1, ADDR);
            self.asm.jcc(Cc::Le, slow);
            // Fast path: the arguments in x0-x2. Those are the emitter's own
            // working registers, never an allocated home, so loading them in
            // order cannot clobber a later argument's source.
            for (index, &argument) in data.args.iter().enumerate() {
                self.load(argument, index as a64::Reg)?;
            }
            self.asm.call(reg_entry);
            self.asm.jmp(join);
        }
        // Slow path: generic dispatch through the slice adapter, arguments
        // staged in this frame. Eligibility proved no live tagged heap value
        // crosses a call here, so nothing needs a shadow root.
        self.asm.bind(slow);
        let self_args_base = self.self_args_base;
        for (index, &argument) in data.args.iter().enumerate() {
            self.load(argument, W0)?;
            self.store_at(W0, a64::SP, self_args_base + index as i32 * 8)?;
        }
        self.imm(W0, u64::from(symbol));
        self.imm(W1, data.args.len() as u64);
        self.address(T0, a64::SP, self_args_base)?;
        self.imm(T1, 0);
        let offset = self.asm.here();
        self.imm(ADDR, self.runtime.call_slice);
        self.call_register(ADDR);
        self.asm.bind(join);
        // Either path can leave a pending transfer -- a deopt, or an error
        // raised inside the callee -- so propagate it exactly as
        // `runtime_call` does.
        let result_offset = self.result_offset;
        self.store_at(W0, a64::SP, result_offset)?;
        if self.runtime.transfer_pending != 0 {
            self.imm(ADDR, self.runtime.transfer_pending);
            self.call_register(ADDR);
            self.compare_imm(W0, 0);
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.jcc(Cc::Ne, exit);
        }
        if let Some(&result) = data.results.first() {
            self.load_at(W0, a64::SP, result_offset)?;
            self.store(result, W0)?;
        }
        // The slow path is a runtime call like any other, and the collector
        // must find the same (empty) root set there.
        self.root_sites.push(RootSyncSite {
            code_offset: u32::try_from(offset).map_err(|_| unsupported!())?,
            live_roots: 0,
            register_roots: 0,
            spill_roots: 0,
        });
        Ok(())
    }

    fn instruction(&mut self, instruction: Inst, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
        if self.polls.contains(&instruction) {
            // Every selected backward-edge terminator reaches the existing
            // root-synchronizing poll before its edge moves.
            self.runtime_call(instruction, None)?;
        }
        // A call to this very function skips generic dispatch and branches to
        // the register entry, when this body has one.
        if data.opcode == Opcode::Call
            && self.reg_entry.is_some()
            && super::register_entry::is_self_call(data, self.runtime.self_sym)
        {
            return self.direct_self_call(data);
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
        if data.opcode == Guard
            && let AuxData::BuiltinBinding { address, expected } = data.aux
        {
            let deopt = self.deopt_label(data)?;
            self.imm(W0, address as u64);
            self.load_at(W0, W0, 0)?;
            self.compare_imm(W0, expected);
            self.asm.jcc(Cc::Ne, deopt);
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
            MemoryFence => {
                // AArch64 is weakly ordered, so unlike x86-64 — where only a
                // store-load barrier costs anything — each kind needs a real
                // instruction. Order the kinds precisely rather than taking
                // the System Z route of a full barrier for all three.
                let barrier = match data.aux {
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Read) => {
                        a64::Barrier::Load
                    }
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Write) => {
                        a64::Barrier::Store
                    }
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Full) => {
                        a64::Barrier::Full
                    }
                    _ => return Err(EmitError::UnsupportedOp(op_tag(Opcode::MemoryFence))),
                };
                self.asm.word(a64::dmb(barrier));
                if let Some(&value) = data.results.first() {
                    self.imm(W0, NIL.0);
                    self.store(value, W0)?;
                }
                return Ok(());
            }
            Trap => {
                // A Trap ends a path that must not continue (bliss-wukf): the
                // code after a call that never returns normally. As in emit.rs
                // and emit_s390x.rs the GENERIC deopt suffices even though it
                // re-runs the function — this is reachable only once a c2i
                // helper has stashed an error, and run_native takes
                // NATIVE_ERROR before it looks at NATIVE_DEOPT, so no
                // committed side effect is repeated and no FrameState is
                // needed. Refusing it cost the whole function its T2 code.
                if self.runtime.deopt == 0 {
                    return Err(unsupported!());
                }
                self.imm(ADDR, self.runtime.deopt);
                self.call_register(ADDR);
                self.imm(W0, NIL.0);
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
        // Past this point every remaining opcode is a value-producing one
        // with at least one argument. Report the opcode, not the line: an
        // operation reaching here with no result or no argument is refused
        // *as that operation*, which is what a reader needs to see.
        let result = *data
            .results
            .first()
            .ok_or(EmitError::UnsupportedOp(op_tag(data.opcode)))?;
        let first = *data
            .args
            .first()
            .ok_or(EmitError::UnsupportedOp(op_tag(data.opcode)))?;
        self.load(first, W0)?;
        match data.opcode {
            Guard if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM || ty.bits == TypeBits::SINGLE_FLOAT || ty.bits == TypeBits::CONS) || matches!(&data.aux, AuxData::EndpList) =>
            {
                // CONS matters as much as the numeric tags: build.rs emits a
                // separate Guard(TypeTag(CONS)) ahead of every Car/Cdr, so
                // refusing it cost the whole function its T2 code for any use of
                // CAR or CDR -- which is most Lisp code (bliss-2yews).
                let deopt = self.deopt_label(data)?;
                match &data.aux {
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM => {
                        self.guard_fixnum(W0, deopt);
                        // The tag alone is not the whole guard. A declared
                        // `(integer lo hi)` reaches here as a FIXNUM TypeTag
                        // carrying a RANGE, and dropping it does not merely
                        // lose an optimisation -- it loses a check the program
                        // can observe. `CheckResult` on a declared FTYPE
                        // lowers to exactly this guard, so without the bounds
                        // a function declared `(integer 0 100000)` quietly
                        // returned 100001 where every other tier signals a
                        // TYPE-ERROR.
                        self.guard_fixnum_range(ty, deopt)?;
                    }
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::CONS => self.guard_cons(W0, deopt),
                    AuxData::EndpList => {
                        self.guard_list(W0, deopt)?
                    }
                    _ => self.guard_single_float(W0, deopt),
                }
            }
            FloatAdd | FloatSub | FloatMul => {
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, W0, deopt)?;
                self.float_operand(*data.args.get(1).ok_or(unsupported!())?, W1, deopt)?;
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
                    self.load(*data.args.get(1).ok_or(unsupported!())?, W1)?;
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
            TypeCheck => {
                self.type_check(first, data)?;
                return self.store(result, W0);
            }
            FixnumMin | FixnumMax | FixnumDiv | FixnumFloor | FixnumRem | FixnumMod => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(W0, deopt);
                self.load(*data.args.get(1).ok_or(unsupported!())?, W1)?;
                self.guard_fixnum(W1, deopt);
                match data.opcode {
                    FixnumMin => self.select_fixnum(false),
                    FixnumMax => self.select_fixnum(true),
                    other => self.divide_fixnums(other, deopt)?,
                }
            }
            FloatCmpEq | FloatCmpLt | FloatCmpLe | FloatCmpGt | FloatCmpGe => {
                // A speculated single-float comparison yielding T or NIL, as
                // emit.rs does it. Each variable operand is guarded to be a
                // single-float exactly as `FloatAdd` guards its own, and an
                // UNORDERED compare -- either operand a NaN -- deopts: the
                // interpreter's NaN ordering is not IEEE's, so native code
                // must not be the one to decide it.
                //
                // Taking that exit FIRST is also what makes the ordinary
                // signed conditions usable below. An unordered `FCMP` sets C
                // and V with N and Z clear, so `LT` (N != V) and `LE` would
                // both read TRUE for a NaN -- the classic AArch64 float-compare
                // trap. `VS` is consulted before either, so by the `csel` the
                // comparison is known ordered and the codes mean what they say.
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, W0, deopt)?;
                self.float_operand(*data.args.get(1).ok_or(unsupported!())?, W1, deopt)?;
                self.unbox_single(F0, W0);
                self.unbox_single(F1, W1);
                self.asm.word(a64::fcmp_single(F0, F1));
                self.asm.jcc(Cc::O, deopt);
                self.imm(T0, NIL.0);
                self.imm(T1, T.0);
                let cc = match data.opcode {
                    FloatCmpEq => Cc::E,
                    FloatCmpLt => Cc::L,
                    FloatCmpLe => Cc::Le,
                    FloatCmpGt => Cc::G,
                    _ => Cc::Ge,
                };
                self.asm.word(a64::csel(W0, T1, T0, cc));
            }
            Guard if matches!(&data.aux, AuxData::StringLayout) => {
                // The layout refinement every simple-string fast path is
                // predicated on. The guard passes its operand through.
                let deopt = self.deopt_label(data)?;
                self.guard_simple_string(deopt);
            }
            StringByteLength => {
                // A layout-proven simple string keeps its byte length in the
                // second header word, so this is one load once that guard has
                // run; shifting by three tags the raw count as a fixnum.
                // Refuse the guarded or effectful form exactly as emit.rs
                // does: this is the fast path for a layout a dominating guard
                // has already proved, and anything else needs the general one.
                if data.args.len() != 1
                    || data.flags.guard
                    || data.flags.effectful
                    || data.frame_state.is_some()
                {
                    return Err(EmitError::UnsupportedOp(op_tag(Opcode::StringByteLength)));
                }
                self.asm
                    .word(a64::and_imm(W0, W0, !7u64).expect("!7 is a logical immediate"));
                self.load_at(W0, W0, 8)?;
                self.asm.word(a64::lsl_imm(W0, W0, 3));
            }
            GenericEq => {
                // Lisp EQ (and NULL / NOT, which speculate.rs rewrites to
                // GenericEq(x, NIL)) is bit-equality of the two tagged words:
                // no guard, any tagged operands. build.rs also puts one in
                // front of every CAR/CDR whose operand may be NIL, so refusing
                // it here cost almost every list-walking function its native
                // code on this target (bliss-jmwqr).
                self.load(*data.args.get(1).ok_or(unsupported!())?, W1)?;
                self.compare(W0, W1);
                self.imm(T0, NIL.0);
                self.imm(T1, T.0);
                self.asm.word(a64::csel(W0, T1, T0, Cc::E));
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
                    return Err(unsupported!());
                }
                self.asm
                    .word(a64::and_imm(W0, W0, !7u64).ok_or(unsupported!())?);
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
                self.load(*data.args.get(1).ok_or(unsupported!())?, W1)?;
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
                    .word(a64::and_imm(W0, W0, !7u64).ok_or(unsupported!())?);
            }
            FixnumShl | FixnumShr => {
                // (ash x n) for a CONSTANT n; a variable amount declines. The
                // constants map holds TAGGED bits, so the shift count is
                // recovered by untagging it.
                let deopt = self.deopt_label(data)?;
                // Report the OPCODE, not this line: a variable shift amount is
                // a shape emit.rs refuses as the opcode too, and the T2 log's
                // "refused Opcode #N in t2/ir.rs" is what makes the two
                // backends' declines greppable the same way (bliss-ieajy.1).
                let refused = || EmitError::UnsupportedOp(op_tag(data.opcode));
                let amount = *data.args.get(1).ok_or_else(refused)?;
                let tagged = *self.constants.get(&amount).ok_or_else(refused)?;
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
                        return Err(unsupported!());
                    }
                    self.asm.word(a64::lsl_imm(T0, W0, k));
                    self.asm.word(a64::asr_imm(T1, T0, k));
                    self.asm.word(a64::cmp(T1, W0));
                    self.asm.jcc(Cc::Ne, deopt);
                    self.asm.word(a64::mov(W0, T0));
                }
            }
            // Name the opcode rather than the line: this arm refuses a whole
            // IR operation, and which one it was is the actionable fact.
            _ => return Err(EmitError::UnsupportedOp(op_tag(data.opcode))),
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
            _ => Err(unsupported!()),
        }
    }

    /// The cold path behind a failed guard: rebuild the interpreter's view of this
    /// activation into frame slots, then hand it to the deopt callback.
    fn deopt(&mut self, state: &FrameState, callback: u64) -> Result<(), EmitError> {
        for index in rematerialization_order(state)? {
            let recipe = &state.remat[index];
            if recipe.result_repr != ValueRepresentation::Tagged {
                return Err(unsupported!());
            }
            match recipe.op {
                RematOp::Const
                | RematOp::BoxFixnum
                | RematOp::UnboxFixnum
                | RematOp::BoxFloat
                | RematOp::UnboxFloat => {
                    if recipe.inputs.len() != 1 {
                        return Err(unsupported!());
                    }
                    let input = recipe.inputs[0].clone();
                    self.deopt_source(&input, state)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(unsupported!());
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
        // The T0 resume adapter's ABI: scope count, total words, the buffer,
        // and the entry-kind word. That last was a reserved zero until the
        // register entry existed; it now carries this code's identity, with
        // bit 63 set when the activation came in through the register entry
        // and so owns no EgclStack frame for the resume to land in
        // (bliss-w6aki). The frame word is written by whichever prologue ran.
        self.imm(W0, scopes.len() as u64);
        self.imm(W1, words as u64);
        let deopt_base = self.deopt_base;
        self.address(T0, a64::SP, deopt_base)?;
        let entry_kind = self.entry_kind_offset;
        self.load_at(T1, a64::SP, entry_kind)?;
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
/// activation slots at runtime calls. Every cycle crosses a GC/signal poll
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
            .ok_or(unsupported!())?;
        if function
            .inst(terminator)
            .targets
            .iter()
            .any(|target| positions.get(&target.block).is_none_or(|&t| t <= index))
        {
            if runtime.poll == 0 {
                return Err(unsupported!());
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
        self_args_base: 0,
        entry_kind_offset: 0,
        reg_entry: None,
        runtime,
        activation_slots,
        root_slots: 0,
        argument_slots: 0,
        roots: HashMap::new(),
        root_sites: Vec::new(),
        result_offset: 0,
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
            return Err(unsupported!());
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

    // A value whose inferred type is confined to the immediate tags is never a
    // heap pointer, so the collector can neither move it nor need to see it,
    // and it needs no shadow root. The inference map assigns each SSA value
    // the type it has from its definition on, so the fact holds wherever the
    // value is live; BOTTOM (unreached) stays rooted. This mirrors emit.rs and
    // emit_s390x.rs: it removes the shadow-slot store and reload around every
    // runtime call in numeric code, and it is what lets a fixnum-recursive
    // function's roots vanish entirely -- the precondition for a register
    // entry and a direct self-call here, as it was on System Z.
    let inference = super::infer::infer(function);
    let proven_immediate =
        |value: Value| super::register_entry::proven_immediate(&inference, value);

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
        let after = u32::try_from(index).map_err(|_| unsupported!())? * 2 + 1;
        let data = function.inst(source);
        // A value this instruction DEFINES does not exist until the callback
        // returns, so it is not a root at the call -- the comment above has
        // always said so, but the filter was missing, and regalloc2 reports a
        // result's range as live from this very point. Rooting it cost two
        // shadow slots on every self-recursive function, which is exactly the
        // `root_slots == 0` that a register entry needs, so no such function
        // could ever get one. System Z excludes defs here for the same reason.
        let defines = |value: &Value| {
            inst.defs
                .iter()
                .any(|def| def.class == RegClass::Gpr && def.num == value.0)
        };
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
            .filter(|value| !defines(value))
            .filter(|value| emitter.homes.contains_key(value))
            .filter(|value| !proven_immediate(*value))
            .collect();
        roots.sort_by_key(|value| value.0);
        roots.dedup();
        emitter.roots.insert(source, roots);
    }
    // A poll runs before edge copies. Anonymous lowered moves define the
    // destination phi homes before the machine terminator, so the ordinary
    // call's Late point does not describe the state published by this emitter.
    for (block_index, &block) in function.block_order().iter().enumerate() {
        let Some(&source) = function.block(block).insts.last() else {
            continue;
        };
        if !emitter.polls.contains(&source) {
            continue;
        }
        let mb = &machine.blocks[block_index];
        let mut boundary = mb.end;
        while boundary > mb.start
            && machine.insts[boundary - 1]
                .source_inst
                .is_none_or(|inst| inst == source)
        {
            boundary -= 1;
        }
        let point = u32::try_from(boundary).map_err(|_| unsupported!())? * 2;
        let data = function.inst(source);
        let mut roots: Vec<_> = machine
            .value_locations
            .iter()
            .filter(|range| {
                range.vreg.class == RegClass::Gpr && range.start <= point && point < range.end
            })
            .map(|range| Value(range.vreg.num))
            .chain(data.args.iter().copied())
            .chain(
                data.targets
                    .iter()
                    .flat_map(|target| target.args.iter().copied()),
            )
            .filter(|value| emitter.homes.contains_key(value))
            .filter(|value| !proven_immediate(*value))
            .collect();
        roots.sort_by_key(|value| value.0);
        roots.dedup();
        emitter.roots.insert(source, roots);
    }
    emitter.root_slots = u16::try_from(emitter.roots.values().map(Vec::len).max().unwrap_or(0))
        .map_err(|_| unsupported!())?;
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
    .map_err(|_| unsupported!())?;
    // Who may carry a register entry is shared IR analysis (register_entry.rs),
    // so this port supplies only the machine half: a second prologue whose
    // arguments arrive in x0-x2, and the direct self-call that branches to it.
    let entry_params = &function.block(function.entry()).params;
    let plan = super::register_entry::plan(&super::register_entry::RegisterEntryQuery {
        function,
        entry_params,
        self_sym: emitter.runtime.self_sym,
        call_slice_rooted: emitter.runtime.call_slice_rooted,
        root_slots: emitter.root_slots,
        args_max: REGISTER_ENTRY_ARGS_MAX,
        call_args_max: super::emit_s390x::CALL_ARGS_MAX,
        // This emitter has no in-frame staging yet, so an eligible body
        // here is one whose only calls are to itself.
        stages_other_calls_in_frame: false,
    });
    let has_reg_entry = plan.eligible;
    if has_reg_entry {
        // Such a body stages every call in its own frame instead, so it needs
        // no argument shadow slots in an activation it does not own.
        emitter.argument_slots = 0;
    }
    let self_args_words = if has_reg_entry {
        REGISTER_ENTRY_ARGS_MAX.max(plan.other_call_words)
    } else {
        0
    };
    let shadow_root_slots = emitter
        .root_slots
        .checked_add(emitter.argument_slots)
        .ok_or(unsupported!())?;
    activation_slots
        .checked_add(shadow_root_slots)
        .ok_or(unsupported!())?;

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
    // One word holds the call result and one the entry kind. Round to a pair
    // so the stack pointer stays 16-byte aligned, which AAPCS64 requires at a call.
    let frame_words = (spill_slots as usize
        + edge_words
        + deopt_words
        + remat_words
        + self_args_words
        + 1
        + 1
        + 1)
        & !1;
    // Every frame access must remain reachable; beyond this the emitter would be
    // materialising an address for each one.
    if frame_words > 0x1000 {
        return Err(unsupported!());
    }
    emitter.frame_bytes = (frame_words * 8) as i32;
    emitter.edge_base = spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    emitter.remat_base = emitter.deopt_base + deopt_words as i32 * 8;
    emitter.self_args_base = emitter.remat_base + remat_words as i32 * 8;
    emitter.entry_kind_offset = (frame_words as i32 - 2) * 8;
    emitter.result_offset = (frame_words as i32 - 1) * 8;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported!());
    }

    emitter.prologue()?;
    // This activation owns its EgclStack frame, so the entry kind is the bare
    // identity. Written after the prologue, which has just claimed the frame.
    let code_id = emitter.runtime.code_id;
    let entry_kind = emitter.entry_kind_offset;
    emitter.imm(W0, code_id);
    emitter.store_at(W0, a64::SP, entry_kind)?;
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.load_at(W0, SLOTS, index as i32 * 8)?;
        emitter.store(parameter, W0)?;
    }
    emitter.asm.jmp(emitter.blocks[&function.entry()]);

    // The register entry's offset, published as FramedCode::compiled_entry so
    // the install log and its tests can see it (0 = no register entry).
    let mut compiled_entry = 0;
    if has_reg_entry {
        // Register entry: the same frame, but the arguments arrive in x0-x2
        // and x28 stays the CALLER's activation pointer -- this body never
        // touches it. Move the arguments home before the entry-kind write,
        // which needs a working register.
        let label = emitter.asm.label();
        emitter.asm.bind(label);
        emitter.reg_entry = Some(label);
        compiled_entry = emitter.asm.here();
        emitter.prologue_without_activation()?;
        for (index, &parameter) in entry_params.iter().enumerate() {
            emitter.store(parameter, index as a64::Reg)?;
        }
        // No frame of its own: a deopt taken here must push one.
        emitter.imm(W0, code_id | (1 << 63));
        emitter.store_at(W0, a64::SP, entry_kind)?;
        emitter.asm.jmp(emitter.blocks[&function.entry()]);
    }

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
        // Imported values this entry must type-check are only tested by the
        // x86-64 framed emitter; without the test the loop's proofs would not
        // hold for an interpreter value, so decline the entry (bliss-5yz5h).
        if !osr.checks.is_empty() {
            continue;
        }
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
        frame_entry: Some(0),
        ordinary_entry: None,
        ordinary_bound_entry: None,
        ordinary_checks_params: false,
        ordinary_stack_required: None,
        native_recursive_entry: None,
        native_entry_kind_slot: None,
        native_control_slot: None,
        native_argument_base: None,
        native_argument_count: 0,
        native_calls: None,
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        native_poll_transfer_sites: Vec::new(),
        #[cfg(all(target_arch = "x86_64", windows))]
        windows_unwind: Vec::new(),
        code: emitter.asm.finish().ok_or(EmitError::BadBranch)?,
        compiled_entry,
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
