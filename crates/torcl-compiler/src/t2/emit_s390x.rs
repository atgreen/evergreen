//! System Z emission for the optimized SSA pipeline.

use super::emit::{EmitError, FramedCode};
use super::frame_state::{FrameState, FrameStateId, ValueSource};
use super::ir::{
    AuxData, Block, BlockCall, Function, InstData, Opcode, TypeBits, Value, ValueRepresentation,
};
use super::mach::{Location, RegClass};
use std::collections::HashMap;
use torcl_rt::asm_s390x::{Asm, Label};
use torcl_rt::value::{NIL, T, TorclVal, UNBOUND};

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
}

fn unsupported() -> EmitError {
    EmitError::UnsupportedOp(0x390)
}

impl Emitter<'_> {
    fn load(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        if let Some(bits) = self.constants.get(&value) {
            self.asm.imm64(register, *bits);
        } else if let Some(slot) = self.heap_constants.get(&value) {
            self.asm.imm64(1, *slot as u64);
            self.asm.load(register, 1, 0);
        } else {
            match self.homes.get(&value).ok_or_else(unsupported)? {
                Home::Register(source) => self.asm.mov(register, *source),
                Home::Stack(slot) => self.asm.load(register, 15, 160 + (*slot as i32) * 8),
            }
        }
        Ok(())
    }

    fn store(&mut self, value: Value, register: u8) -> Result<(), EmitError> {
        match self.homes.get(&value).ok_or_else(unsupported)? {
            Home::Register(destination) => self.asm.mov(*destination, register),
            Home::Stack(slot) => self.asm.store(register, 15, 160 + (*slot as i32) * 8),
        }
        Ok(())
    }

    fn prologue(&mut self) {
        self.asm.prologue();
        self.asm.address(15, 15, -self.frame_bytes);
        self.asm.mov(13, 2);
    }

    fn epilogue(&mut self) {
        self.asm.address(15, 15, self.frame_bytes);
        self.asm.epilogue();
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
        self.asm.mov(4, register);
        self.asm.imm64(5, 7);
        self.asm.and(4, 5);
        self.asm.branch(6, deopt);
    }

    fn edge(&mut self, edge: &BlockCall) -> Result<(), EmitError> {
        let parameters = self.function.block(edge.block).params.clone();
        if parameters.len() != edge.args.len() {
            return Err(unsupported());
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

    fn instruction(&mut self, data: &InstData) -> Result<(), EmitError> {
        use Opcode::*;
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
            _ => {}
        }
        let result = *data.results.first().ok_or_else(unsupported)?;
        let first = *data.args.first().ok_or_else(unsupported)?;
        self.load(first, 2)?;
        match data.opcode {
            Guard if matches!(&data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM) => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(2, deopt);
            }
            FixnumAdd | FixnumSub | FixnumNeg | FixnumCmpEq | FixnumCmpLt | FixnumCmpLe
            | FixnumCmpGt | FixnumCmpGe => {
                let deopt = self.deopt_label(data)?;
                self.guard_fixnum(2, deopt);
                if data.opcode != FixnumNeg {
                    self.load(*data.args.get(1).ok_or_else(unsupported)?, 3)?;
                    self.guard_fixnum(3, deopt);
                }
                match data.opcode {
                    FixnumAdd => self.asm.add(2, 3),
                    FixnumSub => self.asm.sub(2, 3),
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
            _ => return Err(unsupported()),
        }
        // Do not overwrite any allocated home until every guard has passed:
        // a failing instruction must reconstruct its pre-instruction state.
        self.store(result, 2)
    }

    fn deopt_source(&mut self, source: &ValueSource) -> Result<(), EmitError> {
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
            _ => Err(unsupported()),
        }
    }

    fn deopt(&mut self, state: &FrameState, callback: u64) -> Result<(), EmitError> {
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
                self.deopt_source(source)?;
                self.asm.store(2, 15, self.deopt_base + words * 8);
                words += 1;
            }
        }
        self.asm.imm64(2, state.scopes.len() as u64);
        self.asm.imm64(3, words as u64);
        self.asm.address(4, 15, self.deopt_base);
        self.asm.imm64(5, 0);
        self.asm.imm64(1, callback);
        self.asm.call_reg(1);
        self.asm.imm64(2, NIL.0);
        self.epilogue();
        Ok(())
    }
}

