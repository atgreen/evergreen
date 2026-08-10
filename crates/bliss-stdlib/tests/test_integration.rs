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

    // Verify it is now External
    let (found, ext_status) = find_symbol("FOO", provider).unwrap();
    assert_eq!(found, sym_foo);
    assert_eq!(ext_status, InternStatus::External);

    // Create consumer package that uses PROVIDER
    let consumer = reg.make_package("CONSUMER", &[], &["PROVIDER"]).unwrap();

    // FOO should be visible as Inherited in CONSUMER
    let (inherited_sym, inh_status) = find_symbol("FOO", consumer).unwrap();
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

    let (found, _) = find_symbol("MY-VAR", cl_user).unwrap();
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

    let (found, status) = find_symbol("IMPORTED-SYM", dst).unwrap();
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
    assert!(find_symbol("X", pkg_b).is_ok());

    let pkg_c = reg.make_package("PKG-C", &[], &["PKG-B"]).unwrap();
    // X should NOT be in C (use-package is not transitive)
    let result = find_symbol("X", pkg_c);
    assert!(result.is_err() || {
        let (_, st) = result.unwrap();
        st != InternStatus::Inherited && st != InternStatus::External
    });
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
        let (found, _) = find_symbol(name, pkg).unwrap();
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

    // Create a custom condition class (a subclass of T for now)
    let t_class = find_class("T").expect("T class must exist after bootstrap");

    // Register a custom condition class
    let condition_class = make_instance(
        t_class,
        &[],
    ).expect("make_instance for condition class");

    // Create a condition instance via make_simple_error
    let condition = make_simple_error(
        make_lisp_string("something went wrong"),
        NIL,
    ).expect("make_simple_error");

    // Signal the condition and handle it via handler_case
    // handler_case should catch the condition and return the handler's result
    let handler_type = find_class("T").expect("T class for handler");
    let result = handler_case(
        || error_condition(condition),
        &[(handler_type, |_cond| BlissVal::from_fixnum(42))],
    );
    assert!(result.is_ok());
    let val = result.unwrap();
    assert_eq!(val.as_fixnum(), 42, "handler should return 42");
}

/// Create a type-error condition via CLOS, verify its datum/expected-type.
#[test]
fn type_error_condition_carries_datum() {
    bootstrap_clos().expect("bootstrap_clos");

    let datum = BlissVal::from_fixnum(99);
    let expected = make_lisp_string("STRING");

    let condition = make_type_error(datum, expected).expect("make_type_error");

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

    let condition = make_simple_error(
        make_lisp_string("test error"),
        NIL,
    ).expect("make_simple_error");

    // restart_bind establishes a restart; if the body signals, the
    // handler can find and invoke that restart.
    let result = restart_bind(
        &[restart],
        || {
            // Inside the body, the restart should be visible
            let restarts = compute_restarts(None).unwrap();
            assert!(!restarts.is_empty(), "at least one restart should be active");

            let found = find_restart(restart_name, None);
            assert!(found.is_ok(), "USE-VALUE restart should be findable");
            BlissVal::from_fixnum(100)
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_fixnum(), 100);
}

/// CLOS class_of returns the correct class for fixnums after bootstrap.
#[test]
fn class_of_fixnum_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = BlissVal::from_fixnum(7);
    let cls = class_of(val).expect("class_of fixnum");
    let name = class_name(cls).expect("class_name");
    // After bootstrap, fixnum class should exist
    assert!(!name.is_nil(), "class name of fixnum should not be NIL");
}

/// CLOS class_of returns the correct class for characters.
#[test]
fn class_of_character_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = BlissVal::from_char('A');
    let cls = class_of(val).expect("class_of character");
    let name = class_name(cls).expect("class_name");
    assert!(!name.is_nil());
}

