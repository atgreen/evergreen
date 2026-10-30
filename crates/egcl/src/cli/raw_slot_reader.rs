// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A guarded leaf for an exact compiler-proved raw reader. Full original method
//! code (including its lexical continuation helpers) remains the miss path.
use super::*;

fn primitive_current(symbol: u32, env: &Env) -> bool {
    let name = sym_name_rc(EgclVal::from_symbol_index(symbol));
    installed_function(symbol).is_none()
        && named_call_target(env, &name, Some(symbol), true).is_none()
        && !env.generics.contains_key(name.as_ref())
        && !env.methods.contains_key(name.as_ref())
}

pub(super) fn try_read(
    method: EgclVal,
    proof: &bytecode::RawSlotReader,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<EgclVal> {
    let [instance] = args else {
        return None;
    };
    // CLASS-OF can intern built-in class names for noninstances. Establish a
    // real live instance first so every declining guard remains callback-free.
    egcl_stdlib::clos::instance_layout_key(*instance)?;
    let callable = METHOD_COMPILED.borrow().get(&method.0).copied()?;
    if !bytecode::method_no_context_body_matches(callable, &proof.body)
        || !primitive_current(proof.primitive, env)
        || proof
            .helpers
            .iter()
            .any(|&symbol| !primitive_current(symbol, env))
        || !egcl_stdlib::clos::class_protocol_finalized(egcl_stdlib::class_of(*instance))
    {
        return None;
    }
    // Location is obtained only for this exact live wrapper. No callback or
    // Lisp allocation occurs before the matching, forwarding-aware bound read.
    let (layout, index) =
        egcl_stdlib::clos::slot_layout_location(*instance, EgclVal::from_symbol_index(proof.slot))?;
    let value = egcl_stdlib::clos::slot_at_layout(*instance, layout, index)?;
    env.clear_mv();
    #[cfg(test)]
    HITS.with(|hits| {
        let (observed, count) = hits.get();
        if observed == method.0 {
            hits.set((observed, count + 1));
        }
    });
    Some(value)
}

#[cfg(test)]
thread_local! { static HITS: std::cell::Cell<(u64, usize)> = const { std::cell::Cell::new((0, 0)) }; }

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) fn hits() -> usize {
    HITS.with(|hits| hits.get().1)
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) fn observe_method(method: EgclVal) -> impl Drop {
    struct Observation((u64, usize));
    impl Drop for Observation {
        fn drop(&mut self) {
            HITS.with(|hits| hits.set(self.0));
        }
    }
    Observation(HITS.with(|hits| hits.replace((method.0, 0))))
}
