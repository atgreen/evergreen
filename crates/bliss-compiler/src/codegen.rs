//! Code emission and register allocation.
//!
//! Targets x86-64 (primary) and AArch64 (secondary).
//! See spec §4.7.

use crate::ir::{IrGraph, NodeKind};
use bliss_rt::error::BlissError;

/// Target architecture for code emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    Aarch64,
}

/// Return the target architecture of the current platform.
pub fn native_arch() -> TargetArch {
    #[cfg(target_arch = "x86_64")]
    { TargetArch::X86_64 }
    #[cfg(target_arch = "aarch64")]
    { TargetArch::Aarch64 }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    { TargetArch::X86_64 } // default fallback
}

// ── Code buffer ────────────────────────────────────────────────────

/// Buffer for emitted machine code, relocations, and metadata.
pub struct CodeBuffer {
    /// The emitted machine code bytes.
    bytes: Vec<u8>,
    /// Target architecture this code was emitted for.
    arch: TargetArch,
    /// Relocation entries.
    relocations: Vec<Relocation>,
    /// GC stack maps for safepoints.
    stack_maps: Vec<StackMap>,
}

impl CodeBuffer {
    /// Create a new code buffer for the given architecture.
    pub fn new(arch: TargetArch) -> Self {
        CodeBuffer {
            bytes: Vec::new(),
            arch,
            relocations: Vec::new(),
            stack_maps: Vec::new(),
        }
    }

    /// Get the emitted bytes.
    pub fn code(&self) -> &[u8] {
        &self.bytes
    }

    /// Get the total code size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Emit raw bytes into the buffer.
    fn emit_bytes(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
    }

    /// Add a relocation entry.
    fn add_relocation(&mut self, reloc: Relocation) {
        self.relocations.push(reloc);
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

// ── Code emission helpers ──────────────────────────────────────────

/// Emit a minimal function body for the given architecture into the buffer.
/// Walks the IR graph and emits code for each node.
fn emit_graph(buf: &mut CodeBuffer, graph: &IrGraph) {
    match buf.arch {
        TargetArch::X86_64 => emit_x86_64(buf, graph),
        TargetArch::Aarch64 => emit_aarch64(buf, graph),
    }
}

/// Emit x86-64 machine code for the given IR graph.
fn emit_x86_64(buf: &mut CodeBuffer, graph: &IrGraph) {
    let start = graph.start();

    // Walk from Start, find Return node(s) via uses
    for edge in graph.uses(start) {
        let target_kind = graph.node_kind(edge.to);
        match target_kind {
            NodeKind::Return => {
                // Check if the return node has a data input (value to return)
                let ret_inputs = graph.inputs(edge.to);
                let has_data = ret_inputs.iter().any(|e| {
                    matches!(e.kind, crate::ir::EdgeKind::Data)
                });
                if has_data {
                    // Find the data input node and emit a load of its value
                    for inp in ret_inputs {
                        if matches!(inp.kind, crate::ir::EdgeKind::Data) {
                            if let NodeKind::Constant(val) = graph.node_kind(inp.from) {
                                // movabs rax, <val>
                                buf.emit_bytes(&[0x48, 0xB8]);
                                buf.emit_bytes(&val.to_raw().to_le_bytes());
                            }
                        }
                    }
                }
                // ret
                buf.emit_bytes(&[0xC3]);
            }
            _ => {}
        }
    }

    // If nothing was emitted (simple Start->Return with no data), emit a bare ret
    if buf.is_empty() {
        // xor eax, eax (31 C0) — return 0/NIL
        buf.emit_bytes(&[0x31, 0xC0]);
        // ret
        buf.emit_bytes(&[0xC3]);
    }
}

/// Emit AArch64 machine code for the given IR graph.
fn emit_aarch64(buf: &mut CodeBuffer, graph: &IrGraph) {
    let start = graph.start();

    for edge in graph.uses(start) {
        let target_kind = graph.node_kind(edge.to);
        match target_kind {
            NodeKind::Return => {
                let ret_inputs = graph.inputs(edge.to);
                let has_data = ret_inputs.iter().any(|e| {
                    matches!(e.kind, crate::ir::EdgeKind::Data)
                });
                if has_data {
                    for inp in ret_inputs {
                        if matches!(inp.kind, crate::ir::EdgeKind::Data) {
                            if let NodeKind::Constant(val) = graph.node_kind(inp.from) {
                                let raw = val.to_raw();
                                // movz x0, #(raw & 0xFFFF)
                                let movz = 0xD2800000u32 | (((raw & 0xFFFF) as u32) << 5);
                                buf.emit_bytes(&movz.to_le_bytes());
                                // movk x0, #((raw >> 16) & 0xFFFF), lsl #16
                                if raw > 0xFFFF {
                                    let movk = 0xF2A00000u32
                                        | ((((raw >> 16) & 0xFFFF) as u32) << 5);
                                    buf.emit_bytes(&movk.to_le_bytes());
                                }
                            }
                        }
                    }
                }
                // ret (0xD65F03C0)
                buf.emit_bytes(&0xD65F03C0u32.to_le_bytes());
            }
            _ => {}
        }
    }

    if buf.is_empty() {
        // mov x0, #0
        buf.emit_bytes(&0xD2800000u32.to_le_bytes());
        // ret
        buf.emit_bytes(&0xD65F03C0u32.to_le_bytes());
    }
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
        X86_64Backend { _private: () }
    }
}

impl CodegenBackend for X86_64Backend {
    fn emit(&mut self, graph: &IrGraph) -> Result<CodeBuffer, crate::error::CompilerError> {
        if graph.node_count() == 0 {
            return Err(crate::error::CompilerError::CodegenError {
                message: "cannot emit code for empty graph with no nodes".into(),
            });
        }
        let mut buf = CodeBuffer::new(TargetArch::X86_64);
        emit_graph(&mut buf, graph);
        Ok(buf)
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
        Aarch64Backend { _private: () }
    }
}

impl CodegenBackend for Aarch64Backend {
    fn emit(&mut self, graph: &IrGraph) -> Result<CodeBuffer, crate::error::CompilerError> {
        if graph.node_count() == 0 {
            return Err(crate::error::CompilerError::CodegenError {
                message: "cannot emit code for empty graph with no nodes".into(),
            });
        }
        let mut buf = CodeBuffer::new(TargetArch::Aarch64);
        emit_graph(&mut buf, graph);
        Ok(buf)
    }

