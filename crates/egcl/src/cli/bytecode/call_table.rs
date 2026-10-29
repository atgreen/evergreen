// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned targets behind the runtime's stable named-call ordinals.
//! Cold entries resolve names and drive tier transitions. Native entries own
//! their activation setup; interpreter/legacy bridges retain their resolved
//! target before any allocation or Lisp re-entry.

use super::*;
use egcl_rt::call_table::{self as linkage, CallCell};

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[path = "call_table_ordinary.rs"]
mod ordinary;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::OrdinaryCodeDirectory;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::RootIdentity;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::assert_guarded_callable_publication_for_test;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::ordinary_segment_depth_for_test;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::private_probe_target;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::take_root_identities_for_test;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) use ordinary::{active_ordinary_directory, enter_ordinary_segment};

#[derive(Clone)]
enum Target {
    Native {
        function: EgclVal,
        code: Rc<NativeCode>,
        promote: bool,
    },
    Bytecode {
        function: EgclVal,
        body: Arc<BytecodeFunction>,
        promote: bool,
    },
    Builtin {
        slot: u32,
        nargs: usize,
    },
    /// A slot-accessor generic: `accessor_call` with its inline cache, which
    /// falls back to the full call itself (bliss-fskhm). Runs through the
    /// builtin bridges, since it too has no callable object.
    Accessor {
        nargs: usize,
    },
    /// MAKE-INSTANCE with no methods on it: `make_instance_call`, which takes
    /// the class's instance plan and falls back to the full call itself
    /// (bliss-ih7pa). Any argument count; it too has no callable object.
    MakeInstance,
    Function(EgclVal),
}

/// What a target with no callable object runs through the builtin bridges.
#[derive(Clone, Copy)]
enum BridgeCall {
    Builtin(u32),
    Accessor,
    MakeInstance,
}

impl Target {
    fn function(&self) -> EgclVal {
        match self {
            Self::Native { function, .. }
            | Self::Bytecode { function, .. }
            | Self::Function(function) => *function,
            Self::Builtin { .. } | Self::Accessor { .. } | Self::MakeInstance => NIL,
        }
    }

    fn trace(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            Self::Native { function, .. }
            | Self::Bytecode { function, .. }
            | Self::Function(function) => visit(function),
            Self::Builtin { .. } | Self::Accessor { .. } | Self::MakeInstance => {}
        }
    }

    fn is_bridge(&self) -> bool {
        matches!(
            self,
            Self::Builtin { .. } | Self::Accessor { .. } | Self::MakeInstance
        )
    }

    /// The bridge call for `nargs` arguments, or `None` when this is not a
    /// bridge target or the call has the wrong argument count for it.
    fn bridge_call(&self, nargs: usize) -> Option<BridgeCall> {
        match *self {
            Self::Builtin { slot, nargs: n } if n == nargs => Some(BridgeCall::Builtin(slot)),
            Self::Accessor { nargs: n } if n == nargs => Some(BridgeCall::Accessor),
            Self::MakeInstance => Some(BridgeCall::MakeInstance),
            _ => None,
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
type MappedCode = Option<(
    usize,
    super::native_segment_cache::SegmentCacheEntry<super::native_transfer_entry::TransferCode>,
)>;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[path = "call_table_adapter.rs"]
mod adapter;

struct State {
    symbol: u32,
    cell: Arc<CallCell>,
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[allow(clippy::vec_box)] // machine callers borrow immutable descriptor addresses
    ordinary_adapters: RefCell<Vec<Box<adapter::NamedAdapter>>>,
    target: RefCell<Option<Target>>,
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    mapped: RefCell<MappedCode>,
    // Both slots belong to the same execution and keep stable Box addresses
    // across fiber migration. Read the cell at entry, not the Env it held when
    // this target was resolved: nested native activations replace that Env.
    native_env: *const std::cell::Cell<*mut Env>,
    #[cfg(any(
        all(target_arch = "x86_64", unix),
        all(target_arch = "s390x", target_os = "linux")
    ))]
    #[allow(clippy::vec_box)] // generated entries embed stable descriptor addresses
    versions: RefCell<Vec<Box<super::call_table_native::NativeEntry>>>,
    #[cfg(all(target_arch = "x86_64", unix))]
    funcall: RefCell<Option<Box<super::call_table_funcall::Cache>>>,
}

// Box addresses, unlike the vector's storage, stay fixed when ordinals grow.
#[allow(clippy::vec_box)]
type Slots = Vec<Option<Box<State>>>;
static SLOTS: egcl_rt::execution_local::ExecutionLocal<RefCell<Slots>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Vec::new())) };

/// Interpreter/legacy boundary entries. Their native callers do not perform
/// Rust recovery toggles: the generated bridge owns that ABI conversion.
fn bridge_entries() -> Option<[usize; 6]> {
    let callbacks = [
        cold_register as *const () as usize,
        cold_slice as *const () as usize,
        warm_register as *const () as usize,
        warm_slice as *const () as usize,
        builtin_register as *const () as usize,
        builtin_slice as *const () as usize,
    ];
    #[cfg(all(target_arch = "x86_64", unix))]
    {
        static BRIDGES: std::sync::OnceLock<Option<[egcl_rt::jit::JitBuffer; 6]>> =
            std::sync::OnceLock::new();
        BRIDGES
            .get_or_init(|| {
                let toggle = c2i_set_native_sigsegv_recovery as *const () as u64;
                let mut buffers = Vec::with_capacity(callbacks.len());
                for callback in callbacks {
                    let code =
                        egcl_compiler::t2::emit::emit_native_call_bridge(callback as u64, toggle)
                            .ok()?;
                    buffers.push(egcl_rt::jit::JitBuffer::new(&code)?);
                }
                buffers.try_into().ok()
            })
            .as_ref()
            .map(|buffers| std::array::from_fn(|i| buffers[i].as_ptr() as usize))
    }
    #[cfg(not(all(target_arch = "x86_64", unix)))]
    Some(callbacks)
}

pub(super) fn resolve(symbol: u32) -> Option<Arc<CallCell>> {
    #[cfg(test)]
    RESOLVES.with(|count| count.set(count.get() + 1));
    let entries = bridge_entries()?;
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let native = super::native_transfer_entry::published_entries()?;
    let ordinal = linkage::resolve_ordinal(symbol)?;
    install_bytecode_root_scanner();
    Some(SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let count = slots.len().max(ordinal + 1);
        slots.resize_with(count, || None);
        let state = slots[ordinal].get_or_insert_with(|| {
            #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
            let cell = Arc::new(CallCell::with_native_entries(
                ordinal,
                [entries[0], entries[1]],
                native,
            ));
            #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
            let cell = Arc::new(CallCell::new(ordinal, entries[0], entries[1]));
            let state = Box::new(State {
                symbol,
                cell,
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                ordinary_adapters: RefCell::new(Vec::new()),
                target: RefCell::new(None),
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                mapped: RefCell::new(None),
                native_env: NATIVE_ENV.with(std::ptr::from_ref),
                #[cfg(any(
                    all(target_arch = "x86_64", unix),
                    all(target_arch = "s390x", target_os = "linux")
                ))]
                versions: RefCell::new(Vec::new()),
                #[cfg(all(target_arch = "x86_64", unix))]
                funcall: RefCell::new(None),
            });
            state.cell.bind_state(&*state as *const State as usize);
            linkage::register(&state.cell);
            state
        });
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        adapter::ensure(state);
        Arc::clone(&state.cell)
    }))
}

/// The collector stops all executions before visiting this cache. Invalidated
/// targets are released here; active native version descriptors and bridge
/// calls retain their exact code and root their callable independently. Cells themselves never own code, so
/// mutually recursive compiled functions do not form Rc ownership cycles.
pub(super) unsafe fn scan(visit: &mut dyn FnMut(*mut EgclVal)) {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    unsafe {
        ordinary::sweep_all();
    }
    unsafe {
        SLOTS.scan(|slots| {
            for state in slots.borrow().iter().flatten() {
                #[cfg(all(target_arch = "x86_64", unix))]
                if let Some(cache) = state.funcall.borrow_mut().as_mut() {
                    cache.scan();
                }
                #[cfg(any(
                    all(target_arch = "x86_64", unix),
                    all(target_arch = "s390x", target_os = "linux")
                ))]
                {
                    let mut versions = state.versions.borrow_mut();
                    versions.retain(|version| version.keep(&state.cell));
                    for version in versions.iter_mut() {
                        version.trace(visit);
                    }
                }
                let mut target = state.target.borrow_mut();
                if state.cell.is_cold() {
                    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                    {
                        *state.mapped.borrow_mut() = None;
                    }
                    *target = None;
                } else if let Some(target) = target.as_mut() {
                    target.trace(visit);
                }
            }
        })
    };
}

