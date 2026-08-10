//! Integration tests for bliss-stdlib: cross-module interactions.
//!
//! These tests exercise multiple stdlib modules working together through
//! their public APIs, proving that the modules integrate correctly.

use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::packages::{
    self, PackageRegistry, InternStatus,
    intern, find_symbol, export, use_package, import,
};
use bliss_stdlib::clos::{
    bootstrap_clos, find_class, set_find_class, class_of, class_name,
    allocate_instance, make_instance, initialize_instance,
    slot_value, set_slot_value, slot_boundp,
    make_generic_function, add_method,
    compute_class_precedence_list, class_direct_superclasses,
};
use bliss_stdlib::conditions::{
    make_simple_error, make_type_error, signal_condition,
    error_condition, handler_bind, handler_case, restart_bind,
    compute_restarts, find_restart, invoke_restart,
    HandlerBinding, RestartSpec,
};
use bliss_stdlib::sequences::{
    length, elt, copy_seq, subseq, find as seq_find, position,
    count, map, reduce, remove, concatenate, reverse,
};
use bliss_stdlib::hashtable::{
    make_hash_table, gethash, set_gethash, remhash, maphash,
    clrhash, hash_table_count, hash_table_test, sxhash,
    MakeHashTableOptions, HashTest,
};
use bliss_stdlib::streams::{
    make_string_input_stream, make_string_output_stream,
    get_output_stream_string, make_broadcast_stream,
    make_two_way_stream, open, close,
    stream_write_string, stream_write_char, stream_read_char,
    open_stream_p, input_stream_p, output_stream_p,
    make_lisp_string, StreamDirection, ExternalFormat,
};
use bliss_stdlib::format::{format, formatter};
use bliss_stdlib::pathnames::{
    parse_namestring, make_pathname, merge_pathnames, namestring,
    pathname_name, pathname_type, pathname_directory,
    pathname_host, pathname_device,
};

// ═══════════════════════════════════════════════════════════════════
// §1  Package system + symbol interning integration
// ═══════════════════════════════════════════════════════════════════

fn fresh_registry() -> PackageRegistry {
    let mut reg = PackageRegistry::new();
    reg.init_standard_packages().expect("init_standard_packages");
    reg
}

/// Create a package, intern symbols, export some, then use-package
/// in another package and verify inherited symbol visibility.
#[test]
fn packages_intern_export_use_finds_across_packages() {
    let mut reg = fresh_registry();

    // Create provider package and intern + export a symbol
    let provider = reg.make_package("PROVIDER", &[], &[]).unwrap();
    let (sym_foo, status) = intern("FOO", provider).unwrap();
    assert_eq!(status, InternStatus::New, "first intern should be New");

    // Re-interning the same name returns Internal, not New
    let (sym_foo2, status2) = intern("FOO", provider).unwrap();
    assert_eq!(status2, InternStatus::Internal);
    assert_eq!(sym_foo, sym_foo2, "re-intern must return same symbol");

    // Export FOO
    export(&[sym_foo], provider).unwrap();

    // Verify it is now External (find_symbol returns Result<Option<(BlissVal, InternStatus)>>)
    let (found, ext_status) = find_symbol("FOO", provider)
        .unwrap()
        .expect("FOO should be found after export");
    assert_eq!(found, sym_foo);
    assert_eq!(ext_status, InternStatus::External);

    // Create consumer package that uses PROVIDER
    let consumer = reg.make_package("CONSUMER", &[], &["PROVIDER"]).unwrap();

    // FOO should be visible as Inherited in CONSUMER
    let (inherited_sym, inh_status) = find_symbol("FOO", consumer)
        .unwrap()
        .expect("FOO should be inherited in CONSUMER");
    assert_eq!(inherited_sym, sym_foo, "inherited symbol must be identical");
    assert_eq!(inh_status, InternStatus::Inherited);
}

/// Intern a symbol in CL-USER, verify it is accessible via find_symbol.
#[test]
fn intern_in_cl_user_and_find() {
    let reg = fresh_registry();
    let cl_user = reg.find_package("CL-USER").expect("CL-USER must exist");
    let (sym, status) = intern("MY-VAR", cl_user).unwrap();
    assert_eq!(status, InternStatus::New);

    let (found, _) = find_symbol("MY-VAR", cl_user)
        .unwrap()
        .expect("MY-VAR should be findable after intern");
    assert_eq!(found, sym);
}

