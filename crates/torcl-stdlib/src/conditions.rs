//! Condition system — condition types, handlers, and restarts.
//!
//! See spec §5.4.
//!
//! Handler functions, restart functions, and body thunks are invoked through
//! a pluggable `funcall` mechanism.  By default the hook is unset and a
//! token-level default is used (returns the function value itself with no args,
//! or the first arg when args are supplied).  The evaluator installs a real
//! hook via `set_funcall_hook` so that handlers, restarts, and debugger hooks
//! are actually called at runtime.

use crate::clos::{
    allocate_instance_pinned_gc, class_direct_superclasses, class_name, class_of, define_class,
    ensure_clos_bootstrapped, find_class, initialize_instance, make_instance,
};
use crate::streams::make_lisp_string_fresh;
use torcl_rt::error::TorclError;
use torcl_rt::stack::{Frame, FrameType, TorclStack};
use torcl_rt::thread::current_stack;
use torcl_rt::thread::{ConditionHandlerCluster, ConditionRestartCluster};
use torcl_rt::value::{NIL, TAG_SYMBOL, TorclVal};

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Once};

// ── Well-known condition-system symbols ──────────────────────────
//
// The standard CONDITION-hierarchy names, lazily interned into the one shared
// symbol registry (bliss-jtc.6 Stage E). A condition symbol here is therefore
// the *same* object the reader and interpreter produce for that name — no more
// hardcoded symbol-table indices that could collide with the reader's
// sequential interning (the root cause behind bliss-5mf). Each `SYMBOL_X` is a
// `LazyLock<u32>`; dereference (`*SYMBOL_X`) to get the interned index.

/// The CONTINUE restart name.
pub static SYMBOL_CONTINUE: LazyLock<u32> = LazyLock::new(|| intern_name("CONTINUE"));
/// The MUFFLE-WARNING restart name.
pub static SYMBOL_MUFFLE_WARNING: LazyLock<u32> = LazyLock::new(|| intern_name("MUFFLE-WARNING"));
// Internal restart-function sentinels: distinct interned markers under reserved,
// %-prefixed names so they can never alias a user-written or CL symbol.
static INTERNAL_CONTINUE_RESTART_FN: LazyLock<u32> =
    LazyLock::new(|| intern_name("%TORCL-CONTINUE-RESTART-FN"));
static INTERNAL_MUFFLE_WARNING_RESTART_FN: LazyLock<u32> =
    LazyLock::new(|| intern_name("%TORCL-MUFFLE-WARNING-RESTART-FN"));

/// The CONDITION type (root of the condition hierarchy).
pub static SYMBOL_CONDITION: LazyLock<u32> = LazyLock::new(|| intern_name("CONDITION"));
/// WARNING.
pub static SYMBOL_WARNING: LazyLock<u32> = LazyLock::new(|| intern_name("WARNING"));
/// SERIOUS-CONDITION.
pub static SYMBOL_SERIOUS_CONDITION: LazyLock<u32> =
    LazyLock::new(|| intern_name("SERIOUS-CONDITION"));
/// ERROR.
pub static SYMBOL_ERROR: LazyLock<u32> = LazyLock::new(|| intern_name("ERROR"));
/// SIMPLE-ERROR.
pub static SYMBOL_SIMPLE_ERROR: LazyLock<u32> = LazyLock::new(|| intern_name("SIMPLE-ERROR"));
/// TYPE-ERROR.
pub static SYMBOL_TYPE_ERROR: LazyLock<u32> = LazyLock::new(|| intern_name("TYPE-ERROR"));
/// SIMPLE-WARNING.
pub static SYMBOL_SIMPLE_WARNING: LazyLock<u32> = LazyLock::new(|| intern_name("SIMPLE-WARNING"));
/// CONTROL-ERROR.
pub static SYMBOL_CONTROL_ERROR: LazyLock<u32> = LazyLock::new(|| intern_name("CONTROL-ERROR"));
/// SIMPLE-CONDITION.
pub static SYMBOL_SIMPLE_CONDITION: LazyLock<u32> =
    LazyLock::new(|| intern_name("SIMPLE-CONDITION"));
/// STORAGE-CONDITION.
pub static SYMBOL_STORAGE_CONDITION: LazyLock<u32> =
    LazyLock::new(|| intern_name("STORAGE-CONDITION"));
static SYMBOL_FORMAT_CONTROL: LazyLock<u32> = LazyLock::new(|| intern_name("FORMAT-CONTROL"));
static SYMBOL_FORMAT_ARGUMENTS: LazyLock<u32> = LazyLock::new(|| intern_name("FORMAT-ARGUMENTS"));
static SYMBOL_DATUM: LazyLock<u32> = LazyLock::new(|| intern_name("DATUM"));
static SYMBOL_EXPECTED_TYPE: LazyLock<u32> = LazyLock::new(|| intern_name("EXPECTED-TYPE"));

/// Intern a condition-system symbol name into the shared registry.
fn intern_name(name: &str) -> u32 {
    torcl_rt::symbols::intern(name)
}
const INTERNAL_HANDLER_CASE_FN_BASE: i64 = -9_000_000;
const STORAGE_CONDITION_POOL_SIZE: usize = 4;

/// Names of all builtin condition types (used for hierarchy discrimination).
const KNOWN_CONDITION_TYPE_NAMES: &[&str] = &[
    "CONDITION",
    "WARNING",
    "INTERRUPT-CONDITION",
    "TIMEOUT-CONDITION",
    "SERIOUS-CONDITION",
    "ERROR",
    "SIMPLE-ERROR",
    "TYPE-ERROR",
    "SIMPLE-WARNING",
    "CONTROL-ERROR",
    "SIMPLE-CONDITION",
    "STORAGE-CONDITION",
];

/// Check whether a TorclVal represents a known condition type symbol.
fn is_known_condition_type(val: TorclVal) -> bool {
    if (val.0 & 0b111) == TAG_SYMBOL {
        if let Some(name) = torcl_rt::symbols::symbol_name(val.as_symbol_index()) {
            return KNOWN_CONDITION_TYPE_NAMES.contains(&name.as_str());
        }
    }
    false
}

// ── Funcall hook ─────────────────────────────────────────────────
//
// A pluggable function-invocation mechanism.  The evaluator sets this
// so that handler fns, restart fns, debugger hooks, and body thunks
// are actually called.  When unset, the default token-level behaviour
// is used (backward-compatible with unit tests).

type FuncallFn = Box<dyn Fn(TorclVal, &[TorclVal]) -> Result<TorclVal, TorclError>>;

thread_local! {
    static FUNCALL_HOOK: RefCell<Option<FuncallFn>> = RefCell::new(None);
}

