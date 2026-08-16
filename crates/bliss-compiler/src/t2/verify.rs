//! P2 — IR verifier, algorithm A4.07 (spec §4.3.8).
//!
//! **Parcel P2. Owner: (sub-agent).** Implement `verify` to check V1–V10 from
//! spec §4.3.8.1 against the frozen `ir` contract: dangling handles, use/pred
//! consistency, SSA dominance (via `Function::dominators`), block-parameter
//! arity/type agreement on every terminator `BlockCall`, terminator
//! well-formedness (exactly one, last), effect ordering, guard/FrameState
//! well-formedness (spec §4.10 R4.64), and CFG reducibility. Unit-test with both
//! valid IR (built by hand) and intentionally-broken IR asserting each check
//! fires.

use crate::t2::ir::Function;

/// A single verification failure (spec §4.3.8.1 checks V1–V10).
#[derive(Clone, Debug)]
pub struct VerifyError {
    /// Which check failed, e.g. "V3 dominance".
    pub check: &'static str,
    pub detail: String,
}

/// Verify a function; `Ok(())` iff it is well-formed (spec R4.20).
pub fn verify(_f: &Function) -> Result<(), Vec<VerifyError>> {
    todo!("P2: implement V1–V10 (spec §4.3.8.1)")
}
