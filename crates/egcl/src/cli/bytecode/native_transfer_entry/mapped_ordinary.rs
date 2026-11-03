// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reusable ordinary call state below a published mapped activation.
//!
//! The mapped caller remains published for the entire call. The existing
//! ordinary assembly shim owns a *new per-call segment and cold landing*, but
//! no managed callee frame is pushed. All ordinary frames therefore reconstruct
//! as native children. In particular, their escape cannot discard the mapped
//! parent's catches, bindings or Rust publication frame.
use super::*;
use egcl_rt::gc::TraceHostRoots;

pub(super) type Lanes = Vec<Lease>;

/// Only the live parent owns code and Lisp roots. Idle storage has neither a
/// strong code edge nor an ActiveBytecodeRoot, avoiding a code/pool cycle.
pub(super) struct Lease {
    pub(super) code: Rc<TransferCode>,
    body: super::super::ActiveBytecodeRoot,
    storage: Option<std::pin::Pin<Box<Bridge>>>,
}

/// Execution-local through its TransferCode owner. Only pristine storage with
/// no owning code edge or Lisp values is retained, at most four homes per code.
#[derive(Default)]
pub(super) struct Pool {
    idle: RefCell<Vec<std::pin::Pin<Box<Bridge>>>>,
    retired: Cell<bool>,
}
impl Pool {
    pub(super) fn retire(&self) {
        self.retired.set(true);
        self.idle.borrow_mut().clear();
    }
}

/// Stable context home. A clean pooled home may remain initialized, with only
/// nonowning pointers into its exact owning code and pinned backing. Parent,
/// callable, execution-control and transient landing bindings are detached.
pub(super) struct ContextHome {
    slot: Box<std::mem::MaybeUninit<CaptureContext>>,
    live: bool,
}
impl ContextHome {
    fn new() -> Self {
        Self {
            slot: Box::new(std::mem::MaybeUninit::uninit()),
            live: false,
        }
    }
    fn initialize(&mut self, context: CaptureContext) {
        self.clear();
        self.slot.write(context);
        self.live = true;
    }
    pub(super) fn get_mut(&mut self) -> &mut CaptureContext {
        assert!(self.live);
        unsafe { self.slot.assume_init_mut() }
    }
    fn clear(&mut self) {
        if self.live {
            self.live = false;
            #[cfg(test)]
            // Registry teardown can drop code after execution-local counters
            // have retired. Live-phase reads/resets remain strict.
            let _ = CONTEXT_DROPS.try_with(|count| count.set(count.get() + 1));
            // No native frame or generated publication can retain this home.
            unsafe { self.slot.assume_init_drop() };
        }
    }
}
impl std::ops::Deref for ContextHome {
    type Target = CaptureContext;
    fn deref(&self) -> &CaptureContext {
        assert!(self.live);
        unsafe { self.slot.assume_init_ref() }
    }
}
impl std::ops::DerefMut for ContextHome {
    fn deref_mut(&mut self) -> &mut CaptureContext {
        assert!(self.live);
        unsafe { self.slot.assume_init_mut() }
    }
}
impl Drop for ContextHome {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Pinned code-free backing; all generated pointers refer into this storage,
/// never into a movable Lease. It owns no Lisp roots once detached.
pub(super) struct Bridge {
    pub(super) state: Option<Box<ActivationState>>,
    pub(super) children: Box<SegmentActivations>,
    pub(super) context: ContextHome,
    pub(super) function: EgclVal,
    pub(super) args: EntryArgs,
    pub(super) instance: u32,
    pub(super) busy: bool,
    pub(super) generated: Option<egcl_compiler::t2::native_transfer::GeneratedLane>,
    pub(super) dispatch_function: EgclVal,
    clean_return: bool,
}

impl TraceHostRoots for Lease {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.body.trace_host_roots(visit);
        let bridge = self.storage.as_mut().unwrap().as_mut().get_mut();
        if let Some(state) = &mut bridge.state {
            state.trace_host_roots(visit);
        }
        bridge.children.trace_host_roots(visit);
        visit(&mut bridge.function);
        visit(&mut bridge.dispatch_function);
        bridge.args.trace_host_roots(visit);
        if bridge.busy {
            super::super::ActiveCallableRoot(bridge.instance).trace_host_roots(visit);
        }
    }
}