/// Install a funcall hook for the condition system.
///
/// When set, every handler invocation, restart invocation, and debugger-hook
/// call will go through this hook, enabling real function calls.
pub fn set_funcall_hook(
    hook: impl Fn(TorclVal, &[TorclVal]) -> Result<TorclVal, TorclError> + 'static,
) {
    FUNCALL_HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(hook));
    });
}

/// Clear the funcall hook.
pub fn clear_funcall_hook() {
    FUNCALL_HOOK.with(|h| {
        *h.borrow_mut() = None;
    });
}

/// Call a function value with args, going through the hook if set.
fn funcall(function: TorclVal, args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    if function == TorclVal::from_symbol_index(*INTERNAL_CONTINUE_RESTART_FN)
        || function == TorclVal::from_symbol_index(*INTERNAL_MUFFLE_WARNING_RESTART_FN)
    {
        return Ok(args.first().copied().unwrap_or(NIL));
    }
    if function.is_fixnum() && function.as_fixnum() <= INTERNAL_HANDLER_CASE_FN_BASE {
        let matched = with_state(|state| {
            let handler_val = state.handler_case_clauses.get(&function.to_raw()).copied();
            if let Some(handler_val) = handler_val {
                state.pending_handler_case =
                    Some((args.first().copied().unwrap_or(NIL), handler_val));
            }
            handler_val
        });
        if matched.is_some() {
            return Err(TorclError::Internal("__HANDLER_CASE__".into()));
        }
    }
    FUNCALL_HOOK.with(|h| {
        let borrow = h.borrow();
        if let Some(hook) = borrow.as_ref() {
            hook(function, args)
        } else {
            // Default token-level behaviour:
            //  - (funcall fn) → fn
            //  - (funcall fn a b ...) → first arg
            if args.is_empty() {
                Ok(function)
            } else {
                Ok(args[0])
            }
        }
    })
}

// ── Runtime-owned condition system state ─────────────────────────

fn with_state<T>(f: impl FnOnce(&mut torcl_rt::thread::ThreadConditionState) -> T) -> T {
    torcl_rt::with_current_condition_state_mut(f)
}

struct ConditionClusterFrameGuard {
    stack: &'static TorclStack,
}

impl Drop for ConditionClusterFrameGuard {
    fn drop(&mut self) {
        self.stack.pop_frame();
    }
}

struct HandlerClusterGuard {
    _frame: ConditionClusterFrameGuard,
}

impl Drop for HandlerClusterGuard {
    fn drop(&mut self) {
        with_state(|state| {
            state.handler_stack.pop();
        });
    }
}

struct RestartClusterGuard {
    _frame: ConditionClusterFrameGuard,
}

impl Drop for RestartClusterGuard {
    fn drop(&mut self) {
        with_state(|state| {
            state.restart_stack.pop();
        });
    }
}

fn push_condition_cluster_frame(
    num_slots: usize,
) -> Result<(*mut Frame, ConditionClusterFrameGuard), TorclError> {
    let num_slots = u16::try_from(num_slots).map_err(|_| {
        TorclError::ProgramError("condition cluster has too many entries for one frame".into())
    })?;
    let stack = current_stack();
    let frame = stack
        .push_frame(NIL, std::ptr::null(), num_slots, FrameType::Special as u32)
        .ok_or_else(|| TorclError::Internal("TorclStack exhausted for condition cluster".into()))?;
    Ok((frame, ConditionClusterFrameGuard { stack }))
}

fn push_handler_cluster_frame(
    bindings: &[(TorclVal, TorclVal)],
) -> Result<(ConditionClusterFrameGuard, ConditionHandlerCluster), TorclError> {
    let slots_len = bindings.len().saturating_mul(2);
    let (frame, guard) = push_condition_cluster_frame(slots_len)?;
    unsafe {
        let slots = TorclStack::frame_slots_mut(frame);
        for (i, (condition_type, handler_fn)) in bindings.iter().enumerate() {
            slots[i * 2] = *condition_type;
            slots[i * 2 + 1] = *handler_fn;
        }
    }
    Ok((
        guard,
        ConditionHandlerCluster {
            frame: frame as usize,
            count: bindings.len(),
        },
    ))
}

fn establish_handler_cluster(
    bindings: &[(TorclVal, TorclVal)],
) -> Result<HandlerClusterGuard, TorclError> {
    let (frame, cluster) = push_handler_cluster_frame(bindings)?;
    with_state(|state| state.handler_stack.push(cluster));
    Ok(HandlerClusterGuard { _frame: frame })
}

fn push_restart_cluster_frame(
    restarts: &[RestartSpec],
) -> Result<(ConditionClusterFrameGuard, ConditionRestartCluster), TorclError> {
    let slots_len = restarts.len().saturating_mul(5);
    let (frame, guard) = push_condition_cluster_frame(slots_len)?;
    unsafe {
        let slots = TorclStack::frame_slots_mut(frame);
        for (i, spec) in restarts.iter().enumerate() {
            let base = i * 5;
            slots[base] = spec.name;
            slots[base + 1] = spec.function;
            slots[base + 2] = spec.interactive_function.unwrap_or(NIL);
            slots[base + 3] = spec.report_function.unwrap_or(NIL);
            slots[base + 4] = spec.test_function.unwrap_or(NIL);
        }
    }
    Ok((
        guard,
        ConditionRestartCluster {
            frame: frame as usize,
            count: restarts.len(),
        },
    ))
}

fn establish_restart_cluster(restarts: &[RestartSpec]) -> Result<RestartClusterGuard, TorclError> {
    let (frame, cluster) = push_restart_cluster_frame(restarts)?;
    with_state(|state| state.restart_stack.push(cluster));
    Ok(RestartClusterGuard { _frame: frame })
}

fn handler_cluster_entry(cluster: ConditionHandlerCluster, index: usize) -> (TorclVal, TorclVal) {
    debug_assert!(index < cluster.count);
    unsafe {
        let slots = TorclStack::frame_slots_mut(cluster.frame as *mut Frame);
        (slots[index * 2], slots[index * 2 + 1])
    }
}

fn restart_cluster_entry(
    cluster: ConditionRestartCluster,
    index: usize,
) -> (
    TorclVal,
    TorclVal,
    Option<TorclVal>,
    Option<TorclVal>,
    Option<TorclVal>,
) {
    debug_assert!(index < cluster.count);
    unsafe {
        let slots = TorclStack::frame_slots_mut(cluster.frame as *mut Frame);
        let base = index * 5;
        let interactive = (!slots[base + 2].is_nil()).then_some(slots[base + 2]);
        let report = (!slots[base + 3].is_nil()).then_some(slots[base + 3]);
        let test = (!slots[base + 4].is_nil()).then_some(slots[base + 4]);
        (slots[base], slots[base + 1], report, interactive, test)
    }
}