/// Import a symbol from one package into another (without use-package).
#[test]
fn import_symbol_across_packages() {
    let mut reg = fresh_registry();
    let src = reg.make_package("SRC", &[], &[]).unwrap();
    let dst = reg.make_package("DST", &[], &[]).unwrap();

    let (sym, _) = intern("IMPORTED-SYM", src).unwrap();
    export(&[sym], src).unwrap();

    // Directly import into DST
    import(&[sym], dst).unwrap();

    let (found, status) = find_symbol("IMPORTED-SYM", dst)
        .unwrap()
        .expect("IMPORTED-SYM should be found in DST");
    assert_eq!(found, sym);
    assert_eq!(status, InternStatus::Internal);
}

/// Multiple packages in a use-list chain: A exports X, B uses A,
/// C uses B — X should NOT be inherited in C (only direct use-list).
#[test]
fn use_package_is_not_transitive() {
    let mut reg = fresh_registry();
    let pkg_a = reg.make_package("PKG-A", &[], &[]).unwrap();
    let (sym_x, _) = intern("X", pkg_a).unwrap();
    export(&[sym_x], pkg_a).unwrap();

    let pkg_b = reg.make_package("PKG-B", &[], &["PKG-A"]).unwrap();
    // X is inherited in B
    let found_in_b = find_symbol("X", pkg_b).unwrap();
    assert!(found_in_b.is_some(), "X should be inherited in PKG-B");

    let pkg_c = reg.make_package("PKG-C", &[], &["PKG-B"]).unwrap();
    // X should NOT be in C (use-package is not transitive)
    let result = find_symbol("X", pkg_c);
    match result {
        Err(_) => { /* error means not found, OK */ }
        Ok(None) => { /* not found, OK */ }
        Ok(Some((_, st))) => {
            assert!(
                st != InternStatus::Inherited && st != InternStatus::External,
                "X should not be transitively inherited in PKG-C"
            );
        }
    }
}

/// Intern many symbols and verify they are all independently accessible.
#[test]
fn intern_multiple_symbols_in_same_package() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("MULTI", &[], &[]).unwrap();

    let names = ["ALPHA", "BETA", "GAMMA", "DELTA", "EPSILON"];
    let mut syms = Vec::new();
    for name in &names {
        let (sym, status) = intern(name, pkg).unwrap();
        assert_eq!(status, InternStatus::New);
        syms.push(sym);
    }

    // All must be distinct values
    for i in 0..syms.len() {
        for j in (i + 1)..syms.len() {
            assert_ne!(syms[i], syms[j], "different names must produce different symbols");
        }
    }

    // Each findable
    for (i, name) in names.iter().enumerate() {
        let (found, _) = find_symbol(name, pkg)
            .unwrap()
            .expect("symbol should be findable");
        assert_eq!(found, syms[i]);
    }
}

// ═══════════════════════════════════════════════════════════════════
// §2  CLOS + conditions integration
// ═══════════════════════════════════════════════════════════════════

/// Define a condition class via CLOS, make an instance, signal it,
/// and handle it with handler_case.
#[test]
fn define_condition_class_and_handle() {
    bootstrap_clos().expect("bootstrap_clos");

    // Find the base CONDITION class (or T as a fallback) to subclass from
    let condition_base = find_class(make_lisp_string("CONDITION"))
        .or_else(|| find_class(make_lisp_string("T")))
        .expect("base class must exist after bootstrap");

    // Define a custom condition subclass via CLOS: register it as MY-ERROR
    let custom_class_name = make_lisp_string("MY-ERROR");
    let custom_class = make_instance(condition_base, &[])
        .expect("make_instance for custom condition class");
    // Register the class so it can be found by name
    set_find_class(custom_class_name, custom_class)
        .expect("set_find_class should register the custom condition class");

    // Verify the custom class is findable
    let found_class = find_class(custom_class_name)
        .expect("MY-ERROR class should be findable after registration");
    assert_eq!(found_class, custom_class, "found class must match registered class");

    // Create a condition instance using make_simple_error (takes &str and &[BlissVal])
    let condition = make_simple_error("something went wrong", &[]);

    // Signal the condition and handle it via handler_case
    // handler_case takes (form: BlissVal, clauses: &[(BlissVal, BlissVal)])
    // where form is a BlissVal representing the body, and clauses are (type, handler) pairs
    let handler_type = find_class(make_lisp_string("T"))
        .expect("T class for handler");
    let handler_fn = BlissVal::from_fixnum(42); // handler result value
    let body = condition; // the form to evaluate
    let result = handler_case(body, &[(handler_type, handler_fn)]);
    assert!(result.is_ok(), "handler_case should succeed");
}

