// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Default Linux runtime entry for the tagged Invoke emitter. Unsupported
//! bodies stay interpreted while their exact maps are completed. Own code and definitions,
//! capture into rooted snapshots, enter native cleanup for selected throws, and
//! retain bytecode *unwinding* as the fallback.
#![allow(dead_code)]

use super::*;
use super::native_segment_cache::SegmentCacheEntry;
use super::super::CONTROL_COUNTER;
use egcl_compiler::control_scope::{Ownership, ScopeKind};
use egcl_compiler::t2::emit::{RecursiveActivation, RecursiveCallRequest, RecursiveTransfer};
use egcl_compiler::native_unwind::{next_unwind_step, NativeUnwindStep, SelectedTarget};
use egcl_compiler::t2::native_transfer::{
    emit_capture_stub, emit_native_landing_stub, emit_published_helper_veneer,
    SysvNativeLanding, SysvTransferCapture,
};
use egcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable, TransferSiteError};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome};
use std::cell::Cell;
use std::sync::OnceLock;

pub(super) mod root_publication;
mod activation;
mod condition_osr;
mod special_bindings;
use special_bindings::{SavedSpecial, validate_special_bindings, special_request};
use condition_osr::{validate_condition_clusters, relocate_condition_clusters};
#[cfg(test)]
pub(super) use activation::fail_next_guard_reservation_for_test;
use activation::ActivationState;
mod deopt;
mod native_frames;
pub(super) mod ordinary_calls;

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
            fn drop(&mut self) { self.0.set(self.1); }
        }
        let _restore = Restore(enabled, enabled.replace(false));
        body()
    })
}
pub(super) mod native_poll;
mod ordinary_t2;
pub(super) use ordinary_t2::{PreparedT2, install_ordinary_t2, prepare_ordinary_t2};
mod nested;
use nested::{SegmentActivations, finish_nested, prepare_nested, resume_nested};
pub(super) use nested::{
    finish_nested as finish_callable, prepare_callable, resume_nested as resume_callable,
    take_call_arguments,
};
#[cfg(test)]
pub(super) use nested::pending_argument_count;
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
    Some(format_listing(&code, "; Native segment ABI — cached entry.\n"))
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
                (code._completion.as_ptr(), "native cleanup completion veneer"),
                (code._recursive_prepare.as_ref().map_or(std::ptr::null(), |code| code.as_ptr()), "native recursive entry veneer"),
                (finish_recursive as *const u8, "native recursive frame retirement"),
                (code._deopt_veneer.as_ref().map_or(std::ptr::null(), |code| code.as_ptr()), "native precise deopt veneer"),
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
static SEGMENT_RUNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

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

fn try_clone_control_stack(
    stack: &[(String, String)],
) -> Result<Vec<(String, String)>, EgclError> {
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
    _ordinary_arity: Option<JitBuffer>,
    _ordinary_cold: Option<JitBuffer>,
    pub(super) has_deopt: bool,
    bcp_offsets: Vec<u32>,
    osr_entries: HashMap<u32, usize>,
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
    recursive_symbol: Option<u32>,
    recursive_generation: u64,
    pub(super) native_entry_kind_slot: Option<u32>,
    native_control_slot: Option<u32>,
    native_argument_base: Option<u32>,
    native_argument_count: u16,
    landing: JitBuffer,
    sites: SysvTransferTable,
    slots: u16,
    cleanup_depths: std::collections::HashMap<u32, usize>,
    control_scopes: egcl_compiler::control_scope::ScopeMap,
    guard_handler_capacity: usize,
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
    install_baseline_code(symbol, TransferCode::compile_variant(body, false, false)?)
}

/// Ordinary T1 keeps speculative arithmetic and direct mapped recursion when
/// their existing admission checks succeed. Compilation stays synchronous and
/// publication remains T1; ordinary worker optimization still belongs to T2.
pub(super) fn install_t1(
    symbol: u32,
    body: Arc<BytecodeFunction>,
    allow_speculation: bool,
) -> Option<Rc<NativeCode>> {
    if !native_transfer::is_supported() || !fixed_tagged_parameters(&body) {
        return None;
    }
    let code = TransferCode::compile_variant(Arc::clone(&body), allow_speculation, true)
        .or_else(|| TransferCode::compile_variant(body, false, true))?;
    install_baseline_code(symbol, code)
}

pub(super) fn install_baseline_code(symbol: u32, mut code: TransferCode) -> Option<Rc<NativeCode>> {
    code.installed_symbol = Some(symbol);
    let body = Arc::clone(&code.body);
    let code = Rc::new(code);
    let code_info = code.code_info;
    let bcp_offsets = code.bcp_offsets.clone();
    let has_deopt = code.has_deopt;
    let metadata = code.deopt_metadata.clone();
    let entry = code.code.as_ptr();
    let bytes = unsafe { std::slice::from_raw_parts(entry, code.code_len) };
    maybe_write_perf_map(entry as usize, code.code_len, symbol);
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
    let current = || {
        registry_get(symbol).is_some_and(|current| Arc::ptr_eq(&current, &body))
            && recursive_definition_matches(symbol, Arc::as_ptr(&body))
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
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol)?);
    // Ordinary T2 versions retire to an honest non-speculating mapped baseline.
    // The next dispatch can queue optimized worker compilation again; this
    // local baseline frontend must not replace it under a T2 tier label.
    let replacement = TransferCode::compile_variant(Arc::clone(&body), !expected.is_t2, !expected.is_t2)
        .or_else(|| TransferCode::compile_variant(Arc::clone(&body), false, !expected.is_t2))?;
    // Compilation can collect and reenter runtime services. Never let a stale
    // invocation replace code published for a newer version or callable.
    if !current() || egcl_rt::symbols::symbol_function(symbol) != Some(*function) {
        return None;
    }
    let replacement = install_baseline_code(symbol, replacement)?;
    publish_native(symbol, Some(*function), &replacement);
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
    unsafe {
        root_publication::published_mapped(record, || {
            call_table::checked_call(target, record);
            if (*record).outcome.exit == NativeExit::Returned {
                let env = NATIVE_ENV.with(Cell::get);
                (*record).return_values = (*env).native_return_values();
            }
        })
    }
}

static PUBLISHED_ENTRIES: OnceLock<Option<[root_publication::PublishedAdapter; 2]>> = OnceLock::new();