type RestartEntry = (
    TorclVal,
    TorclVal,
    Option<TorclVal>,
    Option<TorclVal>,
    Option<TorclVal>,
);

fn active_restart_entries() -> Vec<RestartEntry> {
    let clusters = with_state(|state| state.restart_stack.clone());
    let mut entries = Vec::new();
    for cluster in clusters.iter().rev() {
        for i in (0..cluster.count).rev() {
            entries.push(restart_cluster_entry(*cluster, i));
        }
    }
    entries
}

// ── Pre-allocated STORAGE-CONDITION pool (D5.13, R5.110, bliss-wzw) ──────────
//
// Signalling on the storage-exhaustion / stack-overflow path MUST NOT allocate,
// intern, define classes, or take a lock that could block or allocate behind
// the already-failing allocator. So the pool lives outside the `RefCell`
// `ConditionState` in its own lock-free structure: fixed atomic slots plus a
// single atomic claim bitmask. The acquire/release path only does atomic loads
// and a CAS — it never borrows a `RefCell` (whose reentrant borrow would panic
// if the storage path were ever re-entered).
//
// The pool is **thread-local**, not a process-wide `static` as sketched in the
// spec's D5.13. That is deliberate: CLOS instance identity here is thread-local
// (`clos::live_instances`), so an instance created on one thread cannot be
// safely inspected — `class_of` / condition-type matching — from another. Each
// thread therefore owns a private pool of instances built against its own
// condition classes. Cross-thread "concurrent claim" (spec 5.4.10) is satisfied
// with zero contention: threads never share a slot. The atomic claim bitmask
// still earns its keep *within* a thread, handing out distinct instances under
// nested / reentrant storage signalling and recycling them on release, with a
// deterministic fallback to slot 0 when all slots are in flight so the signal
// path can never itself fail to produce a condition.
struct StoragePool {
    slots: [AtomicU64; STORAGE_CONDITION_POOL_SIZE],
    /// Bit `i` set ⇒ slot `i` is currently claimed (in flight).
    claimed: AtomicU32,
    initialized: AtomicBool,
}

impl StoragePool {
    const fn new() -> Self {
        // `AtomicU64` is not `Copy`, so the array cannot be built with `[expr; N]`;
        // spell out the slots. A compile-time check keeps this in sync with SIZE.
        const _: () = assert!(
            STORAGE_CONDITION_POOL_SIZE == 4,
            "StoragePool slot literal must match STORAGE_CONDITION_POOL_SIZE"
        );
        StoragePool {
            slots: [
                AtomicU64::new(NIL.0),
                AtomicU64::new(NIL.0),
                AtomicU64::new(NIL.0),
                AtomicU64::new(NIL.0),
            ],
            claimed: AtomicU32::new(0),
            initialized: AtomicBool::new(false),
        }
    }

    /// Bitmask of all valid slots, e.g. `0b1111` for a 4-slot pool.
    const MASK: u32 = (1u32 << STORAGE_CONDITION_POOL_SIZE) - 1;
}

thread_local! {
    static STORAGE_POOL: StoragePool = const { StoragePool::new() };
}

thread_local! {
    /// Flag set by the MUFFLE-WARNING restart to suppress warning output.
    static WARNING_MUFFLED: RefCell<bool> = const { RefCell::new(false) };
}

// ── Condition construction ────────────────────────────────────────

/// `(superclass names, slot names)` for each builtin condition type, keyed by
/// name. Interned lazily by `ensure_condition_class`; expressing the hierarchy in
/// names keeps it independent of interning order.
fn condition_class_spec(name: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match name {
        "CONDITION" => (&[], &[]),
        "SERIOUS-CONDITION" => (&["CONDITION"], &[]),
        "ERROR" => (&["SERIOUS-CONDITION"], &[]),
        "WARNING" => (&["CONDITION"], &[]),
        "INTERRUPT-CONDITION" => (&["CONDITION"], &[]),
        "TIMEOUT-CONDITION" => (&["ERROR"], &[]),
        "SIMPLE-CONDITION" => (&["CONDITION"], &["FORMAT-CONTROL", "FORMAT-ARGUMENTS"]),
        "SIMPLE-ERROR" => (&["ERROR", "SIMPLE-CONDITION"], &[]),
        "TYPE-ERROR" => (&["ERROR"], &["DATUM", "EXPECTED-TYPE"]),
        "SIMPLE-WARNING" => (&["WARNING", "SIMPLE-CONDITION"], &[]),
        "CONTROL-ERROR" => (&["ERROR"], &[]),
        "STORAGE-CONDITION" => (&["SERIOUS-CONDITION"], &[]),
        _ => (&["CONDITION"], &[]),
    }
}

fn ensure_condition_class(name: &str) -> Result<TorclVal, TorclError> {
    let sym = TorclVal::from_symbol_index(intern_name(name));
    if let Some(class) = find_class(sym) {
        return Ok(class);
    }
    if find_class(torcl_rt::value::T).is_none() {
        let _ = ensure_clos_bootstrapped();
        if let Some(class) = find_class(sym) {
            return Ok(class);
        }
    }
    let (supers, slots) = condition_class_spec(name);
    let super_vals: Vec<TorclVal> = supers
        .iter()
        .map(|n| ensure_condition_class(n))
        .collect::<Result<_, _>>()?;
    let slot_vals: Vec<TorclVal> = slots
        .iter()
        .map(|n| TorclVal::from_symbol_index(intern_name(n)))
        .collect();
    define_class(sym, sym, &super_vals, &slot_vals)?;
    Ok(sym)
}

fn ensure_builtin_condition_classes() -> Result<(), TorclError> {
    for name in [
        "CONDITION",
        "SERIOUS-CONDITION",
        "ERROR",
        "WARNING",
        "SIMPLE-CONDITION",
        "SIMPLE-ERROR",
        "TIMEOUT-CONDITION",
        "TYPE-ERROR",
        "SIMPLE-WARNING",
        "CONTROL-ERROR",
        "STORAGE-CONDITION",
    ] {
        ensure_condition_class(name)?;
    }
    Ok(())
}

/// Number of preallocated STORAGE-CONDITION pool instances.
pub fn storage_condition_pool_size() -> usize {
    STORAGE_CONDITION_POOL_SIZE
}

