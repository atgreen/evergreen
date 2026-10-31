// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Default Linux runtime entry for the tagged Invoke emitter. Unsupported
//! bodies stay interpreted while their exact maps are completed. Own code and definitions,
//! capture into rooted snapshots, enter native cleanup for selected throws, and
//! retain bytecode *unwinding* as the fallback.
#![allow(dead_code)]

use super::super::{CONTROL_COUNTER, lexical_identity};
use super::native_segment_cache::SegmentCacheEntry;
use super::*;
use egcl_compiler::control_scope::{Ownership, ScopeKind};
use egcl_compiler::native_unwind::{NativeUnwindStep, SelectedTarget, next_unwind_step};
use egcl_compiler::t2::emit::{RecursiveActivation, RecursiveCallRequest, RecursiveTransfer};
use egcl_compiler::t2::native_transfer::{
    SysvNativeLanding, SysvTransferCapture, emit_capture_stub, emit_native_landing_stub,
    emit_published_helper_veneer,
};
use egcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable, TransferSiteError};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome};
use std::cell::Cell;
use std::sync::OnceLock;

mod activation;
mod condition_osr;
pub(super) mod interpreter_osr;
pub(super) mod local_exits;
pub(super) mod root_publication;
mod special_bindings;
use activation::ActivationState;
#[cfg(test)]
pub(super) use activation::fail_next_guard_reservation_for_test;
#[cfg(test)]
pub(super) use activation::take_fresh_prepares_for_test;
use condition_osr::{relocate_condition_clusters, validate_condition_clusters};
use special_bindings::{SavedSpecial, special_request, validate_special_bindings};
mod deopt;
#[cfg(test)]
pub(super) use deopt::with_ordinary_guard_payload_observer_for_test;
#[cfg(test)]
pub(super) use deopt::with_ordinary_guard_resume_observer_for_test;
pub(super) mod funcall_fast;
mod lean_root;
mod native_frames;
#[cfg(test)]
pub(super) use lean_root::take_lean_root_entries_for_test;
mod callable_identity_cache;
pub(super) mod ordinary_calls;
pub(super) mod ordinary_closure_context;
pub(super) mod ordinary_primitives;
use callable_identity_cache::CallableIdentityCache;

// Two-owner GC fixtures deliberately retain ordinary CALL frames. All other
// tests use production tail selection; the override follows fiber execution.
#[cfg(test)]
static ORDINARY_TAIL_EMISSION: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(true)) };

#[cfg(test)]
pub(super) fn with_ordinary_non_tail_emission_for_test<R>(body: impl FnOnce() -> R) -> R {
    ORDINARY_TAIL_EMISSION.with(|enabled| {
        struct Restore<'a>(&'a Cell<bool>, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(enabled, enabled.replace(false));
        body()
    })
}
mod background_compilation;
pub(super) mod native_poll;
use background_compilation::EmittedTransfer;
pub(super) use background_compilation::{
    CompiledT1, EmissionContext, PreparedT1, compile_background_t1, install_background_t1,
    prepare_background_t1,
};
mod mapped_generated;
#[cfg(test)]
mod mapped_generated_fiber_tests;
#[cfg(test)]
mod mapped_generated_tests;
pub(super) mod mapped_ordinary;
#[cfg(test)]
mod mapped_ordinary_parent_tests;
#[cfg(test)]
mod mapped_ordinary_tests;
mod ordinary_t2;
pub(super) use ordinary_t2::{PreparedT2, install_ordinary_t2, prepare_ordinary_t2};
mod nested;
#[cfg(test)]
pub(super) use nested::pending_argument_count;
use nested::{SegmentActivations, finish_nested, prepare_nested, resume_nested};
pub(super) use nested::{
    finish_nested as finish_callable, prepare_callable, resume_nested as resume_callable,
    take_call_arguments,
};
#[cfg(test)]
mod cache_tests;
use deopt::{prepare_completed_deopt, resume_guard};

// SAFETY: code owners and their non-Send feedback belong only to this Lisp
// execution. The slot follows its fiber across carriers and is retired only
// after that execution stops; no Rc is published to another execution.
static SEGMENT_CACHE: egcl_rt::execution_local::ExecutionLocal<
    RefCell<std::collections::HashMap<usize, SegmentCacheEntry<TransferCode>>>,
> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(std::collections::HashMap::new()))
};

/// Inspect the separately retained segment entry without compiling or executing
/// anything. An installed legacy NativeCode does not describe this mapping.
#[cfg(not(egcl_no_disassembly))]
pub(super) fn cached_listing(body: &Arc<BytecodeFunction>) -> Option<String> {
    let key = Arc::as_ptr(body) as usize;
    let code = SEGMENT_CACHE.with(|cache| cache.borrow().get(&key)?.code.clone())?;
    Some(format_listing(
        &code,
        "; Native segment ABI — cached entry.\n",
    ))
}

/// Decode the actual installed owner, including its precise transfer veneers.
#[cfg(not(egcl_no_disassembly))]
pub(super) fn installed_listing(code: &TransferCode) -> String {
    let header = if code.ordinary_entry.is_some() {
        "; Ordinary x86-64 Lisp ABI — installed native code.\n"
    } else {
        "; Mapped native transfer ABI — installed native code.\n"
    };
    format_listing(code, header)
}

#[cfg(not(egcl_no_disassembly))]
fn format_listing(code: &TransferCode, header: &str) -> String {
    use iced_x86::Formatter;
    use std::fmt::Write;

    let mut out = String::from(header);
    let _ = writeln!(out, "; {} bytes of x86-64:", code.code_len);
    let base = code.code.as_ptr() as u64;
    // SAFETY: the cloned owner retains this immutable executable mapping.
    let bytes = unsafe { std::slice::from_raw_parts(code.code.as_ptr(), code.code_len) };
    let decoder = iced_x86::Decoder::with_ip(64, bytes, base, iced_x86::DecoderOptions::NONE);
    let mut formatter = iced_x86::NasmFormatter::new();
    let mut line = String::new();
    for instruction in decoder {
        line.clear();
        formatter.format(&instruction, &mut line);
        let _ = write!(out, "  +{:04x}:  {line}", instruction.ip() - base);
        if let Some(immediate) = native_immediate(&instruction) {
            for (target, name) in [
                (code._veneer.as_ptr(), "native call veneer"),
                (code._poll.as_ptr(), "native poll veneer"),
                (
                    code._completion.as_ptr(),
                    "native cleanup completion veneer",
                ),
                (
                    code._recursive_prepare
                        .as_ref()
                        .map_or(std::ptr::null(), |code| code.as_ptr()),
                    "native recursive entry veneer",
                ),
                (
                    finish_recursive as *const u8,
                    "native recursive frame retirement",
                ),
                (
                    code._deopt_veneer
                        .as_ref()
                        .map_or(std::ptr::null(), |code| code.as_ptr()),
                    "native precise deopt veneer",
                ),
            ] {
                if !target.is_null() && immediate == target as u64 {
                    let _ = write!(out, "    ; {name}");
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
static SEGMENT_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// #420 admitted recursive shapes that branch between a call and arithmetic onto
// the ordinary ABI. The mapped managed-activation path they used to take still
// exists, and still serves every shape the ordinary emitter refuses, so its
// protocol fixtures need a way to reach it deliberately rather than losing
// their coverage to the wider admission (bliss-lzdkm).
//
// Compilation can run off-thread under `with_gc_stable_mutator`, so this is a
// process-global flag rather than a thread-local. Hold it only through
// `MappedOnlyForTest`, and only in tests that already serialize on
// `heap_test_lock()`.
#[cfg(test)]
static MAPPED_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(in crate::cli::bytecode) struct MappedOnlyForTest;

#[cfg(test)]
impl MappedOnlyForTest {
    pub(in crate::cli::bytecode) fn new() -> Self {
        MAPPED_ONLY.store(true, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for MappedOnlyForTest {
    fn drop(&mut self) {
        MAPPED_ONLY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
fn ordinary_admission_suppressed() -> bool {
    MAPPED_ONLY.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(test))]
fn ordinary_admission_suppressed() -> bool {
    false
}

// Capability tests deliberately exercise the full captured-entry protocol,
// independently of the production cost policy. Serialize with heap_test_lock.
#[cfg(test)]
static FORCE_CAPTURED_ENTRY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(super) fn with_forced_captured_entry_for_test<R>(body: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCE_CAPTURED_ENTRY.store(self.0, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _restore = Restore(FORCE_CAPTURED_ENTRY.swap(true, std::sync::atomic::Ordering::SeqCst));
    body()
}

#[cfg(test)]
static RECURSIVE_ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };

#[cfg(test)]
pub(super) fn take_recursive_entries() -> usize {
    RECURSIVE_ENTRIES.with(|count| count.replace(0))
}

// Keep helper traffic independent from the logical native-entry oracle: the
// latter must continue to count generated entries after these helpers retire.
#[cfg(test)]
static RECURSIVE_HELPER_CALLS: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };

#[cfg(test)]
pub(super) fn take_recursive_helper_calls() -> (usize, usize) {
    RECURSIVE_HELPER_CALLS.with(|count| count.replace((0, 0)))
}

fn try_clone_string(value: &str) -> Result<String, EgclError> {
    let mut copy = String::new();
    copy.try_reserve(value.len()).map_err(|_| EgclError::Oom)?;
    copy.push_str(value);
    Ok(copy)
}

fn try_control_token(prefix: &str) -> Result<String, EgclError> {
    use std::fmt::Write;
    let id = CONTROL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut token = String::new();
    token
        .try_reserve(prefix.len().saturating_add(20))
        .map_err(|_| EgclError::Oom)?;
    write!(&mut token, "{prefix}:{id}").expect("writing to a String cannot fail");
    Ok(token)
}

fn try_clone_control_stack(stack: &[(String, String)]) -> Result<Vec<(String, String)>, EgclError> {
    let mut copy = Vec::new();
    copy.try_reserve(stack.len()).map_err(|_| EgclError::Oom)?;
    for (name, token) in stack {
        copy.push((try_clone_string(name)?, try_clone_string(token)?));
    }
    Ok(copy)
}

struct RestartFunctionTemplates(Vec<Vec<Rc<RefCell<egcl_rt::bytecode::BytecodeFunction>>>>);

impl egcl_rt::gc::TraceHostRoots for RestartFunctionTemplates {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for templates in &self.0 {
            for function in templates {
                super::super::visit_bytecode_function_roots(&mut function.borrow_mut(), visit);
            }
        }
    }
}

fn native_body_roots() -> &'static SharedCell<Vec<std::sync::Weak<ActiveBytecodeRoot>>> {
    static ROOTS: OnceLock<SharedCell<Vec<std::sync::Weak<ActiveBytecodeRoot>>>> = OnceLock::new();
    ROOTS.get_or_init(|| SharedCell::new(Vec::new()))
}

fn scan_native_body_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    native_body_roots().borrow_mut().retain(|weak| {
        let Some(root) = weak.upgrade() else {
            return false;
        };
        // The collector stops mutators before rewriting the owning pool.
        unsafe {
            trace_bytecode_function(Arc::as_ptr(&root._function) as *mut BytecodeFunction, visit);
        }
        true
    });
}

fn retain_native_body(body: &Arc<BytecodeFunction>) -> Arc<ActiveBytecodeRoot> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_native_body_roots));
    let root = Arc::new(ActiveBytecodeRoot::new(body));
    // The root token, not the body, is weakly indexed: dropping the native
    // owner must stop rooting even if a private dispatch cache retains an Arc.
    native_body_roots().borrow_mut().push(Arc::downgrade(&root));
    root
}

#[derive(Clone, Copy)]
enum CompilationTier {
    T1,
    T2,
}

/// Only constructible through the host-specific emitter. Retaining this value
/// retains every embedded code address and the original bytecode definition.
pub(super) struct TransferCode {
    installed_symbol: Option<u32>,
    is_t2: bool,
    body: Arc<BytecodeFunction>,
    _body_roots: Arc<ActiveBytecodeRoot>,
    code: JitBuffer,
    code_info: &'static CodeInfo,
    // Retired definitions retain their exact value recipes as long as code lives.
    _native_calls: Arc<egcl_compiler::t2::x64_calls::NativeCallSites>,
    code_len: usize,
    frame_entry: Option<usize>,
    pub(super) ordinary_entry: Option<usize>,
    ordinary_stack_required: Option<usize>,
    /// A `&key` entry's body entry past binding; see `run_activation`.
    ordinary_bound_entry: Option<usize>,
    /// The ordinary entry checks declared parameter types itself.
    pub(super) ordinary_checks_params: bool,
    _ordinary_keywords: Option<JitBuffer>,
    _ordinary_arity: Option<JitBuffer>,
    _ordinary_cold: Option<JitBuffer>,
    _primitive_cells: Vec<Arc<egcl_rt::call_table::CallCell>>,
    pub(super) has_deopt: bool,
    bcp_offsets: Vec<u32>,
    osr_entries: HashMap<u32, usize>,
    /// Per OSR header, the locals an entry unboxes into raw double homes
    /// (bliss-vwpaz). Each must hold a double-float when the entry is taken.
    osr_raw_doubles: HashMap<u32, Vec<u16>>,
    // OSR imports use managed activation homes, independently of full-call ABI.
    osr_companion: Option<Box<TransferCode>>,
    osr_polls: HashMap<usize, u32>,
    _veneer: JitBuffer,
    _call_entries: Vec<(u32, Arc<egcl_rt::call_table::CallCell>, JitBuffer)>,
    _capture: JitBuffer,
    _poll: JitBuffer,
    _completion: JitBuffer,
    _recursive_prepare: Option<JitBuffer>,
    _deopt_capture: Option<JitBuffer>,
    _deopt_veneer: Option<JitBuffer>,
    deopt_metadata: Option<Arc<T2InstalledMetadata>>,
    deopts: Cell<u32>,
    recompile: Cell<bool>,
    private_probe_revision: Arc<std::sync::atomic::AtomicU64>,
    private_probe_published: Cell<bool>,
    recursive_symbol: Option<u32>,
    recursive_generation: u64,
    pub(super) native_entry_kind_slot: Option<u32>,
    native_control_slot: Option<u32>,
    native_argument_base: Option<u32>,
    pub(super) native_callable_slot: Option<u32>,
    native_argument_count: u16,
    landing: JitBuffer,
    sites: SysvTransferTable,
    slots: u16,
    cleanup_depths: std::collections::HashMap<u32, usize>,
    control_scopes: egcl_compiler::control_scope::ScopeMap,
    guard_handler_capacity: usize,
    /// `BindSpecial` instructions in `body` (sizes each activation's specials).
    special_binds: usize,
    /// `PushCatch` instructions in `body` (sizes each activation's catches).
    catch_pushes: usize,
    /// Buffers of finished activations, reused by later calls of this code so
    /// a call does not rebuild them. Only quiescent states with no Lisp value
    /// left in them are kept; see `ActivationState::recycle`. Each state is
    /// boxed so taking one out and returning it moves a pointer, not the
    /// whole struct (bliss-nkzx6).
    #[allow(clippy::vec_box)]
    state_pool: std::cell::RefCell<Vec<Box<ActivationState>>>,
    /// Cache state has no strong code edge. Dirty weak tokens cover current,
    /// retired and externally retained code without a registry ownership cycle.
    callable_cache: Rc<CallableIdentityCache>,
    /// Lock-free symbol snapshots of `body.names`, built on the first captured
    /// variable access through the ordinary lexical leaf (bliss-gg3i1.7.3).
    leaf_names: std::cell::OnceCell<Box<[egcl_rt::symbols::ResolvedSymbolName]>>,
    #[cfg(test)]
    unavailable_catch: Option<u32>,
    #[cfg(test)]
    unavailable_handler: Option<(u32, u32)>,
}

fn cache_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EGCL_NATIVE_TRANSFER").as_deref() != Some("0".as_ref()))
}
/// Install a baseline representation with its actual transfer ABI. Legacy
/// direct-call sites reject its version and enter through run_native instead.
#[cfg(test)]
pub(super) fn install_baseline(symbol: u32, body: Arc<BytecodeFunction>) -> Option<Rc<NativeCode>> {
    if !native_transfer::is_supported() || !fixed_tagged_parameters(&body) {
        return None;
    }
    install_baseline_code(
        symbol,
        TransferCode::compile_variant(body, false, false, Some(symbol))?,
    )
}

/// Ordinary T1 keeps speculative arithmetic and direct mapped recursion when
/// their existing admission checks succeed. Compilation stays synchronous and
/// publication remains T1; ordinary worker optimization still belongs to T2.
pub(super) fn install_t1(
    symbol: u32,
    body: Arc<BytecodeFunction>,
    allow_speculation: bool,
) -> Option<Rc<NativeCode>> {
    if !native_transfer::is_supported() || !activation_bound_parameters(&body) {
        return None;
    }
    let code =
        TransferCode::compile_variant(Arc::clone(&body), allow_speculation, true, Some(symbol))
            .or_else(|| TransferCode::compile_variant(body, false, true, Some(symbol)))?;
    install_baseline_code(symbol, code)
}

/// The step at which the mapped emitter refused a body (bliss-6tf0e). Each
/// variant is one place `compile_variant` or `emit_prepared_with_ordinary`
/// used to return `None` without saying why; the decline logs print it.
#[derive(Debug)]
pub(super) enum EmitDecline {
    CapturedEnvironment,
    ExecutingIdentity,
    /// The body's name does not read back as a symbol, so the optimizing
    /// frontend has no identity for self calls and deopt frames.
    UnresolvedName,
    Build(egcl_compiler::t2::build::BuildError),
    Scopes(egcl_compiler::control_scope::ScopeError),
    Legalize(egcl_compiler::t2::build::BuildError),
    ExecutableMemory,
    FrameTooLarge,
    /// Unboxed values need a T2 native-stack frame without OSR entries.
    RawValues,
    OsrCompanion(Box<EmitDecline>),
    Emitter(egcl_compiler::t2::emit::EmitError),
    /// A transfer site crosses a scope recovery cannot reconstruct.
    TransferScope(ScopeKind, Ownership),
    CleanupScope(u32),
    FrameEntry,
    DebugInfo,
    StackMap,
    NativeCallSites,
}

impl std::fmt::Display for EmitDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapturedEnvironment => f.write_str("captured variables (closure environment)"),
            Self::ExecutingIdentity => f.write_str("a scope needs an executing identity"),
            Self::UnresolvedName => {
                f.write_str("the function's name is not a symbol the optimizer can identify")
            }
            Self::Build(error) => write!(f, "IR builder: {error:?}"),
            Self::Scopes(error) => write!(f, "control-scope analysis: {error:?}"),
            Self::Legalize(error) => write!(f, "transfer-call legalization: {error:?}"),
            Self::ExecutableMemory => f.write_str("no executable memory for a veneer"),
            Self::FrameTooLarge => f.write_str("frame slot count overflows"),
            Self::RawValues => f.write_str("unboxed values outside a T2 native-stack frame"),
            Self::OsrCompanion(decline) => write!(f, "OSR companion: {decline}"),
            Self::Emitter(error) => {
                write!(f, "mapped emitter: {error:?}{}", describe_emit_error(error))
            }
            Self::TransferScope(kind, ownership) => write!(
                f,
                "a transfer site crosses an unrecoverable {ownership:?} {kind:?} scope"
            ),
            Self::CleanupScope(bcp) => write!(f, "no scope map at the cleanup at bcp {bcp}"),
            Self::FrameEntry => f.write_str("frame entry lies outside the emitted code"),
            Self::DebugInfo => f.write_str("debug info could not be installed"),
            Self::StackMap => f.write_str("stack map could not be installed"),
            Self::NativeCallSites => f.write_str("emitter produced no native call-site table"),
        }
    }
}

/// Why `install_t1` declined `body`, for the decline log only (bliss-s5r0c).
/// It repeats `install_t1`'s checks in order, rebuilds the IR to name the first
/// instruction the builder refuses, and otherwise replays the final compile
/// attempt to name the step that refused it (bliss-6tf0e). Call it only when
/// the reason will be printed: the rebuild and replay are not free.
pub(super) fn mapped_t1_decline_reason(symbol: u32, body: &Arc<BytecodeFunction>) -> String {
    let kind = |op: &Instr| {
        let full = format!("{op:?}");
        full.find(['(', ' ', '{'])
            .map_or(full.clone(), |i| full[..i].to_string())
    };
    if !native_transfer::is_supported() {
        return "mapped transfer unsupported on this platform".into();
    }
    if let Some(reason) = captured_environment_decline(body) {
        return format!("captured variables (closure environment): {reason}");
    }
    if !activation_bound_parameters(body) {
        return if body.variadic {
            "a &rest, &key or &aux parameter is special or out of slot order".into()
        } else {
            "a parameter is special or boxed, not a plain slot".into()
        };
    }
    if let Some(op) = body.code.iter().find(|op| {
        matches!(
            op,
            Instr::PushBlock { register: true, .. } | Instr::NamedTag { .. }
        )
    }) {
        return format!("{} needs an executing identity", kind(op));
    }
    if let Err(error) = egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(body) {
        return format!("IR builder: {error:?}");
    }
    match TransferCode::try_compile_variant(Arc::clone(body), false, true, Some(symbol)) {
        Err(decline) => decline.to_string(),
        Ok(_) => "native emission declined, but compiled on replay".into(),
    }
}

pub(super) fn install_baseline_code(symbol: u32, mut code: TransferCode) -> Option<Rc<NativeCode>> {
    code.set_installed_symbol(symbol);
    let body = Arc::clone(&code.body);
    let code = Rc::new(code);
    let code_info = code.code_info;
    let bcp_offsets = code.bcp_offsets.clone();
    let has_deopt = code.has_deopt;
    let metadata = code.deopt_metadata.clone();
    let entry = code.code.as_ptr();
    let bytes = unsafe { std::slice::from_raw_parts(entry, code.code_len) };
    maybe_write_perf_map("T1", entry as usize, code.code_len, symbol);
    maybe_write_jitdump_code_load("T1", entry as usize, bytes, symbol);
    let native = Rc::new(NativeCode {
        _env_names: NativeEnvNames::new(&body),
        _call_cells: Vec::new(),
        entry: code.code.as_ptr(),
        code_len: code.code_len,
        num_slots: code.slots,
        _storage: NativeCodeStorage::Mapped(code),
        body: Some(body),
        transfer_abi_version: MAPPED_TRANSFER_ABI_VERSION,
        transfer_abi_arch: NATIVE_TRANSFER_ARCH,
        _direct_calls: Vec::new(),
        is_t2: false,
        compiled_entry: 0,
        code_id: 0,
        osr_entries: HashMap::new(),
        bcp_offsets,
        code_info,
        has_deopt,
        t2_metadata: metadata,
    });
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&native)));
    Some(native)
}

