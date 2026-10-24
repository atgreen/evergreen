// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FUNCALL of a function object from a mapped native caller (bliss-vwgb0).
//!
//! The general route forwards the call to the callable's own published entry,
//! which prepares again, declines (an ordinary-ABI body has no mapped child
//! entry) and then applies the object through `apply_function_impl`: designator
//! and wrapper decoding, `bytecode_for_callable`, the tier decision in
//! `native_for_dispatch`, and finally the same lean root entry a named call to
//! that body takes. Each step is correct and none is cheap; together they made
//! `(funcall f x y)` cost three times a named call of the same body.
//!
//! This module remembers, per execution, what that route selected for a few
//! recently called objects, so the FUNCALL cell's checked call can run it
//! directly. It caches only selections that the general route would make on
//! every later call, for as long as the entry stays valid:
//!
//! * installed, current T2 code for an interpreted function (a named DEFUN, a
//!   compiled closure, or the compiled form of a hot tree-walker closure),
//!   entered through `run_callable` as `run_native_callable` enters it: the
//!   lean ordinary root when the body has one, otherwise the general native
//!   activation, which binds `&optional`/`&rest`/`&key` and a captured
//!   environment. An argument count outside the lambda list keeps the general
//!   route, so every arity error is signalled exactly as before;
//! * the direct entry of a builtin behind its `#'name` wrapper, re-checked on
//!   every call exactly as the general route re-checks it.
//!
//! A mapped caller's FUNCALL still forwards to the callable's entry when that
//! entry would run the target as a child activation in the caller's segment
//! (the current definition of a name, with a mapped entry); this module does
//! not replace that route. The callable
//! entry's fallback adapter, the ordinary-ABI callable veneer and Rust callers
//! of `apply_function` consult the same table; a Rust caller outside any entry
//! runs only native targets this way, since their entry polls and owns a
//! segment, and leaves builtins to the published entry.
//!
//! An entry names its object by identity without rooting it. Every collection
//! clears the table (the root scanner calls [`clear_for_collection`]), so no
//! entry can outlive, or be confused with a successor at the same address of,
//! the object it describes. The call table's invalidation epoch changes
//! whenever native code is published or a function cell or tier is
//! invalidated, and a changed epoch misses. A hit also re-checks that its code
//! is still the registry's installed version for its owner (deopt blacklisting
//! and profiler pins remove it without invalidating), that it does not need
//! recompilation, and that the native depth cap allows another activation.

use super::*;
use egcl_compiler::t2::native_transfer::MappedCallRecord;

/// What the general route would run for the cached object.
#[derive(Clone)]
enum Target {
    /// Installed, current T2 code, entered as `run_native_callable` enters it.
    Native {
        /// The registry entry selected, re-checked on every call: deopt
        /// blacklisting and profiler pins remove it without an invalidation.
        native: Rc<NativeCode>,
        owner: u32,
        code: Rc<TransferCode>,
        /// The callable identity the general route hands to the entry. For a
        /// tree-walker closure this is its compiled object, not the wrapper.
        function: EgclVal,
        /// The dispatch symbol whose private roots stay traced while it runs.
        symbol: u32,
        /// Whether a mapped caller's FUNCALL enters this body as a child in
        /// its own segment (`call_table::mapped_callable` accepts it).
        child: bool,
    },
    /// A builtin reached through its `#'name` wrapper.
    Builtin { symbol: u32 },
}

struct Entry {
    function: Cell<u64>,
    revision: Cell<u64>,
    target: std::cell::RefCell<Option<Target>>,
}

struct Table {
    entries: [Entry; 4],
    next: Cell<usize>,
    /// Bumped by every collection, so a fill that spans one is discarded.
    collections: Cell<u64>,
}

static TABLE: egcl_rt::execution_local::ExecutionLocal<Table> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| Table {
        entries: std::array::from_fn(|_| Entry {
            function: Cell::new(NIL.0),
            revision: Cell::new(RETIRED),
            target: std::cell::RefCell::new(None),
        }),
        next: Cell::new(0),
        collections: Cell::new(0),
    })
};