/// Install a caller-built STORAGE-CONDITION pool, replacing any instances the
/// stdlib preallocated (bliss-5mf). The interpreter uses this to seed the pool
/// with CLI-native condition instances — whose class is the one the CLI's
/// condition matcher / TYPE-OF recognize — after its condition classes are live,
/// while still keeping the acquire path allocation-free. Instances should be
/// pinned in the GC heap by the caller (D5.13). Requires exactly
/// `storage_condition_pool_size()` instances.
pub fn set_storage_condition_pool(instances: &[TorclVal]) -> Result<(), TorclError> {
    if instances.len() != STORAGE_CONDITION_POOL_SIZE {
        return Err(TorclError::Internal(format!(
            "STORAGE-CONDITION pool needs {STORAGE_CONDITION_POOL_SIZE} instances, got {}",
            instances.len()
        )));
    }
    STORAGE_POOL.with(|p| {
        for (slot, inst) in p.slots.iter().zip(instances) {
            slot.store(inst.0, Ordering::Release);
        }
        // Fresh instances are all free; publish `initialized` last so the acquire
        // path never observes populated slots before the claim mask is cleared.
        p.claimed.store(0, Ordering::Release);
        p.initialized.store(true, Ordering::Release);
    });
    Ok(())
}

fn initialize_storage_condition_pool() -> Result<(), TorclError> {
    ensure_builtin_condition_classes()?;
    let class = ensure_condition_class("STORAGE-CONDITION")?;
    let mut pool = [NIL; STORAGE_CONDITION_POOL_SIZE];
    for entry in &mut pool {
        // D5.13 (bliss-4v8): the pool lives in the GC heap, pinned, so a moving
        // collection never relocates or frees these preallocated instances — the
        // acquire path hands out raw addresses on the storage-exhaustion path.
        let inst = allocate_instance_pinned_gc(class)?;
        initialize_instance(inst, &[])?;
        *entry = inst;
    }
    set_storage_condition_pool(&pool)
}

fn storage_condition_pool_is_live() -> bool {
    STORAGE_POOL.with(|p| {
        if !p.initialized.load(Ordering::Acquire) {
            return false;
        }
        let first = TorclVal(p.slots[0].load(Ordering::Acquire));
        first != NIL
            && class_inherits_from(
                class_of(first),
                TorclVal::from_symbol_index(*SYMBOL_STORAGE_CONDITION),
            )
    })
}

/// Acquire a preallocated `STORAGE-CONDITION` instance for the heap-exhaustion /
/// stack-overflow signalling path (R5.110). This runs when allocation is already
/// failing, so it MUST NOT allocate, intern, define classes, resolve symbols, or
/// take a lock that could block or allocate: it only does atomic loads and a
/// compare-and-swap on the thread-local pool (bliss-wzw). The pool is filled once
/// at startup by `initialize_condition_runtime_support` (called from the
/// interpreter's `Env::new` after CLOS/condition-class bootstrap and before any
/// user code). If it is somehow not initialized, we fail hard with a fixed
/// `Internal` error rather than lazily allocating on the low-memory path — that
/// lazy fallback was the bug this replaces (bliss-uh4.2).
///
/// The returned instance is *claimed*: pair each success with
/// [`release_preallocated_storage_condition`] once the condition is no longer in
/// flight so the slot can be reused. When every slot is already claimed
/// (deeper nesting than the pool size), acquisition does not fail — it
/// deterministically falls back to slot 0 (unclaimed, shared) so the storage
/// path can always produce a condition to signal.
///
/// Deliberately does NOT call `storage_condition_pool_is_live`, whose
/// `class_of` / class-graph walk could allocate or lock.
pub fn acquire_preallocated_storage_condition() -> Result<TorclVal, TorclError> {
    STORAGE_POOL.with(|p| {
        if !p.initialized.load(Ordering::Acquire) {
            return Err(TorclError::Internal(
                "STORAGE-CONDITION pool not initialized before the storage-failure path".into(),
            ));
        }
        loop {
            let cur = p.claimed.load(Ordering::Acquire);
            let free = !cur & StoragePool::MASK;
            if free == 0 {
                // Exhaustion fallback: hand out slot 0 without claiming. Signalling
                // with a shared instance under total exhaustion is acceptable — the
                // path must never itself fail to produce a condition.
                let bits = p.slots[0].load(Ordering::Acquire);
                if bits == NIL.0 {
                    return Err(TorclError::Internal(
                        "STORAGE-CONDITION pool slot empty".into(),
                    ));
                }
                return Ok(TorclVal(bits));
            }
            let idx = free.trailing_zeros() as usize;
            let next = cur | (1u32 << idx);
            if p.claimed
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let bits = p.slots[idx].load(Ordering::Acquire);
                if bits == NIL.0 {
                    // Slot was never populated; unclaim and report rather than
                    // handing back NIL.
                    p.claimed.fetch_and(!(1u32 << idx), Ordering::AcqRel);
                    return Err(TorclError::Internal(
                        "STORAGE-CONDITION pool slot empty".into(),
                    ));
                }
                return Ok(TorclVal(bits));
            }
            // CAS lost the race; retry with the fresh mask.
        }
    })
}

/// Release a previously [`acquire_preallocated_storage_condition`]-claimed
/// instance, clearing its claim bit so the slot can be reused (bliss-wzw). Safe
/// and allocation-free: only atomic loads and a fetch-and. Releasing an instance
/// that was the exhaustion fallback (slot 0, never claimed) is a harmless no-op
/// on the bit level. Releasing an unknown value is ignored.
pub fn release_preallocated_storage_condition(condition: TorclVal) {
    STORAGE_POOL.with(|p| {
        for (i, slot) in p.slots.iter().enumerate() {
            if slot.load(Ordering::Acquire) == condition.0 {
                p.claimed.fetch_and(!(1u32 << i), Ordering::AcqRel);
                return;
            }
        }
    });
}

/// Forget the preallocated STORAGE-CONDITION pool. Called after a core-image
/// load (torcl-x0f2.7a): the pool instances were allocated in the pre-load heap
/// that `restore_heap` discarded, so `storage_condition_pool_is_live`'s
/// `class_of(first)` would deref a dead instance's wrapper word and SIGSEGV.
/// Clearing lets the next `initialize_condition_runtime_support` re-preallocate
/// from the restored world's classes.
pub fn reset_storage_condition_pool() {
    STORAGE_POOL.with(|p| {
        p.initialized.store(false, Ordering::Release);
        for slot in &p.slots {
            slot.store(NIL.0, Ordering::Release);
        }
        p.claimed.store(0, Ordering::Release);
    });
}

fn runtime_init_storage_condition_support() -> Result<(), TorclError> {
    initialize_condition_runtime_support()
}

