// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Rust-owned ordinary entry without a new callee VM activation. An existing
//! real caller VM frame anchors current debugger/recovery protocols. No caller
//! CAPTURE, adapter record or generated binding authorizes this root ingress.
use super::*;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EGCL_RUST_ORDINARY_ROOT").as_deref() != Some("0".as_ref()))
}

/// The already selected code/function pair is retained by the caller. None
/// leaves the original root route authoritative, with no invocation credit or
/// execution-state mutation. T2 entries keep their existing no-credit policy.
pub(super) fn try_run(
    code: &Rc<TransferCode>,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<Result<EgclVal, EgclError>> {
    if !enabled()
        || code.needs_recompile()
        || !env.locally_specials.is_empty()
        || args.len() != usize::from(code.body.arity)
        || NATIVE_DEPTH.with(Cell::get) >= native_depth_cap()
        || native_error_pending()
    {
        return None;
    }
    // The body and installed entry never change within this retained owner.
    // Cache rejection too: no full bytecode/layout scan belongs on a hot call.
    let offset = *code.rust_root_entry.get_or_init(|| {
        (code.is_t2
            && ordinary_native_body(&code.body)
            && code.body.self_call_sites.is_empty()
            && code.osr_entries.is_empty()
            && fixed_tagged_parameters(&code.body))
        .then(|| code.lean_root_entry())
        .flatten()
    });
    let offset = offset?;
    // This is the current execution's REAL linked caller frame, not an older
    // generated capture. Null-frame Rust hosts retain the old root route.
    let anchor = egcl_rt::current_stack().fp();
    if anchor.is_null() {
        return None;
    }
    let execution = egcl_rt::thread::current_execution_context_for_native()?;
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(args = EntryArgs::new(args));
    egcl_rt::rooted_ref!(_env = &mut *env);
    // Rewritten heap captures use the precise native callable home. Legacy
    // host frames and captured lexical control still require the old route.
    if !egcl_rt::function::is_interpreted_function(*function)
        || (!egcl_rt::function::closure_env(*function).is_nil()
            && (code.native_callable_slot.is_none() || !code.accepts_explicit_callable(*function)))
        || closure_captured_env(*function).is_some()
        || super::super::has_captured_control(*function)
        || (code.native_callable_slot.is_some() && !code.accepts_explicit_callable(*function))
    {
        return None;
    }
    // Enclosing pending errors stay rooted, not only saved as raw Rust values.
    // The old capture is hidden throughout construction, entry and cleanup.
    let mut entry = EntryGuard {
        env: NATIVE_ENV.with(|slot| slot.replace(env)),
        capture: CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())),
        error: take_native_error(),
    };
    egcl_rt::rooted_ref!(_entry = &mut entry);
    // A recovery address belonging to the enclosing machine activation must
    // never cover this Rust-owned poll or the fresh child's machine frame.
    let _recovery = clear_fault_recovery();
    egcl_rt::rooted!(
        lease = mapped_ordinary::Lease::new_rust_owned(Rc::clone(code), env, anchor.cast_mut())
    );
    let _catch_guard = CatchScopeGuard {
        base: env.catch_stack.len(),
        env,
    };
    let _scope = BlockScopeGuard {
        env,
        blocks: std::mem::take(&mut env.block_stack),
        tags: std::mem::take(&mut env.tag_stack),
    };
    // Match the existing root entry protocol after every input, Env, saved
    // error, body and code owner is independently retained. A pending GC or
    // signal is serviced here; it does not force a callee VM activation.
    env.clear_mv();
    if !entry_poll_idle(unsafe { &*execution }) {
        egcl_rt::thread::begin_current_execution_poll();
        egcl_rt::safepoint::poll_safepoint();
        let signal_error = pending_signal_error_for_current_execution();
        egcl_rt::thread::rearm_current_execution_poll();
        if native_error_pending() {
            egcl_rt::thread::request_current_execution_poll();
        }
        if let Some(error) = signal_error {
            return Some(Err(error));
        }
    }
    // Execution and its VM stack remain stable when a poll migrates a fiber;
    // run initializes current carrier/control snapshots after this boundary.
    let current_anchor = egcl_rt::current_stack().fp();
    if current_anchor.is_null() || current_anchor != anchor {
        return Some(Err(invalid_capture()));
    }
    // No handler activation may escape the entry poll. The resumed execution
    // must still own the exact real caller anchor retained by this context.
    let bridge = lease.bridge_mut() as *mut mapped_ordinary::Bridge;
    // Lease initialization sets ordinary_bridge=true, activation=null. Thus an
    // ordinary guard at this physical root uses native CHILD reconstruction
    // above anchor, never the old root-frame/code_info reuse path. The exact
    // code/body roots and the new control remain live through cold cleanup.
    Some(unsafe {
        mapped_ordinary::run(
            code,
            bridge,
            offset,
            *function,
            args.as_slice(),
            env,
            execution,
        )
    })
}

#[cfg(test)]
static ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn note_entry_for_test() {
    ENTRIES.with(|slot| slot.set(slot.get() + 1));
}
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_entries_for_test() -> usize {
    ENTRIES.with(|slot| slot.replace(0))
}

#[cfg(test)]
#[path = "rust_ordinary_root_tests.rs"]
mod tests;

#[cfg(test)]
pub(in crate::cli::bytecode) fn enabled_for_test() -> bool {
    enabled()
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn assert_anchor_for_test(expected: usize) {
    let boundary = root_publication::ACTIVE.with(Cell::get);
    assert!(!boundary.is_null());
    let context = unsafe { &*(*boundary).context };
    if enabled() {
        assert_eq!(context.frame as usize, expected);
        assert!(context.ordinary_bridge && context.activation.is_null());
    } else {
        assert_ne!(context.frame as usize, expected);
        assert!(!context.ordinary_bridge);
    }
}
