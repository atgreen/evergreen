//! `bliss-rt` — Bliss Common Lisp runtime core.
//!
//! Provides the object model (tagged values, heap object layouts),
//! memory management (generational region-based GC), green-thread
//! scheduler, stack/frame layout, safepoint infrastructure, FFI
//! bridge, image persistence, and security sandbox.

// ── Object model ──────────────────────────────────────────────────
pub mod asm;
pub mod bfasl;
pub mod bytecode;
pub mod function;
pub mod object;
pub mod packages;
pub mod symbols;
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

// ── Top-level entry ───────────────────────────────────────────────
pub mod runtime;

// ── Re-exports for convenience ────────────────────────────────────
pub use error::BlissError;
pub use ffi::{load_foreign_library, marshal_to_c, unmarshal_from_c, AlienType, Callback};
pub use gc::{
    alloc_typed, collect_t0_minor, drain_satb_log, full_gc, heap_stats, init_heap, pin,
    register_finalizer, remembered_set_len, store_ref, unpin, walk_heap, write_barrier, Allocator,
    Collector, GcConfig, GcStats, HeapAllocator, HeapCollector, RegionHeader, RegionKind,
    SatbCardBarrier, ShadowRoot, ShadowRootScope, Tlab, WeakPointer, WriteBarrier,
};
pub use image::{
    current_platform_tag, find_appended_image, load_image, platform_tag, save_image,
    validate_image_header, Arch, ImageCompression, ImageHeader, Os, SaveImageOptions, SectionEntry,
    SectionType,
};
pub use object::ObjectHeader;
pub use runtime::{
    check_sigint, install_signal_handlers, parse_cli, set_runtime_init_hook, LogLevel, Runtime,
    RuntimeConfig,
};
pub use safepoint::{
    enter_safepoint, poll_safepoint, resume_all_threads, wait_for_all_threads, SafepointPage,
};
pub use sandbox::{Sandbox, SandboxPolicy};
pub use scheduler::{run_fibers, Scheduler, SchedulerConfig, SchedulerGroup};
pub use stack::{
    eval_stack_budget, visit_stack_refs, BlissStack, CodeInfo, Frame, FrameType, FrameWalker,
    SourceLocation, SourceLocationEntry, StackMapEntry,
};
pub use sync::{
    fiber_sleep, set_pinned_blocking_action, wait_fd, BlissCondVar, BlissMutex, BlissSemaphore,
    IoInterest, PinnedBlockingAction,
};
pub use thread::{
    all_fiber_ids, all_thread_ids, carrier_thread_ids, current_fiber, current_fiber_id,
    current_stack, current_thread, current_thread_id, fiber_carrier_thread, fiber_state,
    fiber_yield, interrupt_fiber, interrupt_thread, join_fiber, join_thread, make_fiber,
    make_thread, park_current_fiber, submit_fiber, thread_is_carrier, thread_yield, Fiber,
    FiberContinuation, FiberId, FiberState, NativeThread, NativeThreadId, NativeThreadState,
};
pub use value::BlissVal;