/// Replace a failed installed version only while both its publication and its
/// definition are still current. Suspended older entries retain their code.
pub(super) fn refresh_installed(expected: &NativeCode) -> Option<Rc<NativeCode>> {
    let NativeCodeStorage::Mapped(code) = &expected._storage else {
        return None;
    };
    if !code.needs_recompile() {
        return None;
    }
    let symbol = code.installed_symbol?;
    let body = Arc::clone(expected.body.as_ref()?);
    // A closure template's code is owned by an instance's private symbol,
    // whose function cell holds no function; the registry alone names its
    // definition.
    let callable = || {
        egcl_rt::symbols::symbol_function(symbol)
            .filter(|&value| egcl_rt::function::is_interpreted_function(value))
    };
    let closure = egcl_rt::symbols::is_uninterned(symbol) && callable().is_none();
    let current = || {
        registry_get(symbol).is_some_and(|current| Arc::ptr_eq(&current, &body))
            && (closure || recursive_definition_matches(symbol, Arc::as_ptr(&body)))
            && NATIVE_REGISTRY.with(|registry| {
                registry
                    .borrow()
                    .get(&symbol)
                    .is_some_and(|native| std::ptr::eq(native.as_ref(), expected))
            })
    };
    if !current() {
        return None;
    }
    let function = callable();
    if function.is_none() && !closure {
        return None;
    }
    egcl_rt::rooted!(function = function);
    // A replacement T2 would speculate from the same profile and fail the
    // same guard; count the retirement so a repeating failure stays at T1.
    if expected.is_t2 {
        note_t2_guard_retirement(symbol, &body);
    }
    // Retire only the exact current publication. Its running activations keep
    // their owner, while subsequent calls execute T0 until replacement arrives.
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().remove(&symbol));
    bump_direct_call_gen_for(symbol);
    if let Some(function) = *function {
        egcl_rt::function::set_tier(function, 0);
    }
    // A T2 guard failure falls back to non-speculating T1; a T1 refresh may
    // use its newer profile. The broker rejects superseded compilation tickets.
    background_t1::request_with_speculation(symbol, !expected.is_t2);
    let forced = matches!(forced_tier(), Some(ForcedTier::T1 | ForcedTier::T2)) || egcl_t2_forced();
    let replacement = if forced {
        background_t1::wait(symbol)
    } else {
        background_t1::poll();
        NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())
    }?;
    // A stale caller must still interpret its original body if the definition
    // changed while preparation or cooperative waiting ran runtime services.
    if replacement
        .body
        .as_ref()
        .is_none_or(|current| !Arc::ptr_eq(current, &body))
        || callable() != *function
    {
        return None;
    }
    Some(replacement)
}

/// Try the mapped cache for an ordinary native invocation. The cache is enabled
/// by default; EGCL_NATIVE_TRANSFER=0 disables only this diagnostic cache path,
/// never restores checked installation. Published mapped entries remain active.
/// The cache owns each compiled body through `TransferCode::body`.
pub(super) fn try_run(
    body: Arc<BytecodeFunction>,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<Result<EgclVal, EgclError>> {
    // Cached: this runs on EVERY ordinary native invocation, and an uncached
    // read here cost a getenv plus an OsString allocation per call -- 2.7% of a
    // SHA-256 profile, for a process-wide switch that cannot change. The same
    // OnceLock idiom as nn_direct_enabled and profiling_disabled.
    if !cache_enabled() {
        return None;
    }
    try_run_enabled(body, args, env)
}

/// Entry after the process-wide cache gate. Unit tests use this seam rather
/// than mutating an environment switch whose value is cached for the process.
pub(super) fn try_run_enabled(
    body: Arc<BytecodeFunction>,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<Result<EgclVal, EgclError>> {
    if !native_transfer::is_supported() {
        return None;
    }

    // Bootstrap exercises thousands of ordinary bytecode calls while the
    // runtime is still constructing its Lisp world.  A process-wide opt-in
    // must not enter the segment ABI during that phase: the bootstrap has not
    // yet established the final evaluator registries and can run code that
    // the segment cache cannot safely retain.  Direct unit tests call this
    // helper after constructing their own test world, so keep that path
    // available under cfg(test).
    if !cfg!(test) && !super::super::BOOT_COMPLETE.with(|ready| ready.get()) {
        return None;
    }
    // Eligible direct self calls stay within the existing segment and own
    // separate precise frames. Calls reentering through a Rust helper still
    // use the bounded legacy bridge; they cannot jump across that Rust frame
    // into the enclosing segment's exceptional landing.
    if !native_transfer::current_segment().is_null() {
        return None;
    }
    let code = cached_code(&body);
    code.map(|code| {
        #[cfg(test)]
        SEGMENT_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        code.run(args, env)
    })
}

fn cached_code(body: &Arc<BytecodeFunction>) -> Option<Rc<TransferCode>> {
    let key = Arc::as_ptr(body) as usize;
    SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entry = cache.entry(key).or_insert_with(|| {
            SegmentCacheEntry::new(body, TransferCode::compile(Arc::clone(body)).map(Rc::new))
        });
        if entry.code.as_ref().is_some_and(|code| code.recompile.get()) {
            entry.code = TransferCode::compile(Arc::clone(body)).map(Rc::new);
        }
        entry.code.clone()
    })
}

#[cfg(test)]
pub(super) fn take_segment_run_count() -> u64 {
    SEGMENT_RUNS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// These mappings outlive every target and caller. Child retirement can drop
/// the last target owner without releasing its own return/continuation PC.
/// Publish the caller suspended beneath a mapped-call adapter frame. Exposed
/// for the sibling callable adapter, whose checked callback also runs Lisp.
///
/// # Safety
/// Same contract as `root_publication::published_mapped`.
pub(in crate::cli::bytecode) unsafe fn published_mapped_record<R>(
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
    body: impl FnOnce() -> R,
) -> R {
    unsafe { root_publication::published_mapped(record, body) }
}

/// The checked compatibility call runs Lisp through the runtime call table,
/// which cannot reach this crate's publication; wrap it here instead.
unsafe extern "C" fn checked_call_published(
    target: u64,
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
) {
    #[cfg(test)]
    PUBLISHED_CHECKED_CALLS.with(|count| count.set(count.get() + 1));
    unsafe {
        root_publication::published_mapped(record, || {
            let direct = call_lean_target(target, record)
                || (call_table::is_builtin_funcall(target) && funcall_fast::call(record));
            if !direct {
                call_table::checked_call(target, record);
            }
            if (*record).outcome.exit == NativeExit::Returned {
                let env = NATIVE_ENV.with(Cell::get);
                (*record).return_values = (*env).native_return_values();
            }
        })
    }
}

/// A mapped caller's named call whose warm target is installed ordinary T2
/// code enters that code's lean root directly, as the checked call would after
/// `warm` and `run_native_callable` selected it (GitHub #487). Returns `false`,
/// having done nothing, for every other target.
///
/// # Safety
/// As for `checked_call_published`, whose publication must be live.
unsafe fn call_lean_target(
    cell: u64,
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
) -> bool {
    let invocation = unsafe { &*(*record).context };
    let Some((code, function, symbol, offset)) =
        (unsafe { call_table::lean_native_target(cell, invocation.nargs) })
    else {
        return false;
    };
    let env = NATIVE_ENV.with(Cell::get);
    if env.is_null() {
        return false;
    }
    #[cfg(test)]
    LEAN_TARGET_CALLS.with(|count| count.set(count.get() + 1));
    // The caller's published, scanned activation owns these argument words.
    let args = if invocation.nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(invocation.args, invocation.nargs) }
    };
    let result = guard_c2i(|| {
        let instance = if code.native_callable_slot.is_some()
            && egcl_rt::function::is_interpreted_function(function)
        {
            egcl_rt::function::definition_index(function)
        } else {
            symbol
        };
        egcl_rt::rooted!(_active_callable_root = super::ActiveCallableRoot(instance));
        if let Some(result) =
            unsafe { mapped_ordinary::call(&code, offset, function, args, &mut *env, cell) }
        {
            result
        } else {
            code.run_ordinary_root(offset, function, args, unsafe { &mut *env })
        }
    });
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
thread_local! {
    static LEAN_TARGET_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// Mapped named calls that entered an ordinary callee's lean root directly.
#[cfg(test)]
pub(super) fn take_lean_target_calls_for_test() -> usize {
    LEAN_TARGET_CALLS.with(|count| count.replace(0))
}

#[cfg(test)]
thread_local! {
    static PUBLISHED_CHECKED_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// Mapped named calls that left generated code for `checked_call_published`.
#[cfg(test)]
pub(super) fn take_published_checked_calls_for_test() -> usize {
    PUBLISHED_CHECKED_CALLS.with(|count| count.replace(0))
}

static PUBLISHED_ENTRIES: OnceLock<Option<[root_publication::PublishedAdapter; 2]>> =
    OnceLock::new();

fn mapped_adapter(pc: usize) -> Option<&'static root_publication::PublishedAdapter> {
    PUBLISHED_ENTRIES
        .get()?
        .as_ref()?
        .iter()
        .find(|entry| entry.contains_return(pc))
}

fn unbound_published_entries() -> Option<[usize; 2]> {
    PUBLISHED_ENTRIES
        .get_or_init(|| {
            let entry = |slice| {
                root_publication::PublishedAdapter::new(
                    egcl_compiler::t2::native_transfer::emit_generated_published_call_entry_descriptor(
                        slice,
                        prepare_nested,
                        finish_nested,
                        resume_nested,
                        checked_call_published,
                        Some(mapped_generated::helpers()),
                    ),
                )
            };
            Some([entry(false)?, entry(true)?])
        })
        .as_ref()
        .map(|entries| std::array::from_fn(|i| entries[i].code.as_ptr() as usize))
}

// These two tail shims belong to one execution and embed only the stable
// execution-local capture-cell address. They introduce no return PC/frame.
static GENERATED_ENTRY_BINDINGS: egcl_rt::execution_local::ExecutionLocal<
    RefCell<Option<[JitBuffer; 2]>>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(None)) };

pub(super) fn published_entries() -> Option<[usize; 2]> {
    let entries = unbound_published_entries()?;
    GENERATED_ENTRY_BINDINGS.with(|slot| {
        if slot.borrow().is_none() {
            let capture = CAPTURE.with(Cell::as_ptr) as usize;
            let mut buffers = Vec::new();
            for entry in entries {
                let mut bytes = vec![0xf3, 0x0f, 0x1e, 0xfa, 0x49, 0xbb]; // mov r11,capture
                bytes.extend_from_slice(&(capture as u64).to_le_bytes());
                bytes.extend_from_slice(&[0x48, 0xb8]);
                bytes.extend_from_slice(&(entry as u64).to_le_bytes());
                bytes.extend_from_slice(&[0xff, 0xe0]);
                buffers.push(JitBuffer::new(&bytes)?);
            }
            *slot.borrow_mut() = Some(buffers.try_into().ok()?);
        }
        Some(
            slot.borrow()
                .as_ref()
                .map(|buffers| std::array::from_fn(|i| buffers[i].as_ptr() as usize))
                .unwrap(),
        )
    })
}

#[cfg(test)]
pub(super) fn fail_next_nested_capture() {
    nested::fail_next_capture();
}

#[cfg(test)]
pub(super) fn take_nested_entries() -> usize {
    nested::take_nested_entries()
}

#[cfg(test)]
pub(super) fn current_guard_callable_for_test() -> Option<EgclVal> {
    let boundary = unsafe { root_publication::ACTIVE.with(Cell::get).as_ref() }?;
    root_publication::frame_callable(boundary)
}

#[cfg(test)]
pub(super) fn take_ordinary_lexical_leaf_hits_for_test() -> usize {
    ordinary_closure_context::HITS.with(|hits| hits.replace(0))
}

impl TransferCode {
    pub(super) fn needs_callable_identity(&self) -> bool {
        self.ordinary_entry.is_some() && (self.has_deopt || !self.body.self_call_sites.is_empty())
    }

    /// Arguments the ordinary entry's body receives: a `&key` body's bound
    /// slots, otherwise its fixed parameters.
    pub(super) fn ordinary_parameter_count(&self) -> usize {
        if self.ordinary_bound_entry.is_some() {
            self.body.param_layout.len()
        } else {
            usize::from(self.body.arity)
        }
    }

    pub(super) fn mapped_entry(&self) -> Option<*const u8> {
        let offset = self.frame_entry.filter(|offset| *offset < self.code_len)?;
        Some(self.code.as_ptr().wrapping_add(offset))
    }

    /// Code bounds for an in-flight ordinary segment. Unlike a published
    /// descriptor, this does not authorize arbitrary named callers to enter
    /// the register ABI: it is used only to retain the already validated root
    /// callable while its generated body is executing.
    pub(super) fn ordinary_code_bounds(&self) -> Option<(usize, usize)> {
        self.ordinary_entry
            .filter(|offset| *offset < self.code_len)?;
        Some((self.code.as_ptr() as usize, self.code_len))
    }

    /// `symbols::find_index` of `body.names[index]`, from a lock-free
    /// snapshot that a rename or image restore refreshes. A name with no
    /// symbol looks again on every use, exactly as `find_index` does.
    pub(super) fn leaf_symbol(&self, index: usize) -> Option<u32> {
        self.leaf_names
            .get_or_init(|| {
                self.body
                    .names
                    .iter()
                    .map(|name| egcl_rt::symbols::ResolvedSymbolName::new(name))
                    .collect()
            })
            .get(index)
            .and_then(egcl_rt::symbols::ResolvedSymbolName::index)
    }

    /// Shared code is safe for distinct closures only when every native frame
    /// carries its own precise callable word and this object owns that body.
    pub(super) fn accepts_explicit_callable(&self, function: EgclVal) -> bool {
        self.native_callable_slot.is_some()
            && egcl_rt::function::is_interpreted_function(function)
            && (!self.body.has_env || closure_captured_env(function).is_some())
            && registry_get(egcl_rt::function::definition_index(function))
                .is_some_and(|body| Arc::ptr_eq(&body, &self.body))
    }

    pub(super) fn ordinary_descriptor(
        &self,
        revision: u64,
    ) -> Option<egcl_rt::call_table::OrdinaryEntryDescriptor> {
        use egcl_rt::call_table::{
            ORDINARY_ENV_CLOSURE, ORDINARY_ENV_NONE, ORDINARY_X64_LISP_KIND,
            ORDINARY_X64_LISP_VERSION, OrdinaryEntryDescriptor,
        };
        // Root ingress validates declared parameters before entering generated
        // code, and source-proven self calls preserve those established facts.
        // A named ordinary caller can enter a typed body only when its entry
        // checks the declarations itself; otherwise do not publish this entry
        // as a general descriptor, and the call takes the checked dispatcher.
        if !self.ordinary_checks_params && self.body.param_types.iter().any(|ty| !ty.is_any()) {
            return None;
        }
        let offset = self
            .ordinary_entry
            .filter(|offset| *offset < self.code_len)?;
        Some(OrdinaryEntryDescriptor {
            kind: ORDINARY_X64_LISP_KIND,
            version: ORDINARY_X64_LISP_VERSION,
            revision,
            entry: (self.code.as_ptr() as usize).checked_add(offset)?,
            code_start: self.code.as_ptr() as usize,
            code_len: self.code_len,
            // An `&optional` entry checks its own count range.
            arity: if self.body.min_args < self.body.arity || self.body.variadic {
                egcl_rt::call_table::ORDINARY_ARITY_ANY
            } else {
                usize::from(self.body.arity)
            },
            environment: if self.native_callable_slot.is_some() {
                ORDINARY_ENV_CLOSURE
            } else {
                ORDINARY_ENV_NONE
            },
            owner: self as *const Self as usize,
            stack_required: self.ordinary_stack_required?,
        })
    }

    #[cfg(test)]
    pub(super) fn pooled_states_for_test(&self) -> usize {
        self.state_pool.borrow().len()
    }

    /// Run the real OSR hand-off ledger move (`take_osr_state`) between two
    /// states prepared for this code, then recycle the source the way its
    /// outer activation does when it returns. Reports how many states that
    /// added to the pool.
    #[cfg(test)]
    pub(super) fn recycle_osr_source_for_test(&self, env: &mut Env) -> Result<usize, EgclError> {
        let mut source = Box::new(ActivationState::prepare(self, env)?);
        source.ready_osr = Some(Box::new(ActivationState::prepare(self, env)?));
        drop(source.take_osr_state());
        let before = self.state_pool.borrow().len();
        source.recycle(self);
        Ok(self.state_pool.borrow().len() - before)
    }

    /// Replace a zero-argument fixture entry with a noncollecting leaf/tail
    /// sequence. The caller retains the old buffer for its diagnostic metadata.
    ///
    /// # Safety
    /// The replacement preserves the ordinary ABI, cannot collect or
    /// publish a frame, and must tail-retire before any collecting child runs.
    #[cfg(test)]
    pub(super) unsafe fn replace_nongc_tail_entry_for_test(
        &mut self,
        bytes: &[u8],
    ) -> Option<JitBuffer> {
        assert_eq!(self.body.arity, 0);
        assert!(self.ordinary_entry.is_some() && self.native_callable_slot.is_none());
        let code = JitBuffer::new(bytes)?;
        self.code_len = bytes.len();
        self.ordinary_entry = Some(0);
        Some(std::mem::replace(&mut self.code, code))
    }

    #[cfg(test)]
    pub(super) fn value_maps_for_test(
        &self,
    ) -> &Arc<egcl_compiler::t2::x64_calls::NativeCallSites> {
        &self._native_calls
    }

    #[cfg(test)]
    pub(super) fn deopt_count(&self) -> u32 {
        self.deopts.get()
    }

    #[cfg(test)]
    pub(super) fn without_catch_destination(mut self, push_bcp: u32) -> Self {
        assert!(matches!(
            self.body.code.get(push_bcp as usize),
            Some(Instr::PushCatch { .. })
        ));
        self.unavailable_catch = Some(push_bcp);
        self
    }

    #[cfg(test)]
    pub(super) fn without_handler_destination(mut self, push_bcp: u32, clause_index: u32) -> Self {
        let Some(Instr::PushHandlerCase { hc, .. }) = self.body.code.get(push_bcp as usize) else {
            panic!("handler scope");
        };
        assert!((clause_index as usize) < self.body.handler_cases[*hc as usize].clauses.len());
        self.unavailable_handler = Some((push_bcp, clause_index));
        self
    }

    pub(super) fn compile(body: Arc<BytecodeFunction>) -> Option<Self> {
        // The uninstalled segment cache runs without the callable whose
        // captured environment a variadic default may read; only installed
        // entries bind variadic lambda lists or capturing environments.
        if body.variadic || body.has_env {
            return None;
        }
        Self::compile_variant(Arc::clone(&body), true, true, None)
            .or_else(|| Self::compile_variant(body, false, true, None))
    }

    #[cfg(test)]
    pub(super) fn compile_nested_protected(body: Arc<BytecodeFunction>) -> Option<Self> {
        Self::compile_variant(body, false, false, None)
    }