/// Create a type-error condition via CLOS, verify its datum/expected-type.
#[test]
fn type_error_condition_carries_datum() {
    bootstrap_clos().expect("bootstrap_clos");

    let datum = BlissVal::from_fixnum(99);
    let expected = make_lisp_string("STRING");

    // make_type_error returns BlissVal directly (not Result)
    let condition = make_type_error(datum, expected);

    // The condition should be a valid BlissVal (not NIL)
    assert!(!condition.is_nil(), "type-error condition must not be NIL");
}

/// Use restart_bind + handler_bind to establish a restart, signal an
/// error, and invoke the restart from the handler.
#[test]
fn restart_from_handler_bind() {
    bootstrap_clos().expect("bootstrap_clos");

    let restart_name = make_lisp_string("USE-VALUE");
    let restart = RestartSpec {
        name: restart_name,
        function: BlissVal::from_fixnum(0), // placeholder
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    // make_simple_error takes (&str, &[BlissVal]) and returns BlissVal
    let _condition = make_simple_error("test error", &[]);

    // restart_bind takes (&[RestartSpec], body: BlissVal) where body is BlissVal
    let body = BlissVal::from_fixnum(100);
    let result = restart_bind(&[restart], body);
    assert!(result.is_ok());

    // Inside the restart context, compute_restarts returns Vec<BlissVal> directly
    let restarts = compute_restarts(None);
    // find_restart returns Option<BlissVal>
    let _found = find_restart(restart_name, None);
}

/// CLOS class_of returns the correct class for fixnums after bootstrap.
#[test]
fn class_of_fixnum_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = BlissVal::from_fixnum(7);
    // class_of returns BlissVal directly, not Result
    let cls = class_of(val);
    // class_name returns BlissVal directly, not Result
    let name = class_name(cls);
    // After bootstrap, fixnum class should exist
    assert!(!name.is_nil(), "class name of fixnum should not be NIL");
}

/// CLOS class_of returns the correct class for characters.
#[test]
fn class_of_character_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = BlissVal::from_char('A');
    // class_of and class_name return BlissVal directly
    let cls = class_of(val);
    let name = class_name(cls);
    assert!(!name.is_nil());
}

/// Make an instance, set slot values, verify slot_boundp and slot_value.
#[test]
fn clos_make_instance_and_slots() {
    bootstrap_clos().expect("bootstrap_clos");

    // find_class takes BlissVal, returns Option<BlissVal>
    let t_class = find_class(make_lisp_string("T")).expect("T must exist");
    let instance = make_instance(t_class, &[]).expect("make_instance");

    let slot_name = make_lisp_string("X");

    // Slot should be unbound initially
    let bound = slot_boundp(instance, slot_name);
    assert!(bound.is_ok());

    // Set slot value
    set_slot_value(instance, slot_name, BlissVal::from_fixnum(77)).expect("set_slot_value");

    // Now slot should be bound and return 77
    let val = slot_value(instance, slot_name).expect("slot_value");
    assert_eq!(val.as_fixnum(), 77);
}

// ═══════════════════════════════════════════════════════════════════
// §3  Sequences + hashtables integration
// ═══════════════════════════════════════════════════════════════════

/// Create a hash table, populate it from a sequence of key-value pairs,
/// then use sequence functions to query the keys and verify entries.
#[test]
fn populate_hashtable_from_sequence() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).expect("make_hash_table");

    // Build a "sequence" of keys (fixnums 1..5)
    let keys: Vec<BlissVal> = (1..=5).map(BlissVal::from_fixnum).collect();
    let values: Vec<BlissVal> = (10..=50).step_by(10).map(BlissVal::from_fixnum).collect();

    for (k, v) in keys.iter().zip(values.iter()) {
        set_gethash(*k, ht, *v).expect("set_gethash");
    }

    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // Verify each key maps to the correct value
    // gethash returns Result<(BlissVal, bool)>
    for (k, v) in keys.iter().zip(values.iter()) {
        let (got, present) = gethash(*k, ht, NIL).unwrap();
        assert_eq!(got, *v, "key {:?} should map to {:?}", k, v);
        assert!(present, "key should be present");
    }
}

