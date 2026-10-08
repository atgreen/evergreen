// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! System Z (s390x) machine-code emission for the T2 framed pipeline.
//!
//! # Purpose
//!
//! Turns an optimised SSA [`Function`] into executable z/Architecture code
//! using the same activation contract as the x86 framed emitter: the body
//! runs over an `EgclStack` activation of `activation_slots` tagged words,
//! extended by the shadow-root slots this emitter needs at runtime calls. The
//! module owns instruction selection for the opcode subset listed below, the
//! native frame layout, GC-root synchronisation around every runtime call,
//! sampled back-edge safepoint polls, OSR entry prologues, and the
//! guard-failure exits that serialise a `FrameState` for the T0 resume
//! adapter. It does not own the native transfer protocol: on this target a
//! runtime call still ends with the checked `transfer_pending` test, and the
//! s390x native-segment entry admits only call-free, scope-free bodies.
//!
//! The AArch64 and ppc64le emitters are structured identically and share
//! [`RuntimeCalls`] and `rematerialization_order` from this file.
//!
//! # Design
//!
//! Register allocation runs (`allocate_framed_s390x`), but emission is
//! home-based rather than allocation-driven. A value whose every reported
//! `value_locations` range names the same location keeps it as its *home*
//! (a callee-saved register r6–r12 or a frame slot); a value with split ranges
//! gets a fresh stable frame slot. Each instruction loads its operands into
//! fixed working registers, operates, and stores the result to its home. That
//! costs moves an allocation-driven emitter would avoid, but keeps selection
//! small and confines the architecture to this file.
//!
//! | role                                          | register |
//! |-----------------------------------------------|----------|
//! | primary working / first argument / result     | r2       |
//! | secondary working / second argument           | r3       |
//! | temporaries / third and fourth arguments      | r4, r5   |
//! | address scratch (helper targets)              | r1       |
//! | activation (frame-slots) pointer, whole body  | r13      |
//! | stack pointer                                 | r15      |
//!
//! `PhysReg.encoding` is the architectural register number on this target.
//!
//! # Frame
//!
//! The prologue is `STMG r6,r15` into the caller's save area, the 160-byte
//! linkage area, then `frame_bytes` more. Above r15+160, in order: spill slots
//! (regalloc2's plus the stable homes added for split values), the edge
//! parallel-copy buffer (max block-param count), the deopt serialisation buffer
//! (max over frame states of four header words plus locals plus stack per
//! scope), the remat buffer (max recipe count), and six words holding the saved
//! call result, the poll countdown, the `EgclStack` pointer the entry
//! received in r3, the caller's frame pointer and stack offset parked
//! across a direct native call, and the code id stamped by the entry (bit 63
//! set by the register entry, which owns no frame). Every access must fit a signed 20-bit
//! displacement, so an oversized frame declines.
//!
//! The entry prologue copies the entry block's parameters from activation
//! slots `0..n` into their homes. OSR entries are additional prologues, one per
//! `osr_entries` record whose frame state is single-scope, stack-empty and
//! all-tagged and whose every live home at the loop header can be imported from
//! a slot; they are published as `(bcp, offset)` pairs in [`FramedCode`].
//!
//! # Runtime calls and GC roots
//!
//! `Call`, `SymbolValue`, `SymbolFunction`, `SetSymbolValue`, `ClearMv` and
//! `TakeValuesToLocals` go through the C-ABI adapters in [`RuntimeCalls`]. A
//! `Call` whose callee resolves to a leaf builtin at compile time uses the
//! direct slice adapter from the runtime's direct-builtin hooks instead, with
//! the table slot and invalidation generation baked in (bliss-flzmi). The
//! collector moves objects and cannot see a home register or native spill
//! slot, so `runtime_call` clears every shadow slot to NIL, writes each live
//! tagged value into the shadow area that follows `activation_slots` (roots
//! first, then call arguments), makes the call, and reloads each root from its
//! shadow before assigning results, so a dying argument that shares a home
//! with the result cannot overwrite it. The live set per call is derived from
//! the same regalloc2 ranges that chose homes: ranges covering the
//! instruction's Late program point, plus its uses, minus its defs. Each site
//! is recorded as a [`RootSyncSite`] and installation checks that the count
//! matches `emitted_safepoints`.
//!
//! When `RuntimeCalls::transfer_pending` is nonzero it is called after every
//! runtime call and a nonzero result takes the shared exit (NIL, epilogue).
//! That is the checked legacy ABI; native transfer dispatch is x86-only.
//!
//! # Direct self-calls
//!
//! A function whose live values across every runtime call are proven
//! immediates (so no shadow roots), whose self-calls pass at most three
//! arguments and whose other calls at most [`CALL_ARGS_MAX`], and which needs
//! no activation-backed instruction, also gets a REGISTER entry after the
//! ordinary one: the same prologue, but the arguments arrive in r2-r4 instead
//! of activation slots and r13 is left as the caller's. Such a body never
//! touches r13: a non-self call stages its arguments in this frame's
//! call-argument area and goes through `call_slice_rooted`, which copies them
//! into rooted storage before the callee can allocate, or enters a resolved
//! native callee directly, which copies them into the callee's published
//! frame first; the direct-builtin shortcut is not taken there, since a
//! builtin reads the area while it may allocate (bliss-of8kz). A self-call then moves its arguments into r2-r4 and `BRASL`s to
//! that entry, skipping c2i dispatch entirely (bliss-hx5xb; measured 0.78 us
//! per generic call on a z17 against 77 ns per loop iteration). The site first
//! compares r15 with the published native stack limit and takes the ordinary
//! `call_slice` path when the stack runs low, so the depth cap still turns
//! runaway recursion into a STORAGE-CONDITION instead of a crash. Both paths
//! share the transfer check. `EGCL_NO_DIRECT_SELF_CALL` disables the fast path.
//!
//! Back-edge polls: a block with a successor at or before itself in block
//! order polls before its edge moves, decrementing a 256-count frame word and
//! calling `poll` through the same root-synchronising path when it reaches
//! zero. A function with a cycle but no `poll` adapter declines.
//!
//! # Supported opcodes
//!
//! Constants (`Const*` are folded; heap literals are loaded through their
//! rooted constant-pool slot, never baked in), `Return`, `Jump`, two-way
//! `Brif`, `Trap` (an unconditional deopt), `MemoryFence` (BCR 14,0 for
//! every kind), `Guard` for the FIXNUM,
//! SINGLE_FLOAT and CONS tags and for the simple-string layout, with the
//! `StringByteLength` and ASCII `StringAsciiCharAt` loads that layout proof
//! refines (the string's byte length is its second word and its bytes follow
//! at offset 16 on every target; only the header's type-id byte moves with
//! the byte order), `FloatAdd/Sub/Mul`
//! on tagged single-floats, and `FixnumAdd/Sub/Mul/Neg` plus the five fixnum
//! comparisons, all tag-guarded and deopting on overflow. Multiply uses the
//! unsigned 128-bit `MLGR` product with a sign correction and requires the
//! high half to equal the low half's sign extension. Unguarded, effect-free
//! `Car`/`Cdr` load through the masked headerless cons pointer; `LogAnd`,
//! `LogOr`, `LogXor` operate directly on tagged bits; `LogNot` re-clears the
//! tag; and `FixnumShl`/`FixnumShr` by a constant amount shift the tagged
//! value, where a negative `Shl` constant is a right shift and a left shift
//! deopts if the value does not survive the round trip (bliss-2yews). `GenericEq`
//! (EQ, and NULL/NOT via EQ against NIL) is a tagged-word compare selecting
//! T/NIL (bliss-pig52). `TypeCheck` answers the fixnum, symbol, integer,
//! simple-string, package, hash-table, NULL and BOOLEAN predicates inline
//! from the tag and the header's type-id byte, declining exactly the cases
//! emit.rs declines (bliss-nlpd4). Everything
//! else, including `StringByteLength`, variable shift amounts and any unboxed
//! representation, returns `UnsupportedOp(0x390)` and the function stays at its
//! current tier, naming the opcode in the T2 log; structural declines (an
//! unsupported frame-state shape, a cycle with no poll adapter, an unboxed
//! representation) report the emitter's own `0x390`.
//!
//! # Deopt exits
//!
//! Each guard gets a cold label. The exit evaluates the frame state's remat
//! recipes in dependency order into the remat buffer, serialises each scope as
//! `(function, bcp, nlocals, nstack)` followed by its slot words into the
//! deopt buffer, and calls `deopt_t2` with `(nscopes, nwords, buffer, 0)`. No
//! home is written before an instruction's guards pass, so the exit always
//! observes pre-instruction state. The adapter must root the stream before it
//! allocates.

use super::emit::{EmitError, FramedCode, RootSyncSite};
use super::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, Inst, InstData, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use super::mach::{Location, RegClass};
use egcl_rt::asm_s390x::{Asm, Label};
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};
use std::collections::{HashMap, HashSet};

/// C-ABI runtime adapters used by optimized code. `transfer_pending` is a
/// nonallocating leaf; the primary result is temporarily unrooted during it.
#[derive(Clone, Default)]
pub struct RuntimeCalls {
    /// The generic deopt (`c2i_deopt`): flags a deoptimization with no resume
    /// point, so `run_native` re-runs the function at T0 from the top. Only
    /// `Trap` uses it (see there); zero declines `Trap`.
    pub deopt: u64,
    pub call_slice: u64,
    /// `call_slice` with the arguments copied into rooted storage before the
    /// callee runs: what a register-entry body uses for its non-self calls,
    /// whose arguments are staged in the native frame where the collector
    /// cannot see them (bliss-of8kz). Zero withholds the register entry from
    /// any body that makes such a call.
    pub call_slice_rooted: u64,
    pub load_global: u64,
    pub load_function: u64,
    pub store_global: u64,
    pub multiple_values: u64,
    pub transfer_pending: u64,
    /// Safepoint callback: nonzero requests immediate native exit.
    pub poll: u64,
    /// The symbol being compiled, when known: a call to it from inside its own
    /// body can become a direct self-call (see the module docs).
    pub self_sym: Option<u32>,
    /// Native callees resolved by the runtime for this function's call sites;
    /// a `Call` to one of them with its arity is entered directly
    /// (bliss-6j6pk, see the module docs).
    pub direct_natives: Vec<super::emit::DirectNativeTarget>,
    /// Stamped into a frame word by each entry and passed by every deopt
    /// stub as the adapter's fourth argument (bit 63 set by the register
    /// entry), so the runtime can resolve the code the stub belongs to even
    /// when it was entered by a direct native call, and knows whether the
    /// activation owns the frame at fp; 0 means "the active code".
    pub code_id: u64,
}

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
    blocks: HashMap<Block, Label>,
    deopts: Vec<(FrameStateId, Label)>,
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
    /// Frame word holding the `EgclStack` pointer the entry received in r3.
    stack_offset: i32,
    /// Frame words parking the caller's fp and sp_offset across a direct
    /// native call.
    saved_fp_offset: i32,
    saved_sp_offset: i32,
    /// Frame word holding the code id, with bit 63 set when this activation
    /// came in through the register entry and so has no EgclStack frame of
    /// its own; the deopt stubs pass it to the runtime (bliss-w6aki).
    entry_kind_offset: i32,
    polls: HashSet<Inst>,
    transfer_exit: Option<Label>,
    /// The register entry, when this function qualifies for direct self-calls.
    reg_entry: Option<Label>,
    /// The lowest callee-saved register the prologue saves: the lowest home
    /// register in use, or r13 when none is below it. Homes are handed out
    /// from r12 downward so this range is as short as the function allows.
    first_saved: u8,
    /// Frame offset of the call-argument area of a register-entry body: a
    /// self-call's slow path stages its arguments there for `call_slice`, and
    /// every other call stages its arguments there too (bliss-of8kz).
    self_args_base: i32,
}