    // Protocol fixtures need a real managed-activation caller even when the
    // same Lisp body is admitted by the production ordinary emitter.
    #[cfg(test)]
    pub(super) fn compile_mapped_for_test(body: Arc<BytecodeFunction>) -> Option<Self> {
        let roots = retain_native_body(&body);
        let ir = egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body).ok()?;
        let call_cells = named_call_cells(&body);
        Self::emit_prepared(
            body,
            roots,
            &ir,
            None,
            &call_cells,
            false,
            CompilationTier::T1,
        )
    }

    pub(super) fn compile_nested(body: Arc<BytecodeFunction>) -> Option<Self> {
        if body.has_env
            || body.variadic
            || body.min_args != body.arity
            || body.max_args != Some(body.arity)
            || !scope_free_native_body(&body, true)
        {
            return None;
        }
        Self::compile_variant(Arc::clone(&body), true, false, None)
            .or_else(|| Self::compile_variant(body, false, false, None))
    }

    fn set_installed_symbol(&mut self, symbol: u32) {
        self.installed_symbol = Some(symbol);
        if let Some(companion) = self.osr_companion.as_mut() {
            companion.set_installed_symbol(symbol);
        }
    }

    fn osr_destination(&self) -> &Self {
        self.osr_companion.as_deref().unwrap_or(self)
    }

    /// Whether a running loop can enter this version at `header`.
    pub(super) fn has_osr_entry(&self, header: u32) -> bool {
        self.osr_destination().osr_entries.contains_key(&header)
    }

    /// Whether an activation whose local `slot` holds `local(slot)` may
    /// enter OSR header `header` of this destination: every local the entry
    /// unboxes into a raw double home holds a double-float (bliss-vwpaz). A
    /// loop whose accumulator is momentarily a fixnum keeps running where it is.
    pub(super) fn osr_imports_admit(&self, header: u32, local: impl Fn(u16) -> EgclVal) -> bool {
        self.osr_raw_doubles
            .get(&header)
            .is_none_or(|slots| slots.iter().all(|&slot| local(slot).is_double_float()))
    }

    /// Whether a guard of this version, or of the companion its loops are
    /// entered in, has failed (bliss-k51a).
    pub(super) fn has_deopted(&self) -> bool {
        self.deopts.get() > 0 || self.osr_destination().deopts.get() > 0
    }

    /// The code an active lower-tier loop enters by OSR: this code, or the
    /// mapped companion an ordinary entry retains for its loop headers.
    #[cfg(test)]
    pub(super) fn osr_target(&self) -> *const Self {
        self.osr_destination()
    }

    /// Bytecode loop headers that OSR can enter, in `osr_target`.
    #[cfg(test)]
    pub(super) fn osr_target_headers(&self) -> Vec<u32> {
        self.osr_destination().osr_entries.keys().copied().collect()
    }

    pub(super) fn private_entry_revision(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.private_probe_published.set(true);
        Arc::clone(&self.private_probe_revision)
    }

    pub(super) fn retire_private_entries(&self) {
        if self.private_probe_published.get() {
            self.private_probe_revision
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            funcall_fast::retire_native_probes();
        }
    }

    fn request_recompile(&self) {
        if !self.recompile.replace(true) {
            self.retire_private_entries();
        }
    }

    pub(super) fn needs_recompile(&self) -> bool {
        self.recompile.get()
    }

    /// `symbol` is the function cell `body` is installed in, when the caller
    /// knows it; otherwise the body's name is read back as a symbol.
    fn compile_variant(
        body: Arc<BytecodeFunction>,
        optimize: bool,
        allow_recursion: bool,
        symbol: Option<u32>,
    ) -> Option<Self> {
        Self::try_compile_variant(body, optimize, allow_recursion, symbol).ok()
    }

    /// `compile_variant`, naming the step that refused `body` (bliss-6tf0e).
    fn try_compile_variant(
        body: Arc<BytecodeFunction>,
        optimize: bool,
        allow_recursion: bool,
        symbol: Option<u32>,
    ) -> Result<Self, EmitDecline> {
        // A variadic body binds its slots, and a capturing body its heap
        // EnvFrame, in `run_activation`.
        if captured_environment_decline(&body).is_some() {
            return Err(EmitDecline::CapturedEnvironment);
        }
        // These scopes need executing identity, dynamic values or inherited
        // state. Do not invent fresh tokens during recovery for live closures.
        if body.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::PushBlock { register: true, .. } | Instr::NamedTag { .. }
            )
        }) {
            return Err(EmitDecline::ExecutingIdentity);
        }
        let roots = retain_native_body(&body);
        let optimize = optimize && scope_free_native_body(&body, false);
        // Mandatory result guards also need exact recovery metadata in the
        // baseline tier, including when VALUES produced the checked primary.
        // This does not enable speculation for multiple-value bodies.
        let result_recovery = scope_free_native_body(&body, true)
            && body
                .code
                .iter()
                .any(|op| matches!(op, Instr::CheckResult(_)));
        let (ir, ordinary_ir, deopt_metadata) = if scope_free_native_body(&body, false)
            || result_recovery
        {
            // A method's callable lives in an uninterned symbol whose name,
            // "(METHOD ...)", does not read back; the installed symbol is its
            // identity for self calls and deopt frames (bliss-6tf0e).
            let symbol = symbol
                .or_else(|| super::super::resolve_sym(&body.name).map(EgclVal::as_symbol_index))
                .ok_or(EmitDecline::UnresolvedName)?;
            let mut ir = egcl_compiler::t2::build::build_for_transfer_optimization(&body, symbol)
                .map_err(EmitDecline::Build)?;
            // A site with no profile yet gets the optimistic fixnum guess.
            // Record it so a disproved guess recompiles rather than blacklists.
            let cold_guesses = RefCell::new(Vec::new());
            let profile = |bcp| {
                use egcl_compiler::t2::speculate::SpecType as CompilerType;
                match type_profile_at(Arc::as_ptr(&body) as usize, bcp) {
                    None => {
                        let mut cold = cold_guesses.borrow_mut();
                        if !cold.contains(&bcp) {
                            cold.push(bcp);
                        }
                        Some(CompilerType::Fixnum)
                    }
                    // A single-float site speculates natively too: the mapped
                    // emitter guards and computes it (bliss-9bw59).
                    Some(profile) => match profile.speculation() {
                        Some(SpecType::Fixnum) => Some(CompilerType::Fixnum),
                        Some(SpecType::SingleFloat) => Some(CompilerType::SingleFloat),
                        // Mapped T1 keeps tagged homes only; raw doubles are T2's.
                        Some(SpecType::DoubleFloat) | None => None,
                    },
                }
            };
            if optimize {
                // A retired family is a failure constraint, not missing type
                // samples. Static declarations must not bypass it in mapped T1.
                egcl_compiler::t2::speculate::speculate_with_feedback(
                    &mut ir,
                    &profile,
                    egcl_compiler::t2::speculate::SpeculateOptions::default(),
                    &|bcp, kind| {
                        use egcl_compiler::t2::speculate::SpecType as CompilerType;
                        let kind = match kind {
                            CompilerType::Fixnum => SpecType::Fixnum,
                            CompilerType::SingleFloat => SpecType::SingleFloat,
                            CompilerType::DoubleFloat => SpecType::DoubleFloat,
                        };
                        type_profile_at(Arc::as_ptr(&body) as usize, bcp)
                            .is_none_or(|profile| profile.permits(kind))
                    },
                );
            }
            let speculations = t2_numeric_speculations(&ir, symbol);
            let mut passes = egcl_compiler::t2::pass::PassManager::new();
            passes.add(Box::new(egcl_compiler::t2::opt_fold::ConstFold));
            passes.add(Box::new(egcl_compiler::t2::opt_gvn::Gvn));
            passes.add(Box::new(egcl_compiler::t2::opt_guard::GuardElim));
            passes.add(Box::new(egcl_compiler::t2::opt_clear_mv::ClearMvElim));
            passes.add(Box::new(egcl_compiler::t2::opt_dce::Dce));
            passes.run(&mut ir);
            let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body)
                .map_err(EmitDecline::Scopes)?;
            let ordinary_ir = ir.clone();
            egcl_compiler::t2::build::legalize_transfer_calls(&mut ir, &scopes)
                .map_err(EmitDecline::Legalize)?;
            let metadata = Arc::new(T2InstalledMetadata {
                speculations,
                cold_guesses: cold_guesses.into_inner(),
                _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
                    bodies: vec![Arc::clone(&body)],
                }),
                deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
            });
            (ir, Some(ordinary_ir), Some(metadata))
        } else {
            let ir = egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body)
                .map_err(EmitDecline::Build)?;
            (ir, None, None)
        };
        let call_cells = named_call_cells(&body);
        Self::try_emit_prepared_with_ordinary(
            body,
            roots,
            &ir,
            ordinary_ir.as_ref(),
            deopt_metadata,
            &call_cells,
            allow_recursion,
            CompilationTier::T1,
        )
    }

    /// Emit the existing mapped ABI from prepared IR. The ordinary worker path
    /// supplies its optimized IR and rooted constant owners through this seam.
    fn emit_prepared(
        body: Arc<BytecodeFunction>,
        roots: Arc<ActiveBytecodeRoot>,
        ir: &egcl_compiler::t2::ir::Function,
        deopt_metadata: Option<Arc<T2InstalledMetadata>>,
        call_cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
        allow_recursion: bool,
        tier: CompilationTier,
    ) -> Option<Self> {
        Self::emit_prepared_with_ordinary(
            body,
            roots,
            ir,
            None,
            deopt_metadata,
            call_cells,
            allow_recursion,
            tier,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_prepared_with_ordinary(
        body: Arc<BytecodeFunction>,
        roots: Arc<ActiveBytecodeRoot>,
        ir: &egcl_compiler::t2::ir::Function,
        ordinary_ir: Option<&egcl_compiler::t2::ir::Function>,
        deopt_metadata: Option<Arc<T2InstalledMetadata>>,
        call_cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
        allow_recursion: bool,
        tier: CompilationTier,
    ) -> Option<Self> {
        Self::try_emit_prepared_with_ordinary(
            body,
            roots,
            ir,
            ordinary_ir,
            deopt_metadata,
            call_cells,
            allow_recursion,
            tier,
        )
        .ok()
    }

    /// `emit_prepared_with_ordinary`, naming the step that refused the body
    /// (bliss-6tf0e). The ordinary and native-frame entries are optional: their
    /// refusals fall back to the mapped emitter, so only its refusal and the
    /// mandatory steps around it are reasons.
    #[allow(clippy::too_many_arguments)]
    fn try_emit_prepared_with_ordinary(
        body: Arc<BytecodeFunction>,
        roots: Arc<ActiveBytecodeRoot>,
        ir: &egcl_compiler::t2::ir::Function,
        ordinary_ir: Option<&egcl_compiler::t2::ir::Function>,
        deopt_metadata: Option<Arc<T2InstalledMetadata>>,
        call_cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
        allow_recursion: bool,
        tier: CompilationTier,
    ) -> Result<Self, EmitDecline> {
        let context = EmissionContext::capture(&body, allow_recursion);
        let artifact = EmittedTransfer::try_emit_prepared_with_ordinary(
            &body,
            ir,
            ordinary_ir,
            deopt_metadata.is_some(),
            call_cells,
            &context,
            tier,
        )?;
        artifact.install(body, roots, deopt_metadata)
    }

    fn queue_private_callable_cache(&self) {
        if self
            .installed_symbol
            .is_some_and(egcl_rt::symbols::is_uninterned)
        {
            CallableIdentityCache::enqueue(&self.callable_cache);
        }
    }

    /// A root for `function` as this code's callable identity, reusing the one
    /// from the previous activation when it names the same function.
    pub(super) fn callable_root(
        &self,
        function: EgclVal,
    ) -> Arc<egcl_rt::CrossThreadRoot<EgclVal>> {
        egcl_rt::rooted!(function = function);
        if let Some(root) = self.cached_callable_root(&function) {
            return root;
        }
        let root = Arc::new(egcl_rt::CrossThreadRoot::new(*function));
        self.queue_private_callable_cache();
        let old = self
            .callable_cache
            .last_callable
            .borrow_mut()
            .replace(Arc::clone(&root));
        drop(old);
        // Recorded with the root, never separately: the bits name the object
        // only while this root keeps it alive.
        self.callable_cache.last_callable_pinned.set(
            if egcl_rt::function::is_interpreted_function(*function) {
                function.0
            } else {
                0
            },
        );
        root
    }

    /// The previous activation's callable root, when it still names the
    /// function in `function`, which the caller keeps rooted: a GC-gated read
    /// may park, and the value is reread after it.
    pub(super) fn cached_callable_root(
        &self,
        function: &EgclVal,
    ) -> Option<Arc<egcl_rt::CrossThreadRoot<EgclVal>>> {
        // Clone out of the cell before the GC-gated read, which may park.
        let previous = self.callable_cache.last_callable.borrow().clone()?;
        // An interpreted function object never moves, and `previous` holds it
        // alive, so equal bits are equal identity without the gate. That is
        // every steady-state call of a named function (GitHub #487).
        let pinned = self.callable_cache.last_callable_pinned.get();
        if pinned != 0 && pinned == function.0 {
            return Some(previous);
        }
        previous
            .with_gc_stable_mutator(|value| *value == *function)
            .then_some(previous)
    }

    /// Apply `inspect` to the previous activation's callable root, without
    /// cloning it, when that root holds exactly the pinned function `function`.
    /// `inspect` must not park or allocate: it runs under the cell's borrow.
    pub(super) fn with_pinned_callable_root<R>(
        &self,
        function: EgclVal,
        inspect: impl FnOnce(&Arc<egcl_rt::CrossThreadRoot<EgclVal>>) -> R,
    ) -> Option<R> {
        let pinned = self.callable_cache.last_callable_pinned.get();
        if pinned == 0 || pinned != function.0 {
            return None;
        }
        self.callable_cache
            .last_callable
            .borrow()
            .as_ref()
            .map(inspect)
    }

    pub(super) fn take_spare_root_identity(&self) -> Option<Rc<super::call_table::RootIdentity>> {
        self.callable_cache.spare_root_identity.borrow_mut().take()
    }

    pub(super) fn store_spare_root_identity(&self, identity: Rc<super::call_table::RootIdentity>) {
        // A collection may have emptied this cache while the identity was
        // active. Re-enqueue before caching it again after the frame retires.
        self.queue_private_callable_cache();
        let old = self
            .callable_cache
            .spare_root_identity
            .borrow_mut()
            .replace(identity);
        drop(old);
    }

    pub(super) fn run(&self, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
        // Diagnostic/cached entries resolve once. Ordinary dispatch supplies its
        // already selected callable through run_callable instead.
        let function = self
            .installed_symbol
            .or(self.recursive_symbol)
            .filter(|symbol| recursive_definition_matches(*symbol, Arc::as_ptr(&self.body)))
            .and_then(egcl_rt::symbols::symbol_function)
            .unwrap_or(NIL);
        self.run_callable(function, args, env)
    }

    pub(super) fn run_callable(
        &self,
        function: EgclVal,
        args: &[EgclVal],
        env: &mut Env,
    ) -> Result<EgclVal, EgclError> {
        // A cached native target can bypass call_registered entirely. Its
        // lambda-list initforms still have the callee's lexical declaration
        // scope, never the caller's local SPECIAL declarations.
        if env.locally_specials.is_empty() {
            self.run_callable_in_definition_scope(function, args, env)
        } else {
            super::super::with_special_declarations(env, Vec::new(), |env| {
                self.run_callable_in_definition_scope(function, args, env)
            })
        }
    }

    fn run_callable_in_definition_scope(
        &self,
        function: EgclVal,
        args: &[EgclVal],
        env: &mut Env,
    ) -> Result<EgclVal, EgclError> {
        let function = if self.native_callable_slot.is_some() {
            let selected = funcall_fast::closure_carrier(function).ok_or_else(invalid_capture)?;
            if !self.accepts_explicit_callable(selected) {
                return Err(invalid_capture());
            }
            selected
        } else {
            function
        };
        if let Some(offset) = self.lean_root_entry() {
            return self.run_ordinary_root(offset, function, args, env);
        }
        self.run_activation(args, env, None, function)
    }

    fn run_activation(
        &self,
        args: &[EgclVal],
        env: &mut Env,
        osr: Option<OsrResume>,
        function: EgclVal,
    ) -> Result<EgclVal, EgclError> {
        let osr_offset = osr.as_ref().map(|resume| resume.offset);
        // Claim both inherited owners before preparation can fail or collect.
        // Tags remain in the rooted Env; the payload owns their exact tokens.
        let mut frame_guard = FrameGuard(osr.as_ref().map(|resume| resume.frame));
        let _catch_guard = CatchScopeGuard {
            base: osr
                .as_ref()
                .map_or(env.catch_stack.len(), |resume| resume.state.catch_base),
            env,
        };
        // Nothing collects before every value below is rooted in `roots`.
        // `bind_params` pads omitted `&optional` arguments below, and
        // `bind_variadic` parses a `&rest`/`&key` tail.
        if osr_offset.is_none() && !super::arity_accepts(&self.body, args.len()) {
            return Err(EgclError::ProgramError(
                "native transfer entry: wrong argument count".into(),
            ));
        }
        if osr_offset.is_none() {
            validate_declared_args(&self.body, args)?;
        }
        let mut state = activation::PooledActivation::new(
            self,
            match osr {
                Some(resume) => resume.state,
                None => ActivationState::prepare_reusing(self, env)?,
            },
        );
        let mut children = SegmentActivations::default();
        // One host root for the callable, a copy of the actuals (inline for
        // common arities), the Env, the state, the child ledger and the
        // enclosing pending error, instead of a link for each (GitHub #487).
        egcl_rt::rooted!(
            roots = lean_root::EntryRoots {
                function,
                code_owner: self.installed_symbol,
                args: EntryArgs::new(args),
                env,
                state: &mut *state,
                children: &mut children,
                entry: None,
            }
        );
        // The rooted copy stays in place; the collector updates it there.
        let args: &[EgclVal] = unsafe { &*std::ptr::from_ref(roots.args.as_slice()) };
        let _depth = osr_offset.is_none().then(|| {
            NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
            NativeDepthGuard
        });
        // Every snapshot is reserved and rooted before machine entry. Cold
        // preparation only copies words; no native frame survives into fallback.
        let stack = egcl_rt::current_stack();
        // Optimized metadata currently owns exactly one logical definition.
        let frame_function = if egcl_rt::function::is_interpreted_function(roots.function) {
            roots.function
        } else {
            self.deopt_metadata
                .as_ref()
                .and_then(|metadata| metadata.deopt_bodies.keys().next().copied())
                .or(self.installed_symbol)
                .map(EgclVal::from_symbol_index)
                .unwrap_or(NIL)
        };
        let frame = if osr_offset.is_some() {
            let frame = frame_guard.0.expect("committed OSR owns its managed frame");
            // Source preparation already reserved the space and retained its
            // own map until machine retirement. This commit cannot allocate,
            // collect, or fail: only switch the now-inactive frame's map.
            assert_eq!(stack.fp(), state.top_frame(frame));
            assert!(unsafe { (*frame).num_locals } >= self.slots);
            unsafe {
                (*frame).code_info = self.code_info;
            }
            frame
        } else {
            stack
                .push_frame(frame_function, self.code_info, self.slots, FLAG_CALL)
                .ok_or_else(|| {
                    EgclError::StackOverflow(
                        egcl_rt::current_fiber_id()
                            .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
                    )
                })?
        };
        frame_guard.0 = Some(frame);
        if osr_offset.is_none() && (self.body.has_env || self.body.variadic) {
            // The interpreter's environment, as in `run_with_binding`: a
            // capturing body gets a fresh heap frame under the callable's
            // captured one (or the caller's), shared with every closure it
            // creates. The rooted state owns it from here; an OSR destination
            // inherits it with the rest of the state (bliss-shih7.27).
            let closure_env = closure_captured_env(roots.function);
            state.env_frame = if self.body.has_env {
                let parent = closure_env
                    .clone()
                    .unwrap_or_else(|| Arc::clone(&env.frame));
                make_env_frame(&self.body, parent).or(closure_env)
            } else {
                closure_env
            };
        }
        if osr_offset.is_none() && self.body.variadic {
            // Defaults may run arbitrary Lisp, collect or exit non-locally.
            // The frame's slots are still NIL, as at the entry poll below, and
            // no native scope, block stack or entry state is installed yet;
            // `frame_guard` pops the frame on an error, as the checked ABI does.
            bind_variadic(&self.body, frame, args, state.env_frame.as_ref(), env)
                .map_err(EgclError::capture_backtrace)?;
        } else if osr_offset.is_none() {
            bind_params(&self.body, frame, args, state.env_frame.as_ref());
        }
        let _dynamic_scope_guard = DynamicScopeGuard {
            env,
            scopes: &mut state.dynamic_scopes,
        };
        let mut context = state.context(self, frame, &mut children, std::ptr::null_mut());
        let _scope = BlockScopeGuard {
            env,
            blocks: std::mem::take(&mut env.block_stack),
            tags: std::mem::take(&mut env.tag_stack),
        };
        if osr_offset.is_none() {
            env.clear_mv();
        }
        // Nested legacy callees and other executions must not overwrite this
        // invocation's capture pointer or an enclosing pending error.
        let _entry = roots.enter(env, &mut context);
        // An enclosing legacy recovery PC describes its own machine frame,
        // never this one or our Rust helpers. This entry has no fault maps yet.
        let _recovery = clear_fault_recovery();
        // The segment owns a rooted EgclStack frame and all host-side
        // execution state at this point, so this is a safe entry poll: a
        // moving GC may park and scan before generated code starts. Native
        // loop/back-edge polling remains a separate emitter gate.
        if osr_offset.is_none() && !current_poll_idle() {
            egcl_rt::thread::begin_current_execution_poll();
            egcl_rt::safepoint::poll_safepoint();
            let signal_error = pending_signal_error_for_current_execution();
            egcl_rt::thread::rearm_current_execution_poll();
            if native_error_pending() {
                egcl_rt::thread::request_current_execution_poll();
            }
            if let Some(error) = signal_error {
                return Err(error);
            }
        }
        // An OSR source has already polled. Its destination header polls again
        // with the inherited scopes installed; deliver pending errors there,
        // through normal transfer handling, rather than skipping their cleanup.
        let _ordinary_segment = super::call_table::enter_ordinary_segment();
        let _ordinary_root =
            if self.needs_callable_identity() && self.native_callable_slot.is_none() {
                Some(
                    _ordinary_segment
                        .retain_root(self, roots.function)
                        .ok_or_else(invalid_capture)?,
                )
            } else {
                None
            };
        // A `&key` body was bound into its frame above; enter past binding
        // with those slots as the fixed arguments. The frame stays scanned.
        let bound_args;
        let (ordinary_offset, args) = match self.ordinary_bound_entry {
            Some(offset) => {
                // Every bound slot, including for a leaf body, which keeps no
                // native argument homes.
                let count = self.ordinary_parameter_count();
                bound_args =
                    unsafe { std::slice::from_raw_parts(frame.add(1).cast::<EgclVal>(), count) };
                (Some(offset), bound_args)
            }
            None => (self.ordinary_entry, args),
        };
        let outcome = if let Some(offset) = ordinary_offset {
            assert!(osr_offset.is_none(), "ordinary entry is not an OSR destination");
            let control = unsafe { native_frames::initialize() };
            let execution = unsafe { &*(*control).execution };
            // Keep the existing execution-specific emergency stack reserve.
            // This additional margin covers the bounded Rust/root-shim adapter;
            // generated child calls check their exact emitted stack bounds.
            let sp: usize;
            unsafe { std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags)); }
            let overflow_bytes = args.len().saturating_sub(3).checked_mul(8)
                .and_then(|size| size.checked_add(15)).map(|size| size & !15)
                .ok_or_else(invalid_capture)?;
            let required = self.ordinary_stack_required.expect("ordinary stack admission bound")
                .checked_add(4096).and_then(|size| size.checked_add(overflow_bytes))
                .ok_or_else(invalid_capture)?;
            if sp.checked_sub(required).is_none_or(|bottom| bottom as u64 <= unsafe { (*control).stack_limit }) {
                egcl_rt::stack::report_stack_overflow(
                    "native-admission", required,
                    Some(NATIVE_DEPTH.with(|depth| depth.get()) as usize),
                    None,
                );
                return Err(EgclError::StackOverflow(egcl_rt::current_fiber_id()
                    .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0))));
            }
            unsafe {
                execution.with_active_native_control(std::pin::Pin::new_unchecked(&mut *control), || {
                    // Tests post requests after the real entry poll, so the
                    // generated CALL admission and cold handler must deliver them.
                    #[cfg(test)]
                    if let Some(observe) = ORDINARY_ENTRY_HOOK.with(|hook| hook.take()) { observe(); }
                    native_transfer::invoke_ordinary_native_segment(
                        self.code.as_ptr().add(offset), execution,
                        if self.native_callable_slot.is_some() { roots.function } else { NIL },
                        args, stack)
                })
            }.map(|returned| returned.outcome)
        } else {
            unsafe {
                native_transfer::invoke_native_segment(
                    self.code.as_ptr().add(osr_offset.unwrap_or(0)), context.activation.cast(), stack)
            }
        }.map_err(|_| EgclError::Internal("native segment is unavailable".into()))?;
        // No Lisp allocation between the machine return and rooting its value
        // or taking ownership of the execution-local error.
        let primary = outcome.value;
        let error = take_native_error();
        // The common outcome: a plain return. Nothing remains to unwind and
        // nothing below allocates before the value is returned, so it needs
        // neither root nor the checks below (GitHub #487).
        if outcome.exit == NativeExit::Returned
            && error.is_none()
            && context.failure.is_none()
            && context.osr.is_none()
            && context.selected.is_none()
            && state.is_quiescent()
        {
            return Ok(primary);
        }
        egcl_rt::rooted!(primary = primary);
        egcl_rt::rooted!(error = error);
        if context.failure.is_some() {
            return Err(invalid_capture());
        }
        if let Some((next, offset)) = context.osr.take() {
            if outcome.exit != NativeExit::Transfer
                || error.is_some()
                || context.selected.is_some()
                || !state.can_osr()
            {
                return Err(invalid_capture());
            }
            let NativeCodeStorage::Mapped(code) = &next._storage else {
                return Err(invalid_capture());
            };
            let code = code.osr_destination();
            if let Some(symbol) = code.installed_symbol {
                OSR_ENTRY_COUNTS.with(|counts| {
                    let mut counts = counts.borrow_mut();
                    let count = counts.entry(symbol).or_insert(0);
                    *count = count.saturating_add(1);
                });
            }
            // Move live payloads while the destination is still traced by the
            // source state. No allocating operation separates take and entry.
            let ready = state.take_osr_state();
            let resume = OsrResume {
                frame: frame_guard.0.take().unwrap(),
                offset,
                state: ready,
            };
            // This suspended source's catch guard remains an error backstop.
            // It can only truncate again after the destination has returned.
            return code.run_activation(&[], env, Some(resume), NIL);
        }
        match outcome.exit {
            NativeExit::Returned
                if error.is_none() && context.selected.is_none() && state.is_quiescent() =>
            {
                Ok(*primary)
            }
            NativeExit::Transfer => {
                if context.recursive_escape {
                    // Every scope-free generated activation has been retired.
                    // The selected transfer leaves this segment without replay
                    // or reconstruction of any of its recursive callers.
                    return error.take().ok_or_else(invalid_capture).and_then(Err);
                }
                unsafe {
                    self.resume_transfer(
                        &mut context,
                        &mut frame_guard.0,
                        env,
                        error.take().ok_or_else(invalid_capture)?,
                    )
                }
            }
            _ => Err(invalid_capture()),
        }
    }
    /// Resume only after the owning machine frame has been abandoned. Frame
    /// ownership moves to T0 once its rooted activation is complete.
    unsafe fn resume_transfer(
        &self,
        context: *mut CaptureContext,
        frame_owner: *mut Option<*mut Frame>,
        env: &mut Env,
        error: EgclError,
    ) -> Result<EgclVal, EgclError> {
        egcl_rt::rooted!(error = Some(error));
        let context = unsafe { &mut *context };
        let frame = context.frame;
        let stack = egcl_rt::current_stack();
        let cleanups = unsafe { &mut *context.cleanups };
        let catches = unsafe { &mut *context.catches };
        let handlers = unsafe { &mut *context.handlers };
        let handler_binds = unsafe { &mut *context.handler_binds };
        let restart_cases = unsafe { &mut *context.restart_cases };
        let dynamic_scopes = unsafe { &mut *context.dynamic_scopes };
        #[cfg(test)]
        NATIVE_FALLBACK_COUNT.with(|count| count.set(count.get() + 1));
        let index = context.selected.ok_or_else(invalid_capture)?;
        if error.is_none() {
            return Err(invalid_capture());
        }
        // A raw double home (bliss-vwpaz) is reboxed as deopt does.
        egcl_rt::rooted!(
            frames = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index) }
                .reconstruct(egcl_rt::gc::alloc_double_float)
                .map_err(|_| invalid_capture())?
        );
        let site = self.sites.sites().nth(index).ok_or_else(invalid_capture)?;
        // Explicit bounded-loop polls recover at a verified logical boundary,
        // which need not be an allocating/calling bytecode instruction. Admit
        // only an exact recorded poll callsite; all frame checks still apply.
        let bounded_loop_poll = self
            ._native_calls
            .get(site.return_offset() as usize)
            .is_some_and(|call| {
                matches!(
                    call.origin,
                    egcl_compiler::t2::x64_calls::NativeCallOrigin::BoundedLoopEntry(_)
                        | egcl_compiler::t2::x64_calls::NativeCallOrigin::BoundedLoopExit(_)
                )
            });
        let saved = frames
            .first()
            .filter(|saved| {
                frames.len() == 1
                    && saved.resume_pc == site.map().origin_bcp
                    && saved.locals.len() == usize::from(self.body.n_locals)
                    && saved.stack.len() <= usize::from(self.body.max_stack)
                    && self.body.code.get(saved.resume_pc as usize).is_some()
                    && (bounded_loop_poll
                        || matches!(
                            self.body.code.get(saved.resume_pc as usize),
                            Some(
                                Instr::CallNamed { .. }
                                    | Instr::EvalHost(_)
                                    | Instr::LoadFunction(_)
                                    | Instr::ValuesToList
                                    | Instr::MakeClosure { .. }
                                    | Instr::LoadEnvVar(_)
                                    | Instr::StoreEnvVar(_)
                                    | Instr::DefineEnvVar(_)
                                    | Instr::PushEnvChild
                                    | Instr::PopEnvChild
                                    | Instr::AllocCons
                                    | Instr::MakeStruct { .. }
                                    | Instr::CleanupReturn
                                    | Instr::SetValues(_)
                                    | Instr::Throw
                                    | Instr::PushCatch { .. }
                                    | Instr::PopHandler
                                    | Instr::PushHandlerCase { .. }
                                    | Instr::PopHandlerCase
                                    | Instr::PushHandlerBind { .. }
                                    | Instr::PopHandlerBind
                                    | Instr::PushRestartCase { .. }
                                    | Instr::PopRestartCase
                                    | Instr::BindSpecial(_)
                                    | Instr::UnbindSpecial(_)
                                    | Instr::LoadGlobal(_)
                                    | Instr::StoreGlobal(_)
                            )
                        ))
            })
            .ok_or_else(invalid_capture)?;
        let running = site.map().control_scopes.iter().filter_map(|scope| {
            if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
                (Some(cleanup_bcp) != context.completed_cleanup).then_some(cleanup_bcp)
            } else {
                None
            }
        });
        if !running.eq(cleanups.iter().map(|saved| saved.cleanup_bcp)) {
            return Err(invalid_capture());
        }
        let live_catches = site.map().control_scopes.iter().filter_map(|scope| {
            matches!(scope.kind, ScopeKind::Catch { .. }).then_some(scope.push_bcp)
        });
        if !live_catches.eq(catches.iter().map(|saved| saved.push_bcp)) {
            return Err(invalid_capture());
        }
        let live_handlers = site.map().control_scopes.iter().filter_map(|scope| {
            matches!(scope.kind, ScopeKind::HandlerCase { .. }).then_some(scope.push_bcp)
        });
        if !live_handlers.eq(handlers.iter().map(|saved| saved.push_bcp)) {
            return Err(invalid_capture());
        }
        let live_handler_binds = site.map().control_scopes.iter().filter_map(|scope| {
            matches!(scope.kind, ScopeKind::HandlerBind { .. }).then_some(scope.push_bcp)
        });
        if !live_handler_binds.eq(handler_binds.iter().map(|saved| saved.push_bcp)) {
            return Err(invalid_capture());
        }
        let live_restart_cases = site.map().control_scopes.iter().filter_map(|scope| {
            matches!(scope.kind, ScopeKind::RestartCase { .. }).then_some(scope.push_bcp)
        });
        if !live_restart_cases.eq(restart_cases.iter().map(|saved| saved.push_bcp)) {
            return Err(invalid_capture());
        }
        if !unsafe { validate_special_bindings(context, &site.map().control_scopes) } {
            return Err(invalid_capture());
        }
        // Reconstructed locals can omit dead arguments, so discard native
        // original-argument recipes before installing bytecode state. T0
        // scans all tagged slots without a native stack map.
        unsafe {
            (*frame).code_info = std::ptr::null();
        }
        for (index, value) in saved.locals.iter().chain(&saved.stack).enumerate() {
            unsafe { slot_set(frame, index as u16, *value) };
        }
        let specials = unsafe { &mut *context.specials };
        // The frame current at the site, which the activation resumes in.
        let site_env_frame = unsafe { (*context.env_frame).clone() };
        let restored_handlers = site
            .map()
            .control_scopes
            .iter()
            // Each handler resumes in the frame it was established in, not in
            // a child frame entered natively after it (bliss-uhsp5).
            .zip(scope_env_frames(
                &site.map().control_scopes,
                &site_env_frame,
            ))
            // A lexical environment is the activation's EnvFrame, not a handler.
            .filter(|(scope, _)| {
                !matches!(
                    scope.kind,
                    ScopeKind::Cleanup { .. }
                        | ScopeKind::SpecialBinding { .. }
                        | ScopeKind::LexicalEnvironment
                )
            })
            .map(|(scope, scope_env)| match scope.kind {
                ScopeKind::Block {
                    id,
                    resume_bcp,
                    register: false,
                } => Handler::Block {
                    block_id: id,
                    token: String::new(),
                    resume_bcp,
                    sp_restore: scope.sp_restore,
                    env: scope_env,
                },
                ScopeKind::Tagbody { id } => Handler::Tag {
                    tagbody_id: id,
                    sp_restore: scope.sp_restore,
                    token: None,
                    tag_bcps: Vec::new(),
                    env: scope_env,
                },
                ScopeKind::Unwind { cleanup_bcp } => Handler::Unwind {
                    cleanup_bcp,
                    sp_restore: scope.sp_restore,
                    env: scope_env,
                },
                ScopeKind::HandlerCase { .. } => {
                    let saved = handlers
                        .iter_mut()
                        .find(|saved| saved.push_bcp == scope.push_bcp)
                        .expect("checked handler identity");
                    Handler::HandlerCase {
                        clauses: std::mem::take(&mut saved.clauses),
                        sp_restore: scope.sp_restore,
                        cluster_base: saved.cluster_base,
                        cluster_frame: saved.cluster_frame,
                        env: scope_env,
                    }
                }
                ScopeKind::HandlerBind { .. } => {
                    let saved = handler_binds
                        .iter()
                        .find(|saved| saved.push_bcp == scope.push_bcp)
                        .expect("checked handler-bind identity");
                    Handler::HandlerBind {
                        cluster_base: saved.cluster_base,
                        cluster_frame: saved.cluster_frame,
                    }
                }
                ScopeKind::RestartCase { resume_bcp, .. } => {
                    let saved = restart_cases
                        .iter()
                        .find(|saved| saved.push_bcp == scope.push_bcp)
                        .expect("checked restart-case identity");
                    Handler::RestartCase {
                        restart_base: saved.restart_base,
                        resume_bcp,
                        sp_restore: scope.sp_restore,
                        cluster_frame: saved.cluster_frame,
                        env: scope_env,
                    }
                }
                ScopeKind::Catch { resume_bcp } => Handler::Catch {
                    token: catches
                        .iter()
                        .find(|saved| saved.push_bcp == scope.push_bcp)
                        .expect("checked catch identity")
                        .token
                        .clone(),
                    resume_bcp,
                    sp_restore: scope.sp_restore,
                    env: scope_env,
                },
                _ => unreachable!("admission checked all scope records"),
            })
            .collect();
        // Bytecode now owns the live cluster frames and registrations.
        handlers.clear();
        handler_binds.clear();
        restart_cases.clear();
        dynamic_scopes.clear();
        catches.clear();
        let mut acts = vec![Activation {
            frame,
            _debug_call: None,
            func: self.body.clone(),
            bcp: saved.resume_pc as usize,
            sp_top: saved.stack.len() as u16,
            n_locals: self.body.n_locals,
            handlers: restored_handlers,
            cleanup_conts: cleanups.drain(..).map(|saved| saved.continuation).collect(),
            dyn_binds: specials.drain(..).map(|saved| saved.binding).collect(),
            // The exact frame, including child frames entered natively.
            env_frame: site_env_frame,
            fn_obj: None,
            sym: u32::MAX,
        }];
        egcl_rt::rooted_ref!(_acts = &mut acts);
        // Transfer ownership only after constructing the activation.
        // Crucially: unwind FIRST, never run the failed CallNamed again.
        unsafe {
            *frame_owner = None;
        }
        let _capture = CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
        let pending = error_to_pending(error.take().expect("checked pending error"), env);
        let result = guard_c2i(|| {
            initiate_unwind(&mut acts, stack, env, pending).and_then(|()| run_loop(&mut acts, env))
        });
        while let Some(mut act) = acts.pop() {
            release_activation_handlers(&mut act, stack, env);
            stack.pop_frame();
        }
        result
    }
}

