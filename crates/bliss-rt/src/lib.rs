//! `bliss-rt` — Bliss Common Lisp runtime core.
//!
//! Provides the object model (tagged values, heap object layouts),
//! memory management (generational region-based GC), green-thread
//! scheduler, stack/frame layout, safepoint infrastructure, FFI
//! bridge, image persistence, and security sandbox.

// ── Object model ──────────────────────────────────────────────────
pub mod object;
pub mod types;
pub mod value;

// ── Memory / GC ───────────────────────────────────────────────────
pub mod gc;
pub mod jit;

// ── Thread runtime ────────────────────────────────────────────────
pub mod safepoint;
pub mod scheduler;
pub mod stack;
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
pub use ffi::{AlienType, Callback, load_foreign_library, marshal_to_c, unmarshal_from_c};
pub use gc::{
    Allocator, Collector, GcConfig, GcStats, HeapAllocator, HeapCollector, RegionHeader,
    RegionKind, SatbCardBarrier, Tlab, WeakPointer, WriteBarrier, alloc_typed, drain_satb_log,
    full_gc, heap_stats, init_heap, register_finalizer, remembered_set_len, store_ref, walk_heap,
    write_barrier,
};
pub use image::{
    Arch, ImageCompression, ImageHeader, Os, SaveImageOptions, SectionEntry, SectionType,
    current_platform_tag, find_appended_image, load_image, platform_tag, save_image,
    validate_image_header,
};
pub use object::ObjectHeader;
pub use runtime::{
    LogLevel, Runtime, RuntimeConfig, check_sigint, install_signal_handlers, parse_cli,
    set_runtime_init_hook,
};
pub use safepoint::{
    SafepointPage, enter_safepoint, poll_safepoint, resume_all_threads, wait_for_all_threads,
};
pub use sandbox::{Sandbox, SandboxPolicy};
pub use scheduler::{Scheduler, SchedulerConfig};
pub use stack::{
    BlissStack, CodeInfo, Frame, FrameType, FrameWalker, SourceLocation, SourceLocationEntry,
    StackMapEntry, eval_stack_budget, visit_stack_refs,
};
pub use thread::{
    GreenThread, GreenThreadId, ThreadState, WorkerThread, all_thread_ids, current_thread,
    current_thread_id, interrupt_thread, join_thread, make_thread, thread_yield,
};
pub use value::BlissVal;
