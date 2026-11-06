// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! First executable/install ownership boundary. This does not publish reusable
//! entries or authorize execution from a payload handle. Entry capability and
//! exact-PC registration remain on the enclosing `Rc<TransferCode>` installation.

use super::*;

/// Frozen executable bytes and their exact physical-version maps. The field
/// partition is also used for pointer-bearing variants, but those remain owned
/// by one installation; only the audited leaf variant has independent leases.
///
/// Address inventory for that first cohort:
/// - main code addresses its retained arity veneer;
/// - the untyped fixed-arity veneer embeds an immediate count and a process-
///   lifetime helper function address;
/// - when lean entry is enabled, its overflow branch addresses the process-
///   lifetime lean-overflow veneer retained by `lean_calls_enabled`'s Box::leak;
/// - entry/overflow code loads current execution and control state through R13.
///   These are invocation-time context loads, not baked installation addresses;
/// - no named cells, probes, heat counters, heap-pool slots, keyword plans or
///   installation addresses are embedded in the audited leaf/helper graph.
///
/// Unused capture/poll/landing buffers stay on the installation. Extending the
/// cohort requires auditing emitted helpers, not only adding bytecode opcodes.
///
/// An Arc is a retention mechanism here, not a proof of cross-execution safety.
/// No Send/Sync implementation or global PC/artifact cache is introduced.
pub(in crate::cli::bytecode) struct ExecutablePayload {
    pub(super) code: JitBuffer,
    pub(super) code_info: &'static CodeInfo,
    pub(super) _native_calls: Arc<egcl_compiler::t2::x64_calls::NativeCallSites>,
    pub(super) code_len: usize,
    pub(super) bcp_offsets: Vec<u32>,
    pub(super) _ordinary_arity: Option<JitBuffer>,
    // Retain the *registered token*, not just its BytecodeFunction Arc. The
    // existing installation retains its original token independently. There is
    // no second scanner or copied constant/relocation storage for this lease.
    pub(super) retained_roots: Option<Arc<ActiveBytecodeRoot>>,
}

pub(super) enum ExecutableOwner {
    Installation(Box<ExecutablePayload>),
    AuditedLeaf(Arc<ExecutablePayload>),
}

impl ExecutableOwner {
    pub(super) fn new(
        mut payload: ExecutablePayload,
        audited_leaf: bool,
        roots: &Arc<ActiveBytecodeRoot>,
    ) -> Self {
        if audited_leaf {
            payload.retained_roots = Some(Arc::clone(roots));
            Self::AuditedLeaf(Arc::new(payload))
        } else {
            Self::Installation(Box::new(payload))
        }
    }

    #[cfg(test)]
    pub(super) fn unleased_fixture_payload(&mut self) -> Option<&mut ExecutablePayload> {
        match self {
            Self::Installation(payload) => Some(payload),
            Self::AuditedLeaf(payload) => Arc::get_mut(payload),
        }
    }

    pub(super) fn payload(&self) -> &ExecutablePayload {
        match self {
            Self::Installation(payload) => payload,
            Self::AuditedLeaf(payload) => payload,
        }
    }
}

/// Fail closed on source operations as well as the emitted helper graph checked
/// at installation. These instructions cannot perform calls, allocate, poll a
/// loop, capture an environment, access dynamic state or establish control.
pub(super) fn binding_free_leaf_body(body: &BytecodeFunction) -> bool {
    !body.has_env
        && !body.variadic
        && body.min_args == body.arity
        && body.param_types.iter().all(|ty| ty.is_any())
        && body.load_time_values.is_empty()
        && body.constants.iter().all(|value| {
            value.is_fixnum()
                || value.is_nil()
                || value.is_symbol()
                || value.is_character()
                || value.is_single_float()
        })
        && !body.code.is_empty()
        && body.code.iter().all(|op| {
            matches!(
                op,
                Instr::Const(_)
                    | Instr::LoadLocal(_)
                    | Instr::StoreLocal(_)
                    | Instr::ClearMv
                    | Instr::Return
            )
        })
}

#[cfg(test)]
mod tests;
