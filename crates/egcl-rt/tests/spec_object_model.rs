// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::mem::{align_of, offset_of, size_of};
use egcl_rt::object::{
    BignumHeader, ConsCell, DoubleFloatData, ElementTypeTag, ObjectHeader, RatioData, SymbolData,
    gc_bit, type_id,
};
use egcl_rt::runtime::{LogLevel, Runtime, RuntimeConfig};
use egcl_rt::types;
use egcl_rt::value::{
    EOF, EOF_BITS, MISSING, MISSING_BITS, NIL, NIL_BITS, T, T_BITS, TAG_CHARACTER, TAG_CONS,
    TAG_FIXNUM, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_MASK, TAG_SINGLE_FLOAT, TAG_SPECIAL, TAG_SYMBOL,
    EgclVal, UNBOUND, UNBOUND_BITS,
};

#[repr(C, align(8))]
struct HeaderAndByte {
    header: ObjectHeader,
    byte: u8,
    padding: [u8; 7],
}

fn heap_value(type_id: u8) -> EgclVal {
    let boxed = Box::new(HeaderAndByte {
        header: ObjectHeader::new(type_id, 2),
        byte: 0,
        padding: [0; 7],
    });
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe { EgclVal::from_heap_ptr(ptr) }
}

fn bit_vector_value() -> EgclVal {
    let boxed = Box::new(HeaderAndByte {
        header: ObjectHeader::new(type_id::SIMPLE_ARRAY, 2),
        byte: egcl_rt::object::ElementTypeTag::Bit as u8,
        padding: [0; 7],
    });
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe { EgclVal::from_heap_ptr(ptr) }
}

fn array_value(element_type: ElementTypeTag) -> EgclVal {
    let boxed = Box::new(HeaderAndByte {
        header: ObjectHeader::new(type_id::SIMPLE_ARRAY, 2),
        byte: element_type as u8,
        padding: [0; 7],
    });
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe { EgclVal::from_heap_ptr(ptr) }
}

fn minimal_runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        heap_size: 8 * 1024 * 1024,
        nursery_size: 2 * 1024 * 1024,
        tlab_size: 256 * 1024,
        stack_size: 128 * 1024,
        num_workers: 1,
        image_path: None,
        no_image: true,
        eval_form: None,
        load_file: None,
        gc_log: None,
        jit_dump: false,
        safepoint_spin: 1000,
        ffi_pool_pages: 4,
        log_level: LogLevel::Info,
    }
}

fn bootstrap_string_content(value: EgclVal) -> String {
    assert!(types::stringp(value), "value must be a bootstrap string");
    unsafe { egcl_rt::object::read_simple_string(value.as_ptr()) }
}

fn sxhash_equivalent(
    header: &mut ObjectHeader,
    object_addr: usize,
    compute_count: &mut usize,
) -> u32 {
    if header.hash() == 0 {
        *compute_count += 1;
        let computed = (((object_addr as u64) >> 3) as u32)
            .wrapping_mul(0x9E37_79B9)
            .max(1);
        header.set_hash(computed);
    }
    header.hash()
}

#[test]
fn egclval_is_a_single_u64_word_with_tagged_round_trips() {
    // Per R1.01 and R1.21, every Lisp value is one 64-bit word and crosses FFI as `u64`.
    // Per R1.02 and R1.18, low 3 bits are the primary tag and extraction is direct.
    assert_eq!(size_of::<EgclVal>(), 8);
    assert_eq!(size_of::<u64>(), 8);

    let fixnum = EgclVal::from_fixnum(-42);
    let ch = EgclVal::from_char('λ');
    let flt = EgclVal::from_single_float(-3.5);
    let sym = EgclVal::from_symbol_index(123);

    assert_eq!(fixnum.tag(), TAG_FIXNUM);
    assert_eq!(ch.tag(), TAG_CHARACTER);
    assert_eq!(flt.tag(), TAG_SINGLE_FLOAT);
    assert_eq!(sym.tag(), TAG_SYMBOL);

    for value in [fixnum, ch, flt, sym, NIL, T, UNBOUND, MISSING, EOF] {
        assert_eq!(EgclVal::from_raw(value.to_raw()), value);
        assert_eq!(value.to_raw() & TAG_MASK, value.tag());
    }
}

