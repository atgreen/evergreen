//! CL stack layout and frame walking.
//!
//! Each green thread owns a `EgclStack` — a contiguous virtual memory
//! region for CL control/value frames. See §2.4 of the spec.

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::value::{TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT, EgclVal};

/// CL stack for a green thread.
/// Default usable size: 512 KiB (configurable via EGCL_STACK_SIZE).
///
/// The stack is owned by exactly one green thread, which is the only mutator
/// of `sp_offset`/`fp`; the GC reads only the `published_*` snapshot taken at a
/// safepoint. `sp_offset`/`fp` therefore use `Cell` (single-threaded interior
/// mutability) so frame push/pop can go through a shared `&EgclStack` (the
/// only handle `Fiber::stack()`/`NativeThread::stack()` hands out) without `&mut`.
pub struct EgclStack {
    /// mmap allocation base, including the protected guard page.
    mapping: *mut u8,
    mapping_len: usize,
    /// Usable stack base, directly after the low guard page.
    base: *mut u8,
    capacity: usize,
    guard: *mut u8,
    /// Stack pointer offset from base (grows upward from base).
    sp_offset: Cell<usize>,
    /// Frame pointer (null if no frames pushed).
    fp: Cell<*mut Frame>,
    /// Published stack pointer for GC scanning while thread is parked at a safepoint.
    published_sp: AtomicUsize,
    /// Published frame pointer for GC scanning while thread is parked at a safepoint.
    published_fp: AtomicPtr<Frame>,
}