pub fn install_runtime_init_hook() {
    static INSTALL_HOOK: Once = Once::new();
    INSTALL_HOOK.call_once(|| {
        torcl_rt::set_runtime_init_hook(runtime_init_storage_condition_support);
    });
}

pub fn initialize_condition_runtime_support() -> Result<(), TorclError> {
    install_runtime_init_hook();
    if storage_condition_pool_is_live() {
        Ok(())
    } else {
        initialize_storage_condition_pool()
    }
}

fn class_inherits_from(class: TorclVal, target: TorclVal) -> bool {
    if class == target || class_name(class) == target {
        return true;
    }
    class_direct_superclasses(class)
        .into_iter()
        .any(|super_class| class_inherits_from(super_class, target))
}

/// Create a simple-error condition.
///
/// Returns a TorclVal fixnum derived from the hash of the format string.
/// The condition is registered in thread-local state so handler_case can
/// recognize it as a condition value.
pub fn make_simple_error(format_control: &str, _format_args: &[TorclVal]) -> TorclVal {
    initialize_condition_runtime_support().expect("bootstrap condition runtime support");
    let class = ensure_condition_class("SIMPLE-ERROR").expect("resolve SIMPLE-ERROR class");
    make_instance(
        class,
        &[
            TorclVal::from_symbol_index(*SYMBOL_FORMAT_CONTROL),
            make_lisp_string_fresh(format_control),
            TorclVal::from_symbol_index(*SYMBOL_FORMAT_ARGUMENTS),
            NIL,
        ],
    )
    .expect("make SIMPLE-ERROR instance")
}

/// Create a type-error condition.
///
/// Returns a TorclVal fixnum combining datum and expected type information.
/// Registered as a condition in thread-local state.
pub fn make_type_error(datum: TorclVal, expected_type: TorclVal) -> TorclVal {
    initialize_condition_runtime_support().expect("bootstrap condition runtime support");
    let class = ensure_condition_class("TYPE-ERROR").expect("resolve TYPE-ERROR class");
    make_instance(
        class,
        &[
            TorclVal::from_symbol_index(*SYMBOL_DATUM),
            datum,
            TorclVal::from_symbol_index(*SYMBOL_EXPECTED_TYPE),
            expected_type,
        ],
    )
    .expect("make TYPE-ERROR instance")
}

/// Check if a TorclVal is a condition instance rooted at CONDITION.
fn is_condition(val: TorclVal) -> bool {
    class_inherits_from(
        class_of(val),
        TorclVal::from_symbol_index(*SYMBOL_CONDITION),
    )
}

/// Check if a handler's condition-type specification matches a given condition.
///
/// Matching rules (issue 7 fix — proper type discrimination):
///  1. Exact match (condition == clause_type) always succeeds.
///  2. NIL clause_type never matches (no valid type).
///  3. If clause_type is a *known* condition type symbol (e.g. ERROR, WARNING),
///     it matches only when it appears in the condition's stored type hierarchy.
///     This prevents a WARNING handler from catching an ERROR, etc.
///  4. If clause_type is an *unknown* symbol (not in the well-known set), it is
///     treated as a catch-all CONDITION-level specifier for backward compat.
fn condition_type_matches(condition: TorclVal, clause_type: TorclVal) -> bool {
    // Rule 1: exact match.
    if condition == clause_type {
        return true;
    }
    // Rule 2: NIL never matches.
    if clause_type.is_nil() {
        return false;
    }
    if is_known_condition_type(clause_type) {
        class_inherits_from(class_of(condition), clause_type)
    } else {
        is_condition(condition)
    }
}

// ── Signalling ────────────────────────────────────────────────────

/// Signal a condition (CL `SIGNAL`). Does not unwind.
///
/// Searches the handler stack from most-recent to oldest for a matching
/// handler.  Each matching handler is *invoked* via `funcall(handler_fn,
/// condition)`.  Per CL semantics, if a handler returns normally (does not
/// perform a non-local transfer), SIGNAL continues searching.  If no handler
/// handles the condition, returns `Ok(())`.
///
/// Per A5.04, the handler stack is temporarily rebound to exclude the
/// current cluster (and everything established after it) before invoking
/// the handler, preventing infinite recursion when a handler re-signals.
pub fn signal_condition(condition: TorclVal) -> Result<(), TorclError> {
    initialize_condition_runtime_support()?;
    break_on_signals_gate(condition)?;

    // Each frame of `handler_stack` is one cluster (the handlers established by a
    // single HANDLER-BIND). Walk clusters most-recent-first, trying a cluster's
    // handlers in source order. Per A5.04 (R5.94/R5.102), while a handler runs,
    // its whole cluster and every newer cluster are disestablished so a re-signal
    // is seen only by strictly-older clusters. Disestablishment moves the tail
    // `handler_stack[ci..]` out with `split_off` and appends it back afterwards —
    // no clone of the whole stack, only the current cluster is copied so it can
    // be iterated while the TLS stack is mutated.
    let cluster_count = with_state(|state| state.handler_stack.len());
    for ci in (0..cluster_count).rev() {
        let Some(cluster) = with_state(|state| state.handler_stack.get(ci).copied()) else {
            continue;
        };
        for i in 0..cluster.count {
            let (condition_type, handler_fn) = handler_cluster_entry(cluster, i);
            if condition_type_matches(condition, condition_type) {
                let tail = with_state(|state| state.handler_stack.split_off(ci));
                let handler_result = funcall(handler_fn, &[condition]);
                with_state(|state| state.handler_stack.extend(tail));
                // A handler that returns normally *declines* — keep searching;
                // one that transferred control surfaces here as Err and propagates.
                handler_result?;
            }
        }
    }

    // All handlers declined (or none matched) — SIGNAL returns NIL / Ok.
    Ok(())
}

fn break_on_signals_gate(condition: TorclVal) -> Result<(), TorclError> {
    let break_spec = with_state(|state| state.break_on_signals);
    let Some(break_spec) = break_spec else {
        return Ok(());
    };
    if !condition_type_matches(condition, break_spec) {
        return Ok(());
    }

    with_state(|state| state.break_on_signals = None);
    let _ = invoke_debugger(condition);
    with_state(|state| state.break_on_signals = Some(break_spec));
    Ok(())
}

/// Signal an error (CL `ERROR`). Enters debugger if unhandled.
///
/// Signals the condition through the handler stack via `signal_condition`.
/// If no handler handles the condition (performs a non-local transfer),
/// the debugger is entered via `invoke_debugger`.  Per ANSI CL, ERROR
/// never returns normally — it either transfers control via a handler or
/// enters the debugger.
pub fn error_condition(condition: TorclVal) -> Result<(), TorclError> {
    // Signal the condition through handlers (per SIGNAL protocol).
    // If a handler performs a non-local transfer, control won't return here.
    signal_condition(condition)?;

    // All handlers declined or none matched — invoke the debugger.
    invoke_debugger(condition)?;

    // If invoke_debugger returned Ok (hook handled it), still report as
    // an unhandled error per CL semantics — ERROR never returns normally.
    Err(TorclError::Internal(
        "unhandled error condition".to_string(),
    ))
}

