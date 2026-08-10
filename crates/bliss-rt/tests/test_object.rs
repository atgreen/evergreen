//! Tests for object.rs — ObjectHeader, heap object layouts, GC bits, type IDs,
//! and struct field layouts.

use bliss_rt::object::*;
use bliss_rt::value::BlissVal;
use std::mem;

// ── ObjectHeader: new() and field extraction ──────────────────────

#[test]
fn header_new_stores_type_id_and_size() {
    let h = ObjectHeader::new(0x42, 100);
    assert_eq!(h.type_id(), 0x42);
    assert_eq!(h.size_units(), 100);
}

#[test]
fn header_new_initializes_gc_bits_zero() {
    let h = ObjectHeader::new(0x01, 1);
    assert_eq!(h.gc_bits(), 0);
}

#[test]
fn header_new_initializes_hash_zero() {
    let h = ObjectHeader::new(0x01, 1);
    assert_eq!(h.hash(), 0);
}

// ── type_id extraction ────────────────────────────────────────────

#[test]
fn header_type_id_extracts_bits_63_56() {
    let h = ObjectHeader::new(0xAB, 0);
    assert_eq!(h.type_id(), 0xAB);
}

#[test]
fn header_type_id_zero() {
    let h = ObjectHeader::new(0x00, 1);
    assert_eq!(h.type_id(), 0x00);
}

#[test]
fn header_type_id_max() {
    let h = ObjectHeader::new(0xFF, 1);
    assert_eq!(h.type_id(), 0xFF);
}

// ── gc_bits / set_gc_bits ─────────────────────────────────────────

#[test]
fn header_gc_bits_round_trip() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_gc_bits(0b1010_1010);
    assert_eq!(h.gc_bits(), 0b1010_1010);
}

#[test]
fn header_set_gc_bits_does_not_clobber_type_id() {
    let mut h = ObjectHeader::new(0xCD, 42);
    h.set_gc_bits(0xFF);
    assert_eq!(h.type_id(), 0xCD);
}

#[test]
fn header_set_gc_bits_does_not_clobber_size() {
    let mut h = ObjectHeader::new(0x01, 999);
    h.set_gc_bits(0xFF);
    assert_eq!(h.size_units(), 999);
}

#[test]
fn header_set_gc_bits_does_not_clobber_hash() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0xDEADBEEF);
    h.set_gc_bits(0x55);
    assert_eq!(h.hash(), 0xDEADBEEF);
}

// ── hash / set_hash ───────────────────────────────────────────────

#[test]
fn header_hash_round_trip() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0x12345678);
    assert_eq!(h.hash(), 0x12345678);
}

#[test]
fn header_hash_zero_value() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0);
    assert_eq!(h.hash(), 0);
}

#[test]
fn header_hash_max_value() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0xFFFFFFFF);
    assert_eq!(h.hash(), 0xFFFFFFFF);
}

#[test]
fn header_set_hash_does_not_clobber_type_id() {
    let mut h = ObjectHeader::new(0xEE, 1);
    h.set_hash(0xFFFFFFFF);
    assert_eq!(h.type_id(), 0xEE);
}

#[test]
fn header_set_hash_does_not_clobber_size() {
    let mut h = ObjectHeader::new(0x01, 12345);
    h.set_hash(0xFFFFFFFF);
    assert_eq!(h.size_units(), 12345);
}

#[test]
fn header_set_hash_does_not_clobber_gc_bits() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_gc_bits(0xAA);
    h.set_hash(0xBBBBBBBB);
    assert_eq!(h.gc_bits(), 0xAA);
}

// ── size_units extraction ─────────────────────────────────────────

#[test]
fn header_size_units_zero() {
    let h = ObjectHeader::new(0x01, 0);
    assert_eq!(h.size_units(), 0);
}

#[test]
fn header_size_units_max() {
    let h = ObjectHeader::new(0x01, 0xFFFF);
    assert_eq!(h.size_units(), 0xFFFF);
}

// ── is_large_object ───────────────────────────────────────────────

#[test]
fn header_is_large_object_true_at_sentinel() {
    let h = ObjectHeader::new(0x01, 0xFFFF);
    assert!(h.is_large_object());
}

#[test]
fn header_is_large_object_false_for_normal() {
    let h = ObjectHeader::new(0x01, 100);
    assert!(!h.is_large_object());
}

#[test]
fn header_is_large_object_false_at_fffe() {
    let h = ObjectHeader::new(0x01, 0xFFFE);
    assert!(!h.is_large_object());
}

// ── Bit-packing round-trip: set all fields, verify each ───────────