fn invalid_capture() -> EgclError {
    EgclError::Internal("invalid native transfer capture".into())
}

/// Parameters the ordinary entry binds itself: fixed ones, or up to three
/// positional parameters of which the trailing ones are `&optional`. The
/// entry pads an omitted one with the unsupplied marker (bliss-6m6ca).
fn ordinary_entry_parameters(body: &BytecodeFunction) -> bool {
    fixed_tagged_parameters(body)
        || ordinary_outer_capture_body(body)
        || (positional_tagged_parameters(body)
            && body.min_args < body.arity
            && body.arity <= 3
            && body.param_types.iter().all(|ty| ty.is_any()))
        || (body.variadic
            && !body.has_env
            && body.param_types.iter().all(|ty| ty.is_any())
            && super::variadic_plan::VariadicPlan::native_keywords(body).is_some())
}

pub(super) fn fixed_tagged_parameters(body: &BytecodeFunction) -> bool {
    body.min_args == body.arity && positional_tagged_parameters(body)
}

/// Fixed positional slot parameters, of which an `&optional` entry prologue's
/// trailing `arity - min_args` may be omitted (bliss-shih7.27). The managed
/// activation entry pads them with the unsupplied marker through `bind_params`;
/// register entries, mapped callees and direct recursion still require
/// `fixed_tagged_parameters`.
pub(super) fn positional_tagged_parameters(body: &BytecodeFunction) -> bool {
    !body.has_env
        && !body.variadic
        && body.min_args <= body.arity
        && body.max_args == Some(body.arity)
        && body.param_layout.len() == usize::from(body.arity)
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
}

/// The symbol a bytecode instruction calls through the T2 IR, if any. A
/// backquote's AllocCons is built as a call to CONS (bliss-elwvl), so it needs
/// that call cell like any CallNamed.
fn named_call_symbol(instruction: &Instr) -> Option<u32> {
    match instruction {
        Instr::CallNamed { sym, .. } => Some(*sym),
        Instr::AllocCons => Some(egcl_rt::symbols::intern("CONS")),
        // A structure constructor is built as its %MAKE-STRUCT call.
        Instr::MakeStruct { .. } => Some(egcl_rt::symbols::intern("EGCL::%MAKE-STRUCT")),
        // A scoped body's structure opcodes are built as their general calls.
        Instr::StructRef { .. } => Some(egcl_rt::symbols::intern("EGCL::%STRUCT-REF")),
        Instr::StructSet { .. } => Some(egcl_rt::symbols::intern("EGCL::%STRUCT-SET")),
        Instr::StructTypeP { .. } => Some(egcl_rt::symbols::intern("EGCL::%STRUCT-TYPEP")),
        _ => None,
    }
}

fn named_call_cells(body: &BytecodeFunction) -> Vec<(u32, Arc<egcl_rt::call_table::CallCell>)> {
    let mut cells: Vec<(u32, Arc<egcl_rt::call_table::CallCell>)> = Vec::new();
    for symbol in body.code.iter().filter_map(named_call_symbol) {
        if !cells.iter().any(|(existing, _)| *existing == symbol)
            && let Some(cell) = call_table::resolve(symbol)
        {
            cells.push((symbol, cell));
        }
    }
    cells
}

/// Parameters the managed activation entry can bind: positional ones, or a
/// non-capturing `&rest`/`&key`/`&aux` lambda list whose `n`th parameter lives
/// in slot `n`, which `bind_variadic` fills before entry (bliss-shih7.27). T2's
/// entry seeds the same slot prefix. Only the managed entry admits the variadic
/// shape; every exact-arity entry still requires `fixed_tagged_parameters`.
pub(super) fn activation_bound_parameters(body: &BytecodeFunction) -> bool {
    positional_tagged_parameters(body)
        || variadic_slot_parameters(body)
        || captured_activation_parameters(body)
}

/// A capturing body whose heap EnvFrame the managed activation entry builds
/// exactly as `run_with_binding` does, binding its boxed parameters there and
/// its slot parameters into the slot prefix T2's entry seeds (bliss-shih7.27).
pub(super) fn captured_activation_parameters(body: &BytecodeFunction) -> bool {
    body.has_env && captured_environment_decline(body).is_none()
}

/// Why the managed entry cannot own `body`'s captured environment, or `None`
/// when it can or there is none. Native code reaches captured variables only
/// through runtime helpers that walk the activation's frame chain exactly as
/// the interpreter does. That frame's parent is the captured environment of the
/// callable actually invoked (`run_activation` receives the instance, never a
/// name to look up), so a closure body reading or writing its creator's
/// bindings resolves them the same way, and many instances of one template
/// share the compiled body while each activation gets its own frame chain
/// (bliss-6fim1; the installed wrapper of bliss-kg667 runs natively). Bindings
/// are found by name: a body that reads an enclosing X and also binds its own
/// captured X takes the interpreter's chain walk before, inside and after the
/// inner binding (pinned by `shadowing_captured_names_match_the_interpreter`).
pub(super) fn captured_environment_decline(body: &BytecodeFunction) -> Option<&'static str> {
    if !body.has_env {
        return None;
    }
    let exact_positional = body.min_args <= body.arity
        && body.max_args == Some(body.arity)
        && body.param_layout.len() == usize::from(body.arity);
    if !(body.variadic || exact_positional) {
        return Some("unsupported lambda-list shape");
    }
    // Boxed parameters take no slot. The remaining ones must be the slot
    // prefix, in order, that the entry seeds from the activation.
    let mut next_slot = 0u16;
    for (_, location) in &body.param_layout {
        if let VarLoc::Slot(slot) = location {
            if *slot != next_slot || *slot >= body.n_locals {
                return Some("a parameter slot is out of order");
            }
            next_slot += 1;
        }
    }
    let boxed = || {
        body.param_layout
            .iter()
            .filter(|(_, location)| matches!(location, VarLoc::Boxed))
    };
    // Declared types align with parameters, not slots; T2 would check the
    // wrong slot once a boxed parameter shifts the prefix.
    if boxed().next().is_some() && body.param_types.iter().any(|ty| !ty.is_any()) {
        return Some("a declared parameter type beside a captured parameter");
    }
    if body
        .code
        .iter()
        .any(|op| matches!(op, Instr::MakeClosureEnv(_)))
    {
        return Some("builds a closure from source (MakeClosureEnv)");
    }
    None
}

fn variadic_slot_parameters(body: &BytecodeFunction) -> bool {
    body.variadic
        && !body.has_env
        && body.param_layout.len() <= usize::from(body.n_locals)
        && body.param_layout.iter().enumerate().all(|(index, (_, location))| {
            matches!(location, VarLoc::Slot(slot) if usize::from(*slot) == index)
        })
}

fn scope_free_native_body(body: &BytecodeFunction, allow_values: bool) -> bool {
    !body.has_env && local_control_native_body(body, allow_values, false, false)
}

/// A body the ordinary x86-64 Lisp entry may run: scope-free, or scope-free
/// apart from local TAGBODYs (bliss-6m6ca). Every DOTIMES, DOLIST, DO and
/// LOOP is one, so admitting them lets a loop call its callees through the
/// ordinary ABI instead of preparing a mapped child activation per call.
///
/// A local `Go` names a tagbody of this same bytecode function: the compiler
/// emits `GoNamed` for a tag owned by an enclosing function, and `NamedTag`
/// (which `try_compile_variant` refuses) when a closure could capture a tag.
/// With no closure, catch, unwind or binding in the body, nothing
/// outside this activation can transfer to its tags, so T2 lowers each `Go`
/// to a plain jump and the ordinary frame keeps no runtime handler record.
/// Deoptimization rebuilds the record from the exact scope map, as it does for
/// a local BLOCK, so these bodies must not own more than one logical frame
/// per guard (see `ordinary_single_frame_states`).
///
/// A SETF expansion such as PUSH or DECF binds its new value through a
/// one-variable `TakeValuesToLocals` (the primary-value binder). Its T2
/// specialization removes the binder, as `packed_f64_native_body` relies on;
/// `ordinary_primary_transport_eliminated` checks that it did (bliss-z4dlm).
pub(super) fn ordinary_native_body(body: &BytecodeFunction) -> bool {
    (!body.has_env || ordinary_outer_capture_body(body))
        && slot_parameters(body, false)
        && body.code.iter().all(ordinary_native_op)
}

// Only a private owner selected before worker detachment may opt into the
// explicit-callable NoEnv ABI. Keep binders, self/control exits and loops on
// their existing paths; the optimized IR must additionally remain a leaf.
fn private_noenv_callable_body(body: &BytecodeFunction) -> bool {
    !body.has_env
        && !body.variadic
        && body.arity <= 3
        && body.min_args == body.arity
        && body.max_args == Some(body.arity)
        && body.params_form.is_nil()
        && body.self_call_sites.is_empty()
        && body.param_layout.len() == usize::from(body.arity)
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
        && body.load_time_values.is_empty()
        && body.nested_functions.is_empty()
        && body.restart_cases.is_empty()
        && body.handler_cases.is_empty()
        && body.handler_binds.is_empty()
        && scope_free_native_body(body, false)
        && !bf_has_free_control_exit(body)
        && !body.code.iter().enumerate().any(|(bcp, op)| match op {
            Instr::Br(target) | Instr::BrIfFalse(target) | Instr::BrIfTrue(target) => {
                (*target as usize) <= bcp
            }
            Instr::Go { target_bcp, .. } => (*target_bcp as usize) <= bcp,
            Instr::GoNamed { .. } => true,
            _ => false,
        })
}

fn private_noenv_leaf_ir(ir: &egcl_compiler::t2::ir::Function) -> bool {
    use egcl_compiler::t2::ir::Opcode;
    ir.osr_entries.is_empty()
        && ir.block_order().iter().all(|&block| {
            ir.block(block).insts.iter().all(|&inst| {
                let data = ir.inst(inst);
                !matches!(
                    data.opcode,
                    Opcode::Call | Opcode::Invoke | Opcode::TailCall | Opcode::CurrentCallable
                ) && !data.flags.call
                    && !data.flags.safepoint
                    && (!data.flags.effectful || data.flags.guard || data.opcode == Opcode::ClearMv)
            })
        })
}

/// Bounded executable cohorts whose immutable template may own metadata
/// independently of an otherwise dead closure instance.
pub(super) fn ordinary_code_owner_body(body: &BytecodeFunction) -> bool {
    ordinary_outer_capture_shape(body)
        || (!body.has_env
            && !body.variadic
            && body.min_args == body.arity
            && body.max_args == Some(body.arity)
            && body.param_layout.len() == usize::from(body.arity)
            && body
                .param_layout
                .iter()
                .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
            && scope_free_native_body(body, false))
}

/// `has_env` covers both outer captures and newly defined lexical cells. This
/// cohort has only the former: the interpreter's fresh child frame would stay
/// empty, cannot escape, and can therefore be omitted by the native entry.
pub(super) fn ordinary_outer_capture_body(body: &BytecodeFunction) -> bool {
    ordinary_captured_callees_enabled() && ordinary_outer_capture_shape(body)
}

fn ordinary_outer_capture_shape(body: &BytecodeFunction) -> bool {
    body.has_env
        && !body.variadic
        && body.min_args == body.arity
        && body.max_args == Some(body.arity)
        && body.param_layout.len() == usize::from(body.arity)
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
        && body
            .code
            .iter()
            .any(|op| matches!(op, Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_)))
        && body.code.iter().all(ordinary_native_op)
}

fn ordinary_captured_callees_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_ORDINARY_CAPTURED_CALLEES").is_none())
}

/// Acyclic captured entries use noncollecting lexical helpers on successful
/// accesses. Keep loops on the mapped entry until repeated-access costs have
/// separate evidence; semantic admission remains an independent check.
fn captured_entry_cost_permits(body: &BytecodeFunction) -> bool {
    if !body.has_env {
        return true;
    }
    let mut accesses = 0;
    for (bcp, instruction) in body.code.iter().enumerate() {
        match instruction {
            Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_) => {
                accesses += 1;
            }
            Instr::Br(target) | Instr::BrIfFalse(target) | Instr::BrIfTrue(target)
                if (*target as usize) <= bcp =>
            {
                return false;
            }
            Instr::Go { target_bcp, .. } if (*target_bcp as usize) <= bcp => return false,
            Instr::GoNamed { .. } => return false,
            _ => {}
        }
    }
    accesses != 0
}

pub(super) fn ordinary_capture_entry_profitable(body: &BytecodeFunction) -> bool {
    #[cfg(test)]
    if FORCE_CAPTURED_ENTRY.load(std::sync::atomic::Ordering::SeqCst) {
        return true;
    }
    captured_entry_cost_permits(body)
}

fn ordinary_native_op(op: &Instr) -> bool {
    local_control_op(op, false, false, true)
        || matches!(
            op,
            Instr::PushTag { .. } | Instr::Go { .. } | Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_)
        )
        || (matches!(op, Instr::LoadFunction(_)) && ordinary_function_designators_enabled())
}

pub(super) fn ordinary_function_designators_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("EGCL_DISABLE_ORDINARY_FUNCTION_DESIGNATORS").is_none())
}

/// Whether specialization removed every primary-value binder the body's
/// bytecode contains, so the ordinary entry never needs multiple values.
fn ordinary_primary_transport_eliminated(ir: &egcl_compiler::t2::ir::Function) -> bool {
    ir.block_order().iter().all(|&block| {
        ir.block(block)
            .insts
            .iter()
            .all(|&inst| ir.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::TakeValuesToLocals)
    })
}

/// The first instruction `ordinary_native_body` refuses, for its decline log.
fn first_refused_ordinary_op(body: &BytecodeFunction) -> Option<String> {
    let op = body.code.iter().find(|op| !ordinary_native_op(op))?;
    let full = format!("{op:?}");
    Some(
        full.find(['(', ' ', '{'])
            .map_or(full.clone(), |i| full[..i].to_string()),
    )
}

/// Whether a body uses the TAGBODY extension `ordinary_native_body` admits.
fn has_local_tagbody(body: &BytecodeFunction) -> bool {
    body.code
        .iter()
        .any(|op| matches!(op, Instr::PushTag { .. }))
}

/// Retained self expansion reconstructs only local BLOCKs in nested logical
/// frames. A tagbody body is admitted only while every guard owns one frame.
pub(super) fn ordinary_capture_protocol_present(
    body: &BytecodeFunction,
    ir: &egcl_compiler::t2::ir::Function,
) -> bool {
    use egcl_compiler::t2::ir::Opcode;
    // Eliminating every lexical access must not accidentally turn a capturing
    // callable into the environment-free directory's strong identity cache.
    // Keep the checked/mapped fallback until there is an explicit carrier.
    !body.has_env
        || ir.block_order().iter().any(|&block| {
            ir.block(block).insts.iter().any(|&inst| {
                matches!(
                    ir.inst(inst).opcode,
                    Opcode::EnvironmentValue
                        | Opcode::SetEnvironmentValue
                        | Opcode::CurrentCallable
                )
            })
        })
}

fn ordinary_single_frame_states(
    body: &BytecodeFunction,
    ir: &egcl_compiler::t2::ir::Function,
) -> bool {
    !(has_local_tagbody(body) || body.has_env)
        || ir
            .frame_states
            .iter()
            .all(|(_, state)| state.scopes.len() == 1)
}

// Raw numeric locals use the existing managed activation's local control scopes.
// Dynamic scopes, escaping control identities and MV still need independent
// representation-aware contracts before they can share this narrow entry.
fn packed_f64_native_body(body: &BytecodeFunction, ir: &egcl_compiler::t2::ir::Function) -> bool {
    numeric_native_body(body, ir, false)
}