fn pooling_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EGCL_MAPPED_BRIDGE_POOL").as_deref() != Some("0".as_ref()))
}

pub(in crate::cli::bytecode) fn overflow_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("EGCL_MAPPED_BRIDGE_OVERFLOW").as_deref() != Some("0".as_ref())
    })
}

#[cfg(test)]
static UNCACHED_CALLS: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_uncached_calls_for_test() -> (usize, usize) {
    UNCACHED_CALLS.with(|slot| slot.replace((0, 0)))
}
#[cfg(test)]
static CACHE_SHAPE: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_cache_shape_for_test() -> (usize, usize) {
    CACHE_SHAPE.with(|slot| slot.replace((0, 0)))
}

#[cfg(test)]
fn record_cache_shape(lanes: &Lanes) {
    assert!(
        lanes
            .iter()
            .skip(mapped_generated::MAX_GENERATED_LANES)
            .all(|lease| lease.bridge().generated.is_none())
    );
    let generated = lanes
        .iter()
        .filter(|lease| lease.bridge().generated.is_some())
        .count();
    assert!(generated <= mapped_generated::MAX_GENERATED_LANES);
    CACHE_SHAPE.with(|slot| {
        let (length, bindings) = slot.get();
        slot.set((length.max(lanes.len()), bindings.max(generated)));
    });
}

/// Reuse the complete scalar context, independently of backing allocation reuse.
fn context_reuse_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("EGCL_MAPPED_BRIDGE_CONTEXT_REUSE").as_deref() != Some("0".as_ref())
    })
}

#[path = "mapped_ordinary_context.rs"]
mod context_reuse;