impl EgclStack {
    /// Allocate a new stack with the given usable size in bytes.
    /// Sets up guard pages for overflow detection.
    pub fn new(size: usize) -> Self {
        let page = stack_page_size();
        if size == 0 {
            let dangling = NonNull::<u8>::dangling().as_ptr();
            return EgclStack {
                mapping: std::ptr::null_mut(),
                mapping_len: 0,
                base: dangling,
                capacity: 0,
                guard: std::ptr::null_mut(),
                sp_offset: Cell::new(0),
                fp: Cell::new(std::ptr::null_mut()),
                published_sp: AtomicUsize::new(0),
                published_fp: AtomicPtr::new(std::ptr::null_mut()),
            };
        }

        let usable_len = align_up(size, page);
        let mapping_len = page
            .checked_add(usable_len)
            .and_then(|n| n.checked_add(page))
            .expect("EgclStack mapping size overflow");
        let mapping = unsafe {
            crate::syscall::mmap(
                std::ptr::null_mut(),
                mapping_len,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                crate::syscall::MAP_PRIVATE | crate::syscall::MAP_ANONYMOUS,
                -1,
                0,
            )
            .expect("EgclStack mmap failed")
        };
        let guard = unsafe { mapping.add(page + usable_len) };
        unsafe {
            crate::syscall::mprotect(guard, page, crate::syscall::PROT_NONE)
                .expect("EgclStack guard mprotect failed");
        }
        crate::runtime::register_sigsegv_stack_guard_range(guard as usize, page);

        EgclStack {
            mapping,
            mapping_len,
            base: unsafe { mapping.add(page) },
            capacity: size,
            guard,
            sp_offset: Cell::new(0),
            fp: Cell::new(std::ptr::null_mut()),
            published_sp: AtomicUsize::new(0),
            published_fp: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Get the base (lowest) address of the stack.
    pub fn base(&self) -> *const u8 {
        self.base as *const u8
    }

    /// Mutable base pointer into the backing buffer.
    ///
    /// SAFETY: the buffer is a fixed-size, single-owner allocation; the only
    /// mutator is this thread's interpreter, which never holds a `&[u8]`/
    /// `&mut [u8]` slice over the same region while frames are live.
    fn base_mut(&self) -> *mut u8 {
        self.base
    }

    /// Byte offsets of the fields the T1 direct-call codegen reads/writes from
    /// emitted x86 (bliss-zhvn). Derived with `offset_of!` so they track the
    /// struct layout automatically — never hardcode these in the emitter.
    /// `sp_offset`/`fp` are `Cell` (repr(transparent)), so the offset is the
    /// value's offset. `base` is a plain `*mut u8`.
    pub const OFFSET_BASE: usize = core::mem::offset_of!(EgclStack, base);
    pub const OFFSET_SP_OFFSET: usize = core::mem::offset_of!(EgclStack, sp_offset);
    pub const OFFSET_FP: usize = core::mem::offset_of!(EgclStack, fp);
    pub const OFFSET_CAPACITY: usize = core::mem::offset_of!(EgclStack, capacity);

    /// Get the current stack pointer.
    pub fn sp(&self) -> *const u8 {
        unsafe { self.base.add(self.sp_offset.get()) }
    }

    /// Get the current frame pointer.
    pub fn fp(&self) -> *const Frame {
        self.fp.get()
    }

    /// Returns total usable size in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns bytes currently in use.
    pub fn used(&self) -> usize {
        self.sp_offset.get()
    }

    /// Base address of the protected overflow guard page, if this stack is nonempty.
    pub fn guard_base(&self) -> Option<*const u8> {
        (!self.guard.is_null()).then_some(self.guard as *const u8)
    }

    /// Publish the current sp and fp so the GC can scan this thread's
    /// stack while it is parked at a safepoint (§2.5.3).
    pub fn publish_top(&self) {
        self.published_sp
            .store(self.sp_offset.get(), Ordering::Release);
        self.published_fp.store(self.fp.get(), Ordering::Release);
    }

    // ── Frame push / pop (D2.03) ───────────────────────────────────

    /// Push a CL activation frame (§2.4.2 header + `num_slots` `EgclVal`
    /// value slots) onto the stack and make it the current frame.
    ///
    /// The value-slot area holds the interpreter frame's lexical locals
    /// followed by its operand stack (D2.03); the caller decides the split.
    /// Slots are zero-initialised to `NIL`.
    ///
    /// Returns the new frame pointer, or `None` if the stack has no room —
    /// the single-stack replacement for the tree-walker's host-SP guard:
    /// the caller maps `None` to `EgclError::StackOverflow` →
    /// `STORAGE-CONDITION` (R2.20).
    pub fn push_frame(
        &self,
        function: EgclVal,
        code_info: *const CodeInfo,
        num_slots: u16,
        flags: u32,
    ) -> Option<*mut Frame> {
        let header = std::mem::size_of::<Frame>();
        // Value slots follow the header; both are 8-byte aligned so the
        // header's natural alignment keeps the slot area aligned too.
        let start = align_up(self.sp_offset.get(), std::mem::align_of::<Frame>());
        let frame_bytes = header + num_slots as usize * std::mem::size_of::<EgclVal>();
        let end = start.checked_add(frame_bytes)?;
        if end > self.capacity {
            return None;
        }

        // SAFETY: `start .. end` is within the backing buffer (checked above),
        // 8-byte aligned, and not aliased by any live frame.
        let frame_ptr = unsafe { self.base_mut().add(start) } as *mut Frame;
        unsafe {
            frame_ptr.write(Frame {
                prev_fp: self.fp.get(),
                return_pc: std::ptr::null(),
                function,
                code_info,
                flags,
                num_locals: num_slots,
                _pad: 0,
            });
            let slots = frame_ptr.add(1) as *mut EgclVal;
            for i in 0..num_slots as usize {
                slots.add(i).write(crate::value::NIL);
            }
        }
        self.fp.set(frame_ptr);
        self.sp_offset.set(end);
        Some(frame_ptr)
    }

    /// Grow the current activation in place for an optimized OSR entry and
    /// install its stack map. Existing frame/slot addresses remain stable;
    /// added slots are initialized to NIL before the larger frame is visible.
    /// The supplied map must describe the tagged slots used by the new code.
    /// No allocation or safepoint occurs. Failure leaves the stack unchanged.
    pub fn grow_top_frame(
        &self,
        num_slots: u16,
        code_info: &'static CodeInfo,
    ) -> Option<*mut Frame> {
        let frame = self.fp.get();
        if frame.is_null() {
            return None;
        }
        // SAFETY: the current frame is owned by this stack's sole mutator.
        let old_slots = unsafe { (*frame).num_locals };
        let extra =
            usize::from(num_slots.checked_sub(old_slots)?) * std::mem::size_of::<EgclVal>();
        let end = self.sp_offset.get().checked_add(extra)?;
        if end > self.capacity {
            return None;
        }
        // SAFETY: no younger frame exists, the added range fits in the stack,
        // and the GC only reads snapshots published at explicit safepoints.
        unsafe {
            let slots = frame.add(1) as *mut EgclVal;
            for index in old_slots..num_slots {
                slots.add(usize::from(index)).write(crate::value::NIL);
            }
            (*frame).num_locals = num_slots;
            (*frame).code_info = code_info;
        }
        self.sp_offset.set(end);
        Some(frame)
    }

    /// Pop the current frame, restoring `fp` to its `prev_fp` and rewinding
    /// `sp` to just below the popped frame.
    ///
    /// # Panics (debug)
    /// Panics in debug builds if there is no current frame.
    pub fn pop_frame(&self) {
        let fp = self.fp.get();
        debug_assert!(!fp.is_null(), "pop_frame with empty stack");
        if fp.is_null() {
            return;
        }
        // SAFETY: `fp` is a frame this stack pushed; its `prev_fp` and address
        // are valid. `offset_from` is within the same allocation.
        unsafe {
            let start = (fp as *const u8).offset_from(self.base as *const u8) as usize;
            self.sp_offset.set(start);
            self.fp.set((*fp).prev_fp);
        }
    }

    /// Number of frames currently on the stack (walks the `prev_fp` chain).
    pub fn frame_depth(&self) -> usize {
        // SAFETY: fp is null or a valid frame this stack pushed.
        unsafe { FrameWalker::new(self.fp.get()).count() }
    }

    /// Mutable view of a frame's value-slot area.
    ///
    /// # Safety
    /// `frame` must be a live frame previously returned by [`push_frame`] on
    /// this stack, with the same `num_slots`.
    pub unsafe fn frame_slots_mut<'a>(frame: *mut Frame) -> &'a mut [EgclVal] {
        unsafe {
            let n = (*frame).num_locals as usize;
            if n == 0 {
                return &mut [];
            }
            let ptr = frame.add(1) as *mut EgclVal;
            std::slice::from_raw_parts_mut(ptr, n)
        }
    }

    /// Read the published stack pointer offset (for GC scanning).
    pub fn published_sp(&self) -> usize {
        self.published_sp.load(Ordering::Acquire)
    }

    /// Read the published frame pointer (for GC scanning).
    pub fn published_fp(&self) -> *const Frame {
        self.published_fp.load(Ordering::Acquire)
    }
}

impl Drop for EgclStack {
    fn drop(&mut self) {
        if !self.guard.is_null() {
            crate::runtime::unregister_sigsegv_stack_guard_range(self.guard as usize);
        }
        if !self.mapping.is_null() && self.mapping_len != 0 {
            let _ = unsafe { crate::syscall::munmap(self.mapping, self.mapping_len) };
        }
    }
}

/// Byte budget for the interpreter's *host* (Rust) call-stack use before it must
/// raise `STORAGE-CONDITION` rather than let the native stack overflow into a
/// process-killing signal (R2.20). Derived from the OS soft stack limit
/// (`RLIMIT_STACK`) minus a red zone, so the guard fires with headroom to spare;
/// overridable with `EGCL_MAX_EVAL_STACK_BYTES`.
///
/// This is an interim guard for the tree-walker, whose activations live on the
/// Rust stack. Once CL activations move onto the per-green-thread `EgclStack`
/// (bliss-nmq), overflow is bounded by that stack's own capacity instead.
pub fn eval_stack_budget() -> usize {
    // Red zone left below the OS limit. The guard is polled at every CL call
    // boundary, so the most stack that can be consumed between two checks is a
    // single call's worth of host frames (kilobytes) — 1 MiB is ample headroom
    // to unwind and run a handler after the guard fires.
    const RED_ZONE: usize = 1024 * 1024;
    #[cfg(unix)]
    const FLOOR: usize = 1024 * 1024;
    #[cfg(not(windows))]
    const DEFAULT: usize = 7 * 1024 * 1024;

    if let Ok(v) = std::env::var("EGCL_MAX_EVAL_STACK_BYTES") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }

    #[cfg(unix)]
    {
        // prlimit64(RLIMIT_STACK): the soft stack limit sizes fiber stacks.
        if let Ok(rl) = crate::syscall::getrlimit(crate::syscall::RLIMIT_STACK) {
            let soft = rl.rlim_cur;
            if soft != 0 && soft != crate::syscall::RLIM_INFINITY {
                return (soft as usize).saturating_sub(RED_ZONE).max(FLOOR);
            }
        }
    }

    #[cfg(windows)]
    {
        let (low, high) = crate::syscall::thread_stack_limits();
        let red_zone = if crate::thread::current_fiber_id().is_some() {
            128 * 1024
        } else {
            RED_ZONE
        };
        high.saturating_sub(low).saturating_sub(red_zone)
    }
    #[cfg(not(windows))]
    {
        DEFAULT
    }
}

/// The shallowest host-stack frame address this thread has probed. The stack
/// grows down, so the distance from it to the current frame is how much host
/// stack the thread is using.
static HOST_STACK_BASE: crate::execution_local::ExecutionLocal<std::cell::Cell<usize>> =
    unsafe { crate::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(0)) };