// Fixnum boxing is a nonallocating tag shift; tagged multiple-value transport
// can therefore share its existing mapped contract after explicit conversion.
fn raw_numeric_native_body(body: &BytecodeFunction, ir: &egcl_compiler::t2::ir::Function) -> bool {
    let allow_values = (0..ir.num_values()).all(|index| {
        ir.value(egcl_compiler::t2::ir::Value(index as u32)).repr
            != egcl_compiler::t2::ir::ValueRepresentation::UnboxedF64
    });
    numeric_native_body(body, ir, allow_values)
}

fn numeric_native_body(
    body: &BytecodeFunction,
    ir: &egcl_compiler::t2::ir::Function,
    allow_values: bool,
) -> bool {
    // The original boot SETF bytecode retains its primary-value binder. Admit
    // that transport only when specialization eliminated every live binder.
    // Tagged multiple values are admitted for scalar fixnum candidates.
    let normalized = ir.block_order().iter().all(|&block| {
        ir.block(block)
            .insts
            .iter()
            .all(|&inst| ir.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::TakeValuesToLocals)
    });
    normalized
        && local_control_native_body(body, allow_values, true, true)
        && !body.code.iter().any(|op| {
            matches!(
                op,
                Instr::PushCatch { .. }
                    | Instr::Throw
                    | Instr::PushUnwind { .. }
                    | Instr::EnterCleanupNormal { .. }
                    | Instr::CleanupReturn
                    | Instr::PushHandlerCase { .. }
                    | Instr::PopHandlerCase
                    | Instr::PushHandlerBind { .. }
                    | Instr::PopHandlerBind
                    | Instr::PushRestartCase { .. }
                    | Instr::PopRestartCase
                    | Instr::BindSpecial(_)
                    | Instr::UnbindSpecial(_)
                    | Instr::LoadFunction(_)
                    | Instr::MakeClosure { .. }
                    | Instr::LoadEnvVar(_)
                    | Instr::StoreEnvVar(_)
                    | Instr::DefineEnvVar(_)
                    | Instr::PushEnvChild
                    | Instr::PopEnvChild
            )
        })
}

fn local_control_native_body(
    body: &BytecodeFunction,
    allow_values: bool,
    allow_local_scopes: bool,
    allow_primary_transport: bool,
) -> bool {
    slot_parameters(body, allow_local_scopes)
        && body.code.iter().all(|op| {
            local_control_op(
                op,
                allow_values,
                allow_local_scopes,
                allow_primary_transport,
            )
        })
}

fn slot_parameters(body: &BytecodeFunction, allow_local_scopes: bool) -> bool {
    // Captured variables are helper-v2 Invokes on the activation's EnvFrame,
    // so a capturing body needs the local-scope (native cleanup) emitter.
    (body.param_layout.len() == usize::from(body.arity) || variadic_slot_parameters(body))
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
        || allow_local_scopes && captured_activation_parameters(body)
}

/// The first instruction `local_control_native_body` refuses under the same
/// flags, named by its variant, for decline logs (bliss-s5r0c).
pub(super) fn first_refused_local_control_op(
    body: &BytecodeFunction,
    allow_values: bool,
    allow_local_scopes: bool,
    allow_primary_transport: bool,
) -> Option<String> {
    let op = body.code.iter().find(|op| {
        !local_control_op(
            op,
            allow_values,
            allow_local_scopes,
            allow_primary_transport,
        )
    })?;
    let full = format!("{op:?}");
    Some(
        full.find(['(', ' ', '{'])
            .map_or(full.clone(), |i| full[..i].to_string()),
    )
}

fn local_control_op(
    op: &Instr,
    allow_values: bool,
    allow_local_scopes: bool,
    allow_primary_transport: bool,
) -> bool {
    // Nested tagged callees support MV; optimized/direct-recursive
    // admission retains its existing restriction.
    (allow_values && matches!(op,
        Instr::SetValues(_) | Instr::TakeValuesToLocals { .. } | Instr::ValuesToList))
    || (allow_primary_transport && matches!(op, Instr::TakeValuesToLocals { nvars: 1, .. }))
    // A closure is a helper-v2 Invoke that indexes the activation's retained
    // body; a capturing one, like every captured-variable access, also
    // reaches the activation's EnvFrame (bliss-shih7.27).
    || (allow_local_scopes && matches!(op, Instr::MakeClosure { .. }
        | Instr::LoadEnvVar(_) | Instr::StoreEnvVar(_) | Instr::DefineEnvVar(_)
        | Instr::PushEnvChild | Instr::PopEnvChild))
    || (allow_local_scopes && matches!(op,
        Instr::PushTag { .. } | Instr::Go { .. } | Instr::PushCatch { .. } | Instr::Throw
            | Instr::PushUnwind { .. } | Instr::EnterCleanupNormal { .. } | Instr::CleanupReturn
            | Instr::PushHandlerCase { .. } | Instr::PopHandlerCase
            | Instr::PushHandlerBind { .. } | Instr::PopHandlerBind
            | Instr::PushRestartCase { .. } | Instr::PopRestartCase
            | Instr::BindSpecial(_) | Instr::UnbindSpecial(_)
            // #'f lowers to an argument-free FunctionLookup Invoke. It
            // needs the native-cleanups transfer emitter that mapped T1
            // already uses; ordinary entries still decline it (bliss-j3fv8).
            | Instr::LoadFunction(_)))
    || matches!(
        op,
        Instr::Const(_)
            | Instr::LoadLocal(_)
            | Instr::StoreLocal(_)
            | Instr::LoadGlobal(_) | Instr::StoreGlobal(_)
            | Instr::Pop
            | Instr::Dup
            | Instr::ClearMv
            // A result assertion preserves the primary and MV tuple;
            // unsupported assertion types still decline in T2 lowering.
            | Instr::CheckResult(_)
            // DEFSTRUCT opcodes: leaf helper calls that deopt on a miss,
            // and the constructor, built as its %MAKE-STRUCT call.
            | Instr::StructRef { .. }
            | Instr::StructSet { .. }
            | Instr::StructTypeP { .. }
            | Instr::MakeStruct { .. }
            | Instr::Br(_)
            | Instr::BrIfFalse(_)
            | Instr::BrIfTrue(_)
            | Instr::CallNamed { .. }
            | Instr::AllocCons
            | Instr::Return
            | Instr::PushBlock {
                register: false,
                ..
            }
            | Instr::PopHandler
            | Instr::ReturnFrom { .. }
    )
}

fn recursive_definition_matches(symbol: u32, body: *const BytecodeFunction) -> bool {
    let Some(function) = egcl_rt::symbols::symbol_function(symbol) else {
        return false;
    };
    callable_definition_matches(symbol, function, body)
}

/// True when `function` is an instance of the closure template whose shared
/// code is installed under the owning instance's private `symbol` with `body`.
/// That symbol has no function cell, and the running instance need not be the
/// owner: every instance of one template runs the same code (bliss-6fim1).
fn closure_code_matches(symbol: u32, function: EgclVal, body: *const BytecodeFunction) -> bool {
    egcl_rt::symbols::is_uninterned(symbol)
        && super::closure_template_matches(function, body)
        && super::registry_get(symbol).is_some_and(|owned| std::ptr::eq(Arc::as_ptr(&owned), body))
}

fn callable_definition_matches(
    symbol: u32,
    function: EgclVal,
    body: *const BytecodeFunction,
) -> bool {
    egcl_rt::function::is_interpreted_function(function)
        && replacement_function_value(symbol, function).is_none()
        && bytecode_for_callable(symbol, function).is_some_and(|(owner, current)| {
            owner == symbol && std::ptr::eq(Arc::as_ptr(&current), body)
        })
}

/// Prepare a distinct precise activation, then return before generated code
/// calls itself. The caller's arguments and suspended roots are already in its
/// scanned frame. No Rust frame spans the subsequent native self call.
unsafe extern "C" fn prepare_recursive(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    #[cfg(test)]
    RECURSIVE_HELPER_CALLS.with(|count| {
        let (prepare, finish) = count.get();
        count.set((prepare + 1, finish));
    });
    unsafe {
        root_publication::published(image, || {
            #[cfg(test)]
            root_publication::observe();
            prepare_recursive_unpublished(request, out)
        })
    }
}

/// Recheck only on invalidation. An alias to another callable must use ordinary
/// dispatch even when its retained bytecode happens to have the same body.
unsafe fn recursive_target_matches(context: *mut CaptureContext, symbol: u32) -> bool {
    let generation = direct_call_gen();
    if generation == unsafe { (*context).recursive_generation } {
        return true;
    }
    egcl_rt::rooted!(function = unsafe { (*(*context).frame).function });
    if !recursive_definition_matches(symbol, unsafe { (*context).body })
        || (egcl_rt::function::is_interpreted_function(*function)
            && egcl_rt::symbols::symbol_function(symbol) != Some(*function))
    {
        return false;
    }
    unsafe {
        (*context).recursive_generation = generation;
    }
    true
}

/// Cold, sampled scheduling runs while the suspended caller is precisely
/// published and before a child frame exists. The actual admission is counted
/// later; a cap, changed definition or failed frame push counts via fallback.
unsafe fn schedule_recursive_promotion(
    context: *mut CaptureContext,
    symbol: u32,
    count: u32,
) -> Result<EgclVal, EgclError> {
    let function = unsafe { (*(*context).frame).function };
    if !t2_enabled() || egcl_rt::function::tier(function) == 2 {
        return Ok(NIL);
    }
    egcl_rt::rooted!(function = function);
    // A fresh snapshot must describe this exact callable/body, never an older
    // recursive chain's replacement. Do not retain registry borrows across work.
    if egcl_rt::symbols::symbol_function(symbol) != Some(*function)
        || !recursive_definition_matches(symbol, unsafe { (*context).body })
    {
        return Ok(NIL);
    }
    poll_t2_completions();
    let needs_t2 = NATIVE_REGISTRY.with(|registry| {
        registry.borrow().get(&symbol).is_some_and(|installed| {
            !installed.is_t2
                && installed
                    .body
                    .as_ref()
                    .is_some_and(|body| std::ptr::eq(Arc::as_ptr(body), unsafe { (*context).body }))
        })
    });
    if needs_t2 {
        let priority =
            (u64::from(egcl_rt::function::back_edge_count(*function)) << 32) | u64::from(count);
        request_t2_compilation(symbol, priority);
        poll_t2_completions();
    }
    Ok(NIL)
}

unsafe fn prepare_recursive_unpublished(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &mut *request.cast::<RecursiveCallRequest>() };
    request.activation = RecursiveActivation {
        previous: std::ptr::null_mut(),
        frame: std::ptr::null_mut(),
        stack: std::ptr::null(),
    };
    request.reserved = 0;
    // Entry polling also covers recursion without a loop back-edge. This is
    // root-safe because no argument exists only in an unscanned register.
    unsafe {
        poll_or_transfer_unpublished(std::ptr::null_mut(), out);
    }
    if unsafe { (*out).exit } != NativeExit::Returned {
        return;
    }
    let context = CAPTURE.with(Cell::get);
    let symbol = request.call.symbol as u32;
    if !unsafe { recursive_target_matches(context, symbol) } {
        unsafe {
            c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out);
        }
        return;
    }
    let here = std::ptr::from_ref(&request) as usize as u64;
    let limit = egcl_rt::stack::current_native_stack_limit();
    if NATIVE_DEPTH.with(|depth| depth.get() >= native_depth_cap()) || here <= limit {
        unsafe {
            c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out);
        }
        return;
    }
    if unsafe { (*context).recursive_profile && !(*(*context).owner).is_t2 } {
        let function = unsafe { (*(*context).frame).function };
        let count = egcl_rt::function::invoke_count(function).saturating_add(1);
        if count % t2_invoke_threshold().max(1) == 0 {
            if let Err(error) =
                guard_c2i(|| unsafe { schedule_recursive_promotion(context, symbol, count) })
            {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
                unsafe {
                    out.write(NativeOutcome {
                        value: NIL,
                        exit: NativeExit::Transfer,
                    });
                }
                return;
            }
        }
    }
    // Publication may change linkage or collect. Reload the traced callable
    // from the owning frame and revalidate before committing any child state.
    if !unsafe { recursive_target_matches(context, symbol) } {
        unsafe {
            c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out);
        }
        return;
    }
    let context = unsafe { &mut *context };
    let body = unsafe { &*context.body };
    // This recursive entry bypasses run_activation's checked ingress. Validate
    // the retained body's contract before allocating or publishing a child.
    for (index, ty) in body.param_types.iter().copied().enumerate() {
        let value = unsafe { request.call.args.add(index).read() };
        if let Err(error) = validate_declared_value(ty, value) {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
            unsafe {
                out.write(NativeOutcome {
                    value: NIL,
                    exit: NativeExit::Transfer,
                });
            }
            return;
        }
    }
    let function = unsafe { (*context.frame).function };
    let stack = egcl_rt::current_stack();
    let Some(frame) = stack.push_frame(
        function,
        unsafe { (*context.frame).code_info },
        context.slots as u16,
        FLAG_CALL,
    ) else {
        unsafe {
            c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out);
        }
        return;
    };
    for (index, (_, location)) in body.param_layout.iter().enumerate() {
        let VarLoc::Slot(slot) = location else {
            unreachable!("admitted slot parameter")
        };
        unsafe {
            slot_set(frame, *slot, request.call.args.add(index).read());
        }
    }
    request.activation = RecursiveActivation {
        previous: context.recursive,
        frame,
        stack,
    };
    context.recursive = std::ptr::from_mut(&mut request.activation);
    #[cfg(test)]
    RECURSIVE_ENTRIES.with(|count| count.set(count.get() + 1));
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
    if context.recursive_profile {
        egcl_rt::function::record_invocation(function);
    }
    c2i_clear_mv();
    // The untagged activation address goes straight to the generated entry.
    unsafe {
        out.write(NativeOutcome {
            value: EgclVal(frame.add(1) as u64),
            exit: NativeExit::Returned,
        });
    }
}

/// Nonallocating retirement shared by normal returns and cold chain exits.
unsafe fn retire_recursive(context: &mut CaptureContext, activation: *mut RecursiveActivation) {
    let record = unsafe { &*activation };
    assert_eq!(context.recursive, activation);
    // Preparation stores the exact stack that owns this live frame. A fiber's
    // Arc-owned Lisp stack stays put when its carrier changes, and retirement
    // cannot yield or collect. Do not cache the carrier's execution context.
    let stack = unsafe { &*record.stack };
    debug_assert!(std::ptr::eq(stack, egcl_rt::current_stack()));
    assert_eq!(stack.fp(), record.frame);
    stack.pop_frame();
    context.recursive = record.previous;
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() - 1));
}

unsafe extern "C" fn finish_recursive(activation: *mut RecursiveActivation) {
    #[cfg(test)]
    RECURSIVE_HELPER_CALLS.with(|count| {
        let (prepare, finish) = count.get();
        count.set((prepare, finish + 1));
    });
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    unsafe {
        retire_recursive(context, activation);
    }
}

struct CapturePause(*mut CaptureContext);
impl Drop for CapturePause {
    fn drop(&mut self) {
        CAPTURE.with(|slot| slot.set(self.0));
    }
}

/// The actuals of one native entry, copied so the collector can update them,
/// without a heap allocation for common arities.
struct EntryArgs {
    inline: [EgclVal; crate::cli::ROOTED_ARGS_INLINE],
    len: usize,
    spill: Vec<EgclVal>,
}

impl EntryArgs {
    fn new(args: &[EgclVal]) -> Self {
        let mut inline = [NIL; crate::cli::ROOTED_ARGS_INLINE];
        let spill = if args.len() <= inline.len() {
            inline[..args.len()].copy_from_slice(args);
            Vec::new()
        } else {
            args.to_vec()
        };
        Self {
            inline,
            len: args.len(),
            spill,
        }
    }

    /// The actuals `first` followed by `rest`, as one argument list.
    pub(super) fn from_parts(first: &[EgclVal], rest: &[EgclVal]) -> Self {
        let len = first.len() + rest.len();
        let mut inline = [NIL; crate::cli::ROOTED_ARGS_INLINE];
        let spill = if len <= inline.len() {
            inline[..first.len()].copy_from_slice(first);
            inline[first.len()..len].copy_from_slice(rest);
            Vec::new()
        } else {
            let mut spill = Vec::with_capacity(len);
            spill.extend_from_slice(first);
            spill.extend_from_slice(rest);
            spill
        };
        Self { inline, len, spill }
    }

    pub(super) fn as_slice(&self) -> &[EgclVal] {
        if self.len <= self.inline.len() {
            &self.inline[..self.len]
        } else {
            &self.spill
        }
    }
}

impl egcl_rt::gc::TraceHostRoots for EntryArgs {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        let len = self.len.min(self.inline.len());
        for value in &mut self.inline[..len] {
            visit(value);
        }
        for value in &mut self.spill {
            visit(value);
        }
    }
}

struct FaultRecoveryGuard {
    null: usize,
    stack: usize,
}

/// Clear this thread's legacy fault-recovery IPs for a native entry, and
/// restore them when the guard drops. Nearly every entry finds both clear
/// already, and then has nothing to save or restore.
fn clear_fault_recovery() -> Option<FaultRecoveryGuard> {
    match egcl_rt::runtime::current_sigsegv_recovery_ips() {
        (0, 0) => None,
        (null, stack) => {
            egcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
            Some(FaultRecoveryGuard { null, stack })
        }
    }
}
impl Drop for FaultRecoveryGuard {
    fn drop(&mut self) {
        egcl_rt::runtime::set_sigsegv_recovery_ips(self.null, self.stack);
    }
}

struct FrameGuard(Option<*mut Frame>);
impl Drop for FrameGuard {
    fn drop(&mut self) {
        if let Some(frame) = self.0 {
            let stack = egcl_rt::current_stack();
            debug_assert_eq!(stack.fp(), frame);
            stack.pop_frame();
        }
    }
}

struct SavedCleanup {
    cleanup_bcp: u32,
    continuation: CleanupCont,
}

struct SavedHandler {
    push_bcp: u32,
    clauses: Vec<RuntimeClause>,
    cluster_base: usize,
    cluster_frame: *mut Frame,
}

struct SavedHandlerBind {
    push_bcp: u32,
    cluster_base: usize,
    cluster_frame: *mut Frame,
}

struct SavedRestartCase {
    push_bcp: u32,
    restart_base: usize,
    resume_bcp: u32,
    sp_restore: u16,
    cluster_frame: *mut Frame,
}

enum DynamicScope {
    HandlerCase {
        cluster_base: usize,
        cluster_frame: *mut Frame,
    },
    HandlerBind {
        cluster_base: usize,
        cluster_frame: *mut Frame,
    },
    RestartCase {
        restart_base: usize,
        cluster_frame: *mut Frame,
    },
}

struct DynamicScopeGuard {
    env: *mut Env,
    scopes: *mut Vec<DynamicScope>,
}
impl Drop for DynamicScopeGuard {
    fn drop(&mut self) {
        let scopes = unsafe { &mut *self.scopes };
        // Most activations establish no dynamic scope: skip the stack lookup.
        if scopes.is_empty() {
            return;
        }
        let stack = egcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        while let Some(scope) = scopes.pop() {
            match scope {
                DynamicScope::HandlerCase {
                    cluster_base,
                    cluster_frame,
                }
                | DynamicScope::HandlerBind {
                    cluster_base,
                    cluster_frame,
                } => {
                    env.handlers.truncate(cluster_base);
                    pop_condition_cluster_frame(stack, cluster_frame);
                }
                DynamicScope::RestartCase {
                    restart_base,
                    cluster_frame,
                } => {
                    env.restarts.truncate(restart_base);
                    pop_condition_cluster_frame(stack, cluster_frame);
                }
            }
        }
    }
}