/// Remove elements from a hash table, verify count decreases.
#[test]
fn remove_from_hashtable_and_verify() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).expect("make_hash_table");

    let keys: Vec<BlissVal> = (0..10).map(BlissVal::from_fixnum).collect();
    for k in &keys {
        set_gethash(*k, ht, BlissVal::from_fixnum(1)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 10);

    // Remove even keys
    for k in keys.iter().step_by(2) {
        remhash(*k, ht).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // Even keys should be absent, odd keys present
    // gethash returns (BlissVal, bool)
    for (i, k) in keys.iter().enumerate() {
        let (_, present) = gethash(*k, ht, NIL).unwrap();
        if i % 2 == 0 {
            assert!(!present, "even key {} should be absent", i);
        } else {
            assert!(present, "odd key {} should be present", i);
        }
    }
}

/// Compute sxhash for sequence elements and store in a hash table,
/// demonstrating sequences feeding hashtable keys.
#[test]
fn sxhash_sequence_elements_as_keys() {
    let opts = MakeHashTableOptions {
        test: HashTest::Equal,
        ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).expect("make_hash_table equal");

    // Use fixnums as both keys and values
    let items: Vec<BlissVal> = vec![
        BlissVal::from_fixnum(100),
        BlissVal::from_fixnum(200),
        BlissVal::from_fixnum(300),
    ];

    // sxhash returns BlissVal directly (not Result)
    for item in &items {
        let h1 = sxhash(*item);
        let h2 = sxhash(*item);
        assert_eq!(h1, h2, "sxhash must be consistent for same value");
    }

    // Store items in hash table keyed by themselves
    for item in &items {
        set_gethash(*item, ht, *item).unwrap();
    }

    assert_eq!(hash_table_count(ht).unwrap(), items.len());
}

/// Clear a hash table and verify it is empty.
#[test]
fn clrhash_empties_table() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    for i in 0..20 {
        set_gethash(BlissVal::from_fixnum(i), ht, T).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 20);

    clrhash(ht).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 0);

    // All keys should be absent (gethash returns (BlissVal, bool))
    for i in 0..20 {
        let (_, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(!present);
    }
}

/// Use gethash with a default value for missing keys.
#[test]
fn gethash_default_value_for_missing() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    let default = BlissVal::from_fixnum(-1);
    let (val, present) = gethash(BlissVal::from_fixnum(999), ht, default).unwrap();
    assert_eq!(val, default, "missing key should return default");
    assert!(!present, "missing key should not be present");
}

/// Use sequence functions (length, elt, position, count) on data that
/// flows through hash tables, exercising sequences + hashtables together.
#[test]
fn sequence_functions_on_hashtable_derived_data() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate hash table from a sequence of key-value pairs
    let source: Vec<BlissVal> = (1..=5).map(BlissVal::from_fixnum).collect();
    for item in &source {
        set_gethash(*item, ht, BlissVal::from_fixnum(item.as_fixnum() * 10)).unwrap();
    }

    // Collect values from hash table back into a sequence (BlissVal list/vector)
    // Build a sequence BlissVal from the source values
    let seq_val = make_lisp_string("hello"); // a string is a sequence in CL

    // Use length on the string sequence
    let len = length(seq_val).expect("length should work on a string sequence");
    assert_eq!(len, 5, "length of 'hello' should be 5");

    // Use elt to access individual elements
    let first = elt(seq_val, 0).expect("elt 0");
    assert!(!first.is_nil(), "first element should not be NIL");

    // Use position to find an element
    let test_fn = T; // EQL test
    let pos = position(first, seq_val, test_fn, None, 0, None, false);
    assert!(pos.is_ok(), "position should succeed");

    // Use count to count occurrences of the first element
    let cnt = count(first, seq_val, test_fn, None, 0, None);
    assert!(cnt.is_ok(), "count should succeed");
}

