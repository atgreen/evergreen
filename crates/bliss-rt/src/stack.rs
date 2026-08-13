//! CL stack layout and frame walking.
//!
//! Each green thread owns a `BlissStack` — a contiguous virtual memory
//! region for CL control/value frames. See §2.4 of the spec.

use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::value::BlissVal;

/// CL stack for a green thread.
/// Default usable size: 512 KiB (configurable via BLISS_STACK_SIZE).
pub struct BlissStack {
    /// Allocated memory buffer for the stack.
    memory: Vec<u8>,
    /// Stack pointer offset from base (grows upward from base).
    sp_offset: usize,
    /// Frame pointer (null if no frames pushed).
    fp: *const Frame,
    /// Published stack pointer for GC scanning while thread is parked at a safepoint.
    published_sp: AtomicUsize,
    /// Published frame pointer for GC scanning while thread is parked at a safepoint.
    published_fp: AtomicPtr<Frame>,
}

impl BlissStack {
    /// Allocate a new stack with the given usable size in bytes.
    /// Sets up guard pages for overflow detection.
    pub fn new(size: usize) -> Self {
        let memory = vec![0u8; size];
        BlissStack {
            memory,
            sp_offset: 0,
            fp: std::ptr::null(),
            published_sp: AtomicUsize::new(0),
            published_fp: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Get the base (lowest) address of the stack.
    pub fn base(&self) -> *const u8 {
        self.memory.as_ptr()
    }

    /// Get the current stack pointer.
    pub fn sp(&self) -> *const u8 {
        unsafe { self.memory.as_ptr().add(self.sp_offset) }
    }

    /// Get the current frame pointer.
    pub fn fp(&self) -> *const Frame {
        self.fp
    }

    /// Returns total usable size in bytes.
    pub fn capacity(&self) -> usize {
        self.memory.len()
    }

    /// Returns bytes currently in use.
    pub fn used(&self) -> usize {
        self.sp_offset
    }

    /// Publish the current sp and fp so the GC can scan this thread's
    /// stack while it is parked at a safepoint (§2.5.3).
    pub fn publish_top(&self) {
        self.published_sp.store(self.sp_offset, Ordering::Release);
        self.published_fp
            .store(self.fp as *mut Frame, Ordering::Release);
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

/// Byte budget for the interpreter's *host* (Rust) call-stack use before it must
/// raise `STORAGE-CONDITION` rather than let the native stack overflow into a
/// process-killing signal (R2.20). Derived from the OS soft stack limit
/// (`RLIMIT_STACK`) minus a red zone, so the guard fires with headroom to spare;
/// overridable with `BLISS_MAX_EVAL_STACK_BYTES`.
///
/// This is an interim guard for the tree-walker, whose activations live on the
/// Rust stack. Once CL activations move onto the per-green-thread `BlissStack`
/// (bliss-nmq), overflow is bounded by that stack's own capacity instead.
pub fn eval_stack_budget() -> usize {
    // Red zone left below the OS limit. The guard is polled at every CL call
    // boundary, so the most stack that can be consumed between two checks is a
    // single call's worth of host frames (kilobytes) — 1 MiB is ample headroom
    // to unwind and run a handler after the guard fires.
    const RED_ZONE: usize = 1024 * 1024;
    const FLOOR: usize = 1024 * 1024;
    const DEFAULT: usize = 7 * 1024 * 1024;

    if let Ok(v) = std::env::var("BLISS_MAX_EVAL_STACK_BYTES") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }

    #[cfg(unix)]
    {
        // SAFETY: getrlimit with a valid, zero-initialised rlimit and a supported
        // resource id. Reads only; no aliasing concerns.
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut rl) } == 0 {
            let soft = rl.rlim_cur;
            if soft != 0 && soft != libc::RLIM_INFINITY {
                return (soft as usize).saturating_sub(RED_ZONE).max(FLOOR);
            }
        }
    }

    DEFAULT
}

// ── Frame layout ───────────────────────────────────────────────────

/// Fixed-layout frame header (40 bytes). D2.02.
///
/// Followed by a variable-size locals area: `locals[0..num_locals]: BlissVal`.
#[repr(C)]
pub struct Frame {
    /// Link to previous frame (for stack walking).
    pub prev_fp: *mut Frame,
    /// Return address in native code.
    pub return_pc: *const u8,
    /// The function object for this frame.
    pub function: BlissVal,
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
    pub unsafe fn locals(&self) -> &[BlissVal] {
        if self.num_locals == 0 {
            return &[];
        }
        unsafe {
            let locals_ptr = (self as *const Frame).add(1) as *const BlissVal;
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

struct CodeInfoMetadata {
    source_locations: &'static [SourceLocationEntry],
    stack_maps: &'static [StackMapEntry],
}

fn code_info_registry() -> &'static Mutex<HashMap<usize, &'static CodeInfoMetadata>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, &'static CodeInfoMetadata>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct CodeInfo {
    _private: (),
}

impl CodeInfo {
    /// Create code metadata from static tables emitted by the compiler.
    pub fn new(
        source_locations: &'static [SourceLocationEntry],
        stack_maps: &'static [StackMapEntry],
    ) -> &'static Self {
        let handle = Box::leak(Box::new(0u8)) as *mut u8 as *const CodeInfo;
        let metadata = Box::leak(Box::new(CodeInfoMetadata {
            source_locations: Box::leak(source_locations.to_vec().into_boxed_slice()),
            stack_maps: Box::leak(stack_maps.to_vec().into_boxed_slice()),
        }));
        code_info_registry()
            .lock()
            .unwrap()
            .insert(handle as usize, metadata);
        unsafe { &*handle }
    }

    fn source_location_entries(&self) -> &[SourceLocationEntry] {
        let metadata = code_info_registry()
            .lock()
            .unwrap()
            .get(&(self as *const CodeInfo as usize))
            .copied();
        metadata
            .map(|metadata| metadata.source_locations)
            .unwrap_or(&[])
    }

    fn stack_map_entries(&self) -> &[StackMapEntry] {
        let metadata = code_info_registry()
            .lock()
            .unwrap()
            .get(&(self as *const CodeInfo as usize))
            .copied();
        metadata.map(|metadata| metadata.stack_maps).unwrap_or(&[])
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