unsafe fn state(cell: u64) -> &'static State {
    // Only generated code belonging to this execution calls these entries.
    // The execution's SLOTS owns the Box until that execution has stopped.
    let cell = unsafe { &*(cell as *const CallCell) };
    unsafe { &*(cell.state_address() as *const State) }
}

/// Temporary checked-ABI adapter behind the published native contract. The
/// shared entry calls this only after preparation has declined. Reload the
/// slot here: preparation may have invalidated or replaced the target.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe extern "C" fn checked_call(
    cell: u64,
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
) {
    use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
    let invocation = unsafe { &*(*record).context };
    let entry = unsafe { &*(cell as *const CallCell) }.checked_entry_address(true);
    let entry = unsafe { &*entry }.load(std::sync::atomic::Ordering::Acquire);
    let call: unsafe extern "C" fn(u64, u64, *mut EgclVal, u64) -> u64 =
        unsafe { std::mem::transmute(entry) };
    c2i_set_native_sigsegv_recovery(1);
    let value = unsafe { call(cell, invocation.nargs as u64, invocation.args, 0) };
    c2i_set_native_sigsegv_recovery(0);
    // All checked frames have returned. This conversion does not allocate,
    // change MV, or transfer control across this Rust frame.
    unsafe {
        (*record).outcome = NativeOutcome {
            value: EgclVal(value),
            exit: if native_error_pending() {
                NativeExit::Transfer
            } else {
                NativeExit::Returned
            },
        };
    }
}

fn select_target(symbol: u32, nargs: usize) -> Option<Target> {
    let function = egcl_rt::symbols::symbol_function(symbol)?;
    if replacement_function_value(symbol, function).is_some() {
        return Some(Target::Function(function));
    }
    if egcl_rt::function::is_interpreted_function(function)
        && let Some((owner, body)) = bytecode_for_callable(symbol, function)
    {
        if owner != symbol {
            return Some(Target::Function(function));
        }
        if let Some(code) = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())
        {
            if code
                .body
                .as_ref()
                .is_some_and(|owned| Arc::ptr_eq(owned, &body))
            {
                let promote = !code.is_t2
                    && t2_enabled()
                    && !T2_DECLINED.with(|declined| declined.borrow().contains(&symbol));
                return Some(Target::Native {
                    function,
                    code,
                    promote,
                });
            }
        }
        let promote = !profiling_disabled()
            && !is_profile_pinned(symbol)
            && !T1_DECLINED.with(|declined| declined.borrow().contains(&symbol));
        return Some(Target::Bytecode {
            function,
            body,
            promote,
        });
    }
    // Memoized per symbol: a cell whose callee has no cached target -- a generic
    // function, whose symbol has no function object -- resolves again on every
    // call, and the unmemoized lookup allocates the symbol's name and scans the
    // builtin table each time.
    if let Some(slot) = super::super::direct_builtin_slot_memoized(symbol, nargs) {
        return Some(Target::Builtin { slot, nargs });
    }
    // A slot-accessor generic. Changing its function cell invalidates this
    // cell, and `accessor_call` revalidates everything else per call.
    if (nargs == 1 || nargs == 2) && super::super::names_accessor_generic(symbol) {
        return Some(Target::Accessor { nargs });
    }
    // CL:MAKE-INSTANCE. `make_instance_call` revalidates per call.
    if nargs >= 1 && super::super::names_builtin_make_instance(symbol) {
        return Some(Target::MakeInstance);
    }
    egcl_rt::function::is_interpreted_function(function).then_some(Target::Function(function))
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) struct MappedCallee {
    pub(super) code: Rc<super::native_transfer_entry::TransferCode>,
    pub(super) function: EgclVal,
}
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
impl egcl_rt::gc::TraceHostRoots for MappedCallee {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
    }
}

/// Admit a warmed mapped baseline or a mapped form of the exact installed T2
/// body. Cold resolution and pending promotion still use the existing cell.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn mapped_callee(cell: u64) -> Option<MappedCallee> {
    let state = unsafe { state(cell) };
    if state.cell.is_cold() {
        return None;
    }
    let target = state.target.borrow().clone()?;
    let Target::Native {
        code,
        function,
        promote,
    } = target
    else {
        return None;
    };
    if promote && promotion_due(function) {
        return None;
    }
    if let NativeCodeStorage::Mapped(mapped) = &code._storage {
        if mapped.mapped_entry().is_none() || mapped.needs_recompile() {
            // Only explicitly advertised managed-slot entries accept this adapter.
            // Refresh still belongs to the linkage revision transition.
            return None;
        }
        let body = code.body.as_ref()?;
        if !super::native_transfer_entry::fixed_tagged_parameters(body) {
            return None;
        }
        return Some(MappedCallee {
            code: Rc::clone(mapped),
            function,
        });
    }
    if !code.is_t2 {
        return None;
    }
    egcl_rt::rooted!(function = function);
    let body = Arc::clone(code.body.as_ref()?);
    let key = Arc::as_ptr(&body) as usize;
    if let Some((cached_key, entry)) = state.mapped.borrow().as_ref()
        && *cached_key == key
        && entry
            .code
            .as_ref()
            .is_none_or(|code| !code.needs_recompile())
    {
        return entry
            .code
            .clone()
            .filter(|code| code.mapped_entry().is_some())
            .map(|code| MappedCallee {
                code,
                function: *function,
            });
    }
    // No borrow spans compilation or another cell's resolution/root scanner.
    let code =
        super::native_transfer_entry::TransferCode::compile_nested(Arc::clone(&body)).map(Rc::new);
    *state.mapped.borrow_mut() = Some((
        key,
        super::native_segment_cache::SegmentCacheEntry::new(&body, code.clone()),
    ));
    code.filter(|code| code.mapped_entry().is_some())
        .map(|code| MappedCallee {
            code,
            function: *function,
        })
}

/// Exact callable lookup reuses named code only while that registry still
/// describes this object. Saved definitions and captured closures retain their
/// exact-object adapter instead of accidentally entering a replacement.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn mapped_callable(function: EgclVal, nargs: usize) -> Option<MappedCallee> {
    egcl_rt::rooted!(function = function);
    if function.is_symbol() {
        *function = egcl_rt::symbols::symbol_function(function.as_symbol_index())?;
    }
    if !egcl_rt::function::is_interpreted_function(*function)
        || closure_captured_env(*function).is_some()
    {
        return None;
    }
    let symbol = egcl_rt::function::name(*function).symbol_index()?;
    if egcl_rt::symbols::symbol_function(symbol) != Some(*function)
        || bytecode_for_callable(symbol, *function).is_none_or(|(owner, _)| owner != symbol)
    {
        return None;
    }
    let cell = resolve(symbol)?;
    let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
    if cell.is_cold() {
        let revision = linkage::revision(cell.ordinal());
        let target = select_target(symbol, nargs)?;
        if target.function() != *function {
            return None;
        }
        publish_target(state, revision, Some(target));
    }
    let selected = unsafe { mapped_callee(Arc::as_ptr(&cell) as u64) }?;
    (selected.function == *function).then_some(selected)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn is_builtin_funcall(cell: u64) -> bool {
    super::call_table_funcall::is_builtin(unsafe { state(cell) }.symbol)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn is_builtin_apply(cell: u64) -> bool {
    // Asked on every mapped native call; see call_table_funcall::is_builtin.
    thread_local! {
        static APPLY: std::cell::RefCell<egcl_rt::symbols::ResolvedSymbolName> =
            std::cell::RefCell::new(egcl_rt::symbols::ResolvedSymbolName::new("APPLY"));
    }
    let symbol = unsafe { state(cell) }.symbol;
    APPLY.with(|name| name.borrow_mut().refreshed_index()) == Some(symbol)
        && super::super::installed_function(symbol).is_none()
}

/// Feedback belongs only to the exact version still selected by a warm cell.
/// A retained child can finish after publication invalidated that selection.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn owns_mapped(code: &super::native_transfer_entry::TransferCode) -> bool {
    SLOTS.with(|slots| {
        slots.borrow().iter().flatten().any(|state| {
            !state.cell.is_cold()
                && state
                    .mapped
                    .borrow()
                    .as_ref()
                    .and_then(|(_, entry)| entry.code.as_ref())
                    .is_some_and(|current| std::ptr::eq(current.as_ref(), code))
        })
    })
}