fn mapped_adapter(pc: usize) -> Option<&'static root_publication::PublishedAdapter> {
    PUBLISHED_ENTRIES.get()?.as_ref()?.iter().find(|entry| entry.contains_return(pc))
}

pub(super) fn published_entries() -> Option<[usize; 2]> {
    PUBLISHED_ENTRIES
        .get_or_init(|| {
            let entry = |slice| {
                root_publication::PublishedAdapter::new(
                    egcl_compiler::t2::native_transfer::emit_published_call_entry_descriptor(
                        slice,
                        prepare_nested,
                        finish_nested,
                        resume_nested,
                        checked_call_published,
                    ),
                )
            };
            Some([entry(false)?, entry(true)?])
        })
        .as_ref()
        .map(|entries| std::array::from_fn(|i| entries[i].code.as_ptr() as usize))
}

#[cfg(test)]
pub(super) fn fail_next_nested_capture() {
    nested::fail_next_capture();
}

#[cfg(test)]
pub(super) fn take_nested_entries() -> usize {
    nested::take_nested_entries()
}

impl TransferCode {
    pub(super) fn mapped_entry(&self) -> Option<*const u8> {
        let offset = self.frame_entry.filter(|offset| *offset < self.code_len)?;
        Some(self.code.as_ptr().wrapping_add(offset))
    }

    pub(super) fn ordinary_descriptor(&self, revision: u64) -> Option<egcl_rt::call_table::OrdinaryEntryDescriptor> {
        use egcl_rt::call_table::{OrdinaryEntryDescriptor, ORDINARY_X64_LISP_KIND, ORDINARY_X64_LISP_VERSION, ORDINARY_ENV_NONE};
        let offset = self.ordinary_entry.filter(|offset| *offset < self.code_len)?;
        Some(OrdinaryEntryDescriptor {
            kind: ORDINARY_X64_LISP_KIND, version: ORDINARY_X64_LISP_VERSION, revision,
            entry: (self.code.as_ptr() as usize).checked_add(offset)?,
            code_start: self.code.as_ptr() as usize, code_len: self.code_len,
            arity: usize::from(self.body.arity), environment: ORDINARY_ENV_NONE,
            owner: self as *const Self as usize, stack_required: self.ordinary_stack_required?,
        })
    }

    #[cfg(test)]
    pub(super) fn value_maps_for_test(&self) -> &Arc<egcl_compiler::t2::x64_calls::NativeCallSites> {
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
        Self::compile_variant(Arc::clone(&body), true, true)
            .or_else(|| Self::compile_variant(body, false, true))
    }

    #[cfg(test)]
    pub(super) fn compile_nested_protected(body: Arc<BytecodeFunction>) -> Option<Self> {
        Self::compile_variant(body, false, false)
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
        Self::compile_variant(Arc::clone(&body), true, false)
            .or_else(|| Self::compile_variant(body, false, false))
    }

    pub(super) fn needs_recompile(&self) -> bool {
        self.recompile.get()
    }