#[cfg(test)]
thread_local! {
    static FAST_CALLS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// (native, builtin) FUNCALLs this module ran, for the structural tests.
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_fast_calls_for_test() -> (usize, usize) {
    FAST_CALLS.with(|count| count.replace((0, 0)))
}

/// Whether `function` currently has a cached target, for the structural tests.
#[cfg(test)]
pub(in crate::cli::bytecode) fn is_cached_for_test(function: EgclVal) -> bool {
    lookup(function).is_some()
}

/// Called by the bytecode root scanner with every execution stopped.
///
/// # Safety
/// As for `ExecutionLocal::scan`: no execution may be using its table.
pub(in crate::cli::bytecode) unsafe fn clear_for_collection() {
    // Generated FUNCALL probes record this stamp with the raw object bits
    // they compare; a collection retires every such entry at once.
    COLLECTION_STAMP.fetch_add(1, std::sync::atomic::Ordering::Release);
    unsafe {
        TABLE.scan(|table| {
            table
                .collections
                .set(table.collections.get().wrapping_add(1));
            for entry in &table.entries {
                // Unreachable until refilled. The target itself is released by
                // that refill, never here: dropping code is not scanner work.
                entry.function.set(NIL.0);
                entry.revision.set(RETIRED);
            }
        });
    }
}

/// Moved by every collection. See `collection_stamp_address`.
static COLLECTION_STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current collection stamp.
pub(in crate::cli::bytecode) fn collection_stamp() -> u64 {
    COLLECTION_STAMP.load(std::sync::atomic::Ordering::Acquire)
}

/// The collection stamp word, for the generated FUNCALL probe.
pub(in crate::cli::bytecode) fn collection_stamp_address() -> usize {
    COLLECTION_STAMP.as_ptr() as usize
}

/// The installed T2 code the general route runs for `function` with `nargs`
/// actuals, when that is `function`'s own code (not a wrapper's or a
/// tree-walker closure's compiled object), with the symbol owning it.
/// Selection may fill the table; it does not collect.
pub(in crate::cli::bytecode) fn native_target(function: EgclVal, nargs: usize) -> Option<(u32, Rc<TransferCode>)> {
    match lookup_or_fill(function, nargs)? {
        Target::Native { code, function: callable, symbol, .. } if callable == function => Some((symbol, code)),
        _ => None,
    }
}

/// An entry revision no epoch reaches: the entry matches nothing.
const RETIRED: u64 = u64::MAX;

/// The cached target for `function`, if one is current. Allocation-free.
fn lookup(function: EgclVal) -> Option<Target> {
    let epoch = egcl_rt::call_table::invalidation_epoch();
    TABLE.with(|table| {
        table
            .entries
            .iter()
            .find(|entry| entry.function.get() == function.0 && entry.revision.get() == epoch)
            .and_then(|entry| entry.target.borrow().clone())
    })
}

/// Select the target the general route would run, and remember it.
/// Selection reads Rust-side registries only and should never collect; if a
/// collection does intervene, the selected target's callable may have moved,
/// so this declines rather than run it, and the caller takes the general
/// route. The caller roots `function` and publishes any native caller.
fn fill(function: EgclVal, nargs: usize) -> Option<Target> {
    let collections = TABLE.with(|table| table.collections.get());
    let epoch = egcl_rt::call_table::invalidation_epoch();
    let target = select(function, nargs)?;
    TABLE.with(|table| {
        if table.collections.get() != collections {
            return None;
        }
        let index = table.next.get();
        table.next.set((index + 1) % table.entries.len());
        let entry = &table.entries[index];
        entry.function.set(function.0);
        entry.revision.set(epoch);
        // The previous target, if any, drops here, in its own execution.
        drop(entry.target.replace(Some(target.clone())));
        Some(target)
    })
}

fn lookup_or_fill(function: EgclVal, nargs: usize) -> Option<Target> {
    lookup(function)
        .or_else(|| fill(function, nargs))
        .filter(|target| admissible(target, nargs))
}

/// The closure cons behind `function`, as `apply_function_impl` decodes it.
fn closure_cons(function: EgclVal) -> Option<EgclVal> {
    if function.is_cons() {
        return super::super::super::is_closure_cons(function).then_some(function);
    }
    super::super::super::reified_closure(function)
}

fn select(function: EgclVal, nargs: usize) -> Option<Target> {
    if profiling_disabled() {
        return None;
    }
    let mut callable = function;
    if let Some(closure) = closure_cons(function) {
        let id = cp(closure).1.as_fixnum() as u64;
        if super::super::super::generic_wrapper_named(id) {
            return None;
        }
        if let Some(symbol) = super::super::super::builtin_wrapper_symbol(id) {
            // Re-checked per call; only remember a builtin that answers now.
            super::super::super::direct_builtin_slot_memoized(symbol, nargs)?;
            return Some(Target::Builtin { symbol });
        }
        // A hot tree-walker closure dispatches to its compiled object once its
        // single compilation attempt has settled.
        callable = super::super::super::settled_tree_closure(id)?;
        // The general route counts invocations of the outer object below T2.
        // Its compiled object is T2 by the checks below, so nothing is counted.
    }
    // A captured environment does not change the route: `run_callable` binds
    // it for a body that reads one, exactly as `run_native_callable` does.
    if !egcl_rt::function::is_interpreted_function(callable)
        || super::super::super::reified_closure(callable).is_some()
    {
        return None;
    }
    let name = egcl_rt::function::name(callable).symbol_index()?;
    let (symbol, body) = bytecode_for_callable(name, callable)?;
    if !arity_accepts(&body, nargs)
        || t1_declined(symbol)
        || closure_body_declined(symbol, &body)
        || is_profile_pinned(symbol)
    {
        return None;
    }
    // `closure_compilation_owner` keeps a previous owner only while it is
    // unpinned and still registered with this exact body.
    let owner = CLOSURE_COMPILATION
        .with(|cache| {
            cache
                .borrow()
                .get(&(Arc::as_ptr(&body) as usize))
                .map(|state| state.owner)
        })
        .filter(|&owner| {
            !is_profile_pinned(owner)
                && registry_get(owner).is_some_and(|entry| Arc::ptr_eq(&entry, &body))
        })
        .unwrap_or(symbol);
    let native = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&owner).cloned())?;
    if !native.is_t2
        || native
            .body
            .as_ref()
            .is_none_or(|selected| !Arc::ptr_eq(selected, &body))
    {
        return None;
    }
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        return None;
    };
    if code.needs_recompile() {
        return None;
    }
    // `call_table::mapped_callable` admits only the current, uncaptured
    // definition of its name; a function cell change moves the epoch.
    let child = code.mapped_entry().is_some()
        && closure_captured_env(callable).is_none()
        && egcl_rt::symbols::symbol_function(name) == Some(callable);
    Some(Target::Native {
        native: Rc::clone(&native),
        owner,
        code: Rc::clone(code),
        function: callable,
        symbol,
        child,
    })
}