fn cold(state: &State, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    #[cfg(test)]
    record_target_lookup();
    let revision = linkage::revision(state.cell.ordinal());
    egcl_rt::rooted!(result = c2i_call_result(state.symbol as u64, args, 0)?);
    #[cfg(all(target_arch = "x86_64", unix))]
    if !args.is_empty() && super::call_table_funcall::is_builtin(state.symbol) {
        let entries = bridge_entries().expect("resolved cell owns bridges");
        let mut cache = state.funcall.borrow_mut();
        if cache.is_none() {
            *cache = super::call_table_funcall::Cache::new([entries[0], entries[1]]);
        }
        if let Some(cache) = cache.as_mut() {
            *state.target.borrow_mut() = None;
            cache.synchronize_revision(revision);
            cache.select(args[0]);
            let entries = cache.entries();
            linkage::publish(&state.cell, revision, entries[0], entries[1]);
        }
        return Ok(*result);
    }
    publish_target(state, revision, select_target(state.symbol, args.len()));
    Ok(*result)
}

fn publish_target(state: &State, revision: u64, target: Option<Target>) {
    if target.is_some() {
        let entries = bridge_entries().expect("resolved cell owns installed bridges");
        #[allow(unused_mut)]
        let mut native = if target.as_ref().is_some_and(Target::is_bridge) {
            [entries[4], entries[5]]
        } else {
            [entries[2], entries[3]]
        };
        #[cfg(any(
            all(target_arch = "x86_64", unix),
            all(target_arch = "s390x", target_os = "linux")
        ))]
        if let Some(Target::Native {
            function,
            code,
            promote: false,
        }) = target.as_ref()
            && let Some(version) = super::call_table_native::NativeEntry::new(
                state.symbol,
                *function,
                Rc::clone(code),
                native,
            )
        {
            native = version.entries();
            state.versions.borrow_mut().push(version);
        }
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        let ordinary = match target.as_ref() {
            // Private semantic operations must keep their named adapter even
            // if a user binds this interned name and FUNCALL warms that body.
            Some(Target::Native {
                code,
                function,
                promote,
            }) if !super::native_transfer_entry::ordinary_primitives::is_private(state.symbol) => {
                match &code._storage {
                    NativeCodeStorage::Mapped(code) => code
                        .ordinary_descriptor(revision)
                        .map(|descriptor| (descriptor, Rc::clone(code), *function, *promote)),
                    _ => None,
                }
            }
            _ => None,
        };
        *state.target.borrow_mut() = target;
        linkage::publish(&state.cell, revision, native[0], native[1]);
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        if let Some((descriptor, code, function, promote)) = ordinary {
            // The immutable owner is retained before the revision-checked slot
            // becomes visible to generated calls in this execution.
            let heat =
                (!profiling_disabled() && egcl_rt::function::tier(function) < 2).then(|| {
                    egcl_rt::call_table::OrdinaryCallHeat {
                        descriptor: 0, // filled with the retained descriptor identity
                        invocation_counter: egcl_rt::function::invocation_counter_address(function)
                            .expect("native interpreted callable has a pinned counter")
                            as usize,
                        back_edge_counter: egcl_rt::function::back_edge_counter_address(function)
                            .expect("native interpreted callable has a pinned loop counter")
                            as usize,
                        invocation_threshold: if promote { t2_invoke_threshold() } else { 0 },
                        back_edge_threshold: if promote { t2_backedge_threshold() } else { 0 },
                    }
                });
            unsafe {
                ordinary::publish_ordinary_with_heat(
                    Arc::as_ptr(&state.cell) as u64,
                    descriptor,
                    code,
                    function,
                    heat,
                );
            }
        }
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        if super::native_transfer_entry::ordinary_primitives::is_private(state.symbol) {
            adapter::ensure(state);
        }
        #[cfg(any(
            all(target_arch = "x86_64", unix),
            all(target_arch = "s390x", target_os = "linux")
        ))]
        state
            .versions
            .borrow_mut()
            .retain(|version| version.keep(&state.cell));
    }
}

/// Ordinary cold admission shares the current named-call target's warming and
/// publication path. No table borrow spans execution of the selected callable.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn ordinary_call(symbol: u32, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    if let Some(result) = super::native_transfer_entry::ordinary_primitives::invoke(symbol, args) {
        return result;
    }
    let Some(cell) = resolve(symbol) else {
        return c2i_call_result(u64::from(symbol), args, 0);
    };
    let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
    if let Some(result) = ordinary_funcall_fast(state, args) {
        return result;
    }
    if cell.is_cold() {
        cold(state, args)
    } else {
        warm(state, args)
    }
}

/// FUNCALL or APPLY reached from an ordinary-ABI caller (bliss-6m6ca). The
/// builtin FUNCALL cell stays cold so that its mapped callers keep their
/// per-callable entries; without this, every such call would evaluate the
/// call generically through `cold` and republish that cell. Instead apply the
/// function object the way the ordinary callable veneer does: through the
/// per-execution selection cache when it holds a current target, and a symbol
/// designator through `apply_function_impl`. Both ordinary helpers that
/// reach here are published, have polled and pass rooted arguments, as that
/// veneer does, so `Admit::Any` holds.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn ordinary_funcall_fast(state: &State, args: &[EgclVal]) -> Option<Result<EgclVal, EgclError>> {
    if args.is_empty() || !super::call_table_funcall::names_funcall_or_apply(state.symbol) {
        return None;
    }
    let env = NATIVE_ENV.with(|env| env.get());
    if env.is_null() {
        return None;
    }
    let env = unsafe { &mut *env };
    // `call` is the function and its actuals, in scanned storage: a missed
    // selection may collect, so read the function from it again afterwards.
    let apply = |call: &[EgclVal], env: &mut super::super::Env| {
        let (&function, rest) = call.split_first()?;
        let admit = super::native_transfer_entry::funcall_fast::Admit::Any;
        if let Some(result) =
            super::native_transfer_entry::funcall_fast::apply(function, rest, env, admit)
        {
            return Some(result);
        }
        // A symbol names its global function, which the cache never holds;
        // `cold` would evaluate it generically and republish. A function
        // object that missed keeps the general path, whose selection the
        // next call then finds in the cache.
        call[0]
            .is_symbol()
            .then(|| super::super::apply_function_impl(call[0], &call[1..], env))
    };
    if super::call_table_funcall::is_builtin(state.symbol) {
        // Teach the cell's generated probe first: the call below may tier,
        // collect or redefine, and the probe re-checks all of that.
        adapter::record_funcall(state, args[0], args.len() - 1);
        return apply(args, env);
    }
    if args.len() < 2 || !unsafe { is_builtin_apply(Arc::as_ptr(&state.cell) as u64) } {
        return None;
    }
    // The evaluator's spreading. A dotted final list keeps the general path,
    // which signals its error. Collecting the list does not allocate Lisp
    // objects; root the spread copy before anything can.
    let (&last, leading) = args.split_last()?;
    let spread = super::super::apply_spread_list(last).ok()?;
    egcl_rt::rooted!(values = leading.to_vec());
    values.extend(spread);
    apply(&values, env)
}