    fn compile_variant(
        body: Arc<BytecodeFunction>,
        optimize: bool,
        allow_recursion: bool,
    ) -> Option<Self> {
        if body.variadic || body.has_env {
            return None;
        }
        // These scopes need executing identity, dynamic values or inherited
        // state. Do not invent fresh tokens during recovery for live closures.
        if body.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::PushBlock { register: true, .. } | Instr::NamedTag { .. }
            )
        }) {
            return None;
        }
        let roots = retain_native_body(&body);
        let optimize = optimize && scope_free_native_body(&body, false);
        let (ir, deopt_metadata) = if optimize {
            let symbol = super::super::resolve_sym(&body.name)?.as_symbol_index();
            let mut ir = egcl_compiler::t2::build::build_for_transfer_optimization(&body, symbol).ok()?;
            let profile = |bcp| {
                use egcl_compiler::t2::speculate::SpecType as CompilerType;
                match type_profile_at(Arc::as_ptr(&body) as usize, bcp) {
                    None => Some(CompilerType::Fixnum),
                    Some(profile) => match profile.speculation() {
                        Some(SpecType::Fixnum) => Some(CompilerType::Fixnum),
                        _ => None,
                    },
                }
            };
            egcl_compiler::t2::speculate::speculate(&mut ir, &profile);
            let speculations = t2_numeric_speculations(&ir, symbol);
            let mut passes = egcl_compiler::t2::pass::PassManager::new();
            passes.add(Box::new(egcl_compiler::t2::opt_fold::ConstFold));
            passes.add(Box::new(egcl_compiler::t2::opt_gvn::Gvn));
            passes.add(Box::new(egcl_compiler::t2::opt_guard::GuardElim));
            passes.add(Box::new(egcl_compiler::t2::opt_clear_mv::ClearMvElim));
            passes.add(Box::new(egcl_compiler::t2::opt_dce::Dce));
            passes.run(&mut ir);
            let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).ok()?;
            egcl_compiler::t2::build::legalize_transfer_calls(&mut ir, &scopes).ok()?;
            let metadata = Arc::new(T2InstalledMetadata {
                speculations,
                _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies { bodies: vec![Arc::clone(&body)] }),
                deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
            });
            (ir, Some(metadata))
        } else {
            (egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body).ok()?, None)
        };
        let call_cells: Vec<_> = body.code.iter().filter_map(|instruction| {
            let Instr::CallNamed { sym, .. } = instruction else { return None };
            call_table::resolve(*sym).map(|cell| (*sym, cell))
        }).collect();
        Self::emit_prepared(body, roots, &ir, deopt_metadata, &call_cells, allow_recursion, CompilationTier::T1)
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
        Self::emit_prepared_with_ordinary(body, roots, ir, None, deopt_metadata, call_cells, allow_recursion, tier)
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
        // Every normal mapped entry clears the complete MV buffer before
        // admission (outer, nested, and recursive). OSR deliberately preserves
        // pending values; the analysis treats each OSR header as unknown.
        let mut prepared_ir = ir.clone();
        egcl_compiler::t2::opt_clear_mv::eliminate_redundant(&mut prepared_ir, true);
        let ir = &prepared_ir;
        let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8))?;
        let deopt_capture = if deopt_metadata.is_some() {
            Some(JitBuffer::new(&emit_capture_stub(prepare_completed_deopt, dispatch as *const u8))?)
        } else { None };
        let mut deopt_veneer = match &deopt_capture {
            Some(capture) => Some(JitBuffer::new(&emit_published_helper_veneer(resume_guard, capture.as_ptr()))?),
            None => None,
        };
        let veneer = JitBuffer::new(&emit_published_helper_veneer(call_or_throw, capture.as_ptr()))?;
        let mut call_entries = Vec::new();
        for (sym, cell) in call_cells {
            if call_entries.iter().any(|(symbol, _, _)| symbol == sym) {
                continue;
            }
            let mapped = JitBuffer::new(
                &egcl_compiler::t2::native_transfer::emit_published_call_veneer(
                    Arc::as_ptr(cell) as u64,
                    cell.entry_address(false) as u64,
                    cell.entry_address(true) as u64,
                    capture.as_ptr(),
                ),
            )?;
            call_entries.push((*sym, Arc::clone(cell), mapped));
        }
        let named_veneers: Vec<_> = call_entries.iter()
            .map(|(symbol, _, adapter)| (*symbol, adapter.as_ptr() as u64))
            .collect();
        // Optimized baseline code still needs T1 heat and OSR handling. The
        // construction tier, not IR optimization or deopt metadata, selects this.
        let poll_fast_path = matches!(tier, CompilationTier::T2).then(native_poll::binding).flatten();
        let poll_helper = match tier {
            CompilationTier::T1 => poll_or_transfer,
            CompilationTier::T2 => poll_t2_or_transfer,
        };
        let poll = JitBuffer::new(&emit_published_helper_veneer(poll_helper, capture.as_ptr()))?;
        let base_slots = body.n_locals.checked_add(body.max_stack)?;
        let completion = JitBuffer::new(&emit_published_helper_veneer(complete_cleanup, capture.as_ptr()))?;
        let landing = JitBuffer::new(&emit_native_landing_stub())?;
        // Direct recursion first shares only immutable code/site metadata.
        // Dynamic handler and cleanup records still require their own mapped
        // activation ownership before these bodies can use recursive entries.
        // Sample before reading the definition: a concurrent replacement after
        // the identity check must leave a generation mismatch at entry.
        let recursive_generation = direct_call_gen();
        let recursive_symbol = super::super::resolve_sym(&body.name)
            .filter(|_| allow_recursion && scope_free_native_body(&body, false))
            .map(EgclVal::as_symbol_index)
            .filter(|symbol| recursive_definition_matches(*symbol, Arc::as_ptr(&body)))
            .filter(|symbol| body.code.iter().any(|op| {
                matches!(op, Instr::CallNamed { sym, .. } if sym == symbol)
            }));
        let mut recursive_prepare = match recursive_symbol {
            Some(_) => Some(JitBuffer::new(&emit_published_helper_veneer(prepare_recursive, capture.as_ptr()))?),
            None => None,
        };
        let recursion = recursive_symbol.map(|symbol| RecursiveTransfer {
            symbol,
            arity: usize::from(body.arity),
            prepare: recursive_prepare.as_ref().unwrap().as_ptr() as u64,
            finish: finish_recursive as *const () as u64,
        });
        let native_frames = if matches!(tier, CompilationTier::T2)
            && body.arity <= 4 && ir.osr_entries.is_empty()
            && deopt_metadata.is_some()
        {
            recursive_symbol.zip(native_frames::flags()).and_then(|(symbol, flags)| {
                let cold = JitBuffer::new(&emit_published_helper_veneer(
                    native_frames::slow_call, capture.as_ptr()))?;
                let guard = JitBuffer::new(&emit_published_helper_veneer(
                    deopt::resume_native_guard, deopt_capture.as_ref()?.as_ptr()))?;
                let result = egcl_compiler::t2::emit::emit_framed_native_handlers_with_native_recursion(
                    ir, veneer.as_ptr() as u64, base_slots,
                    save_cleanup as *const () as u64, completion.as_ptr() as u64,
                    c2i_t2_mv as *const () as u64, deliver_catch as *const () as u64,
                    deliver_handler as *const () as u64, poll.as_ptr() as u64,
                    guard.as_ptr() as u64, &named_veneers,
                    egcl_compiler::t2::emit::NativeRecursiveTransfer {
                        symbol, arity: usize::from(body.arity),
                        initialize: native_frames::initialize as *const () as u64,
                        cold_veneer: cold.as_ptr() as u64, flags,
                    },
                ).inspect_err(|error| t2_log_write(format_args!(
                    "{}: native recursive entry: {error:?}", display_fn_name(&body.name)
                ))).ok()?;
                recursive_prepare = Some(cold);
                deopt_veneer = Some(guard);
                Some(result)
            })
        } else { None };
        let mut ordinary_arity = None;
        let mut ordinary_cold = None;
        let ordinary = ordinary_ir.filter(|_| matches!(tier, CompilationTier::T2)
            && fixed_tagged_parameters(&body) && scope_free_native_body(&body, false))
            .and_then(|ir| {
                use egcl_compiler::t2::emit::{OrdinaryLispEntryOptions, OrdinaryCallTarget, OrdinaryCallFlags};
                let mut ir = ir.clone();
                egcl_compiler::t2::opt_clear_mv::eliminate_redundant(&mut ir, true);
                let arity = JitBuffer::new(&egcl_compiler::t2::native_transfer::emit_ordinary_arity_veneer(
                    ordinary_calls::wrong_arity, usize::from(body.arity)))?;
                let options = OrdinaryLispEntryOptions {
                    arity: usize::from(body.arity), arity_error_veneer: arity.as_ptr() as u64,
                };
                let emitted = egcl_compiler::t2::emit::emit_framed_ordinary_leaf(&ir, options).ok()
                    .or_else(|| {
                        let flags = native_frames::flags()?;
                        let cold = JitBuffer::new(&egcl_compiler::t2::native_transfer::emit_ordinary_published_veneer(
                            ordinary_calls::cold_call))?;
                        let emit_call = egcl_compiler::t2::emit::emit_framed_ordinary_tail_call;
                        #[cfg(test)]
                        let emit_call = if ORDINARY_TAIL_EMISSION.with(Cell::get) {
                            emit_call
                        } else { egcl_compiler::t2::emit::emit_framed_ordinary_call };
                        let emitted = call_cells.iter().find_map(|(symbol, cell)| {
                            emit_call(&ir, options,
                                OrdinaryCallTarget { symbol: *symbol,
                                    descriptor_slot: cell.ordinary_entry_address() as u64,
                                    revision_address: cell.revision_address() as u64 },
                                OrdinaryCallFlags { gc_requested: flags.gc_requested,
                                    sigusr1_pending: flags.sigusr1_pending, posted_signal: flags.posted_signal,
                                    native_errors: flags.native_errors, signal_epoch: flags.signal_epoch },
                                cold.as_ptr() as u64).ok()
                        })?;
                        ordinary_cold = Some(cold);
                        Some(emitted)
                    })?;
                let sites = SysvTransferTable::new(emitted.code.len(), Vec::new()).ok()?;
                ordinary_arity = Some(arity);
                Some((emitted, sites))
            });
        // A shape not yet admitted by the native-frame emitter retains its
        // existing mapped execution and original recovery veneers.
        let (emitted, sites) = ordinary.or(native_frames).or_else(|| egcl_compiler::t2::emit::emit_framed_native_handlers_with_entries_and_poll_fast_path(
            ir,
            veneer.as_ptr() as u64,
            base_slots,
            save_cleanup as *const () as u64,
            completion.as_ptr() as u64,
            c2i_t2_mv as *const () as u64,
            deliver_catch as *const () as u64,
            deliver_handler as *const () as u64,
            poll.as_ptr() as u64,
            recursion,
            deopt_veneer.as_ref().map(|code| code.as_ptr() as u64),
            &named_veneers,
            egcl_compiler::t2::emit::NativeRootPublication::PublishedHomes,
            poll_fast_path,
        )
        .inspect_err(|error| t2_log_write(format_args!(
            "{}: mapped emitter: {error:?}{}",
            display_fn_name(&body.name), describe_emit_error(error)
        )))
        .ok())?;
        // Reconstruct local, non-escaping BLOCK/TAGBODY records and pending
        // UNWIND-PROTECT cleanups from exact scope maps. Other records require the
        // forthcoming native scope state; rejecting them is part of admission.
        for site in sites.sites() {
            if site.map().control_scopes.iter().any(|scope| {
                scope.ownership != Ownership::Local
                    || !matches!(
                        scope.kind,
                        ScopeKind::Block {
                            register: false,
                            ..
                        } | ScopeKind::Tagbody { .. }
                            | ScopeKind::Unwind { .. }
                            | ScopeKind::Cleanup { .. }
                            | ScopeKind::Catch { .. }
                            | ScopeKind::HandlerCase { .. }
                            | ScopeKind::HandlerBind { .. }
                            | ScopeKind::RestartCase { .. }
                            | ScopeKind::SpecialBinding { .. }
                    )
            }) {
                return None;
            }
        }
        let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).ok()?;
        let guard_handler_capacity = (0..body.code.len()).filter_map(|bcp| {
            scopes.before(bcp as u32)
        }).map(|active| active.iter().filter(|scope| !matches!(scope.kind,
            ScopeKind::Cleanup { .. } | ScopeKind::PendingCleanup { .. } | ScopeKind::SpecialBinding { .. }
        )).count()).max().unwrap_or(0);
        let mut cleanup_depths = std::collections::HashMap::new();
        for instruction in &body.code {
            if let Instr::PushUnwind { cleanup_bcp, .. } = instruction {
                let depth = scopes
                    .before(*cleanup_bcp)?
                    .iter()
                    .filter(|scope| {
                        matches!(
                            scope.kind,
                            ScopeKind::Block { .. }
                                | ScopeKind::Tagbody { .. }
                                | ScopeKind::Unwind { .. }
                                | ScopeKind::Catch { .. }
                                | ScopeKind::HandlerCase { .. }
                                | ScopeKind::HandlerBind { .. }
                                | ScopeKind::RestartCase { .. }
                        )
                    })
                    .count();
                cleanup_depths.insert(*cleanup_bcp, depth);
            }
        }
        let slots = base_slots.checked_add(emitted.shadow_root_slots)?;
        if emitted.frame_entry.is_some_and(|offset| offset >= emitted.code.len()) {
            return None;
        }
        let mut code = JitBuffer::new(&emitted.code)?;
        // Native entries do not receive the managed slots pointer in RDI:
        // private entries receive control; ordinary entries receive argument 1.
        // Runtime stack capture uses their immutable native argument homes.
        let arguments = native_debug::arguments(&body, emitted.native_recursive_entry.is_some() || emitted.ordinary_entry.is_some());
        let debug_info = code
            .install_debug_info_with_arguments(&body.name, arguments.as_ref())
            .ok()?;
        let code_info = install_stack_map(slots, debug_info)?;
        let osr_polls = emitted.native_calls.as_ref()?.iter().filter_map(|site| {
            let egcl_compiler::t2::x64_calls::NativeCallOrigin::LoopPoll(block) = site.origin else {
                return None;
            };
            ir.osr_entries.iter().find(|entry| entry.block == block)
                .filter(|entry| {
                    let state = ir.frame_states.get(entry.frame_state);
                    state.scopes.len() == 1 && state.scopes[0].stack.is_empty()
                })
                .map(|entry| (site.return_offset, entry.bcp))
        }).collect();
        Some(Self {
            installed_symbol: None,
            is_t2: matches!(tier, CompilationTier::T2),
            #[cfg(test)]
            unavailable_catch: None,
            #[cfg(test)]
            unavailable_handler: None,
            body,
            _body_roots: roots,
            code,
            code_info,
            code_len: emitted.code.len(),
            frame_entry: emitted.frame_entry,
            ordinary_entry: emitted.ordinary_entry,
            ordinary_stack_required: emitted.ordinary_stack_required,
            _ordinary_arity: ordinary_arity,
            _ordinary_cold: ordinary_cold,
            _native_calls: Arc::new(emitted.native_calls?),
            has_deopt: emitted.has_deopt,
            bcp_offsets: emitted.bcp_offsets,
            osr_entries: emitted.osr_entries.into_iter().collect(),
            osr_polls,
            _veneer: veneer,
            _call_entries: call_entries,
            _capture: capture,
            _poll: poll,
            _completion: completion,
            _recursive_prepare: recursive_prepare,
            _deopt_capture: deopt_capture,
            _deopt_veneer: deopt_veneer,
            deopt_metadata,
            deopts: Cell::new(0),
            recompile: Cell::new(false),
            recursive_symbol,
            recursive_generation,
            native_entry_kind_slot: emitted.native_entry_kind_slot,
            native_control_slot: emitted.native_control_slot,
            native_argument_base: emitted.native_argument_base,
            native_argument_count: emitted.native_argument_count,
            landing,
            sites,
            slots,
            cleanup_depths,
            control_scopes: scopes,
            guard_handler_capacity,
        })
    }

    pub(super) fn run(&self, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
        // Diagnostic/cached entries resolve once. Ordinary dispatch supplies its
        // already selected callable through run_callable instead.
        let function = self.installed_symbol.or(self.recursive_symbol)
            .filter(|symbol| recursive_definition_matches(*symbol, Arc::as_ptr(&self.body)))
            .and_then(egcl_rt::symbols::symbol_function)
            .unwrap_or(NIL);
        self.run_callable(function, args, env)
    }

    pub(super) fn run_callable(&self, function: EgclVal, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
        self.run_activation(args, env, None, function)
    }

    fn run_activation(
        &self,
        args: &[EgclVal],
        env: &mut Env,
        osr: Option<OsrResume>,
        function: EgclVal,
    ) -> Result<EgclVal, EgclError> {
        egcl_rt::rooted!(function = function);
        let osr_offset = osr.as_ref().map(|resume| resume.offset);
        // Claim both inherited owners before preparation can fail or collect.
        // Tags remain in the rooted Env; the payload owns their exact tokens.
        let mut frame_guard = FrameGuard(osr.as_ref().map(|resume| resume.frame));
        let _catch_guard = CatchScopeGuard {
            base: osr.as_ref().map_or(env.catch_stack.len(), |resume| resume.state.catch_base),
            env,
        };
        egcl_rt::rooted!(args = args.to_vec());
        egcl_rt::rooted_ref!(_env = &mut *env);
        if osr_offset.is_none() && args.len() != usize::from(self.body.arity) {
            return Err(EgclError::ProgramError(
                "native transfer entry: wrong argument count".into(),
            ));
        }
        if osr_offset.is_none() {
            validate_declared_args(&self.body, &args)?;
        }
        let mut state = match osr {
            Some(resume) => *resume.state,
            None => ActivationState::prepare(self, env)?,
        };
        egcl_rt::rooted_ref!(_state = &mut state);
        let _depth = osr_offset.is_none().then(|| {
            NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
            NativeDepthGuard
        });
        // Every snapshot is reserved and rooted before machine entry. Cold
        // preparation only copies words; no native frame survives into fallback.
        let stack = egcl_rt::current_stack();
        // Optimized metadata currently owns exactly one logical definition.
        let frame_function = if egcl_rt::function::is_interpreted_function(*function) {
            *function
        } else {
            self.deopt_metadata.as_ref()
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
            unsafe { (*frame).code_info = self.code_info; }
            frame
        } else { stack
            .push_frame(frame_function, self.code_info, self.slots, FLAG_CALL)
            .ok_or_else(|| {
                EgclError::StackOverflow(
                    egcl_rt::current_fiber_id()
                        .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
                )
            })? };
        frame_guard.0 = Some(frame);
        if osr_offset.is_none() {
            bind_params(&self.body, frame, &args, None);
        }
        let _dynamic_scope_guard = DynamicScopeGuard {
            env,
            scopes: &mut state.dynamic_scopes,
        };
        let mut children = SegmentActivations::default();
        egcl_rt::rooted_ref!(_children = &mut children);
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
        let mut entry = EntryGuard {
            env: NATIVE_ENV.with(|slot| slot.replace(env)),
            capture: CAPTURE.with(|slot| slot.replace(&mut context)),
            error: NATIVE_ERROR.with(|slot| slot.take()),
        };
        egcl_rt::rooted_ref!(_entry = &mut entry);
        // An enclosing legacy recovery PC describes its own machine frame,
        // never this one or our Rust helpers. This entry has no fault maps yet.
        let _recovery = FaultRecoveryGuard {
            null: egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            stack: egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        };
        egcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
        // The segment owns a rooted EgclStack frame and all host-side
        // execution state at this point, so this is a safe entry poll: a
        // moving GC may park and scan before generated code starts. Native
        // loop/back-edge polling remains a separate emitter gate.
        if osr_offset.is_none() {
            egcl_rt::safepoint::poll_safepoint();
            if let Some(error) = pending_signal_error_for_current_execution() {
                return Err(error);
            }
        }
        // An OSR source has already polled. Its destination header polls again
        // with the inherited scopes installed; deliver pending errors there,
        // through normal transfer handling, rather than skipping their cleanup.
        let _ordinary_segment = super::call_table::enter_ordinary_segment();
        let outcome = if let Some(offset) = self.ordinary_entry {
            assert!(osr_offset.is_none(), "ordinary entry is not an OSR destination");
            let control = unsafe { native_frames::initialize() };
            let execution = unsafe { &*(*control).execution };
            // Keep the existing execution-specific emergency stack reserve.
            // This additional margin covers the bounded Rust/root-shim adapter;
            // generated child calls check their exact emitted stack bounds.
            let sp: usize;
            unsafe { std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags)); }
            let required = self.ordinary_stack_required.expect("ordinary stack admission bound")
                .checked_add(4096).ok_or_else(invalid_capture)?;
            if sp.checked_sub(required).is_none_or(|bottom| bottom as u64 <= unsafe { (*control).stack_limit }) {
                return Err(EgclError::StackOverflow(egcl_rt::current_fiber_id()
                    .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0))));
            }
            unsafe {
                execution.with_active_native_control(std::pin::Pin::new_unchecked(&mut *control), || {
                    native_transfer::invoke_ordinary_native_segment(
                        self.code.as_ptr().add(offset), execution, NIL, &args, stack)
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
        egcl_rt::rooted!(primary = outcome.value);
        egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
        if context.failure.is_some() {
            return Err(invalid_capture());
        }
        if let Some((next, offset)) = context.osr.take() {
            if outcome.exit != NativeExit::Transfer || error.is_some()
                || context.selected.is_some() || !state.can_osr()
            {
                return Err(invalid_capture());
            }
            let NativeCodeStorage::Mapped(code) = &next._storage else {
                return Err(invalid_capture());
            };
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
        // This path currently admits only tagged values. A future
        // unboxed emitter must supply its boxing/emergency contract.
        egcl_rt::rooted!(
            frames = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index) }
                .reconstruct(|_| unreachable!("tagged-only entry"))
                .map_err(|_| invalid_capture())?
        );
        let site = self.sites.sites().nth(index).ok_or_else(invalid_capture)?;
        let saved = frames
            .first()
            .filter(|saved| {
                frames.len() == 1
                    && saved.resume_pc == site.map().origin_bcp
                    && saved.locals.len() == usize::from(self.body.n_locals)
                    && saved.stack.len() <= usize::from(self.body.max_stack)
                    && matches!(
                        self.body.code.get(saved.resume_pc as usize),
                        Some(
                            Instr::CallNamed { .. }
                                | Instr::EvalHost(_)
                                | Instr::LoadFunction(_)
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
                    )
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
        unsafe { (*frame).code_info = std::ptr::null(); }
        for (index, value) in saved.locals.iter().chain(&saved.stack).enumerate() {
            unsafe { slot_set(frame, index as u16, *value) };
        }
        let specials = unsafe { &mut *context.specials };
        let restored_handlers = site
            .map()
            .control_scopes
            .iter()
            .filter(|scope| !matches!(scope.kind, ScopeKind::Cleanup { .. } | ScopeKind::SpecialBinding { .. }))
            .map(|scope| match scope.kind {
                ScopeKind::Block {
                    id,
                    resume_bcp,
                    register: false,
                } => Handler::Block {
                    block_id: id,
                    token: String::new(),
                    resume_bcp,
                    sp_restore: scope.sp_restore,
                },
                ScopeKind::Tagbody { id } => Handler::Tag {
                    tagbody_id: id,
                    sp_restore: scope.sp_restore,
                    token: None,
                    tag_bcps: Vec::new(),
                },
                ScopeKind::Unwind { cleanup_bcp } => Handler::Unwind {
                    cleanup_bcp,
                    sp_restore: scope.sp_restore,
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
            env_frame: None,
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

pub(super) fn fixed_tagged_parameters(body: &BytecodeFunction) -> bool {
    !body.has_env
        && !body.variadic
        && body.min_args == body.arity
        && body.max_args == Some(body.arity)
        && body.param_layout.len() == usize::from(body.arity)
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
        && body
            .param_types
            .iter()
            .all(|ty| matches!(ty, DeclaredType::Any))
}

fn scope_free_native_body(body: &BytecodeFunction, allow_values: bool) -> bool {
    local_control_native_body(body, allow_values, false)
}

fn local_control_native_body(body: &BytecodeFunction, allow_values: bool, allow_local_scopes: bool) -> bool {
    body.param_types
        .iter()
        .all(|ty| matches!(ty, DeclaredType::Any))
        && body.param_layout.len() == usize::from(body.arity)
        && body
            .param_layout
            .iter()
            .all(|(_, location)| matches!(location, VarLoc::Slot(_)))
        && body.code.iter().all(|op| {
            // Nested tagged callees support MV; optimized/direct-recursive
            // admission retains its existing restriction.
            (allow_values && matches!(op, Instr::SetValues(_) | Instr::TakeValuesToLocals { .. }))
            || (allow_local_scopes && matches!(op,
                Instr::PushTag { .. } | Instr::Go { .. } | Instr::PushCatch { .. } | Instr::Throw
                    | Instr::PushUnwind { .. } | Instr::EnterCleanupNormal { .. } | Instr::CleanupReturn
                    | Instr::PushHandlerCase { .. } | Instr::PopHandlerCase
                    | Instr::PushHandlerBind { .. } | Instr::PopHandlerBind
                    | Instr::PushRestartCase { .. } | Instr::PopRestartCase
                    | Instr::BindSpecial(_) | Instr::UnbindSpecial(_)))
            || matches!(
                op,
                Instr::Const(_)
                    | Instr::LoadLocal(_)
                    | Instr::StoreLocal(_)
                    | Instr::LoadGlobal(_) | Instr::StoreGlobal(_)
                    | Instr::Pop
                    | Instr::Dup
                    | Instr::ClearMv
                    | Instr::Br(_)
                    | Instr::BrIfFalse(_)
                    | Instr::BrIfTrue(_)
                    | Instr::CallNamed { .. }
                    | Instr::Return
                    | Instr::PushBlock {
                        register: false,
                        ..
                    }
                    | Instr::PopHandler
                    | Instr::ReturnFrom { .. }
            )
        })
}

fn recursive_definition_matches(symbol: u32, body: *const BytecodeFunction) -> bool {
    let Some(function) = egcl_rt::symbols::symbol_function(symbol) else {
        return false;
    };
    callable_definition_matches(symbol, function, body)
}

fn callable_definition_matches(symbol: u32, function: EgclVal, body: *const BytecodeFunction) -> bool {
    egcl_rt::function::is_interpreted_function(function)
        && replacement_function_value(symbol, function).is_none()
        && bytecode_for_callable(symbol, function).is_some_and(|(owner, current)|
            owner == symbol && std::ptr::eq(Arc::as_ptr(&current), body))
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
    unsafe { (*context).recursive_generation = generation; }
    true
}

/// Cold, sampled scheduling runs while the suspended caller is precisely
/// published and before a child frame exists. The actual admission is counted
/// later; a cap, changed definition or failed frame push counts via fallback.
unsafe fn schedule_recursive_promotion(context: *mut CaptureContext, symbol: u32, count: u32) -> Result<EgclVal, EgclError> {
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
            !installed.is_t2 && installed.body.as_ref().is_some_and(|body|
                std::ptr::eq(Arc::as_ptr(body), unsafe { (*context).body }))
        })
    });
    if needs_t2 {
        let priority = (u64::from(egcl_rt::function::back_edge_count(*function)) << 32) | u64::from(count);
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
    unsafe { poll_or_transfer_unpublished(std::ptr::null_mut(), out); }
    if unsafe { (*out).exit } != NativeExit::Returned { return; }
    let context = CAPTURE.with(Cell::get);
    let symbol = request.call.symbol as u32;
    if !unsafe { recursive_target_matches(context, symbol) } {
        unsafe { c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out); }
        return;
    }
    let here = std::ptr::from_ref(&request) as usize as u64;
    let limit = egcl_rt::stack::current_native_stack_limit();
    if NATIVE_DEPTH.with(|depth| depth.get() >= native_depth_cap()) || here <= limit {
        unsafe { c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out); }
        return;
    }
    if unsafe { (*context).recursive_profile && !(*(*context).owner).is_t2 } {
        let function = unsafe { (*(*context).frame).function };
        let count = egcl_rt::function::invoke_count(function).saturating_add(1);
        if count % t2_invoke_threshold().max(1) == 0 {
            if let Err(error) = guard_c2i(|| unsafe { schedule_recursive_promotion(context, symbol, count) }) {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
                unsafe { out.write(NativeOutcome { value: NIL, exit: NativeExit::Transfer }); }
                return;
            }
        }
    }
    // Publication may change linkage or collect. Reload the traced callable
    // from the owning frame and revalidate before committing any child state.
    if !unsafe { recursive_target_matches(context, symbol) } {
        unsafe { c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out); }
        return;
    }
    let context = unsafe { &mut *context };
    let function = unsafe { (*context.frame).function };
    let stack = egcl_rt::current_stack();
    let Some(frame) = stack.push_frame(
        function,
        unsafe { (*context.frame).code_info },
        context.slots as u16,
        FLAG_CALL,
    ) else {
        unsafe { c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out); }
        return;
    };
    let body = unsafe { &*context.body };
    for (index, (_, location)) in body.param_layout.iter().enumerate() {
        let VarLoc::Slot(slot) = location else { unreachable!("admitted slot parameter") };
        unsafe { slot_set(frame, *slot, request.call.args.add(index).read()); }
    }
    request.activation = RecursiveActivation { previous: context.recursive, frame, stack };
    context.recursive = std::ptr::from_mut(&mut request.activation);
    #[cfg(test)]
    RECURSIVE_ENTRIES.with(|count| count.set(count.get() + 1));
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
    if context.recursive_profile {
        egcl_rt::function::record_invocation(function);
    }
    c2i_clear_mv();
    // The untagged activation address goes straight to the generated entry.
    unsafe { out.write(NativeOutcome {
        value: EgclVal(frame.add(1) as u64), exit: NativeExit::Returned,
    }); }
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

struct FaultRecoveryGuard {
    null: usize,
    stack: usize,
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
        let stack = egcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let scopes = unsafe { &mut *self.scopes };
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

unsafe extern "C" fn call_or_throw(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    unsafe {
        root_publication::published(image, || call_or_throw_unpublished(request, out))
    }
}

unsafe fn call_or_throw_unpublished(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
) {
    use egcl_compiler::t2::emit::{
        TransferCallRequest, TRANSFER_CATCH_ENTER_REQUEST, TRANSFER_CATCH_LEAVE_REQUEST,
        TRANSFER_THROW_REQUEST,
    };
    use egcl_rt::native_transfer::NativeOutcome;
    let call = unsafe { &*request.cast::<TransferCallRequest>() };
    let request_kind = call.symbol & !u64::from(u32::MAX);
    use egcl_compiler::t2::emit::{
        TRANSFER_SPECIAL_BIND_REQUEST, TRANSFER_SPECIAL_UNBIND_REQUEST,
        TRANSFER_GLOBAL_LOAD_REQUEST, TRANSFER_GLOBAL_STORE_REQUEST,
    };
    if matches!(request_kind, TRANSFER_SPECIAL_BIND_REQUEST | TRANSFER_SPECIAL_UNBIND_REQUEST
        | TRANSFER_GLOBAL_LOAD_REQUEST | TRANSFER_GLOBAL_STORE_REQUEST) {
        let result = if native_error_pending() { Ok(NIL) } else {
            guard_c2i(|| unsafe { special_request(call) })
        };
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
                NIL
            }
        };
        unsafe { out.write(NativeOutcome {
            value,
            exit: if native_error_pending() { NativeExit::Transfer } else { NativeExit::Returned },
        }); }
        return;
    }
    use egcl_compiler::t2::emit::{
        TRANSFER_HANDLER_BIND_ENTER_REQUEST, TRANSFER_HANDLER_BIND_LEAVE_REQUEST,
        TRANSFER_HANDLER_ENTER_REQUEST, TRANSFER_HANDLER_LEAVE_REQUEST,
        TRANSFER_RESTART_CASE_ENTER_REQUEST, TRANSFER_RESTART_CASE_LEAVE_REQUEST,
        TRANSFER_HOST_EVAL_REQUEST, TRANSFER_FUNCTION_LOOKUP_REQUEST,
    };
    if matches!(
        request_kind,
        TRANSFER_HOST_EVAL_REQUEST | TRANSFER_FUNCTION_LOOKUP_REQUEST
    ) {
        let mut value = NIL;
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 0);
                let context = unsafe { &*CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                if request_kind == TRANSFER_HOST_EVAL_REQUEST {
                    let body = unsafe { &*context.body };
                    let form = *body.constants
                        .get(call.symbol as u32 as usize)
                        .ok_or_else(invalid_capture)?;
                    eval_form(form, env)
                } else {
                    let symbol = EgclVal::from_symbol_index(call.symbol as u32);
                    Ok(super::super::symbol_function_object(env, symbol).unwrap_or(symbol))
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
                        let captured_frame = Arc::clone(&env.frame);
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
                            restart_symbols.push(
                                resolve_sym(&restart.name).ok_or_else(invalid_capture)?,
                            );
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
                    env.catch_stack
                        .push((tag, try_clone_string(&token)?));
                    catches.push(SavedCatch { push_bcp, token });
                } else {
                    assert_eq!(call.nargs, 0);
                    let saved = catches.last().expect("live native catch");
                    assert_eq!(saved.push_bcp, push_bcp);
                    assert!(env
                        .catch_stack
                        .last()
                        .is_some_and(|(_, token)| token == &saved.token));
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
        unsafe { out.write(NativeOutcome { value: NIL, exit: NativeExit::Returned }) };
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
            if (*out).exit == NativeExit::Returned && prepare_osr(image) {
                (*out).exit = NativeExit::Transfer;
            }
        })
    }
}

