//! Tests for bliss-stdlib error types (StdlibError enum).
//!
//! Covers: every variant's construction & field access, Debug output,
//! std::error::Error trait conformance, to_condition conversion,
//! plus additional edge-case tests for streams and hashtable modules.

use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::error::{StdlibError, to_condition};
use bliss_stdlib::hashtable::*;
use bliss_stdlib::streams::*;

// ── StdlibError variant construction and field access ────────────────

#[test]
fn package_conflict_fields() {
    let err = StdlibError::PackageConflict {
        package: "CL-USER".into(), symbol: "FOO".into(), message: "conflict".into(),
    };
    match &err {
        StdlibError::PackageConflict { package, symbol, message } => {
            assert_eq!(package, "CL-USER");
            assert_eq!(symbol, "FOO");
            assert_eq!(message, "conflict");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn package_not_found_field() {
    match &StdlibError::PackageNotFound("NO-SUCH".into()) {
        StdlibError::PackageNotFound(name) => assert_eq!(name, "NO-SUCH"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn stream_io_error_fields() {
    let err = StdlibError::StreamIoError { stream: "stdin".into(), message: "fail".into() };
    match &err {
        StdlibError::StreamIoError { stream, message } => {
            assert_eq!(stream, "stdin");
            assert_eq!(message, "fail");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn end_of_file_field() {
    match &StdlibError::EndOfFile { stream: "s".into() } {
        StdlibError::EndOfFile { stream } => assert_eq!(stream, "s"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn pathname_error_fields() {
    let err = StdlibError::PathnameError { pathname: "/tmp/x".into(), message: "m".into() };
    match &err {
        StdlibError::PathnameError { pathname, message } => {
            assert_eq!(pathname, "/tmp/x");
            assert_eq!(message, "m");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn logical_pathname_error_fields() {
    let err = StdlibError::LogicalPathnameError { host: "SYS".into(), message: "m".into() };
    match &err {
        StdlibError::LogicalPathnameError { host, message } => {
            assert_eq!(host, "SYS");
            assert_eq!(message, "m");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn format_error_fields() {
    let err = StdlibError::FormatError {
        control_string: "~A ~X".into(), position: 3, message: "bad".into(),
    };
    match &err {
        StdlibError::FormatError { control_string, position, message } => {
            assert_eq!(control_string, "~A ~X");
            assert_eq!(*position, 3);
            assert_eq!(message, "bad");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn clos_error_field() {
    match &StdlibError::ClosError { message: "no method".into() } {
        StdlibError::ClosError { message } => assert_eq!(message, "no method"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn unbound_slot_fields() {
    let err = StdlibError::UnboundSlot { instance: "obj".into(), slot_name: "X".into() };
    match &err {
        StdlibError::UnboundSlot { instance, slot_name } => {
            assert_eq!(instance, "obj");
            assert_eq!(slot_name, "X");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn method_combination_error_fields() {
    let err = StdlibError::MethodCombinationError {
        generic_function: "GF".into(), message: "bad".into(),
    };
    match &err {
        StdlibError::MethodCombinationError { generic_function, message } => {
            assert_eq!(generic_function, "GF");
            assert_eq!(message, "bad");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn sequence_bounds_error_fields() {
    let err = StdlibError::SequenceBoundsError { sequence_length: 5, index: 10 };
    match &err {
        StdlibError::SequenceBoundsError { sequence_length, index } => {
            assert_eq!(*sequence_length, 5);
            assert_eq!(*index, 10);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn hash_table_error_field() {
    match &StdlibError::HashTableError { message: "bad".into() } {
        StdlibError::HashTableError { message } => assert_eq!(message, "bad"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn sort_error_field() {
    match &StdlibError::SortError { message: "bad pred".into() } {
        StdlibError::SortError { message } => assert_eq!(message, "bad pred"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn print_not_readable_field() {
    match &StdlibError::PrintNotReadable { object: "#<fn>".into() } {
        StdlibError::PrintNotReadable { object } => assert_eq!(object, "#<fn>"),
        _ => panic!("wrong variant"),
    }
}

// ── Debug output for every variant ───────────────────────────────────

#[test]
fn debug_output_contains_variant_name_for_all() {
    let cases: Vec<(&str, StdlibError)> = vec![
        ("PackageConflict", StdlibError::PackageConflict {
            package: "P".into(), symbol: "S".into(), message: "M".into(),
        }),
        ("PackageNotFound", StdlibError::PackageNotFound("P".into())),
        ("StreamIoError", StdlibError::StreamIoError { stream: "S".into(), message: "M".into() }),
        ("EndOfFile", StdlibError::EndOfFile { stream: "S".into() }),
        ("PathnameError", StdlibError::PathnameError { pathname: "P".into(), message: "M".into() }),
        ("LogicalPathnameError", StdlibError::LogicalPathnameError { host: "H".into(), message: "M".into() }),
        ("FormatError", StdlibError::FormatError { control_string: "~A".into(), position: 0, message: "M".into() }),
        ("ClosError", StdlibError::ClosError { message: "M".into() }),
        ("UnboundSlot", StdlibError::UnboundSlot { instance: "I".into(), slot_name: "S".into() }),
        ("MethodCombinationError", StdlibError::MethodCombinationError { generic_function: "G".into(), message: "M".into() }),
        ("SequenceBoundsError", StdlibError::SequenceBoundsError { sequence_length: 5, index: 10 }),
        ("HashTableError", StdlibError::HashTableError { message: "M".into() }),
        ("SortError", StdlibError::SortError { message: "M".into() }),
        ("PrintNotReadable", StdlibError::PrintNotReadable { object: "O".into() }),
    ];
    for (name, err) in &cases {
        let dbg = format!("{:?}", err);
        assert!(dbg.contains(name), "Debug for {} should contain variant name, got: {}", name, dbg);
    }
}

#[test]
fn debug_includes_field_values() {
    let err = StdlibError::PackageConflict {
        package: "CL".into(), symbol: "CAR".into(), message: "dup".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("CL"));
    assert!(dbg.contains("CAR"));
}

// ── std::error::Error trait conformance ──────────────────────────────

#[test]
fn implements_error_trait() {
    fn assert_error<E: std::error::Error>(_e: &E) {}
    assert_error(&StdlibError::PackageNotFound("T".into()));
}

#[test]
fn display_non_empty_for_all_variants() {
    let variants: Vec<StdlibError> = vec![
        StdlibError::PackageConflict { package: "P".into(), symbol: "S".into(), message: "M".into() },
        StdlibError::PackageNotFound("P".into()),
        StdlibError::StreamIoError { stream: "S".into(), message: "M".into() },
        StdlibError::EndOfFile { stream: "S".into() },
        StdlibError::PathnameError { pathname: "P".into(), message: "M".into() },
        StdlibError::LogicalPathnameError { host: "H".into(), message: "M".into() },
        StdlibError::FormatError { control_string: "~A".into(), position: 0, message: "M".into() },
        StdlibError::ClosError { message: "M".into() },
        StdlibError::UnboundSlot { instance: "I".into(), slot_name: "S".into() },
        StdlibError::MethodCombinationError { generic_function: "G".into(), message: "M".into() },
        StdlibError::SequenceBoundsError { sequence_length: 0, index: 0 },
        StdlibError::HashTableError { message: "M".into() },
        StdlibError::SortError { message: "M".into() },
        StdlibError::PrintNotReadable { object: "O".into() },
    ];
    for v in &variants {
        let display = format!("{}", v);
        assert!(!display.is_empty(), "Display for {:?} should be non-empty", v);
    }
}

#[test]
fn error_source_is_none() {
    use std::error::Error;
    let err = StdlibError::StreamIoError { stream: "s".into(), message: "m".into() };
    assert!(err.source().is_none());
}

#[test]
fn is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StdlibError>();
}

#[test]
fn can_be_boxed_as_dyn_error() {
    let boxed: Box<dyn std::error::Error> = Box::new(StdlibError::ClosError { message: "t".into() });
    assert!(!format!("{}", boxed).is_empty());
}

// ── to_condition for every variant ───────────────────────────────────

#[test]
fn to_condition_returns_non_nil_for_all_variants() {
    let variants: Vec<StdlibError> = vec![
        StdlibError::PackageConflict { package: "P".into(), symbol: "S".into(), message: "M".into() },
        StdlibError::PackageNotFound("P".into()),
        StdlibError::StreamIoError { stream: "S".into(), message: "M".into() },
        StdlibError::EndOfFile { stream: "S".into() },
        StdlibError::PathnameError { pathname: "P".into(), message: "M".into() },
        StdlibError::LogicalPathnameError { host: "H".into(), message: "M".into() },
        StdlibError::FormatError { control_string: "~Z".into(), position: 0, message: "M".into() },
        StdlibError::ClosError { message: "M".into() },
        StdlibError::UnboundSlot { instance: "I".into(), slot_name: "S".into() },
        StdlibError::MethodCombinationError { generic_function: "G".into(), message: "M".into() },
        StdlibError::SequenceBoundsError { sequence_length: 3, index: 5 },
        StdlibError::HashTableError { message: "M".into() },
        StdlibError::SortError { message: "M".into() },
        StdlibError::PrintNotReadable { object: "#<fn>".into() },
    ];
    for v in &variants {
        let cond = to_condition(v);
        assert_ne!(cond, NIL, "to_condition({:?}) should not return NIL", v);
    }
}

// ── Edge cases: empty/unicode strings, boundary values ───────────────

#[test]
fn empty_string_fields_do_not_panic() {
    let err = StdlibError::PackageConflict { package: "".into(), symbol: "".into(), message: "".into() };
    let _ = format!("{:?}", err);
    let _ = format!("{}", err);
}

#[test]
fn unicode_string_fields() {
    let err = StdlibError::PackageConflict {
        package: "パッケージ".into(), symbol: "シンボル".into(), message: "衝突".into(),
    };
    assert!(format!("{:?}", err).contains("パッケージ"));
}

#[test]
fn sequence_bounds_zero_length_zero_index() {
    match &StdlibError::SequenceBoundsError { sequence_length: 0, index: 0 } {
        StdlibError::SequenceBoundsError { sequence_length, index } => {
            assert_eq!(*sequence_length, 0);
            assert_eq!(*index, 0);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn format_error_max_position() {
    match &StdlibError::FormatError { control_string: "x".into(), position: usize::MAX, message: "m".into() } {
        StdlibError::FormatError { position, .. } => assert_eq!(*position, usize::MAX),
        _ => panic!("wrong variant"),
    }
}

// ══════════════════════════════════════════════════════════════════════
// Additional streams edge cases
// ══════════════════════════════════════════════════════════════════════

#[test]
fn stream_direction_all_distinct() {
    let dirs = [StreamDirection::Input, StreamDirection::Output, StreamDirection::Io];
    for i in 0..dirs.len() {
        for j in (i+1)..dirs.len() {
            assert_ne!(dirs[i], dirs[j]);
        }
    }
}

#[test]
fn write_byte_to_input_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("abc"), 0, None).unwrap();
    assert!(stream_write_byte(s, BlissVal::from_fixnum(65)).is_err());
}

#[test]
fn write_string_to_input_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("abc"), 0, None).unwrap();
    assert!(stream_write_string(s, make_lisp_string("x"), 0, None).is_err());
}

#[test]
fn read_byte_from_output_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    assert!(stream_read_byte(s).is_err());
}

#[test]
fn unread_char_on_output_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    assert!(stream_unread_char(s, BlissVal::from_char('x')).is_err());
}

#[test]
fn element_type_for_broadcast_stream() {
    let s = make_string_output_stream(NIL).unwrap();
    let b = make_broadcast_stream(&[s]).unwrap();
    assert_ne!(stream_element_type(b), NIL);
}

#[test]
fn element_type_for_two_way_stream() {
    let inp = make_string_input_stream(make_lisp_string("t"), 0, None).unwrap();
    let out = make_string_output_stream(NIL).unwrap();
    let tw = make_two_way_stream(inp, out).unwrap();
    assert_ne!(stream_element_type(tw), NIL);
}

#[test]
fn write_string_with_start_end_substring() {
    let s = make_string_output_stream(NIL).unwrap();
    stream_write_string(s, make_lisp_string("hello world"), 6, Some(11)).unwrap();
    assert_eq!(get_output_stream_string(s).unwrap(), make_lisp_string("world"));
}

#[test]
fn write_string_with_start_only() {
    let s = make_string_output_stream(NIL).unwrap();
    stream_write_string(s, make_lisp_string("abcdef"), 3, None).unwrap();
    assert_eq!(get_output_stream_string(s).unwrap(), make_lisp_string("def"));
}

#[test]
fn force_output_on_closed_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    close(s, false).unwrap();
    assert!(stream_force_output(s).is_err());
}

#[test]
fn finish_output_on_closed_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    close(s, false).unwrap();
    assert!(stream_finish_output(s).is_err());
}

#[test]
fn listen_on_closed_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("d"), 0, None).unwrap();
    close(s, false).unwrap();
    assert!(stream_listen(s).is_err());
}

// ══════════════════════════════════════════════════════════════════════
// Additional hashtable edge cases
// ══════════════════════════════════════════════════════════════════════

#[test]
fn rehash_threshold_exactly_one_is_valid() {
    let opts = MakeHashTableOptions { rehash_threshold: 1.0, ..MakeHashTableOptions::default() };
    let ht = make_hash_table(&opts).expect("threshold 1.0 should be valid");
    assert!((hash_table_rehash_threshold(ht).unwrap() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn rehash_size_just_above_one_is_valid() {
    let opts = MakeHashTableOptions { rehash_size: 1.01, ..MakeHashTableOptions::default() };
    let ht = make_hash_table(&opts).expect("rehash_size 1.01 should be valid");
    assert!((hash_table_rehash_size(ht).unwrap() - 1.01).abs() < f64::EPSILON);
}

#[test]
fn empty_table_operations() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 0);
    let (val, present) = gethash(BlissVal::from_fixnum(0), ht, BlissVal::from_fixnum(42)).unwrap();
    assert!(!present);
    assert_eq!(val, BlissVal::from_fixnum(42));
    assert!(!remhash(BlissVal::from_fixnum(0), ht).unwrap());
    assert!(clrhash(ht).is_ok());
    assert!(maphash(T, ht).is_ok());
}

#[test]
fn table_grows_past_initial_capacity() {
    let opts = MakeHashTableOptions {
        size: 4, rehash_threshold: 0.5, ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).unwrap();
    for i in 0..20 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i * 100)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 20);
    for i in 0..20 {
        let (val, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(present, "key {} missing after rehash", i);
        assert_eq!(val, BlissVal::from_fixnum(i * 100));
    }
    assert!(hash_table_size(ht).unwrap() > 4);
}

#[test]
fn insert_remove_reinsert() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let key = BlissVal::from_fixnum(7);
    set_gethash(key, ht, BlissVal::from_fixnum(70)).unwrap();
    remhash(key, ht).unwrap();
    assert!(!gethash(key, ht, NIL).unwrap().1);
    set_gethash(key, ht, BlissVal::from_fixnum(700)).unwrap();
    let (val, present) = gethash(key, ht, NIL).unwrap();
    assert!(present);
    assert_eq!(val, BlissVal::from_fixnum(700));
}

#[test]
fn query_functions_on_non_table_errors() {
    let bad = BlissVal::from_fixnum(0);
    assert!(hash_table_size(bad).is_err());
    assert!(hash_table_rehash_size(bad).is_err());
    assert!(hash_table_rehash_threshold(bad).is_err());
    assert!(hash_table_test(bad).is_err());
}