/// [`ordinary_call`] for a named adapter that already holds its call cell, so
/// a compiled caller reaching a builtin or interpreted target does not
/// resolve the symbol again on every call (GitHub #487).
///
/// # Safety
/// `cell` must be a live call cell of this execution.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn ordinary_call_cell(cell: u64, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let state = unsafe { state(cell) };
    if let Some(result) =
        super::native_transfer_entry::ordinary_primitives::invoke(state.symbol, args)
    {
        return result;
    }
    if let Some(result) = ordinary_funcall_fast(state, args) {
        return result;
    }
    if state.cell.is_cold() {
        return cold(state, args);
    }
    // A warm builtin, as `warm` runs it, without cloning the target or
    // rooting a callable it does not have.
    let call = state
        .target
        .borrow()
        .as_ref()
        .and_then(|target| target.bridge_call(args.len()));
    if let Some(call) = call {
        let env = unsafe { (*state.native_env).get() };
        if !env.is_null() {
            return run_bridge_call(call, state, args, unsafe { &mut *env });
        }
    }
    warm(state, args)
}

/// Run a bridge target. Call only with no borrow of the cell held.
fn run_bridge_call(
    call: BridgeCall,
    state: &State,
    args: &[EgclVal],
    env: &mut super::super::Env,
) -> Result<EgclVal, EgclError> {
    match call {
        BridgeCall::Builtin(slot) => match super::super::call_direct_builtin(slot, args, env) {
            Some(result) => result,
            None => cold(state, args),
        },
        BridgeCall::Accessor => super::super::accessor_call(state.symbol, args, env),
        BridgeCall::MakeInstance => super::super::make_instance_call(state.symbol, args, env),
    }
}

/// What `warm` would enter for this cell when that is installed T2 code with a
/// lean ordinary root entry (see `native_transfer_entry::lean_root`): the code,
/// its callable, the cell's symbol and the entry offset. `None` leaves every
/// other case, and every tier or depth decision, to `warm`.
///
/// # Safety
/// As for [`ordinary_call_cell`].
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn lean_native_target(
    cell: u64,
    nargs: usize,
) -> Option<(
    Rc<super::native_transfer_entry::TransferCode>,
    EgclVal,
    u32,
    usize,
)> {
    let state = unsafe { state(cell) };
    if state.cell.is_cold() {
        return None;
    }
    let target = state.target.borrow();
    let Some(Target::Native {
        code,
        function,
        promote: false,
    }) = target.as_ref()
    else {
        return None;
    };
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        return None;
    };
    // Only T2 code: `warm` counts invocations below T2.
    if !code.is_t2
        || mapped.needs_recompile()
        || !code
            .body
            .as_ref()
            .is_some_and(|body| arity_accepts(body, nargs))
        || NATIVE_DEPTH.with(|depth| depth.get()) >= native_depth_cap()
    {
        return None;
    }
    let offset = mapped.lean_root_entry()?;
    Some((Rc::clone(mapped), *function, state.symbol, offset))
}

/// The symbol a named adapter's cell dispatches.
///
/// # Safety
/// As for [`ordinary_call_cell`].
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn cell_symbol(cell: u64) -> u32 {
    unsafe { state(cell) }.symbol
}

/// Whether [`mapped_callee`] is certain to decline this warm cell without
/// compiling anything: a builtin, bytecode or function target, or installed
/// code with no managed-slot entry (an ordinary-ABI body). The mapped-call
/// entry then goes straight to the checked call instead of first preparing a
/// child it cannot use (GitHub #487). FUNCALL and APPLY are never declined:
/// their builtin cells have mapped forms of their own.
///
/// # Safety
/// As for [`mapped_callee`].
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) unsafe fn mapped_callee_declines(cell: u64) -> bool {
    let state = unsafe { state(cell) };
    if state.cell.is_cold() {
        return false;
    }
    match state.target.borrow().as_ref() {
        Some(Target::Builtin { .. }) => {
            !super::call_table_funcall::names_funcall_or_apply(state.symbol)
        }
        Some(Target::Accessor { .. } | Target::MakeInstance) => true,
        Some(Target::Native { code, .. }) => match &code._storage {
            NativeCodeStorage::Mapped(mapped) => mapped.mapped_entry().is_none(),
            _ => false,
        },
        Some(Target::Bytecode { .. } | Target::Function(_)) => {
            !super::call_table_funcall::names_funcall_or_apply(state.symbol)
        }
        None => false,
    }
}

fn promotion_due(function: EgclVal) -> bool {
    egcl_rt::function::invoke_count(function).saturating_add(1) >= t2_invoke_threshold()
        || egcl_rt::function::back_edge_count(function) >= t2_backedge_threshold()
}

fn warm(state: &State, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    #[cfg(test)]
    GENERAL_DISPATCHES.with(|count| count.set(count.get() + 1));
    // Drop the RefCell borrow BEFORE a call can allocate, yield, redefine the
    // target or trigger the GC scanner. The clone keeps its old code alive.
    let target = { state.target.borrow().clone() };
    let Some(target) = target else {
        return cold(state, args);
    };
    egcl_rt::rooted!(function = target.function());
    let env_ptr = NATIVE_ENV.with(|env| env.get());
    if env_ptr.is_null() {
        return Ok(NIL);
    }
    let env = unsafe { &mut *env_ptr };
    match target {
        Target::Native { code, promote, .. } => {
            let Some(body) = &code.body else {
                return cold(state, args);
            };
            if !arity_accepts(body, args.len()) {
                return cold(state, args);
            }
            if promote && promotion_due(*function) {
                return cold(state, args);
            }
            if !code.is_t2 && !profiling_disabled() {
                egcl_rt::function::record_invocation(*function);
            }
            if NATIVE_DEPTH.with(|depth| depth.get()) >= native_depth_cap() {
                run_with_sym(Arc::clone(body), args, *function, state.symbol, env)
            } else {
                run_native_callable(&code, state.symbol, *function, args, env)
            }
        }
        Target::Bytecode { body, promote, .. } => {
            if !arity_accepts(&body, args.len()) {
                return cold(state, args);
            }
            if promote
                && egcl_rt::function::invoke_count(*function).saturating_add(1) >= t1_threshold()
            {
                return cold(state, args);
            }
            if !profiling_disabled() {
                egcl_rt::function::record_invocation(*function);
            }
            run_with_sym(body, args, *function, state.symbol, env)
        }
        Target::Builtin { .. } | Target::Accessor { .. } | Target::MakeInstance => {
            match target.bridge_call(args.len()) {
                Some(call) => run_bridge_call(call, state, args, env),
                None => cold(state, args),
            }
        }
        Target::Function(_) => apply_function(*function, args, env),
    }
}

extern "C" fn cold_register(cell: u64, n: u64, a0: u64, a1: u64, a2: u64, _: u64) -> u64 {
    registers(cell, n, [EgclVal(a0), EgclVal(a1), EgclVal(a2)], false)
}
extern "C" fn warm_register(cell: u64, n: u64, a0: u64, a1: u64, a2: u64, _: u64) -> u64 {
    registers(cell, n, [EgclVal(a0), EgclVal(a1), EgclVal(a2)], true)
}
fn registers(cell: u64, n: u64, args: [EgclVal; 3], ready: bool) -> u64 {
    with_register_args(n, args, |args| invoke(cell, args, ready))
}
fn with_register_args(n: u64, args: [EgclVal; 3], call: impl FnOnce(&[EgclVal]) -> u64) -> u64 {
    if n > 3 {
        return NIL.0;
    }
    // The native ABI leaves unused argument registers unspecified. They must
    // never enter the precise root set, even though the callee ignores them.
    egcl_rt::rooted!(
        args = [
            if n > 0 { args[0] } else { NIL },
            if n > 1 { args[1] } else { NIL },
            if n > 2 { args[2] } else { NIL },
        ]
    );
    call(&args[..n as usize])
}
extern "C" fn cold_slice(cell: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    slice(cell, n, args, false)
}
extern "C" fn warm_slice(cell: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    slice(cell, n, args, true)
}
fn slice(cell: u64, n: u64, args: *const EgclVal, ready: bool) -> u64 {
    if n != 0 && args.is_null() {
        return NIL.0;
    }
    // The generated caller owns this GC-scanned activation slice.
    let args = if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, n as usize) }
    };
    invoke(cell, args, ready)
}
fn invoke(cell: u64, args: &[EgclVal], ready: bool) -> u64 {
    finish_c2i_call(guard_c2i(|| {
        let state = unsafe { state(cell) };
        if ready {
            warm(state, args)
        } else {
            cold(state, args)
        }
    }))
}