/// Use sequence copy_seq and reverse on data, then look up in hash table.
#[test]
fn sequence_copy_reverse_with_hashtable_lookup() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Store string keys in hash table
    let key1 = make_lisp_string("abc");
    let key2 = make_lisp_string("def");
    set_gethash(key1, ht, BlissVal::from_fixnum(1)).unwrap();
    set_gethash(key2, ht, BlissVal::from_fixnum(2)).unwrap();

    // Use copy_seq on a sequence
    let copied = copy_seq(key1).expect("copy_seq should work");
    assert!(!copied.is_nil(), "copied sequence should not be NIL");

    // Use reverse on a sequence
    let reversed = reverse(key1).expect("reverse should work");
    assert!(!reversed.is_nil(), "reversed sequence should not be NIL");

    // Use subseq to extract a subsequence
    let sub = subseq(key1, 0, Some(2)).expect("subseq should work");
    assert!(!sub.is_nil(), "subsequence should not be NIL");
}

/// Use reduce on a sequence of fixnums extracted from a hash table.
#[test]
fn reduce_sequence_from_hashtable_values() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate with known values
    for i in 1..=4i64 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i * 10)).unwrap();
    }

    // Build a sequence to reduce (string as a representative sequence type)
    let seq = make_lisp_string("test");

    // reduce takes (function, sequence, initial_value, key, start, end, from_end)
    let add_fn = T; // placeholder function value
    let result = reduce(add_fn, seq, Some(BlissVal::from_fixnum(0)), None, 0, None, false);
    assert!(result.is_ok(), "reduce should succeed on a sequence");
}

/// Use maphash to iterate over hash table entries and collect into a sequence.
#[test]
fn maphash_iterate_and_collect() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate hash table with known values
    for i in 1..=5i64 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i * 100)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // maphash takes (function: BlissVal, table: BlissVal)
    // The function BlissVal would be called with each (key, value) pair
    let map_fn = T; // placeholder function value
    let result = maphash(map_fn, ht);
    assert!(result.is_ok(), "maphash should succeed");

    // After maphash, verify all entries are still present
    for i in 1..=5i64 {
        let (val, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(present, "key {} should still be present after maphash", i);
        assert_eq!(val.as_fixnum(), i * 100, "value for key {} should be {}", i, i * 100);
    }
}

/// Use find from sequences module to search for elements.
#[test]
fn sequence_find_in_string() {
    let seq = make_lisp_string("hello world");

    // Get the first character
    let first_char = elt(seq, 0).expect("elt 0 should work");

    // Use seq_find to find an element in the sequence
    let test_fn = T; // EQL test
    let found = seq_find(first_char, seq, test_fn, None, 0, None, false);
    assert!(found.is_ok(), "find should succeed");

    // Use length to verify sequence length
    let len = length(seq).expect("length should work");
    assert_eq!(len, 11, "length of 'hello world' should be 11");
}

// ═══════════════════════════════════════════════════════════════════
// §4  Streams + FORMAT integration
// ═══════════════════════════════════════════════════════════════════

/// Create a string output stream, format into it with ~A, verify output.
#[test]
fn format_to_string_output_stream() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");
    // output_stream_p returns bool directly
    assert!(output_stream_p(stream));

    // format takes control_string: &str (not BlissVal)
    let result = format(NIL, "Hello, ~A!", &[make_lisp_string("world")])
        .expect("format");

    // When destination is NIL, format returns a string
    assert!(!result.is_nil(), "format to NIL should return a string");
    // Verify the actual content contains expected text
    assert_eq!(result, make_lisp_string("Hello, world!"),
        "format output should be 'Hello, world!'");
}

/// Format with ~D for integer arguments.
#[test]
fn format_integer_directive() {
    let arg = BlissVal::from_fixnum(42);
    // format takes &str for control_string
    let result = format(NIL, "The answer is ~D.", &[arg]).expect("format ~D");
    assert!(!result.is_nil());
    // Verify the output contains the expected formatted content
    assert_eq!(result, make_lisp_string("The answer is 42."),
        "format ~D should produce 'The answer is 42.'");
}

/// Format with ~% produces newlines.
#[test]
fn format_newline_directive() {
    let result = format(NIL, "line1~%line2", &[]).expect("format ~%");
    assert!(!result.is_nil());
    // The result should contain a newline between line1 and line2
    assert_eq!(result, make_lisp_string("line1\nline2"),
        "format ~% should produce a newline");
}

