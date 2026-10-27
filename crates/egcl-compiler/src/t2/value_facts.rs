// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Structural value facts shared by inference, proof consumers and verification.
//!
//! A successful guard and a nullable layout probe are different evidence. Neither
//! profiles nor a broad STRING/VECTOR type establish a storage layout. Facts name
//! SSA values, never heap/interior addresses, so relocation does not invalidate
//! them. Only immutable storage properties belong here; contents, lengths and
//! adjustable/displaced layouts would require effect/alias invalidation.
//!
//! This is an immutable snapshot. Rebuild after changing instructions, edges or
//! OSR records. Exact forwarding closes seeded phi graphs; it never treats a
//! Boolean TypeCheck or a representation conversion as an identity. Alternate
//! entries conservatively disable relational layout reuse at reachable consumers.

use super::ir::{
    AuxData, Block, DominatorTree, Function, Inst, InstFlags, Opcode, Value, ValueDef,
    ValueRepresentation,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

const BUDGET: usize = 512;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayoutKind {
    SimpleString,
    PackedU8,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayoutEvidence {
    /// Successful deoptimizing guard: its result is the checked object.
    RefinedValue,
    /// Header width or NIL: the consumer must retain its failure check.
    NullableHeaderWidth,
}

#[derive(Copy, Clone, Debug)]
pub struct LayoutFact {
    pub producer: Inst,
    pub subject: Value,
    pub evidence: Value,
    pub kind: LayoutKind,
    pub encoding: LayoutEvidence,
}

#[derive(Clone, Debug, Default)]
pub struct StructuralFacts {
    forward: HashMap<Value, Vec<Value>>,
    positions: HashMap<Inst, (Block, usize)>,
    dominators: Option<DominatorTree>,
    osr_reachable: HashSet<Block>,
    layouts: Vec<LayoutFact>,
    origins: RefCell<HashMap<Value, Option<Value>>>,
    successors: HashMap<Block, Vec<Block>>,
    osr_entries: Vec<Block>,
    all_entry_dominance: RefCell<HashMap<(Block, Block), bool>>,
}

impl StructuralFacts {
    pub fn analyze(f: &Function) -> Self {
        let mut facts = Self {
            dominators: Some(f.dominators()),
            ..Self::default()
        };
        for &block in f.block_order() {
            for (position, &inst) in f.block(block).insts.iter().enumerate() {
                facts.positions.insert(inst, (block, position));
                let d = f.inst(inst);
                if d.args.len() != 1
                    || d.results.len() != 1
                    || d.args
                        .iter()
                        .chain(&d.results)
                        .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
                {
                    continue;
                }
                let proof = match (&d.opcode, &d.aux) {
                    (Opcode::Guard, AuxData::StringLayout)
                        if d.flags
                            == (InstFlags {
                                guard: true,
                                effectful: true,
                                ..InstFlags::default()
                            })
                            && d.frame_state.is_some()
                            && d.targets.is_empty() =>
                    {
                        Some((LayoutKind::SimpleString, LayoutEvidence::RefinedValue))
                    }
                    (Opcode::PackedU8Layout, AuxData::None)
                        if d.flags == InstFlags::default()
                            && d.frame_state.is_none()
                            && d.targets.is_empty()
                            && f.value(d.results[0]).ty.bits
                                == super::ir::TypeBits::FIXNUM.join(super::ir::TypeBits::NULL) =>
                    {
                        Some((LayoutKind::PackedU8, LayoutEvidence::NullableHeaderWidth))
                    }
                    _ => None,
                };
                if let Some((kind, encoding)) = proof {
                    facts.layouts.push(LayoutFact {
                        producer: inst,
                        subject: d.args[0],
                        evidence: d.results[0],
                        kind,
                        encoding,
                    });
                }
            }
        }
        // Every edge participates, including two edges from the same block.
        let mut incoming = HashMap::<Value, Vec<Value>>::new();
        for &block in f.block_order() {
            if let Some(term) = f.terminator(block) {
                for edge in &f.inst(term).targets {
                    for (&param, &arg) in f.block(edge.block).params.iter().zip(&edge.args) {
                        incoming.entry(param).or_default().push(arg);
                    }
                }
            }
        }
        for index in 0..f.num_values() {
            let value = Value(index as u32);
            if f.value(value).repr != ValueRepresentation::Tagged {
                continue;
            }
            let inputs = match f.value(value).def {
                ValueDef::Result { inst, num: 0 }
                    if facts.positions.contains_key(&inst)
                        && f.inst(inst).opcode == Opcode::Guard
                        && f.inst(inst).args.len() == 1
                        && f.inst(inst).results.len() == 1 =>
                {
                    f.inst(inst).args.clone()
                }
                ValueDef::Param { block, .. } if block != f.entry() => {
                    let Some(inputs) = incoming.remove(&value) else {
                        continue;
                    };
                    inputs
                }
                _ => Vec::new(),
            };
            facts.forward.insert(value, inputs);
        }
        for &block in f.block_order() {
            facts.successors.insert(block, f.succs(block));
        }
        facts.osr_entries = f.osr_entries.iter().map(|entry| entry.block).collect();
        let mut todo = facts.osr_entries.clone();
        while let Some(block) = todo.pop() {
            if facts.osr_reachable.insert(block) {
                todo.extend(f.succs(block));
            }
        }
        facts
    }

    /// Unique actual seed of a closed forwarding graph, under ordinary entry.
    /// Use same_identity_at/layout_before for a proof at an executable use.
    pub fn origin(&self, value: Value) -> Option<Value> {
        if let Some(cached) = self.origins.borrow().get(&value) {
            return *cached;
        }
        let seed = self.forwarding_seed(value, None);
        self.origins.borrow_mut().insert(value, seed);
        seed
    }

    fn forwarding_seed(&self, value: Value, stop: Option<Value>) -> Option<Value> {
        let mut todo = vec![value];
        let mut graph = HashMap::<Value, Vec<Value>>::new();
        let mut seeds = HashSet::new();
        while let Some(value) = todo.pop() {
            if graph.contains_key(&value) {
                continue;
            }
            if graph.len() == BUDGET || !self.forward.contains_key(&value) {
                return None;
            }
            let inputs = if Some(value) == stop {
                Vec::new()
            } else {
                self.forward.get(&value)?.clone()
            };
            if inputs.is_empty() {
                seeds.insert(value);
            }
            todo.extend(inputs.iter().copied());
            graph.insert(value, inputs);
        }
        if seeds.len() != 1 {
            return None;
        }
        let seed = *seeds.iter().next()?;
        let mut reaches = seeds;
        for _ in 0..graph.len() {
            let before = reaches.len();
            for (&node, inputs) in &graph {
                if inputs.iter().any(|input| reaches.contains(input)) {
                    reaches.insert(node);
                }
            }
            if reaches.len() == before {
                break;
            }
        }
        (reaches.len() == graph.len()).then_some(seed)
    }

    pub fn osr_reaches(&self, block: Block) -> bool {
        self.osr_reachable.contains(&block)
    }

    pub fn same_identity_at(&self, a: Value, b: Value, consumer: Inst) -> bool {
        if a == b {
            return true;
        }
        let Some(&(block, _)) = self.positions.get(&consumer) else {
            return false;
        };
        !self.osr_reaches(block)
            && self
                .origin(a)
                .is_some_and(|seed| self.origin(b) == Some(seed))
    }

    /// Ordinary CFG dominance plus all alternate OSR entries. Reaching the
    /// producer block executes its instructions from the start; bypassing it
    /// cannot establish a fact from a guard or conditional in that block.
    pub fn block_dominates_all_entries(&self, before: Block, after: Block) -> bool {
        if !self
            .dominators
            .as_ref()
            .is_some_and(|dom| dom.dominates(before, after))
        {
            return false;
        }
        // These conclusions need no alternate-entry traversal. The snapshot's
        // reachability table already proves that no OSR entry can reach AFTER.
        if before == after || !self.osr_reaches(after) {
            return true;
        }
        let cached = self
            .all_entry_dominance
            .borrow()
            .get(&(before, after))
            .copied();
        if let Some(answer) = cached {
            return answer;
        }
        let mut todo = self.osr_entries.clone();
        let mut seen = HashSet::new();
        while let Some(block) = todo.pop() {
            if block == before || !seen.insert(block) {
                continue;
            }
            if block == after {
                self.all_entry_dominance
                    .borrow_mut()
                    .insert((before, after), false);
                return false;
            }
            if let Some(next) = self.successors.get(&block) {
                todo.extend(next);
            }
        }
        self.all_entry_dominance
            .borrow_mut()
            .insert((before, after), true);
        true
    }

    /// A direct refined result may remain valid after OSR if its guard executes
    /// on every entry. This does not equate independently imported aliases.
    pub fn runs_before(&self, producer: Inst, consumer: Inst) -> bool {
        let (Some(&(before, i)), Some(&(after, j))) =
            (self.positions.get(&producer), self.positions.get(&consumer))
        else {
            return false;
        };
        (before != after || i < j) && self.block_dominates_all_entries(before, after)
    }

    pub fn precedes(&self, producer: Inst, consumer: Inst) -> bool {
        let (Some(&(before, i)), Some(&(after, j))) =
            (self.positions.get(&producer), self.positions.get(&consumer))
        else {
            return false;
        };
        !self.osr_reaches(after)
            && if before == after {
                i < j
            } else {
                self.dominators
                    .as_ref()
                    .is_some_and(|dom| dom.dominates(before, after))
            }
    }

    /// Dominating evidence about this exact object. Nullable evidence is NOT a
    /// successful layout refinement; consumers must inspect `encoding`.
    pub fn layout_before(
        &self,
        subject: Value,
        kind: LayoutKind,
        consumer: Inst,
    ) -> Option<LayoutFact> {
        self.layouts.iter().copied().find(|fact| {
            fact.kind == kind
                && self.precedes(fact.producer, consumer)
                && self.same_identity_at(fact.subject, subject, consumer)
        })
    }

    /// A raw load must retain a data dependency on the successful guard result,
    /// not merely happen to name the same unchecked input. Every forwarding
    /// path must pass through that refined value before reaching an older seed.
    pub fn refined_layout_before(
        &self,
        value: Value,
        kind: LayoutKind,
        consumer: Inst,
    ) -> Option<LayoutFact> {
        self.layouts.iter().copied().find(|fact| {
            fact.kind == kind
                && fact.encoding == LayoutEvidence::RefinedValue
                && (if value == fact.evidence {
                    self.runs_before(fact.producer, consumer)
                } else {
                    self.precedes(fact.producer, consumer)
                })
                && self.forwarding_seed(value, Some(fact.evidence)) == Some(fact.evidence)
        })
    }

    pub fn layout_producer(&self, inst: Inst) -> Option<LayoutFact> {
        self.layouts
            .iter()
            .copied()
            .find(|fact| fact.producer == inst)
    }

    pub fn witness_for(&self, subject: Value, witness: Value, consumer: Inst) -> bool {
        self.layouts.iter().any(|fact| {
            fact.kind == LayoutKind::PackedU8
                && fact.encoding == LayoutEvidence::NullableHeaderWidth
                && fact.evidence == witness
                && self.precedes(fact.producer, consumer)
                && self.same_identity_at(fact.subject, subject, consumer)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameState;
    use crate::t2::ir::{BlockCall, IRType, TypeBits};

    fn data(opcode: Opcode, args: Vec<Value>) -> super::super::ir::InstData {
        super::super::ir::InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            frame_state: None,
            targets: vec![],
            source_pos: 0,
        }
    }
    fn probe(f: &mut Function, b: Block, array: Value) -> (Inst, Value) {
        let (inst, result) = f.push_inst(
            b,
            data(Opcode::PackedU8Layout, vec![array]),
            &[(
                IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        (inst, result[0])
    }
    fn finish(f: &mut Function, b: Block, value: Value) -> Inst {
        let mut ret = data(Opcode::Return, vec![value]);
        ret.flags.terminator = true;
        f.set_terminator(b, ret)
    }
    fn guard(f: &mut Function, b: Block, input: Value, aux: AuxData) -> (Inst, Value) {
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let mut d = data(Opcode::Guard, vec![input]);
        d.aux = aux;
        d.frame_state = Some(state);
        d.flags.guard = true;
        d.flags.effectful = true;
        let (inst, value) = f.push_inst(b, d, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        (inst, value[0])
    }

    #[test]
    fn numeric_inference_exposes_nullable_evidence_without_claiming_success() {
        let mut f = Function::new("nullable-is-not-success");
        let b = f.entry();
        // A fixnum is certainly not an array: its real probe will return NIL.
        let (_, input) = f.push_inst(
            b,
            super::super::ir::InstData {
                aux: AuxData::FixnumImm(9),
                ..data(Opcode::ConstFixnum, vec![])
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let (inst, witness) = probe(&mut f, b, input[0]);
        let ret = finish(&mut f, b, witness);
        let inference = super::super::infer::infer(&f);
        assert_eq!(inference.range(input[0]).unwrap().lo, 9);
        let evidence = inference
            .structural()
            .layout_before(input[0], LayoutKind::PackedU8, ret)
            .unwrap();
        assert_eq!(evidence.producer, inst);
        assert_eq!(evidence.encoding, LayoutEvidence::NullableHeaderWidth);
        assert!(inference.structural().witness_for(input[0], witness, ret));
        assert!(
            inference
                .structural()
                .layout_before(input[0], LayoutKind::SimpleString, ret)
                .is_none()
        );
    }

    #[test]
    fn reuse_nullable_probes_across_identity_guards_and_effects_preserves_nil() {
        let mut f = Function::new("nullable-proof-chain");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        let (first, width) = probe(&mut f, b, input);
        let (_, alias) = guard(&mut f, b, input, AuxData::TypeTag(IRType::TOP));
        // This effect must remain. Immutable layout evidence holds across calls;
        // it says nothing about mutable contents, length or an interior pointer.
        let mut call = data(Opcode::Call, vec![alias]);
        call.flags.effectful = true;
        call.flags.call = true;
        call.aux = AuxData::CallTarget(123);
        let (effect, _) = f.push_inst(b, call, &[]);
        let (second, _) = probe(&mut f, b, alias);
        let (third, last) = probe(&mut f, b, alias);
        let ret = finish(&mut f, b, last);
        assert_eq!(super::super::opt_guard::reuse_layout_evidence(&mut f), 2);
        assert!(f.block(b).insts.contains(&first));
        assert!(f.block(b).insts.contains(&effect));
        assert!(!f.block(b).insts.contains(&second));
        assert!(!f.block(b).insts.contains(&third));
        assert_eq!(f.inst(ret).args, vec![width]);
        assert_eq!(super::super::opt_guard::reuse_layout_evidence(&mut f), 0);
        assert!(StructuralFacts::analyze(&f).witness_for(input, width, ret));
    }

    #[test]
    fn type_checks_and_representation_changes_do_not_forward_object_identity() {
        let mut f = Function::new("boolean-is-not-input");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        let (_, checked) = f.push_inst(
            b,
            super::super::ir::InstData {
                aux: AuxData::TypeTag(IRType::of(TypeBits::STRING)),
                ..data(Opcode::TypeCheck, vec![input])
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, converted) = f.push_inst(
            b,
            data(Opcode::UnboxFloat, vec![input]),
            &[(IRType::TOP, ValueRepresentation::UnboxedF64)],
        );
        let ret = finish(&mut f, b, input);
        let facts = StructuralFacts::analyze(&f);
        assert!(!facts.same_identity_at(input, checked[0], ret));
        assert!(!facts.same_identity_at(input, converted[0], ret));
        assert!(
            facts
                .layout_before(input, LayoutKind::SimpleString, ret)
                .is_none()
        );
    }

    #[test]
    fn inferred_string_type_does_not_establish_direct_storage_layout() {
        let mut f = Function::new("semantic-string-not-storage");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::of(TypeBits::STRING), ValueRepresentation::Tagged);
        let (_, alias) = guard(
            &mut f,
            b,
            input,
            AuxData::TypeTag(IRType::of(TypeBits::STRING)),
        );
        let ret = finish(&mut f, b, alias);
        let inf = super::super::infer::infer(&f);
        assert_eq!(inf.ty(alias).bits, TypeBits::STRING);
        assert!(
            inf.structural()
                .layout_before(alias, LayoutKind::SimpleString, ret)
                .is_none()
        );
    }

    #[test]
    fn siblings_and_mixed_join_never_supply_a_dominating_layout() {
        let mut f = Function::new("branch-proofs");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        let other = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        let left = f.make_block();
        let right = f.make_block();
        let join = f.make_block();
        let phi = f.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
        let mut branch = data(Opcode::Brif, vec![input]);
        branch.flags.terminator = true;
        branch.targets = vec![
            BlockCall {
                block: left,
                args: vec![],
            },
            BlockCall {
                block: right,
                args: vec![],
            },
        ];
        f.set_terminator(b, branch);
        let (_, width) = probe(&mut f, left, input);
        for (block, value) in [(left, input), (right, other)] {
            let mut jump = data(Opcode::Jump, vec![]);
            jump.flags.terminator = true;
            jump.targets = vec![BlockCall {
                block: join,
                args: vec![value],
            }];
            f.set_terminator(block, jump);
        }
        let ret = finish(&mut f, join, phi);
        let facts = StructuralFacts::analyze(&f);
        assert!(facts.origin(phi).is_none());
        assert!(!facts.witness_for(input, width, ret));
        assert!(
            facts
                .layout_before(phi, LayoutKind::PackedU8, ret)
                .is_none()
        );
    }
    #[test]
    fn direct_guard_reexecutes_after_osr_but_downstream_import_cannot_bypass_it() {
        let mut f = Function::new("osr-direct-proof");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        let (_, proof) = guard(&mut f, b, input, AuxData::StringLayout);
        let ret = finish(&mut f, b, proof);
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        f.osr_entries.push(super::super::ir::OsrEntry {
            block: b,
            bcp: 0,
            frame_state: state,
            control_scopes: vec![],
            checks: vec![],
        });
        let facts = StructuralFacts::analyze(&f);
        assert!(
            facts
                .refined_layout_before(proof, LayoutKind::SimpleString, ret)
                .is_some()
        );
        assert!(
            facts
                .refined_layout_before(input, LayoutKind::SimpleString, ret)
                .is_none(),
            "raw same-object input is not an explicit guard dependency"
        );
        let downstream = f.make_block();
        let ret2 = finish(&mut f, downstream, proof);
        let mut jump = data(Opcode::Jump, vec![]);
        jump.flags.terminator = true;
        jump.targets = vec![BlockCall {
            block: downstream,
            args: vec![],
        }];
        *f.inst_mut(ret) = jump;
        let facts = StructuralFacts::analyze(&f);
        for _ in 0..2 {
            assert!(
                facts
                    .refined_layout_before(proof, LayoutKind::SimpleString, ret2)
                    .is_some(),
                "every OSR path still executes the earlier guard"
            );
        }
        f.osr_entries[0].block = downstream;
        let facts = StructuralFacts::analyze(&f);
        for _ in 0..2 {
            assert!(
                facts
                    .refined_layout_before(proof, LayoutKind::SimpleString, ret2)
                    .is_none(),
                "a fresh snapshot must reject the new bypassing entry"
            );
        }
    }

    #[test]
    fn nullable_probe_fact_reuse_never_rewrites_mapped_imports() {
        let mut f = Function::new("mapped-probe");
        let b = f.entry();
        let input = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
        probe(&mut f, b, input);
        let (_, last) = probe(&mut f, b, input);
        finish(&mut f, b, last);
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        f.osr_entries.push(super::super::ir::OsrEntry {
            block: b,
            bcp: 0,
            frame_state: state,
            control_scopes: vec![],
            checks: vec![],
        });
        assert_eq!(super::super::opt_guard::reuse_layout_evidence(&mut f), 0);
    }
}