/// True when this thread has used its whole [`eval_stack_budget`] of HOST (Rust)
/// stack, so the caller must raise `STORAGE-CONDITION` instead of recursing into
/// the guard page (R2.20).
///
/// The budget leaves a red zone below the OS limit, and this is polled at call
/// boundaries where at most one call's frames (kilobytes) can be consumed
/// between polls — so there is always room left to unwind and run a handler.
///
/// Unlike the `EgclStack` guard page, which bounds CL frames, nothing bounded
/// the Rust frames a nest of `apply_function` calls consumes: a runaway Lisp
/// recursion through FUNCALL killed the process with SIGSEGV, and a legitimately
/// deep one (Serapeum's type walkers, whose own handlers recover from
/// STORAGE-CONDITION) could not load at all (bliss-3zvm).
#[inline]
pub fn host_stack_budget_exhausted() -> bool {
    #[cfg(not(windows))]
    static BUDGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let local = 0usize;
    let probe = std::hint::black_box(&local) as *const usize as usize;
    if let Some((low, high)) =
        crate::thread::current_fiber().and_then(|fiber| fiber.native_stack_bounds())
    {
        if probe < low || probe > high || probe.saturating_sub(low) < 128 * 1024 {
            return true;
        }
    }
    // Windows threads can have different reservations. Check the actual
    // remaining stack as well as an optional user budget; never let a large
    // override move the guard below the thread's allocation boundary.
    #[cfg(windows)]
    {
        let (low, high) = crate::syscall::thread_stack_limits();
        let reserve = if crate::thread::current_fiber().is_some() {
            128 * 1024
        } else {
            1024 * 1024
        };
        if probe < low || probe > high || probe.saturating_sub(low) < reserve {
            return true;
        }
    }
    HOST_STACK_BASE.with(|base| {
        let recorded = base.get();
        // A shallower frame than any seen before establishes the reference point:
        // the first poll on the thread, or a later one from further out.
        if probe > recorded {
            base.set(probe);
            return false;
        }
        #[cfg(windows)]
        let budget = {
            static BUDGET: crate::execution_local::ExecutionLocal<std::cell::OnceCell<usize>> =
                unsafe { crate::execution_local::ExecutionLocal::new(std::cell::OnceCell::new) };
            BUDGET.with(|b| *b.get_or_init(eval_stack_budget))
        };
        #[cfg(not(windows))]
        let budget = *BUDGET.get_or_init(eval_stack_budget);
        let budget = crate::thread::current_fiber()
            .map(|fiber| budget.min(fiber.native_stack_size().saturating_sub(128 * 1024)))
            .unwrap_or(budget);
        recorded - probe > budget
    })
}

