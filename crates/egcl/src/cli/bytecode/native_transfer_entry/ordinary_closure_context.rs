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
        if lexical_identity::encoded(name) {
            Env::set_lexical_frame(&frame, name, args[2]);
        } else {
            Env::set_frame_var(&frame, name, args[2]);
        }
        Ok(NIL)
    } else {
        let value = if lexical_identity::encoded(name) {
            Env::lookup_lexical_frame(&frame, name)
        } else {
            Env::lookup_resolved_frame(&frame, name, symbol)
        };
        value.ok_or_else(|| {
            EgclError::UnboundVariable(lexical_identity::resolve(name).unwrap_or(NIL))
        })
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
            if callable != args[0] {
                return None;
            }
            let frame = captured_frame(owner, callable)?;
            let index = usize::try_from(args[1].as_fixnum()).ok()?;
            let name = owner.body.names.get(index)?;
            let symbol = owner.leaf_symbol(index);
            let answer = if store {
                let updated = if lexical_identity::encoded(name) {
                    Env::set_lexical_frame(&frame, name, args[2])
                } else {
                    Env::set_resolved_frame_var(&frame, name, symbol, args[2])
                };
                if !updated {
                    return None;
                }
                NIL
            } else if lexical_identity::encoded(name) {
                Env::lookup_lexical_frame(&frame, name)?
            } else {
                Env::lookup_resolved_frame(&frame, name, symbol)?
            };
            #[cfg(test)]
            HITS.with(|hits| hits.set(hits.get() + 1));
            Some(answer)
        })
    }
}

/// One validated (callable, code) pair and the frame it captured.
struct FrameEntry {
    callable: Cell<u64>,
    owner: Cell<usize>,
    body: Cell<usize>,
    epoch: Cell<u64>,
    stamp: Cell<u64>,
    /// Weak: the cache must never keep a dead closure's frame alive.
    frame: std::cell::RefCell<std::sync::Weak<FrameCell>>,
}

struct FrameCache {
    entries: [FrameEntry; 4],
    next: Cell<usize>,
}

/// Captured frames of recently accessed closures (bliss-gg3i1.7.3). An entry
/// names its callable by raw bits without rooting it: the collection stamp
/// moves on every collection (and on descriptor destruction), so an entry can
/// neither outlive nor be confused with a successor of the object it names.
/// The invalidation epoch moves on every redefinition, publication and tier
/// change, as for the FUNCALL caches. Only the stamp-free answer of
/// `accepts_explicit_callable` and `closure_captured_env` is remembered; both
/// depend only on the callable's definition identity and the code's body.
static FRAMES: egcl_rt::execution_local::ExecutionLocal<FrameCache> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| FrameCache {
        entries: std::array::from_fn(|_| FrameEntry {
            callable: Cell::new(NIL.0),
            owner: Cell::new(0),
            body: Cell::new(0),
            epoch: Cell::new(u64::MAX),
            stamp: Cell::new(u64::MAX),
            frame: std::cell::RefCell::new(std::sync::Weak::new()),
        }),
        next: Cell::new(0),
    })
};

/// `closure_captured_env(callable)` once `owner` accepts `callable` as its
/// explicit callable. Never allocates Lisp objects or collects.
fn captured_frame(owner: &TransferCode, callable: EgclVal) -> Option<Arc<FrameCell>> {
    let epoch = egcl_rt::call_table::invalidation_epoch();
    let stamp = super::funcall_fast::collection_stamp();
    let owner_address = std::ptr::from_ref(owner) as usize;
    let body = Arc::as_ptr(&owner.body) as usize;
    let hit = FRAMES.with(|cache| {
        cache
            .entries
            .iter()
            .find(|entry| {
                entry.callable.get() == callable.0
                    && entry.owner.get() == owner_address
                    && entry.body.get() == body
                    && entry.epoch.get() == epoch
                    && entry.stamp.get() == stamp
            })
            .and_then(|entry| entry.frame.borrow().upgrade())
    });
    if hit.is_some() {
        return hit;
    }
    if !owner.accepts_explicit_callable(callable) {
        return None;
    }
    let frame = closure_captured_env(callable)?;
    // Selection reads Rust registries only; a change during it misses.
    if egcl_rt::call_table::invalidation_epoch() != epoch
        || super::funcall_fast::collection_stamp() != stamp
    {
        return Some(frame);
    }
    FRAMES.with(|cache| {
        let index = cache.next.get();
        cache.next.set((index + 1) % cache.entries.len());
        let entry = &cache.entries[index];
        entry.callable.set(callable.0);
        entry.owner.set(owner_address);
        entry.body.set(body);
        entry.epoch.set(epoch);
        entry.stamp.set(stamp);
        *entry.frame.borrow_mut() = Arc::downgrade(&frame);
    });
    Some(frame)
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
