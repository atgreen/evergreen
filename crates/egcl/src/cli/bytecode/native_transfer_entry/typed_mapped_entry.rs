// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Checking entries admitted only below an explicit mapped caller publication.
use super::*;

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("EGCL_MAPPED_TYPED_ORDINARY").as_deref() != Ok("0"))
}

impl TransferCode {
    /// A fixed T2 entry that performs its own declaration checks. The existing
    /// incoming-root rejection veneer remains authoritative until native body
    /// homes exist. This capability never authorizes a lean managed-root call.
    pub(in crate::cli::bytecode) fn checked_typed_mapped_entry(
        &self,
        function: EgclVal,
    ) -> Option<usize> {
        if !enabled()
            || !self.is_t2
            || self.needs_recompile()
            || !fixed_tagged_parameters(&self.body)
            || !self.body.self_call_sites.is_empty()
            || !self.osr_entries.is_empty()
            || self.ordinary_bound_entry.is_some()
            || !self.body.param_types.iter().any(|ty| !ty.is_any())
            || !self.ordinary_checks_params
            || !egcl_rt::function::is_interpreted_function(function)
            || !egcl_rt::function::closure_env(function).is_nil()
            || closure_captured_env(function).is_some()
            || has_captured_control(function)
        {
            return None;
        }
        // A compatible descriptor proves the existing public checking entry,
        // arity, code bounds, owner and stack bound. Never select self/bound
        // entries that omit checks, and never add a second argument binder.
        let descriptor = self.ordinary_descriptor(0)?;
        if !descriptor.is_compatible() || descriptor.arity != usize::from(self.body.arity) {
            return None;
        }
        descriptor.entry.checked_sub(descriptor.code_start)
    }
}

#[cfg(test)]
#[path = "typed_mapped_entry_tests.rs"]
mod tests;
