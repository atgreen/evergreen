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
///
/// Walks the IR graph and emits x86-64 instructions for each node kind.
fn emit_x86_64(buf: &mut CodeBuffer, graph: &IrGraph) {
    use crate::ir::EdgeKind;
    use std::collections::{HashMap, HashSet};

    let start = graph.start();

    // BFS to visit all reachable nodes in topological order
    let mut visited = HashSet::new();
    let mut worklist = vec![start];
    let mut ordered = Vec::new();

    while let Some(node) = worklist.pop() {
        if !visited.insert(node) {
            continue;
        }
        ordered.push(node);
        for edge in graph.uses(node) {
            if !visited.contains(&edge.to) {
                worklist.push(edge.to);
            }
        }
    }

    // Track label offsets for branches
    let mut node_offsets: HashMap<crate::ir::NodeId, usize> = HashMap::new();

    // Function prologue: push rbp; mov rbp, rsp
    buf.emit_bytes(&[0x55]);             // push rbp
    buf.emit_bytes(&[0x48, 0x89, 0xE5]); // mov rbp, rsp

    for &node in &ordered {
        node_offsets.insert(node, buf.len());
        let kind = graph.node_kind(node).clone();
        match kind {
            NodeKind::Start => {
                // No code needed for start
            }
            NodeKind::Return => {
                // Load the return value from data input into rax
                let ret_inputs = graph.inputs(node);
                let has_data = ret_inputs.iter().any(|e| e.kind == EdgeKind::Data);
                if has_data {
                    for inp in ret_inputs {
                        if inp.kind == EdgeKind::Data {
                            emit_node_value_x86_64(buf, graph, inp.from);
                        }
                    }
                } else {
                    // No return value: xor eax, eax (return 0/NIL)
                    buf.emit_bytes(&[0x31, 0xC0]);
                }
                // Function epilogue: pop rbp; ret
                buf.emit_bytes(&[0x5D]); // pop rbp
                buf.emit_bytes(&[0xC3]); // ret
            }
            NodeKind::Constant(_) => {
                // Constants are emitted inline when used
            }
            NodeKind::Parameter(idx) => {
                // Parameters arrive in System V ABI registers: rdi, rsi, rdx, rcx, r8, r9
                // Move parameter to rax for now
                let param_regs: &[&[u8]] = &[
                    &[0x48, 0x89, 0xF8], // mov rax, rdi (param 0)
                    &[0x48, 0x89, 0xF0], // mov rax, rsi (param 1)
                    &[0x48, 0x89, 0xD0], // mov rax, rdx (param 2)
                    &[0x48, 0x89, 0xC8], // mov rax, rcx (param 3)
                    &[0x4C, 0x89, 0xC0], // mov rax, r8  (param 4)
                    &[0x4C, 0x89, 0xC8], // mov rax, r9  (param 5)
                ];
                if (idx as usize) < param_regs.len() {
                    buf.emit_bytes(param_regs[idx as usize]);
                }
            }
            NodeKind::Phi => {
                // Phi nodes are resolved during register allocation;
                // emit a nop placeholder (the register allocator inserts moves)
                buf.emit_bytes(&[0x90]); // nop
            }
            NodeKind::Region => {
                // Region is a control-flow merge point; serves as a label target.
                // No code emitted, but offset is recorded above.
            }
            NodeKind::Branch => {
                // Conditional branch: test rax, rax; jz <target>
                // The condition value is in rax from the preceding data input.
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                    }
                }
                buf.emit_bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
                // jz rel32 (placeholder offset, needs relocation)
                buf.emit_bytes(&[0x0F, 0x84]);
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&[0x00, 0x00, 0x00, 0x00]); // placeholder
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::PcRel32,
                    target: 0, // resolved during linking
                });
            }
            NodeKind::Call => {
                // Function call: the callee is in the first data input.
                // Load callee address into rax, then call *rax.
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                        break;
                    }
                }
                // call *rax (FF D0)
                buf.emit_bytes(&[0xFF, 0xD0]);
                // Add an inline-cache relocation at the call site
                buf.add_relocation(Relocation {
                    offset: (buf.len() - 2) as u32,
                    kind: RelocKind::InlineCache,
                    target: 0,
                });
            }
            NodeKind::TypeCheck { expected_type } => {
                // Runtime type guard: load value, check tag, trap if mismatch.
                // Load the value from data input
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                        break;
                    }
                }
                // Extract tag: mov rcx, rax; and rcx, 0x7
                buf.emit_bytes(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                buf.emit_bytes(&[0x48, 0x83, 0xE1, 0x07]); // and rcx, 7
                // Compare with expected tag
                let expected_tag = (expected_type.to_raw() & 0x7) as u8;
                buf.emit_bytes(&[0x48, 0x83, 0xF9, expected_tag]); // cmp rcx, tag
                // jne <deopt stub> (placeholder)
                buf.emit_bytes(&[0x0F, 0x85]);
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&[0x00, 0x00, 0x00, 0x00]);
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::PcRel32,
                    target: 0, // deopt stub address
                });
            }
            NodeKind::Box => {
                // Tag an unboxed value: shift left 3, OR with tag
                // The data input provides the raw value in rax
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                        break;
                    }
                }
                // shl rax, 3 (shift left by tag width)
                buf.emit_bytes(&[0x48, 0xC1, 0xE0, 0x03]);
                // The specific tag OR would depend on the type being boxed
            }
            NodeKind::Unbox => {
                // Remove tag from a tagged value: shift right 3
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                        break;
                    }
                }
                // shr rax, 3
                buf.emit_bytes(&[0x48, 0xC1, 0xE8, 0x03]);
            }
            NodeKind::MemLoad { offset } => {
                // Heap load: load base address from data input, then load [base + offset]
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_x86_64(buf, graph, inp.from);
                        break;
                    }
                }
                // Mask off tag bits: and rax, ~7
                buf.emit_bytes(&[0x48, 0x83, 0xE0, 0xF8]);
                // mov rax, [rax + offset]
                if offset >= -128 && offset <= 127 {
                    buf.emit_bytes(&[0x48, 0x8B, 0x40, offset as u8]);
                } else {
                    buf.emit_bytes(&[0x48, 0x8B, 0x80]);
                    buf.emit_bytes(&(offset as i32).to_le_bytes());
                }
                // Add GC root relocation
                buf.add_relocation(Relocation {
                    offset: (buf.len() - 4) as u32,
                    kind: RelocKind::GcRoot,
                    target: 0,
                });
            }
            NodeKind::MemStore { offset } => {
                // Heap store: first data input = base, second = value
                let inputs = graph.inputs(node);
                let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
                if data_inputs.len() >= 2 {
                    // Load base address into rcx
                    emit_node_value_x86_64(buf, graph, data_inputs[0].from);
                    buf.emit_bytes(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                    // Mask off tag bits
                    buf.emit_bytes(&[0x48, 0x83, 0xE1, 0xF8]); // and rcx, ~7
                    // Load value into rax
                    emit_node_value_x86_64(buf, graph, data_inputs[1].from);
                    // mov [rcx + offset], rax
                    if offset >= -128 && offset <= 127 {
                        buf.emit_bytes(&[0x48, 0x89, 0x41, offset as u8]);
                    } else {
                        buf.emit_bytes(&[0x48, 0x89, 0x81]);
                        buf.emit_bytes(&(offset as i32).to_le_bytes());
                    }
                }
            }
            NodeKind::Safepoint => {
                // GC safepoint poll: load from safepoint page, trap if page is protected.
                // test [safepoint_page], eax — placeholder with relocation
                buf.emit_bytes(&[0x85, 0x05]); // test [rip + disp32], eax
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&[0x00, 0x00, 0x00, 0x00]);
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::Safepoint,
                    target: 0,
                });
                // Record stack map at this safepoint
                buf.stack_maps.push(StackMap {
                    pc_offset: buf.len() as u32,
                    ref_bitmap: vec![0],
                    ref_registers: 0,
                });
            }
        }
    }

    // If nothing was emitted (shouldn't happen with a valid graph), emit a bare ret
    if buf.is_empty() {
        buf.emit_bytes(&[0x31, 0xC0]); // xor eax, eax
        buf.emit_bytes(&[0xC3]);       // ret
    }
}