struct HandlerScopeGuard {
    env: *mut Env,
    handlers: *mut Vec<SavedHandler>,
}
impl Drop for HandlerScopeGuard {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let handlers = unsafe { &mut *self.handlers };
        while let Some(saved) = handlers.pop() {
            env.handlers.truncate(saved.cluster_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct HandlerBindScopeGuard {
    env: *mut Env,
    handler_binds: *mut Vec<SavedHandlerBind>,
}
impl Drop for HandlerBindScopeGuard {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let binds = unsafe { &mut *self.handler_binds };
        while let Some(saved) = binds.pop() {
            env.handlers.truncate(saved.cluster_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct RestartCaseScopeGuard {
    env: *mut Env,
    restart_cases: *mut Vec<SavedRestartCase>,
}
impl Drop for RestartCaseScopeGuard {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let cases = unsafe { &mut *self.restart_cases };
        while let Some(saved) = cases.pop() {
            env.restarts.truncate(saved.restart_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct PreparedHandler {
    push_bcp: u32,
    clause_index: u32,
    token: String,
    payload: ControlPayload,
}
impl egcl_rt::gc::TraceHostRoots for PreparedHandler {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.payload.trace_host_roots(visit);
    }
}

unsafe extern "C" fn deliver_handler(
    push_bcp: u32,
    clause_index: u32,
    _unused: EgclVal,
) -> EgclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let prepared = unsafe { &mut *context.prepared_handler }
        .take()
        .expect("prepared native handler payload");
    assert_eq!(prepared.push_bcp, push_bcp);
    assert_eq!(prepared.clause_index, clause_index);
    prepared.payload.restore();
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    env.clear_mv();
    #[cfg(test)]
    NATIVE_HANDLER_COUNT.with(|count| count.set(count.get() + 1));
    super::super::take_control_value(&prepared.token)
}

/// Ownership passed between machine versions of one managed activation.
/// Catch tags stay in Env; their registration tokens are never regenerated.
struct OsrResume {
    frame: *mut Frame,
    offset: usize,
    state: Box<ActivationState>,
}

struct SavedCatch {
    push_bcp: u32,
    token: String,
}

struct PreparedCatch {
    push_bcp: u32,
    resume_bcp: u32,
    token: String,
    payload: ControlPayload,
}
impl egcl_rt::gc::TraceHostRoots for PreparedCatch {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.payload.trace_host_roots(visit);
    }
}

// Noncollecting: the prepared payload is rooted until this helper consumes it,
// and no Lisp allocation or suspension occurs before the returned primary is
// stored in its generated-code home.
unsafe extern "C" fn deliver_catch(push_bcp: u32, resume_bcp: u32, _unused: EgclVal) -> EgclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let prepared = unsafe { &mut *context.prepared_catch }
        .take()
        .expect("prepared native catch payload");
    assert_eq!(prepared.push_bcp, push_bcp);
    assert_eq!(prepared.resume_bcp, resume_bcp);
    prepared.payload.restore();
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let primary = super::super::take_control_mv(&prepared.token, env);
    #[cfg(test)]
    NATIVE_CATCH_COUNT.with(|count| count.set(count.get() + 1));
    primary
}

struct CatchScopeGuard {
    env: *mut Env,
    base: usize,
}
impl Drop for CatchScopeGuard {
    fn drop(&mut self) {
        unsafe {
            (*self.env).catch_stack.truncate(self.base);
        }
    }
}
impl egcl_rt::gc::TraceHostRoots for SavedCleanup {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.continuation.trace_host_roots(visit);
    }
}

// These helpers never execute Lisp, collect, yield or signal. Rust allocation
// of the MV copy retains the current runtime's host-allocation policy; emergency
// storage exhaustion is still an installation gate for the entire opt-in ABI.
unsafe extern "C" fn save_cleanup(cleanup_bcp: u32, resume_bcp: u32, value: EgclVal) -> EgclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    let cleanups = unsafe { &mut *context.cleanups };
    let depths = unsafe { &*context.cleanup_depths };
    // Capacity was reserved before native entry; no continuation-stack growth
    // occurs inside the helper. Nested function entries own separate stacks.
    assert!(cleanups.len() < cleanups.capacity());
    // Do not use `Vec::clone` after native entry. Its infallible allocation
    // could abort before the storage-condition route gets a chance to run.
    // Reserve first, then copy into the exact capacity; a failed reserve is
    // reported through the execution-local error slot and the generated
    // helper chain will leave native code on its next transfer boundary.
    let values = if env.mv_active {
        let mut values = Vec::new();
        if values.try_reserve(env.mv.len()).is_err() {
            // Install a resumable propagation continuation instead of leaving
            // `complete_cleanup` with no saved record. The completion helper
            // will return through the normal transfer machinery, which turns
            // this pending OOM into the preallocated storage-condition path.
            cleanups.push(SavedCleanup {
                cleanup_bcp,
                continuation: CleanupCont {
                    handler_depth: depths[&cleanup_bcp],
                    action: CleanupAction::Resume {
                        pending: Pending::Propagate(EgclError::Oom),
                        payload: ControlPayload::default(),
                    },
                },
            });
            return NIL;
        }
        values.extend_from_slice(&env.mv);
        Some(values)
    } else {
        None
    };
    cleanups.push(SavedCleanup {
        cleanup_bcp,
        continuation: CleanupCont {
            handler_depth: depths[&cleanup_bcp],
            action: CleanupAction::Normal {
                resume_bcp,
                value,
                values,
            },
        },
    });
    NIL
}

/// Successful lexical reads/writes neither allocate Lisp objects nor reenter
/// Lisp. Their own activation already roots its environment and the machine
/// caller retains every live home. Avoid publishing a second boundary only
/// while there is no request that needs that boundary to park or signal.
unsafe fn lexical_environment_leaf(
    request: *mut u8,
    image: *mut SysvTransferCapture,
) -> Option<EgclVal> {
    use egcl_compiler::t2::emit::{TRANSFER_LEXICAL_ENVIRONMENT_REQUEST, TransferCallRequest};
    let call = unsafe { &*request.cast::<TransferCallRequest>() };
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if call.symbol & !u64::from(u32::MAX) != TRANSFER_LEXICAL_ENVIRONMENT_REQUEST
        || !*ENABLED.get_or_init(|| std::env::var_os("EGCL_LEXICAL_LEAF").is_none_or(|v| v != "0"))
        || !current_poll_idle()
    {
        return None;
    }
    let context = unsafe { CAPTURE.with(Cell::get).as_ref() }?;
    if context.selected.is_some()
        || context.failure.is_some()
        || context.osr.is_some()
        || context.recursive_escape
        || context.owner.is_null()
        || context.env_frame.is_null()
    {
        return None;
    }
    let owner = unsafe { &*context.owner };
    let pc = unsafe { (*image).return_pc } as usize;
    if !pc
        .checked_sub(owner.code.as_ptr() as usize)
        .is_some_and(|offset| {
            offset < owner.code_len && owner._native_calls.value_map(offset).is_some()
        })
        || !std::ptr::eq(context.body, Arc::as_ptr(&owner.body))
    {
        // A direct child can belong to a different retained code owner. Its
        // publication resolves the exact physical owner before inspecting state.
        return None;
    }
    let instruction = owner.body.code.get(call.symbol as u32 as usize)?;
    let frame = unsafe { &*context.env_frame }.as_ref()?;
    let (index, store) = match instruction {
        Instr::LoadEnvVar(index) if call.nargs == 0 => (*index, false),
        Instr::StoreEnvVar(index) if call.nargs == 1 && !call.args.is_null() => (*index, true),
        _ => return None,
    };
    let name = owner.body.names.get(usize::from(index))?;
    let value = if lexical_identity::encoded(name) {
        if store {
            let value = unsafe { *call.args };
            if !Env::set_lexical_frame(frame, name, value) {
                return None;
            }
            NIL
        } else {
            Env::lookup_lexical_frame(frame, name)?
        }
    } else {
        // Preserve the resolved ordinary-name fast path. Encoded identity keys
        // must never be treated as names to intern or resolve in that registry.
        let symbol = owner.leaf_symbol(usize::from(index));
        if store {
            let value = unsafe { *call.args };
            if !Env::set_resolved_frame_var(frame, name, symbol, value) {
                return None;
            }
            NIL
        } else {
            // Missing bindings must signal with the full published native stack.
            Env::lookup_resolved_frame(frame, name, symbol)?
        }
    };
    #[cfg(test)]
    lexical_leaf_tests::record_hit(store);
    Some(value)
}

unsafe extern "C" fn call_or_throw(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    #[cfg(test)]
    lexical_leaf_tests::before_request(request);
    if let Some(value) = unsafe { lexical_environment_leaf(request, image) } {
        unsafe {
            out.write(NativeOutcome {
                value,
                exit: NativeExit::Returned,
            });
        }
        return;
    }
    unsafe {
        root_publication::published(image, || {
            use egcl_compiler::t2::emit::{
                TRANSFER_LEXICAL_ENVIRONMENT_REQUEST, TransferCallRequest,
            };
            let kind = (*request.cast::<TransferCallRequest>()).symbol & !u64::from(u32::MAX);
            if kind == TRANSFER_LEXICAL_ENVIRONMENT_REQUEST {
                #[cfg(test)]
                lexical_leaf_tests::record_published(request);
                // Calls can be the compiler's cooperation points. Service any
                // armed request while every native frame is precisely visible.
                if !current_poll_idle() {
                    poll_or_transfer_unpublished(std::ptr::null_mut(), out);
                    if (*out).exit != NativeExit::Returned {
                        return;
                    }
                }
                #[cfg(test)]
                lexical_leaf_tests::inside_published(request);
            }
            call_or_throw_unpublished(request, out)
        })
    }
}

#[cfg(test)]
mod lexical_leaf_tests;

/// Run the captured-variable instruction at the request's PC against the
/// executing activation's heap EnvFrame, exactly as the interpreter's arm for
/// it does (bliss-shih7.27). The frame lives in the rooted activation state;
/// take an Arc out of it before anything allocates, and hold no reference into
/// the state while the collector may trace it.
unsafe fn lexical_environment_request(
    call: &egcl_compiler::t2::emit::TransferCallRequest,
) -> Result<EgclVal, EgclError> {
    let context = unsafe { &*CAPTURE.with(Cell::get) };
    let body = unsafe { &*context.body };
    let instruction = *body
        .code
        .get(call.symbol as u32 as usize)
        .ok_or_else(invalid_capture)?;
    let name = |index: u16| {
        body.names
            .get(usize::from(index))
            .ok_or_else(invalid_capture)
    };
    let current = || unsafe { (*context.env_frame).clone() }.ok_or_else(invalid_capture);
    match instruction {
        Instr::LoadEnvVar(index) => {
            assert_eq!(call.nargs, 0);
            let name = name(index)?;
            Env::lookup_lexical_frame(&current()?, name).ok_or_else(|| {
                EgclError::UnboundVariable(lexical_identity::resolve(name).unwrap_or(NIL))
            })
        }
        Instr::StoreEnvVar(index) | Instr::DefineEnvVar(index) => {
            assert_eq!(call.nargs, 1);
            let (name, frame) = (name(index)?, current()?);
            // The argument area stays rooted; neither store allocates Lisp.
            let value = unsafe { *call.args };
            if matches!(instruction, Instr::StoreEnvVar(_)) {
                Env::set_lexical_frame(&frame, name, value);
            } else {
                bind_boxed_param(&frame, name, value);
            }
            Ok(NIL)
        }
        Instr::PushEnvChild => {
            push_env_child(unsafe { &mut *context.env_frame });
            Ok(NIL)
        }
        Instr::PopEnvChild => {
            pop_env_child(unsafe { &mut *context.env_frame });
            Ok(NIL)
        }
        Instr::MakeClosure {
            func,
            capture_env: true,
        } => {
            let nested = Arc::clone(
                body.nested_functions
                    .get(func as usize)
                    .ok_or_else(invalid_capture)?,
            );
            let captured = unsafe { (*context.env_frame).clone() };
            // As for a non-capturing closure: nothing allocates between its
            // creation and the outcome write.
            let closure = super::make_template_closure(&nested, captured, unsafe {
                &*NATIVE_ENV.with(Cell::get)
            });
            Ok(closure)
        }
        _ => Err(invalid_capture()),
    }
}

unsafe fn call_or_throw_unpublished(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
) {
    use egcl_compiler::t2::emit::{
        TRANSFER_CATCH_ENTER_REQUEST, TRANSFER_CATCH_LEAVE_REQUEST, TRANSFER_THROW_REQUEST,
        TransferCallRequest,
    };
    use egcl_rt::native_transfer::NativeOutcome;
    let call = unsafe { &*request.cast::<TransferCallRequest>() };
    let request_kind = call.symbol & !u64::from(u32::MAX);
    use egcl_compiler::t2::emit::{
        TRANSFER_DIRECT_EXIT_REQUEST, TRANSFER_GLOBAL_LOAD_REQUEST, TRANSFER_GLOBAL_STORE_REQUEST,
        TRANSFER_SPECIAL_BIND_REQUEST, TRANSFER_SPECIAL_UNBIND_REQUEST,
    };
    if request_kind == TRANSFER_DIRECT_EXIT_REQUEST {
        let result = if native_error_pending() {
            Ok(NIL)
        } else {
            guard_c2i(|| unsafe { local_exits::request(call) })
        };
        if let Err(error) = result {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if matches!(
        request_kind,
        TRANSFER_SPECIAL_BIND_REQUEST
            | TRANSFER_SPECIAL_UNBIND_REQUEST
            | TRANSFER_GLOBAL_LOAD_REQUEST
            | TRANSFER_GLOBAL_STORE_REQUEST
    ) {
        let result = if native_error_pending() {
            Ok(NIL)
        } else {
            guard_c2i(|| unsafe { special_request(call) })
        };
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
                NIL
            }
        };
        unsafe {
            out.write(NativeOutcome {
                value,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    use egcl_compiler::t2::emit::{
        TRANSFER_FUNCTION_LOOKUP_REQUEST, TRANSFER_HANDLER_BIND_ENTER_REQUEST,
        TRANSFER_HANDLER_BIND_LEAVE_REQUEST, TRANSFER_HANDLER_ENTER_REQUEST,
        TRANSFER_HANDLER_LEAVE_REQUEST, TRANSFER_HOST_EVAL_REQUEST,
        TRANSFER_LEXICAL_ENVIRONMENT_REQUEST, TRANSFER_MAKE_CLOSURE_REQUEST,
        TRANSFER_RESTART_CASE_ENTER_REQUEST, TRANSFER_RESTART_CASE_LEAVE_REQUEST,
        TRANSFER_VALUES_TO_LIST_REQUEST,
    };
    if request_kind == TRANSFER_LEXICAL_ENVIRONMENT_REQUEST {
        let mut value = NIL;
        if !native_error_pending() {
            match guard_c2i(|| unsafe { lexical_environment_request(call) }) {
                Ok(result) => value = result,
                Err(error) => NATIVE_ERROR.with(|slot| slot.set_first(error)),
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if request_kind == TRANSFER_VALUES_TO_LIST_REQUEST {
        // Same result as the T0 arm: the primary alone when no tuple is
        // active. args[0] lives in the mapped outgoing area; copy it into a
        // rooted Vec before vec_to_list allocates (bliss-6z1hh).
        let mut value = NIL;
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 1);
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let primary = unsafe { *call.args };
                egcl_rt::rooted!(
                    vals = if env.mv_active {
                        env.mv.clone()
                    } else {
                        vec![primary]
                    }
                );
                let list = vec_to_list(&vals);
                env.clear_mv();
                Ok(list)
            });
            match result {
                Ok(result) => value = result,
                Err(error) => NATIVE_ERROR.with(|slot| slot.set_first(error)),
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if matches!(
        request_kind,
        TRANSFER_HOST_EVAL_REQUEST
            | TRANSFER_FUNCTION_LOOKUP_REQUEST
            | TRANSFER_MAKE_CLOSURE_REQUEST
    ) {
        let mut value = NIL;
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 0);
                let context = unsafe { &*CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                if request_kind == TRANSFER_HOST_EVAL_REQUEST {
                    let body = unsafe { &*context.body };
                    let form = *body
                        .constants
                        .get(call.symbol as u32 as usize)
                        .ok_or_else(invalid_capture)?;
                    eval_form(form, env)
                } else if request_kind == TRANSFER_MAKE_CLOSURE_REQUEST {
                    // Index the executing activation's retained body, never a
                    // template pointer baked into code. Clone the Arc out before
                    // allocating; the closure is not rooted, so nothing may
                    // allocate between its creation and the outcome write.
                    let body = unsafe { &*context.body };
                    let nested = Arc::clone(
                        body.nested_functions
                            .get(call.symbol as u32 as usize)
                            .ok_or_else(invalid_capture)?,
                    );
                    let closure = super::make_template_closure(&nested, None, env);
                    Ok(closure)
                } else {
                    let symbol = EgclVal::from_symbol_index(call.symbol as u32);
                    super::super::symbol_function_object(env, symbol)
                        .ok_or_else(|| super::undefined_function_lookup_error(env, symbol))
                }
            });
            match result {
                Ok(result) => value = result,
                Err(error) => NATIVE_ERROR.with(|slot| slot.set_first(error)),
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if matches!(
        request_kind,
        TRANSFER_HANDLER_BIND_ENTER_REQUEST
            | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
            | TRANSFER_RESTART_CASE_ENTER_REQUEST
            | TRANSFER_RESTART_CASE_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                if request_kind != TRANSFER_HANDLER_BIND_ENTER_REQUEST {
                    assert_eq!(call.nargs, 0);
                }
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                if matches!(
                    request_kind,
                    TRANSFER_HANDLER_BIND_ENTER_REQUEST | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
                ) {
                    let binds = unsafe { &mut *context.handler_binds };
                    let Some(instruction) = body.code.get(push_bcp as usize) else {
                        return Err(invalid_capture());
                    };
                    let Instr::PushHandlerBind { hb } = instruction else {
                        return Err(invalid_capture());
                    };
                    if request_kind == TRANSFER_HANDLER_BIND_ENTER_REQUEST {
                        assert!(binds.len() < binds.capacity());
                        let info = &body.handler_binds[*hb as usize];
                        assert_eq!(call.nargs, info.types.len());
                        let cluster_base = env.handlers.len();
                        let mut entries = Vec::new();
                        entries
                            .try_reserve(info.types.len())
                            .map_err(|_| EgclError::Oom)?;
                        let mut values = Vec::new();
                        values
                            .try_reserve(info.types.len().saturating_mul(2))
                            .map_err(|_| EgclError::Oom)?;
                        egcl_rt::rooted_ref!(_entries = &mut entries);
                        egcl_rt::rooted_ref!(_values = &mut values);
                        for (index, type_name) in info.types.iter().enumerate() {
                            let type_symbol = resolve_sym(type_name).ok_or_else(invalid_capture)?;
                            // The native call's argument area remains rooted;
                            // reread after interning, which can relocate values.
                            let handler_value = unsafe { *call.args.add(index) };
                            entries.push(HandlerEntry {
                                type_name: try_clone_string(type_name)?,
                                compound_type: NIL,
                                handler: HandlerImpl::Function(handler_value),
                            });
                            values.push(type_symbol);
                            values.push(handler_value);
                        }
                        let cluster_frame =
                            push_condition_cluster_frame(egcl_rt::current_stack(), &values)?;
                        env.handlers.push(HandlerCluster { entries });
                        binds.push(SavedHandlerBind {
                            push_bcp,
                            cluster_base,
                            cluster_frame,
                        });
                        unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::HandlerBind {
                            cluster_base,
                            cluster_frame,
                        });
                    } else {
                        let saved = binds.pop().expect("live native handler-bind");
                        assert_eq!(saved.push_bcp, push_bcp);
                        let Some(DynamicScope::HandlerBind {
                            cluster_base: live_base,
                            cluster_frame: live_frame,
                        }) = unsafe { &mut *context.dynamic_scopes }.pop()
                        else {
                            return Err(invalid_capture());
                        };
                        assert_eq!(
                            (live_base, live_frame),
                            (saved.cluster_base, saved.cluster_frame)
                        );
                        env.handlers.truncate(saved.cluster_base);
                        pop_condition_cluster_frame(egcl_rt::current_stack(), saved.cluster_frame);
                    }
                } else {
                    let cases = unsafe { &mut *context.restart_cases };
                    let Some(instruction) = body.code.get(push_bcp as usize) else {
                        return Err(invalid_capture());
                    };
                    let Instr::PushRestartCase {
                        rc,
                        resume_bcp,
                        sp_restore,
                    } = instruction
                    else {
                        return Err(invalid_capture());
                    };
                    if request_kind == TRANSFER_RESTART_CASE_ENTER_REQUEST {
                        assert!(cases.len() < cases.capacity());
                        let info = &body.restart_cases[*rc as usize];
                        let restart_base = env.restarts.len();
                        // As T0: restart clauses see the activation's
                        // captured variables when it has any.
                        let captured_frame = unsafe { (*context.env_frame).clone() }
                            .unwrap_or_else(|| Arc::clone(&env.frame));
                        let mut values = Vec::new();
                        values
                            .try_reserve(info.restarts.len().saturating_mul(5))
                            .map_err(|_| EgclError::Oom)?;
                        egcl_rt::rooted_ref!(_values = &mut values);
                        // Finish every fallible string/stack copy before
                        // mutating the live restart stack. A later OOM then
                        // cannot leave a partially registered restart-case.
                        let mut prepared = Vec::new();
                        prepared
                            .try_reserve(info.restarts.len())
                            .map_err(|_| EgclError::Oom)?;
                        for restart in &info.restarts {
                            prepared.push((
                                try_clone_string(&restart.name)?,
                                try_clone_control_stack(&env.block_stack)?,
                                try_clone_control_stack(&env.tag_stack)?,
                            ));
                        }
                        let mut restart_symbols = Vec::new();
                        restart_symbols
                            .try_reserve(info.restarts.len())
                            .map_err(|_| EgclError::Oom)?;
                        for restart in &info.restarts {
                            restart_symbols
                                .push(resolve_sym(&restart.name).ok_or_else(invalid_capture)?);
                        }
                        let templates = unsafe { &*context.restart_templates };
                        let Some(function_templates) = templates.0.get(*rc as usize) else {
                            return Err(invalid_capture());
                        };
                        if function_templates.len() != info.restarts.len() {
                            return Err(invalid_capture());
                        }
                        for (
                            (_index, ((_restart, (name, captured_blocks, captured_tags)), symbol)),
                            function,
                        ) in info
                            .restarts
                            .iter()
                            .zip(prepared)
                            .zip(restart_symbols)
                            .enumerate()
                            .zip(function_templates)
                        {
                            env.restarts.push(RestartEntry {
                                name,
                                captured_blocks,
                                captured_tags,
                                function: RestartFunction::Bytecode {
                                    function: Rc::clone(function),
                                    captured_frame: Arc::clone(&captured_frame),
                                },
                                interactive_function: None,
                                test_function: None,
                                unwind_on_invoke: true,
                                group_base: restart_base,
                                id: super::super::next_restart_id(),
                                restart_obj: NIL,
                                report: NIL,
                            });
                            values.push(symbol);
                            values.extend([NIL, NIL, NIL, NIL]);
                        }
                        let cluster_frame =
                            push_condition_cluster_frame(egcl_rt::current_stack(), &values)?;
                        cases.push(SavedRestartCase {
                            push_bcp,
                            restart_base,
                            resume_bcp: *resume_bcp,
                            sp_restore: *sp_restore,
                            cluster_frame,
                        });
                        unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::RestartCase {
                            restart_base,
                            cluster_frame,
                        });
                    } else {
                        let saved = cases.pop().expect("live native restart-case");
                        assert_eq!(saved.push_bcp, push_bcp);
                        let Some(DynamicScope::RestartCase {
                            restart_base: live_base,
                            cluster_frame: live_frame,
                        }) = unsafe { &mut *context.dynamic_scopes }.pop()
                        else {
                            return Err(invalid_capture());
                        };
                        assert_eq!(
                            (live_base, live_frame),
                            (saved.restart_base, saved.cluster_frame)
                        );
                        env.restarts.truncate(saved.restart_base);
                        pop_condition_cluster_frame(egcl_rt::current_stack(), saved.cluster_frame);
                    }
                }
                Ok(NIL)
            });
            if let Err(error) = result {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if matches!(
        request_kind,
        TRANSFER_HANDLER_ENTER_REQUEST
            | TRANSFER_HANDLER_LEAVE_REQUEST
            | TRANSFER_HANDLER_BIND_ENTER_REQUEST
            | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
            | TRANSFER_RESTART_CASE_ENTER_REQUEST
            | TRANSFER_RESTART_CASE_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 0);
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let handlers = unsafe { &mut *context.handlers };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                let Some(Instr::PushHandlerCase { hc, .. }) = body.code.get(push_bcp as usize)
                else {
                    return Err(invalid_capture());
                };
                if request_kind == TRANSFER_HANDLER_ENTER_REQUEST {
                    assert!(handlers.len() < handlers.capacity());
                    let info = &body.handler_cases[*hc as usize];
                    let cluster_base = env.handlers.len();
                    let mut clauses = Vec::new();
                    clauses
                        .try_reserve(info.clauses.len())
                        .map_err(|_| EgclError::Oom)?;
                    let mut entries = Vec::new();
                    entries
                        .try_reserve(info.clauses.len())
                        .map_err(|_| EgclError::Oom)?;
                    let mut values = Vec::new();
                    values
                        .try_reserve(info.clauses.len().saturating_mul(2))
                        .map_err(|_| EgclError::Oom)?;
                    egcl_rt::rooted_ref!(_values = &mut values);
                    for clause in &info.clauses {
                        let token = try_control_token("__HANDLER_CASE__")?;
                        entries.push(HandlerEntry {
                            type_name: try_clone_string(&clause.type_name)?,
                            compound_type: NIL,
                            handler: HandlerImpl::HandlerCase {
                                token: try_clone_string(&token)?,
                                var_name: None,
                                body: NIL,
                                captured_frame: Arc::clone(&env.frame),
                            },
                        });
                        clauses.push(RuntimeClause {
                            token,
                            type_name: try_clone_string(&clause.type_name)?,
                            body_bcp: clause.body_bcp,
                            var_slot: clause.var_slot,
                        });
                        values.push(resolve_sym(&clause.type_name).ok_or_else(invalid_capture)?);
                        values.push(NIL);
                    }
                    let cluster_frame =
                        push_condition_cluster_frame(egcl_rt::current_stack(), &values)?;
                    env.handlers.push(HandlerCluster { entries });
                    handlers.push(SavedHandler {
                        push_bcp,
                        clauses,
                        cluster_base,
                        cluster_frame,
                    });
                    unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::HandlerCase {
                        cluster_base,
                        cluster_frame,
                    });
                } else {
                    let saved = handlers.pop().expect("live native handler cluster");
                    assert_eq!(saved.push_bcp, push_bcp);
                    let Some(DynamicScope::HandlerCase {
                        cluster_base: live_base,
                        cluster_frame: live_frame,
                    }) = unsafe { &mut *context.dynamic_scopes }.pop()
                    else {
                        return Err(invalid_capture());
                    };
                    assert_eq!(
                        (live_base, live_frame),
                        (saved.cluster_base, saved.cluster_frame)
                    );
                    env.handlers.truncate(saved.cluster_base);
                    pop_condition_cluster_frame(egcl_rt::current_stack(), saved.cluster_frame);
                }
                Ok(NIL)
            });
            if let Err(error) = result {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }

    if matches!(
        request_kind,
        TRANSFER_CATCH_ENTER_REQUEST | TRANSFER_CATCH_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let catches = unsafe { &mut *context.catches };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                assert!(matches!(
                    body.code.get(push_bcp as usize),
                    Some(Instr::PushCatch { .. })
                ));
                if request_kind == TRANSFER_CATCH_ENTER_REQUEST {
                    assert_eq!(call.nargs, 1);
                    assert!(catches.len() < catches.capacity());
                    let tag = unsafe { call.args.read() };
                    let token = try_control_token("__THROW__")?;
                    env.catch_stack.push((tag, try_clone_string(&token)?));
                    catches.push(SavedCatch { push_bcp, token });
                } else {
                    assert_eq!(call.nargs, 0);
                    let saved = catches.last().expect("live native catch");
                    assert_eq!(saved.push_bcp, push_bcp);
                    assert!(
                        env.catch_stack
                            .last()
                            .is_some_and(|(_, token)| token == &saved.token)
                    );
                    env.catch_stack.pop();
                    catches.pop();
                }
                Ok(NIL)
            });
            if let Err(error) = result {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if call.symbol != TRANSFER_THROW_REQUEST {
        unsafe {
            c2i_call_legacy_v2(request, out);
        }
        return;
    }
    if !native_error_pending() {
        let result = guard_c2i(|| {
            assert_eq!(call.nargs, 2);
            egcl_rt::rooted!(tag = unsafe { call.args.read() });
            egcl_rt::rooted!(value = unsafe { call.args.add(1).read() });
            let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
            let token = env
                .catch_stack
                .iter()
                .rev()
                .find(|(live_tag, _)| *live_tag == *tag)
                .map(|(_, token)| token.clone());
            match token {
                Some(token) => {
                    super::super::store_control_mv(&token, *value, env);
                    Err(EgclError::Internal(token))
                }
                None => Err(EgclError::ControlError(format!(
                    "attempt to THROW to a tag that is not active: {}",
                    val_as_str(*tag)
                ))),
            }
        });
        if let Err(error) = result {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
        }
    }
    unsafe {
        out.write(NativeOutcome {
            value: NIL,
            exit: NativeExit::Transfer,
        });
    }
}

/// Native loop-header poll used by the segment emitter. The poll may park for
/// a moving-GC rendezvous and may publish a pending signal/error, so it runs
/// behind the same rooted helper veneer as an exceptional call. A normal poll
/// returns a successful outcome; the veneer never performs a status check in
/// generated code. A nonzero result enters the existing capture/landing path.
///
/// T2 entry may return before publication when there is no pending work.
/// Baseline code uses `poll_or_transfer`; already-published Rust callers use
/// `poll_or_transfer_unpublished`.
/// Box a raw double for mapped T2 code (bliss-thcbb). The request word is the
/// double's IEEE bits, not a pointer. The allocation can collect, so the
/// calling frame is published for its extent; generated code records the exact
/// map of its live values at this call's return address.
unsafe extern "C" fn box_double(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    let bits = request as u64;
    let value = unsafe {
        root_publication::published(image, || {
            egcl_rt::gc::alloc_double_float(f64::from_bits(bits))
        })
    };
    // Nothing collects between the allocation and the veneer's return.
    unsafe {
        out.write(NativeOutcome {
            value,
            exit: NativeExit::Returned,
        })
    };
}

/// Install the process-lifetime box veneer used by raw double code
/// (bliss-thcbb). The helper always returns normally, so its cold entry, a
/// shared capture stub, is never taken; both buffers live for the process.
pub(super) fn install_box_double_veneer() {
    static VENEER: OnceLock<usize> = OnceLock::new();
    let address = *VENEER.get_or_init(|| {
        let Some(capture) = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8))
        else {
            return 0;
        };
        let Some(veneer) = JitBuffer::new(
            &egcl_compiler::t2::native_transfer::emit_published_helper_veneer(
                box_double,
                capture.as_ptr(),
            ),
        ) else {
            return 0;
        };
        let address = veneer.as_ptr() as usize;
        std::mem::forget(capture);
        std::mem::forget(veneer);
        address
    });
    egcl_compiler::t2::emit::install_box_double_veneer(address as u64);
}

unsafe extern "C" fn poll_t2_or_transfer(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    if idle_t2_poll(request) {
        #[cfg(test)]
        POLL_PATHS.with(|counts| {
            let (idle, published) = counts.get();
            counts.set((idle + 1, published));
        });
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: NativeExit::Returned,
            })
        };
        native_poll::refresh();
        return;
    }
    unsafe { poll_or_transfer(request, out, image) };
    native_poll::refresh();
}