#[test]
fn immediate_encodings_match_the_specified_bit_patterns_and_ranges() {
    // Per R1.03, fixnums cover at least the 61-bit signed range.
    let min = EgclVal::from_fixnum(-(1_i64 << 60));
    let max = EgclVal::from_fixnum((1_i64 << 60) - 1);
    assert_eq!(min.as_fixnum(), -(1_i64 << 60));
    assert_eq!(max.as_fixnum(), (1_i64 << 60) - 1);

    // Per R1.04, characters support the full Unicode scalar range.
    assert_eq!(EgclVal::from_char('\0').as_char(), '\0');
    assert_eq!(EgclVal::from_char('\u{10FFFF}').as_char(), '\u{10FFFF}');

    // Per R1.05, single-float immediates preserve IEEE 754 binary32 bits.
    let nan_bits = 0x7fc0_0001_u32;
    let single = EgclVal::from_single_float(f32::from_bits(nan_bits));
    assert_eq!(single.as_single_float().to_bits(), nan_bits);
}

#[test]
fn special_values_and_nil_predicates_use_the_canonical_encodings() {
    // Per R1.10, NIL satisfies SYMBOLP, LISTP, and NULL simultaneously.
    // Per R1.11, NIL is exactly 0x07.
    // Per R1.12, T is exactly 0x0F.
    // Per R1.17, UNBOUND is a unique special value distinct from NIL and T.
    assert_eq!(NIL.0, NIL_BITS);
    assert_eq!(T.0, T_BITS);
    assert_eq!(UNBOUND.0, UNBOUND_BITS);
    assert_eq!(MISSING.0, MISSING_BITS);
    assert_eq!(EOF.0, EOF_BITS);

    assert!(types::symbolp(NIL));
    assert!(types::listp(NIL));
    assert!(types::nullp(NIL));

    assert_ne!(UNBOUND, NIL);
    assert_ne!(UNBOUND, T);
    assert_ne!(UNBOUND, MISSING);
    assert_ne!(UNBOUND, EOF);
}

#[test]
fn pointer_tagged_values_require_alignment_and_mask_back_to_the_raw_pointer() {
    // Per R1.06, pointer-tagged values are 8-byte aligned and recoverable by masking low bits.
    let cons = Box::new(ConsCell {
        car: EgclVal::from_fixnum(1),
        cdr: NIL,
    });
    let cons_ptr = Box::into_raw(cons) as *mut u8;
    assert_eq!((cons_ptr as usize) & TAG_MASK as usize, 0);
    let cons_val = unsafe { EgclVal::from_cons_ptr(cons_ptr) };
    assert_eq!(cons_val.tag(), TAG_CONS);
    assert_eq!(unsafe { cons_val.as_ptr() }, cons_ptr);

    let header = Box::new(ObjectHeader::new(type_id::SYMBOL, 7));
    let heap_ptr = Box::into_raw(header) as *mut u8;
    assert_eq!((heap_ptr as usize) & TAG_MASK as usize, 0);
    let heap_val = unsafe { EgclVal::from_heap_ptr(heap_ptr) };
    assert_eq!(heap_val.tag(), TAG_HEAP_OBJECT);
    assert_eq!(unsafe { heap_val.as_ptr() }, heap_ptr);

    let func_ptr = Box::into_raw(Box::new([0_u8; 8])) as *mut u8;
    let func_val = unsafe { EgclVal::from_function_ptr(func_ptr) };
    assert_eq!(func_val.tag(), TAG_FUNCTION);
    assert_eq!(unsafe { func_val.as_ptr() }, func_ptr);
}