/// Signal a continuable error (CL `CERROR`).
///
/// Establishes a CONTINUE restart that allows the caller to continue from
/// the error, then signals the condition through the handler stack.  If no
/// handler handles the condition, the debugger is entered via `invoke_debugger`;
/// the CONTINUE restart allows returning from the debugger.
pub fn cerror(_continue_string: &str, condition: TorclVal) -> Result<(), TorclError> {
    // Establish a CONTINUE restart using the named constant (issue 8 fix).
    let continue_restart = RestartSpec {
        name: TorclVal::from_symbol_index(*SYMBOL_CONTINUE),
        function: TorclVal::from_symbol_index(*INTERNAL_CONTINUE_RESTART_FN),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    let _cluster = establish_restart_cluster(&[continue_restart])?;

    // Signal the condition through handlers via signal_condition.
    signal_condition(condition)?;

    // No handler handled the condition — invoke the debugger.
    // Per A5.10 / R5.105, CERROR calls invoke_debugger when unhandled.
    // The CONTINUE restart allows the debugger (or hook) to return.
    let _debugger_result = invoke_debugger(condition);

    // Per CERROR semantics, the CONTINUE restart was implicitly invoked
    // (either by the debugger hook or by default), allowing execution to
    // continue from the error.
    Ok(())
}

/// Signal a warning (CL `WARN`). Establishes MUFFLE-WARNING restart.
///
/// Signals the condition through the handler stack.  If a handler invokes
/// the MUFFLE-WARNING restart, the warning is silenced.  If no handler
/// handles the warning, per R5.104 a message is printed to *error-output*.
/// Always returns `Ok(())`.
pub fn warn_condition(condition: TorclVal) -> Result<(), TorclError> {
    initialize_condition_runtime_support()?;
    // Establish a MUFFLE-WARNING restart using the named constant (issue 8 fix).
    let muffle_restart = RestartSpec {
        name: TorclVal::from_symbol_index(*SYMBOL_MUFFLE_WARNING),
        function: TorclVal::from_symbol_index(*INTERNAL_MUFFLE_WARNING_RESTART_FN),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    // Reset the muffled flag before signalling.
    WARNING_MUFFLED.with(|m| *m.borrow_mut() = false);

    let _cluster = establish_restart_cluster(&[muffle_restart])?;

    // Signal the warning through handlers via signal_condition.
    // Per CL semantics, warnings do not enter the debugger.
    signal_condition(condition)?;

    // Per R5.104: only print the warning if MUFFLE-WARNING was NOT invoked.
    let muffled = WARNING_MUFFLED.with(|m| *m.borrow());
    if !muffled {
        eprintln!("WARNING: condition {:?}", condition);
    }

    Ok(())
}

// ── Handler binding ───────────────────────────────────────────────

/// A condition handler binding.
///
/// Represents a binding between a condition type and a handler function,
/// used to construct bindings for `handler_bind` / `handler_bind_fn`.
pub struct HandlerBinding {
    /// The condition type this handler matches against.
    pub condition_type: TorclVal,
    /// The handler function to invoke when the condition type matches.
    pub handler_fn: TorclVal,
}

impl HandlerBinding {
    /// Create a new handler binding.
    pub fn new(condition_type: TorclVal, handler_fn: TorclVal) -> Self {
        HandlerBinding {
            condition_type,
            handler_fn,
        }
    }

    /// Convert to the tuple representation used by handler_bind.
    pub fn as_tuple(&self) -> (TorclVal, TorclVal) {
        (self.condition_type, self.handler_fn)
    }
}

/// Establish handler bindings (without unwinding — HANDLER-BIND). R5.19.
///
/// Pushes handler bindings onto the handler stack, evaluates the body,
/// then pops the bindings.  When `body` is a pre-evaluated TorclVal,
/// it is returned directly (no conditions can be signalled during a
/// pre-evaluated body).  For real body evaluation with conditions
/// active, use `handler_bind_fn`.
pub fn handler_bind(
    bindings: &[(TorclVal, TorclVal)],
    body: TorclVal,
) -> Result<TorclVal, TorclError> {
    let _cluster = establish_handler_cluster(bindings)?;

    // Evaluate the body via funcall if the value looks like a callable
    // (function tag), otherwise return it directly.  This provides backward
    // compat for tests that pass a pre-evaluated TorclVal while supporting
    // real thunks when the evaluator wraps the body in a closure.

    if body.is_function() {
        funcall(body, &[])
    } else {
        Ok(body)
    }
}

/// Establish handler bindings with a Rust closure body (HANDLER-BIND). R5.19.
///
/// This is the closure-based variant that allows conditions to be signalled
/// during body evaluation.  The handlers are active during the closure call.
pub fn handler_bind_fn(
    bindings: &[(TorclVal, TorclVal)],
    body: impl FnOnce() -> Result<TorclVal, TorclError>,
) -> Result<TorclVal, TorclError> {
    let _cluster = establish_handler_cluster(bindings)?;

    body()
}

/// Establish handler case (unwind before handler — HANDLER-CASE). R5.19.
///
/// If the form is a registered condition and there are clauses, the first
/// clause's handler value is returned (simulating the handler catching the
/// condition). Otherwise, the form value is returned directly.
pub fn handler_case(
    form: TorclVal,
    clauses: &[(TorclVal, TorclVal)],
) -> Result<TorclVal, TorclError> {
    if is_condition(form) {
        for (clause_type, handler_val) in clauses {
            if condition_type_matches(form, *clause_type) {
                // Clause matches — unwind and return the clause handler value.
                // Per CL HANDLER-CASE, the stack is unwound before the
                // handler runs.  If handler_val is a function, funcall it
                // with the condition; otherwise return it directly as the
                // pre-evaluated clause result.
                if handler_val.is_function() {
                    return funcall(*handler_val, &[form]);
                }
                return Ok(*handler_val);
            }
        }
        return Ok(form);
    }
    if !form.is_function() {
        return Ok(form);
    }

    handler_case_fn(clauses, || funcall(form, &[]))
}

/// Establish handler case around a protected computation (HANDLER-CASE). R5.19.
///
/// This is the closure-based entrypoint used when the protected form must be
/// evaluated with handler clauses dynamically installed.
pub fn handler_case_fn(
    clauses: &[(TorclVal, TorclVal)],
    body: impl FnOnce() -> Result<TorclVal, TorclError>,
) -> Result<TorclVal, TorclError> {
    if clauses.is_empty() {
        return body();
    }

    let mut bindings = Vec::with_capacity(clauses.len());
    let mut installed = Vec::with_capacity(clauses.len());
    with_state(|state| {
        state.pending_handler_case = None;
        for (clause_type, handler_val) in clauses {
            let id = INTERNAL_HANDLER_CASE_FN_BASE - state.next_handler_case_id;
            state.next_handler_case_id += 1;
            let token = TorclVal::from_fixnum(id);
            state
                .handler_case_clauses
                .insert(token.to_raw(), *handler_val);
            bindings.push((*clause_type, token));
            installed.push(token.to_raw());
        }
    });

    let result = handler_bind_fn(&bindings, body);

    for raw in installed {
        with_state(|state| {
            state.handler_case_clauses.remove(&raw);
        });
    }

    match result {
        Ok(value) => Ok(value),
        Err(TorclError::Internal(message)) if message == "__HANDLER_CASE__" => {
            let matched = with_state(|state| state.pending_handler_case.take());
            if let Some((condition, handler_val)) = matched {
                if handler_val.is_function() {
                    funcall(handler_val, &[condition])
                } else {
                    Ok(handler_val)
                }
            } else {
                Err(TorclError::Internal(
                    "handler-case lost pending match".into(),
                ))
            }
        }
        Err(err) => Err(err),
    }
}

// ── Restart protocol ──────────────────────────────────────────────

/// Specification for a restart.
pub struct RestartSpec {
    pub name: TorclVal,
    pub function: TorclVal,
    pub report_function: Option<TorclVal>,
    pub interactive_function: Option<TorclVal>,
    pub test_function: Option<TorclVal>,
}

/// Establish restart bindings (RESTART-BIND / RESTART-CASE). R5.20.
///
/// Registers the restart specs in thread-local state, evaluates the body,
/// then removes them.  Restarts have dynamic extent — they are only visible
/// during the body and are removed when restart_bind returns.
///
/// When `body` is a pre-evaluated TorclVal, it is returned directly.
/// For real body evaluation with restarts active, use `restart_bind_fn`.
pub fn restart_bind(restarts: &[RestartSpec], body: TorclVal) -> Result<TorclVal, TorclError> {
    let _cluster = establish_restart_cluster(restarts)?;

    // Evaluate the body.  If it's a function, invoke it via funcall;
    // otherwise return the pre-evaluated value directly.

    if body.is_function() {
        funcall(body, &[])
    } else {
        Ok(body)
    }
}

/// Establish restart bindings with a Rust closure body (RESTART-BIND). R5.20.
///
/// This is the closure-based variant that allows restarts to be exercised
/// during body evaluation.  The restarts are active during the closure call.
pub fn restart_bind_fn(
    restarts: &[RestartSpec],
    body: impl FnOnce() -> Result<TorclVal, TorclError>,
) -> Result<TorclVal, TorclError> {
    let _cluster = establish_restart_cluster(restarts)?;

    body()
}

/// Compute available restarts for a condition.
///
/// Returns all currently established restarts as TorclVal names, ordered
/// newest-first (most recently established first) per R5.97 / A5.08.
/// If a condition is provided, restarts whose test_function rejects the
/// condition are filtered out.
pub fn compute_restarts(condition: Option<TorclVal>) -> Vec<TorclVal> {
    active_restart_entries()
        .into_iter()
        .filter(|(_, _, _, _, test_function)| {
            if let (Some(test_fn), Some(cond)) = (*test_function, condition) {
                match funcall(test_fn, &[cond]) {
                    Ok(result) => !result.is_nil(),
                    Err(_) => false,
                }
            } else {
                true
            }
        })
        .map(|(name, _, _, _, _)| name)
        .collect()
}

/// Find a restart by name.
///
/// Searches the restart registry (most recent first) for the most recently
/// established *applicable* restart with the given name.  Per R5.98, when
/// a condition is provided, restarts whose test_function rejects the
/// condition are skipped.  Returns the restart's function value if found.
pub fn find_restart(name: TorclVal, condition: Option<TorclVal>) -> Option<TorclVal> {
    for (entry_name, function, _, _, test_function) in active_restart_entries() {
        if entry_name == name {
            if let (Some(test_fn), Some(cond)) = (test_function, condition) {
                match funcall(test_fn, &[cond]) {
                    Ok(result) if !result.is_nil() => return Some(function),
                    _ => continue,
                }
            } else {
                return Some(function);
            }
        }
    }
    None
}

/// Invoke a restart by its function value or name.
///
/// Takes the function value (as returned by `find_restart`) or a restart
/// name (symbol) and the arguments to pass.  Per A5.09, funcalls the
/// restart's function with the provided args.
///
/// If the argument is a symbol (restart name) and the restart is not found,
/// a CONTROL-ERROR is signalled per §5.4.9.
pub fn invoke_restart(restart: TorclVal, args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    // Look up the restart entry by function value or name.
    let restart_entry =
        active_restart_entries()
            .into_iter()
            .find_map(|(name, function, _, _, _)| {
                if function == restart || name == restart {
                    Some((function, name))
                } else {
                    None
                }
            });

    // If invoking MUFFLE-WARNING, set the muffled flag so warn_condition
    // knows to suppress the warning message (issue 2 fix).
    if let Some((_, name)) = restart_entry {
        let muffle_name = TorclVal::from_symbol_index(*SYMBOL_MUFFLE_WARNING);
        if name == muffle_name {
            WARNING_MUFFLED.with(|m| *m.borrow_mut() = true);
        }
    }

    let restart_fn = restart_entry.map(|(f, _)| f);

    match restart_fn {
        Some(func) => {
            // Found in registry — funcall the restart function with args.
            funcall(func, args)
        }
        None => {
            // Not found in registry.
            if restart.is_symbol() {
                // Per A5.09 / §5.4.9: if the restart name is not found,
                // signal a CONTROL-ERROR.
                Err(TorclError::Internal(format!(
                    "CONTROL-ERROR: no restart named {:?} is active",
                    restart
                )))
            } else {
                // Treat as a direct restart function value (restart object)
                // and funcall it with the provided args.
                funcall(restart, args)
            }
        }
    }
}

/// Invoke a restart interactively.
///
/// Looks up the restart entry by its function value, funcalls the restart's
/// `interactive_function` (if present) to produce a list of arguments per
/// A5.09, then invokes the restart function with those arguments.
/// If no interactive function is present, invokes the restart with no arguments.
pub fn invoke_restart_interactively(restart: TorclVal) -> Result<TorclVal, TorclError> {
    // Look up the restart entry to find the interactive_function.
    let interactive_fn =
        active_restart_entries()
            .into_iter()
            .find_map(|(name, function, _, interactive, _)| {
                if function == restart || name == restart {
                    interactive
                } else {
                    None
                }
            });

    if let Some(int_fn) = interactive_fn {
        // Per A5.09: funcall the interactive function to produce an arg list.
        let produced_args = funcall(int_fn, &[])?;
        // Now invoke the restart function with the produced arguments.
        invoke_restart(restart, &[produced_args])
    } else {
        // No interactive function — invoke the restart with no arguments.
        invoke_restart(restart, &[])
    }
}

// ── Debugger hook ─────────────────────────────────────────────────

/// Set `*DEBUGGER-HOOK*`. R5.22.
///
/// When set to `Some(hook)`, the hook function will be invoked before
/// entering the debugger for unhandled conditions.  Set to `None` to
/// clear the hook.
pub fn set_debugger_hook(hook: Option<TorclVal>) {
    with_state(|state| {
        state.debugger_hook = hook;
        if hook.is_none() {
            state.debugger_invoked = false;
        }
    });
}

/// Set `*BREAK-ON-SIGNALS*`.
pub fn set_break_on_signals(type_spec: Option<TorclVal>) {
    with_state(|state| state.break_on_signals = type_spec);
}

/// Invoke the debugger for an unhandled condition.
///
/// Per R5.101 / A5.11, if `*DEBUGGER-HOOK*` is set it MUST be funcall'd
/// before entering the debugger.  The hook receives two arguments: the
/// condition and the hook function itself.  `*DEBUGGER-HOOK*` is set to
/// NIL before calling the hook (per ANSI CL) and is NOT restored — the
/// hook itself or subsequent code may rebind it.
///
/// Returns `Err` to indicate the debugger was entered.
pub fn invoke_debugger(condition: TorclVal) -> Result<(), TorclError> {
    initialize_condition_runtime_support()?;
    let hook = with_state(|state| state.debugger_hook);

    if let Some(hook_fn) = hook {
        // Per ANSI CL A5.11: set *DEBUGGER-HOOK* to NIL before calling
        // the hook, to prevent infinite recursion if the hook itself
        // signals an error.
        with_state(|state| {
            state.debugger_hook = None;
            state.debugger_invoked = true;
        });

        // Invoke the hook function: `(funcall hook-fn condition hook-fn)`.
        // Per R5.101 / A5.11 the hook receives (condition, hook-fn).
        let _hook_result = funcall(hook_fn, &[condition, hook_fn]);

        // Per ANSI CL A5.11: *DEBUGGER-HOOK* is NOT restored after calling
        // the hook.  The hook itself may rebind it if needed.

        // Hook returned normally — enter the standard debugger.
        return Err(TorclError::Internal(format!(
            "debugger entered for condition: {:?}",
            condition
        )));
    }

    // No hook — enter debugger directly.
    Err(TorclError::Internal(format!(
        "debugger entered (no hook) for condition: {:?}",
        condition
    )))
}

/// Signal a runtime low-memory/storage failure using a preallocated
/// `STORAGE-CONDITION` instance per R5.110.
pub fn signal_storage_condition_for_runtime_error(
    error: &TorclError,
) -> Result<TorclVal, TorclError> {
    match error {
        TorclError::Oom | TorclError::StackOverflow(_) => {
            let condition = acquire_preallocated_storage_condition()?;
            let result = signal_condition(condition);
            release_preallocated_storage_condition(condition);
            result?;
            Ok(condition)
        }
        _ => Err(TorclError::Internal(
            "runtime error does not map to STORAGE-CONDITION".into(),
        )),
    }
}

#[cfg(test)]
mod storage_pool_cas_tests {
    //! bliss-wzw: the lock-free CAS claim/release protocol for the pre-allocated
    //! STORAGE-CONDITION pool. These exercise the concurrency primitive directly
    //! with sentinel TorclVals (real-instance / class recognition is covered by
    //! the CLI-side and pinned-GC tests); acquire never inspects slot classes.
    use super::*;

    /// A non-NIL sentinel standing in for a pooled instance.
    fn sentinel(n: u64) -> TorclVal {
        TorclVal(n << 3)
    }

    #[test]
    fn claim_release_and_deterministic_exhaustion_fallback() {
        let pool = [sentinel(1), sentinel(2), sentinel(3), sentinel(4)];
        set_storage_condition_pool(&pool).unwrap();

        // The four claims hand out four *distinct* slots.
        let claims: Vec<TorclVal> = (0..STORAGE_CONDITION_POOL_SIZE)
            .map(|_| acquire_preallocated_storage_condition().unwrap())
            .collect();
        let mut got: Vec<u64> = claims.iter().map(|c| c.0).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![sentinel(1).0, sentinel(2).0, sentinel(3).0, sentinel(4).0]
        );

        // Pool exhausted → deterministic fallback to slot 0, no error.
        let fallback = acquire_preallocated_storage_condition().unwrap();
        assert_eq!(fallback.0, sentinel(1).0);

        // Releasing a claimed slot lets the next claim reuse exactly it.
        release_preallocated_storage_condition(claims[1]);
        let reused = acquire_preallocated_storage_condition().unwrap();
        assert_eq!(reused.0, claims[1].0);
    }

    #[test]
    fn uninitialized_pool_errors_rather_than_allocating() {
        // A thread that never installed a pool must fail hard on the storage
        // path rather than lazily allocate (bliss-uh4.2).
        std::thread::spawn(|| {
            assert!(acquire_preallocated_storage_condition().is_err());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn concurrent_threads_claim_from_independent_pools() {
        // Pools are thread-local: many OS threads claim/release concurrently with
        // zero contention and each only ever sees its own instances — the
        // architecture's answer to spec 5.4.10 "concurrent claim".
        let handles: Vec<_> = (0..8u64)
            .map(|t| {
                std::thread::spawn(move || {
                    let base = (t + 1) * 100;
                    let pool = [
                        sentinel(base),
                        sentinel(base + 1),
                        sentinel(base + 2),
                        sentinel(base + 3),
                    ];
                    set_storage_condition_pool(&pool).unwrap();
                    for _ in 0..2000 {
                        let c = acquire_preallocated_storage_condition().unwrap();
                        let v = c.0 >> 3;
                        assert!(
                            (base..base + 4).contains(&v),
                            "thread {t} saw a foreign slot {v}"
                        );
                        release_preallocated_storage_condition(c);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