/// A read-only preflight: all work that can park, yield, allocate, deliver a
/// signal, or prepare OSR stays behind the existing root publication.
fn idle_t2_poll(request: *mut u8) -> bool {
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
    !egcl_rt::safepoint::poll_work_pending()
        && PENDING_NATIVE_ERRORS.load(AtomicOrdering::Relaxed) == 0
        && !egcl_rt::runtime::process_signal_activity_pending()
        && !egcl_rt::any_posted_pending_signal()
        && !egcl_rt::sandbox_cpu_deadline_armed()
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
    let Some(context) = (unsafe { context.as_mut() }) else { return false };
    let code = unsafe { &*context.owner };
    if code.is_t2 || !context.recursive.is_null() {
        return false;
    }
    let Some(symbol) = code.installed_symbol else { return false };
    let Some(offset) = (unsafe { (*image).return_pc as usize }).checked_sub(context.code_base) else {
        return false;
    };
    let Some(header) = code.osr_polls.get(&offset) else { return false };
    let Some(next) = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned()) else {
        return false;
    };
    if !next.is_t2 || next.body.as_ref().is_none_or(|body| !Arc::ptr_eq(body, &code.body))
        || !recursive_definition_matches(symbol, Arc::as_ptr(&code.body))
        || !matches!(&next._storage, NativeCodeStorage::Mapped(_))
    {
        return false;
    }
    let Some(entry) = next.osr_entries.get(header).copied() else { return false };
    // Scan body admission only once a replacement is ready, not on every
    // baseline loop poll while the optimizing worker is still compiling.
    if !local_control_native_body(&code.body, true, true) {
        return false;
    }
    // An OSR jump keeps this activation's registrations, not new copies.
    // Validate exact lexical provenance against every live runtime ledger.
    let Some(scopes) = code.control_scopes.before(*header) else { return false };
    if scopes.iter().any(|scope| scope.ownership != Ownership::Local
        || !matches!(scope.kind, ScopeKind::Block { register: false, .. }
            | ScopeKind::Tagbody { .. } | ScopeKind::Catch { .. }
            | ScopeKind::Unwind { .. } | ScopeKind::Cleanup { .. }
            | ScopeKind::HandlerCase { .. } | ScopeKind::HandlerBind { .. }
            | ScopeKind::RestartCase { .. } | ScopeKind::SpecialBinding { .. }))
    {
        return false;
    }
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    if !unsafe { validate_condition_clusters(context, scopes, env)
        && validate_special_bindings(context, scopes) } { return false; }
    let cleanups = unsafe { &*context.cleanups };
    let mut saved_cleanups = cleanups.iter();
    let mut handler_depth = 0;
    for scope in scopes {
        if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
            let Some(saved) = saved_cleanups.next() else { return false };
            if saved.cleanup_bcp != cleanup_bcp || saved.continuation.handler_depth != handler_depth {
                return false;
            }
        } else if !matches!(scope.kind, ScopeKind::SpecialBinding { .. }) {
            handler_depth += 1;
        }
    }
    if saved_cleanups.next().is_some() || context.completed_cleanup.is_some() {
        return false;
    }
    let catches = unsafe { &*context.catches };
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    if !scopes.iter().filter(|scope| matches!(scope.kind, ScopeKind::Catch { .. }))
        .map(|scope| scope.push_bcp).eq(catches.iter().map(|saved| saved.push_bcp))
        || env.catch_stack.len() != context.catch_base + catches.len()
        || !env.catch_stack[context.catch_base..].iter().map(|(_, token)| token)
            .eq(catches.iter().map(|saved| &saved.token))
    {
        return false;
    }
    let Some(index) = code.sites.sites().position(|site| site.return_offset() as usize == offset) else {
        return false;
    };
    let snapshot = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index) };
    if unsafe { snapshot.capture_from_activation(context.code_base, &*image,
        std::slice::from_raw_parts(context.activation, context.slots)) }.is_err()
    {
        return false;
    }
    let Ok(frames) = snapshot.reconstruct(|_| unreachable!("tagged mapped OSR")) else {
        return false;
    };
    egcl_rt::rooted!(frames = frames);
    let [frame] = frames.as_slice() else { return false };
    if frame.resume_pc != *header || frame.locals.len() != usize::from(code.body.n_locals)
        || !frame.stack.is_empty()
    {
        return false;
    }
    // All destination allocation remains on the source's published side. A
    // failed reservation simply leaves this T1 loop and its cleanups running.
    #[cfg(test)]
    if FAIL_OSR_PREPARATION.with(|fail| fail.replace(false)) {
        return false;
    }
    let NativeCodeStorage::Mapped(destination) = &next._storage else { return false };
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let Ok(prepared) = ActivationState::prepare(destination, env) else { return false };
    let mut prepared = Box::new(prepared);
    egcl_rt::rooted_ref!(_prepared = &mut *prepared);
    let stack = egcl_rt::current_stack();
    if !native_transfer::is_supported() {
        return false;
    }
    let slots = destination.slots.max(unsafe { (*context.frame).num_locals });
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
    let Some(delta) = (unsafe { stack.grow_frame_with_special_suffix(context.frame, slots, code.code_info) }) else {
        return false;
    };
    unsafe { relocate_condition_clusters(context, delta); }
    for (slot, value) in frame.locals.iter().enumerate() {
        unsafe { slot_set(context.frame, slot as u16, *value) };
    }
    // End the local root before moving the box; the source activation now
    // owns and traces the prepared state's exact payload slots.
    drop(_prepared);
    unsafe { *context.ready_osr = Some(prepared); }
    context.osr = Some((next, entry));
    true
}