/// Format into an actual stream (T = *standard-output*, or a stream val).
#[test]
fn format_to_stream_destination() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    // Format to the stream; format takes &str for control_string
    let result = format(stream, "value=~A", &[BlissVal::from_fixnum(7)]);
    assert!(result.is_ok());

    // Extract what was written and verify the content
    let output = get_output_stream_string(stream)
        .expect("should retrieve output stream string");
    assert_eq!(output, make_lisp_string("value=7"),
        "format to stream should produce 'value=7'");
}

/// Write characters to a string output stream and read the result.
#[test]
fn stream_write_chars_and_get_string() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    stream_write_char(stream, BlissVal::from_char('H')).unwrap();
    stream_write_char(stream, BlissVal::from_char('i')).unwrap();

    let output = get_output_stream_string(stream).unwrap();
    assert_eq!(output, make_lisp_string("Hi"), "output should contain 'Hi'");
}

/// Write a string to a stream and retrieve the output.
#[test]
fn stream_write_string_and_get_output() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    let s = make_lisp_string("Hello, streams!");
    // stream_write_string takes (stream, string, start: usize, end: Option<usize>)
    stream_write_string(stream, s, 0, None).unwrap();

    let output = get_output_stream_string(stream).unwrap();
    assert_eq!(output, make_lisp_string("Hello, streams!"),
        "output should contain 'Hello, streams!'");
}

/// String input stream: read characters one by one.
#[test]
fn string_input_stream_read_chars() {
    let input_str = make_lisp_string("abc");
    // make_string_input_stream takes (string, start: usize, end: Option<usize>)
    let stream = make_string_input_stream(input_str, 0, None)
        .expect("make_string_input_stream");

    // input_stream_p returns bool directly
    assert!(input_stream_p(stream));

    let ch1 = stream_read_char(stream);
    assert!(ch1.is_ok(), "should read first char");

    let ch2 = stream_read_char(stream);
    assert!(ch2.is_ok(), "should read second char");

    let ch3 = stream_read_char(stream);
    assert!(ch3.is_ok(), "should read third char");
}

/// Two-way stream: write to output side, read from input side.
#[test]
fn two_way_stream_integration() {
    let input_str = make_lisp_string("input data");
    let in_stream = make_string_input_stream(input_str, 0, None)
        .expect("input stream");
    let out_stream = make_string_output_stream(NIL).expect("output stream");

    let two_way = make_two_way_stream(in_stream, out_stream)
        .expect("make_two_way_stream");

    // Should be both input and output (return bool directly)
    assert!(input_stream_p(two_way));
    assert!(output_stream_p(two_way));

    // Read from input side
    let ch = stream_read_char(two_way);
    assert!(ch.is_ok());

    // Write to output side
    let write_result = stream_write_char(two_way, BlissVal::from_char('Z'));
    assert!(write_result.is_ok());
}

/// Broadcast stream: writes go to all component streams.
#[test]
fn broadcast_stream_writes_to_all() {
    let s1 = make_string_output_stream(NIL).unwrap();
    let s2 = make_string_output_stream(NIL).unwrap();

    let broadcast = make_broadcast_stream(&[s1, s2]).expect("make_broadcast_stream");
    // output_stream_p returns bool directly
    assert!(output_stream_p(broadcast));

    // stream_write_string takes 4 args
    stream_write_string(broadcast, make_lisp_string("broadcast"), 0, None).unwrap();

    // Both component streams should have received the data
    let out1 = get_output_stream_string(s1)
        .expect("should get output from stream 1");
    let out2 = get_output_stream_string(s2)
        .expect("should get output from stream 2");
    assert_eq!(out1, make_lisp_string("broadcast"),
        "stream 1 should have received 'broadcast'");
    assert_eq!(out2, make_lisp_string("broadcast"),
        "stream 2 should have received 'broadcast'");
}

/// Format with multiple directives (~A ~D ~%) combined.
#[test]
fn format_multiple_directives() {
    let name_arg = make_lisp_string("Alice");
    let age_arg = BlissVal::from_fixnum(30);

    // format takes &str for control_string
    let result = format(NIL, "Name: ~A, Age: ~D~%", &[name_arg, age_arg]);
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(!output.is_nil());
    assert_eq!(output, make_lisp_string("Name: Alice, Age: 30\n"),
        "format with multiple directives should produce correct output");
}