fn invocation(record: *mut MappedCallRecord) -> &'static [EgclVal] {
    let invocation = unsafe { &*(*record).context };
    if invocation.nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(invocation.args, invocation.nargs) }
    }
}

fn admissible(target: &Target, nargs: usize) -> bool {
    match target {
        // `call_registered` hands an argument count outside the lambda list
        // to the evaluator, which signals the standard PROGRAM-ERROR.
        Target::Native {
            native,
            owner,
            code,
            ..
        } => {
            arity_accepts(&code.body, nargs)
                && !code.needs_recompile()
                && NATIVE_DEPTH.with(|depth| depth.get()) < native_depth_cap()
                && NATIVE_REGISTRY.with(|registry| {
                    registry
                        .borrow()
                        .get(owner)
                        .is_some_and(|current| Rc::ptr_eq(current, native))
                })
        }
        Target::Builtin { .. } => true,
    }
}

/// Run `target` on `args`, or `None`, having done nothing, when its builtin
/// declines them. The caller roots or scans every argument word.
fn run(target: Target, args: &[EgclVal], env: &mut Env) -> Option<Result<EgclVal, EgclError>> {
    // The same call-depth accounting as `apply_function_impl`.
    let _call_depth = match super::super::super::CallDepthGuard::enter() {
        Ok(guard) => guard,
        Err(error) => return Some(Err(error)),
    };
    match target {
        Target::Native {
            code,
            function,
            symbol,
            ..
        } => {
            #[cfg(test)]
            FAST_CALLS.with(|count| count.set((count.get().0 + 1, count.get().1)));
            // Nothing has allocated since selection; the entry roots the
            // callable and its own copy of the actuals before anything can.
            egcl_rt::rooted!(_active_callable_root = super::super::ActiveCallableRoot(symbol));
            Some(code.run_callable(function, args, env))
        }
        Target::Builtin { symbol } => {
            // The general route asks again on every call: a definition
            // installed for the name, or an arity outside the kernel, declines.
            let slot = super::super::super::direct_builtin_slot_memoized(symbol, args.len())?;
            #[cfg(test)]
            FAST_CALLS.with(|count| count.set((count.get().0, count.get().1 + 1)));
            // Kernels may allocate before they finish reading their arguments;
            // `apply_function_impl` hands them rooted copies, and so does this.
            const INLINE: usize = 4;
            egcl_rt::rooted!(inline = [NIL; INLINE]);
            egcl_rt::rooted!(spilled = Vec::<EgclVal>::new());
            let args: &[EgclVal] = if args.len() <= INLINE {
                inline[..args.len()].copy_from_slice(args);
                &inline[..args.len()]
            } else {
                spilled.extend_from_slice(args);
                &spilled
            };
            // A kernel that declines its arguments has done nothing yet.
            super::super::super::call_direct_builtin(slot, args, env)
        }
    }
}

