//! Code emission and register allocation.
//!
//! Targets x86-64 (primary) and AArch64 (secondary).
//! See spec §4.7.

use crate::ir::IrGraph;
use bliss_rt::error::BlissError;

/// Target architecture for code emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    Aarch64,
}

/// Return the target architecture of the current platform.
pub fn native_arch() -> TargetArch {
    unimplemented!("native_arch")
}

// ── Code buffer ────────────────────────────────────────────────────

/// Buffer for emitted machine code, relocations, and metadata.
pub struct CodeBuffer {
    _private: (),
}

impl CodeBuffer {
    /// Create a new code buffer for the given architecture.
    pub fn new(arch: TargetArch) -> Self {
        unimplemented!("CodeBuffer::new")
    }

    /// Get the emitted bytes.
    pub fn code(&self) -> &[u8] {
        unimplemented!("CodeBuffer::code")
    }

    /// Get the total code size in bytes.
    pub fn len(&self) -> usize {
        unimplemented!("CodeBuffer::len")
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        unimplemented!("CodeBuffer::is_empty")
    }
}

// ── Relocations ────────────────────────────────────────────────────

/// A relocation entry in emitted code.
#[derive(Clone, Debug)]
pub struct Relocation {
    /// Offset within the code buffer.
    pub offset: u32,
    /// Kind of relocation.
    pub kind: RelocKind,
    /// Target value/address.
    pub target: u64,
}

/// Relocation kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocKind {
    /// Absolute 64-bit address.
    Abs64,
    /// PC-relative 32-bit offset.
    PcRel32,
    /// GC root reference.
    GcRoot,
    /// Inline cache site.
    InlineCache,
    /// Safepoint poll site.
    Safepoint,
}

// ── GC stack maps ──────────────────────────────────────────────────

/// A GC stack map for a safepoint location.
#[derive(Clone, Debug)]
pub struct StackMap {
    /// PC offset within the function.
    pub pc_offset: u32,
    /// Bitmap of stack slots containing GC references.
    pub ref_bitmap: Vec<u8>,
    /// Register set containing GC references.
    pub ref_registers: u32,
}

// ── Codegen trait ──────────────────────────────────────────────────

/// Code generation backend trait.
pub trait CodegenBackend {
    /// Lower IR graph to machine code.
    fn emit(&mut self, graph: &IrGraph) -> Result<CodeBuffer, crate::error::CompilerError>;

    /// Get the target architecture.
    fn target_arch(&self) -> TargetArch;
}

/// x86-64 code generation backend.
pub struct X86_64Backend {
    _private: (),
}

impl X86_64Backend {
    pub fn new() -> Self {
        unimplemented!("X86_64Backend::new")
    }
}

impl CodegenBackend for X86_64Backend {
    fn emit(&mut self, _graph: &IrGraph) -> Result<CodeBuffer, crate::error::CompilerError> {
        unimplemented!("X86_64Backend::emit")
    }

    fn target_arch(&self) -> TargetArch {
        TargetArch::X86_64
    }
}

/// AArch64 code generation backend.
pub struct Aarch64Backend {
    _private: (),
}

impl Aarch64Backend {
    pub fn new() -> Self {
        unimplemented!("Aarch64Backend::new")
    }
}

impl CodegenBackend for Aarch64Backend {
    fn emit(&mut self, _graph: &IrGraph) -> Result<CodeBuffer, crate::error::CompilerError> {
        unimplemented!("Aarch64Backend::emit")
    }

    fn target_arch(&self) -> TargetArch {
        TargetArch::Aarch64
    }
}

// ── Register allocator ─────────────────────────────────────────────

/// Linear-scan register allocator over SSA live ranges.
pub struct LinearScanAllocator {
    _private: (),
}

impl LinearScanAllocator {
    /// Create a new register allocator for the given architecture.
    pub fn new(arch: TargetArch) -> Self {
        unimplemented!("LinearScanAllocator::new")
    }

    /// Allocate registers for the given IR graph, producing spill decisions.
    pub fn allocate(&mut self, graph: &IrGraph) -> Result<RegisterAllocation, crate::error::CompilerError> {
        unimplemented!("LinearScanAllocator::allocate")
    }
}

/// Result of register allocation.
pub struct RegisterAllocation {
    _private: (),
}

// ── Code patching ──────────────────────────────────────────────────

/// Patch a call site, IC stub, or safepoint in installed code.
///
/// # Safety
/// `site` must be a valid patchable site address.
pub unsafe fn patch_code(site: *mut u8, new_target: *const u8) -> Result<(), BlissError> {
    unimplemented!("patch_code")
}
