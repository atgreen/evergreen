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

use crate::t2::ir::{Block, Function};

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
pub fn build_osr_entry(_f: &mut Function, _header: Block) -> OsrEntry {
    todo!("P8: OSR entry block importing live interpreter state as block params")
}