/// High bits of a structural decline code; the low 16 bits carry the line.
pub const S390X_DECLINE_TAG: u32 = 0x5390_0000;

/// The most arguments a non-self call may pass from a register-entry body:
/// the rooting slice adapter copies that many (bliss-of8kz).
pub const CALL_ARGS_MAX: usize = 8;

/// A structural decline (a shape this emitter does not handle, as opposed to
/// an opcode it refuses, which carries the opcode tag): names the line that
/// declined, so the T2 log can say where. The runtime decodes it as
/// `[s390x emitter declined at emit_s390x.rs:LINE]`.
macro_rules! unsupported {
    () => {
        EmitError::UnsupportedOp(S390X_DECLINE_TAG | line!())
    };
}

/// Order shared recipes before their users without recursive host calls or
/// exponential re-emission. Public emitter callers may supply unverified IR,
/// so reject missing dependencies and cycles here as well as in the verifier.
pub(super) fn rematerialization_order(state: &FrameState) -> Result<Vec<usize>, EmitError> {
    let mut marks = vec![0; state.remat.len()];
    let mut order = Vec::new();
    let roots = state
        .scopes
        .iter()
        .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
        .filter_map(|source| match source {
            ValueSource::Remat(id) => Some(id.0 as usize),
            _ => None,
        });
    for root in roots {
        let mut pending = vec![(root, false)];
        while let Some((index, finish)) = pending.pop() {
            let mark = marks.get_mut(index).ok_or(unsupported!())?;
            if finish {
                *mark = 2;
                order.push(index);
                continue;
            }
            match *mark {
                2 => continue,
                1 => return Err(unsupported!()),
                _ => *mark = 1,
            }
            pending.push((index, true));
            for input in state.remat[index].inputs.iter().rev() {
                if let ValueSource::Remat(id) = input {
                    pending.push((id.0 as usize, false));
                }
            }
        }
    }
    Ok(order)
}

