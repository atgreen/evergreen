// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! P8 — OSR entry region in the IR (spec §4.6 A4.03, §4.10 R4.66).
//!
//! **Parcel P8. Owner: (sub-agent).** Model on-stack-replacement entry as a
//! dedicated OSR entry block whose block parameters IMPORT the live interpreter
//! slots at a loop header (the structural inverse of a `FrameState`): given a
//! `Function` and a target loop-header `Block` (a bytecode back-edge target),
//! synthesise an OSR entry block that receives the live locals/operand-stack as
//! typed block parameters (representations matching the source-tier conversions),
//! then jumps into the loop header passing them. Also produce the OSR entry-map
//! description (source slot → param, with any box/unbox conversion) that the
//! runtime (spec §4.6 D4.09 / A4.03) uses to transfer state in. Unit-test on a
//! hand-built counted-loop `Function`: the OSR block's params match the header's
//! live-in values and it branches to the header with matching args.

use crate::t2::ir::{AuxData, Block, BlockCall, Function, InstData, InstFlags, Opcode, Value};

/// Per-slot import descriptor for the OSR entry map (spec §4.6 D4.09 inverse).
#[derive(Clone, Debug)]
pub struct OsrSlotImport {
    /// Source interpreter slot (local index, or operand-stack position).
    pub source_slot: u32,
    /// The IR value (an OSR entry-block parameter) it is imported into.
    pub param: crate::t2::ir::Value,
}

/// The result of building an OSR entry (spec §4.6 A4.03, §4.10 R4.66).
#[derive(Clone, Debug)]
pub struct OsrEntry {
    pub entry_block: Block,
    pub header: Block,
    pub imports: Vec<OsrSlotImport>,
}

