//! `bliss-rt` — Bliss Common Lisp runtime core.
//!
//! Provides the object model (tagged values, heap object layouts),
//! memory management (generational region-based GC), green-thread
//! scheduler, stack/frame layout, safepoint infrastructure, FFI
//! bridge, image persistence, and security sandbox.

// ── Object model ──────────────────────────────────────────────────
pub mod bignum;
pub mod asm;
pub mod bfasl;
pub mod bytecode;
/// Portable fiber context switch (no libc ucontext) — bliss-bca.5.
pub mod context;
pub mod function;
pub mod fxhash;
pub mod object;
pub mod packages;
pub mod symbols;
/// Direct Linux syscalls (no libc) for runtime-internal OS services (bliss-bca.5).
pub mod syscall;
pub mod types;
pub mod value;

// ── Memory / GC ───────────────────────────────────────────────────
pub mod gc;
pub mod jit;
pub mod lock_order;

// ── Thread runtime ────────────────────────────────────────────────
pub mod safepoint;
pub mod scheduler;
pub mod stack;
pub mod sync;
pub mod thread;

// ── FFI ───────────────────────────────────────────────────────────
pub mod ffi;

// ── Image persistence ─────────────────────────────────────────────
pub mod image;

// ── Security sandbox ──────────────────────────────────────────────
pub mod sandbox;

// ── Error types ───────────────────────────────────────────────────
pub mod error;
pub mod events;
pub mod log;

// ── Top-level entry ───────────────────────────────────────────────
pub mod runtime;

// ── Re-exports for convenience ────────────────────────────────────
pub use error::BlissError;
pub use ffi::{AlienType, Callback, load_foreign_library, marshal_to_c, unmarshal_from_c};
pub use gc::{
    Allocator, Collector, CrossThreadRoot, GcConfig, GcStats, HeapAllocator, HeapCollector,
    RegionHeader, RegionKind, SatbCardBarrier, ShadowRoot, ShadowRootScope, Tlab, WeakPointer,
    WriteBarrier, alloc_typed, collect_t0_minor, drain_satb_log, full_gc, heap_stats, init_heap,
    pin, register_finalizer, remembered_set_len, set_offheap_hooks, store_ref, unpin, walk_heap,
    write_barrier,
};
pub use image::{
    Arch, ImageCompression, ImageHeader, Os, SaveImageOptions, SectionEntry, SectionType,
    current_platform_tag, find_appended_image, load_image, load_image_from_bytes, platform_tag,
    save_image, validate_image_header,
};
pub use object::ObjectHeader;
pub use runtime::{
    LogLevel, Runtime, RuntimeConfig, check_sigfpe, check_sigint, check_sigpipe,
    check_sigsegv_null_guard, check_sigsegv_stack_guard, check_sigterm, install_signal_handlers,
    parse_cli, set_runtime_init_hook,
};
pub use safepoint::{
    SafepointPage, enter_safepoint, poll_safepoint, resume_all_threads, wait_for_all_threads,
};
pub use sandbox::{Sandbox, SandboxPolicy};
pub use scheduler::{Scheduler, SchedulerConfig, SchedulerGroup, run_fibers};
pub use stack::{
    BlissStack, CodeInfo, Frame, FrameType, FrameWalker, SourceLocation, SourceLocationEntry,
    StackMapEntry, eval_stack_budget, visit_stack_refs,
};
pub use sync::{
    BlissCondVar, BlissMutex, BlissSemaphore, IoInterest, PinnedBlockingAction, fiber_sleep,
    set_pinned_blocking_action, wait_fd,
};
pub use thread::{
    Fiber, FiberContinuation, FiberId, FiberState, NativeThread, NativeThreadId, NativeThreadState,
    PendingSignal, all_fiber_ids, all_thread_ids, carrier_thread_ids,
    clear_current_sandbox_cpu_deadline, current_fiber, current_fiber_id, current_stack,
    current_thread, current_thread_id, fiber_carrier_thread, fiber_state, fiber_yield,
    interrupt_fiber, interrupt_thread, join_fiber, join_thread, make_fiber, make_thread,
    park_current_fiber, poll_current_sandbox_cpu_deadline, post_current_pending_signal,
    post_foreground_pending_signal, set_current_execution_foreground, set_thread_entry_runner,
    start_current_sandbox_cpu_deadline, submit_fiber, take_current_pending_signal,
    thread_is_carrier, thread_yield, with_current_condition_state_mut,
};
pub use value::BlissVal;