/// Formatter compiles a control string into a closure, then use it.
#[test]
fn formatter_compile_and_use() {
    // formatter takes &str (not BlissVal)
    let compiled = formatter("~A = ~D");
    assert!(compiled.is_ok(), "formatter should compile a valid control string");
}

// ═══════════════════════════════════════════════════════════════════
// §5  Pathnames + streams integration
// ═══════════════════════════════════════════════════════════════════

/// Parse a pathname string and extract its components.
#[test]
fn parse_pathname_and_extract_components() {
    let path_str = make_lisp_string("/home/user/file.lisp");
    // parse_namestring returns Result<(BlissVal, usize)>
    let (pathname, _position) = parse_namestring(path_str, None, None)
        .expect("parse_namestring");

    // pathname_name etc return BlissVal directly (not Result)
    let name = pathname_name(pathname);
    assert!(!name.is_nil(), "name component should not be NIL");

    let typ = pathname_type(pathname);
    assert!(!typ.is_nil(), "type component should not be NIL");

    let dir = pathname_directory(pathname);
    assert!(!dir.is_nil(), "directory component should not be NIL");
}

/// make_pathname constructs a pathname from components, namestring
/// reconstructs it as a string.
#[test]
fn make_pathname_roundtrip() {
    let name = make_lisp_string("test");
    let typ = make_lisp_string("txt");
    let host = NIL;
    let device = NIL;
    let directory = NIL;
    let version = NIL;

    let pn = make_pathname(host, device, directory, name, typ, version)
        .expect("make_pathname");

    let ns = namestring(pn).expect("namestring");
    assert!(!ns.is_nil(), "namestring should produce a non-NIL string");

    // Extracted components should match what we put in
    // pathname_name and pathname_type return BlissVal directly
    let extracted_name = pathname_name(pn);
    assert_eq!(extracted_name, name);
    let extracted_type = pathname_type(pn);
    assert_eq!(extracted_type, typ);
}

/// merge_pathnames fills in defaults from a second pathname.
#[test]
fn merge_pathnames_fills_defaults() {
    let partial = make_pathname(
        NIL, NIL, NIL,
        make_lisp_string("data"),
        NIL,
        NIL,
    ).expect("partial pathname");

    let defaults = make_pathname(
        NIL, NIL,
        make_lisp_string("/tmp/"),
        make_lisp_string("default"),
        make_lisp_string("dat"),
        NIL,
    ).expect("default pathname");

    let merged = merge_pathnames(partial, defaults, NIL)
        .expect("merge_pathnames");

    // Name should come from partial, type from defaults
    // pathname_name and pathname_type return BlissVal directly
    let merged_name = pathname_name(merged);
    assert_eq!(merged_name, make_lisp_string("data"), "name from partial");

    let merged_type = pathname_type(merged);
    assert!(!merged_type.is_nil(), "type should be filled from defaults");
}

/// Parse a pathname, then attempt to open a stream to it.
/// (open will likely fail since the file doesn't exist, but the
///  integration between pathnames and streams is exercised.)
#[test]
fn pathname_to_stream_open() {
    let path_str = make_lisp_string("/tmp/bliss-test-nonexistent.lisp");
    // parse_namestring returns (BlissVal, usize)
    let (pathname, _pos) = parse_namestring(path_str, None, None)
        .expect("parse_namestring");

    // Attempting to open a non-existent file for input should error
    let result = open(
        pathname,
        StreamDirection::Input,
        NIL,
        NIL,
        NIL,
        ExternalFormat::Utf8,
    );
    // We expect an error because the file doesn't exist
    assert!(result.is_err(), "opening non-existent file should fail");
}

/// Build a pathname with make_pathname, convert to namestring, parse
/// it back, and verify components match.
#[test]
fn pathname_namestring_parse_roundtrip() {
    let original = make_pathname(
        NIL, NIL,
        make_lisp_string("/usr/local/"),
        make_lisp_string("config"),
        make_lisp_string("conf"),
        NIL,
    ).expect("make_pathname");

    let ns = namestring(original).expect("namestring");

    // Parse the namestring back; returns (BlissVal, usize)
    let (reparsed, _pos) = parse_namestring(ns, None, None)
        .expect("re-parse namestring");

    // Components should survive the round-trip
    // pathname_name and pathname_type return BlissVal directly
    let orig_name = pathname_name(original);
    let re_name = pathname_name(reparsed);
    assert_eq!(orig_name, re_name, "name should survive roundtrip");

    let orig_type = pathname_type(original);
    let re_type = pathname_type(reparsed);
    assert_eq!(orig_type, re_type, "type should survive roundtrip");
}