/// Build an OSR entry region that enters `f` at loop-header `header`.
///
/// The loop `header` already carries one block parameter per loop-carried
/// interpreter slot (locals + live operand-stack). Those header parameters ARE
/// the live-in set: this pass has no separate liveness analysis, it takes the
/// header's parameter list as the authoritative set of slots the source tier
/// must transfer in (spec §4.6 D4.09 invariant 1: `slot_count` == live slots at
/// the header).
///
/// We synthesise a fresh OSR entry block with a matching parameter for each
/// header parameter — same IR type and, crucially, the same `ValueRepresentation`
/// (spec R4.66: the imported values' representations MUST match the D4.09
/// `ConversionKind`s so the state A4.03 transfers in is exactly what the loop
/// body consumes). The needed box/unbox conversion is therefore captured
/// implicitly by the parameter's representation rather than as a separate field:
/// a `Tagged` header slot imports a `Tagged` param (`ConversionKind::None`), an
/// `UnboxedFixnum` header slot imports an `UnboxedFixnum` param (the source tier
/// unboxes on the way in), etc.
///
/// The entry block is terminated with a `Jump` into `header`, passing the new
/// parameters positionally as the edge arguments so the header's block
/// parameters receive them. This is the structural inverse of a `FrameState`,
/// which EXPORTS live SSA values on deopt; here we IMPORT them on OSR entry.
///
/// The OSR block is a fresh, alternate entry to the CFG — it is deliberately NOT
/// made the function's primary entry; the runtime dispatches to it directly via
/// the OSR entry map.
pub fn build_osr_entry(f: &mut Function, header: Block) -> OsrEntry {
    // Snapshot the header's parameter (type, repr) list — the live-in set. We
    // copy it out first because we are about to mutate the value/block arenas.
    let header_slots: Vec<(crate::t2::ir::IRType, crate::t2::ir::ValueRepresentation)> = f
        .block(header)
        .params
        .iter()
        .map(|&p| {
            let vd = f.value(p);
            (vd.ty, vd.repr)
        })
        .collect();

    // Fresh alternate entry block.
    let entry_block = f.make_block();

    // One imported parameter per header slot, representation-matched.
    let mut imports = Vec::with_capacity(header_slots.len());
    let mut args: Vec<Value> = Vec::with_capacity(header_slots.len());
    for (slot, (ty, repr)) in header_slots.into_iter().enumerate() {
        let param = f.add_block_param(entry_block, ty, repr);
        args.push(param);
        imports.push(OsrSlotImport {
            source_slot: slot as u32,
            param,
        });
    }

    // Jump into the loop header, forwarding the imported slots as edge args so
    // they bind the header's block parameters.
    f.set_terminator(
        entry_block,
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

    OsrEntry {
        entry_block,
        header,
        imports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{
        AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueDef,
        ValueRepresentation,
    };

    /// Build a small counted-loop function:
    ///   entry -> header(i: unboxed-fixnum, acc: tagged, n: tagged)
    ///   header: brif back to itself or exit (shape is irrelevant to P8).
    /// The header's three block params model the loop-carried interpreter slots.
    fn counted_loop() -> (Function, crate::t2::ir::Block) {
        let mut f = Function::new("counted_loop");
        let entry = f.entry();
        let header = f.make_block();
        let exit = f.make_block();

        // Three live-in slots with distinct types + representations, so the test
        // proves per-slot representation matching (not just count).
        let i = f.add_block_param(
            header,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let acc = f.add_block_param(
            header,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
        let _n = f.add_block_param(
            header,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );

        // Minimal well-formed CFG: entry -> header -> exit. The loop body /
        // back-edge args are elided — this unit only exercises build_osr_entry,
        // which reads the header's param *signature*, not the CFG's edge args.
        let _ = (i, acc);
        f.set_terminator(
            entry,
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
        // header terminates to exit (loop body elided — not relevant here).
        f.set_terminator(
            header,
            InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![BlockCall {
                    block: exit,
                    args: vec![],
                }],
                frame_state: None,
                source_pos: 0,
            },
        );
        f.set_terminator(
            exit,
            InstData {
                opcode: Opcode::Return,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        (f, header)
    }

    // spec-covers: R4.66
    // A dedicated OSR entry region (a distinct block, not the header or the
    // normal entry) whose live-ins import one param per interpreter slot with
    // MATCHING type and repr — "the state A4.03 transfers in is exactly the
    // state the loop body consumes".
    #[test]
    fn osr_entry_imports_header_slots() {
        let (mut f, header) = counted_loop();

        // Record the header's param types/reprs before building.
        let header_params: Vec<_> = f.block(header).params.clone();
        let header_sig: Vec<_> = header_params
            .iter()
            .map(|&p| {
                let vd = f.value(p);
                (vd.ty, vd.repr)
            })
            .collect();
        let n = header_params.len();
        assert_eq!(n, 3);

        let osr = build_osr_entry(&mut f, header);

        // (1) The OSR entry block is a fresh, distinct alternate entry.
        assert_eq!(osr.header, header);
        assert_ne!(osr.entry_block, header);
        assert_ne!(osr.entry_block, f.entry());

        // (2) It has one param per header slot, with matching type + repr.
        let osr_params: Vec<_> = f.block(osr.entry_block).params.clone();
        assert_eq!(osr_params.len(), n);
        for (idx, &p) in osr_params.iter().enumerate() {
            let vd = f.value(p);
            assert_eq!(vd.ty, header_sig[idx].0, "type mismatch at slot {idx}");
            assert_eq!(vd.repr, header_sig[idx].1, "repr mismatch at slot {idx}");
            // The param really is defined by the OSR entry block.
            assert_eq!(
                vd.def,
                ValueDef::Param {
                    block: osr.entry_block,
                    num: idx as u16
                }
            );
        }

        // (3) The terminator is a Jump to the header whose args are exactly the
        // OSR block's params, in order.
        let term = f.terminator(osr.entry_block).expect("osr block terminated");
        let term_data = f.inst(term);
        assert_eq!(term_data.opcode, Opcode::Jump);
        assert_eq!(term_data.targets.len(), 1);
        assert_eq!(term_data.targets[0].block, header);
        assert_eq!(term_data.targets[0].args, osr_params);
        assert_eq!(f.succs(osr.entry_block), vec![header]);

        // (4) The import map has one entry per slot, source_slot is the header
        // param index, and param points at the receiving OSR param.
        assert_eq!(osr.imports.len(), n);
        for (idx, imp) in osr.imports.iter().enumerate() {
            assert_eq!(imp.source_slot, idx as u32);
            assert_eq!(imp.param, osr_params[idx]);
        }
    }

    #[test]
    fn osr_entry_empty_header_has_no_imports() {
        // A header with zero loop-carried slots yields an OSR block with no
        // params and an argless jump — the degenerate but valid case.
        let mut f = Function::new("empty");
        let header = f.make_block();
        f.set_terminator(
            header,
            InstData {
                opcode: Opcode::Return,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );

        let osr = build_osr_entry(&mut f, header);
        assert!(osr.imports.is_empty());
        assert!(f.block(osr.entry_block).params.is_empty());
        let term = f.terminator(osr.entry_block).unwrap();
        assert!(f.inst(term).targets[0].args.is_empty());
    }
}