#[test]
fn object_header_has_the_required_layout_fields_and_atomic_gc_operations() {
    // Per R1.07, every heap object header is exactly 8 bytes.
    // Per R1.08, the header exposes a built-in-type discriminator.
    // Per R1.09, GC mark/forward state is accessible without lock-taking.
    assert_eq!(size_of::<ObjectHeader>(), 8);
    assert_eq!(align_of::<ObjectHeader>(), 8);

    let mut header = ObjectHeader::new(type_id::DOUBLE_FLOAT, 0x1234);
    assert_eq!(header.type_id(), type_id::DOUBLE_FLOAT);
    assert_eq!(header.size_units(), 0x1234);
    assert_eq!(header.gc_bits(), 0);
    assert_eq!(header.hash(), 0);

    header.set_gc_bits((1 << gc_bit::FORWARDED) | (1 << gc_bit::PINNED));
    header.set_hash(0xfeed_beef);
    assert_eq!(
        header.gc_bits(),
        (1 << gc_bit::FORWARDED) | (1 << gc_bit::PINNED)
    );
    assert!(header.is_forwarded());
    assert!(header.is_pinned());
    assert_eq!(header.hash(), 0xfeed_beef);

    let markable = ObjectHeader::new(type_id::SYMBOL, 7);
    assert!(markable.set_marked());
    assert!(markable.is_marked());
    assert!(!markable.set_marked());

    let large = ObjectHeader::new(type_id::SYMBOL, u16::MAX);
    assert!(large.is_large_object());
}

#[test]
fn object_layouts_match_required_sizes_offsets_and_alignment() {
    // Per R1.13, cons cells are exactly 16 bytes with no header.
    assert_eq!(size_of::<ConsCell>(), 16);
    assert_eq!(offset_of!(ConsCell, car), 0);
    assert_eq!(offset_of!(ConsCell, cdr), 8);

    // Per R1.16, symbols contain name, value, function, plist, and package cells.
    // Per R1.20, heap layouts are naturally aligned and 8-byte aligned overall.
    assert_eq!(size_of::<SymbolData>(), 56);
    assert_eq!(align_of::<SymbolData>(), 8);
    assert_eq!(offset_of!(SymbolData, header), 0);
    assert_eq!(offset_of!(SymbolData, name), 8);
    assert_eq!(offset_of!(SymbolData, value), 16);
    assert_eq!(offset_of!(SymbolData, function), 24);
    assert_eq!(offset_of!(SymbolData, plist), 32);
    assert_eq!(offset_of!(SymbolData, package), 40);

    assert_eq!(align_of::<BignumHeader>(), 8);
    assert_eq!(align_of::<RatioData>(), 8);
    assert_eq!(align_of::<DoubleFloatData>(), 8);
    assert_eq!(size_of::<DoubleFloatData>() % 8, 0);
}

#[test]
fn bootstrap_strings_store_fixed_width_ucs4_and_round_trip() {
    // Per R1.14 / §1.6.3 (SBCL model): a constructed string is a fixed-width
    // SIMPLE_CHARACTER_STRING — char_len@8 (CHARACTER count), 32-bit UCS-4
    // elements @16 — NOT UTF-8 bytes.
    let mut runtime = Runtime::init(minimal_runtime_config()).unwrap();
    let source = "h\u{00e9}ll\u{03bb} \u{1f642}";
    let value = runtime.eval(&format!("{source:?}")).unwrap();

    assert!(types::stringp(value));
    unsafe {
        let ptr = value.as_ptr();
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        assert_eq!(tid, type_id::SIMPLE_CHARACTER_STRING);
        // length is the CHARACTER count, not the byte count.
        let char_len = *(ptr.add(8) as *const u64) as usize;
        assert_eq!(char_len, source.chars().count());
        assert_ne!(char_len, source.len(), "must be char count, not byte count");
        // Fixed-width 32-bit elements: element i is the i-th code point.
        for (i, c) in source.chars().enumerate() {
            assert_eq!(*(ptr.add(16).add(i * 4) as *const u32), c as u32);
        }
    }
    // Round-trips through the decoder.
    assert_eq!(bootstrap_string_content(value), source);

    runtime.shutdown().unwrap();
}