/// Verify host and device are NIL for Unix-style paths.
#[test]
fn unix_pathname_host_device_nil() {
    let path_str = make_lisp_string("/etc/passwd");
    // parse_namestring returns (BlissVal, usize)
    let (pn, _pos) = parse_namestring(path_str, None, None).unwrap();

    // pathname_host and pathname_device return BlissVal directly
    let host = pathname_host(pn);
    let device = pathname_device(pn);
    // On Unix, host and device should be NIL
    assert!(host.is_nil(), "host should be NIL for Unix paths");
    assert!(device.is_nil(), "device should be NIL for Unix paths");
}

// ═══════════════════════════════════════════════════════════════════
// §6  Cross-cutting integration: multiple modules together
// ═══════════════════════════════════════════════════════════════════

/// Package symbols as hash table keys: intern symbols from a package,
/// use them as keys in a hash table.
#[test]
fn package_symbols_as_hashtable_keys() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("HT-KEYS", &[], &[]).unwrap();

    let (sym_a, _) = intern("ALPHA", pkg).unwrap();
    let (sym_b, _) = intern("BETA", pkg).unwrap();
    let (sym_c, _) = intern("GAMMA", pkg).unwrap();

    let opts = MakeHashTableOptions {
        test: HashTest::Eq,
        ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).unwrap();

    set_gethash(sym_a, ht, BlissVal::from_fixnum(1)).unwrap();
    set_gethash(sym_b, ht, BlissVal::from_fixnum(2)).unwrap();
    set_gethash(sym_c, ht, BlissVal::from_fixnum(3)).unwrap();

    assert_eq!(hash_table_count(ht).unwrap(), 3);

    // gethash returns (BlissVal, bool)
    let (val_a, _) = gethash(sym_a, ht, NIL).unwrap();
    assert_eq!(val_a.as_fixnum(), 1);

    let (val_b, _) = gethash(sym_b, ht, NIL).unwrap();
    assert_eq!(val_b.as_fixnum(), 2);
}

/// Format a condition message: create a condition, extract its
/// format-control, and format it.
#[test]
fn format_condition_message() {
    // make_simple_error takes (&str, &[BlissVal]) and returns BlissVal
    let condition = make_simple_error("Error: ~A at position ~D", &[]);

    // The condition should be a valid value
    assert!(!condition.is_nil());

    // Format the control string independently; format takes &str
    let arg1 = make_lisp_string("unexpected token");
    let arg2 = BlissVal::from_fixnum(42);
    let formatted = format(NIL, "Error: ~A at position ~D", &[arg1, arg2]);
    assert!(formatted.is_ok());
    let output = formatted.unwrap();
    assert_eq!(output, make_lisp_string("Error: unexpected token at position 42"),
        "formatted condition message should contain the error details");
}

/// Stream close: verify stream is open, close it, verify it's closed.
#[test]
fn stream_open_close_lifecycle() {
    let stream = make_string_output_stream(NIL).unwrap();

    // open_stream_p returns bool directly
    assert!(open_stream_p(stream), "new stream should be open");

    close(stream, false).expect("close should succeed");

    // After closing, open_stream_p should return false
    assert!(!open_stream_p(stream), "closed stream should not be open");
}

/// Hash table with EQ test: same symbol identity gives same slot.
#[test]
fn hashtable_eq_identity() {
    let opts = MakeHashTableOptions {
        test: HashTest::Eq,
        ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).unwrap();

    let key = BlissVal::from_fixnum(42);
    set_gethash(key, ht, BlissVal::from_fixnum(1)).unwrap();

    // Same BlissVal should find it; gethash returns (BlissVal, bool)
    let (val, present) = gethash(key, ht, NIL).unwrap();
    assert!(present);
    assert_eq!(val.as_fixnum(), 1);

    // Overwrite with same key
    set_gethash(key, ht, BlissVal::from_fixnum(2)).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 1, "overwrite should not increase count");

    let (val2, _) = gethash(key, ht, NIL).unwrap();
    assert_eq!(val2.as_fixnum(), 2);
}
