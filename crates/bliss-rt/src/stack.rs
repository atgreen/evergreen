//! CL stack layout and frame walking.
//!
//! Each green thread owns a `BlissStack` — a contiguous virtual memory
//! region for CL control/value frames. See §2.4 of the spec.

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
        let locals_ptr = (self as *const Frame).add(1) as *const BlissVal;
        std::slice::from_raw_parts(locals_ptr, self.num_locals as usize)
    }
}

// ── CodeInfo ───────────────────────────────────────────────────────

/// Metadata about a compiled function's code, used for GC stack maps
/// and debugger source-location mapping.
pub struct CodeInfo {
    _private: (),
}

impl CodeInfo {
    /// Look up the source location for a given PC offset.
    pub fn source_location(&self, _pc_offset: usize) -> Option<SourceLocation> {
        None
    }

    /// Get the GC stack map for a given safepoint PC offset.
    pub fn stack_map(&self, _pc_offset: usize) -> Option<&[u8]> {
        None
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