/// Round `n` up to the next multiple of `align` (a power of two).
#[inline]
fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

#[inline]
fn stack_page_size() -> usize {
    crate::syscall::page_size()
}

// ── Frame layout ───────────────────────────────────────────────────

/// Fixed-layout frame header (40 bytes). D2.02.
///
/// Followed by a variable-size locals area: `locals[0..num_locals]: EgclVal`.
#[repr(C)]
pub struct Frame {
    /// Link to previous frame (for stack walking).
    pub prev_fp: *mut Frame,
    /// Return address in native code.
    pub return_pc: *const u8,
    /// The function object for this frame.
    pub function: EgclVal,
    /// Pointer to safepoint map + source location table.
    pub code_info: *const CodeInfo,
    /// Frame type and flags.
    pub flags: u32,
    /// Number of local variable slots.
    pub num_locals: u16,
    pub _pad: u16,
}

/// Frame type (bits 1:0 of `flags`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Call = 0b00,
    Catch = 0b01,
    Unwind = 0b10,
    Special = 0b11,
}

impl Frame {
    /// Extract the frame type from the low 2 bits of flags.
    pub fn frame_type(&self) -> FrameType {
        match self.flags & 0b11 {
            0b00 => FrameType::Call,
            0b01 => FrameType::Catch,
            0b10 => FrameType::Unwind,
            0b11 => FrameType::Special,
            _ => unreachable!(),
        }
    }