extern "C" fn builtin_register(cell: u64, n: u64, a0: u64, a1: u64, a2: u64, _: u64) -> u64 {
    with_register_args(n, [EgclVal(a0), EgclVal(a1), EgclVal(a2)], |args| {
        invoke_builtin(cell, args)
    })
}

extern "C" fn builtin_slice(cell: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    if n != 0 && args.is_null() {
        return NIL.0;
    }
    // As in the general slice bridge, the caller's activation roots this slice.
    let args = if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, n as usize) }
    };
    invoke_builtin(cell, args)
}

fn invoke_builtin(cell: u64, args: &[EgclVal]) -> u64 {
    // Finish each branch inside the panic boundary. Forwarding the full Rust
    // Result through a shared return block copies its error payload even on
    // success; the native caller only needs the tagged value and pending error.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state = unsafe { state(cell) };
        // Copy just the builtin metadata and release its borrow before any
        // allocation or Lisp reentry. Builtins have no callable object to root.
        let call = state
            .target
            .borrow()
            .as_ref()
            .and_then(|target| target.bridge_call(args.len()));
        let Some(call) = call else {
            return finish_c2i_call(cold(state, args));
        };
        // The owning execution cannot retire while one of its calls is active.
        let env = unsafe { (*state.native_env).get() };
        if env.is_null() {
            return NIL.0;
        }
        finish_c2i_call(run_bridge_call(call, state, args, unsafe { &mut *env }))
    })) {
        Ok(value) => value,
        Err(payload) => finish_c2i_call(Err(c2i_panic_error(payload))),
    }
}