/// Helper: emit the value of a node into rax (x86-64).
fn emit_node_value_x86_64(buf: &mut CodeBuffer, graph: &IrGraph, node: crate::ir::NodeId) {
    let kind = graph.node_kind(node);
    match kind {
        NodeKind::Constant(val) => {
            // movabs rax, <val>
            buf.emit_bytes(&[0x48, 0xB8]);
            buf.emit_bytes(&val.to_raw().to_le_bytes());
        }
        NodeKind::Parameter(idx) => {
            let param_regs: &[&[u8]] = &[
                &[0x48, 0x89, 0xF8], // mov rax, rdi
                &[0x48, 0x89, 0xF0], // mov rax, rsi
                &[0x48, 0x89, 0xD0], // mov rax, rdx
                &[0x48, 0x89, 0xC8], // mov rax, rcx
                &[0x4C, 0x89, 0xC0], // mov rax, r8
                &[0x4C, 0x89, 0xC8], // mov rax, r9
            ];
            if (*idx as usize) < param_regs.len() {
                buf.emit_bytes(param_regs[*idx as usize]);
            }
        }
        _ => {
            // For other nodes, the value is assumed to already be in rax
            // from previous emission.
        }
    }
}

/// Emit AArch64 machine code for the given IR graph.
///
/// Walks the IR graph and emits AArch64 instructions for each node kind.
fn emit_aarch64(buf: &mut CodeBuffer, graph: &IrGraph) {
    use crate::ir::EdgeKind;
    use std::collections::HashSet;

    let start = graph.start();

    // BFS traversal for topological ordering
    let mut visited = HashSet::new();
    let mut worklist = vec![start];
    let mut ordered = Vec::new();

    while let Some(node) = worklist.pop() {
        if !visited.insert(node) {
            continue;
        }
        ordered.push(node);
        for edge in graph.uses(node) {
            if !visited.contains(&edge.to) {
                worklist.push(edge.to);
            }
        }
    }

    // Function prologue: stp x29, x30, [sp, #-16]!; mov x29, sp
    buf.emit_bytes(&0xA9BF7BFDu32.to_le_bytes()); // stp x29, x30, [sp, #-16]!
    buf.emit_bytes(&0x910003FDu32.to_le_bytes()); // mov x29, sp

    for &node in &ordered {
        let kind = graph.node_kind(node).clone();
        match kind {
            NodeKind::Start => {}
            NodeKind::Return => {
                let ret_inputs = graph.inputs(node);
                let has_data = ret_inputs.iter().any(|e| e.kind == EdgeKind::Data);
                if has_data {
                    for inp in ret_inputs {
                        if inp.kind == EdgeKind::Data {
                            emit_node_value_aarch64(buf, graph, inp.from);
                        }
                    }
                } else {
                    // mov x0, #0
                    buf.emit_bytes(&0xD2800000u32.to_le_bytes());
                }
                // Epilogue: ldp x29, x30, [sp], #16; ret
                buf.emit_bytes(&0xA8C17BFDu32.to_le_bytes()); // ldp x29, x30, [sp], #16
                buf.emit_bytes(&0xD65F03C0u32.to_le_bytes()); // ret
            }
            NodeKind::Constant(_) => {
                // Emitted inline when used
            }
            NodeKind::Parameter(idx) => {
                // AArch64 passes params in x0-x7; move to x0 for use
                if idx > 0 && idx <= 7 {
                    // mov x0, x<idx>
                    let mov = 0xAA0003E0u32 | ((idx as u32) << 16);
                    buf.emit_bytes(&mov.to_le_bytes());
                }
            }
            NodeKind::Phi => {
                buf.emit_bytes(&0xD503201Fu32.to_le_bytes()); // nop
            }
            NodeKind::Region => {}
            NodeKind::Branch => {
                // cbz x0, <target> (placeholder)
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&0xB4000000u32.to_le_bytes()); // cbz x0, #0
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::PcRel32,
                    target: 0,
                });
            }
            NodeKind::Call => {
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                // blr x0
                buf.emit_bytes(&0xD63F0000u32.to_le_bytes());
                buf.add_relocation(Relocation {
                    offset: (buf.len() - 4) as u32,
                    kind: RelocKind::InlineCache,
                    target: 0,
                });
            }
            NodeKind::TypeCheck { expected_type } => {
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                // Extract tag: and x1, x0, #7
                buf.emit_bytes(&0x92400401u32.to_le_bytes()); // and x1, x0, #0x7
                // Compare with expected tag
                let expected_tag = (expected_type.to_raw() & 0x7) as u32;
                let cmp_imm = 0xF1000020u32 | (expected_tag << 10); // cmp x1, #tag
                buf.emit_bytes(&cmp_imm.to_le_bytes());
                // b.ne <deopt> (placeholder)
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&0x54000001u32.to_le_bytes()); // b.ne #0
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::PcRel32,
                    target: 0,
                });
            }
            NodeKind::Box => {
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                // lsl x0, x0, #3
                buf.emit_bytes(&0xD37CEC00u32.to_le_bytes());
            }
            NodeKind::Unbox => {
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                // lsr x0, x0, #3
                buf.emit_bytes(&0xD340FC00u32.to_le_bytes());
            }
            NodeKind::MemLoad { offset } => {
                let inputs = graph.inputs(node);
                for inp in inputs {
                    if inp.kind == EdgeKind::Data {
                        emit_node_value_aarch64(buf, graph, inp.from);
                        break;
                    }
                }
                // Mask off tag: and x0, x0, #~7
                buf.emit_bytes(&0x927CF800u32.to_le_bytes()); // and x0, x0, #0xFFFFFFFFFFFFFFF8
                // ldr x0, [x0, #offset]
                if offset >= 0 && offset < 32768 && (offset % 8 == 0) {
                    let ldr = 0xF9400000u32 | (((offset as u32 / 8) & 0xFFF) << 10);
                    buf.emit_bytes(&ldr.to_le_bytes());
                } else {
                    // Use add + ldr for larger offsets
                    buf.emit_bytes(&0xF9400000u32.to_le_bytes()); // ldr x0, [x0]
                }
                buf.add_relocation(Relocation {
                    offset: (buf.len() - 4) as u32,
                    kind: RelocKind::GcRoot,
                    target: 0,
                });
            }
            NodeKind::MemStore { offset } => {
                let inputs = graph.inputs(node);
                let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
                if data_inputs.len() >= 2 {
                    // Load base into x1
                    emit_node_value_aarch64(buf, graph, data_inputs[0].from);
                    // mov x1, x0
                    buf.emit_bytes(&0xAA0003E1u32.to_le_bytes());
                    // Mask off tag
                    buf.emit_bytes(&0x927CF821u32.to_le_bytes()); // and x1, x1, #~7
                    // Load value into x0
                    emit_node_value_aarch64(buf, graph, data_inputs[1].from);
                    // str x0, [x1, #offset]
                    if offset >= 0 && offset < 32768 && (offset % 8 == 0) {
                        let str_inst = 0xF9000020u32 | (((offset as u32 / 8) & 0xFFF) << 10);
                        buf.emit_bytes(&str_inst.to_le_bytes());
                    } else {
                        buf.emit_bytes(&0xF9000020u32.to_le_bytes()); // str x0, [x1]
                    }
                }
            }
            NodeKind::Safepoint => {
                // Safepoint poll: load from safepoint page
                let reloc_offset = buf.len() as u32;
                buf.emit_bytes(&0xF9400000u32.to_le_bytes()); // ldr x0, [x0] placeholder
                buf.add_relocation(Relocation {
                    offset: reloc_offset,
                    kind: RelocKind::Safepoint,
                    target: 0,
                });
                buf.stack_maps.push(StackMap {
                    pc_offset: buf.len() as u32,
                    ref_bitmap: vec![0],
                    ref_registers: 0,
                });
            }
        }
    }

    if buf.is_empty() {
        buf.emit_bytes(&0xD2800000u32.to_le_bytes()); // mov x0, #0
        buf.emit_bytes(&0xD65F03C0u32.to_le_bytes()); // ret
    }
}