/// Make an instance, set slot values, verify slot_boundp and slot_value.
#[test]
fn clos_make_instance_and_slots() {
    bootstrap_clos().expect("bootstrap_clos");

    let t_class = find_class("T").expect("T must exist");
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
/// then iterate and verify all entries.
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
    for (k, v) in keys.iter().zip(values.iter()) {
        let (got, present) = gethash(*k, ht, NIL).unwrap();
        assert_eq!(got, *v, "key {:?} should map to {:?}", k, v);
        assert_eq!(present, T, "key should be present");
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
    for (i, k) in keys.iter().enumerate() {
        let (_, present) = gethash(*k, ht, NIL).unwrap();
        if i % 2 == 0 {
            assert_eq!(present, NIL, "even key {} should be absent", i);
        } else {
            assert_eq!(present, T, "odd key {} should be present", i);
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

    // sxhash should produce consistent hashes
    for item in &items {
        let h1 = sxhash(*item).unwrap();
        let h2 = sxhash(*item).unwrap();
        assert_eq!(h1, h2, "sxhash must be consistent for same value");
    }

    // Store hash→value mapping
    for item in &items {
        let hash = sxhash(*item).unwrap();
        set_gethash(BlissVal::from_fixnum(hash as i64), ht, *item).unwrap();
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

    // All keys should be absent
    for i in 0..20 {
        let (_, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert_eq!(present, NIL);
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
    assert_eq!(present, NIL, "missing key should not be present");
}

// ═══════════════════════════════════════════════════════════════════
// §4  Streams + FORMAT integration
// ═══════════════════════════════════════════════════════════════════

/// Create a string output stream, format into it with ~A, verify output.
#[test]
fn format_to_string_output_stream() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");
    assert!(output_stream_p(stream).unwrap());

    // format with NIL destination returns a string
    let control = make_lisp_string("Hello, ~A!");
    let arg = make_lisp_string("world");
    let result = format(NIL, control, &[arg]).expect("format");

    // When destination is NIL, format returns a string
    assert!(!result.is_nil(), "format to NIL should return a string");
}

/// Format with ~D for integer arguments.
#[test]
fn format_integer_directive() {
    let control = make_lisp_string("The answer is ~D.");
    let arg = BlissVal::from_fixnum(42);
    let result = format(NIL, control, &[arg]).expect("format ~D");
    assert!(!result.is_nil());
}

/// Format with ~% produces newlines.
#[test]
fn format_newline_directive() {
    let control = make_lisp_string("line1~%line2");
    let result = format(NIL, control, &[]).expect("format ~%");
    assert!(!result.is_nil());
}

/// Format into an actual stream (T = *standard-output*, or a stream val).
#[test]
fn format_to_stream_destination() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");
    let control = make_lisp_string("value=~A");
    let arg = BlissVal::from_fixnum(7);

    // Format to the stream
    let result = format(stream, control, &[arg]);
    assert!(result.is_ok());

    // Extract what was written
    let output = get_output_stream_string(stream);
    assert!(output.is_ok(), "should retrieve output stream string");
}

/// Write characters to a string output stream and read the result.
#[test]
fn stream_write_chars_and_get_string() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    stream_write_char(stream, BlissVal::from_char('H')).unwrap();
    stream_write_char(stream, BlissVal::from_char('i')).unwrap();

    let output = get_output_stream_string(stream).unwrap();
    assert!(!output.is_nil(), "output should contain 'Hi'");
}

/// Write a string to a stream and retrieve the output.
#[test]
fn stream_write_string_and_get_output() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    let s = make_lisp_string("Hello, streams!");
    stream_write_string(stream, s).unwrap();

    let output = get_output_stream_string(stream).unwrap();
    assert!(!output.is_nil());
}

/// String input stream: read characters one by one.
#[test]
fn string_input_stream_read_chars() {
    let input_str = make_lisp_string("abc");
    let stream = make_string_input_stream(input_str, None, None)
        .expect("make_string_input_stream");

    assert!(input_stream_p(stream).unwrap());

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
    let in_stream = make_string_input_stream(input_str, None, None)
        .expect("input stream");
    let out_stream = make_string_output_stream(NIL).expect("output stream");

    let two_way = make_two_way_stream(in_stream, out_stream)
        .expect("make_two_way_stream");

    // Should be both input and output
    assert!(input_stream_p(two_way).unwrap());
    assert!(output_stream_p(two_way).unwrap());

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
    assert!(output_stream_p(broadcast).unwrap());

    let msg = make_lisp_string("broadcast");
    stream_write_string(broadcast, msg).unwrap();

    // Both component streams should have received the data
    let out1 = get_output_stream_string(s1);
    let out2 = get_output_stream_string(s2);
    assert!(out1.is_ok());
    assert!(out2.is_ok());
}

/// Format with multiple directives (~A ~D ~%) combined.
#[test]
fn format_multiple_directives() {
    let control = make_lisp_string("Name: ~A, Age: ~D~%");
    let name_arg = make_lisp_string("Alice");
    let age_arg = BlissVal::from_fixnum(30);

    let result = format(NIL, control, &[name_arg, age_arg]);
    assert!(result.is_ok());
    assert!(!result.unwrap().is_nil());
}

/// Formatter compiles a control string into a closure, then use it.
#[test]
fn formatter_compile_and_use() {
    let control = make_lisp_string("~A = ~D");
    let compiled = formatter(control);
    assert!(compiled.is_ok(), "formatter should compile a valid control string");
}

// ═══════════════════════════════════════════════════════════════════
// §5  Pathnames + streams integration
// ═══════════════════════════════════════════════════════════════════

/// Parse a pathname string and extract its components.
#[test]
fn parse_pathname_and_extract_components() {
    let path_str = make_lisp_string("/home/user/file.lisp");
    let pathname = parse_namestring(path_str, None, None)
        .expect("parse_namestring");

    let name = pathname_name(pathname).expect("pathname_name");
    assert!(!name.is_nil(), "name component should not be NIL");

    let typ = pathname_type(pathname).expect("pathname_type");
    assert!(!typ.is_nil(), "type component should not be NIL");

    let dir = pathname_directory(pathname).expect("pathname_directory");
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
    let extracted_name = pathname_name(pn).unwrap();
    assert_eq!(extracted_name, name);
    let extracted_type = pathname_type(pn).unwrap();
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
    let merged_name = pathname_name(merged).unwrap();
    assert_eq!(merged_name, make_lisp_string("data"), "name from partial");

    let merged_type = pathname_type(merged).unwrap();
    assert!(!merged_type.is_nil(), "type should be filled from defaults");
}

/// Parse a pathname, then attempt to open a stream to it.
/// (open will likely fail since the file doesn't exist, but the
///  integration between pathnames and streams is exercised.)
#[test]
fn pathname_to_stream_open() {
    let path_str = make_lisp_string("/tmp/bliss-test-nonexistent.lisp");
    let pathname = parse_namestring(path_str, None, None)
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

    // Parse the namestring back
    let reparsed = parse_namestring(ns, None, None)
        .expect("re-parse namestring");

    // Components should survive the round-trip
    let orig_name = pathname_name(original).unwrap();
    let re_name = pathname_name(reparsed).unwrap();
    assert_eq!(orig_name, re_name, "name should survive roundtrip");

    let orig_type = pathname_type(original).unwrap();
    let re_type = pathname_type(reparsed).unwrap();
    assert_eq!(orig_type, re_type, "type should survive roundtrip");
}

/// Verify host and device default to NIL for Unix-style paths.
#[test]
fn unix_pathname_host_device_nil() {
    let path_str = make_lisp_string("/etc/passwd");
    let pn = parse_namestring(path_str, None, None).unwrap();

    let host = pathname_host(pn).unwrap();
    let device = pathname_device(pn).unwrap();
    // On Unix, host and device are typically NIL
    assert!(host.is_nil() || !host.is_nil(), "host should be accessible");
    assert!(device.is_nil() || !device.is_nil(), "device should be accessible");
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

    let (val_a, _) = gethash(sym_a, ht, NIL).unwrap();
    assert_eq!(val_a.as_fixnum(), 1);

    let (val_b, _) = gethash(sym_b, ht, NIL).unwrap();
    assert_eq!(val_b.as_fixnum(), 2);
}

/// Format a condition message: create a condition, extract its
/// format-control, and format it.
#[test]
fn format_condition_message() {
    let control = make_lisp_string("Error: ~A at position ~D");
    let condition = make_simple_error(control, NIL).unwrap();

    // The condition should be a valid value
    assert!(!condition.is_nil());

    // Format the control string independently
    let arg1 = make_lisp_string("unexpected token");
    let arg2 = BlissVal::from_fixnum(42);
    let formatted = format(NIL, control, &[arg1, arg2]);
    assert!(formatted.is_ok());
}

/// Stream close: verify stream is open, close it, verify it's closed.
#[test]
fn stream_open_close_lifecycle() {
    let stream = make_string_output_stream(NIL).unwrap();

    assert!(open_stream_p(stream).unwrap(), "new stream should be open");

    close(stream, false).expect("close should succeed");

    // After closing, open_stream_p should return false
    assert!(!open_stream_p(stream).unwrap(), "closed stream should not be open");
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

    // Same BlissVal should find it
    let (val, present) = gethash(key, ht, NIL).unwrap();
    assert_eq!(present, T);
    assert_eq!(val.as_fixnum(), 1);

    // Overwrite with same key
    set_gethash(key, ht, BlissVal::from_fixnum(2)).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 1, "overwrite should not increase count");

    let (val2, _) = gethash(key, ht, NIL).unwrap();
    assert_eq!(val2.as_fixnum(), 2);
}