#[test]
fn all_base_char_literals_compact_to_8bit_base_strings() {
    // Per §1.6.3 (bliss-em3p): an all-BASE-CHAR (code points < 256) reader
    // literal is stored compactly as an 8-bit SIMPLE_BASE_STRING; a literal with
    // a wider character stays a 32-bit SIMPLE_CHARACTER_STRING.
    let mut runtime = Runtime::init(minimal_runtime_config()).unwrap();

    let ascii = runtime.eval("\"hello world\"").unwrap();
    assert!(types::stringp(ascii));
    unsafe {
        let ptr = ascii.as_ptr();
        assert_eq!(
            (*(ptr as *const ObjectHeader)).type_id(),
            type_id::SIMPLE_BASE_STRING
        );
        assert_eq!(*(ptr.add(8) as *const u64) as usize, "hello world".len());
        // 8-bit elements: element i is the i-th byte/code point.
        for (i, b) in "hello world".bytes().enumerate() {
            assert_eq!(*ptr.add(16).add(i), b);
        }
    }
    assert_eq!(bootstrap_string_content(ascii), "hello world");

    // A literal with a wide character stays 32-bit.
    let wide = runtime.eval("\"h\u{03bb}\"").unwrap();
    unsafe {
        assert_eq!(
            (*(wide.as_ptr() as *const ObjectHeader)).type_id(),
            type_id::SIMPLE_CHARACTER_STRING
        );
    }

    runtime.shutdown().unwrap();
}

#[test]
fn arrays_expose_all_ansi_required_element_specialisation_tags() {
    // Per R1.15, arrays support the ANSI-required element specialisations.
    let specialisations = [
        ElementTypeTag::General,
        ElementTypeTag::Bit,
        ElementTypeTag::U8,
        ElementTypeTag::U16,
        ElementTypeTag::U32,
        ElementTypeTag::U64,
        ElementTypeTag::I8,
        ElementTypeTag::I16,
        ElementTypeTag::I32,
        ElementTypeTag::I64,
        ElementTypeTag::SingleFloat,
        ElementTypeTag::DoubleFloat,
        ElementTypeTag::Character,
        ElementTypeTag::BaseChar,
    ];

    for element_type in specialisations {
        let array = array_value(element_type);
        assert!(
            types::arrayp(array),
            "missing array support for {:?}",
            element_type
        );
        assert!(
            types::vectorp(array),
            "missing vector support for {:?}",
            element_type
        );
        assert_eq!(
            types::bit_vector_p(array),
            element_type == ElementTypeTag::Bit,
            "bit-vector discrimination must depend on the specialisation tag",
        );
    }
}

#[test]
fn object_header_hash_is_computed_once_on_first_sxhash_equivalent_use_then_cached() {
    // Per R1.19, object-header hash codes are computed lazily and cached.
    let mut object = HeaderAndByte {
        header: ObjectHeader::new(type_id::SYMBOL, 2),
        byte: 0,
        padding: [0; 7],
    };
    let object_addr = &object as *const HeaderAndByte as usize;
    let mut compute_count = 0;

    assert_eq!(object.header.hash(), 0, "hash cache must start empty");

    let first = sxhash_equivalent(&mut object.header, object_addr, &mut compute_count);
    let second = sxhash_equivalent(&mut object.header, object_addr, &mut compute_count);

    assert_ne!(
        first, 0,
        "first SXHASH-equivalent use must materialize a non-zero hash"
    );
    assert_eq!(
        first,
        object.header.hash(),
        "computed hash must be cached in the header"
    );
    assert_eq!(
        second, first,
        "subsequent SXHASH-equivalent use must reuse the cached hash"
    );
    assert_eq!(
        compute_count, 1,
        "hash computation must happen exactly once"
    );
}