impl Emitter<'_> {
    /// A call to this very function, entered through the register entry. The
    /// published native stack limit is compared first: once the stack runs low
    /// the ordinary `call_slice` path is taken instead, whose adapter enforces
    /// the native depth cap and raises a catchable STORAGE-CONDITION. A zero
    /// limit (never published) compares false, so the guard is inert.
    fn direct_self_call(&mut self, data: &InstData) -> Result<(), EmitError> {
        let reg_entry = self.reg_entry.ok_or(unsupported!())?;
        let AuxData::CallTarget(symbol) = data.aux else {
            return Err(unsupported!());
        };
        if data.args.len() > 3 {
            return Err(unsupported!());
        }
        let slow = self.asm.label();
        let join = self.asm.label();
        // A self-call with the wrong argument count must reach the generic
        // adapter, which raises PROGRAM-ERROR; the register entry binds
        // whatever is in r2-r4 (bliss-w6aki: `(g 0 0)` in a one-parameter G
        // answered 0 at T2). Only the slow path is emitted for it.
        let arity_matches =
            data.args.len() == self.function.block(self.function.entry()).params.len();
        if arity_matches {
            self.asm.imm64(1, egcl_rt::stack::native_stack_limit_addr());
            self.asm.load(1, 1, 0);
            // Signed compare is right: stack addresses are canonical and
            // positive. CC0 (equal) or CC1 (r15 low) means the stack is at or
            // past the limit.
            self.asm.compare(15, 1);
            self.asm.branch(12, slow);
            // Fast path: arguments in r2-r4 (no overlap with the r6-r12 homes),
            // then a relative call to the register entry; the result is in r2.
            for (index, &argument) in data.args.iter().enumerate() {
                self.load(argument, 2 + index as u8)?;
            }
            self.asm.call_label(reg_entry);
            self.asm.branch(15, join);
        }
        // Slow path: generic dispatch through the slice adapter, the arguments
        // staged in this frame. No live tagged heap value exists across the
        // call (eligibility), so nothing needs a shadow root here.
        self.asm.bind(slow);
        for (index, &argument) in data.args.iter().enumerate() {
            self.load(argument, 2)?;
            self.asm
                .store(2, 15, self.self_args_base + index as i32 * 8);
        }
        self.asm.imm64(2, u64::from(symbol));
        self.asm.imm64(3, data.args.len() as u64);
        self.asm.address(4, 15, self.self_args_base);
        self.asm.imm64(5, 0);
        let offset = self.asm.here();
        self.asm.imm64(1, self.runtime.call_slice);
        self.asm.call_reg(1);
        self.asm.bind(join);
        // Either path can leave a pending transfer (a deopt or an error raised
        // inside the callee); propagate it exactly as runtime_call does.
        self.asm.store(2, 15, self.result_offset);
        if self.runtime.transfer_pending != 0 {
            self.asm.imm64(1, self.runtime.transfer_pending);
            self.asm.call_reg(1);
            self.asm.imm64(3, 0);
            self.asm.compare(2, 3);
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.branch(6, exit);
        }
        if let Some(&result) = data.results.first() {
            self.asm.load(2, 15, self.result_offset);
            self.store(result, 2)?;
        }
        self.root_sites.push(RootSyncSite {
            code_offset: u32::try_from(offset).map_err(|_| unsupported!())?,
            live_roots: 0,
            register_roots: 0,
            spill_roots: 0,
        });
        Ok(())
    }

    fn runtime_call(
        &mut self,
        instruction: Inst,
        data: Option<&InstData>,
    ) -> Result<(), EmitError> {
        if let Some(data) = data
            && data.opcode == Opcode::Call
            && self.reg_entry.is_some()
            && matches!(data.aux, AuxData::CallTarget(symbol) if Some(symbol) == self.runtime.self_sym)
        {
            return self.direct_self_call(data);
        }
        let roots = self.roots.get(&instruction).ok_or(unsupported!())?.clone();
        let root_base = i32::from(self.activation_slots) * 8;
        let argument_base = root_base + i32::from(self.root_slots) * 8;
        // Clear unused shadows too: an earlier call may have had more live
        // roots or arguments. Dead objects need not be retained indefinitely.
        self.asm.imm64(2, NIL.0);
        for index in 0..u32::from(self.root_slots) + u32::from(self.argument_slots) {
            self.asm.store(2, 13, root_base + index as i32 * 8);
        }
        for (index, &value) in roots.iter().enumerate() {
            self.load(value, 2)?;
            self.asm.store(2, 13, root_base + index as i32 * 8);
        }
        // Bound after the helper call when a direct native fast path precedes
        // it: both paths leave the result in r2 and share the post-call code.
        let mut direct_join: Option<Label> = None;
        let helper = if let Some(data) = data {
            match data.opcode {
                Opcode::Call => {
                    let AuxData::CallTarget(symbol) = data.aux else {
                        return Err(unsupported!());
                    };
                    // A register-entry body must not touch r13 (the caller's
                    // activation when entered that way): it stages the
                    // arguments in its own frame instead (bliss-of8kz).
                    let in_frame = self.reg_entry.is_some();
                    let (args_reg, args_base) = if in_frame {
                        (15, self.self_args_base)
                    } else {
                        (13, argument_base)
                    };
                    for (index, &arg) in data.args.iter().enumerate() {
                        self.load(arg, 2)?;
                        self.asm.store(2, args_reg, args_base + index as i32 * 8);
                    }
                    // A native callee the runtime resolved for this site is
                    // entered directly; its guards fall through to the slice
                    // adapter below (bliss-6j6pk).
                    if let Some(target) = self.direct_native_for(symbol, data.args.len()) {
                        let slow = self.asm.label();
                        let join = self.asm.label();
                        self.direct_native_call(&target, args_reg, args_base, slow);
                        self.asm.branch(15, join);
                        self.asm.bind(slow);
                        direct_join = Some(join);
                    }
                    // A leaf builtin resolved at compile time goes through the
                    // direct slice adapter with its table slot baked into r2
                    // and the invalidation generation in r5, as emit.rs does
                    // with its register adapter (bliss-x5y.27, bliss-flzmi).
                    // Not from a register-entry body: the builtin reads the
                    // unrooted frame area while it may allocate.
                    let builtin = if in_frame {
                        None
                    } else {
                        super::emit::direct_builtin_slice_for(symbol, data.args.len())
                    };
                    match builtin {
                        Some((adapter, slot, generation)) => {
                            self.asm
                                .imm64(2, (u64::from(slot) << 32) | u64::from(symbol));
                            self.asm.imm64(3, data.args.len() as u64);
                            self.asm.address(4, args_reg, args_base);
                            self.asm.imm64(5, generation);
                            adapter
                        }
                        None => {
                            self.asm.imm64(2, u64::from(symbol));
                            self.asm.imm64(3, data.args.len() as u64);
                            self.asm.address(4, args_reg, args_base);
                            self.asm.imm64(5, 0);
                            if in_frame {
                                self.runtime.call_slice_rooted
                            } else {
                                self.runtime.call_slice
                            }
                        }
                    }
                }
                Opcode::SymbolValue | Opcode::SymbolFunction | Opcode::SetSymbolValue => {
                    let AuxData::SymbolRef(symbol) = data.aux else {
                        return Err(unsupported!());
                    };
                    if data.opcode == Opcode::SetSymbolValue {
                        self.load(*data.args.first().ok_or(unsupported!())?, 3)?;
                    }
                    self.asm.imm64(2, u64::from(symbol));
                    match data.opcode {
                        Opcode::SymbolValue => self.runtime.load_global,
                        Opcode::SymbolFunction => self.runtime.load_function,
                        _ => self.runtime.store_global,
                    }
                }
                Opcode::ClearMv => {
                    self.asm.imm64(2, NIL.0);
                    self.asm.imm64(3, 0);
                    self.asm.imm64(4, 0);
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
                    self.load(*data.args.first().ok_or(unsupported!())?, 2)?;
                    self.asm.address(3, 13, i32::from(slot_base) * 8);
                    self.asm.imm64(4, u64::from(nvars));
                    self.runtime.multiple_values
                }
                _ => return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode))),
            }
        } else {
            self.runtime.poll
        };
        if helper == 0 {
            return Err(unsupported!());
        }
        let offset = self.asm.here();
        self.asm.imm64(1, helper);
        self.asm.call_reg(1);
        if let Some(join) = direct_join {
            self.asm.bind(join);
        }
        self.asm.store(2, 15, self.result_offset);
        if data.is_none() || self.runtime.transfer_pending != 0 {
            if data.is_some() {
                self.asm.imm64(1, self.runtime.transfer_pending);
                self.asm.call_reg(1);
            }
            self.asm.imm64(3, 0);
            self.asm.compare(2, 3);
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.branch(6, exit);
        }
        // Restore roots before assigning results. A dying argument may share
        // its home with the call result, but must never overwrite that result.
        for (index, &value) in roots.iter().enumerate() {
            self.asm.load(2, 13, root_base + index as i32 * 8);
            self.store(value, 2)?;
        }
        if let Some(data) = data {
            if let AuxData::ValuesLocals { slot_base, .. } = data.aux {
                for (index, &value) in data.results.iter().enumerate() {
                    self.asm
                        .load(2, 13, (i32::from(slot_base) + index as i32) * 8);
                    self.store(value, 2)?;
                }
            } else if let Some(&result) = data.results.first() {
                self.asm.load(2, 15, self.result_offset);
                self.store(result, 2)?;
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

    fn load(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        if let Some(bits) = self.constants.get(&value) {
            self.asm.imm64(register, *bits);
        } else if let Some(slot) = self.heap_constants.get(&value) {
            self.asm.imm64(1, *slot as u64);
            self.asm.load(register, 1, 0);
        } else {
            match self.homes.get(&value).ok_or(unsupported!())? {
                Home::Register(source) => self.asm.mov(register, *source),
                Home::Stack(slot) => self.asm.load(register, 15, 160 + (*slot as i32) * 8),
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        match self.homes.get(&value).ok_or(unsupported!())? {
            Home::Register(destination) => self.asm.mov(*destination, register),
            Home::Stack(slot) => self.asm.store(register, 15, 160 + (*slot as i32) * 8),
        }
        Ok(())
    }

    fn prologue(&mut self) {
        self.asm.prologue_from(self.first_saved);
        self.asm.address(15, 15, -self.frame_bytes);
        self.asm.mov(13, 2);
        // The entry's second argument is the EgclStack; a direct native call
        // pushes the callee's frame on it. (The register entry does not run
        // this: its r3 is an argument, and a function with a register entry
        // makes only self-calls, so it never needs the pointer.)
        self.asm.store(3, 15, self.stack_offset);
        // This activation owns the frame at fp (run_native's, or the one a
        // direct call published): a deopt may resume on it.
        self.asm.imm64(2, self.runtime.code_id);
        self.asm.store(2, 15, self.entry_kind_offset);
        self.init_poll_word();
    }

    /// The sampled back-edge poll's countdown; a function without a back
    /// edge never reads it, so its entry need not write it.
    fn init_poll_word(&mut self) {
        if !self.polls.is_empty() {
            self.asm.imm64(2, 256);
            self.asm.store(2, 15, self.poll_offset);
        }
    }

    /// The listed native callee for `symbol` at this arity, if any.
    fn direct_native_for(
        &self,
        symbol: u32,
        nargs: usize,
    ) -> Option<super::emit::DirectNativeTarget> {
        if self.runtime.self_sym == Some(symbol) {
            return None;
        }
        self.runtime
            .direct_natives
            .iter()
            .find(|target| target.symbol == symbol && usize::from(target.nargs) == nargs)
            .cloned()
    }

    /// Enter `target` directly from a `Call` site whose arguments are already
    /// staged in the shadow argument area at `argument_base` (bliss-6j6pk; the
    /// T1 emitter's `direct_native_call`, with the caller's bookkeeping in
    /// frame words instead of callee-saved registers, since r6-r12 are homes
    /// here). Falls back to `slow` -- the ordinary `call_slice` sequence --
    /// when the native stack is near its limit, when the direct-call
    /// generation has moved since the target was resolved, or when the
    /// callee's frame would not fit the EgclStack. On the fast path the
    /// callee's frame is pushed and published (fp, sp_offset) before the
    /// entry runs, so every argument slot is a scanned root while the callee
    /// allocates; afterwards the caller's fp and sp_offset are restored and
    /// the result is in r2, exactly where the slow path leaves it. The
    /// callee's entry preserves r6-r15, so the homes and r13 survive it.
    fn direct_native_call(
        &mut self,
        target: &super::emit::DirectNativeTarget,
        args_reg: u8,
        argument_base: i32,
        slow: Label,
    ) {
        let bs_base = egcl_rt::EgclStack::OFFSET_BASE as i32;
        let bs_sp = egcl_rt::EgclStack::OFFSET_SP_OFFSET as i32;
        let bs_fp = egcl_rt::EgclStack::OFFSET_FP as i32;
        let bs_cap = egcl_rt::EgclStack::OFFSET_CAPACITY as i32;
        let f_prev = core::mem::offset_of!(egcl_rt::Frame, prev_fp) as i32;
        let f_ret = core::mem::offset_of!(egcl_rt::Frame, return_pc) as i32;
        let f_func = core::mem::offset_of!(egcl_rt::Frame, function) as i32;
        let f_ci = core::mem::offset_of!(egcl_rt::Frame, code_info) as i32;
        let f_flags = core::mem::offset_of!(egcl_rt::Frame, flags) as i32;
        let f_nloc = core::mem::offset_of!(egcl_rt::Frame, num_locals) as i32;
        let hdr = core::mem::size_of::<egcl_rt::Frame>() as i32;
        let nslots = i32::from(target.num_slots);
        let nargs = i32::from(target.nargs);
        let framebytes = hdr + 8 * nslots;
        // Native stack headroom, as the direct self-call checks it.
        self.asm.imm64(1, egcl_rt::stack::native_stack_limit_addr());
        self.asm.load(1, 1, 0);
        self.asm.compare(15, 1);
        self.asm.branch(12, slow);
        // Guard 1: the direct-call generation still equals the baked one.
        self.asm.imm64(1, target.generation_addr);
        self.asm.load(4, 1, 0);
        self.asm.imm64(5, target.generation);
        self.asm.compare(4, 5);
        self.asm.branch(6, slow);
        // r1 = the EgclStack; r2 = base, r3 = old sp_offset, r4 = new sp_offset.
        self.asm.load(1, 15, self.stack_offset);
        self.asm.load(2, 1, bs_base);
        self.asm.load(3, 1, bs_sp);
        self.asm.address(4, 3, framebytes);
        // Guard 2: the callee's frame fits. new sp_offset > capacity => slow.
        self.asm.load(5, 1, bs_cap);
        self.asm.compare(4, 5);
        self.asm.branch(2, slow);
        // r5 = frame start = base + old sp_offset.
        self.asm.mov(5, 2);
        self.asm.add(5, 3);
        // Park the caller's fp (r2) and sp_offset (r3).
        self.asm.load(2, 1, bs_fp);
        self.asm.store(2, 15, self.saved_fp_offset);
        self.asm.store(3, 15, self.saved_sp_offset);
        // Frame header.
        self.asm.store(2, 5, f_prev);
        self.asm.imm64(2, 0);
        self.asm.store(2, 5, f_ret);
        self.asm.imm64(2, NIL.0);
        self.asm.store(2, 5, f_func);
        self.asm.imm64(2, target.code_info);
        self.asm.store(2, 5, f_ci);
        self.asm.imm64(2, u64::from(target.frame_flags));
        self.asm.store_u32(2, 5, f_flags);
        self.asm.imm64(2, nslots as u64);
        self.asm.store_u16(2, 5, f_nloc);
        // Bind the arguments from the staging area (the activation's shadow
        // slots, or the native frame of a register-entry body); the rest
        // start as NIL.
        for index in 0..nargs {
            self.asm.load(2, args_reg, argument_base + 8 * index);
            self.asm.store(2, 5, hdr + 8 * index);
        }
        if nargs < nslots {
            self.asm.imm64(2, NIL.0);
            for index in nargs..nslots {
                self.asm.store(2, 5, hdr + 8 * index);
            }
        }
        // Publish the frame, then call: r2 = the callee's slots, r3 = the stack.
        self.asm.store(5, 1, bs_fp);
        self.asm.store(4, 1, bs_sp);
        self.asm.address(2, 5, hdr);
        self.asm.mov(3, 1);
        self.asm.imm64(1, target.entry);
        self.asm.call_reg(1);
        // Pop the callee's frame: restore the parked fp and sp_offset.
        self.asm.load(1, 15, self.stack_offset);
        self.asm.load(3, 15, self.saved_fp_offset);
        self.asm.store(3, 1, bs_fp);
        self.asm.load(3, 15, self.saved_sp_offset);
        self.asm.store(3, 1, bs_sp);
    }

    fn epilogue(&mut self) {
        self.asm.address(15, 15, self.frame_bytes);
        self.asm.epilogue_from(self.first_saved);
    }

    fn deopt_label(&mut self, data: &InstData) -> Result<Label, EmitError> {
        let state = data.frame_state.ok_or(unsupported!())?;
        if self.function.frame_states.get(state).scopes.is_empty() {
            return Err(unsupported!());
        }
        let label = self.asm.label();
        self.deopts.push((state, label));
        Ok(label)
    }

    /// The fixnum tag is 0, so one TMLL answers "any tag bit set".
    fn guard_fixnum(&mut self, register: u8, deopt: Label) {
        self.asm.test_mask_low(register, 7);
        self.asm.branch(7, deopt);
    }

    /// r4 = the low three bits of `register`, for a tag compare.
    fn load_tag(&mut self, register: u8) {
        self.asm.mov(4, register);
        self.asm.imm64(5, 7);
        self.asm.and(4, 5);
    }

    fn guard_single_float(&mut self, register: u8, deopt: Label) {
        self.load_tag(register);
        self.asm.compare_imm(4, 4);
        self.asm.branch(6, deopt);
    }

    /// A type predicate on the tagged value in r2, leaving T or NIL in r2.
    /// Mirrors emit.rs's emit_type_check exactly, including what it DECLINES:
    /// CONS and LIST (an interpreter closure is a cons that is not of type
    /// LIST, bliss-74rl) and the STRING class (a complex string is a
    /// COMPLEX_ARRAY, so the two simple layouts alone would answer NIL where
    /// the other tiers answer T, bliss-c02n). `bits == STRING` is the layout
    /// guard's form and keeps the simple-layout test. Declining keeps the tiers
    /// bit-identical, which is the bliss-x5y.9 rule.
    ///
    /// The object header's type id lives in bits 63:56 of the header word, the
    /// byte at offset 7 on little-endian x86-64 and at offset 0 here (bliss-nlpd4
    /// was the first measured s390x opcode gap: SYMBOL-NAME and the LOOP
    /// keyword helper declined on it).
    fn type_check(&mut self, first: Value, data: &InstData) -> Result<(), EmitError> {
        use egcl_rt::bytecode::typep_class;
        use egcl_rt::object::type_id;
        use egcl_rt::value::{TAG_HEAP_OBJECT, TAG_SYMBOL};
        let refused = || EmitError::UnsupportedOp(super::emit::op_tag(Opcode::TypeCheck));
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
        // r2 holds the operand throughout; r3-r5 are scratch.
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
        self.asm.imm64(2, NIL.0);
        self.asm.branch(15, done);
        self.asm.bind(found);
        self.asm.imm64(2, T.0);
        self.asm.bind(done);
        Ok(())
    }

    /// Branch to `target` when r2 equals the tagged constant.
    fn branch_if_equal_imm(&mut self, bits: u64, target: Label) {
        if let Ok(half) = i16::try_from(bits as i64) {
            self.asm.compare_imm(2, half);
        } else {
            self.asm.imm64(3, bits);
            self.asm.compare(2, 3);
        }
        self.asm.branch(8, target);
    }

    /// Branch to `target` when r2's low three bits equal `tag`.
    fn branch_if_tag(&mut self, tag: u64, target: Label) {
        self.load_tag(2);
        self.asm.compare_imm(4, tag as i16);
        self.asm.branch(8, target);
    }

    /// Branch to `target` unless r2's low three bits equal `tag`.
    fn branch_unless_tag(&mut self, tag: u64, target: Label) {
        self.load_tag(2);
        self.asm.compare_imm(4, tag as i16);
        self.asm.branch(6, target);
    }

    /// Load the header type id of the heap object in r2 into r4: clear the tag
    /// bits to reach the header word and read its most significant byte, which
    /// is byte 0 on big-endian System Z.
    fn load_type_id(&mut self) {
        self.asm.mov(4, 2);
        self.asm.imm64(5, !7u64);
        self.asm.and(4, 5);
        self.asm.load_u8(4, 4, 0);
    }

    /// Branch to `target` when the type id loaded by [`Self::load_type_id`] is `id`.
    fn branch_if_type_id(&mut self, id: u8, target: Label) {
        self.asm.imm64(5, u64::from(id));
        self.asm.compare(4, 5);
        self.asm.branch(8, target);
    }

    /// The simple-string layout guard on the value in r2 (emit.rs's
    /// guard_simple_string): a heap object whose header type id is one of the
    /// two simple string layouts, else deopt. r2 is left as it was, since the
    /// guard's result is its operand under a refined identity.
    fn guard_simple_string(&mut self, deopt: Label) {
        use egcl_rt::object::type_id;
        self.asm.mov(4, 2);
        self.asm.imm64(5, 7);
        self.asm.and(4, 5);
        self.asm.imm64(5, egcl_rt::value::TAG_HEAP_OBJECT);
        self.asm.compare(4, 5);
        self.asm.branch(6, deopt);
        let layout_ok = self.asm.label();
        self.load_type_id();
        self.branch_if_type_id(type_id::SIMPLE_BASE_STRING, layout_ok);
        self.branch_if_type_id(type_id::SIMPLE_CHARACTER_STRING, layout_ok);
        self.asm.branch(15, deopt);
        self.asm.bind(layout_ok);
    }

    /// `(CHAR string index)` for an ASCII character, emit.rs's
    /// emit_string_ascii_char_at: the string in r2 is layout-proven; the index
    /// must be a non-negative fixnum below the byte length, and every byte
    /// through the selected one must be ASCII, since a byte index is a
    /// character index only then (byte 2 of "éa" is not character 2). Anything
    /// else deopts to the stdlib's UTF-8-aware CL:CHAR. Leaves the tagged
    /// character in r2. r1 carries the transient byte, as RAX does on x86-64.
    fn string_ascii_char_at(&mut self, index: Value, deopt: Label) -> Result<(), EmitError> {
        // r4 = the untagged string, r3 = the tagged index.
        self.asm.mov(4, 2);
        self.asm.imm64(5, !7u64);
        self.asm.and(4, 5);
        self.load(index, 3)?;
        // A fixnum (NGR sets the condition code: nonzero low bits deopt)...
        self.asm.mov(5, 3);
        self.asm.imm64(2, 7);
        self.asm.and(5, 2);
        self.asm.branch(6, deopt);
        // ...that is non-negative and below the byte length; both compare as
        // tagged fixnums, which keeps the order of all representable sizes.
        self.asm.imm64(2, 0);
        self.asm.compare(3, 2);
        self.asm.branch(4, deopt);
        self.asm.load(2, 4, 8);
        self.asm.shift_left(2, 2, 3);
        self.asm.compare(3, 2);
        self.asm.branch(10, deopt);
        // r5 = &data[0], the cursor; r2 = &data[index], the bound.
        self.asm.shift_right_signed(3, 3, 3);
        self.asm.address(5, 4, 16);
        self.asm.mov(2, 5);
        self.asm.add(2, 3);
        let scan = self.asm.label();
        let selected = self.asm.label();
        self.asm.bind(scan);
        self.asm.load_u8(1, 5, 0);
        self.asm.imm64(3, 128);
        self.asm.compare(1, 3);
        self.asm.branch(10, deopt);
        self.asm.compare(5, 2);
        self.asm.branch(8, selected);
        self.asm.add_imm(5, 1);
        self.asm.branch(15, scan);
        self.asm.bind(selected);
        // The tagged character: (byte << 3) | TAG_CHARACTER.
        self.asm.shift_left(2, 1, 3);
        self.asm.imm64(3, egcl_rt::value::TAG_CHARACTER);
        self.asm.or(2, 3);
        Ok(())
    }

    fn guard_cons(&mut self, register: u8, deopt: Label) {
        self.load_tag(register);
        self.asm.compare_imm(4, egcl_rt::value::TAG_CONS as i16);
        self.asm.branch(6, deopt);
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

    fn multiply_fixnums(&mut self, deopt: Label) {
        // Multiply a signed untagged lhs by the tagged rhs. MLGR supplies
        // the full unsigned 128-bit product on every z10-compatible CPU.
        // Correct its high half for signed operands, then require that high
        // half to equal the low half's sign extension. No allocated home is
        // modified before the overflow exit, so deopt retains both inputs.
        self.asm.shift_right_signed(5, 2, 3);
        self.asm.mov(4, 3);
        self.asm.mov(3, 5);
        self.asm.multiply_unsigned_wide(2, 4);
        self.asm.imm64(0, 0);
        let lhs_nonnegative = self.asm.label();
        self.asm.compare(5, 0);
        self.asm.branch(10, lhs_nonnegative);
        self.asm.sub(2, 4);
        self.asm.bind(lhs_nonnegative);
        let rhs_nonnegative = self.asm.label();
        self.asm.compare(4, 0);
        self.asm.branch(10, rhs_nonnegative);
        self.asm.sub(2, 5);
        self.asm.bind(rhs_nonnegative);
        self.asm.shift_right_signed(4, 3, 63);
        self.asm.compare(2, 4);
        self.asm.branch(6, deopt);
        self.asm.mov(2, 3);
    }

    fn edge(&mut self, edge: &BlockCall) -> Result<(), EmitError> {
        let parameters = self.function.block(edge.block).params.clone();
        if parameters.len() != edge.args.len() {
            return Err(unsupported!());
        }
        // A parallel copy through private native slots handles cycles and
        // overlapping register/spill homes without clobbering an edge input.
        for (index, &source) in edge.args.iter().enumerate() {
            self.load(source, 2)?;
            self.asm.store(2, 15, self.edge_base + index as i32 * 8);
        }
        for (index, destination) in parameters.into_iter().enumerate() {
            self.asm.load(2, 15, self.edge_base + index as i32 * 8);
            self.store(destination, 2)?;
        }
        self.asm.branch(15, self.blocks[&edge.block]);
        Ok(())
    }

    fn instruction(&mut self, instruction: Inst, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
        if self.polls.contains(&instruction) {
            let skip = self.asm.label();
            self.asm.load(2, 15, self.poll_offset);
            self.asm.add_imm(2, -1);
            self.asm.store(2, 15, self.poll_offset);
            self.asm.branch(6, skip);
            self.asm.imm64(2, 256);
            self.asm.store(2, 15, self.poll_offset);
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
                    self.load(value, 2)?;
                } else {
                    self.asm.imm64(2, NIL.0);
                }
                self.epilogue();
                return Ok(());
            }
            Jump if data.targets.len() == 1 => return self.edge(&data.targets[0]),
            Brif if data.args.len() == 1 && data.targets.len() == 2 => {
                let otherwise = self.asm.label();
                self.load(data.args[0], 2)?;
                self.asm.imm64(3, NIL.0);
                self.asm.compare(2, 3);
                self.asm.branch(8, otherwise);
                self.edge(&data.targets[0])?;
                self.asm.bind(otherwise);
                return self.edge(&data.targets[1]);
            }
            MemoryFence => {
                // A full fence for every kind (see bytecode_s390x.rs); the
                // value is NIL.
                self.asm.serialize();
                if let Some(&value) = data.results.first() {
                    self.asm.imm64(2, NIL.0);
                    self.store(value, 2)?;
                }
                return Ok(());
            }
            Trap => {
                // A Trap ends a path that must not continue (bliss-wukf): the
                // code after a call that never returns normally. As in
                // emit.rs, the GENERIC deopt suffices even though it re-runs
                // the function: this is only reachable once a c2i helper has
                // stashed an error, and run_native takes NATIVE_ERROR before
                // it looks at NATIVE_DEOPT, so no committed side effect is
                // repeated and no FrameState is needed (REDUCE's :from-end
                // path was the first measured s390x function refused here).
                if self.runtime.deopt == 0 {
                    return Err(unsupported!());
                }
                self.asm.imm64(1, self.runtime.deopt);
                self.asm.call_reg(1);
                self.asm.imm64(2, NIL.0);
                self.epilogue();
                return Ok(());
            }
            _ => {}
        }
        // Everything below produces a value from a first operand; an opcode
        // that does neither is refused by name, so the T2 log says which.
        let Some(&result) = data.results.first() else {
            return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)));
        };
        let Some(&first) = data.args.first() else {
            return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)));
        };
        self.load(first, 2)?;
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
                        self.guard_fixnum(2, deopt)
                    }
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::CONS => self.guard_cons(2, deopt),
                    _ => self.guard_single_float(2, deopt),
                }
            }
            FloatAdd | FloatSub | FloatMul => {
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, 2, deopt)?;
                self.float_operand(*data.args.get(1).ok_or(unsupported!())?, 3, deopt)?;
                self.asm.load_float_bits(0, 2);
                self.asm.load_float_bits(2, 3);
                match data.opcode {
                    FloatAdd => self.asm.add_single(0, 2),
                    FloatSub => self.asm.sub_single(0, 2),
                    _ => self.asm.multiply_single(0, 2),
                }
                self.asm.store_float_bits(2, 0);
                self.asm.imm64(3, 0xffff_ffff_0000_0000);
                self.asm.and(2, 3);
                self.asm.add_imm(2, 4);
            }
            FixnumAdd | FixnumSub | FixnumMul | FixnumNeg | FixnumCmpEq | FixnumCmpLt
            | FixnumCmpLe | FixnumCmpGt | FixnumCmpGe => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(2, deopt);
                if data.opcode != FixnumNeg {
                    self.load(*data.args.get(1).ok_or(unsupported!())?, 3)?;
                    self.guard_fixnum(3, deopt);
                }
                match data.opcode {
                    FixnumAdd => self.asm.add(2, 3),
                    FixnumSub => self.asm.sub(2, 3),
                    FixnumMul => {
                        self.multiply_fixnums(deopt);
                        return self.store(result, 2);
                    }
                    FixnumNeg => {
                        self.asm.imm64(3, 0);
                        self.asm.sub(3, 2);
                        self.asm.mov(2, 3);
                    }
                    comparison => {
                        self.asm.compare(2, 3);
                        let yes = self.asm.label();
                        let done = self.asm.label();
                        let mask = match comparison {
                            FixnumCmpEq => 8,
                            FixnumCmpLt => 4,
                            FixnumCmpLe => 12,
                            FixnumCmpGt => 2,
                            FixnumCmpGe => 10,
                            _ => unreachable!(),
                        };
                        self.asm.branch(mask, yes);
                        self.asm.imm64(2, NIL.0);
                        self.asm.branch(15, done);
                        self.asm.bind(yes);
                        self.asm.imm64(2, T.0);
                        self.asm.bind(done);
                        return self.store(result, 2);
                    }
                }
                self.asm.branch(1, deopt); // signed arithmetic overflow (CC3)
            }
            GenericEq => {
                // Lisp EQ (and NULL / NOT, which speculate.rs rewrites to
                // GenericEq(x, NIL)) is bit-equality of the two tagged words, so
                // it is one CGR and a select of T/NIL, exactly as emit.rs's
                // emit_generic_eq does it. No guard: the operands may be any
                // tagged values and the rewrite carries no frame state. Declining
                // it cost every function containing a NOT, a NULL or an EQ its
                // T2 code on s390x (bliss-pig52).
                self.load(*data.args.get(1).ok_or(unsupported!())?, 3)?;
                self.asm.compare(2, 3);
                let yes = self.asm.label();
                let done = self.asm.label();
                self.asm.branch(8, yes);
                self.asm.imm64(2, NIL.0);
                self.asm.branch(15, done);
                self.asm.bind(yes);
                self.asm.imm64(2, T.0);
                self.asm.bind(done);
            }
            Car | Cdr => {
                // A cons cell is HEADERLESS and its tagged pointer differs from
                // the cell address only in the low three bits, so masking them
                // off gives the base and the two slots sit at +0 and +8. Mirrors
                // emit.rs and emit_a64.rs, including the refusal of the guarded
                // form: this is the fast path for a cons whose type the IR has
                // already proven, and a Car still carrying a guard, an effect or
                // a FrameState needs the general path instead.
                if data.flags.guard || data.flags.effectful || data.frame_state.is_some() {
                    return Err(unsupported!());
                }
                self.asm.imm64(3, !7u64);
                self.asm.and(2, 3);
                let offset = if data.opcode == Car { 0 } else { 8 };
                self.asm.load(2, 2, offset);
            }
            LogAnd | LogOr | LogXor => {
                // Exact on the tagged representation, exactly as the x86 and
                // AArch64 emitters do it: both operands carry the same zero tag,
                // so tagged(a) OP tagged(b) = (a OP b)<<3 = tagged(a OP b). No
                // untag, no retag, no overflow.
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(2, deopt);
                self.load(*data.args.get(1).ok_or(unsupported!())?, 3)?;
                self.guard_fixnum(3, deopt);
                match data.opcode {
                    LogAnd => self.asm.and(2, 3),
                    LogOr => self.asm.or(2, 3),
                    _ => self.asm.xor(2, 3),
                }
            }
            LogNot => {
                // NOT leaves the tag bits set, so they must be cleared again:
                // ~(x<<3) has its low three bits all 1, and masking them off
                // yields ~(x<<3) - 7 = (~x)<<3, which is tagged(~x). System Z
                // has no register NOT, so XOR with all ones.
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(2, deopt);
                self.asm.imm64(3, u64::MAX);
                self.asm.xor(2, 3);
                self.asm.imm64(3, !7u64);
                self.asm.and(2, 3);
            }
            FixnumShl | FixnumShr => {
                // (ash x n) for a CONSTANT n; a variable amount declines. The
                // constants map holds TAGGED bits, so the shift count is
                // recovered by untagging it.
                let deopt = self.deopt_label(data)?;
                let amount = *data.args.get(1).ok_or(unsupported!())?;
                let tagged = *self
                    .constants
                    .get(&amount)
                    .ok_or_else(|| EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)))?;
                let n = (tagged as i64) >> 3;
                self.guard_fixnum(2, deopt);
                // DIRECTION, mirroring emit.rs and easy to get backwards: on
                // FixnumShl a NEGATIVE constant is a RIGHT shift, because that is
                // how `(ash x -2)` lowers -- ASH becomes FixnumShl whatever the
                // sign, and the emitter reads the constant to choose. FixnumShr is
                // always a right shift. Inverting this is not a crash, it is a
                // wrong number: on AArch64 it made `(ash -17 -2)` answer -68
                // instead of -5, and only differential testing against x86-64
                // showed it (bliss-ys4ha).
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
                    // gives, and SRAG takes a 6-bit amount.
                    let k = right.min(60) as u8;
                    self.asm.shift_right_signed(2, 2, 3 + k);
                    self.asm.shift_left(2, 2, 3);
                } else {
                    // Left shift by n: tagged(x)<<n = tagged(x<<n), but it can
                    // leave fixnum range, and unlike add/sub no condition code
                    // reports that. Shift back and compare: if the value does not
                    // survive the round trip it overflowed, so deopt and let the
                    // generic path produce a bignum. r2 is untouched until the
                    // check passes, so the deopt exit sees the operand.
                    if n > 60 {
                        return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)));
                    }
                    let k = n as u8;
                    self.asm.shift_left(3, 2, k);
                    self.asm.shift_right_signed(4, 3, k);
                    self.asm.compare(4, 2);
                    self.asm.branch(6, deopt);
                    self.asm.mov(2, 3);
                }
            }
            TypeCheck => self.type_check(first, data)?,
            Guard if matches!(&data.aux, AuxData::StringLayout) => {
                if data.args.len() != 1 || !data.flags.guard || !data.flags.effectful {
                    return Err(EmitError::UnsupportedOp(super::emit::op_tag(Opcode::Guard)));
                }
                let deopt = self.deopt_label(data)?;
                self.guard_simple_string(deopt);
            }
            StringByteLength => {
                if data.args.len() != 1
                    || data.flags.guard
                    || data.flags.effectful
                    || data.frame_state.is_some()
                {
                    return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)));
                }
                // The operand is layout-proven. r4 = the untagged object; its
                // byte length is the second word, returned as a fixnum.
                self.asm.mov(4, 2);
                self.asm.imm64(5, !7u64);
                self.asm.and(4, 5);
                self.asm.load(2, 4, 8);
                self.asm.shift_left(2, 2, 3);
            }
            StringAsciiCharAt => {
                if data.args.len() != 2
                    || !data.flags.guard
                    || !data.flags.effectful
                    || data.frame_state.is_none()
                {
                    return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode)));
                }
                let deopt = self.deopt_label(data)?;
                self.string_ascii_char_at(data.args[1], deopt)?;
            }
            // Name the opcode: the T2 log then says "refused Opcode #N in
            // t2/ir.rs" instead of this emitter's anonymous 0x390, so a
            // histogram of declines over real code ranks the missing arms
            // (bliss-nlpd4).
            _ => return Err(EmitError::UnsupportedOp(super::emit::op_tag(data.opcode))),
        }
        // Do not overwrite any allocated home until every guard has passed:
        // a failing instruction must reconstruct its pre-instruction state.
        self.store(result, 2)
    }

    fn deopt_source(&mut self, source: &ValueSource, state: &FrameState) -> Result<(), EmitError> {
        match source {
            ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            } => self.load(*value, 2),
            ValueSource::Const(value)
                if !value.is_heap_object() && !value.is_cons() && !value.is_function() =>
            {
                self.asm.imm64(2, value.0);
                Ok(())
            }
            ValueSource::Unbound => {
                self.asm.imm64(2, UNBOUND.0);
                Ok(())
            }
            ValueSource::Remat(id) if (id.0 as usize) < state.remat.len() => {
                self.asm.load(2, 15, self.remat_base + id.0 as i32 * 8);
                Ok(())
            }
            _ => Err(unsupported!()),
        }
    }

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
                    self.deopt_source(&recipe.inputs[0], state)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(unsupported!());
                    }
                    self.deopt_source(&recipe.inputs[0], state)?;
                    self.asm.mov(3, 2);
                    self.deopt_source(&recipe.inputs[1], state)?;
                    if recipe.op == RematOp::FixnumAdd {
                        self.asm.add(2, 3);
                    } else {
                        self.asm.sub(3, 2);
                        self.asm.mov(2, 3);
                    }
                }
            }
            self.asm.store(2, 15, self.remat_base + index as i32 * 8);
        }
        let mut words = 0;
        for scope in &state.scopes {
            for header in [
                scope.function as u64,
                scope.bcp as u64,
                scope.locals.len() as u64,
                scope.stack.len() as u64,
            ] {
                self.asm.imm64(2, header);
                self.asm.store(2, 15, self.deopt_base + words * 8);
                words += 1;
            }
            for source in scope.locals.iter().chain(&scope.stack) {
                self.deopt_source(source, state)?;
                self.asm.store(2, 15, self.deopt_base + words * 8);
                words += 1;
            }
        }
        self.asm.imm64(2, state.scopes.len() as u64);
        self.asm.imm64(3, words as u64);
        self.asm.address(4, 15, self.deopt_base);
        self.asm.load(5, 15, self.entry_kind_offset);
        self.asm.imm64(1, callback);
        self.asm.call_reg(1);
        // The callback resumes T0 inline and returns the activation's result
        // (c2i_deopt_t2_inline, bliss-w6aki), or NIL with a transfer pending;
        // either way r2 is what this native level returns. Returning here
        // rather than continuing is what makes a deopt inside a direct
        // self-call level correct: the level above receives a finished value.
        self.epilogue();
        Ok(())
    }
}