#[test]
fn header_full_round_trip() {
    let mut h = ObjectHeader::new(0xAB, 0x1234);
    h.set_gc_bits(0xCD);
    h.set_hash(0xDEADBEEF);

    assert_eq!(h.type_id(), 0xAB);
    assert_eq!(h.gc_bits(), 0xCD);
    assert_eq!(h.hash(), 0xDEADBEEF);
    assert_eq!(h.size_units(), 0x1234);
}

// ── GC bit flag accessors ─────────────────────────────────────────

#[test]
fn header_is_marked_initially_false() {
    let h = ObjectHeader::new(0x01, 1);
    assert!(!h.is_marked());
}

#[test]
fn header_set_marked() {
    let h = ObjectHeader::new(0x01, 1);
    let was_unmarked = h.set_marked();
    assert!(was_unmarked); // should succeed (previously unmarked)
    assert!(h.is_marked());
}

#[test]
fn header_is_forwarded_initially_false() {
    let h = ObjectHeader::new(0x01, 1);
    assert!(!h.is_forwarded());
}

#[test]
fn header_is_forwarded_when_bit_set() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_gc_bits(1 << gc_bit::FORWARDED);
    assert!(h.is_forwarded());
}

#[test]
fn header_is_pinned_initially_false() {
    let h = ObjectHeader::new(0x01, 1);
    assert!(!h.is_pinned());
}

#[test]
fn header_is_pinned_when_bit_set() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_gc_bits(1 << gc_bit::PINNED);
    assert!(h.is_pinned());
}

// ── GC bit combinations ──────────────────────────────────────────

#[test]
fn header_multiple_gc_flags_independent() {
    let mut h = ObjectHeader::new(0x01, 1);
    let flags = (1 << gc_bit::MARK)
        | (1 << gc_bit::GREY)
        | (1 << gc_bit::FORWARDED)
        | (1 << gc_bit::PINNED)
        | (1 << gc_bit::REMEMBERED);
    h.set_gc_bits(flags);

    assert!(h.is_marked());
    assert!(h.is_forwarded());
    assert!(h.is_pinned());
    // GREY and REMEMBERED are set too but no dedicated accessor; verify via gc_bits
    assert_ne!(h.gc_bits() & (1 << gc_bit::GREY), 0);
    assert_ne!(h.gc_bits() & (1 << gc_bit::REMEMBERED), 0);
}

#[test]
fn header_pinned_without_mark() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_gc_bits(1 << gc_bit::PINNED);
    assert!(h.is_pinned());
    assert!(!h.is_marked());
    assert!(!h.is_forwarded());
}

// ── gc_bit module constants ───────────────────────────────────────

#[test]
fn gc_bit_constants_values() {
    assert_eq!(gc_bit::MARK, 7);
    assert_eq!(gc_bit::GREY, 6);
    assert_eq!(gc_bit::FORWARDED, 5);
    assert_eq!(gc_bit::PINNED, 4);
    assert_eq!(gc_bit::REMEMBERED, 3);
}

// ── type_id constants ─────────────────────────────────────────────

#[test]
fn type_id_constants_values() {
    assert_eq!(type_id::CONS, 0x01);
    assert_eq!(type_id::SYMBOL, 0x02);
    assert_eq!(type_id::SIMPLE_VECTOR, 0x03);
    assert_eq!(type_id::SIMPLE_ARRAY, 0x04);
    assert_eq!(type_id::SIMPLE_BASE_STRING, 0x05);
    assert_eq!(type_id::SIMPLE_CHARACTER_STRING, 0x06);
    assert_eq!(type_id::COMPLEX_ARRAY, 0x07);
    assert_eq!(type_id::BIGNUM, 0x08);
    assert_eq!(type_id::RATIO, 0x09);
    assert_eq!(type_id::COMPLEX, 0x0A);
    assert_eq!(type_id::DOUBLE_FLOAT, 0x0B);
    assert_eq!(type_id::HASH_TABLE, 0x0C);
    assert_eq!(type_id::STRUCTURE, 0x0D);
    assert_eq!(type_id::STANDARD_OBJECT, 0x0E);
    assert_eq!(type_id::FUNCTION_INTERPRETED, 0x0F);
    assert_eq!(type_id::COMPILED_FUNCTION, 0x10);
    assert_eq!(type_id::CLOSURE, 0x11);
    assert_eq!(type_id::PACKAGE, 0x12);
    assert_eq!(type_id::STREAM, 0x13);
    assert_eq!(type_id::PATHNAME, 0x14);
    assert_eq!(type_id::READTABLE, 0x15);
    assert_eq!(type_id::CONDITION, 0x16);
    assert_eq!(type_id::RESTART, 0x17);
}