impl Lease {
    fn new(code: Rc<TransferCode>, env: &mut Env, frame: *mut Frame) -> Self {
        #[cfg(test)]
        PREPARES.with(|count| count.set(count.get() + 1));
        let cached = if pooling_enabled() && !code.bridge_pool.retired.get() {
            code.bridge_pool.idle.borrow_mut().pop()
        } else {
            None
        };
        let storage_reused = cached.is_some();
        let mut storage = cached.unwrap_or_else(|| {
            #[cfg(test)]
            STORAGE_BIRTHS.with(|count| count.set(count.get() + 1));
            Box::pin(Bridge {
                state: Some(ActivationState::native_child(env)),
                children: Box::default(),
                context: ContextHome::new(),
                function: NIL,
                args: EntryArgs::new(&[]),
                instance: u32::MAX,
                busy: false,
                generated: None,
                dispatch_function: NIL,
                clean_return: false,
            })
        });
        let bridge = storage.as_mut().get_mut();
        let state = bridge.state.as_mut().unwrap();
        state.catch_base = env.catch_stack.len();
        let constructed = !bridge.context.live;
        if constructed {
            let mut context =
                state.context(&code, frame, &mut *bridge.children, std::ptr::null_mut());
            context.ordinary_bridge = true;
            context.activation = std::ptr::null_mut();
            context.bridge_clean_return = &mut bridge.clean_return;
            bridge.context.initialize(context);
        } else {
            debug_assert!(context_reuse_enabled());
            bridge.context.attach(&code, env, frame);
        }
        // Body-root acquisition remains per active checkout. The idle core
        // owns neither this token nor an Rc<TransferCode> (no pool cycle).
        let body = super::super::ActiveBytecodeRoot::new(&code.body);
        if constructed {
            #[cfg(test)]
            CONTEXT_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
            // This diagnostic counts construction, not checkout of a retained
            // context. Backing reuse is a separate property of a construction.
            crate::cli::activation_census::bridge_lease(storage_reused, || {
                crate::cli::activation_census::Shape {
                    owner: code.installed_symbol,
                    t2: code.is_t2,
                    ordinary: code.ordinary_entry.is_some(),
                    captured: code.body.has_env,
                    self_calls: !code.body.self_call_sites.is_empty(),
                    osr: !code.osr_entries.is_empty(),
                    variadic: code.body.variadic,
                }
            });
        }
        Self {
            code,
            body,
            storage: Some(storage),
        }
    }
    pub(super) fn bridge(&self) -> &Bridge {
        self.storage.as_ref().unwrap().as_ref().get_ref()
    }
    fn bridge_mut(&mut self) -> &mut Bridge {
        self.storage.as_mut().unwrap().as_mut().get_mut()
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut storage = self.storage.take().unwrap();
        let bridge = storage.as_mut().get_mut();
        let context = &*bridge.context;
        let clean = !std::thread::panicking()
            && bridge.clean_return
            && !bridge.busy
            && bridge.children.is_empty()
            && bridge.state.as_ref().unwrap().is_pristine_native_child()
            && context.selected.is_none()
            && context.failure.is_none()
            && context.osr.is_none()
            && context.recursive.is_null()
            && !context.recursive_escape
            && !context.completed_deopt
            && context.completed_cleanup.is_none();
        if !clean {
            // Preserve the old dirty destruction order: state (including
            // outstanding specials), children, then the still-live context.
            // The live lease keeps code/body valid until all three drop.
            return;
        }
        // Parent generated bindings have already retired before lane storage.
        // Drop controls/publication before exposing the backing to another lease.
        bridge.generated = None;
        if context_reuse_enabled() {
            bridge.context.park();
        } else {
            bridge.context.clear();
        }
        bridge.function = NIL;
        bridge.dispatch_function = NIL;
        bridge.args = EntryArgs::new(&[]);
        bridge.instance = u32::MAX;
        bridge.clean_return = false;
        bridge.state.as_mut().unwrap().catch_base = 0;
        if pooling_enabled() && !self.code.bridge_pool.retired.get() {
            let mut pool = self.code.bridge_pool.idle.borrow_mut();
            if pool.len() < 4 {
                pool.push(storage);
            }
        }
        // Native-child backing never enters the general managed-state pool.
    }
}

