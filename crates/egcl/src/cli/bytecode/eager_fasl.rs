// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in T1 compilation of named FASL definitions and their closure templates.
use super::*;

pub(super) fn promote(symbol: u32, function: EgclVal) {
    if cfg!(egcl_no_t1)
        || env_flag("EGCL_FASL_EAGER_T1") != Some(true)
        || !backend_is_bytecode()
        || profiling_disabled()
        || matches!(forced_tier(), Some(ForcedTier::Interp | ForcedTier::T0))
        || is_profile_pinned(symbol)
    {
        return;
    }
    // Preparation/publication can allocate. The loader roots the entire unit's
    // constants, and this callable stays rooted until the owner finishes waiting.
    egcl_rt::rooted!(function = function);
    let Some(body) = registry_get(symbol) else {
        return;
    };
    egcl_rt::rooted!(_body_root = ActiveBytecodeRoot::new(&body));
    promote_one(symbol, *function, &body);

    // A template is a code identity, never an invocation. Mint it through the
    // same path closure creation uses, without constructing captures or running
    // any Lisp body/default. The root above traces this whole nested graph.
    // Use an iterative walk and visit shared Arc bodies once per definition.
    let mut pending = body.nested_functions.clone();
    let mut seen = std::collections::HashSet::new();
    while let Some(nested) = pending.pop() {
        if !seen.insert(Arc::as_ptr(&nested) as usize) {
            continue;
        }
        pending.extend(nested.nested_functions.iter().cloned());
        let (prototype, template_symbol) = closure_templates::template_for(&nested);
        egcl_rt::rooted!(prototype = prototype);
        promote_one(template_symbol, *prototype, &nested);
    }
}

fn promote_one(symbol: u32, function: EgclVal, body: &Arc<BytecodeFunction>) {
    egcl_rt::rooted!(function = function);
    if is_profile_pinned(symbol) || !background_t1::request(symbol) {
        return;
    }
    let Some(native) = background_t1::wait(symbol) else {
        return;
    };
    // The ordinary compiler generation checks reject replaced definitions; keep
    // the same exact-body check as dispatch before synchronous-port publication.
    if native
        .body
        .as_ref()
        .is_none_or(|installed| !Arc::ptr_eq(installed, body))
        || egcl_rt::symbols::symbol_function(symbol) != Some(*function)
    {
        return;
    }
    // Background installation publishes on its owning mutator already. Other
    // hosts use the synchronous request shim, whose caller owns publication.
    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "s390x", target_os = "linux")
    )))]
    {
        mark_fresh_promotion(symbol);
        publish_native(symbol, Some(*function), &native);
    }
    egcl_rt::blog!(
        "compile",
        egcl_rt::log::TRACE,
        "[T1] {}: eager FASL compilation ready",
        sym_label(symbol)
    );
}