#[test]
fn type_id_range_is_contiguous() {
    // Verify IDs run from 0x01..=0x17 with no gaps
    let ids = [
        type_id::CONS, type_id::SYMBOL, type_id::SIMPLE_VECTOR,
        type_id::SIMPLE_ARRAY, type_id::SIMPLE_BASE_STRING,
        type_id::SIMPLE_CHARACTER_STRING, type_id::COMPLEX_ARRAY,
        type_id::BIGNUM, type_id::RATIO, type_id::COMPLEX,
        type_id::DOUBLE_FLOAT, type_id::HASH_TABLE, type_id::STRUCTURE,
        type_id::STANDARD_OBJECT, type_id::FUNCTION_INTERPRETED,
        type_id::COMPILED_FUNCTION, type_id::CLOSURE, type_id::PACKAGE,
        type_id::STREAM, type_id::PATHNAME, type_id::READTABLE,
        type_id::CONDITION, type_id::RESTART,
    ];
    for (i, &id) in ids.iter().enumerate() {
        assert_eq!(id, (i + 1) as u8);
    }
}

// ── ConsCell layout ───────────────────────────────────────────────

#[test]
fn cons_cell_size_is_16_bytes() {
    assert_eq!(mem::size_of::<ConsCell>(), 16);
}

#[test]
fn cons_cell_car_offset_is_0() {
    assert_eq!(memoffset(|c: &ConsCell| &c.car), 0);
}

#[test]
fn cons_cell_cdr_offset_is_8() {
    assert_eq!(memoffset(|c: &ConsCell| &c.cdr), 8);
}

#[test]
fn cons_cell_car_cdr_are_blissval() {
    // Structural test: car and cdr must be BlissVal (8 bytes each)
    assert_eq!(mem::size_of::<BlissVal>(), 8);
}

// ── SymbolData layout ─────────────────────────────────────────────

#[test]
fn symbol_data_has_correct_fields() {
    let size = mem::size_of::<SymbolData>();
    // header(8) + name(8) + value(8) + function(8) + plist(8) + package(8) + flags(4) + tls_index(4) = 56
    assert_eq!(size, 56);
}

#[test]
fn symbol_data_field_offsets() {
    assert_eq!(memoffset(|s: &SymbolData| &s.header), 0);
    assert_eq!(memoffset(|s: &SymbolData| &s.name), 8);
    assert_eq!(memoffset(|s: &SymbolData| &s.value), 16);
    assert_eq!(memoffset(|s: &SymbolData| &s.function), 24);
    assert_eq!(memoffset(|s: &SymbolData| &s.plist), 32);
    assert_eq!(memoffset(|s: &SymbolData| &s.package), 40);
    assert_eq!(memoffset(|s: &SymbolData| &s.flags), 48);
    assert_eq!(memoffset(|s: &SymbolData| &s.tls_index), 52);
}

// ── symbol_flags constants ────────────────────────────────────────

#[test]
fn symbol_flags_constants() {
    assert_eq!(symbol_flags::CONSTANT, 1);
    assert_eq!(symbol_flags::SPECIAL, 2);
    assert_eq!(symbol_flags::MACRO, 4);
    assert_eq!(symbol_flags::COMPILER_MACRO, 8);
}

#[test]
fn symbol_flags_are_distinct_bits() {
    let all = symbol_flags::CONSTANT
        | symbol_flags::SPECIAL
        | symbol_flags::MACRO
        | symbol_flags::COMPILER_MACRO;
    assert_eq!(all, 0b1111);
}

// ── ElementTypeTag enum ───────────────────────────────────────────

#[test]
fn element_type_tag_discriminant_values() {
    assert_eq!(ElementTypeTag::General as u8, 0);
    assert_eq!(ElementTypeTag::Bit as u8, 1);
    assert_eq!(ElementTypeTag::U8 as u8, 2);
    assert_eq!(ElementTypeTag::U16 as u8, 3);
    assert_eq!(ElementTypeTag::U32 as u8, 4);
    assert_eq!(ElementTypeTag::U64 as u8, 5);
    assert_eq!(ElementTypeTag::I8 as u8, 6);
    assert_eq!(ElementTypeTag::I16 as u8, 7);
    assert_eq!(ElementTypeTag::I32 as u8, 8);
    assert_eq!(ElementTypeTag::I64 as u8, 9);
    assert_eq!(ElementTypeTag::SingleFloat as u8, 10);
    assert_eq!(ElementTypeTag::DoubleFloat as u8, 11);
    assert_eq!(ElementTypeTag::Character as u8, 12);
    assert_eq!(ElementTypeTag::BaseChar as u8, 13);
}