    /// Get a slice of local variables in this frame.
    /// Locals are stored contiguously right after the Frame header.
    ///
    /// # Safety
    /// Frame must be valid and `num_locals` must be correct.
    pub unsafe fn locals(&self) -> &[EgclVal] {
        if self.num_locals == 0 {
            return &[];
        }
        unsafe {
            let locals_ptr = (self as *const Frame).add(1) as *const EgclVal;
            std::slice::from_raw_parts(locals_ptr, self.num_locals as usize)
        }
    }
}

// ── CodeInfo ───────────────────────────────────────────────────────

/// Metadata about a compiled function's code, used for GC stack maps
/// and debugger source-location mapping.
#[derive(Clone, Debug)]
pub struct SourceLocationEntry {
    pub pc_offset: usize,
    pub location: SourceLocation,
}

#[derive(Clone, Copy, Debug)]
pub struct StackMapEntry {
    pub pc_offset: usize,
    pub bytes: usize,
    pub len: usize,
}

pub struct CodeInfo {
    source_locations: &'static [SourceLocationEntry],
    stack_maps: &'static [StackMapEntry],
}

impl CodeInfo {
    /// Empty metadata for tests/bootstrap frames that have no compiled maps.
    pub const fn empty() -> Self {
        Self {
            source_locations: &[],
            stack_maps: &[],
        }
    }

    /// Create code metadata from static tables emitted by the compiler.
    pub fn new(
        source_locations: &'static [SourceLocationEntry],
        stack_maps: &'static [StackMapEntry],
    ) -> &'static Self {
        Box::leak(Box::new(CodeInfo {
            source_locations: Box::leak(source_locations.to_vec().into_boxed_slice()),
            stack_maps: Box::leak(stack_maps.to_vec().into_boxed_slice()),
        }))
    }

    fn source_location_entries(&self) -> &[SourceLocationEntry] {
        self.source_locations
    }

    fn stack_map_entries(&self) -> &[StackMapEntry] {
        self.stack_maps
    }

    /// Look up the source location for a given PC offset.
    pub fn source_location(&self, pc_offset: usize) -> Option<SourceLocation> {
        let entries = self.source_location_entries();
        let idx = entries.partition_point(|entry| entry.pc_offset <= pc_offset);
        if idx == 0 {
            None
        } else {
            Some(entries[idx - 1].location.clone())
        }
    }

    /// Get the GC stack map for a given safepoint PC offset.
    pub fn stack_map(&self, pc_offset: usize) -> Option<&[u8]> {
        let entries = self.stack_map_entries();
        let idx = entries.partition_point(|entry| entry.pc_offset <= pc_offset);
        if idx == 0 {
            return None;
        }
        let entry = &entries[idx - 1];
        if entry.bytes == 0 || entry.len == 0 {
            None
        } else {
            Some(unsafe { std::slice::from_raw_parts(entry.bytes as *const u8, entry.len) })
        }
    }
}

/// Source location (file, line, column).
#[derive(Clone, Debug)]
pub struct SourceLocation {
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
}

// ── Precise CL-stack scanning (nmq.3) ──────────────────────────────

/// Is `v` a heap reference (points to a GC-managed object)? Fixnums,
/// characters, single-floats, symbols, and the special immediates are not.
#[inline]
fn is_heap_reference(v: EgclVal) -> bool {
    matches!(v.tag(), TAG_CONS | TAG_HEAP_OBJECT | TAG_FUNCTION)
}

