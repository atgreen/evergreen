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
//! Eligible published T1 object calls use a separate uncached candidate and
//! canonical per-call heat/dispatch. They never enter this T2 table or teach
//! generated probes; crossing promotion thresholds stays on general dispatch.
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
        /// Dispatch/profiling identity, distinct from the selected instance.
        symbol: u32,
        /// Immutable root choice captured after select validates the callable;
        /// the entry's collection/invalidation guards bound this cached ID.
        actual_instance: u32,
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
    retire_native_probes();
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

/// Invalidate weak generated probes before either heap movement or destruction
/// of descriptors they may still name. No execution can yield between its
/// checked hit and publishing the hidden callable in the callee's frame.
pub(in crate::cli::bytecode) fn retire_native_probes() {
    COLLECTION_STAMP.fetch_add(1, std::sync::atomic::Ordering::Release);
    #[cfg(test)]
    PROBE_RETIREMENTS.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
thread_local! {
    static PROBE_RETIREMENTS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn probe_retirements_for_test() -> usize {
    PROBE_RETIREMENTS.with(Cell::get)
}

/// The collection/descriptor-lease stamp (not a collection counter).
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
pub(in crate::cli::bytecode) fn native_target(
    function: EgclVal,
    nargs: usize,
) -> Option<(u32, Rc<TransferCode>, EgclVal, std::rc::Weak<NativeCode>)> {
    match lookup_or_fill(function, nargs)? {
        Target::Native {
            native,
            code,
            function: callable,
            symbol,
            owner,
            ..
        } if callable == function || code.accepts_explicit_callable(callable) => {
            // One descriptor/cell per shared compiled template, never one
            // permanently published descriptor for each ephemeral instance.
            let symbol = if code.native_callable_slot.is_some() {
                owner
            } else {
                symbol
            };
            Some((symbol, code, callable, Rc::downgrade(&native)))
        }
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

/// Normalize only already-selected identities; this never compiles, allocates
/// Lisp objects, polls, or resolves the current body from a mutable registry.
pub(in crate::cli::bytecode) fn closure_carrier(mut function: EgclVal) -> Option<EgclVal> {
    if let Some(symbol) = function.symbol_index() {
        function = egcl_rt::symbols::symbol_function(symbol)?;
    }
    if let Some(closure) = closure_cons(function) {
        let id = cp(closure).1.as_fixnum() as u64;
        super::super::super::settled_tree_closure(id)
    } else {
        Some(function)
    }
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
    // Mirror the named adapter's actual managed-entry preference. An
    // eligible dual-entry T2 object takes the ordinary bridge; advertising a
    // mapped offset alone no longer means mapped_callable will select it.
    // A function cell change moves the cache epoch as before.
    let child = code.mapped_entry().is_some()
        && !call_table::prefer_ordinary_child(&native, callable, false)
        && closure_captured_env(callable).is_none()
        && egcl_rt::symbols::symbol_function(name) == Some(callable);
    let actual_instance =
        if code.native_callable_slot.is_some() || egcl_rt::symbols::is_uninterned(symbol) {
            egcl_rt::function::definition_index(callable)
        } else {
            symbol
        };
    Some(Target::Native {
        native: Rc::clone(&native),
        owner,
        code: Rc::clone(code),
        function: callable,
        symbol,
        actual_instance,
        child,
    })
}

/// A read-only preparation result, deliberately absent from the T2 table and
/// native_target exports: T1 must credit heat on every committed invocation.
struct T1Candidate {
    symbol: u32,
    body: Arc<BytecodeFunction>,
    native: Rc<NativeCode>,
    actual_instance: u32,
}

fn t1_object_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EGCL_MAPPED_T1_FUNCALL").as_deref() != Some("0".as_ref()))
}

/// Pure unpublished preparation. No heat, completion poll, compiler request,
/// cache publication or Lisp allocation belongs here. Legacy non-template
/// uninterned objects have additional representative-owner aggregation; their
/// original route remains authoritative, as do cons/reified source wrappers.
fn t1_candidate(function: EgclVal, nargs: usize) -> Option<T1Candidate> {
    if !t1_object_enabled()
        || profiling_disabled()
        || !egcl_rt::function::is_interpreted_function(function)
        || super::super::super::reified_closure(function).is_some()
        || NATIVE_DEPTH.with(Cell::get) >= native_depth_cap()
    {
        return None;
    }
    let env = NATIVE_ENV.with(Cell::get);
    if env.is_null() || !unsafe { (&*env).locally_specials.is_empty() } {
        return None;
    }
    let name = egcl_rt::function::name(function).symbol_index()?;
    let symbol = name;
    let template = egcl_rt::function::is_closure_instance(function)
        || egcl_rt::function::is_closure_template(function);
    let identity_matches = if template {
        egcl_rt::function::definition_index(function) == symbol
    } else {
        !egcl_rt::symbols::is_uninterned(symbol)
            && egcl_rt::symbols::symbol_function(symbol) == Some(function)
    };
    if !identity_matches || t1_declined(symbol) || is_profile_pinned(symbol) {
        return None;
    }
    // Unlike bytecode_for_callable/registry_get, this query must NOT synchronize
    // a stale local generation, retire native code or mint a private registry.
    let body = REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())?;
    if template {
        if closure_bodies()
            .borrow()
            .get(&symbol)
            .is_none_or(|shared| !Arc::ptr_eq(shared, &body))
        {
            return None;
        }
    } else {
        let definition = named_definitions().borrow().get(&symbol).cloned()?;
        if definition.function != function
            || REGISTRY_GENERATION.with(|generation| generation.borrow().get(&symbol).copied())
                != Some(definition.generation)
            || definition
                .body
                .as_ref()
                .is_none_or(|shared| !Arc::ptr_eq(shared, &body))
        {
            return None;
        }
    }
    if !arity_accepts(&body, nargs)
        || CLOSURE_COMPILATION.with(|cache| {
            cache
                .borrow()
                .get(&(Arc::as_ptr(&body) as usize))
                .is_some_and(|owner| owner.owner != symbol)
        })
    {
        return None;
    }
    let native = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())?;
    if native.is_t2 || native.body.as_ref().is_none_or(|saved| !Arc::ptr_eq(saved, &body))
        // This includes the named T1 switch, fixed ordinary shape and the
        // next-invocation/backedge promotion boundary BEFORE any credit.
        || !call_table::prefer_ordinary_child(&native, function, t2_enabled())
    {
        return None;
    }
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        return None;
    };
    // The explicit receiver guard's private-body lookup is already satisfied
    // by the pure local/shared template checks above. Avoid its registry_get.
    if code.native_callable_slot.is_some()
        && (egcl_rt::function::definition_index(function) != symbol
            || (code.body.has_env && closure_captured_env(function).is_none()))
    {
        return None;
    }
    let actual_instance =
        if code.native_callable_slot.is_some() || egcl_rt::symbols::is_uninterned(symbol) {
            egcl_rt::function::definition_index(function)
        } else {
            symbol
        };
    Some(T1Candidate {
        symbol,
        body,
        native,
        actual_instance,
    })
}