#[test]
fn element_type_tag_equality() {
    assert_eq!(ElementTypeTag::General, ElementTypeTag::General);
    assert_ne!(ElementTypeTag::General, ElementTypeTag::Bit);
}

// ── BignumHeader layout ──────────────────────────────────────────

#[test]
fn bignum_header_layout() {
    assert_eq!(memoffset(|b: &BignumHeader| &b.header), 0);
    assert_eq!(memoffset(|b: &BignumHeader| &b.sign), 8);
    assert_eq!(memoffset(|b: &BignumHeader| &b.n_limbs), 12);
    assert_eq!(mem::size_of::<BignumHeader>(), 16);
}

// ── RatioData layout ─────────────────────────────────────────────

#[test]
fn ratio_data_layout() {
    assert_eq!(memoffset(|r: &RatioData| &r.header), 0);
    assert_eq!(memoffset(|r: &RatioData| &r.numerator), 8);
    assert_eq!(memoffset(|r: &RatioData| &r.denominator), 16);
    assert_eq!(mem::size_of::<RatioData>(), 24);
}

// ── ComplexData layout ───────────────────────────────────────────

#[test]
fn complex_data_layout() {
    assert_eq!(memoffset(|c: &ComplexData| &c.header), 0);
    assert_eq!(memoffset(|c: &ComplexData| &c.realpart), 8);
    assert_eq!(memoffset(|c: &ComplexData| &c.imagpart), 16);
    assert_eq!(mem::size_of::<ComplexData>(), 24);
}

// ── DoubleFloatData layout ───────────────────────────────────────

#[test]
fn double_float_data_layout() {
    assert_eq!(memoffset(|d: &DoubleFloatData| &d.header), 0);
    assert_eq!(memoffset(|d: &DoubleFloatData| &d.value), 8);
    assert_eq!(mem::size_of::<DoubleFloatData>(), 16);
}

// ── InterpretedFunctionData layout ───────────────────────────────

#[test]
fn interpreted_function_data_layout() {
    assert_eq!(memoffset(|f: &InterpretedFunctionData| &f.header), 0);
    assert_eq!(memoffset(|f: &InterpretedFunctionData| &f.lambda_list), 8);
    assert_eq!(memoffset(|f: &InterpretedFunctionData| &f.body), 16);
    assert_eq!(memoffset(|f: &InterpretedFunctionData| &f.env), 24);
    assert_eq!(memoffset(|f: &InterpretedFunctionData| &f.name), 32);
    assert_eq!(mem::size_of::<InterpretedFunctionData>(), 40);
}

// ── CompiledFunctionData layout ──────────────────────────────────

#[test]
fn compiled_function_data_layout() {
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.header), 0);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.entry_point), 8);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.code_size), 16);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.name), 24);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.lambda_list), 32);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.min_args), 40);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.max_args), 42);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.tier), 44);
    assert_eq!(memoffset(|f: &CompiledFunctionData| &f.constants), 48);
    assert_eq!(mem::size_of::<CompiledFunctionData>(), 56);
}

// ── ClosureData layout ──────────────────────────────────────────

#[test]
fn closure_data_layout() {
    assert_eq!(memoffset(|c: &ClosureData| &c.header), 0);
    assert_eq!(memoffset(|c: &ClosureData| &c.function), 8);
    assert_eq!(mem::size_of::<ClosureData>(), 16);
}

// ── StreamData layout ───────────────────────────────────────────

#[test]
fn stream_data_layout() {
    assert_eq!(memoffset(|s: &StreamData| &s.header), 0);
    assert_eq!(memoffset(|s: &StreamData| &s.direction), 8);
    assert_eq!(memoffset(|s: &StreamData| &s.element_type), 9);
    assert_eq!(memoffset(|s: &StreamData| &s.ops), 16);
    assert_eq!(memoffset(|s: &StreamData| &s.state), 24);
    assert_eq!(memoffset(|s: &StreamData| &s.column), 32);
    assert_eq!(mem::size_of::<StreamData>(), 40);
}

// ── stream_direction constants ───────────────────────────────────

#[test]
fn stream_direction_constants() {
    assert_eq!(stream_direction::INPUT, 0);
    assert_eq!(stream_direction::OUTPUT, 1);
    assert_eq!(stream_direction::IO, 2);
}