/// Which cached targets a caller of [`apply`] may run itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::bytecode) enum Admit {
    /// Inside a published callable entry, which has already polled and owns a
    /// segment: any target, including a builtin's kernel.
    Any,
    /// A Rust caller outside any entry: only native code, whose own entry
    /// polls and owns its segment. A builtin keeps the published entry.
    Native,
}

/// `apply_function_impl` for a function object with a current cached target,
/// or `None`, having done nothing, so the caller takes the general route.
pub(in crate::cli::bytecode) fn apply(
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
    admit: Admit,
) -> Option<Result<EgclVal, EgclError>> {
    if let Some(target) = lookup(function) {
        return admitted(target, args.len(), admit).and_then(|target| run(target, args, env));
    }
    // Selection may collect: keep the callable and the actuals rooted across
    // it, and run on the rooted copies.
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(args = args.to_vec());
    let target = admitted(fill(*function, args.len())?, args.len(), admit)?;
    run(target, &args, env)
}

fn admitted(target: Target, nargs: usize, admit: Admit) -> Option<Target> {
    (admissible(&target, nargs)
        && !(admit == Admit::Native && matches!(target, Target::Builtin { .. })))
    .then_some(target)
}

/// A target a mapped caller's FUNCALL leaves to the callable's own entry:
/// the current definition of a name, with a mapped entry, runs there as a
/// child in the caller's segment.
fn enters_as_child(target: &Target) -> bool {
    matches!(target, Target::Native { child: true, .. })
}

/// Whether preparation may leave this FUNCALL to the checked call, which then
/// runs it through [`call`]. Allocation-free and unpublished: a miss declines.
///
/// # Safety
/// `record` is the live adapter record of the builtin FUNCALL cell.
pub(in crate::cli::bytecode) unsafe fn cached(record: *mut MappedCallRecord) -> bool {
    let args = invocation(record);
    let Some((&function, rest)) = args.split_first() else {
        return false;
    };
    lookup(function)
        .is_some_and(|target| admissible(&target, rest.len()) && !enters_as_child(&target))
}

/// [`cached`], filling the table first on a miss. The caller is published, and
/// its scanned activation holds the arguments.
///
/// # Safety
/// As for [`cached`].
pub(in crate::cli::bytecode) unsafe fn prepare(record: *mut MappedCallRecord) -> bool {
    let args = invocation(record);
    let Some((&function, rest)) = args.split_first() else {
        return false;
    };
    lookup_or_fill(function, rest.len()).is_some_and(|target| !enters_as_child(&target))
}

/// Run a builtin FUNCALL's cached target from the checked call. Returns
/// `false`, having done nothing, when there is no current admissible target;
/// the general checked call then runs it.
///
/// # Safety
/// As for `checked_call_published`, whose publication must be live, for the
/// builtin FUNCALL cell. Its scanned activation holds the arguments.
pub(in crate::cli::bytecode) unsafe fn call(record: *mut MappedCallRecord) -> bool {
    let args = invocation(record);
    let Some((&function, rest)) = args.split_first() else {
        return false;
    };
    let Some(target) =
        lookup_or_fill(function, rest.len()).filter(|target| !enters_as_child(target))
    else {
        return false;
    };
    let env = NATIVE_ENV.with(Cell::get);
    if env.is_null() {
        return false;
    }
    let declined = Cell::new(false);
    let result = guard_c2i(|| {
        run(target, rest, unsafe { &mut *env }).unwrap_or_else(|| {
            declined.set(true);
            Ok(NIL)
        })
    });
    if declined.get() {
        return false;
    }
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            stash_native_error(error);
            NIL
        }
    };
    unsafe {
        (*record).outcome = NativeOutcome {
            value,
            exit: if native_error_pending() {
                NativeExit::Transfer
            } else {
                NativeExit::Returned
            },
        };
    }
    true
}