/// Helper: emit the value of a node into x0 (AArch64).
fn emit_node_value_aarch64(buf: &mut CodeBuffer, graph: &IrGraph, node: crate::ir::NodeId) {
    let kind = graph.node_kind(node);
    match kind {
        NodeKind::Constant(val) => {
            let raw = val.to_raw();
            // movz x0, #(raw & 0xFFFF)
            let movz = 0xD2800000u32 | (((raw & 0xFFFF) as u32) << 5);
            buf.emit_bytes(&movz.to_le_bytes());
            // movk for higher 16-bit chunks
            if raw > 0xFFFF {
                let movk16 = 0xF2A00000u32 | ((((raw >> 16) & 0xFFFF) as u32) << 5);
                buf.emit_bytes(&movk16.to_le_bytes());
            }
            if raw > 0xFFFF_FFFF {
                let movk32 = 0xF2C00000u32 | ((((raw >> 32) & 0xFFFF) as u32) << 5);
                buf.emit_bytes(&movk32.to_le_bytes());
            }
            if raw > 0xFFFF_FFFF_FFFF {
                let movk48 = 0xF2E00000u32 | ((((raw >> 48) & 0xFFFF) as u32) << 5);
                buf.emit_bytes(&movk48.to_le_bytes());
            }
        }
        NodeKind::Parameter(idx) => {
            if *idx > 0 && *idx <= 7 {
                let mov = 0xAA0003E0u32 | ((*idx as u32) << 16);
                buf.emit_bytes(&mov.to_le_bytes());
            }
        }
        _ => {
            // Value assumed to already be in x0
        }
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
    ///
    /// Performs a simplified linear-scan allocation:
    /// 1. Assigns each value-producing node an ordinal (its "live range start").
    /// 2. Iterates nodes in order, assigning the lowest free register.
    /// 3. When all GPRs are occupied, spills the node to a stack slot.
    pub fn allocate(
        &mut self,
        graph: &IrGraph,
    ) -> Result<RegisterAllocation, crate::error::CompilerError> {
        use crate::ir::NodeKind;
        use std::collections::HashMap;

        if graph.node_count() == 0 {
            return Err(crate::error::CompilerError::RegisterAllocationError {
                message: "cannot allocate registers for empty graph with no nodes".into(),
            });
        }

        let num_gprs = match self.arch {
            TargetArch::X86_64 => 16u32,  // RAX..R15
            TargetArch::Aarch64 => 31u32, // X0..X30
        };

        // Collect value-producing nodes (nodes that produce a data output).
        // Non-value nodes (Start, Return, Region, Branch, MemStore, Safepoint)
        // don't need register assignments.
        let mut value_nodes = Vec::new();
        let start = graph.start();

        // BFS traversal to get a topological ordering of nodes
        let mut visited = std::collections::HashSet::new();
        let mut worklist = vec![start];
        let mut ordered = Vec::new();

        while let Some(node) = worklist.pop() {
            if !visited.insert(node) {
                continue;
            }
            ordered.push(node);
            for edge in graph.uses(node) {
                if !visited.contains(&edge.to) {
                    worklist.push(edge.to);
                }
            }
        }

        // Determine which nodes produce values and need register assignment
        for &node in &ordered {
            let kind = graph.node_kind(node);
            let produces_value = matches!(
                kind,
                NodeKind::Constant(_)
                    | NodeKind::Parameter(_)
                    | NodeKind::Phi
                    | NodeKind::Call
                    | NodeKind::Box
                    | NodeKind::Unbox
                    | NodeKind::MemLoad { .. }
                    | NodeKind::TypeCheck { .. }
            );
            if produces_value {
                value_nodes.push(node);
            }
        }

        // Compute live ranges: a node is live from its definition until
        // its last use. We approximate this with ordinal positions.
        let node_order: HashMap<crate::ir::NodeId, usize> = ordered
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, i))
            .collect();

        // For each value node, find the last use position
        let mut last_use: HashMap<crate::ir::NodeId, usize> = HashMap::new();
        for &node in &value_nodes {
            let def_pos = node_order.get(&node).copied().unwrap_or(0);
            let mut end = def_pos;
            for edge in graph.uses(node) {
                if let Some(&use_pos) = node_order.get(&edge.to) {
                    if use_pos > end {
                        end = use_pos;
                    }
                }
            }
            last_use.insert(node, end);
        }

        // Linear scan: assign registers greedily, spill when exhausted
        let mut assignments: HashMap<crate::ir::NodeId, Option<u32>> = HashMap::new();
        // Track which registers are free and which nodes hold them
        let mut reg_holders: Vec<Option<crate::ir::NodeId>> = vec![None; num_gprs as usize];
        let mut spill_count = 0u32;

        for &node in &value_nodes {
            let current_pos = node_order.get(&node).copied().unwrap_or(0);

            // Free registers whose holder's live range has ended
            for slot in reg_holders.iter_mut() {
                if let Some(holder) = *slot {
                    if let Some(&end) = last_use.get(&holder) {
                        if end < current_pos {
                            *slot = None;
                        }
                    }
                }
            }

            // Find a free register
            let mut assigned = false;
            for (reg_idx, slot) in reg_holders.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(node);
                    assignments.insert(node, Some(reg_idx as u32));
                    assigned = true;
                    break;
                }
            }

            if !assigned {
                // All registers occupied — spill this node
                assignments.insert(node, None);
                spill_count += 1;
            }
        }

        Ok(RegisterAllocation {
            arch: self.arch,
            num_gprs,
            spill_slots: spill_count,
            assignments,
        })
    }
}

