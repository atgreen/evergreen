// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::{Collector, EgclVal, HeapCollector};
use egcl_stdlib::{sequences, streams};

#[test]
fn nil_storage_metadata_displacement_and_stream_writes_remain_precise() {
    egcl_rt::rooted!(base = sequences::build_nil_complex_vector(6, 2, true, true).unwrap());
    egcl_rt::rooted!(
        view = sequences::build_nil_displaced_vector(*base, 1, 3, 0, false, true).unwrap()
    );
    assert_eq!(sequences::array_total_size(*view), Some(3));
    assert_eq!(sequences::length(*view).unwrap(), 0);
    assert_eq!(
        sequences::array_element_kind(*view),
        Some(sequences::ArrayElementKind::Nil)
    );
    assert!(sequences::elt(*base, 0).is_err());
    assert!(sequences::set_elt(*base, 0, EgclVal::from_char('x')).is_err());
    let before = base.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(base.to_raw(), before, "displacement base must move");
    assert_eq!(sequences::cvec_displacement(*view).unwrap().0, *base);
    egcl_rt::rooted!(copy = sequences::copy_seq(*view).unwrap());
    assert!(egcl_rt::nil_array::is_vector(*copy));
    assert_eq!(sequences::length(*copy).unwrap(), 0);
    egcl_rt::rooted!(empty = streams::make_lisp_string(""));
    assert!(egcl_stdlib::equality::equal(*copy, *empty));
    egcl_rt::rooted!(
        stream = streams::make_string_output_stream_with_element(streams::StringOutputElement::Nil)
            .unwrap()
    );
    assert!(streams::stream_element_type(*stream).is_nil());
    assert!(streams::stream_write_char(*stream, EgclVal::from_char('x')).is_err());
    egcl_rt::rooted!(text = streams::make_lisp_string_fresh("x"));
    assert!(streams::stream_write_string(*stream, *text, 0, None).is_err());
    streams::stream_write_string(*stream, *copy, 0, None).unwrap();
    egcl_rt::rooted!(result = streams::get_output_stream_string(*stream).unwrap());
    assert!(egcl_rt::nil_array::is_vector(*result));
    assert_eq!(sequences::length(*result).unwrap(), 0);
    egcl_rt::rooted!(again = streams::get_output_stream_string(*stream).unwrap());
    assert_ne!(*result, *again);
    // Existing internal NIL placeholder is still the ordinary default stream.
    egcl_rt::rooted!(ordinary = streams::make_string_output_stream(egcl_rt::value::NIL).unwrap());
    streams::stream_write_char(*ordinary, EgclVal::from_char('x')).unwrap();
    egcl_rt::rooted!(ordinary_result = streams::get_output_stream_string(*ordinary).unwrap());
    assert!(ordinary_result.is_string());
}

#[test]
fn nil_predicates_preserve_registered_strings_and_pathname_sequences() {
    use egcl_stdlib::pathnames;
    egcl_rt::rooted!(text = streams::make_lisp_string("nil-array-control"));
    pathnames::register_string(*text, "nil-array-control");
    assert_eq!(egcl_rt::nil_array::len(*text), None);
    assert!(!sequences::is_nil_array(*text));
    assert_eq!(sequences::length(*text).unwrap(), 17);
    assert_eq!(sequences::array_total_size(*text), Some(17));
    egcl_rt::rooted!(path = pathnames::parse_namestring(*text, None, None).unwrap().0);
    assert_eq!(egcl_rt::nil_array::len(*path), None);
    assert!(!sequences::is_nil_array(*path));
    assert!(!egcl_rt::types::stringp(*path));
    assert!(sequences::length(*path).is_err());
    assert_eq!(sequences::array_total_size(*path), None);
}

#[test]
fn nil_synonym_element_types_follow_long_chains_and_detect_cycles() {
    egcl_rt::rooted!(
        target = streams::make_string_output_stream_with_element(streams::StringOutputElement::Nil)
            .unwrap()
    );
    egcl_rt::rooted!(head = *target);
    let mut symbols = Vec::new();
    egcl_rt::rooted_ref!(_symbols = &mut symbols);
    for i in 0..128 {
        let symbol = EgclVal::from_symbol_index(egcl_rt::symbols::intern(&format!(
            "EGCL-NIL-STREAM-CHAIN-{i}"
        )));
        streams::set_symbol_stream(symbol, *head);
        symbols.push(symbol);
        *head = streams::make_synonym_stream(symbol).unwrap();
    }
    assert!(streams::stream_element_type(*head).is_nil());
    egcl_rt::rooted!(ordinary = streams::make_string_output_stream(egcl_rt::value::NIL).unwrap());
    streams::set_symbol_stream(symbols[0], *ordinary);
    assert!(!streams::stream_element_type(*head).is_nil());
    // Close the complete 128-link chain into a cycle: termination depends on
    // repeated identity, not a fixed limit that rejects valid long chains.
    streams::set_symbol_stream(symbols[0], *head);
    assert!(streams::stream_element_type(*head).is_nil());
    for symbol in symbols.iter().copied() {
        streams::remove_symbol_stream(symbol);
    }
}

#[test]
fn nil_map_and_concatenate_validate_rank_and_length_before_result_allocation() {
    use egcl_rt::value::NIL;
    for spec in [
        "(vector nil 0)",
        "(array nil (0))",
        "(simple-array nil (0))",
    ] {
        let (kind, _) = egcl_compiler::reader::read_from_string(spec).unwrap();
        egcl_rt::rooted!(kind = kind);
        // Empty input never invokes the deliberately non-callable NIL argument.
        egcl_rt::rooted!(mapped = sequences::map(*kind, NIL, &[NIL]).unwrap());
        assert!(egcl_rt::nil_array::is_vector(*mapped), "{spec}");
        egcl_rt::rooted!(joined = sequences::concatenate(*kind, &[NIL]).unwrap());
        assert!(egcl_rt::nil_array::is_vector(*joined), "{spec}");
        assert!(sequences::build_result_sequence(*kind, &[NIL]).is_err());
    }
    for spec in [
        "(array nil ())",
        "(simple-array nil (0 0))",
        "(vector nil 1)",
        "(array nil (1))",
        "(simple-array nil (1))",
    ] {
        let (kind, _) = egcl_compiler::reader::read_from_string(spec).unwrap();
        egcl_rt::rooted!(kind = kind);
        assert!(sequences::map(*kind, NIL, &[NIL]).is_err(), "{spec}");
        assert!(sequences::concatenate(*kind, &[NIL]).is_err(), "{spec}");
    }
}