/// Emit the supported integer SSA operations using the System Z C entry ABI.
/// Runtime calls, backward CFG edges and unboxed representations currently decline compilation;
/// precise guard exits serialize all virtual scopes for the shared T0 resume
/// adapter. That adapter must copy the stream into roots before allocating.
pub fn emit_framed(
    function: &Function,
    deopt_t2: u64,
    activation_slots: u16,
) -> Result<FramedCode, EmitError> {
    // Backward edges need a signal/GC poll with synchronized native roots.
    // Until that call path exists, keep these functions in the polling T1
    // backend. This also catches irreducible cycles without relying on the
    // optional OSR metadata to describe every loop.
    let positions: HashMap<_, _> = function
        .block_order()
        .iter()
        .enumerate()
        .map(|(index, &block)| (block, index))
        .collect();
    for (index, &block) in function.block_order().iter().enumerate() {
        if function
            .succs(block)
            .iter()
            .any(|target| positions.get(target).is_none_or(|&target| target <= index))
        {
            return Err(unsupported());
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
                (Opcode::ConstFixnum, AuxData::FixnumImm(value)) => TorclVal::from_fixnum(*value).0,
                (Opcode::ConstFloat, AuxData::FloatImm(value)) => {
                    ((value.to_bits() as u64) << 32) | 4
                }
                (Opcode::ConstChar, AuxData::CharImm(value)) => TorclVal::from_char(*value).0,
                (Opcode::ConstSymbol, AuxData::SymbolRef(value)) => {
                    TorclVal::from_symbol_index(*value).0
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
    let frame_words = spill_slots as usize + edge_words + deopt_words;
    // Every load/store and frame adjustment must fit a signed 20-bit address.
    if frame_words > (524280 - 160) / 8 {
        return Err(unsupported());
    }
    emitter.frame_bytes = (frame_words * 8) as i32;
    emitter.edge_base = 160 + spill_slots as i32 * 8;
    emitter.deopt_base = emitter.edge_base + edge_words as i32 * 8;
    if function.block(function.entry()).params.len() > activation_slots as usize {
        return Err(unsupported());
    }
    emitter.prologue();
    for (index, &parameter) in function.block(function.entry()).params.iter().enumerate() {
        emitter.asm.load(2, 13, index as i32 * 8);
        emitter.store(parameter, 2)?;
    }
    emitter.asm.branch(15, emitter.blocks[&function.entry()]);
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
            emitter.instruction(data)?;
        }
    }
    let has_deopt = !emitter.deopts.is_empty();
    for (state, label) in std::mem::take(&mut emitter.deopts) {
        emitter.asm.bind(label);
        emitter.deopt(function.frame_states.get(state), deopt_t2)?;
    }
    Ok(FramedCode {
        code: emitter.asm.finish().ok_or(EmitError::BadBranch)?,
        compiled_entry: 0,
        osr_entries: Vec::new(),
        bcp_offsets,
        native_spill_slots: frame_words as u32,
        regalloc_spill_slots: machine.num_spill_slots,
        allocation_edits: machine.allocation_edits.len(),
        shadow_root_slots: 0,
        emitted_safepoints: 0,
        root_sync_sites: Vec::new(),
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
    use torcl_rt::value::TorclVal;

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
                stack: vec![source, ValueSource::Const(TorclVal::from_fixnum(1))],
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
        assert!(code.code.starts_with(&[0xeb, 0x6f, 0xf0, 0x30, 0, 0x24]));
        assert!(code.has_deopt);
        assert_ne!(code.bcp_offsets[9], u32::MAX);
        assert_eq!(code.shadow_root_slots, 0);
    }

    #[test]
    fn declines_loops_until_native_safepoint_polling_is_available() {
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
        extern "C" fn deopt(_: u64, _: u64, _: *const u64, _: u64) {}
        let compiled = emit_framed(&f, deopt as *const () as u64, 20).unwrap();
        assert!(compiled.regalloc_spill_slots > 0);
        assert!(compiled.allocation_edits > 0);
        #[cfg(all(target_arch = "s390x", unix))]
        {
            let code = torcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
            // SAFETY: the emitted entry accepts these 20 tagged activation
            // slots and the live JIT buffer owns all executed instructions.
            let entry: unsafe extern "C" fn(*mut u64) -> u64 =
                unsafe { std::mem::transmute(code.as_ptr()) };
            let mut slots: Vec<_> = (1..=20).map(|n| TorclVal::from_fixnum(n).0).collect();
            assert_eq!(
                unsafe { entry(slots.as_mut_ptr()) },
                TorclVal::from_fixnum(210).0
            );
        }
    }

    #[cfg(all(target_arch = "s390x", unix))]
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
        let code = torcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
        // SAFETY: emit_framed's entry uses the System Z C ABI and accesses
        // only the supplied activation slots; the JIT buffer owns its code.
        let entry: unsafe extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr()) };
        let mut slots = [TorclVal::from_fixnum(41).0, 0, 0];
        assert_eq!(
            unsafe { entry(slots.as_mut_ptr()) },
            TorclVal::from_fixnum(42).0
        );
        DEOPT.with(|out| assert!(out.borrow().is_empty()));
        for input in [
            TorclVal::from_fixnum((1_i64 << 60) - 1).0,
            torcl_rt::value::NIL.0,
        ] {
            slots[0] = input;
            assert_eq!(unsafe { entry(slots.as_mut_ptr()) }, torcl_rt::value::NIL.0);
            DEOPT.with(|out| {
                assert_eq!(*out.borrow(), [1, 71, 9, 1, 2, input, input, 8]);
                out.borrow_mut().clear();
            });
        }
    }
}