unsafe extern "C" fn poll_or_transfer(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    #[cfg(test)]
    POLL_PATHS.with(|counts| {
        let (idle, published) = counts.get();
        counts.set((idle, published + 1));
    });
    unsafe {
        root_publication::published(image, || {
            #[cfg(test)]
            {
                root_publication::observe();
                native_poll::observe_request();
            }
            poll_or_transfer_unpublished(request, out);
            #[cfg(test)]
            root_publication::observe_after();
            if (*out).exit == NativeExit::Returned && prepare_osr(image) {
                (*out).exit = NativeExit::Transfer;
            }
        })
    }
}

/// A read-only preflight: all work that can park, yield, allocate, deliver a
/// signal, or prepare OSR stays behind the existing root publication.
fn idle_t2_poll(request: *mut u8) -> bool {
    if crate::cli::sprof::pending() {
        return false;
    }
    // Preserve loop heat and every T1 OSR opportunity, including test hooks
    // that deliberately collect only after the native homes are published.
    if request as usize & 1 != 0 {
        return false;
    }
    #[cfg(test)]
    if root_publication::OBSERVE.with(|hook| hook.get().is_some()) {
        return false;
    }
    // These queries neither initialize a carrier nor consume a notification.
    // A concurrent request after an idle read remains pending at the next
    // unchanged poll site, just as when it races an ordinary poll's read.
    !egcl_rt::thread::current_execution_poll_pending()
}

#[cfg(test)]
thread_local! {
    static POLL_PATHS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

#[cfg(test)]
pub(super) fn reset_poll_paths_for_test() {
    POLL_PATHS.with(|counts| counts.set((0, 0)));
}

#[cfg(test)]
pub(super) fn take_poll_paths_for_test() -> (usize, usize) {
    POLL_PATHS.with(|counts| counts.replace((0, 0)))
}

#[cfg(test)]
thread_local! {
    static FAIL_OSR_PREPARATION: Cell<bool> = const { Cell::new(false) };
    static FORCE_OSR_FRAME_GROWTH: Cell<u16> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn fail_next_osr_preparation_for_test() {
    FAIL_OSR_PREPARATION.with(|fail| fail.set(true));
}

#[cfg(test)]
pub(super) fn osr_preparation_failure_pending_for_test() -> bool {
    FAIL_OSR_PREPARATION.with(Cell::get)
}

#[cfg(test)]
pub(super) fn force_next_osr_frame_growth_for_test(extra: u16) {
    FORCE_OSR_FRAME_GROWTH.with(|slots| slots.set(extra));
}

/// Select only an exact, published mapped replacement at a mapped loop header.
/// Reconstruct only on this cold route, through the poll's exact native homes.
unsafe fn prepare_osr(image: *const SysvTransferCapture) -> bool {
    let context = CAPTURE.with(Cell::get);
    let Some(context) = (unsafe { context.as_mut() }) else {
        return false;
    };
    let code = unsafe { &*context.owner };
    if code.is_t2 || !context.recursive.is_null() {
        return false;
    }
    let Some(symbol) = code.installed_symbol else {
        return false;
    };
    let Some(offset) = (unsafe { (*image).return_pc as usize }).checked_sub(context.code_base)
    else {
        return false;
    };
    let Some(header) = code.osr_polls.get(&offset) else {
        return false;
    };
    let Some(next) = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())
    else {
        return false;
    };
    let running = unsafe { (*context.frame).function };
    if !next.is_t2
        || next
            .body
            .as_ref()
            .is_none_or(|body| !Arc::ptr_eq(body, &code.body))
        || !(recursive_definition_matches(symbol, Arc::as_ptr(&code.body))
            || closure_code_matches(symbol, running, Arc::as_ptr(&code.body)))
        || !matches!(&next._storage, NativeCodeStorage::Mapped(_))
    {
        return false;
    }
    let NativeCodeStorage::Mapped(destination) = &next._storage else {
        return false;
    };
    let destination = destination.osr_destination();
    let Some(entry) = destination.osr_entries.get(header).copied() else {
        return false;
    };
    // Scan body admission only once a replacement is ready, not on every
    // baseline loop poll while the optimizing worker is still compiling.
    if !local_control_native_body(&code.body, true, true, false) {
        return false;
    }
    // An OSR jump keeps this activation's registrations, not new copies.
    // Validate exact lexical provenance against every live runtime ledger.
    let Some(scopes) = code.control_scopes.before(*header) else {
        return false;
    };
    if scopes.iter().any(|scope| {
        scope.ownership != Ownership::Local
            || !matches!(
                scope.kind,
                ScopeKind::Block { register: false, .. }
            | ScopeKind::Tagbody { .. } | ScopeKind::Catch { .. }
            | ScopeKind::Unwind { .. } | ScopeKind::Cleanup { .. }
            | ScopeKind::HandlerCase { .. } | ScopeKind::HandlerBind { .. }
            | ScopeKind::RestartCase { .. } | ScopeKind::SpecialBinding { .. }
            // The state's EnvFrame moves with the OSR hand-off.
            | ScopeKind::LexicalEnvironment
            )
    }) {
        return false;
    }
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    if !unsafe {
        validate_condition_clusters(context, scopes, env)
            && validate_special_bindings(context, scopes)
    } {
        return false;
    }
    let cleanups = unsafe { &*context.cleanups };
    let mut saved_cleanups = cleanups.iter();
    let mut handler_depth = 0;
    for scope in scopes {
        if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
            let Some(saved) = saved_cleanups.next() else {
                return false;
            };
            if saved.cleanup_bcp != cleanup_bcp || saved.continuation.handler_depth != handler_depth
            {
                return false;
            }
        } else if !matches!(
            scope.kind,
            ScopeKind::SpecialBinding { .. } | ScopeKind::LexicalEnvironment
        ) {
            handler_depth += 1;
        }
    }
    if saved_cleanups.next().is_some() || context.completed_cleanup.is_some() {
        return false;
    }
    let catches = unsafe { &*context.catches };
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    if !scopes
        .iter()
        .filter(|scope| matches!(scope.kind, ScopeKind::Catch { .. }))
        .map(|scope| scope.push_bcp)
        .eq(catches.iter().map(|saved| saved.push_bcp))
        || env.catch_stack.len() != context.catch_base + catches.len()
        || !env.catch_stack[context.catch_base..]
            .iter()
            .map(|(_, token)| token)
            .eq(catches.iter().map(|saved| &saved.token))
    {
        return false;
    }
    let Some(index) = code
        .sites
        .sites()
        .position(|site| site.return_offset() as usize == offset)
    else {
        return false;
    };
    let snapshot = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index) };
    if unsafe {
        snapshot.capture_from_activation(
            context.code_base,
            &*image,
            std::slice::from_raw_parts(context.activation, context.slots),
        )
    }
    .is_err()
    {
        return false;
    }
    let Ok(frames) = snapshot.reconstruct(|_| unreachable!("tagged mapped OSR")) else {
        return false;
    };
    egcl_rt::rooted!(frames = frames);
    let [frame] = frames.as_slice() else {
        return false;
    };
    if frame.resume_pc != *header
        || frame.locals.len() != usize::from(code.body.n_locals)
        || !frame.stack.is_empty()
        || !destination.osr_imports_admit(*header, |slot| {
            frame.locals.get(usize::from(slot)).copied().unwrap_or(NIL)
        })
    {
        return false;
    }
    // All destination allocation remains on the source's published side. A
    // failed reservation simply leaves this T1 loop and its cleanups running.
    #[cfg(test)]
    if FAIL_OSR_PREPARATION.with(|fail| fail.replace(false)) {
        return false;
    }
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let Ok(prepared) = ActivationState::prepare(destination, env) else {
        return false;
    };
    let mut prepared = Box::new(prepared);
    egcl_rt::rooted_ref!(_prepared = &mut *prepared);
    let stack = egcl_rt::current_stack();
    if !native_transfer::is_supported() {
        return false;
    }
    let slots = destination
        .slots
        .max(unsafe { (*context.frame).num_locals });
    #[cfg(test)]
    let slots = {
        let extra = FORCE_OSR_FRAME_GROWTH.with(|slots| slots.replace(0));
        let Some(forced) = (unsafe { (*context.frame).num_locals }).checked_add(extra) else {
            return false;
        };
        slots.max(forced)
    };
    // Preserve the SOURCE CodeInfo through the remaining cold publication.
    // Callable homes keep their addresses; owned Special frames move as one
    // suffix and every ledger pointer is repaired before a safepoint.
    // Failure changes neither stack bounds nor its map. After success there
    // is no allocation, safepoint, or fallible step before the commit below.
    let Some(delta) =
        (unsafe { stack.grow_frame_with_special_suffix(context.frame, slots, code.code_info) })
    else {
        return false;
    };
    unsafe {
        relocate_condition_clusters(context, delta);
    }
    for (slot, value) in frame.locals.iter().enumerate() {
        unsafe { slot_set(context.frame, slot as u16, *value) };
    }
    // End the local root before moving the box; the source activation now
    // owns and traces the prepared state's exact payload slots.
    drop(_prepared);
    unsafe {
        *context.ready_osr = Some(prepared);
    }
    context.osr = Some((next, entry));
    true
}

/// Nothing for a poll to service: no stop-the-world or interrupt request, no
/// poll, yield or deadline armed for this execution, no process signal it has
/// not drained, no posted signal and no pending native error. With all of
/// these clear, an entry or helper poll is a no-op, so the native entries and
/// their helpers test this first (GitHub #487).
pub(in crate::cli::bytecode) fn entry_poll_idle(
    execution: &egcl_rt::exec_context::LispExecutionContext,
) -> bool {
    egcl_rt::safepoint::global_poll_idle()
        && !execution.poll_pending()
        && !execution.yield_requested()
        && !egcl_rt::runtime::process_signal_activity_pending()
        && !egcl_rt::any_posted_pending_signal()
        && !native_error_pending()
        && !egcl_rt::sandbox_cpu_deadline_armed()
}

/// [`entry_poll_idle`] for the current execution; `false` without one.
pub(in crate::cli::bytecode) fn current_poll_idle() -> bool {
    egcl_rt::thread::current_execution_context_for_native()
        .is_some_and(|execution| entry_poll_idle(unsafe { &*execution }))
}

/// Request the real execution-local deferred interrupt without an OS signal.
#[cfg(test)]
pub(in crate::cli::bytecode) fn request_interrupt_for_test() {
    egcl_rt::thread::post_current_pending_signal(egcl_rt::PendingSignal::Interrupt);
    egcl_rt::runtime::mark_process_signal_activity();
}

pub(in crate::cli::bytecode) unsafe fn poll_or_transfer_unpublished(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
) {
    use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
    // Most helper polls find nothing to do; skip the drain (GitHub #487).
    if request as usize & 1 == 0 && current_poll_idle() {
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: NativeExit::Returned,
            });
        }
        return;
    }
    // Clear before inspecting authoritative sources. An arrival during this
    // drain must remain armed; refreshing control snapshots must never clear it.
    egcl_rt::thread::begin_current_execution_poll();
    egcl_rt::safepoint::poll_safepoint();
    if request as usize & 1 != 0
        && let Err(error) = guard_c2i(|| {
            record_loop_heat();
            Ok(NIL)
        })
    {
        NATIVE_ERROR.with(|slot| slot.set_first(error));
    }
    // The scheduler validates every active segment before resuming a fiber,
    // including yields inside ordinary helpers. This poll handles only the
    // independent GC, preemption and asynchronous-transfer obligations.
    let exit = if super::native_loop_should_exit() != 0 {
        NativeExit::Transfer
    } else {
        NativeExit::Returned
    };
    egcl_rt::thread::rearm_current_execution_poll();
    if native_error_pending() {
        egcl_rt::thread::request_current_execution_poll();
    }
    unsafe {
        out.write(NativeOutcome { value: NIL, exit });
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static AFTER_HEAT_VALIDATION: Cell<Option<fn()>> = const { Cell::new(None) };
}

/// Attribute sampled loop visits only to the exact installed definition.
/// Publication may replace future entry points while this activation retains
/// its code; it cannot turn the running mapped frame into a legacy OSR frame.
fn record_loop_heat() {
    if profiling_disabled() {
        return;
    }
    let capture = CAPTURE.with(Cell::get);
    if capture.is_null() {
        return;
    }
    // A bridge has no managed callee frame; its installed T2 body needs no
    // lower-tier loop heat. The physical native maps own its callable.
    if unsafe { (*capture).ordinary_bridge } {
        return;
    }
    let (symbol, body, function) = unsafe {
        let code = &*(*capture).owner;
        (
            code.installed_symbol,
            Arc::as_ptr(&code.body),
            (*(*capture).frame).function,
        )
    };
    let Some(symbol) = symbol else {
        return;
    };
    // The published poll may have moved this callable. Retain its current
    // frame value, then validate one binding snapshot; a concurrent setter
    // cannot redirect this increment to a replacement or a nonfunction.
    egcl_rt::rooted!(function = function);
    // A closure template's code is installed under its owning instance's
    // private symbol, which has no function cell; any instance of that
    // template heats the shared code (bliss-6fim1).
    let installed = if egcl_rt::symbols::symbol_function(symbol) == Some(*function) {
        callable_definition_matches(symbol, *function, body)
    } else {
        closure_code_matches(symbol, *function, body)
    };
    if !installed {
        return;
    }
    #[cfg(test)]
    AFTER_HEAT_VALIDATION.with(|hook| {
        if let Some(observe) = hook.get() {
            observe();
        }
    });
    let count = egcl_rt::function::record_back_edges(*function, 1);
    let threshold = t2_backedge_threshold().max(1);
    if !t2_enabled() || count < threshold || count % threshold != 0 {
        return;
    }
    poll_t2_completions();
    if NATIVE_REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&symbol)
            .is_some_and(|code| code.is_t2)
    }) {
        return;
    }
    let priority = (u64::from(count) << 32) | u64::from(egcl_rt::function::invoke_count(*function));
    request_t2_compilation(symbol, priority);
    poll_t2_completions();
}

unsafe extern "C" fn complete_cleanup(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    unsafe { root_publication::published(image, || complete_cleanup_unpublished(request, out)) }
}

unsafe fn complete_cleanup_unpublished(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
) {
    use egcl_compiler::t2::emit::TransferCleanupRequest;
    use egcl_rt::native_transfer::NativeOutcome;
    let request = unsafe { &*request.cast::<TransferCleanupRequest>() };
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    assert_eq!(request.activation, context.activation);
    let cleanups = unsafe { &mut *context.cleanups };
    let saved = cleanups.last().expect("verified cleanup stack");
    assert_eq!(u64::from(saved.cleanup_bcp), request.cleanup_bcp);
    if matches!(saved.continuation.action, CleanupAction::Resume { .. }) {
        // Keep the continuation rooted and the pre-op stack intact until cold
        // capture has consumed its exact source map. Rust returns first.
        context.completed_cleanup = Some(saved.cleanup_bcp);
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            });
        }
        return;
    }
    let saved = cleanups.pop().expect("verified cleanup stack");
    let CleanupAction::Normal {
        resume_bcp,
        value,
        values,
    } = saved.continuation.action
    else {
        unreachable!("checked normal continuation")
    };
    assert_eq!(u64::from(resume_bcp), request.resume_bcp);
    match values {
        Some(values) => env.set_mv(values),
        None => env.clear_mv(),
    }
    unsafe {
        out.write(NativeOutcome {
            value,
            exit: NativeExit::Returned,
        });
    }
}

#[repr(C)]
struct DispatchPacket {
    entry: *const u8,
    request: *mut u8,
}

const _: () = {
    assert!(std::mem::size_of::<DispatchPacket>() == 16);
    assert!(std::mem::offset_of!(DispatchPacket, entry) == 0);
    assert!(std::mem::offset_of!(DispatchPacket, request) == 8);
};

#[cfg(test)]
static NATIVE_CLEANUP_COUNT: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_cleanup_count() -> usize {
    NATIVE_CLEANUP_COUNT.with(|count| count.replace(0))
}

#[cfg(test)]
static NATIVE_CATCH_COUNT: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
static NATIVE_FALLBACK_COUNT: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_catch_count() -> usize {
    NATIVE_CATCH_COUNT.with(|count| count.replace(0))
}
#[cfg(test)]
pub(super) fn take_native_fallback_count() -> usize {
    NATIVE_FALLBACK_COUNT.with(|count| count.replace(0))
}

#[cfg(test)]
static NATIVE_HANDLER_COUNT: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_handler_count() -> usize {
    NATIVE_HANDLER_COUNT.with(|count| count.replace(0))
}

#[repr(C)]
struct CaptureContext {
    // Explicit C machine prefix. No other Rust field is decoded by assembly.
    generated: *mut egcl_compiler::t2::native_transfer::GeneratedActivation,
    generated_owner: Option<Box<mapped_generated::Prepared>>,
    ordinary_bridge: bool,
    mapped_ordinary: *mut mapped_ordinary::Lanes,
    osr: Option<(Rc<NativeCode>, usize)>,
    ready_osr: *mut Option<Box<ActivationState>>,
    guard_resume: *mut Option<OuterResumeState>,
    children: *mut SegmentActivations,
    nested: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
    owner: *const TransferCode,
    completed_deopt: bool,
    recursive_enabled: bool,
    recursive_profile: bool,
    recursive_generation: u64,
    native_recursive: Option<egcl_compiler::t2::emit::NativeRecursiveControl>,
    native_poll: Option<egcl_compiler::t2::emit::NativePollControl>,
    recursive: *mut RecursiveActivation,
    recursive_escape: bool,
    handlers: *mut Vec<SavedHandler>,
    handler_binds: *mut Vec<SavedHandlerBind>,
    restart_cases: *mut Vec<SavedRestartCase>,
    restart_templates: *const RestartFunctionTemplates,
    dynamic_scopes: *mut Vec<DynamicScope>,
    cluster_frames: *mut Vec<(u32, *mut Frame)>,
    prepared_handler: *mut Option<PreparedHandler>,
    /// The owning `ActivationState::env_frame`.
    env_frame: *mut Option<Arc<SharedCell<EnvFrame>>>,
    #[cfg(test)]
    unavailable_catch: Option<u32>,
    #[cfg(test)]
    unavailable_handler: Option<(u32, u32)>,
    prepared_catch: *mut Option<PreparedCatch>,
    frame: *mut Frame,
    body: *const BytecodeFunction,
    catches: *mut Vec<SavedCatch>,
    catch_base: usize,
    completed_cleanup: Option<u32>,
    landing_stub: *const u8,
    landing: SysvNativeLanding,
    dispatch: DispatchPacket,
    cleanups: *mut Vec<SavedCleanup>,
    specials: *mut Vec<SavedSpecial>,
    cleanup_depths: *const std::collections::HashMap<u32, usize>,
    code_base: usize,
    sites: *const SysvTransferTable,
    snapshots: *mut u8,
    activation: *mut EgclVal,
    slots: usize,
    selected: Option<usize>,
    failure: Option<TransferSiteError>,
}
static CAPTURE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut CaptureContext>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };
#[cfg(test)]
pub(super) fn condition_capture_for_test() -> Option<(usize, bool, *mut Frame)> {
    let context = unsafe { CAPTURE.with(Cell::get).as_ref() }?;
    Some((
        context.owner as usize,
        !context.nested.is_null(),
        context.frame,
    ))
}

#[cfg(test)]
pub(super) fn child_capture_for_test() -> Option<(usize, bool)> {
    let capture = CAPTURE.with(Cell::get);
    if capture.is_null() {
        None
    } else {
        Some(unsafe { ((*capture).owner as usize, !(*capture).nested.is_null()) })
    }
}

struct EntryGuard {
    env: *mut Env,
    capture: *mut CaptureContext,
    error: Option<EgclError>,
}

