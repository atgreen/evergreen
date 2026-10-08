// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! riscv64 machine-code emission for the T2 framed pipeline.
//!
//! Structured identically to [`super::emit_s390x`], whose module
//! documentation describes the activation contract, the home-based emission
//! strategy over regalloc2's ranges, root synchronisation at runtime calls,
//! sampled back-edge polls, OSR prologues and deopt exits. Only the
//! architecture differs:
//!
//! | role                                          | register         |
//! |-----------------------------------------------|------------------|
//! | primary working / first argument / result     | a0               |
//! | secondary working / second argument           | a1               |
//! | third and fourth arguments                    | a2, a3           |
//! | address scratch (helper targets)              | t0               |
//! | temporaries (guards, overflow, multiply)      | t1, t2, t3       |
//! | activation (frame-slots) pointer, whole body  | s2               |
//! | value homes (regalloc2 pool)                  | s1, s3–s11       |
//! | stack pointer                                 | sp               |
//!
//! `PhysReg.encoding` is the architectural register number on this target.
//!
//! # Frame
//!
//! The prologue drops `sp` by `frame_bytes + 96`, keeping 16-byte alignment,
//! then saves ra, s1 and s2–s11 in the top 96 bytes. Below them, from `sp`
//! upwards: spill slots (regalloc2's plus the stable homes added for split
//! values), the edge parallel-copy buffer, the deopt serialisation buffer,
//! the remat buffer, and two words holding the saved call result and the
//! poll countdown. Displacements beyond 12 bits go through the assembler's
//! `t6`, so an oversized frame costs instructions, not a decline.
//!
//! # Arithmetic
//!
//! RISC-V has no condition codes, so fixnum add and subtract compute signed
//! overflow explicitly (`(sum ^ a) & (sum ^ b) < 0`), multiply compares
//! `mulh` against the sign of `mul`, and comparisons branch directly into a
//! T/NIL select. Single-float templates move the high 32 bits of the tagged
//! word through `fmv.w.x`/`fmv.x.w` and rebuild the tag with `slli`/`ori`.

use super::emit::{EmitError, FramedCode, RootSyncSite};
use super::emit_s390x::rematerialization_order;
use super::frame_state::{FrameState, FrameStateId, RematOp, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, Inst, InstData, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use super::mach::{Location, RegClass};
use egcl_rt::asm_riscv64::{
    Asm, Cond, Label,
    reg::{A0, A1, A2, A3, FA0, FA1, RA, SP, T0, T1, T2, T3, ZERO},
};
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};
use std::collections::{HashMap, HashSet};

/// C-ABI runtime adapters used by optimized code. `transfer_pending` is a
/// nonallocating leaf; the primary result is temporarily unrooted during it.
#[derive(Clone, Copy, Default)]
pub struct RuntimeCalls {
    pub call_slice: u64,
    pub load_global: u64,
    pub load_function: u64,
    pub store_global: u64,
    pub multiple_values: u64,
    pub transfer_pending: u64,
    /// Safepoint callback: nonzero requests immediate native exit.
    pub poll: u64,
}

/// The activation pointer for the whole body.
const SLOTS: u8 = 18;
/// Callee-saved registers the prologue preserves: ra, s1, s2–s11.
const SAVED: [u8; 12] = [RA, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];
const SAVE_BYTES: i32 = 8 * SAVED.len() as i32;

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
    polls: HashSet<Inst>,
    transfer_exit: Option<Label>,
}

fn unsupported() -> EmitError {
    EmitError::UnsupportedOp(0xf364)
}

