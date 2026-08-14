//! One ObjectHeader-compatible layout contract for every GC-managed object
//! (bliss-jtc.19). Asserts header field offsets, the size/type_id encoding, and
//! forwarding/pinning behavior across conses, strings, vectors, CLOS instances,
//! functions, and numeric heap objects — the object kinds the allocator, heap
//! walker, tracer, and evacuator must all agree on (spec §1.3).

use bliss_rt::object::{
    gc_bit, type_id, BignumHeader, ClosureData, ComplexData, CompiledFunctionData, ConsCell,
    DoubleFloatData, InterpretedFunctionData, ObjectHeader, RatioData, SymbolData,
};
use std::mem::{offset_of, size_of};

/// The base header is exactly 8 bytes and its bit-fields round-trip (§1.3).
#[test]
fn header_is_eight_bytes_and_fields_roundtrip() {
    assert_eq!(size_of::<ObjectHeader>(), 8, "ObjectHeader must be 8 bytes");
    // size is the total footprint in 8-byte units; try a representative value.
    let h = ObjectHeader::new(type_id::SIMPLE_VECTOR, 5);
    assert_eq!(h.type_id(), type_id::SIMPLE_VECTOR);
    assert_eq!(h.size_units(), 5);
    assert_eq!(h.gc_bits(), 0, "fresh header has no gc bits set");
    assert!(!h.is_forwarded() && !h.is_pinned() && !h.is_marked());
    assert!(!h.is_large_object());
}

/// The large-object sentinel is recognised.
#[test]
fn large_object_sentinel() {
    let h = ObjectHeader::new(type_id::SIMPLE_ARRAY, 0xFFFF);
    assert!(h.is_large_object());
    assert_eq!(h.type_id(), type_id::SIMPLE_ARRAY);
}

/// Forwarding sets the FORWARDED gc-bit while leaving type_id and size intact,
/// so the heap walker can still stride over an evacuated object (R1.09). This
/// property must hold for every heap type the collector may move.
#[test]
fn forwarding_preserves_type_id_and_size() {
    for &tid in &[
        type_id::CONS,
        type_id::SIMPLE_BASE_STRING,
        type_id::SIMPLE_CHARACTER_STRING,
        type_id::SIMPLE_VECTOR,
        type_id::STANDARD_OBJECT,      // CLOS instance
        type_id::FUNCTION_INTERPRETED, // function
        type_id::COMPILED_FUNCTION,
        type_id::CLOSURE,
        type_id::BIGNUM,
        type_id::RATIO,
        type_id::COMPLEX,
        type_id::DOUBLE_FLOAT,
    ] {
        let mut h = ObjectHeader::new(tid, 4);
        h.set_forwarded();
        assert!(h.is_forwarded(), "type {tid:#x}: forwarded bit set");
        assert_eq!(h.type_id(), tid, "type {tid:#x}: type_id preserved");
        assert_eq!(h.size_units(), 4, "type {tid:#x}: size preserved");
        // The forwarded bit occupies gc_bit::FORWARDED and nothing else.
        assert_eq!(h.gc_bits(), 1 << gc_bit::FORWARDED);
    }
}

/// Pinning sets the PINNED gc-bit independently of forwarding.
#[test]
fn pinning_is_independent() {
    let mut h = ObjectHeader::new(type_id::HASH_TABLE, 2);
    h.set_pinned();
    assert!(h.is_pinned());
    assert!(!h.is_forwarded());
    assert_eq!(h.gc_bits(), 1 << gc_bit::PINNED);
}

/// Every header-bearing layout struct places the `ObjectHeader` at offset 0, so
/// header access is uniform regardless of object kind (§1.5–§1.16).
#[test]
fn header_is_at_offset_zero_for_all_layouts() {
    assert_eq!(offset_of!(SymbolData, header), 0);
    assert_eq!(offset_of!(BignumHeader, header), 0);
    assert_eq!(offset_of!(RatioData, header), 0);
    assert_eq!(offset_of!(ComplexData, header), 0);
    assert_eq!(offset_of!(DoubleFloatData, header), 0);
    assert_eq!(offset_of!(InterpretedFunctionData, header), 0);
    assert_eq!(offset_of!(CompiledFunctionData, header), 0);
    assert_eq!(offset_of!(ClosureData, header), 0);
}

/// Cons cells are headerless and exactly 16 bytes (R1.13); their type_id is
/// reserved only for type checks on forwarded cons pages, never stored inline.
#[test]
fn cons_is_headerless_sixteen_bytes() {
    assert_eq!(size_of::<ConsCell>(), 16, "cons must be exactly 16 bytes");
    // The reserved discriminator still exists for forwarded-cons type checks.
    assert_eq!(type_id::CONS, 0x01);
}