/// Visit every heap-reference slot in the CL frames reachable from `fp`,
/// **precisely** — each slot is a tagged `EgclVal`, so references are
/// identified exactly by tag, with no conservative pinning of non-reference CL
/// data (fixnums, chars, …). The visitor receives a mutable pointer to each
/// reference slot so the GC can both mark the referent and update the slot when
/// the object relocates (§2.4.4 "GC of the control stack").
///
/// Interpreter (T0) frames and compiled (T1) frames share the §2.4.2 layout, so
/// one walk covers mixed-tier stacks.
///
/// # Safety
/// `fp` must be null or point to a valid frame chain whose `num_locals` are
/// correct (as produced by [`EgclStack::push_frame`]).
pub unsafe fn visit_stack_refs(fp: *const Frame, mut visit: impl FnMut(&mut EgclVal)) {
    let mut cur = fp;
    while !cur.is_null() {
        let frame = cur as *mut Frame;
        // SAFETY: caller guarantees a valid frame chain.
        unsafe {
            let n = (*frame).num_locals as usize;
            let slots = frame.add(1) as *mut EgclVal;
            // A compiled frame carries a GC stack map (via its CodeInfo) that says
            // which activation slots hold references at this safepoint; an
            // interpreter frame has no map, and every slot is a tagged EgclVal
            // (bliss-jtc.4). Either way a marked slot is a live reference only if
            // its tag says so, so scanning stays precise.
            let code_info = (*frame).code_info;
            let bitmap: Option<&[u8]> = if code_info.is_null() {
                None
            } else {
                (*code_info).stack_map(0)
            };
            for i in 0..n {
                let scan = match bitmap {
                    // Compiled frame: consult the stack-map ref bitmap (bounds-
                    // guarded against a short/stale map).
                    Some(bm) => (i / 8) < bm.len() && (bm[i / 8] >> (i % 8)) & 1 == 1,
                    // No map: interpreter frame (or a compiled frame with no
                    // reference slots) — scan every slot by tag.
                    None => true,
                };
                if scan {
                    let slot = &mut *slots.add(i);
                    if is_heap_reference(*slot) {
                        visit(slot);
                    }
                }
            }
            cur = (*frame).prev_fp as *const Frame;
        }
    }
}

// ── Frame walker ───────────────────────────────────────────────────

/// Iterator over CL stack frames via the prev_fp chain.
pub struct FrameWalker {
    current: *const Frame,
}

impl FrameWalker {
    /// Create a frame walker starting from the given frame pointer.
    ///
    /// # Safety
    /// `fp` must point to a valid `Frame` or be null.
    pub unsafe fn new(fp: *const Frame) -> Self {
        FrameWalker { current: fp }
    }
}

impl Iterator for FrameWalker {
    type Item = *const Frame;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current.is_null() {
            return None;
        }
        let frame = self.current;
        // Walk to the previous frame via prev_fp.
        self.current = unsafe { (*frame).prev_fp as *const Frame };
        Some(frame)
    }
}

/// Lowest stack address compiled code may recurse to before it must stop taking
/// direct (non-c2i) calls. Read by JIT-emitted code at the address
/// [`native_stack_limit_addr`] reports.
///
/// Zero means "no limit known", and compiled code then behaves exactly as it did
/// before this existed — so a runtime that never calls [`set_native_stack_limit`]
/// is unaffected.
///
/// This exists because T2's direct self-call skips `c2i_call_args`, and with it
/// the `native_depth_cap()` check that is the only bound on recursion depth in
/// compiled code — the T2 prologue has no stack guard. Without a limit a deeply
/// self-recursive function runs off the C stack and returns a WRONG ANSWER:
/// `(deep 400000)` answered 30, and `(deep 200000)` answered a raw stack address
/// (bliss-b4fd).
pub static NATIVE_STACK_LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Address of [`NATIVE_STACK_LIMIT`], for the code emitter to bake into a load.
pub fn native_stack_limit_addr() -> u64 {
    (&NATIVE_STACK_LIMIT as *const std::sync::atomic::AtomicU64) as u64
}

/// Publish the limit. `reserve_bytes` is how much of the stack below the current
/// frame compiled code may use; the remainder is headroom for the interpreter to
/// unwind and signal in.
///
/// Called once, early, from a frame near the base of the thread's stack. A
/// too-low estimate only costs performance (self-calls take the c2i path
/// sooner); the limit is never allowed to exceed the current stack pointer, so a
/// bad reservation cannot push it above live frames.
pub fn set_native_stack_limit(reserve_bytes: u64) {
    let here = &reserve_bytes as *const u64 as u64;
    let limit = here.saturating_sub(reserve_bytes);
    NATIVE_STACK_LIMIT.store(limit, std::sync::atomic::Ordering::Release);
}