impl Emitter<'_> {
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
        // Clear unused shadows too: an earlier call may have had more live
        // roots or arguments. Dead objects need not be retained indefinitely.
        self.asm.imm64(A0, NIL.0);
        for index in 0..u32::from(self.root_slots) + u32::from(self.argument_slots) {
            self.asm.store(A0, SLOTS, root_base + index as i32 * 8);
        }
        for (index, &value) in roots.iter().enumerate() {
            self.load(value, A0)?;
            self.asm.store(A0, SLOTS, root_base + index as i32 * 8);
        }
        let helper = if let Some(data) = data {
            match data.opcode {
                Opcode::Call => {
                    let AuxData::CallTarget(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    for (index, &arg) in data.args.iter().enumerate() {
                        self.load(arg, A0)?;
                        self.asm.store(A0, SLOTS, argument_base + index as i32 * 8);
                    }
                    self.asm.imm64(A0, u64::from(symbol));
                    self.asm.imm64(A1, data.args.len() as u64);
                    self.asm.address(A2, SLOTS, argument_base);
                    self.asm.imm64(A3, 0);
                    self.runtime.call_slice
                }
                Opcode::SymbolValue | Opcode::SymbolFunction | Opcode::SetSymbolValue => {
                    let AuxData::SymbolRef(symbol) = data.aux else {
                        return Err(unsupported());
                    };
                    if data.opcode == Opcode::SetSymbolValue {
                        self.load(*data.args.first().ok_or_else(unsupported)?, A1)?;
                    }
                    self.asm.imm64(A0, u64::from(symbol));
                    match data.opcode {
                        Opcode::SymbolValue => self.runtime.load_global,
                        Opcode::SymbolFunction => self.runtime.load_function,
                        _ => self.runtime.store_global,
                    }
                }
                Opcode::ClearMv => {
                    self.asm.imm64(A0, NIL.0);
                    self.asm.imm64(A1, 0);
                    self.asm.imm64(A2, 0);
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
                    self.load(*data.args.first().ok_or_else(unsupported)?, A0)?;
                    self.asm.address(A1, SLOTS, i32::from(slot_base) * 8);
                    self.asm.imm64(A2, u64::from(nvars));
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
        self.asm.imm64(T0, helper);
        self.asm.call_reg(T0);
        self.asm.store(A0, SP, self.result_offset);
        if data.is_none() || self.runtime.transfer_pending != 0 {
            if data.is_some() {
                self.asm.imm64(T0, self.runtime.transfer_pending);
                self.asm.call_reg(T0);
            }
            let exit = *self.transfer_exit.get_or_insert_with(|| self.asm.label());
            self.asm.branch(Cond::Ne, A0, ZERO, exit);
        }
        // Restore roots before assigning results. A dying argument may share
        // its home with the call result, but must never overwrite that result.
        for (index, &value) in roots.iter().enumerate() {
            self.asm.load(A0, SLOTS, root_base + index as i32 * 8);
            self.store(value, A0)?;
        }
        if let Some(data) = data {
            if let AuxData::ValuesLocals { slot_base, .. } = data.aux {
                for (index, &value) in data.results.iter().enumerate() {
                    self.asm
                        .load(A0, SLOTS, (i32::from(slot_base) + index as i32) * 8);
                    self.store(value, A0)?;
                }
            } else if let Some(&result) = data.results.first() {
                self.asm.load(A0, SP, self.result_offset);
                self.store(result, A0)?;
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

    fn load(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        if let Some(bits) = self.constants.get(&value) {
            self.asm.imm64(register, *bits);
        } else if let Some(slot) = self.heap_constants.get(&value) {
            self.asm.imm64(T0, *slot as u64);
            self.asm.load(register, T0, 0);
        } else {
            match self.homes.get(&value).ok_or_else(unsupported)? {
                Home::Register(source) => self.asm.mov(register, *source),
                Home::Stack(slot) => self.asm.load(register, SP, (*slot as i32) * 8),
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        match self.homes.get(&value).ok_or_else(unsupported)? {
            Home::Register(destination) => self.asm.mov(*destination, register),
            Home::Stack(slot) => self.asm.store(register, SP, (*slot as i32) * 8),
        }
        Ok(())
    }

    fn prologue(&mut self) {
        self.asm.address(SP, SP, -(self.frame_bytes + SAVE_BYTES));
        for (index, &register) in SAVED.iter().enumerate() {
            self.asm
                .store(register, SP, self.frame_bytes + 8 * index as i32);
        }
        self.asm.mov(SLOTS, A0);
        self.asm.imm64(A1, 256);
        self.asm.store(A1, SP, self.poll_offset);
    }

    fn epilogue(&mut self) {
        for (index, &register) in SAVED.iter().enumerate() {
            self.asm
                .load(register, SP, self.frame_bytes + 8 * index as i32);
        }
        self.asm.address(SP, SP, self.frame_bytes + SAVE_BYTES);
        self.asm.ret();
    }

    fn deopt_label(&mut self, data: &InstData) -> Result<Label, EmitError> {
        let state = data.frame_state.ok_or_else(unsupported)?;
        if self.function.frame_states.get(state).scopes.is_empty() {
            return Err(unsupported());
        }
        let label = self.asm.label();
        self.deopts.push((state, label));
        Ok(label)
    }

    fn guard_fixnum(&mut self, register: u8, deopt: Label) {
        self.asm.and_imm(T1, register, 7);
        self.asm.branch(Cond::Ne, T1, ZERO, deopt);
    }

    fn guard_single_float(&mut self, register: u8, deopt: Label) {
        self.asm.and_imm(T1, register, 7);
        self.asm.add_imm(T2, ZERO, 4);
        self.asm.branch(Cond::Ne, T1, T2, deopt);
    }

    fn guard_cons(&mut self, register: u8, deopt: Label) {
        self.asm.and_imm(T1, register, 7);
        self.asm.imm64(T2, egcl_rt::value::TAG_CONS);
        self.asm.branch(Cond::Ne, T1, T2, deopt);
    }

    /// `a0 = a0 + a1`, deopting on signed overflow, which on a flagless ISA
    /// is "both inputs disagree with the sum in sign". The sum is kept in a
    /// temporary until the check passes so the exit sees the operands.
    fn add_overflow(&mut self, deopt: Label) {
        self.asm.add(T1, A0, A1);
        self.asm.xor(T2, T1, A0);
        self.asm.xor(T3, T1, A1);
        self.asm.and(T2, T2, T3);
        self.asm.branch(Cond::Lt, T2, ZERO, deopt);
        self.asm.mov(A0, T1);
    }

    /// `a0 = a0 - a1`, deopting when the inputs differ in sign and the
    /// result's sign differs from the minuend's.
    fn sub_overflow(&mut self, deopt: Label) {
        self.asm.sub(T1, A0, A1);
        self.asm.xor(T2, A0, A1);
        self.asm.xor(T3, A0, T1);
        self.asm.and(T2, T2, T3);
        self.asm.branch(Cond::Lt, T2, ZERO, deopt);
        self.asm.mov(A0, T1);
    }

    /// `a0 = select(a0 cond a1, T, NIL)`.
    fn boolean(&mut self, cond: Cond) {
        let yes = self.asm.label();
        let done = self.asm.label();
        self.asm.branch(cond, A0, A1, yes);
        self.asm.imm64(A0, NIL.0);
        self.asm.jump(done);
        self.asm.bind(yes);
        self.asm.imm64(A0, T.0);
        self.asm.bind(done);
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
        // Untag the lhs and multiply by the tagged rhs, giving the tagged
        // product. The signed 128-bit product fits 64 bits exactly when its
        // high half equals the sign extension of the low half. No allocated
        // home is modified before the overflow exit, so deopt retains both
        // inputs.
        self.asm.shift_right_signed(T1, A0, 3);
        self.asm.mul(T2, T1, A1);
        self.asm.mulh(T3, T1, A1);
        self.asm.shift_right_signed(T1, T2, 63);
        self.asm.branch(Cond::Ne, T3, T1, deopt);
        self.asm.mov(A0, T2);
    }

    fn edge(&mut self, edge: &BlockCall) -> Result<(), EmitError> {
        let parameters = self.function.block(edge.block).params.clone();
        if parameters.len() != edge.args.len() {
            return Err(unsupported());
        }
        // A parallel copy through private native slots handles cycles and
        // overlapping register/spill homes without clobbering an edge input.
        for (index, &source) in edge.args.iter().enumerate() {
            self.load(source, A0)?;
            self.asm.store(A0, SP, self.edge_base + index as i32 * 8);
        }
        for (index, destination) in parameters.into_iter().enumerate() {
            self.asm.load(A0, SP, self.edge_base + index as i32 * 8);
            self.store(destination, A0)?;
        }
        self.asm.jump(self.blocks[&edge.block]);
        Ok(())
    }

    fn instruction(&mut self, instruction: Inst, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
        if self.polls.contains(&instruction) {
            let skip = self.asm.label();
            self.asm.load(A0, SP, self.poll_offset);
            self.asm.add_imm(A0, A0, -1);
            self.asm.store(A0, SP, self.poll_offset);
            self.asm.branch(Cond::Ne, A0, ZERO, skip);
            self.asm.imm64(A0, 256);
            self.asm.store(A0, SP, self.poll_offset);
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
                    self.load(value, A0)?;
                } else {
                    self.asm.imm64(A0, NIL.0);
                }
                self.epilogue();
                return Ok(());
            }
            Jump if data.targets.len() == 1 => return self.edge(&data.targets[0]),
            Brif if data.args.len() == 1 && data.targets.len() == 2 => {
                let otherwise = self.asm.label();
                self.load(data.args[0], A0)?;
                self.asm.imm64(A1, NIL.0);
                self.asm.branch(Cond::Eq, A0, A1, otherwise);
                self.edge(&data.targets[0])?;
                self.asm.bind(otherwise);
                return self.edge(&data.targets[1]);
            }
            _ => {}
        }
        let result = *data.results.first().ok_or_else(unsupported)?;
        let first = *data.args.first().ok_or_else(unsupported)?;
        self.load(first, A0)?;
        match data.opcode {
            Guard if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM || ty.bits == TypeBits::SINGLE_FLOAT || ty.bits == TypeBits::CONS) =>
            {
                // CONS matters as much as the numeric tags: build.rs emits a
                // separate Guard(TypeTag(CONS)) ahead of every Car/Cdr, so
                // refusing it would cost the whole function its T2 code for
                // any use of CAR or CDR (bliss-2yews).
                let deopt = self.deopt_label(data)?;
                match &data.aux {
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM => {
                        self.guard_fixnum(A0, deopt)
                    }
                    AuxData::TypeTag(ty) if ty.bits == TypeBits::CONS => self.guard_cons(A0, deopt),
                    _ => self.guard_single_float(A0, deopt),
                }
            }
            FloatAdd | FloatSub | FloatMul => {
                let deopt = self.deopt_label(data)?;
                self.float_operand(first, A0, deopt)?;
                self.float_operand(*data.args.get(1).ok_or_else(unsupported)?, A1, deopt)?;
                // The single's bits are the high half of the tagged word.
                self.asm.shift_right(T1, A0, 32);
                self.asm.float_from_bits(FA0, T1);
                self.asm.shift_right(T1, A1, 32);
                self.asm.float_from_bits(FA1, T1);
                match data.opcode {
                    FloatAdd => self.asm.add_single(FA0, FA0, FA1),
                    FloatSub => self.asm.sub_single(FA0, FA0, FA1),
                    _ => self.asm.multiply_single(FA0, FA0, FA1),
                }
                // fmv.x.w sign-extends bit 31; the shift discards that.
                self.asm.bits_from_float(A0, FA0);
                self.asm.shift_left(A0, A0, 32);
                self.asm.or_imm(A0, A0, 4);
            }
            FixnumAdd | FixnumSub | FixnumMul | FixnumNeg | FixnumCmpEq | FixnumCmpLt
            | FixnumCmpLe | FixnumCmpGt | FixnumCmpGe => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(A0, deopt);
                if data.opcode != FixnumNeg {
                    self.load(*data.args.get(1).ok_or_else(unsupported)?, A1)?;
                    self.guard_fixnum(A1, deopt);
                }
                match data.opcode {
                    FixnumAdd => self.add_overflow(deopt),
                    FixnumSub => self.sub_overflow(deopt),
                    FixnumMul => self.multiply_fixnums(deopt),
                    FixnumNeg => {
                        self.asm.mov(A1, A0);
                        self.asm.mov(A0, ZERO);
                        self.sub_overflow(deopt);
                    }
                    comparison => {
                        self.boolean(match comparison {
                            FixnumCmpEq => Cond::Eq,
                            FixnumCmpLt => Cond::Lt,
                            FixnumCmpLe => Cond::Le,
                            FixnumCmpGt => Cond::Gt,
                            FixnumCmpGe => Cond::Ge,
                            _ => unreachable!(),
                        });
                    }
                }
            }
            GenericEq => {
                // Lisp EQ (and NULL / NOT, which speculate.rs rewrites to
                // GenericEq(x, NIL)) is bit-equality of the two tagged words.
                // No guard: the operands may be any tagged values and the
                // rewrite carries no frame state (bliss-pig52).
                self.load(*data.args.get(1).ok_or_else(unsupported)?, A1)?;
                self.boolean(Cond::Eq);
            }
            Car | Cdr => {
                // A cons cell is HEADERLESS and its tagged pointer differs from
                // the cell address only in the low three bits, so masking them
                // off gives the base and the two slots sit at +0 and +8. The
                // guarded form is refused as on every other target: this is
                // the fast path for a cons whose type the IR has proven.
                if data.flags.guard || data.flags.effectful || data.frame_state.is_some() {
                    return Err(unsupported());
                }
                self.asm.and_imm(A0, A0, -8);
                let offset = if data.opcode == Car { 0 } else { 8 };
                self.asm.load(A0, A0, offset);
            }
            LogAnd | LogOr | LogXor => {
                // Exact on the tagged representation: both operands carry the
                // same zero tag, so tagged(a) OP tagged(b) = tagged(a OP b).
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(A0, deopt);
                self.load(*data.args.get(1).ok_or_else(unsupported)?, A1)?;
                self.guard_fixnum(A1, deopt);
                match data.opcode {
                    LogAnd => self.asm.and(A0, A0, A1),
                    LogOr => self.asm.or(A0, A0, A1),
                    _ => self.asm.xor(A0, A0, A1),
                }
            }
            LogNot => {
                // ~(x<<3) has its low three bits set; clearing them yields
                // (~x)<<3, which is tagged(~x).
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(A0, deopt);
                self.asm.xor_imm(A0, A0, -1);
                self.asm.and_imm(A0, A0, -8);
            }
            FixnumShl | FixnumShr => {
                // (ash x n) for a CONSTANT n; a variable amount declines. The
                // constants map holds TAGGED bits, so the shift count is
                // recovered by untagging it.
                let deopt = self.deopt_label(data)?;
                let amount = *data.args.get(1).ok_or_else(unsupported)?;
                let tagged = *self.constants.get(&amount).ok_or_else(unsupported)?;
                let n = (tagged as i64) >> 3;
                self.guard_fixnum(A0, deopt);
                // DIRECTION, mirroring emit.rs: on FixnumShl a NEGATIVE
                // constant is a RIGHT shift, because that is how `(ash x -2)`
                // lowers. FixnumShr is always a right shift (bliss-ys4ha).
                let shift_right = if data.opcode == FixnumShr {
                    Some(n.max(0))
                } else if n < 0 {
                    Some(-n)
                } else {
                    None
                };
                if let Some(right) = shift_right {
                    // Untag, shift arithmetically, retag. Cannot overflow.
                    // Saturated at 60: a 61-bit payload shifted further only
                    // ever yields 0 or -1, and the amount must stay below 64.
                    let k = right.min(60) as u8;
                    self.asm.shift_right_signed(A0, A0, 3 + k);
                    self.asm.shift_left(A0, A0, 3);
                } else {
                    // tagged(x)<<n = tagged(x<<n) unless it leaves fixnum
                    // range; shift back and compare to detect that, leaving
                    // a0 untouched until the check passes.
                    if n > 60 {
                        return Err(unsupported());
                    }
                    let k = n as u8;
                    self.asm.shift_left(A1, A0, k);
                    self.asm.shift_right_signed(A2, A1, k);
                    self.asm.branch(Cond::Ne, A2, A0, deopt);
                    self.asm.mov(A0, A1);
                }
            }
            _ => return Err(unsupported()),
        }
        // Do not overwrite any allocated home until every guard has passed:
        // a failing instruction must reconstruct its pre-instruction state.
        self.store(result, A0)
    }

    fn deopt_source(&mut self, source: &ValueSource, state: &FrameState) -> Result<(), EmitError> {
        match source {
            ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            } => self.load(*value, A0),
            ValueSource::Const(value)
                if !value.is_heap_object() && !value.is_cons() && !value.is_function() =>
            {
                self.asm.imm64(A0, value.0);
                Ok(())
            }
            ValueSource::Unbound => {
                self.asm.imm64(A0, UNBOUND.0);
                Ok(())
            }
            ValueSource::Remat(id) if (id.0 as usize) < state.remat.len() => {
                self.asm.load(A0, SP, self.remat_base + id.0 as i32 * 8);
                Ok(())
            }
            _ => Err(unsupported()),
        }
    }

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
                    self.deopt_source(&recipe.inputs[0], state)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(unsupported());
                    }
                    self.deopt_source(&recipe.inputs[0], state)?;
                    self.asm.mov(A1, A0);
                    self.deopt_source(&recipe.inputs[1], state)?;
                    if recipe.op == RematOp::FixnumAdd {
                        self.asm.add(A0, A1, A0);
                    } else {
                        self.asm.sub(A0, A1, A0);
                    }
                }
            }
            self.asm.store(A0, SP, self.remat_base + index as i32 * 8);
        }
        let mut words = 0;
        for scope in &state.scopes {
            for header in [
                scope.function as u64,
                scope.bcp as u64,
                scope.locals.len() as u64,
                scope.stack.len() as u64,
            ] {
                self.asm.imm64(A0, header);
                self.asm.store(A0, SP, self.deopt_base + words * 8);
                words += 1;
            }
            for source in scope.locals.iter().chain(&scope.stack) {
                self.deopt_source(source, state)?;
                self.asm.store(A0, SP, self.deopt_base + words * 8);
                words += 1;
            }
        }
        self.asm.imm64(A0, state.scopes.len() as u64);
        self.asm.imm64(A1, words as u64);
        self.asm.address(A2, SP, self.deopt_base);
        self.asm.imm64(A3, 0);
        self.asm.imm64(T0, callback);
        self.asm.call_reg(T0);
        self.asm.imm64(A0, NIL.0);
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

