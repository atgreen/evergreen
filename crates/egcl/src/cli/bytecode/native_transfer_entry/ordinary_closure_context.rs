// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Captured outer-frame operations for ordinary native callees. The native
//! frame, rather than a template/code-address lookup, supplies the identity.
use super::*;

pub(super) fn access(args: &[EgclVal], store: bool) -> Result<EgclVal, EgclError> {
    if args.len() != if store { 3 } else { 2 }
        || !args[1].is_fixnum()
        || args[1].as_fixnum() < 0
        || !egcl_rt::function::is_interpreted_function(args[0])
    {
        return Err(invalid_capture());
    }
    let boundary = root_publication::ACTIVE.with(Cell::get);
    let boundary = unsafe { boundary.as_ref() }.ok_or_else(invalid_capture)?;
    // This is the immutable owner selected from the exact native return PC,
    // retained by the publication directory even after a function redefinition.
    let owner = unsafe { boundary.owner.as_ref() }.ok_or_else(invalid_capture)?;
    if root_publication::frame_callable(boundary) != Some(args[0])
        || owner.native_callable_slot.is_none()
    {
        return Err(invalid_capture());
    }
    // Observe this exact published semantic access, not an unrelated loop
    // poll. No frame/registry borrow or Lisp allocation spans the observer.
    #[cfg(test)]
    root_publication::observe();
    #[cfg(test)]
    if store {
        if let Some(hook) = STORE_OBSERVER.with(|hook| hook.replace(None)) {
            hook(std::ptr::from_ref(&args[2]));
        }
    }
    #[cfg(test)]
    PUBLISHED.with(|count| count.set(count.get() + 1));
    let index = usize::try_from(args[1].as_fixnum()).map_err(|_| invalid_capture())?;
    let name = owner.body.names.get(index).ok_or_else(invalid_capture)?;
    let frame = closure_captured_env(args[0]).ok_or_else(invalid_capture)?;
    let symbol = egcl_rt::symbols::find_index(name);
    if store {
        // The existing Env mutation protocol owns the write barrier and shared
        // lexical-cell semantics. It performs no Lisp allocation and no borrow
        // of the frame remains live when the adapter next polls/returns.
        Env::set_frame_var(&frame, name, args[2]);
        Ok(NIL)
    } else {
        Env::lookup_resolved_frame(&frame, name, symbol)
            .ok_or_else(|| EgclError::UnboundVariable(resolve_sym(name).unwrap_or(NIL)))
    }
}

/// Noncollecting success path. Every rejection retains the published adapter.
pub(super) unsafe fn leaf(
    symbol: u32,
    args: &[EgclVal],
    image: *mut SysvTransferCapture,
) -> Option<EgclVal> {
    let store = super::ordinary_primitives::lexical_store(symbol)?;
    #[cfg(test)]
    if let Some(hook) = BEFORE_LEAF.with(|hook| hook.replace(None)) {
        hook(store);
    }
    if args.len() != if store { 3 } else { 2 }
        || !args[1].is_fixnum()
        || args[1].as_fixnum() < 0
        || !current_poll_idle()
        || native_error_pending()
    {
        return None;
    }
    #[cfg(test)]
    if root_publication::OBSERVE.with(Cell::get).is_some()
        || root_publication::OBSERVE_AFTER.with(Cell::get).is_some()
        || STORE_OBSERVER.with(Cell::get).is_some()
    {
        return None;
    }
    unsafe {
        root_publication::with_leaf_callable(image, |owner, callable| {
            if callable != args[0] || !owner.accepts_explicit_callable(callable) {
                return None;
            }
            let index = usize::try_from(args[1].as_fixnum()).ok()?;
            let name = owner.body.names.get(index)?;
            let frame = closure_captured_env(callable)?;
            let symbol = egcl_rt::symbols::find_index(name);
            let answer = if store {
                if !Env::set_resolved_frame_var(&frame, name, symbol, args[2]) {
                    return None;
                }
                NIL
            } else {
                Env::lookup_resolved_frame(&frame, name, symbol)?
            };
            #[cfg(test)]
            HITS.with(|hits| hits.set(hits.get() + 1));
            Some(answer)
        })
    }
}

#[cfg(test)]
pub(in crate::cli::bytecode) static HITS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };

#[cfg(test)]
type BeforeLeafHook = Option<fn(bool)>;
#[cfg(test)]
type StoreObserverHook = Option<fn(*const EgclVal)>;
#[cfg(test)]
pub(in crate::cli::bytecode) static BEFORE_LEAF: egcl_rt::execution_local::ExecutionLocal<
    Cell<BeforeLeafHook>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };
#[cfg(test)]
pub(in crate::cli::bytecode) static STORE_OBSERVER: egcl_rt::execution_local::ExecutionLocal<
    Cell<StoreObserverHook>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };
#[cfg(test)]
pub(in crate::cli::bytecode) static PUBLISHED: egcl_rt::execution_local::ExecutionLocal<
    Cell<usize>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