    fn target_arch(&self) -> TargetArch {
        TargetArch::Aarch64
    }
}

// ── Register allocator ─────────────────────────────────────────────

/// Linear-scan register allocator over SSA live ranges.
pub struct LinearScanAllocator {
    /// Target architecture determines available registers.
    arch: TargetArch,
}

impl LinearScanAllocator {
    /// Create a new register allocator for the given architecture.
    pub fn new(arch: TargetArch) -> Self {
        LinearScanAllocator { arch }
    }

    /// Allocate registers for the given IR graph, producing spill decisions.
    pub fn allocate(
        &mut self,
        graph: &IrGraph,
    ) -> Result<RegisterAllocation, crate::error::CompilerError> {
        if graph.node_count() == 0 {
            return Err(crate::error::CompilerError::RegisterAllocationError {
                message: "cannot allocate registers for empty graph with no nodes".into(),
            });
        }
        // For each node, assign a register or spill slot.
        // In a real implementation this would compute live ranges, build
        // an interval list, and do linear scan allocation.
        let num_gprs = match self.arch {
            TargetArch::X86_64 => 16u32,  // RAX..R15
            TargetArch::Aarch64 => 31u32, // X0..X30
        };
        Ok(RegisterAllocation {
            arch: self.arch,
            num_gprs,
            spill_slots: 0,
        })
    }
}

/// Result of register allocation.
pub struct RegisterAllocation {
    /// Architecture this allocation targets.
    arch: TargetArch,
    /// Number of general-purpose registers available.
    num_gprs: u32,
    /// Number of stack spill slots needed.
    spill_slots: u32,
}

// ── Code patching ──────────────────────────────────────────────────

/// Patch a call site, IC stub, or safepoint in installed code.
///
/// # Safety
/// `site` must be a valid patchable site address.
pub unsafe fn patch_code(site: *mut u8, new_target: *const u8) -> Result<(), BlissError> {
    if site.is_null() {
        return Err(BlissError::Internal(
            "patch_code: null site pointer".into(),
        ));
    }
    // Write the new target address at the patch site.
    // On x86-64 this would be a 4-byte relative offset or 8-byte absolute.
    // We write an 8-byte absolute address for simplicity.
    let target_bytes = (new_target as u64).to_le_bytes();
    for (i, &byte) in target_bytes.iter().enumerate() {
        site.add(i).write(byte);
    }
    Ok(())
}
