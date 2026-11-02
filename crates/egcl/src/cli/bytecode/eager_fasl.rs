// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in T1 compilation of named FASL definitions and portable closure subgraphs.
use super::*;

/// Per-load graph identity bookkeeping. A Weak reserves the allocation identity
/// without retaining a body (and its unrooted constants) after its load action.
/// This also prevents a dropped deferred body from aliasing a later allocation's
/// numeric address. No execution identity or counter is synthesized here.
pub(super) struct LoadUnit {
    restoring: bool,
    templates: HashMap<usize, std::sync::Weak<BytecodeFunction>>,
}

impl LoadUnit {
    pub(super) fn new(restoring: bool) -> Self {
        Self {
            restoring,
            templates: HashMap::new(),
        }
    }

    fn enabled(&self) -> bool {
        !self.restoring
            && !cfg!(egcl_no_t1)
            && env_flag("EGCL_FASL_EAGER_T1") == Some(true)
            && backend_is_bytecode()
            && !profiling_disabled()
            && !matches!(forced_tier(), Some(ForcedTier::Interp | ForcedTier::T0))
    }

    pub(super) fn promote_named(&mut self, symbol: u32, function: EgclVal) {
        if !self.enabled() || is_profile_pinned(symbol) {
            return;
        }
        // Preparation/publication can allocate. The loader roots the unit's
        // constants; retain this callable until its owning mutator has waited.
        egcl_rt::rooted!(function = function);
        let Some(body) = registry_get(symbol) else {
            return;
        };
        egcl_rt::rooted!(_body_root = ActiveBytecodeRoot::new(&body));
        promote_one(symbol, *function, &body);
        self.promote_nested(&body);
    }

    /// Compile only stable closure-template bodies that the runtime will use.
    /// Expander/restart/thunk roots keep their current binding and entry path;
    /// compiling an unused native root would not make their execution eager.
    pub(super) fn promote_nested(&mut self, root: &Arc<BytecodeFunction>) {
        if !self.enabled() {
            return;
        }
        egcl_rt::rooted!(_body_root = ActiveBytecodeRoot::new(root));
        // Includes closures below restart-clause Box bodies. Runtime cloning of
        // an owner keeps its nested Arc bodies, so these exact template cells
        // are also the ones encountered by macro/restart/thunk execution.
        for nested in self.new_templates(root) {
            let (prototype, symbol) = closure_templates::template_for(&nested);
            egcl_rt::rooted!(prototype = prototype);
            promote_one(symbol, *prototype, &nested);
        }
    }

    fn new_templates(&mut self, root: &BytecodeFunction) -> Vec<Arc<BytecodeFunction>> {
        let mut pending = vec![root];
        let mut visited = std::collections::HashSet::new();
        let mut result = Vec::new();
        while let Some(body) = pending.pop() {
            if !visited.insert(std::ptr::from_ref(body) as usize) {
                continue;
            }
            for table in &body.restart_cases {
                pending.extend(
                    table
                        .restarts
                        .iter()
                        .map(|restart| restart.function.as_ref()),
                );
            }
            for nested in &body.nested_functions {
                let key = Arc::as_ptr(nested) as usize;
                if self.templates.contains_key(&key) {
                    continue;
                }
                self.templates.insert(key, Arc::downgrade(nested));
                result.push(Arc::clone(nested));
                pending.push(nested);
            }
        }
        result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_subgraph_walk_deduplicates_shared_templates_below_restart_clauses() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(seed)").unwrap().0);
        egcl_rt::rooted!(
            forms = reader::read_from_string("((lambda (delta) (+ seed delta)))")
                .unwrap()
                .0
        );
        let mut root = compile_function(
            "EAGER-SUBGRAPH-IDENTITY",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .expect("closure graph fixture");
        assert_eq!(root.nested_functions.len(), 1);
        let nested = Arc::clone(&root.nested_functions[0]);
        let identity = Arc::as_ptr(&nested) as usize;
        let mut restart = root.clone();
        restart.nested_functions = vec![Arc::clone(&nested), Arc::clone(&nested)];
        root.nested_functions.clear();
        root.restart_cases.push(RestartCaseInfo {
            restarts: vec![RestartClauseInfo {
                name: "EAGER-SUBGRAPH-RESTART".to_string(),
                function: Box::new(restart),
            }],
        });
        let mut unit = LoadUnit::new(false);
        let found = unit.new_templates(&root);
        assert_eq!(
            found.len(),
            1,
            "shared Arc below restart metadata is visited once"
        );
        assert!(Arc::ptr_eq(&found[0], &nested));
        assert!(
            unit.new_templates(&root).is_empty(),
            "deduplicate across load actions"
        );
        let weak = Arc::downgrade(&nested);
        drop(found);
        drop(nested);
        drop(root);
        assert!(
            weak.upgrade().is_none(),
            "bookkeeping must not retain unrooted constants"
        );
        assert!(unit.templates.contains_key(&identity));
        assert!(unit.templates[&identity].upgrade().is_none());
        // Keeping the Weak reserves this allocation identity until the load
        // ends, so another deferred action cannot reuse its address as a key.
    }
}
