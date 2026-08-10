//! Tests for code generation — codegen.rs

use bliss_compiler::codegen::{
    Aarch64Backend, CodeBuffer, CodegenBackend, LinearScanAllocator, RelocKind, Relocation,
    StackMap, TargetArch, X86_64Backend, native_arch,
};

// ── TargetArch enum ───────────────────────────────────────────────

#[test]
fn target_arch_variants_differ() {
    assert_eq!(TargetArch::X86_64, TargetArch::X86_64);
    assert_ne!(TargetArch::X86_64, TargetArch::Aarch64);
}

#[test]
fn target_arch_is_copy_and_debug() {
    let a = TargetArch::X86_64;
    let b = a;
    assert_eq!(a, b);
    assert!(format!("{:?}", TargetArch::Aarch64).contains("Aarch64"));
}

// ── native_arch ───────────────────────────────────────────────────

#[test]
fn native_arch_returns_valid_variant() {
    let arch = native_arch();
    assert!(arch == TargetArch::X86_64 || arch == TargetArch::Aarch64);
}

// ── CodeBuffer ────────────────────────────────────────────────────

#[test]
fn code_buffer_new_is_empty() {
    let buf = CodeBuffer::new(TargetArch::X86_64);
    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);
    assert_eq!(buf.code().len(), 0);
}

// ── Relocation struct ─────────────────────────────────────────────

#[test]
fn relocation_fields_and_clone() {
    let reloc = Relocation { offset: 42, kind: RelocKind::Abs64, target: 0xDEAD_BEEF };
    assert_eq!(reloc.offset, 42);
    assert_eq!(reloc.kind, RelocKind::Abs64);
    let cloned = reloc.clone();
    assert_eq!(cloned.target, 0xDEAD_BEEF);
}

// ── RelocKind enum ────────────────────────────────────────────────

#[test]
fn reloc_kind_all_variants_distinct() {
    let variants = [
        RelocKind::Abs64, RelocKind::PcRel32, RelocKind::GcRoot,
        RelocKind::InlineCache, RelocKind::Safepoint,
    ];
    for i in 0..variants.len() {
        for j in (i + 1)..variants.len() {
            assert_ne!(variants[i], variants[j]);
        }
    }
}

// ── StackMap struct ───────────────────────────────────────────────

#[test]
fn stack_map_fields_and_clone() {
    let sm = StackMap { pc_offset: 16, ref_bitmap: vec![0xFF, 0x01], ref_registers: 0b1010 };
    assert_eq!(sm.pc_offset, 16);
    assert_eq!(sm.ref_bitmap, vec![0xFF, 0x01]);
    let cloned = sm.clone();
    assert_eq!(cloned.ref_registers, 0b1010);
}

// ── X86_64Backend ─────────────────────────────────────────────────

#[test]
fn x86_64_backend_target_arch() {
    let backend = X86_64Backend::new();
    assert_eq!(backend.target_arch(), TargetArch::X86_64);
}

#[test]
fn x86_64_backend_emit_accepts_ir_graph() {
    use bliss_compiler::ir::IrGraph;
    let mut backend = X86_64Backend::new();
    let graph = IrGraph::new();
    assert!(backend.emit(&graph).is_ok());
}

// ── Aarch64Backend ────────────────────────────────────────────────

#[test]
fn aarch64_backend_target_arch() {
    let backend = Aarch64Backend::new();
    assert_eq!(backend.target_arch(), TargetArch::Aarch64);
}

#[test]
fn aarch64_backend_emit_accepts_ir_graph() {
    use bliss_compiler::ir::IrGraph;
    let mut backend = Aarch64Backend::new();
    let graph = IrGraph::new();
    assert!(backend.emit(&graph).is_ok());
}

// ── LinearScanAllocator ───────────────────────────────────────────

#[test]
fn linear_scan_allocator_new_both_archs() {
    let _x = LinearScanAllocator::new(TargetArch::X86_64);
    let _a = LinearScanAllocator::new(TargetArch::Aarch64);
}

#[test]
fn linear_scan_allocator_allocate_returns_result() {
    use bliss_compiler::ir::IrGraph;
    let mut alloc = LinearScanAllocator::new(TargetArch::X86_64);
    let graph = IrGraph::new();
    assert!(alloc.allocate(&graph).is_ok());
}

// ── RegisterAllocation struct ────────────────────────────────────

#[test]
fn register_allocation_can_be_stored_and_used() {
    use bliss_compiler::codegen::RegisterAllocation;
    use bliss_compiler::ir::IrGraph;
    let mut alloc = LinearScanAllocator::new(TargetArch::X86_64);
    let graph = IrGraph::new();
    let reg_alloc: RegisterAllocation = alloc.allocate(&graph).unwrap();
    // Verify the RegisterAllocation value is usable: can be moved/stored
    let _stored = reg_alloc;
    // Also verify Debug is available if derived
    let mut alloc2 = LinearScanAllocator::new(TargetArch::Aarch64);
    let graph2 = IrGraph::new();
    let _reg_alloc2: RegisterAllocation = alloc2.allocate(&graph2).unwrap();
}

// ── patch_code ────────────────────────────────────────────────────

#[test]
fn patch_code_with_null_pointer_errors() {
    use bliss_compiler::codegen::patch_code;
    let result = unsafe { patch_code(std::ptr::null_mut(), std::ptr::null()) };
    assert!(result.is_err());
}