/// Emit without ordinary runtime adapters. Calls decline compilation; guards
/// can still serialize virtual scopes for the precise T0 resume adapter.
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

/// Emit optimized System Z code with native roots synchronized through extra
/// activation slots at runtime calls. Precise guard exits serialize virtual
/// scopes for the T0 adapter, which must root the stream before allocating.
/// Every cycle crosses a sampled GC/signal poll before its edge transfers.
pub fn emit_framed_with_runtime(
    function: &Function,
    deopt_t2: u64,
    activation_slots: u16,
    runtime: RuntimeCalls,
) -> Result<FramedCode, EmitError> {
    // Backward edges need a signal/GC poll with synchronized native roots.
    // This also catches irreducible cycles without relying on OSR metadata.
    let positions: HashMap<_, _> = function
        .block_order()
        .iter()
        .enumerate()
        .map(|(index, &block)| (block, index))
        .collect();
    let mut polls = HashSet::new();
    for (index, &block) in function.block_order().iter().enumerate() {
        if function
            .succs(block)
            .iter()
            .any(|target| positions.get(target).is_none_or(|&target| target <= index))
        {
            if runtime.poll == 0 {
                return Err(unsupported!());
            }
            polls.insert(*function.block(block).insts.last().ok_or(unsupported!())?);
        }
    }
    let mut machine = super::lower::lower(function);
    super::regalloc::allocate_framed_s390x(&mut machine)
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
        stack_offset: 0,
        saved_fp_offset: 0,
        saved_sp_offset: 0,
        entry_kind_offset: 0,
        polls,
        transfer_exit: None,
        reg_entry: None,
        first_saved: 6,
        self_args_base: 0,
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
            // The allocator numbers r6..r12 upward; mirror them onto r12..r6
            // so the registers in use sit at the top of the callee-saved
            // range and the prologue saves only from the lowest one.
            Some(Location::Register(register)) => Home::Register(18 - register.encoding),
            Some(Location::Stack(slot)) => Home::Stack(slot.0),
            None => {
                let slot = spill_slots;
                spill_slots += 1;
                Home::Stack(slot)
            }
        };
        emitter.homes.insert(value, home);
    }
    emitter.first_saved = emitter
        .homes
        .values()
        .filter_map(|home| match home {
            Home::Register(register) => Some(*register),
            Home::Stack(_) => None,
        })
        .min()
        .unwrap_or(13)
        .min(13);
    // A value whose type is confined to non-pointer immediates (fixnum, single
    // float, character, symbol, NIL/T) can never be relocated by the moving GC,
    // so it needs no shadow root. The inference map assigns each SSA value the
    // type it has from its definition on, so the fact holds wherever the value
    // is live; BOTTOM (unreached) stays rooted. This mirrors emit.rs, and it is
    // what lets a fixnum-recursive function reach the direct self-call: its
    // roots vanish entirely.
    let inference = super::infer::infer(function);
    let proven_immediate = |value: Value| -> bool {
        const IMMEDIATE: TypeBits = TypeBits(
            TypeBits::FIXNUM.0
                | TypeBits::SINGLE_FLOAT.0
                | TypeBits::CHARACTER.0
                | TypeBits::SYMBOL.0
                | TypeBits::NULL.0,
        );
        let bits = inference.ty(value).bits;
        !bits.is_bottom() && bits.meet(IMMEDIATE) == bits
    };
    // The same precise regalloc2 ranges that choose native homes determine
    // roots at each actual runtime call. Include dying early arguments, and
    // exclude results that do not exist until the callback returns.
    for (index, inst) in machine.insts.iter().enumerate() {
        let Some(source) = inst.source_inst else {
            continue;
        };
        if !calls_runtime(function.inst(source).opcode) {
            continue;
        }
        let after = u32::try_from(index).map_err(|_| unsupported!())? * 2 + 1;
        let mut roots: Vec<Value> = machine
            .value_locations
            .iter()
            .filter(|range| {
                range.vreg.class == RegClass::Gpr && range.start <= after && after < range.end
            })
            .map(|range| range.vreg)
            .chain(inst.uses.iter().copied())
            .filter(|v| v.class == RegClass::Gpr && !inst.defs.contains(v))
            .map(|v| Value(v.num))
            .filter(|value| emitter.homes.contains_key(value))
            .filter(|value| !proven_immediate(*value))
            .collect();
        roots.sort_by_key(|value| value.0);
        roots.dedup();
        emitter.roots.insert(source, roots);
    }
    // Poll before the edge's parallel transfers. Lowering represents these
    // transfers as anonymous moves before the machine terminator; their
    // destination phi values do not exist yet in our emitted code.
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
    // Direct self-call eligibility (module docs). Self-call arguments never go
    // through the activation, so an eligible function needs no argument shadow
    // slots at all; nothing in its body then touches r13, which is what makes
    // the activation-less register entry sound.
    let entry_params = &function.block(function.entry()).params;
    let instructions = || {
        function
            .block_order()
            .iter()
            .flat_map(|&block| &function.block(block).insts)
            .map(|&inst| function.inst(inst))
    };
    // Self-calls pass their arguments in r2-r4; any other call stages them in
    // this frame's call-argument area (see the module docs).
    let is_self_call = |data: &InstData| matches!(data.aux, AuxData::CallTarget(symbol) if Some(symbol) == emitter.runtime.self_sym);
    let calls_fit_register_entry = instructions().all(|data| {
        data.opcode != Opcode::Call
            || (is_self_call(data) && data.args.len() <= 3)
            || (matches!(data.aux, AuxData::CallTarget(_)) && data.args.len() <= CALL_ARGS_MAX)
    });
    let other_call_words = instructions()
        .filter(|data| data.opcode == Opcode::Call && !is_self_call(data))
        .map(|data| data.args.len())
        .max()
        .unwrap_or(0);
    let has_reg_entry = emitter.runtime.self_sym.is_some()
        && std::env::var_os("EGCL_NO_DIRECT_SELF_CALL").is_none()
        && !function.is_variadic()
        && entry_params.len() <= 3
        && !entry_params
            .iter()
            .any(|&param| function.is_entry_param_checked(param))
        && emitter.root_slots == 0
        && calls_fit_register_entry
        && (other_call_words == 0 || emitter.runtime.call_slice_rooted != 0)
        && instructions().all(|data| data.opcode != Opcode::TakeValuesToLocals);
    if has_reg_entry {
        emitter.argument_slots = 0;
    }
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
    let self_args_words = if has_reg_entry {
        3.max(other_call_words)
    } else {
        0
    };
    let frame_words =
        spill_slots as usize + edge_words + deopt_words + remat_words + self_args_words + 6;
    // Every load/store and frame adjustment must fit a signed 20-bit address.
    if frame_words > (524280 - 160) / 8 {
        return Err(unsupported!());
    }
    emitter.frame_bytes = (frame_words * 8) as i32;
    emitter.edge_base = 160 + spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    emitter.remat_base = emitter.deopt_base + deopt_words as i32 * 8;
    emitter.self_args_base = emitter.remat_base + remat_words as i32 * 8;
    emitter.result_offset = 160 + (frame_words as i32 - 6) * 8;
    emitter.poll_offset = emitter.result_offset + 8;
    emitter.stack_offset = emitter.poll_offset + 8;
    emitter.saved_fp_offset = emitter.stack_offset + 8;
    emitter.saved_sp_offset = emitter.saved_fp_offset + 8;
    emitter.entry_kind_offset = emitter.saved_sp_offset + 8;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported!());
    }
    emitter.prologue();
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.asm.load(2, 13, index as i32 * 8);
        emitter.store(parameter, 2)?;
    }
    emitter.asm.branch(15, emitter.blocks[&function.entry()]);
    // The register entry's offset, published as FramedCode::compiled_entry so
    // the install log and its tests can see it (0 = no register entry).
    let mut compiled_entry = 0;
    if has_reg_entry {
        // Register entry: the same frame, but the arguments arrive in r2-r4 and
        // r13 stays the caller's. Move the arguments home BEFORE the poll word
        // is initialised, since that initialisation uses r2 as scratch.
        let label = emitter.asm.label();
        emitter.asm.bind(label);
        emitter.reg_entry = Some(label);
        compiled_entry = emitter.asm.here();
        emitter.asm.prologue_from(emitter.first_saved);
        emitter.asm.address(15, 15, -emitter.frame_bytes);
        for (index, &parameter) in entry_params.iter().enumerate() {
            emitter.store(parameter, 2 + index as u8)?;
        }
        // No frame of its own (r13 is the caller's): a deopt must push one.
        emitter.asm.imm64(2, emitter.runtime.code_id | (1 << 63));
        emitter.asm.store(2, 15, emitter.entry_kind_offset);
        emitter.init_poll_word();
        emitter.asm.branch(15, emitter.blocks[&function.entry()]);
    }
    let mut bcp_offsets = Vec::new();
    for &block in function.block_order() {
        emitter.asm.bind(emitter.blocks[&block]);
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
        // An entry must reconstruct every live native home, including values
        // defined above the loop. Constants are rematerialized by load().
        if live
            .iter()
            .any(|value| !imports.iter().any(|(_, imported)| imported == value))
        {
            continue;
        }
        let offset = emitter.asm.here();
        emitter.prologue();
        for (slot, value) in imports {
            emitter.asm.load(2, 13, slot as i32 * 8);
            emitter.store(value, 2)?;
        }
        emitter.asm.branch(15, emitter.blocks[&osr.block]);
        osr_entries.push((osr.bcp, offset));
    }
    let has_deopt = !emitter.deopts.is_empty();
    if let Some(exit) = emitter.transfer_exit {
        emitter.asm.bind(exit);
        emitter.asm.imm64(2, NIL.0);
        emitter.epilogue();
    }
    for (state, label) in std::mem::take(&mut emitter.deopts) {
        emitter.asm.bind(label);
        emitter.deopt(function.frame_states.get(state), deopt_t2)?;
    }
    Ok(FramedCode {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
    };
    use egcl_rt::value::EgclVal;

    fn add_one() -> Function {
        let mut f = Function::new("z-add-one");
        let block = f.entry();
        let x = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let instruction = |opcode, args, aux, frame_state| InstData {
            opcode,
            args,
            results: vec![],
            aux,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state,
            source_pos: 0,
        };
        let (_, one) = f.push_inst(
            block,
            instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1), None),
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let source = ValueSource::Value {
            value: x,
            repr: ValueRepresentation::Tagged,
        };
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 71,
                bcp: 9,
                locals: vec![source.clone()],
                stack: vec![source, ValueSource::Const(EgclVal::from_fixnum(1))],
            }],
            remat: vec![],
        });
        let (_, sum) = f.push_inst(
            block,
            instruction(
                Opcode::FixnumAdd,
                vec![x, one[0]],
                AuxData::None,
                Some(state),
            ),
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            block,
            instruction(Opcode::Return, vec![sum[0]], AuxData::None, None),
        );
        f
    }

    #[test]
    fn emits_system_z_with_precise_guard_metadata() {
        let code = emit_framed(&add_one(), 0, 3).unwrap();
        // STMG first,r15 into the caller's save area: first is whichever
        // callee-saved register the function uses lowest (r6 when it uses
        // them all), so only the shape is fixed, not the register.
        let first = code.code[1] >> 4;
        assert!((6..=13).contains(&first), "first saved register: r{first}");
        assert!(code.code.starts_with(&[
            0xeb,
            first << 4 | 0xf,
            0xf0,
            0x30 + 8 * (first - 6),
            0,
            0x24
        ]));
        assert!(code.has_deopt);
        assert_ne!(code.bcp_offsets[9], u32::MAX);
        assert_eq!(code.shadow_root_slots, 0);
    }

    #[test]
    fn calls_reload_relocated_register_and_spill_roots_without_losing_results() {
        thread_local! {
            static FRAME: std::cell::Cell<(*mut u64, usize)> = const { std::cell::Cell::new((std::ptr::null_mut(), 0)) };
        }
        extern "C" fn callback(_: u64, _: u64, _: *const EgclVal, _: u64) -> u64 {
            FRAME.with(|frame| {
                let (pointer, count) = frame.get();
                // Simulate a moving collector updating every scanned slot.
                for slot in unsafe { std::slice::from_raw_parts_mut(pointer, count) } {
                    if *slot == 0x4001 {
                        *slot = 0x5001;
                    }
                }
            });
            0x9001
        }
        for return_original in [true, false] {
            let mut f = Function::new("z-calling-roots");
            let block = f.entry();
            let args: Vec<_> = (0..20)
                .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
                .collect();
            let (_, result) = f.push_inst(
                block,
                InstData {
                    opcode: Opcode::Call,
                    args: args.clone(),
                    results: vec![],
                    aux: AuxData::CallTarget(71),
                    flags: InstFlags {
                        call: true,
                        safepoint: true,
                        effectful: true,
                        ..Default::default()
                    },
                    targets: vec![],
                    frame_state: None,
                    source_pos: 0,
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            f.set_terminator(
                block,
                InstData {
                    opcode: Opcode::Return,
                    args: vec![if return_original { args[0] } else { result[0] }],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: None,
                    source_pos: 0,
                },
            );
            let compiled = emit_framed_with_runtime(
                &f,
                0,
                20,
                RuntimeCalls {
                    call_slice: callback as *const () as u64,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(compiled.emitted_safepoints, 1);
            assert_eq!(compiled.root_sync_sites[0].live_roots, 20);
            assert!(compiled.root_sync_sites[0].spill_roots > 0);
            assert!(compiled.root_sync_sites[0].register_roots > 0);
            assert!(compiled.shadow_root_slots >= 40);
            #[cfg(all(target_arch = "s390x", unix))]
            {
                let code = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
                // SAFETY: the buffer owns the generated C-ABI entry, and the
                // activation includes every shadow slot advertised by it.
                let entry: unsafe extern "C" fn(*mut u64) -> u64 =
                    unsafe { std::mem::transmute(code.as_ptr()) };
                let mut slots = vec![0x4001; 20 + compiled.shadow_root_slots as usize];
                FRAME.with(|frame| frame.set((slots.as_mut_ptr(), slots.len())));
                assert_eq!(
                    unsafe { entry(slots.as_mut_ptr()) },
                    if return_original { 0x5001 } else { 0x9001 }
                );
                FRAME.with(|frame| frame.set((std::ptr::null_mut(), 0)));
            }
        }
    }

    #[test]
    fn declines_loops_without_a_bound_safepoint_poll() {
        let mut f = Function::new("z-needs-poll");
        let header = f.make_block();
        for block in [f.entry(), header] {
            f.set_terminator(
                block,
                InstData {
                    opcode: Opcode::Jump,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    targets: vec![BlockCall {
                        block: header,
                        args: vec![],
                    }],
                    frame_state: None,
                    source_pos: 0,
                },
            );
        }
        assert!(
            emit_framed(&f, 0, 0).is_err(),
            "must not install a loop that cannot reach a GC/signal poll"
        );
    }

    #[test]
    fn loops_poll_and_reload_moved_roots_before_the_next_backedge() {
        let mut f = Function::new("z-loop-roots");
        let entry = f.entry();
        let header = f.make_block();
        let inputs: Vec<_> = (0..20)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let carried: Vec<_> = (0..20)
            .map(|_| f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 73,
                bcp: 12,
                locals: carried
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(crate::t2::ir::OsrEntry {
            block: header,
            bcp: 12,
            frame_state: state,
        });
        for (block, args) in [(entry, inputs), (header, carried)] {
            f.set_terminator(
                block,
                InstData {
                    opcode: Opcode::Jump,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    targets: vec![BlockCall {
                        block: header,
                        args,
                    }],
                    frame_state: None,
                    source_pos: 0,
                },
            );
        }
        thread_local! {
            static POLL_FRAME: std::cell::Cell<(*mut u64, usize)> = const { std::cell::Cell::new((std::ptr::null_mut(), 0)) };
        }
        extern "C" fn poll() -> u64 {
            POLL_FRAME.with(|state| {
                let (frame, count) = state.get();
                for index in 0..20 {
                    unsafe {
                        assert_eq!(
                            *frame.add(20 + index),
                            0x1001 + index as u64 * 16 + count as u64 * 0x10000
                        );
                        *frame.add(20 + index) += 0x10000;
                    }
                }
                state.set((frame, count + 1));
                u64::from(count == 1)
            })
        }
        let compiled = emit_framed_with_runtime(
            &f,
            0,
            20,
            RuntimeCalls {
                poll: poll as *const () as usize as u64,
                ..RuntimeCalls::default()
            },
        )
        .expect("loop with a bound poll must compile");
        assert_eq!(compiled.osr_entries.len(), 1);
        assert_eq!(compiled.root_sync_sites.len(), 1);
        assert_eq!(compiled.root_sync_sites[0].live_roots, 20);
        #[cfg(target_arch = "s390x")]
        {
            let buffer = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
            for offset in [0, compiled.osr_entries[0].1] {
                let entry: extern "C" fn(*mut u64) -> u64 =
                    unsafe { std::mem::transmute(buffer.as_ptr().add(offset)) };
                let mut slots = vec![NIL.0; 20 + compiled.shadow_root_slots as usize];
                for (index, slot) in slots[..20].iter_mut().enumerate() {
                    *slot = 0x1001 + index as u64 * 16;
                }
                POLL_FRAME.with(|state| state.set((slots.as_mut_ptr(), 0)));
                assert_eq!(entry(slots.as_mut_ptr()), NIL.0);
                POLL_FRAME.with(|state| {
                    assert_eq!(state.get().1, 2);
                    state.set((std::ptr::null_mut(), 0));
                });
            }
        }
    }

    #[test]
    fn preserves_twenty_live_inputs_through_spilled_arithmetic() {
        let mut f = Function::new("z-pressure");
        let block = f.entry();
        let parameters: Vec<_> = (0..20)
            .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 72,
                bcp: 0,
                locals: parameters
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut sum = parameters[0];
        for &parameter in &parameters[1..] {
            let (_, result) = f.push_inst(
                block,
                InstData {
                    opcode: Opcode::FixnumAdd,
                    args: vec![sum, parameter],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: Some(state),
                    source_pos: 0,
                },
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            );
            sum = result[0];
        }
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Return,
                args: vec![sum],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        extern "C" fn deopt(_: u64, _: u64, _: *const u64, _: u64) -> u64 {
            NIL.0
        }
        let compiled = emit_framed(&f, deopt as *const () as u64, 20).unwrap();
        assert!(compiled.regalloc_spill_slots > 0);
        assert!(compiled.allocation_edits > 0);
        #[cfg(all(target_arch = "s390x", unix))]
        {
            let code = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
            // SAFETY: the emitted entry accepts these 20 tagged activation
            // slots and the live JIT buffer owns all executed instructions.
            let entry: unsafe extern "C" fn(*mut u64) -> u64 =
                unsafe { std::mem::transmute(code.as_ptr()) };
            let mut slots: Vec<_> = (1..=20).map(|n| EgclVal::from_fixnum(n).0).collect();
            assert_eq!(
                unsafe { entry(slots.as_mut_ptr()) },
                EgclVal::from_fixnum(210).0
            );
        }
    }

    fn binary_numeric(opcode: Opcode) -> Function {
        let mut f = Function::new("z-numeric");
        let block = f.entry();
        let values: Vec<_> = (0..2)
            .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let sources: Vec<_> = values
            .iter()
            .map(|&value| ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            })
            .collect();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 75,
                bcp: 5,
                locals: sources.clone(),
                stack: sources,
            }],
            remat: vec![],
        });
        let (_, result) = f.push_inst(
            block,
            InstData {
                opcode,
                args: values,
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Return,
                args: vec![result[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    #[test]
    fn multiplies_fixnums_with_exact_signed_overflow_guards() {
        thread_local! { static SAVED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) }; }
        extern "C" fn deopt(_: u64, words: u64, pointer: *const u64, _: u64) -> u64 {
            SAVED.with(|out| {
                *out.borrow_mut() =
                    unsafe { std::slice::from_raw_parts(pointer, words as usize) }.to_vec()
            });
            NIL.0
        }
        let compiled = emit_framed(
            &binary_numeric(Opcode::FixnumMul),
            deopt as *const () as usize as u64,
            2,
        )
        .expect("emit guarded multiply");
        assert!(compiled.has_deopt);
        #[cfg(target_arch = "s390x")]
        {
            let buffer = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
            let entry: extern "C" fn(*mut u64) -> u64 =
                unsafe { std::mem::transmute(buffer.as_ptr()) };
            let min = -(1_i64 << 60);
            let max = (1_i64 << 60) - 1;
            for lhs in [min, min + 1, -123456789, -2, -1, 0, 1, 2, 123456789, max] {
                for rhs in [min, min + 1, -2, -1, 0, 1, 2, max] {
                    SAVED.with(|out| out.borrow_mut().clear());
                    let mut slots = [EgclVal::from_fixnum(lhs).0, EgclVal::from_fixnum(rhs).0];
                    let product = i128::from(lhs) * i128::from(rhs);
                    let result = entry(slots.as_mut_ptr());
                    if (i128::from(min)..=i128::from(max)).contains(&product) {
                        assert_eq!(
                            result,
                            EgclVal::from_fixnum(product as i64).0,
                            "{lhs} * {rhs}"
                        );
                        SAVED.with(|out| assert!(out.borrow().is_empty()));
                    } else {
                        assert_eq!(result, NIL.0);
                        SAVED.with(|out| {
                            assert_eq!(
                                *out.borrow(),
                                [75, 5, 2, 2, slots[0], slots[1], slots[0], slots[1]]
                            )
                        });
                    }
                }
            }
        }
    }

    #[test]
    fn executes_tagged_single_float_arithmetic() {
        extern "C" fn deopt(_: u64, _: u64, _: *const u64, _: u64) -> u64 {
            NIL.0
        }
        for opcode in [Opcode::FloatAdd, Opcode::FloatSub, Opcode::FloatMul] {
            let compiled = emit_framed(
                &binary_numeric(opcode),
                deopt as *const () as usize as u64,
                2,
            )
            .expect("emit single-float arithmetic");
            assert!(compiled.has_deopt);
            #[cfg(target_arch = "s390x")]
            {
                let buffer = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
                let entry: extern "C" fn(*mut u64) -> u64 =
                    unsafe { std::mem::transmute(buffer.as_ptr()) };
                for lhs in [-9.5_f32, -0.0, 0.0, 1.25, f32::MIN_POSITIVE, f32::MAX] {
                    for rhs in [-2.0_f32, -0.0, 0.0, 2.5] {
                        let expected = match opcode {
                            Opcode::FloatAdd => lhs + rhs,
                            Opcode::FloatSub => lhs - rhs,
                            _ => lhs * rhs,
                        };
                        let mut slots = [
                            EgclVal::from_single_float(lhs).0,
                            EgclVal::from_single_float(rhs).0,
                        ];
                        assert_eq!(
                            entry(slots.as_mut_ptr()),
                            EgclVal::from_single_float(expected).0,
                            "{opcode:?}: {lhs} and {rhs}"
                        );
                    }
                }
                let mut wrong_type = [EgclVal::from_fixnum(2).0, NIL.0];
                assert_eq!(entry(wrong_type.as_mut_ptr()), NIL.0);
            }
        }
    }

    fn rematerialized_guard() -> (Function, FrameStateId) {
        use crate::t2::frame_state::{RematOp, RematRecipe, RematRecipeId};
        let mut f = Function::new("z-rematerialized-deopt");
        let block = f.entry();
        let values: Vec<_> = (0..21)
            .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let source = |value| ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        };
        // Forward references deliberately exercise dependency ordering. The
        // sum's twenty deopt-live inputs also force native spills.
        let mut recipes = vec![RematRecipe {
            op: RematOp::FixnumSub,
            inputs: vec![
                ValueSource::Remat(RematRecipeId(1)),
                ValueSource::Const(EgclVal::from_fixnum(10)),
            ],
            result_repr: ValueRepresentation::Tagged,
        }];
        for index in 0..19 {
            recipes.push(RematRecipe {
                op: RematOp::FixnumAdd,
                inputs: vec![
                    source(values[index]),
                    if index == 18 {
                        source(values[19])
                    } else {
                        ValueSource::Remat(RematRecipeId(index as u32 + 2))
                    },
                ],
                result_repr: ValueRepresentation::Tagged,
            });
        }
        recipes.push(RematRecipe {
            op: RematOp::Const,
            inputs: vec![ValueSource::Remat(RematRecipeId(0))],
            result_repr: ValueRepresentation::Tagged,
        });
        recipes.push(RematRecipe {
            op: RematOp::FixnumAdd,
            inputs: vec![
                ValueSource::Remat(RematRecipeId(20)),
                ValueSource::Remat(RematRecipeId(0)),
            ],
            result_repr: ValueRepresentation::Tagged,
        });
        let locals = vec![
            ValueSource::Remat(RematRecipeId(0)),
            ValueSource::Remat(RematRecipeId(21)),
        ];
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 74,
                bcp: 9,
                locals,
                stack: vec![source(values[20])],
            }],
            remat: recipes,
        });
        let (_, guarded) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::Guard,
                args: vec![values[20]],
                results: vec![],
                aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Return,
                args: vec![guarded[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        (f, state)
    }

    #[test]
    fn reconstructs_nested_shared_recipes_from_registers_and_spills() {
        thread_local! { static RECONSTRUCTED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) }; }
        extern "C" fn deopt(scopes: u64, words: u64, pointer: *const u64, _: u64) -> u64 {
            assert_eq!(scopes, 1);
            RECONSTRUCTED.with(|out| {
                *out.borrow_mut() =
                    unsafe { std::slice::from_raw_parts(pointer, words as usize) }.to_vec()
            });
            NIL.0
        }
        let (f, _) = rematerialized_guard();
        let compiled = emit_framed(&f, deopt as *const () as usize as u64, 21)
            .expect("emit rematerialized frame state");
        assert!(compiled.regalloc_spill_slots > 0);
        #[cfg(target_arch = "s390x")]
        {
            let buffer = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
            let entry: extern "C" fn(*mut u64) -> u64 =
                unsafe { std::mem::transmute(buffer.as_ptr()) };
            let mut slots: Vec<_> = (1..=20)
                .map(|n| EgclVal::from_fixnum(n).0)
                .chain([NIL.0])
                .collect();
            assert_eq!(entry(slots.as_mut_ptr()), NIL.0);
            let mut expected = vec![74, 9, 2, 1];
            expected.extend([
                EgclVal::from_fixnum(200).0,
                EgclVal::from_fixnum(400).0,
                NIL.0,
            ]);
            RECONSTRUCTED.with(|out| assert_eq!(*out.borrow(), expected));
        }
    }

    #[test]
    fn rejects_invalid_rematerialization_dependencies() {
        use crate::t2::frame_state::RematRecipeId;
        let (mut f, state) = rematerialized_guard();
        f.frame_states.get_mut(state).remat[0].inputs[0] = ValueSource::Remat(RematRecipeId(0));
        assert!(
            emit_framed(&f, 0, 21).is_err(),
            "cyclic recipe must decline"
        );
        f.frame_states.get_mut(state).remat[0].inputs[0] = ValueSource::Remat(RematRecipeId(999));
        assert!(
            emit_framed(&f, 0, 21).is_err(),
            "missing recipe must decline"
        );
    }

    #[cfg(all(target_arch = "s390x", unix))]
    #[test]
    fn executes_optimized_add_and_reconstructs_failed_guards() {
        thread_local! {
            static DEOPT: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        extern "C" fn deopt(scopes: u64, words: u64, pointer: *const u64, _: u64) -> u64 {
            let values = unsafe { std::slice::from_raw_parts(pointer, words as usize) };
            DEOPT.with(|out| {
                let mut out = out.borrow_mut();
                out.push(scopes);
                out.extend_from_slice(values);
            });
            NIL.0
        }
        let compiled = emit_framed(&add_one(), deopt as *const () as u64, 3).unwrap();
        let code = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
        // SAFETY: emit_framed's entry uses the System Z C ABI and accesses
        // only the supplied activation slots; the JIT buffer owns its code.
        let entry: unsafe extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr()) };
        let mut slots = [EgclVal::from_fixnum(41).0, 0, 0];
        assert_eq!(
            unsafe { entry(slots.as_mut_ptr()) },
            EgclVal::from_fixnum(42).0
        );
        DEOPT.with(|out| assert!(out.borrow().is_empty()));
        for input in [
            EgclVal::from_fixnum((1_i64 << 60) - 1).0,
            egcl_rt::value::NIL.0,
        ] {
            slots[0] = input;
            assert_eq!(unsafe { entry(slots.as_mut_ptr()) }, egcl_rt::value::NIL.0);
            DEOPT.with(|out| {
                assert_eq!(*out.borrow(), [1, 71, 9, 1, 2, input, input, 8]);
                out.borrow_mut().clear();
            });
        }
    }
}