#[cfg(test)]
static ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
static PREPARES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
// PREPARES counts live lane checkouts, including retained-context hits.
// STORAGE_BIRTHS counts the four backing allocations only, never context reset.
#[cfg(test)]
static CONTEXT_DROPS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
fn take_context_drops_for_test() -> usize {
    CONTEXT_DROPS.with(|count| count.replace(0))
}
#[cfg(test)]
static CONTEXT_CONSTRUCTIONS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
fn take_context_constructions_for_test() -> usize {
    CONTEXT_CONSTRUCTIONS.with(|count| count.replace(0))
}
#[cfg(test)]
static STORAGE_BIRTHS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_storage_births_for_test() -> usize {
    STORAGE_BIRTHS.with(|count| count.replace(0))
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn idle_storage_for_test(code: &TransferCode) -> (usize, usize) {
    let pool = code.bridge_pool.idle.borrow();
    (
        pool.len(),
        pool.first().map_or(0, |bridge| {
            bridge.as_ref().get_ref() as *const Bridge as usize
        }),
    )
}

pub(super) fn entry_counter() -> *mut usize {
    #[cfg(test)]
    {
        ENTRIES.with(Cell::as_ptr)
    }
    #[cfg(not(test))]
    {
        std::ptr::null_mut()
    }
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_entries_for_test() -> usize {
    ENTRIES.with(|count| count.replace(0))
}
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_prepares_for_test() -> usize {
    PREPARES.with(|count| count.replace(0))
}

/// Restore execution pointers and depth even when a checked Rust helper panics.
/// The parent roots the bridge throughout; this guard is not another root list.
struct Restore {
    capture_slot: *const Cell<*mut CaptureContext>,
    previous: *mut CaptureContext,
    bridge: *mut Bridge,
    depth: *mut u32,
    old_depth: u32,
}
impl Drop for Restore {
    fn drop(&mut self) {
        unsafe {
            (*self.capture_slot).set(self.previous);
            *self.depth = self.old_depth;
            (*self.bridge).busy = false;
            (*self.bridge).function = NIL;
            (*self.bridge).args = EntryArgs::new(&[]);
            (*self.bridge).state.as_mut().unwrap().env_frame = None;
        }
    }
}

/// The caller has already performed lean_native_target's exact current-target,
/// arity, depth and entry-cohort checks. It keeps published_mapped live around
/// this function. None means no state changed and the old root entry must run.
pub(super) unsafe fn call(
    code: &Rc<TransferCode>,
    offset: usize,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
    cell: Option<u64>,
) -> Option<Result<EgclVal, EgclError>> {
    use crate::cli::activation_census::AdmissionReason as Reason;
    let decline = |reason| {
        crate::cli::activation_census::admission(reason, || {
            (
                code.installed_symbol,
                Some(code.census_shape()),
                Some(code.census_entry()),
            )
        });
        None
    };
    let parent = CAPTURE.with(Cell::get);
    if parent.is_null() {
        return decline(Reason::BridgeNoContext);
    }
    if !current_poll_idle() {
        return decline(Reason::BridgePollPending);
    }
    if native_error_pending() {
        return decline(Reason::BridgeNativeError);
    }
    // Same ordered checks as before. These reasons describe the context's
    // capabilities; caller identity comes only from the checked request's PC.
    if unsafe { (*parent).ordinary_bridge } {
        return decline(Reason::BridgeContextIsBridge);
    }
    if unsafe { (*(*parent).owner).ordinary_entry.is_some() } {
        return decline(Reason::BridgeContextHasOrdinaryEntry);
    }
    if !code.body.self_call_sites.is_empty() {
        return decline(Reason::BridgeSelfCalls);
    }
    let slot = unsafe { (*parent).mapped_ordinary };
    if slot.is_null() {
        return decline(Reason::BridgeNoLanes);
    }
    if args.len() != usize::from(code.body.arity) {
        return decline(Reason::BridgeArity);
    }
    if code.native_callable_slot.is_some() && !code.accepts_explicit_callable(function) {
        return decline(Reason::BridgeReceiver);
    }
    let Some(execution) = egcl_rt::thread::current_execution_context_for_native() else {
        return decline(Reason::BridgeNoExecution);
    };
    crate::cli::activation_census::admission(Reason::BridgeEligible, || {
        (
            code.installed_symbol,
            Some(code.census_shape()),
            Some(code.census_entry()),
        )
    });
    // No Rust borrow of the GC-scanned cache survives an allocating callee.
    // Generated pointers remain limited to their first four stable slots.
    // Append-only Rust spill leases reuse context/body roots within a parent;
    // more-polymorphic calls retain the rooted uncached ABI below this bound.
    const RUST_LANES: usize = 16;
    let cache_limit = if overflow_enabled() {
        RUST_LANES
    } else {
        mapped_generated::MAX_GENERATED_LANES
    };
    let cached = unsafe {
        (&*slot)
            .iter()
            .position(|bridge| Rc::ptr_eq(&bridge.code, code))
    };
    let index = match cached {
        Some(index) => {
            if unsafe { (&*slot)[index].bridge().busy } {
                crate::cli::activation_census::note_refusal(
                    crate::cli::activation_census::Refusal::Busy,
                    unsafe { (*(*parent).owner).installed_symbol },
                    code.installed_symbol,
                    code.is_t2,
                );
                return None;
            }
            index
        }
        None => {
            if unsafe { (&*slot).len() } == cache_limit {
                if overflow_enabled() {
                    // This call owns an uncached lease, not another generated
                    // lane. Its precise host root replaces parent-lane tracing;
                    // pinned backing and code/body ownership span all native
                    // execution and cold recovery. run's Restore retires the
                    // active context before the lease can detach/pool storage.
                    egcl_rt::rooted!(
                        lease = crate::cli::activation_census::uncached_bridge_lease(|| {
                            Lease::new(Rc::clone(code), env, unsafe { (*parent).frame })
                        })
                    );
                    let bridge = lease.bridge_mut() as *mut Bridge;
                    #[cfg(test)]
                    UNCACHED_CALLS.with(|counts| {
                        let (calls, _) = counts.get();
                        counts.set((calls + 1, unsafe { (&*slot).len() }));
                    });
                    #[cfg(test)]
                    record_cache_shape(unsafe { &*slot });
                    // Never bind a generated pointer into this local lease.
                    return Some(unsafe {
                        run(code, bridge, offset, function, args, env, execution)
                    });
                }
                crate::cli::activation_census::note_refusal(
                    crate::cli::activation_census::Refusal::Capacity,
                    unsafe { (*(*parent).owner).installed_symbol },
                    code.installed_symbol,
                    code.is_t2,
                );
                return None;
            }
            let frame = unsafe { (*parent).frame };
            let bridge = Lease::new(Rc::clone(code), env, frame);
            let index = unsafe { (&*slot).len() };
            unsafe { (&mut *slot).push(bridge) };
            index
        }
    };
    let bridge = unsafe { (&mut *slot)[index].bridge_mut() as *mut Bridge };
    // OSR may replace/grow the parent managed extent between calls.
    unsafe { (*bridge).context.get_mut().frame = (*parent).frame };
    let result = unsafe { run(code, bridge, offset, function, args, env, execution) };
    // Only callers that authorize generated reuse supply a named cell. T1
    // keeps selecting/counting heat in Rust. A function-object call has no
    // named descriptor cell: FUNCALL must not be bound as its target callee.
    if index < mapped_generated::MAX_GENERATED_LANES
        && result.is_ok()
        && let Some(cell) = cell
    {
        unsafe { mapped_generated::bind(parent, bridge, code, cell, offset) };
    }
    #[cfg(test)]
    record_cache_shape(unsafe { &*slot });
    Some(result)
}

unsafe fn run(
    code: &Rc<TransferCode>,
    bridge: *mut Bridge,
    offset: usize,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
    execution: *const egcl_rt::exec_context::LispExecutionContext,
) -> Result<EgclVal, EgclError> {
    // Keep an immutable code lease independently of the GC-scanned bridge.
    let code = Rc::clone(code);
    // Caller publication owns input words; copy while no Lisp can allocate.
    unsafe {
        (*bridge).function = function;
        (*bridge).dispatch_function = function;
        (*bridge).args = EntryArgs::new(args);
        (*bridge).instance = if code.native_callable_slot.is_some()
            && egcl_rt::function::is_interpreted_function(function)
        {
            egcl_rt::function::definition_index(function)
        } else {
            code.installed_symbol.unwrap_or(u32::MAX)
        };
        (*bridge).busy = true;
        (*bridge).clean_return = false;
        (*bridge).context.get_mut().recursive_escape = false;
        (*bridge).state.as_mut().unwrap().env_frame = if code.native_callable_slot.is_some() {
            closure_captured_env(function)
        } else {
            None
        };
    }
    let capture_slot = CAPTURE.with(std::ptr::from_ref);
    let context = unsafe { std::ptr::from_mut(&mut *(*bridge).context.get_mut()) };
    let previous = unsafe { (*capture_slot).replace(context) };
    let depth = NATIVE_DEPTH.with(Cell::as_ptr);
    let old_depth = unsafe { *depth };
    let _restore = Restore {
        capture_slot,
        previous,
        bridge,
        depth,
        old_depth,
    };
    unsafe { *depth += 1 };
    let control = if unsafe { (*context).native_recursive.is_none() } {
        unsafe { native_frames::initialize_for(context, env, execution, depth) }
    } else {
        unsafe { (*context).native_recursive.as_mut().unwrap() as *mut _ }
    };
    unsafe {
        (*control).anchor_depth = *depth;
    }
    // A reused control's signal/carrier snapshots deliberately stay stale after
    // migration or notification: ordinary admission services the normal cold
    // poll before refreshing them. Never acknowledge producer epochs here.
    let sp: usize;
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags))
    };
    let overflow = args
        .len()
        .saturating_sub(3)
        .checked_mul(8)
        .and_then(|n| n.checked_add(15))
        .map(|n| n & !15)
        .ok_or_else(invalid_capture)?;
    let required = code
        .ordinary_stack_required
        .ok_or_else(invalid_capture)?
        .checked_add(4096)
        .and_then(|n| n.checked_add(overflow))
        .ok_or_else(invalid_capture)?;
    if sp
        .checked_sub(required)
        .is_none_or(|bottom| bottom as u64 <= unsafe { (*control).stack_limit })
    {
        egcl_rt::stack::report_stack_overflow(
            "native-admission",
            required,
            Some(NATIVE_DEPTH.with(|depth| depth.get()) as usize),
            None,
        );
        return Err(EgclError::StackOverflow(
            egcl_rt::current_fiber_id()
                .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
        ));
    }
    let _catch_guard = CatchScopeGuard {
        base: env.catch_stack.len(),
        env,
    };
    let _scope = BlockScopeGuard {
        env,
        blocks: std::mem::take(&mut env.block_stack),
        tags: std::mem::take(&mut env.tag_stack),
    };
    env.clear_mv();
    let directory = super::super::call_table::enter_ordinary_segment();
    let _identity = if code.needs_callable_identity() && code.native_callable_slot.is_none() {
        Some(
            directory
                .retain_root(&code, unsafe { (*bridge).function })
                .ok_or_else(invalid_capture)?,
        )
    } else {
        None
    };
    let _recovery = clear_fault_recovery();
    let outcome = unsafe {
        (&*execution).with_active_native_control(
            std::pin::Pin::new_unchecked(&mut *control),
            || {
                #[cfg(test)]
                {
                    ENTRIES.with(|count| count.set(count.get() + 1));
                    if let Some(observe) = ORDINARY_ENTRY_HOOK.with(|hook| hook.take()) {
                        observe();
                    }
                }
                native_transfer::invoke_ordinary_native_segment(
                    code.code.as_ptr().add(offset),
                    &*execution,
                    if code.native_callable_slot.is_some() {
                        (*bridge).function
                    } else {
                        NIL
                    },
                    (*bridge).args.as_slice(),
                    egcl_rt::current_stack(),
                )
            },
        )
    }
    .map_err(|_| EgclError::Internal("ordinary mapped bridge segment is unavailable".into()))?
    .outcome;
    let error = take_native_error();
    if outcome.exit == NativeExit::Returned
        && error.is_none()
        && unsafe { (*bridge).state.as_ref().unwrap().is_quiescent() }
    {
        unsafe { (*bridge).clean_return = true };
        return Ok(outcome.value);
    }
    error.ok_or_else(invalid_capture).and_then(Err)
}

#[cfg(test)]
#[path = "mapped_ordinary_pool_tests.rs"]
mod pool_tests;

#[cfg(test)]
#[path = "mapped_ordinary_overflow_tests.rs"]
mod overflow_tests;

#[cfg(test)]
#[path = "mapped_ordinary_context_tests.rs"]
mod context_tests;