// ── PathnameData layout ─────────────────────────────────────────

#[test]
fn pathname_data_layout() {
    assert_eq!(memoffset(|p: &PathnameData| &p.header), 0);
    assert_eq!(memoffset(|p: &PathnameData| &p.host), 8);
    assert_eq!(memoffset(|p: &PathnameData| &p.device), 16);
    assert_eq!(memoffset(|p: &PathnameData| &p.directory), 24);
    assert_eq!(memoffset(|p: &PathnameData| &p.name), 32);
    assert_eq!(memoffset(|p: &PathnameData| &p.type_field), 40);
    assert_eq!(memoffset(|p: &PathnameData| &p.version), 48);
    assert_eq!(mem::size_of::<PathnameData>(), 56);
}

// ── ReadtableData layout ────────────────────────────────────────

#[test]
fn readtable_data_layout() {
    assert_eq!(memoffset(|r: &ReadtableData| &r.header), 0);
    assert_eq!(memoffset(|r: &ReadtableData| &r.case_mode), 8);
    assert_eq!(memoffset(|r: &ReadtableData| &r.char_table), 16);
    assert_eq!(memoffset(|r: &ReadtableData| &r.extended_table), 24);
    assert_eq!(memoffset(|r: &ReadtableData| &r.macro_table), 32);
    assert_eq!(memoffset(|r: &ReadtableData| &r.dispatch_table), 40);
    assert_eq!(mem::size_of::<ReadtableData>(), 48);
}

// ── RestartData layout ──────────────────────────────────────────

#[test]
fn restart_data_layout() {
    assert_eq!(memoffset(|r: &RestartData| &r.header), 0);
    assert_eq!(memoffset(|r: &RestartData| &r.name), 8);
    assert_eq!(memoffset(|r: &RestartData| &r.function), 16);
    assert_eq!(memoffset(|r: &RestartData| &r.report_function), 24);
    assert_eq!(memoffset(|r: &RestartData| &r.interactive_function), 32);
    assert_eq!(memoffset(|r: &RestartData| &r.test_function), 40);
    assert_eq!(mem::size_of::<RestartData>(), 48);
}

// ── Edge cases: type_id boundaries ──────────────────────────────

#[test]
fn header_type_id_boundary_0x00() {
    let h = ObjectHeader::new(0x00, 1);
    assert_eq!(h.type_id(), 0x00);
    assert_eq!(h.size_units(), 1);
}

#[test]
fn header_type_id_boundary_0xff() {
    let h = ObjectHeader::new(0xFF, 1);
    assert_eq!(h.type_id(), 0xFF);
    assert_eq!(h.size_units(), 1);
}

// ── Edge cases: size_units boundaries ───────────────────────────

#[test]
fn header_size_units_boundary_0() {
    let h = ObjectHeader::new(0x01, 0);
    assert_eq!(h.size_units(), 0);
    assert!(!h.is_large_object());
}

#[test]
fn header_size_units_boundary_0xffff() {
    let h = ObjectHeader::new(0x01, 0xFFFF);
    assert_eq!(h.size_units(), 0xFFFF);
    assert!(h.is_large_object());
}

// ── Edge cases: hash boundaries ─────────────────────────────────

#[test]
fn header_hash_boundary_zero() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0);
    assert_eq!(h.hash(), 0);
}

#[test]
fn header_hash_boundary_max() {
    let mut h = ObjectHeader::new(0x01, 1);
    h.set_hash(0xFFFFFFFF);
    assert_eq!(h.hash(), 0xFFFFFFFF);
}

// ── ObjectHeader is 8 bytes ─────────────────────────────────────

#[test]
fn object_header_is_8_bytes() {
    assert_eq!(mem::size_of::<ObjectHeader>(), 8);
}

// ── Helper: field offset calculator ─────────────────────────────

/// Compute the byte offset of a field within a struct.
/// Uses a zeroed instance — safe for repr(C) structs with no validity invariants
/// beyond those guaranteed by all-zeros.
fn memoffset<T, F, R>(field: F) -> usize
where
    F: Fn(&T) -> &R,
{
    // Safety: We create a zeroed T on the stack, take a reference, compute
    // the field pointer offset, then immediately drop it.  All our structs
    // are repr(C) with fields that are valid at all-zeros (u8/u16/u32/u64/
    // f64/pointers/BlissVal which is repr(transparent) over u64).
    let val: T = unsafe { mem::zeroed() };
    let base = &val as *const T as usize;
    let field_ptr = field(&val) as *const R as usize;
    field_ptr - base
}