/// Emit optimized riscv64 code with native roots synchronized through extra
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
                return Err(unsupported());
            }
            polls.insert(*function.block(block).insts.last().ok_or_else(unsupported)?);
        }
    }
    let mut machine = super::lower::lower(function);
    super::regalloc::allocate_framed_riscv64(&mut machine)
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
        let after = u32::try_from(index).map_err(|_| unsupported())? * 2 + 1;
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
        let point = u32::try_from(boundary).map_err(|_| unsupported())? * 2;
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
    // Two words for the saved call result and the poll countdown, then round
    // to keep sp 16-byte aligned (the save area is 96 bytes, itself aligned).
    let frame_words = (spill_slots as usize + edge_words + deopt_words + remat_words + 2 + 1) & !1;
    // Every displacement must fit the assembler's 32-bit wide-offset path.
    if frame_words > (i32::MAX as usize - SAVE_BYTES as usize) / 8 {
        return Err(unsupported());
    }
    emitter.frame_bytes = (frame_words * 8) as i32;
    emitter.edge_base = spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    emitter.remat_base = emitter.deopt_base + deopt_words as i32 * 8;
    emitter.result_offset = emitter.remat_base + remat_words as i32 * 8;
    emitter.poll_offset = emitter.result_offset + 8;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported());
    }
    emitter.prologue();
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.asm.load(A0, SLOTS, index as i32 * 8);
        emitter.store(parameter, A0)?;
    }
    emitter.asm.jump(emitter.blocks[&function.entry()]);
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
            emitter.asm.load(A0, SLOTS, slot as i32 * 8);
            emitter.store(value, A0)?;
        }
        emitter.asm.jump(emitter.blocks[&osr.block]);
        osr_entries.push((osr.bcp, offset));
    }
    let has_deopt = !emitter.deopts.is_empty();
    if let Some(exit) = emitter.transfer_exit {
        emitter.asm.bind(exit);
        emitter.asm.imm64(A0, NIL.0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
    };
    use egcl_rt::value::EgclVal;

    fn add_one() -> Function {
        let mut f = Function::new("rv-add-one");
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
    fn emits_riscv64_with_precise_guard_metadata() {
        let code = emit_framed(&add_one(), 0, 3).unwrap();
        // The prologue opens with `addi sp, sp, -N` for a 16-byte-aligned N.
        let first = u32::from_le_bytes(code.code[..4].try_into().unwrap());
        assert_eq!(first & 0xfffff, 0x10113, "addi sp, sp, imm");
        let imm = (first as i32) >> 20;
        assert!(imm < 0 && imm % 16 == 0, "{imm}");
        assert_eq!(code.code.len() % 4, 0);
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
            let mut f = Function::new("rv-calling-roots");
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
            #[cfg(all(target_arch = "riscv64", unix))]
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
        let mut f = Function::new("rv-needs-poll");
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
        let mut f = Function::new("rv-loop-roots");
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
        #[cfg(target_arch = "riscv64")]
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
        let mut f = Function::new("rv-pressure");
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
        extern "C" fn deopt(_: u64, _: u64, _: *const u64, _: u64) {}
        let compiled = emit_framed(&f, deopt as *const () as u64, 20).unwrap();
        assert!(compiled.regalloc_spill_slots > 0);
        assert!(compiled.allocation_edits > 0);
        #[cfg(all(target_arch = "riscv64", unix))]
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
        let mut f = Function::new("rv-numeric");
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
        extern "C" fn deopt(_: u64, words: u64, pointer: *const u64, _: u64) {
            SAVED.with(|out| {
                *out.borrow_mut() =
                    unsafe { std::slice::from_raw_parts(pointer, words as usize) }.to_vec()
            });
        }
        let compiled = emit_framed(
            &binary_numeric(Opcode::FixnumMul),
            deopt as *const () as usize as u64,
            2,
        )
        .expect("emit guarded multiply");
        assert!(compiled.has_deopt);
        #[cfg(target_arch = "riscv64")]
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
        extern "C" fn deopt(_: u64, _: u64, _: *const u64, _: u64) {}
        for opcode in [Opcode::FloatAdd, Opcode::FloatSub, Opcode::FloatMul] {
            let compiled = emit_framed(
                &binary_numeric(opcode),
                deopt as *const () as usize as u64,
                2,
            )
            .expect("emit single-float arithmetic");
            assert!(compiled.has_deopt);
            #[cfg(target_arch = "riscv64")]
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
        let mut f = Function::new("rv-rematerialized-deopt");
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
        extern "C" fn deopt(scopes: u64, words: u64, pointer: *const u64, _: u64) {
            assert_eq!(scopes, 1);
            RECONSTRUCTED.with(|out| {
                *out.borrow_mut() =
                    unsafe { std::slice::from_raw_parts(pointer, words as usize) }.to_vec()
            });
        }
        let (f, _) = rematerialized_guard();
        let compiled = emit_framed(&f, deopt as *const () as usize as u64, 21)
            .expect("emit rematerialized frame state");
        assert!(compiled.regalloc_spill_slots > 0);
        #[cfg(target_arch = "riscv64")]
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

    #[cfg(all(target_arch = "riscv64", unix))]
    #[test]
    fn executes_optimized_add_and_reconstructs_failed_guards() {
        thread_local! {
            static DEOPT: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        extern "C" fn deopt(scopes: u64, words: u64, pointer: *const u64, _: u64) {
            let values = unsafe { std::slice::from_raw_parts(pointer, words as usize) };
            DEOPT.with(|out| {
                let mut out = out.borrow_mut();
                out.push(scopes);
                out.extend_from_slice(values);
            });
        }
        let compiled = emit_framed(&add_one(), deopt as *const () as u64, 3).unwrap();
        let code = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
        // SAFETY: emit_framed's entry uses the LP64 C ABI and accesses only
        // the supplied activation slots; the JIT buffer owns its code.
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