/// Enter a published callable from a Rust caller. The outer shim owns no Lisp
/// activation; its selected callable and argument buffer are host roots. Every
/// callback establishes a fresh segment and temporarily hides the enclosing
/// capture, so a selected transfer cannot jump across live Rust scopes.
#[cfg(test)]
pub(super) unsafe fn invoke_outer_callable(
    code: *const u8,
    invocation: *mut u64,
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_env = &mut *env);
    let mut entry = EntryGuard {
        env: NATIVE_ENV.with(|slot| slot.replace(env)),
        capture: CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())),
        error: NATIVE_ERROR.with(|slot| slot.take()),
    };
    egcl_rt::rooted_ref!(_entry = &mut entry);
    let _recovery = FaultRecoveryGuard {
        null: egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
        stack: egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
    };
    egcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
    let _ordinary_segment = super::call_table::enter_ordinary_segment();
    let outcome = unsafe {
        native_transfer::invoke_native_segment(code, invocation, egcl_rt::current_stack())
    }
    .map_err(|_| EgclError::Internal("published native callable boundary is unavailable".into()))?;
    egcl_rt::rooted!(primary = outcome.value);
    egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
    match outcome.exit {
        NativeExit::Returned if error.is_none() => Ok(*primary),
        NativeExit::Transfer => Err(error.take().ok_or_else(invalid_capture)?),
        _ => Err(invalid_capture()),
    }
}
/// Enter an ordinary callable adapter in a fresh root segment. The adapter
/// may interpret the body, but its machine ingress and return use the Lisp ABI.
/// Hiding the enclosing capture prevents transfers from crossing Rust frames.
pub(super) unsafe fn invoke_outer_ordinary(
    code: *const u8,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted_ref!(_env = &mut *env);
    let mut entry = EntryGuard {
        env: NATIVE_ENV.with(|slot| slot.replace(env)),
        capture: CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())),
        error: NATIVE_ERROR.with(|slot| slot.take()),
    };
    egcl_rt::rooted_ref!(_entry = &mut entry);
    let _recovery = FaultRecoveryGuard {
        null: egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
        stack: egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
    };
    egcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
    let _ordinary_segment = super::call_table::enter_ordinary_segment();
    let execution = egcl_rt::thread::current_execution_context_for_native()
        .ok_or_else(|| EgclError::Internal("ordinary callable has no execution".into()))?;
    let overflow_bytes = args
        .len()
        .saturating_sub(3)
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .ok_or_else(invalid_capture)?;
    let required = overflow_bytes
        .checked_add(8192)
        .ok_or_else(invalid_capture)?;
    let sp: usize;
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    if sp
        .checked_sub(required)
        .is_none_or(|bottom| (bottom as u64) <= egcl_rt::stack::current_native_stack_limit())
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
    // Rust callers can retain an earlier zero/many-value tuple. The callable
    // and actuals already have independent roots; establish the ordinary entry
    // precondition before the machine adapter can poll or execute Lisp code.
    env.clear_mv();
    let returned = unsafe {
        native_transfer::invoke_ordinary_native_segment(
            code,
            &*execution,
            function,
            args,
            egcl_rt::current_stack(),
        )
    }
    .map_err(|_| EgclError::Internal("ordinary callable boundary is unavailable".into()))?;
    egcl_rt::rooted!(primary = returned.outcome.value);
    egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
    match returned.outcome.exit {
        NativeExit::Returned if error.is_none() => Ok(*primary),
        NativeExit::Transfer => Err(error.take().ok_or_else(invalid_capture)?),
        _ => Err(invalid_capture()),
    }
}

impl egcl_rt::gc::TraceHostRoots for EntryGuard {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.error.trace_host_roots(visit);
    }
}
impl Drop for EntryGuard {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.env));
        CAPTURE.with(|slot| slot.set(self.capture));
        restore_native_error(self.error.take());
    }
}

// A completed escape always lands in its immediate machine caller. Nested
// entries retain their caller-owned cold record; only outer entries leave the
// segment. No Rust frame is crossed by the subsequent assembly dispatch.
fn prepare_escape(context: &mut CaptureContext) {
    context.dispatch = DispatchPacket {
        entry: native_transfer::leave_native_segment as *const u8,
        request: native_transfer::current_segment().cast(),
    };
    if !context.nested.is_null() {
        let record = unsafe { &*context.nested };
        context.landing = SysvNativeLanding {
            stack_pointer: context.nested.cast(),
            entry: record.cold_entry,
        };
        context.dispatch = DispatchPacket {
            entry: context.landing_stub,
            request: std::ptr::from_mut(&mut context.landing).cast(),
        };
    }
}

// Capture into reserved, rooted snapshots before live signaling can allocate
// or reenter Lisp. Rust returns before the capture stub dispatches; assembly
// only discards generated frames, never a Rust signaling or handler frame.
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    // The helper's publication ended with its Rust frame, but the capture stub
    // built an equivalent image describing the same suspended caller, and
    // preparation below signals and allocates. Publish for its whole extent;
    // the guard unlinks before the stub discards this image.
    unsafe {
        root_publication::published(capture, || {
            #[cfg(test)]
            root_publication::observe_cold();
            prepare_unpublished(capture)
        })
    }
}

unsafe fn prepare_unpublished(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    prepare_escape(context);
    if context.osr.is_some() {
        // The poll's exact state is in scanned canonical slots. Its Rust scope
        // has returned; retire this machine frame before entering T2.
        capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
        return;
    }
    #[cfg(test)]
    if !context.nested.is_null() && nested::take_capture_failure() {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    }
    if context.recursive_enabled {
        // This entry admits no dynamic handlers, cleanups, bindings or captured
        // environments. An escaping transfer therefore leaves every recursive
        // activation, but still validates the exact source/root map first.
        let frame = if context.recursive.is_null() {
            context.frame
        } else {
            unsafe { (*context.recursive).frame }
        };
        let sites = unsafe { &*context.sites };
        let captured = (|| {
            if context.failure.is_some() {
                return Ok(()); // retain the inherited failure; still retire below
            }
            if egcl_rt::current_stack().fp() != frame {
                return Err(TransferSiteError::InvalidLandingCapture);
            }
            let index = sites
                .sites()
                .position(|site| {
                    context.code_base + site.return_offset() as usize == capture.return_pc as usize
                })
                .ok_or(TransferSiteError::WrongReturnPc)?;
            unsafe {
                let snapshot = &mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index);
                if context.native_recursive.is_some() {
                    snapshot.capture(context.code_base, capture)?;
                } else {
                    snapshot.capture_from_activation(
                        context.code_base,
                        capture,
                        std::slice::from_raw_parts(frame.add(1).cast(), context.slots),
                    )?;
                }
            }
            Ok(())
        })();
        if let Err(failure) = captured {
            context.failure.get_or_insert(failure);
        }
        while !context.recursive.is_null() {
            unsafe {
                retire_recursive(context, context.recursive);
            }
        }
        native_frames::retire_children(context);
        context.recursive_escape = true;
        capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
        return;
    }
    if context.failure.is_some() {
        capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
        return;
    }
    // A poll can deliver a pending signal (an interrupt, or STORAGE-CONDITION
    // when the heap is exhausted, GitHub #471) at a return PC the compiler
    // gave no transfer site, such as a straight-line poll. When the activation
    // is quiescent, with no live handler, catch, restart, cleanup or dynamic
    // binding (as `ActivationState::is_quiescent` requires to return), nothing
    // in it can receive the error or needs unwinding: leave it as an ordinary
    // segment escape does, and let the caller signal the error. One with live
    // scopes still needs a site (bliss-q5846).
    if unsafe { &*context.sites }
        .lookup(context.code_base, capture.return_pc as usize)
        .is_none()
        && NATIVE_ERROR.with(|slot| slot.is_some())
        && context.completed_cleanup.is_none()
        && unsafe {
            (*context.dynamic_scopes).is_empty()
                && (*context.handlers).is_empty()
                && (*context.handler_binds).is_empty()
                && (*context.restart_cases).is_empty()
                && (*context.catches).is_empty()
                && (*context.cleanups).is_empty()
                && (*context.specials).is_empty()
                && (*context.prepared_handler).is_none()
                && (*context.prepared_catch).is_none()
        }
        && egcl_rt::current_stack().fp() == context.frame
    {
        native_frames::retire_children(context);
        context.recursive_escape = true;
        capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
        return;
    }
    let mut expected_frame = context.frame;
    let mut frames_valid = true;
    let cluster_frames = unsafe { &mut *context.cluster_frames };
    cluster_frames.clear();
    cluster_frames.extend(
        unsafe { &*context.handlers }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.extend(
        unsafe { &*context.handler_binds }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.extend(
        unsafe { &*context.restart_cases }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.sort_unstable_by_key(|(push_bcp, _)| *push_bcp);
    for &(_, cluster_frame) in cluster_frames.iter() {
        frames_valid &= unsafe { (*cluster_frame).prev_fp == expected_frame };
        expected_frame = cluster_frame;
    }
    if context.dispatch.request.is_null()
        || !frames_valid
        || egcl_rt::current_stack().fp() != expected_frame
    {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    } else {
        let result = guard_c2i(|| {
            if let Err(error) = unsafe { prepare_transfer(context, capture) } {
                context.failure = Some(error);
                return Err(invalid_capture());
            }
            Ok(NIL)
        });
        if let Err(error) = result {
            context
                .failure
                .get_or_insert(TransferSiteError::InvalidLandingCapture);
            NATIVE_ERROR.with(|slot| slot.set_first(error));
        }
    }
    capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
}

unsafe fn prepare_transfer(
    context: &mut CaptureContext,
    capture: &mut SysvTransferCapture,
) -> Result<(), TransferSiteError> {
    let sites = unsafe { &*context.sites };
    let site = sites
        .lookup(context.code_base, capture.return_pc as usize)
        .ok_or(TransferSiteError::WrongReturnPc)?;
    let index = sites
        .sites()
        .position(|candidate| std::ptr::eq(candidate, site))
        .ok_or(TransferSiteError::WrongReturnPc)?;
    // End the mutable snapshot borrow before live signaling can allocate or
    // reenter Lisp. The execution-owned snapshot vector remains rooted.
    unsafe {
        (&mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index)).capture_from_activation(
            context.code_base,
            capture,
            std::slice::from_raw_parts(context.activation, context.slots),
        )?;
    }
    context.selected = Some(index);
    if let Some(completed) = context.completed_cleanup {
        let cleanups = unsafe { &mut *context.cleanups };
        let saved = cleanups.pop().expect("completing rooted native cleanup");
        assert_eq!(saved.cleanup_bcp, completed);
        let CleanupAction::Resume { pending, payload } = saved.continuation.action else {
            unreachable!("completion helper checked pending continuation")
        };
        payload.restore();
        NATIVE_ERROR.with(|slot| {
            assert!(!slot.is_some());
            slot.replace(Some(unmatched_error(pending)));
        });
    }
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    // Signal before retiring dynamic state. Returning handlers and restart
    // searches see the original context; only selected transfers start unwind.
    if let Some(error) = NATIVE_ERROR.with(|slot| slot.take()) {
        let selected = if matches!(error, EgclError::Internal(_) | EgclError::Signalled(_)) {
            error
        } else {
            signal_raw_error_in_context(env, error)
        };
        NATIVE_ERROR.with(|slot| {
            slot.replace(Some(selected));
        });
    }
    let mut selected_transfer = false;
    let mut selected_target = SelectedTarget::OutsideFrame;
    let mut selected_clause = None;
    let mut handler_token = None;
    NATIVE_ERROR.with(|slot| {
        slot.visit(|error| {
            // A declined condition is already signaled and must leave this
            // activation. A selected enclosing restart likewise unwinds here
            // before its owner consumes the arguments. Both may run checked
            // local cleanup without guessing an outer native destination.
            if matches!(error, EgclError::Signalled(_)) {
                selected_transfer = true;
            } else if let Some(id) = super::super::restart_invoked_id(error) {
                selected_transfer = env.restarts.iter().any(|restart| restart.id == id);
            } else if let Some(token) = handler_case_token(error) {
                selected_transfer = env.handlers.iter().any(|cluster| cluster.entries.iter().any(|entry|
                    matches!(&entry.handler, HandlerImpl::HandlerCase { token: live, .. } if live == &token)));
                for saved in unsafe { &*context.handlers } {
                    if let Some(index) = saved.clauses.iter().position(|clause| clause.token == token) {
                        selected_target = SelectedTarget::Scope { push_bcp: saved.push_bcp };
                        selected_clause = Some(index as u32);
                        break;
                    }
                }
                handler_token = Some(token);
            } else if let EgclError::Internal(token) = error {
                selected_transfer = env.catch_stack.iter().any(|(_, live)| live == token);
                if let Some(saved) = unsafe { &*context.catches }.iter().find(|saved| &saved.token == token) {
                    selected_target = SelectedTarget::Scope { push_bcp: saved.push_bcp };
                }
            }
        })
    });
    if !selected_transfer {
        return Ok(());
    }
    // Plan against scope prefixes without mutating live catch registrations.
    // If any subsequent step lacks a native destination, fallback still sees
    // the exact state described by this source site's reconstruction map.
    let scopes = &site.map().control_scopes;
    if !unsafe { validate_special_bindings(context, scopes) } {
        return Err(TransferSiteError::InvalidLandingCapture);
    }
    let mut remaining = scopes.as_slice();
    let action = loop {
        match next_unwind_step(remaining, selected_target) {
            NativeUnwindStep::RetireCatch { scope_index }
            | NativeUnwindStep::RetireHandler { scope_index }
            | NativeUnwindStep::RetireSpecial { scope_index } => {
                remaining = &remaining[..scope_index];
            }
            action => break action,
        }
    };
    let (scope_index, handler_depth, landing) = match action {
        NativeUnwindStep::RunCleanup {
            scope_index,
            handler_depth,
        } => {
            let Some(landing) = site.native_cleanup_landing(context.code_base, capture)? else {
                return Ok(());
            };
            (scope_index, handler_depth, landing)
        }
        NativeUnwindStep::EnterTarget { scope_index } => {
            let landing = match scopes[scope_index].kind {
                ScopeKind::Catch { .. } => {
                    #[cfg(test)]
                    if context.unavailable_catch == Some(scopes[scope_index].push_bcp) {
                        return Ok(());
                    }
                    site.native_catch_landing(
                        context.code_base,
                        capture,
                        scopes[scope_index].push_bcp,
                    )?
                }
                ScopeKind::HandlerCase { .. } => {
                    let Some(clause) = selected_clause else {
                        return Ok(());
                    };
                    #[cfg(test)]
                    if context.unavailable_handler == Some((scopes[scope_index].push_bcp, clause)) {
                        return Ok(());
                    }
                    site.native_handler_landing(
                        context.code_base,
                        capture,
                        scopes[scope_index].push_bcp,
                        clause,
                    )?
                }
                _ => return Ok(()),
            };
            let Some(landing) = landing else {
                return Ok(());
            };
            let handler_depth = scopes[..scope_index]
                .iter()
                .filter(|scope| {
                    matches!(
                        scope.kind,
                        ScopeKind::Block { .. }
                            | ScopeKind::Tagbody { .. }
                            | ScopeKind::Catch { .. }
                            | ScopeKind::Unwind { .. }
                            | ScopeKind::HandlerCase { .. }
                            | ScopeKind::HandlerBind { .. }
                            | ScopeKind::RestartCase { .. }
                    )
                })
                .count();
            (scope_index, handler_depth, landing)
        }
        _ => return Ok(()),
    };
    // Validate every retiring record before changing either stack. This also
    // checks selected-catch identity independently of its resume address.
    let retiring = scopes[scope_index..]
        .iter()
        .rev()
        .filter(|scope| matches!(scope.kind, ScopeKind::Catch { .. }));
    let catches = unsafe { &mut *context.catches };
    let mut saved = catches.iter().rev();
    let mut live = env.catch_stack.iter().rev();
    let mut retire_count = 0;
    for scope in retiring {
        let Some(record) = saved.next() else {
            return Ok(());
        };
        let Some((_, token)) = live.next() else {
            return Ok(());
        };
        if record.push_bcp != scope.push_bcp || &record.token != token {
            return Ok(());
        }
        retire_count += 1;
    }
    let handlers = unsafe { &mut *context.handlers };
    let mut saved_handlers = handlers.iter().rev();
    let mut handler_count = 0;
    let mut cluster_end = env.handlers.len();
    for scope in scopes[scope_index..]
        .iter()
        .rev()
        .filter(|s| matches!(s.kind, ScopeKind::HandlerCase { .. }))
    {
        let Some(saved) = saved_handlers.next() else {
            return Ok(());
        };
        if saved.push_bcp != scope.push_bcp || saved.cluster_base + 1 != cluster_end {
            return Ok(());
        }
        let Some(cluster) = env.handlers.get(saved.cluster_base) else {
            return Ok(());
        };
        if cluster.entries.len() != saved.clauses.len() || !cluster.entries.iter().zip(&saved.clauses).all(|(entry, clause)|
            matches!(&entry.handler, HandlerImpl::HandlerCase { token, .. } if token == &clause.token)) {
            return Ok(());
        }
        handler_count += 1;
        cluster_end = saved.cluster_base;
    }
    // Restore native homes while canonical snapshots still own every root.
    unsafe {
        (&*context.snapshots.cast::<SysvSiteSnapshot>().add(index))
            .write_back(context.code_base, capture)?;
    }
    let error = NATIVE_ERROR
        .with(|slot| slot.take())
        .expect("selected rooted transfer");
    let cleanups = unsafe { &mut *context.cleanups };
    while cleanups
        .last()
        .is_some_and(|saved| saved.continuation.handler_depth > handler_depth)
    {
        // Dropping the private payload cannot erase a newer throw's values,
        // even when both transfers name the same catch binding.
        cleanups.pop();
    }
    // Live signaling above saw every binding. Only now, after selecting and
    // validating a destination, restore the crossed lexical suffix before
    // executing its cleanup or selected clause.
    let keep_specials = scopes[..scope_index]
        .iter()
        .filter(|scope| matches!(scope.kind, ScopeKind::SpecialBinding { .. }))
        .count();
    let specials = unsafe { &mut *context.specials };
    while specials.len() > keep_specials {
        specials.pop();
    }
    match scopes[scope_index].kind {
        ScopeKind::Unwind { cleanup_bcp } => {
            assert!(cleanups.len() < cleanups.capacity());
            cleanups.push(SavedCleanup {
                cleanup_bcp,
                continuation: CleanupCont {
                    handler_depth,
                    action: CleanupAction::Resume {
                        payload: ControlPayload::for_error(&error),
                        pending: error_to_pending(error, env),
                    },
                },
            });
            #[cfg(test)]
            NATIVE_CLEANUP_COUNT.with(|count| count.set(count.get() + 1));
        }
        ScopeKind::Catch { resume_bcp } => {
            let EgclError::Internal(token) = error else {
                unreachable!("selected catch token");
            };
            let prepared = unsafe { &mut *context.prepared_catch };
            assert!(prepared.is_none());
            *prepared = Some(PreparedCatch {
                push_bcp: scopes[scope_index].push_bcp,
                resume_bcp,
                payload: ControlPayload::take(&token),
                token,
            });
        }
        ScopeKind::HandlerCase { .. } => {
            let prepared = unsafe { &mut *context.prepared_handler };
            assert!(prepared.is_none());
            *prepared = Some(PreparedHandler {
                push_bcp: scopes[scope_index].push_bcp,
                clause_index: selected_clause.expect("selected native clause"),
                token: handler_token.expect("selected handler binding"),
                payload: ControlPayload::for_error(&error),
            });
        }
        _ => unreachable!("validated native destination"),
    }
    for _ in 0..handler_count {
        let saved = handlers.pop().expect("validated crossed handler");
        let Some(DynamicScope::HandlerCase {
            cluster_base: live_base,
            cluster_frame: live_frame,
        }) = unsafe { &mut *context.dynamic_scopes }.pop()
        else {
            return Err(TransferSiteError::InvalidLandingCapture);
        };
        if (live_base, live_frame) != (saved.cluster_base, saved.cluster_frame) {
            return Err(TransferSiteError::InvalidLandingCapture);
        }
        env.handlers.truncate(saved.cluster_base);
        pop_condition_cluster_frame(egcl_rt::current_stack(), saved.cluster_frame);
    }
    catches.truncate(catches.len() - retire_count);
    env.catch_stack
        .truncate(env.catch_stack.len() - retire_count);
    // The rooted continuation owns the pending transfer before generated
    // cleanup code can allocate or suspend. Clear any helper's secondary values.
    env.clear_mv();
    context.completed_cleanup = None;
    context.selected = None;
    context.landing = landing;
    context.dispatch = DispatchPacket {
        entry: context.landing_stub,
        request: std::ptr::from_mut(&mut context.landing).cast(),
    };
    Ok(())
}

#[unsafe(naked)]
unsafe extern "C" fn dispatch(_packet: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "mov rax, [rdi]", "mov rdi, [rdi + 8]", "jmp rax");
}

#[cfg(test)]
thread_local! {
    static ORDINARY_ENTRY_HOOK: Cell<Option<fn()>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn with_ordinary_entry_hook_for_test<R>(
    observe: fn(),
    body: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<fn()>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ORDINARY_ENTRY_HOOK.with(|hook| hook.set(self.0));
        }
    }
    let _restore = Restore(ORDINARY_ENTRY_HOOK.with(|hook| hook.replace(Some(observe))));
    body()
}

#[cfg(test)]
mod ordinary_osr_companion_tests {
    use super::*;
    use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use egcl_compiler::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, OsrEntry, ValueRepresentation,
    };

    #[test]
    fn ordinary_full_entry_retains_executable_mapped_osr_import() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
        let symbol = egcl_rt::symbols::intern("ORDINARY-OSR-COMPANION");
        let body = Arc::new(
            compile_function(
                "ORDINARY-OSR-COMPANION",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        // Model the two entry protocols directly: the full call binds X from
        // RDI, while an OSR import reads X from the retained activation local.
        let mut ir = Function::new("ordinary-osr-companion");
        let entry = ir.entry();
        let value = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let state = ir.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: symbol,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        ir.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![value],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
        );
        ir.osr_entries.push(OsrEntry {
            control_scopes: vec![],
            bcp: 0,
            block: entry,
            frame_state: state,
            checks: vec![],
        });
        egcl_compiler::t2::verify::verify(&ir).unwrap();
        let mut code = TransferCode::emit_prepared_with_ordinary(
            Arc::clone(&body),
            retain_native_body(&body),
            &ir,
            Some(&ir),
            None,
            &[],
            false,
            CompilationTier::T2,
        )
        .expect("both entry protocols emit");
        code.set_installed_symbol(symbol);
        assert!(
            code.ordinary_entry.is_some(),
            "full calls use the register ABI"
        );
        assert!(
            code.osr_entries.is_empty(),
            "primary offsets describe only primary code"
        );
        let destination = code.osr_destination();
        assert!(
            destination.ordinary_entry.is_none(),
            "OSR retains mapped entry ABI"
        );
        assert_eq!(destination.installed_symbol, Some(symbol));
        let offset = *destination
            .osr_entries
            .get(&0)
            .expect("retain the original OSR header");
        assert!(offset < destination.code_len);
        assert_eq!(
            code.run(&[EgclVal::from_fixnum(41)], &mut env).unwrap(),
            EgclVal::from_fixnum(41)
        );
        let state = Box::new(ActivationState::prepare(destination, &mut env).unwrap());
        let stack = egcl_rt::current_stack();
        let frame = stack
            .push_frame(
                EgclVal::from_symbol_index(symbol),
                destination.code_info,
                destination.slots,
                FLAG_CALL,
            )
            .unwrap();
        unsafe {
            slot_set(frame, 0, EgclVal::from_fixnum(73));
        }
        let result = destination
            .run_activation(
                &[],
                &mut env,
                Some(OsrResume {
                    frame,
                    offset,
                    state,
                }),
                NIL,
            )
            .unwrap();
        assert_eq!(
            result,
            EgclVal::from_fixnum(73),
            "OSR imports the live activation local"
        );
    }
}