unsafe fn poll_or_transfer_unpublished(
    request: *mut u8,
    out: *mut egcl_rt::native_transfer::NativeOutcome,
) {
    use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
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
    let (symbol, body, function) = unsafe {
        let code = &*(*capture).owner;
        (code.installed_symbol, Arc::as_ptr(&code.body), (*(*capture).frame).function)
    };
    let Some(symbol) = symbol else {
        return;
    };
    // The published poll may have moved this callable. Retain its current
    // frame value, then validate one binding snapshot; a concurrent setter
    // cannot redirect this increment to a replacement or a nonfunction.
    egcl_rt::rooted!(function = function);
    if egcl_rt::symbols::symbol_function(symbol) != Some(*function)
        || !callable_definition_matches(symbol, *function, body)
    {
        return;
    }
    #[cfg(test)]
    AFTER_HEAT_VALIDATION.with(|hook| {
        if let Some(observe) = hook.get() { observe(); }
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
    unsafe {
        root_publication::published(image, || complete_cleanup_unpublished(request, out))
    }
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

struct CaptureContext {
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
    Some((context.owner as usize, !context.nested.is_null(), context.frame))
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
    }.map_err(|_| EgclError::Internal("published native callable boundary is unavailable".into()))?;
    egcl_rt::rooted!(primary = outcome.value);
    egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
    match outcome.exit {
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
        NATIVE_ERROR.with(|slot| slot.replace(self.error.take()));
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
        (&mut *context.snapshots.cast::<SysvSiteSnapshot>().add(index))
            .capture_from_activation(
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
        let selected = if matches!(
            error,
            EgclError::Internal(_) | EgclError::Signalled(_)
        ) {
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
    let keep_specials = scopes[..scope_index].iter()
        .filter(|scope| matches!(scope.kind, ScopeKind::SpecialBinding { .. })).count();
    let specials = unsafe { &mut *context.specials };
    while specials.len() > keep_specials { specials.pop(); }
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