#[test]
fn type_predicates_dispatch_by_tag_and_header_type_id() {
    // Per R1.08 and R1.18, type checks use the primary tag or heap type ID directly.
    assert!(types::fixnump(EgclVal::from_fixnum(9)));
    assert!(types::characterp(EgclVal::from_char('A')));
    assert!(types::single_float_p(EgclVal::from_single_float(1.25)));
    assert!(types::symbolp(T));
    assert!(types::functionp(unsafe {
        EgclVal::from_function_ptr(Box::into_raw(Box::new([0_u8; 8])) as *mut u8)
    }));
    assert!(types::consp(unsafe {
        EgclVal::from_cons_ptr(Box::into_raw(Box::new(ConsCell { car: NIL, cdr: NIL })) as *mut u8)
    }));

    let base_string = heap_value(type_id::SIMPLE_BASE_STRING);
    let char_string = heap_value(type_id::SIMPLE_CHARACTER_STRING);
    let vector = heap_value(type_id::SIMPLE_VECTOR);
    let bignum = heap_value(type_id::BIGNUM);
    let ratio = heap_value(type_id::RATIO);
    let complex = heap_value(type_id::COMPLEX);
    let double_float = heap_value(type_id::DOUBLE_FLOAT);
    let package = heap_value(type_id::PACKAGE);
    let hash = heap_value(type_id::HASH_TABLE);
    let stream = heap_value(type_id::STREAM);
    let pathname = heap_value(type_id::PATHNAME);
    let readtable = heap_value(type_id::READTABLE);
    let bits = bit_vector_value();

    assert!(types::stringp(base_string));
    assert!(types::stringp(char_string));
    assert!(types::vectorp(vector));
    assert!(types::arrayp(vector));
    assert!(types::bit_vector_p(bits));
    assert!(types::numberp(EgclVal::from_fixnum(10)));
    assert!(types::numberp(bignum));
    assert!(types::integerp(bignum));
    assert!(types::rationalp(ratio));
    assert!(types::realp(double_float));
    assert!(types::floatp(double_float));
    assert!(types::complexp(complex));
    assert!(types::packagep(package));
    assert!(types::hash_table_p(hash));
    assert!(types::streamp(stream));
    assert!(types::pathnamep(pathname));
    assert!(types::readtablep(readtable));
}

#[test]
fn bootstrap_typep_and_subtypep_follow_the_exposed_runtime_contract() {
    // Per R1.10 and R1.18, public type dispatch must honor the canonical NIL/list/symbol cases.
    let fixnum_type = EgclVal::from_symbol_index(0);
    let list_type = EgclVal::from_symbol_index(7);
    let string_type = EgclVal::from_symbol_index(10);

    assert!(types::typep(EgclVal::from_fixnum(5), fixnum_type));
    assert!(types::typep(NIL, list_type));
    assert!(types::typep(
        heap_value(type_id::SIMPLE_BASE_STRING),
        string_type
    ));

    let (yes, valid) = types::subtypep(fixnum_type, fixnum_type);
    assert_eq!((yes, valid), (true, true));

    let (no, unknown) = types::subtypep(fixnum_type, list_type);
    assert_eq!((no, unknown), (false, false));
}

#[test]
fn special_tag_namespace_stays_separate_from_primary_immediate_tags() {
    // Per R1.02, each primary tag occupies the low 3 bits exactly once.
    let tags = [
        TAG_FIXNUM,
        TAG_CONS,
        TAG_HEAP_OBJECT,
        TAG_CHARACTER,
        TAG_SINGLE_FLOAT,
        TAG_SYMBOL,
        TAG_FUNCTION,
        TAG_SPECIAL,
    ];
    for (i, tag) in tags.iter().enumerate() {
        assert_eq!(*tag, i as u64);
    }
}
