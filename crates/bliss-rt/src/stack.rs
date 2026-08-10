//! CL stack layout and frame walking.
//!
//! Each green thread owns a `BlissStack` — a contiguous virtual memory
//! region for CL control/value frames. See §2.4 of the spec.

use crate::value::BlissVal;

/// CL stack for a green thread.
/// Default usable size: 512 KiB (configurable via BLISS_STACK_SIZE).
pub struct BlissStack {
    _private: (),
}

impl BlissStack {
    /// Allocate a new stack with the given usable size in bytes.
    /// Sets up guard pages for overflow detection.
    pub fn new(size: usize) -> Self {
        unimplemented!("BlissStack::new")
    }

    /// Get the base (lowest) address of the stack.
    pub fn base(&self) -> *const u8 {
        unimplemented!("BlissStack::base")
    }

    /// Get the current stack pointer.
    pub fn sp(&self) -> *const u8 {
        unimplemented!("BlissStack::sp")
    }

    /// Get the current frame pointer.
    pub fn fp(&self) -> *const Frame {
        unimplemented!("BlissStack::fp")
    }

    /// Returns total usable size in bytes.
    pub fn capacity(&self) -> usize {
        unimplemented!("BlissStack::capacity")
    }

    /// Returns bytes currently in use.
    pub fn used(&self) -> usize {
        unimplemented!("BlissStack::used")
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
    /// Extract the frame type from flags.
    pub fn frame_type(&self) -> FrameType {
        unimplemented!("Frame::frame_type")
    }

    /// Get a slice of local variables in this frame.
    ///
    /// # Safety
    /// Frame must be valid and `num_locals` must be correct.
    pub unsafe fn locals(&self) -> &[BlissVal] {
        unimplemented!("Frame::locals")
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
    pub fn source_location(&self, pc_offset: usize) -> Option<SourceLocation> {
        unimplemented!("CodeInfo::source_location")
    }

    /// Get the GC stack map for a given safepoint PC offset.
    pub fn stack_map(&self, pc_offset: usize) -> Option<&[u8]> {
        unimplemented!("CodeInfo::stack_map")
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
    _private: (),
}

impl FrameWalker {
    /// Create a frame walker starting from the given frame pointer.
    ///
    /// # Safety
    /// `fp` must point to a valid `Frame`.
    pub unsafe fn new(fp: *const Frame) -> Self {
        unimplemented!("FrameWalker::new")
    }
}

impl Iterator for FrameWalker {
    type Item = *const Frame;

    fn next(&mut self) -> Option<Self::Item> {
        unimplemented!("FrameWalker::next")
    }
}