/// Result of register allocation: maps each IR node to a physical register
/// or a spill slot.
pub struct RegisterAllocation {
    /// Architecture this allocation targets.
    arch: TargetArch,
    /// Number of general-purpose registers available on this architecture.
    num_gprs: u32,
    /// Number of stack spill slots needed.
    spill_slots: u32,
    /// Map from node id to assigned register (None = spilled).
    assignments: std::collections::HashMap<crate::ir::NodeId, Option<u32>>,
}

impl RegisterAllocation {
    /// Get the target architecture.
    pub fn target_arch(&self) -> TargetArch {
        self.arch
    }

    /// Get the number of general-purpose registers available.
    pub fn num_gprs(&self) -> u32 {
        self.num_gprs
    }

    /// Get the number of spill slots required.
    pub fn spill_slots(&self) -> u32 {
        self.spill_slots
    }

    /// Query which register a given node was assigned to.
    /// Returns `Some(reg_index)` for a register assignment,
    /// `None` if the node was spilled to the stack.
    pub fn register_for(&self, node: crate::ir::NodeId) -> Option<u32> {
        self.assignments.get(&node).copied().flatten()
    }

    /// Returns true if the given node was spilled.
    pub fn is_spilled(&self, node: crate::ir::NodeId) -> bool {
        matches!(self.assignments.get(&node), Some(None))
    }
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
        unsafe { site.add(i).write(byte); }
    }
    Ok(())
}