#[cfg(test)]
thread_local! {
    static TARGET_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static GENERAL_DISPATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    static RESOLVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Symbol-to-cell resolutions on this thread since the last call.
#[cfg(test)]
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
pub(super) fn take_resolves_for_test() -> usize {
    RESOLVES.with(|count| count.replace(0))
}

/// Native entries and tier dispatches recorded on this thread since the last call.
#[cfg(test)]
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
pub(super) fn take_target_lookups_for_test() -> usize {
    TARGET_LOOKUPS.with(|count| count.replace(0))
}

#[cfg(test)]
pub(super) fn record_target_lookup() {
    TARGET_LOOKUPS.with(|count| count.set(count.get() + 1));
}

#[cfg(all(test, target_arch = "x86_64", unix))]
#[path = "call_table_builtin_return_tests.rs"]
mod builtin_return_tests;

#[cfg(all(test, target_arch = "x86_64", unix))]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn private_primitive_keeps_its_adapter_after_public_callable_warming() {
        use std::sync::atomic::Ordering;
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        crate::cli::read_eval_all_env(
            "(setq *ordinary-private-cell-value* 73)
             (defun ordinary-private-cell-reader () *ordinary-private-cell-value*)",
            &mut env,
        )
        .unwrap();
        let reader_symbol = egcl_rt::symbols::intern("ORDINARY-PRIVATE-CELL-READER");
        egcl_rt::rooted!(params = reader::read_from_string("()").unwrap().0);
        egcl_rt::rooted!(
            forms = reader::read_from_string("(*ordinary-private-cell-value*)")
                .unwrap()
                .0
        );
        let body = Arc::new(
            compile_function(
                "ORDINARY-PRIVATE-CELL-READER",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        registry_put(reader_symbol, Arc::clone(&body));
        let reader_code = native_transfer_entry::install_t1(reader_symbol, body, false).unwrap();
        let NativeCodeStorage::Mapped(reader_owner) = &reader_code._storage else {
            unreachable!()
        };
        assert!(reader_owner.ordinary_entry.is_some());
        egcl_rt::rooted!(
            reader_function = egcl_rt::symbols::symbol_function(reader_symbol).unwrap()
        );
        publish_native(reader_symbol, Some(*reader_function), &reader_code);
        let private_symbol = egcl_rt::symbols::find_index("%EGCL-ORDINARY-LOADGLOBAL").unwrap();
        assert!(native_transfer_entry::ordinary_primitives::is_private(
            private_symbol
        ));
        egcl_rt::rooted!(
            previous = egcl_rt::symbols::symbol_function(private_symbol)
                .unwrap_or(egcl_rt::value::UNBOUND)
        );
        for replacement in [991, 992] {
            crate::cli::read_eval_all_env(
                &format!("(defun %egcl-ordinary-loadglobal (x) {replacement})"),
                &mut env,
            )
            .unwrap();
            egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
            egcl_rt::rooted!(
                forms = reader::read_from_string(&format!("({replacement})"))
                    .unwrap()
                    .0
            );
            let body = Arc::new(
                compile_function(
                    "%EGCL-ORDINARY-LOADGLOBAL",
                    *params,
                    *forms,
                    &env,
                    false,
                    false,
                )
                .unwrap(),
            );
            registry_put(private_symbol, Arc::clone(&body));
            let replacement_code =
                native_transfer_entry::install_t1(private_symbol, body, false).unwrap();
            let NativeCodeStorage::Mapped(replacement_owner) = &replacement_code._storage else {
                unreachable!()
            };
            assert!(replacement_owner.ordinary_entry.is_some());
            egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(private_symbol).unwrap());
            publish_native(private_symbol, Some(*function), &replacement_code);
            // This is the exact callable-selection path used by FUNCALL. It
            // must warm the public binding without replacing primitive ingress.
            let cell = resolve(private_symbol).unwrap();
            assert!(mapped_callable(*function, 1).is_some());
            let warmed = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
            assert_ne!(warmed, 0);
            assert_eq!(
                unsafe { (*(warmed as *const linkage::OrdinaryEntryDescriptor)).arity },
                linkage::ORDINARY_ARITY_ANY,
                "warming must preserve the semantic adapter immediately"
            );
            // Reproduce an earlier native publication from before the private
            // IDs were initialized. Its exact owner must remain retained while
            // resolve replaces the stale public-body descriptor.
            let descriptor = replacement_owner
                .ordinary_descriptor(linkage::revision(cell.ordinal()))
                .unwrap();
            unsafe {
                ordinary::publish_ordinary_with_heat(
                    Arc::as_ptr(&cell) as u64,
                    descriptor,
                    Rc::clone(replacement_owner),
                    *function,
                    None,
                );
            }
            let seeded = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
            assert_eq!(
                unsafe { (*(seeded as *const linkage::OrdinaryEntryDescriptor)).owner },
                Rc::as_ptr(replacement_owner) as usize
            );
            resolve(private_symbol).unwrap();
            let pointer = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
            assert_ne!(pointer, 0);
            let descriptor = unsafe { &*(pointer as *const linkage::OrdinaryEntryDescriptor) };
            assert_eq!(descriptor.arity, linkage::ORDINARY_ARITY_ANY);
            assert_ne!(descriptor.owner, Rc::as_ptr(replacement_owner) as usize);
            assert_eq!(
                unsafe { &*cell.ordinary_heat_address() }.load(Ordering::Acquire),
                0
            );
            assert_eq!(
                crate::cli::read_eval_all_env(
                    "(funcall #'%egcl-ordinary-loadglobal nil)",
                    &mut env
                )
                .unwrap(),
                EgclVal::from_fixnum(replacement)
            );
            assert_eq!(
                run_native_callable(&reader_code, reader_symbol, *reader_function, &[], &mut env)
                    .unwrap(),
                EgclVal::from_fixnum(73),
                "compiled global reads retain primitive semantics"
            );
        }
        egcl_rt::symbols::set_symbol_function(private_symbol, *previous);
        registry_remove(private_symbol);
    }

    #[test]
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn promotable_t1_publishes_real_ordinary_code_and_retained_heat() {
        use std::sync::atomic::Ordering;
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "ORDINARY-PROMOTABLE-T1";
        super::super::super::read_eval_all_env("(defun ordinary-promotable-t1 (x) x)", &mut env)
            .unwrap();
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        registry_put(symbol, body);
        let code = try_promote_to_t1_with_speculation(symbol, false).unwrap();
        assert!(!code.is_t2);
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            panic!("ordinary T1")
        };
        let owner = Rc::as_ptr(mapped) as usize;
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        let revision = linkage::revision(cell.ordinal());
        publish_target(
            state,
            revision,
            Some(Target::Native {
                code: Rc::clone(&code),
                function: *function,
                promote: true,
            }),
        );
        let pointer = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
        assert_ne!(pointer, 0);
        let descriptor = unsafe { &*(pointer as *const linkage::OrdinaryEntryDescriptor) };
        assert_eq!(
            descriptor.owner, owner,
            "promotable T1 must bypass the named adapter"
        );
        if !profiling_disabled() {
            let heat = unsafe { &*cell.ordinary_heat_address() }.load(Ordering::Acquire);
            assert_ne!(heat, 0);
            let heat = unsafe { &*(heat as *const linkage::OrdinaryCallHeat) };
            assert_eq!(heat.descriptor, pointer);
            assert_eq!(
                heat.invocation_counter,
                egcl_rt::function::invocation_counter_address(*function).unwrap() as usize
            );
            assert_eq!(heat.invocation_threshold, t2_invoke_threshold());
            assert_eq!(heat.back_edge_threshold, t2_backedge_threshold());
            let original_heat = heat as *const linkage::OrdinaryCallHeat;
            publish_target(
                state,
                revision,
                Some(Target::Native {
                    code: Rc::clone(&code),
                    function: *function,
                    promote: false,
                }),
            );
            let updated = unsafe { &*cell.ordinary_heat_address() }.load(Ordering::Acquire);
            assert_ne!(
                updated, original_heat as usize,
                "declined T2 replaces heat policy without mutating an in-flight record"
            );
            let updated = unsafe { &*(updated as *const linkage::OrdinaryCallHeat) };
            assert_eq!(updated.descriptor, pointer);
            assert_eq!(updated.invocation_threshold, 0);
            assert_eq!(updated.back_edge_threshold, 0);
            assert_eq!(
                unsafe { (*original_heat).invocation_threshold },
                t2_invoke_threshold()
            );
        }
        linkage::invalidate(symbol);
        assert_eq!(
            unsafe { &*cell.ordinary_heat_address() }.load(Ordering::Acquire),
            0
        );
        adapter::ensure(state);
        assert_eq!(
            unsafe { &*cell.ordinary_heat_address() }.load(Ordering::Acquire),
            0,
            "replacement named adapter must not count the old callable"
        );
    }

    #[test]
    fn unused_argument_registers_are_not_exposed_to_the_collector() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _thread = egcl_rt::thread::current_thread_id();
        let used = EgclVal::from_char('z');
        // These are deliberately not Lisp values. Inspect the root slots
        // themselves: out-of-heap poison can be ignored by a normal GC and
        // therefore would not reliably detect the invalid root publication.
        let unused = [EgclVal(0xfafafafafafafafa), EgclVal(0xfbfbfbfbfbfbfbfa)];
        for n in [0, 1] {
            with_register_args(n, [used, unused[0], unused[1]], |args| {
                assert_eq!(args.len(), n as usize);
                let (mut found_used, mut found_unused) = (false, false);
                egcl_rt::gc::with_heap_snapshot(|| unsafe {
                    egcl_rt::gc::visit_shake_host_roots(&[], &mut |slot| {
                        let value = *slot;
                        found_used |= slot == args.as_ptr().cast_mut() && value == used;
                        found_unused |= unused.contains(&value);
                    });
                })
                .unwrap();
                assert!(
                    !found_unused,
                    "unused register words became precise GC roots"
                );
                assert_eq!(found_used, n != 0, "only supplied arguments may be rooted");
                NIL.0
            });
        }
    }

    #[test]
    fn warmed_checked_slot_preserves_installed_target_results() {
        check_native_slot("SLOT-LOOKUP-LEAF", "()", "42", &[], 42);
        check_native_slot(
            "SLOT-LOOKUP-THREE",
            "(a b c)",
            "(+ a (+ b c))",
            &[1, 2, 3],
            6,
        );
        check_native_slot(
            "SLOT-LOOKUP-WIDE",
            "(a b c d)",
            "(+ a (+ b (+ c d)))",
            &[1, 2, 3, 4],
            10,
        );
    }

    #[test]
    fn supplied_optional_arguments_preserve_checked_slot_results() {
        check_native_slot(
            "SLOT-LOOKUP-OPTIONAL",
            "(a &optional (b 20) (c 30) (d 40))",
            "(+ a (+ b (+ c d)))",
            &[1, 2, 3, 4],
            10,
        );
    }

    #[test]
    fn literal_optional_defaults_preserve_checked_slot_results() {
        check_native_slot(
            "SLOT-LOOKUP-DEFAULTS",
            "(a &optional (b 20) (c 30) (d 40))",
            "(+ a (+ b (+ c d)))",
            &[1, 2],
            73,
        );
        check_native_slot(
            "SLOT-LOOKUP-NO-REQUIRED",
            "(&optional (a 10) (b 20))",
            "(+ a b)",
            &[],
            30,
        );
        check_native_slot(
            "SLOT-LOOKUP-NIL-DEFAULT",
            "(a &optional b)",
            "(if b a (+ a 1))",
            &[1],
            2,
        );
    }

    #[test]
    fn large_depth_caps_preserve_checked_slot_calls() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::bytecode::call_table::tests::warmed_checked_slot_preserves_installed_target_results",
                "--test-threads=1"])
            .env("EGCL_NATIVE_DEPTH_CAP", "4000000000")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn gethash_slot_caches_builtin_and_preserves_both_values() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(
            table =
                super::super::super::read_eval_all_env("(make-hash-table :test 'eql)", &mut env,)
                    .unwrap()
        );
        let symbol = super::super::super::resolve_sym("GETHASH")
            .unwrap()
            .as_symbol_index();
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        egcl_rt::rooted!(args = [EgclVal::from_fixnum(7), *table, EgclVal::from_fixnum(99)]);
        for nargs in [2, 3] {
            egcl_stdlib::remhash(args[0], *table).unwrap();
            let default = if nargs == 2 { NIL } else { args[2] };
            assert_eq!(cold(state, &args[..nargs]).unwrap(), default);
            assert_eq!(env.mv, vec![default, NIL]);
            assert!(env.mv_active);
            assert!(!cell.is_cold(), "GETHASH must install its builtin target");
            assert!(matches!(
                *state.target.borrow(),
                Some(Target::Builtin { .. })
            ));
            egcl_stdlib::set_gethash(args[0], *table, NIL).unwrap();
            TARGET_LOOKUPS.with(|count| count.set(0));
            GENERAL_DISPATCHES.with(|count| count.set(0));
            for _ in 0..100 {
                let entry = unsafe { &*cell.checked_entry_address(false) }
                    .load(std::sync::atomic::Ordering::Acquire);
                let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                assert_eq!(
                    call(
                        Arc::as_ptr(&cell) as u64,
                        nargs as u64,
                        args[0].0,
                        args[1].0,
                        args[2].0,
                        0
                    ),
                    NIL.0
                );
                assert_eq!(env.mv, vec![NIL, T], "a stored NIL is present");
                assert!(env.mv_active);
            }
            assert_eq!(
                TARGET_LOOKUPS.with(|count| count.get()),
                0,
                "warmed GETHASH calls must not resolve their target again"
            );
            assert_eq!(
                GENERAL_DISPATCHES.with(|count| count.get()),
                0,
                "warmed builtins must bypass general target dispatch"
            );
        }
        // Ordinary callable entry attaches its native backtrace to errors.
        // Check the original condition rather than requiring an untraced enum.
        let arity_error = warm(state, &args[..1]).unwrap_err();
        assert!(matches!(
            arity_error.without_backtrace(),
            EgclError::ProgramError(_)
        ));
        let type_error = warm(state, &[args[0], NIL]).unwrap_err();
        assert!(matches!(
            type_error.without_backtrace(),
            EgclError::TypeError { .. }
        ));
    }

    #[test]
    fn builtin_slice_reads_the_current_activation_environment() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut first = Env::new(false);
        egcl_rt::rooted_ref!(_first_root = &mut first);
        let mut second = Env::new(false);
        egcl_rt::rooted_ref!(_second_root = &mut second);
        let symbol = super::super::super::resolve_sym("VALUES")
            .unwrap()
            .as_symbol_index();
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut first)));
        let args = [1, 2, 3, 4].map(EgclVal::from_fixnum);
        assert_eq!(cold(state, &args).unwrap(), args[0]);
        first.clear_mv();
        NATIVE_ENV.with(|slot| slot.set(&mut second));
        GENERAL_DISPATCHES.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.checked_entry_address(true) }
                .load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 4, args.as_ptr(), 0),
                args[0].0
            );
            assert_eq!(second.mv, args);
            assert!(second.mv_active);
            assert!(first.mv.is_empty());
            assert!(!first.mv_active);
        }
        assert_eq!(GENERAL_DISPATCHES.with(|count| count.get()), 0);
        // A new arity must re-resolve and install a usable zero-argument entry.
        for _ in 0..2 {
            let entry = unsafe { &*cell.checked_entry_address(true) }
                .load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 0, std::ptr::null(), 0),
                NIL.0
            );
            assert!(second.mv.is_empty());
            assert!(second.mv_active);
        }
    }

    #[test]
    fn native_frame_header_keeps_its_callable_alive() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let stack = egcl_rt::current_stack();
        let (frame, symbol) = {
            egcl_rt::rooted!(form = super::super::super::vec_to_list(&[NIL]));
            let body = Arc::new(
                compile_function("FRAME-OWNED-CALLBACK", NIL, *form, &env, false, false).unwrap(),
            );
            egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
            let symbol = egcl_rt::function::name(*function).as_symbol_index();
            (
                stack
                    .push_frame(*function, std::ptr::null(), 0, FLAG_CALL)
                    .unwrap(),
                symbol,
            )
        };
        env.mv.clear();
        egcl_rt::gc::full_gc().unwrap();
        let retained = is_registered(symbol);
        let callable = unsafe { (*frame).function };
        stack.pop_frame();
        assert!(
            retained,
            "the activation header must keep its callable and captures alive"
        );
        assert!(egcl_rt::function::is_interpreted_function(callable));
    }

    #[test]
    fn checked_funcall_preserves_warm_callback_results() {
        check_native_funcall(false);
    }

    #[test]
    fn captured_checked_funcall_preserves_warm_callback_results() {
        check_native_funcall(true);
    }

    #[test]
    fn nested_funcall_preserves_callback_identity_under_the_installed_abi() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        for (name, expression) in [
            ("PIC-LEAF", "(+ x 1)"),
            ("PIC-MIDDLE", "(funcall #'pic-leaf x)"),
            ("PIC-OUTER", "(funcall #'pic-middle x)"),
            ("PIC-INDEPENDENT", "(+ x 2)"),
        ] {
            super::super::super::read_eval_all_env(
                &format!("(defun {name} (x) {expression})"),
                &mut env,
            )
            .unwrap();
            let symbol = egcl_rt::symbols::intern(name);
            egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
            egcl_rt::rooted!(
                form = reader::read_from_string(&format!("({expression})"))
                    .unwrap()
                    .0
            );
            let body =
                Arc::new(compile_function(name, *params, *form, &env, false, false).unwrap());
            let needs_function_lookup = matches!(name, "PIC-MIDDLE" | "PIC-OUTER");
            assert_eq!(
                body.code
                    .iter()
                    .any(|op| matches!(op, Instr::LoadFunction(_))),
                needs_function_lookup
            );
            registry_put(symbol, body);
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact);
            // Callers that look up a global function (LoadFunction) are
            // admitted to mapped T2 on x86-64 Linux as well (#460).
            let code = install_t2_completion(T2Completion {
                panicked: false,
                _permit: None,
                dependencies: Vec::new(),
                sym: symbol,
                generation,
                artifact: Some(artifact.expect("compile admitted callback")),
                input,
            })
            .unwrap();
            assert!(code.is_t2);
            #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
            {
                assert!(matches!(code._storage, NativeCodeStorage::Mapped(_)));
                let expected = if name == "PIC-INDEPENDENT" { 43 } else { 42 };
                assert_eq!(
                    run_native(&code, symbol, &[EgclVal::from_fixnum(41)], &mut env).unwrap(),
                    EgclVal::from_fixnum(expected)
                );
            }
        }
        let cell = resolve(egcl_rt::symbols::intern("FUNCALL")).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        egcl_rt::rooted!(
            function =
                egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-OUTER")).unwrap()
        );
        egcl_rt::rooted!(args = [*function, EgclVal::from_fixnum(41)]);
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        for _ in 0..3 {
            assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        }
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.checked_entry_address(false) }
                .load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
                EgclVal::from_fixnum(42).0
            );
        }
        if checked_native_installation_allowed() {
            assert_eq!(
                TARGET_LOOKUPS.with(|count| count.get()),
                0,
                "nested callbacks must not evict each other's warmed native identity"
            );
        } else {
            assert!(
                TARGET_LOOKUPS.with(|count| count.get()) >= 100,
                "Linux calls through checked_entry_address must retain bridge dispatch"
            );
            assert!(
                !state
                    .funcall
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .select(*function),
                "the checked native PIC must reject a mapped callback"
            );
        }
        let symbol = egcl_rt::symbols::intern("PIC-LEAF");
        let input = snapshot_t2_input(symbol, 0).unwrap();
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
        install_t2_completion(T2Completion {
            panicked: false,
            _permit: None,
            dependencies: Vec::new(),
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .unwrap();
        args[0] =
            egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-INDEPENDENT")).unwrap();
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(43));
        args[0] = egcl_rt::symbols::symbol_function(symbol).unwrap();
        TARGET_LOOKUPS.with(|count| count.set(0));
        let entry = unsafe { &*cell.checked_entry_address(false) }
            .load(std::sync::atomic::Ordering::Acquire);
        let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(entry) };
        assert_eq!(
            call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
            EgclVal::from_fixnum(42).0
        );
        assert!(
            TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "republication must invalidate every identity, even when another callback refills first"
        );
        args[0] = *function;
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        egcl_rt::gc::full_gc().unwrap();
        for name in ["PIC-LEAF", "PIC-MIDDLE", "PIC-OUTER"] {
            args[0] = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern(name)).unwrap();
            TARGET_LOOKUPS.with(|count| count.set(0));
            let entry = unsafe { &*cell.checked_entry_address(false) }
                .load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
                EgclVal::from_fixnum(42).0
            );
            assert!(TARGET_LOOKUPS.with(|count| count.get()) > 0);
            if !checked_native_installation_allowed() {
                // Linux has no checked-native PIC identity to invalidate. It
                // must still refuse that ABI and execute the current callback.
                assert!(!state.funcall.borrow_mut().as_mut().unwrap().select(args[0]));
            }
        }
        if !checked_native_installation_allowed() {
            egcl_rt::rooted!(
                selected_leaf =
                    egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-LEAF"))
                        .unwrap()
            );
            super::super::super::read_eval_all_env("(defun pic-leaf (x) (+ x 10))", &mut env)
                .unwrap();
            // The selected leaf is an old callable; the selected OUTER performs
            // FUNCTION lookup inside its unchanged body and sees the new leaf.
            for collect in [false, true] {
                if collect {
                    egcl_rt::gc::full_gc().unwrap();
                }
                egcl_rt::rooted!(
                    callbacks = vec![
                        *selected_leaf,
                        *function,
                        egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern(
                            "PIC-INDEPENDENT"
                        ))
                        .unwrap(),
                        egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-LEAF"))
                            .unwrap(),
                    ]
                );
                for (index, expected) in [42, 51, 43, 51].into_iter().enumerate() {
                    args[0] = callbacks[index];
                    let entry = unsafe { &*cell.checked_entry_address(false) }
                        .load(std::sync::atomic::Ordering::Acquire);
                    let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                        unsafe { std::mem::transmute(entry) };
                    assert_eq!(
                        call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
                        EgclVal::from_fixnum(expected).0
                    );
                }
            }
        }
    }

    fn check_native_funcall(captured: bool) {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        for (name, params, expression) in [
            ("OBJECT-CALL-LEAF", "(x)", "(+ x 1)"),
            ("OBJECT-CALL-CALLER", "(f x)", "(funcall f x)"),
        ] {
            super::super::super::read_eval_all_env(
                &format!("(defun {name} {params} {expression})"),
                &mut env,
            )
            .unwrap();
            let symbol = egcl_rt::symbols::intern(name);
            egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
            egcl_rt::rooted!(
                form = reader::read_from_string(&format!("({expression})"))
                    .unwrap()
                    .0
            );
            let body = compile_function(name, *params, *form, &env, false, false).unwrap();
            registry_put(symbol, Arc::new(body));
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
            assert!(
                install_t2_completion(T2Completion {
                    panicked: false,
                    _permit: None,
                    dependencies: Vec::new(),
                    sym: symbol,
                    generation,
                    artifact: Some(artifact),
                    input,
                })
                .unwrap()
                .is_t2
            );
        }
        egcl_rt::rooted!(
            function =
                egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("OBJECT-CALL-LEAF"))
                    .unwrap()
        );
        if captured {
            egcl_rt::rooted!(params = reader::read_from_string("(seed)").unwrap().0);
            egcl_rt::rooted!(
                form = reader::read_from_string("((lambda (x) (+ seed x)))")
                    .unwrap()
                    .0
            );
            let factory =
                compile_function("OBJECT-CAPTURE-FACTORY", *params, *form, &env, true, false)
                    .unwrap();
            let body = Arc::clone(&factory.nested_functions[0]);
            let capture = Arc::new(SharedCell::new(EnvFrame {
                vars: Default::default(),
                symbol_vars: Default::default(),
                parent: None,
            }));
            bind_boxed_param(&capture, "SEED", EgclVal::from_fixnum(1));
            *function = make_bytecode_closure(&body, Some(capture));
            let symbol = egcl_rt::function::name(*function).as_symbol_index();
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact);
            // Closure bodies that read captured bindings reach mapped native
            // code on x86-64 Linux as well (#494).
            assert!(
                install_t2_completion(T2Completion {
                    panicked: false,
                    _permit: None,
                    dependencies: Vec::new(),
                    sym: symbol,
                    generation,
                    artifact: Some(artifact.expect("compile captured callback")),
                    input,
                })
                .unwrap()
                .is_t2
            );
        }
        egcl_rt::rooted!(args = [*function, EgclVal::from_fixnum(41)]);
        let cell = resolve(egcl_rt::symbols::intern("OBJECT-CALL-CALLER")).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        TARGET_LOOKUPS.with(|count| count.set(0));
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        assert!(TARGET_LOOKUPS.with(|count| count.get()) > 0);
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.checked_entry_address(false) }
                .load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
                EgclVal::from_fixnum(42).0
            );
        }
        if checked_native_installation_allowed() {
            assert_eq!(
                TARGET_LOOKUPS.with(|count| count.get()),
                0,
                "native FUNCALL must bypass target lookup and Rust frame setup"
            );
        } else {
            assert!(
                TARGET_LOOKUPS.with(|count| count.get()) >= 100,
                "checked FUNCALL entries must use the Linux bridge"
            );
        }
        let symbol = egcl_rt::function::name(*function).as_symbol_index();
        let input = snapshot_t2_input(symbol, 0).unwrap();
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable(compile_t2_artifact);
        assert!(
            install_t2_completion(T2Completion {
                panicked: false,
                _permit: None,
                dependencies: Vec::new(),
                sym: symbol,
                generation,
                artifact: Some(artifact.expect("recompile callback")),
                input,
            })
            .unwrap()
            .is_t2
        );
        TARGET_LOOKUPS.with(|count| count.set(0));
        let entry = unsafe { &*cell.checked_entry_address(false) }
            .load(std::sync::atomic::Ordering::Acquire);
        let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(entry) };
        assert_eq!(
            call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
            EgclVal::from_fixnum(42).0
        );
        assert!(
            TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "replacing installed native code must invalidate the callback cache"
        );
        // A previous ordinary target must not remain a strong root when the
        // same slot switches to the weak native-FUNCALL dispatcher.
        let funcall_cell = resolve(egcl_rt::symbols::intern("FUNCALL")).unwrap();
        let funcall_state = unsafe { super::state(Arc::as_ptr(&funcall_cell) as u64) };
        let obsolete_symbol = {
            egcl_rt::rooted!(form = super::super::super::vec_to_list(&[NIL]));
            let body = Arc::new(
                compile_function("OBSOLETE-FUNCALL-TARGET", NIL, *form, &env, false, false)
                    .unwrap(),
            );
            egcl_rt::rooted!(obsolete = make_bytecode_closure(&body, None));
            *funcall_state.target.borrow_mut() = Some(Target::Function(*obsolete));
            egcl_rt::function::name(*obsolete).as_symbol_index()
        };
        assert_eq!(
            cold(funcall_state, &args[..]).unwrap(),
            EgclVal::from_fixnum(42)
        );
        env.mv.clear();
        egcl_rt::gc::full_gc().unwrap();
        assert!(
            !is_registered(obsolete_symbol),
            "installing native FUNCALL must release the previous generic target"
        );
    }

    fn check_native_slot(name: &str, params: &str, expression: &str, args: &[i64], expected: i64) {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        super::super::super::read_eval_all_env(
            &format!("(defun {name} {params} {expression})"),
            &mut env,
        )
        .unwrap();
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
        egcl_rt::rooted!(
            form = reader::read_from_string(&format!("({expression})"))
                .unwrap()
                .0
        );
        let body = compile_function(name, *params, *form, &env, false, false)
            .expect("compile native slot body");
        let args: Vec<EgclVal> = args.iter().copied().map(EgclVal::from_fixnum).collect();
        egcl_rt::rooted!(args = args);
        registry_put(symbol, Arc::new(body));
        let input = snapshot_t2_input(symbol, 0).expect("snapshot leaf");
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable(compile_t2_artifact);
        // &optional bodies reach mapped T1/T2 on x86-64 Linux too (#457).
        let code = install_t2_completion(T2Completion {
            panicked: false,
            _permit: None,
            dependencies: Vec::new(),
            sym: symbol,
            generation,
            artifact: Some(artifact.expect("compile T2 leaf")),
            input,
        })
        .expect("install T2 leaf");
        assert!(code.is_t2);
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        assert!(matches!(code._storage, NativeCodeStorage::Mapped(_)));
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        TARGET_LOOKUPS.with(|count| count.set(0));
        assert_eq!(cold(state, &args).unwrap(), EgclVal::from_fixnum(expected));
        assert!(
            TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "cold-call probe must be live"
        );
        assert!(!cell.is_cold());
        assert!(matches!(
            *state.target.borrow(),
            Some(Target::Native { .. })
        ));
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.checked_entry_address(args.len() > 3) }
                .load(std::sync::atomic::Ordering::Acquire);
            let result = if args.len() > 3 {
                let call: extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                call(
                    Arc::as_ptr(&cell) as u64,
                    args.len() as u64,
                    args.as_ptr(),
                    0,
                )
            } else {
                let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                let arg = |i| args.get(i).copied().unwrap_or(NIL).0;
                call(
                    Arc::as_ptr(&cell) as u64,
                    args.len() as u64,
                    arg(0),
                    arg(1),
                    arg(2),
                    0,
                )
            };
            assert_eq!(result, EgclVal::from_fixnum(expected).0);
        }
        if checked_native_installation_allowed() {
            assert_eq!(
                TARGET_LOOKUPS.with(|count| count.get()),
                0,
                "warm checked-native calls bypass target resolution"
            );
        } else {
            // This probe deliberately enters checked_entry_address. Mapped
            // callers use the separate published cell entry tested by linkage.
            assert!(
                TARGET_LOOKUPS.with(|count| count.get()) >= 100,
                "checked bridge calls must resolve their Linux mapped/interpreter target"
            );
        }
        registry_remove(symbol);
    }
}