/// Once credited, this call is consumed even if the bridge declines. Reentering
/// apply_function_impl would count it twice. Keep canonical completion/queue/
/// force policy, including a replacement that arrives after preparation.
fn run_t1(
    candidate: T1Candidate,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    let _call_depth = super::super::super::CallDepthGuard::enter()?;
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(args = EntryArgs::new(args));
    egcl_rt::rooted!(_active = super::super::ActiveCallableRoot(candidate.actual_instance));
    egcl_rt::rooted!(_body = super::super::ActiveBytecodeRoot::new(&candidate.body));
    // Retain the prepared code/helpers through the canonical poll. The
    // explicit body root above traces constants independently of registries.
    let _prepared_owner = candidate.native;
    record_callable_invocation(*function);
    // A concurrent definition setter may win after the pure snapshot. Once
    // counted, let canonical retained-object dispatch resolve that race, without
    // reentering apply_function_impl or crediting the invocation again.
    let name = egcl_rt::function::name(*function).as_symbol_index();
    if bytecode_for_callable(name, *function).is_none_or(|(symbol, body)| {
        symbol != candidate.symbol || !Arc::ptr_eq(&body, &candidate.body)
    }) {
        return call_registered(name, args.as_slice(), *function, env).unwrap_or_else(|| {
            run_with_sym(
                Arc::clone(&candidate.body),
                args.as_slice(),
                *function,
                candidate.symbol,
                env,
            )
        });
    }
    let count = dispatch_invoke_count(candidate.symbol, Some(*function));
    let selected = native_for_dispatch(candidate.symbol, Some(*function), count, &candidate.body);
    #[cfg(test)]
    FAST_CALLS.with(|counter| counter.set((counter.get().0 + 1, counter.get().1)));
    if let Some(native) = selected
        && native
            .body
            .as_ref()
            .is_some_and(|body| Arc::ptr_eq(body, &candidate.body))
        && NATIVE_DEPTH.with(Cell::get) < native_depth_cap()
    {
        if let NativeCodeStorage::Mapped(code) = &native._storage
            && fixed_tagged_parameters(&candidate.body)
            && candidate.body.self_call_sites.is_empty()
            && let Some(offset) = code.lean_root_entry()
            && let Some(result) = unsafe {
                mapped_ordinary::call(code, offset, *function, args.as_slice(), env, None)
            }
        {
            return result;
        }
        return run_native_callable(&native, candidate.symbol, *function, args.as_slice(), env);
    }
    run_with_sym(
        candidate.body,
        args.as_slice(),
        *function,
        candidate.symbol,
        env,
    )
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

/// Provenance of this dispatch, not merely whether CAPTURE is nonnull.
/// Generic Rust/ordinary reentry can retain a mapped context without owning a
/// currently published mapped call. Only `call(record)` constructs the second
/// variant, inside checked_call_published's published_mapped scope.
#[derive(Clone, Copy)]
enum Ingress {
    Root,
    PublishedMapped,
}

/// Run `target` on `args`, or `None`, having done nothing, when its builtin
/// declines them. The caller roots or scans every argument word.
fn run(
    target: Target,
    args: &[EgclVal],
    env: &mut Env,
    ingress: Ingress,
) -> Option<Result<EgclVal, EgclError>> {
    // The same call-depth accounting as `apply_function_impl`.
    let _call_depth = match super::super::super::CallDepthGuard::enter() {
        Ok(guard) => guard,
        Err(error) => return Some(Err(error)),
    };
    match target {
        Target::Native {
            code,
            function,
            actual_instance,
            ..
        } => {
            #[cfg(test)]
            FAST_CALLS.with(|count| count.set((count.get().0 + 1, count.get().1)));
            // Nothing has allocated since selection; the entry roots the
            // callable and its own copy of the actuals before anything can.
            egcl_rt::rooted!(
                _active_callable_root = super::super::ActiveCallableRoot(actual_instance)
            );
            // Reuse the existing native ABI only below the explicit mapped
            // publication. Preserve run_callable's declaration-scope handling
            // by declining when the caller has local SPECIAL declarations.
            if matches!(ingress, Ingress::PublishedMapped)
                && env.locally_specials.is_empty()
                && code.is_t2
                && let Some(offset) = code.lean_root_entry()
                && let Some(result) =
                    unsafe { mapped_ordinary::call(&code, offset, function, args, env, None) }
            {
                return Some(result);
            }
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
        return admitted(target, args.len(), admit)
            .and_then(|target| run(target, args, env, Ingress::Root));
    }
    // Selection may collect: keep the callable and the actuals rooted across
    // it, and run on the rooted copies.
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(args = args.to_vec());
    let target = admitted(fill(*function, args.len())?, args.len(), admit)?;
    run(target, &args, env, Ingress::Root)
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
/// runs it through [`call`]. Unpublished and noncollecting: a miss declines.
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
        || t1_candidate(function, rest.len()).is_some()
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
    // A positive T1 preparation must remain pure; only the preexisting T2
    // miss path may synchronize registries while this caller is published.
    t1_candidate(function, rest.len()).is_some()
        || lookup_or_fill(function, rest.len()).is_some_and(|target| !enters_as_child(&target))
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
    let target = lookup_or_fill(function, rest.len()).filter(|target| !enters_as_child(target));
    let env = NATIVE_ENV.with(Cell::get);
    if env.is_null() {
        return false;
    }
    // published_mapped intentionally exempts registered Rust outer records:
    // their words are host roots, not a newly published generated caller.
    let ingress = if unsafe { super::generated_mapped_ingress(record) } {
        Ingress::PublishedMapped
    } else {
        Ingress::Root
    };
    let t1 = if target.is_none() && matches!(ingress, Ingress::PublishedMapped) {
        t1_candidate(function, rest.len())
    } else {
        None
    };
    if target.is_none() && t1.is_none() {
        return false;
    }
    let declined = Cell::new(false);
    let result = guard_c2i(|| {
        if let Some(candidate) = t1 {
            // Only this private published ingress can consume a T1 candidate.
            // It cannot arm a closure word, table entry or generated probe.
            run_t1(candidate, function, rest, unsafe { &mut *env })
        } else {
            run(target.unwrap(), rest, unsafe { &mut *env }, ingress).unwrap_or_else(|| {
                declined.set(true);
                Ok(NIL)
            })
        }
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

#[cfg(test)]
#[path = "t1_object_tests.rs"]
mod t1_object_tests;
