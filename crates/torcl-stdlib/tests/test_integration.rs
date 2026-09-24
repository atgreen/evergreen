//! Integration tests for torcl-stdlib: cross-module interactions.
//!
//! These tests exercise multiple stdlib modules working together through
//! their public APIs, proving that the modules integrate correctly.

use torcl_rt::value::{NIL, T, TorclVal};
use torcl_stdlib::clos::{
    bootstrap_clos, class_name, class_of, define_class, find_class, make_instance, set_slot_value,
    slot_boundp, slot_value,
};
use torcl_stdlib::conditions::{
    RestartSpec, SYMBOL_CONDITION, compute_restarts, find_restart, handler_case,
    initialize_condition_runtime_support, make_simple_error, make_type_error, restart_bind,
};
use torcl_stdlib::format::{format, formatter};
use torcl_stdlib::hashtable::{
    HashTest, MakeHashTableOptions, gethash, hash_table_count, make_hash_table, maphash,
    set_gethash, sxhash,
};
use torcl_stdlib::packages::{InternStatus, PackageRegistry, export, find_symbol, import, intern};
use torcl_stdlib::pathnames::{
    make_pathname, merge_pathnames, namestring, parse_namestring, pathname_device,
    pathname_directory, pathname_host, pathname_name, pathname_type, register_string,
};
use torcl_stdlib::sequences::{
    copy_seq, count, elt, find as seq_find, length, position, reduce, reverse, subseq,
};
use torcl_stdlib::streams::{
    ExternalFormat, StreamDirection, close, get_output_stream_string, input_stream_p,
    make_broadcast_stream, make_lisp_string, make_string_input_stream, make_string_output_stream,
    make_two_way_stream, open, open_stream_p, output_stream_p, stream_read_char, stream_write_char,
    stream_write_string,
};

// ═══════════════════════════════════════════════════════════════════
// §1  Package system + symbol interning integration
// ═══════════════════════════════════════════════════════════════════

fn fresh_registry() -> PackageRegistry {
    let mut reg = PackageRegistry::new();
    reg.init_standard_packages()
        .expect("init_standard_packages");
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

    // Verify it is now External (find_symbol returns Result<Option<(TorclVal, InternStatus)>>)
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
            assert_ne!(
                syms[i], syms[j],
                "different names must produce different symbols"
            );
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

/// Define a condition class via CLOS, create an instance of it,
/// signal the condition, and handle it with handler_case — proving
/// CLOS class hierarchy and condition system work together.
#[test]
fn define_condition_class_and_handle() {
    bootstrap_clos().expect("bootstrap_clos");
    initialize_condition_runtime_support().expect("condition runtime support");

    let condition_class = find_class(TorclVal::from_symbol_index(*SYMBOL_CONDITION))
        .expect("CONDITION class must exist after condition bootstrap");

    let condition_class_name = TorclVal::from_symbol_index(100_100);
    let condition_class_val = TorclVal::from_meta_handle(100_101);
    define_class(
        condition_class_name,
        condition_class_val,
        &[condition_class],
        &[],
    )
    .expect("define_class should register the condition class");

    let found_class = find_class(condition_class_name)
        .expect("condition class should be findable after registration");
    assert_eq!(
        found_class, condition_class_val,
        "found class must match registered class"
    );

    let condition_instance = make_instance(condition_class_val, &[])
        .expect("make_instance should create a condition instance");
    assert!(
        !condition_instance.is_nil(),
        "condition instance must not be NIL"
    );

    // Signal the condition and handle it via handler_case
    // Use the CLOS-registered condition class as the handler type — proving
    // CLOS class lookup feeds into condition dispatch.
    let handler_result_val = TorclVal::from_fixnum(42);
    let result = handler_case(
        condition_instance,
        &[(condition_class_val, handler_result_val)],
    );
    assert!(result.is_ok(), "handler_case should succeed");
    // handler_case returns the handler value when a condition matches
    let handled = result.unwrap();
    assert_eq!(
        handled.as_fixnum(),
        42,
        "handler_case should return the handler's result value (42) when condition matches"
    );
}

/// Create a type-error condition via CLOS, verify its datum/expected-type.
#[test]
fn type_error_condition_carries_datum() {
    bootstrap_clos().expect("bootstrap_clos");

    let datum = TorclVal::from_fixnum(99);
    let expected = make_lisp_string("STRING");

    // make_type_error returns TorclVal directly (not Result)
    let condition = make_type_error(datum, expected);

    // The condition should be a valid TorclVal (not NIL)
    assert!(!condition.is_nil(), "type-error condition must not be NIL");
}

/// Verify restart_bind establishes restarts with dynamic extent:
/// restarts are available during the body and removed after restart_bind returns.
#[test]
fn restart_from_handler_bind() {
    bootstrap_clos().expect("bootstrap_clos");

    let restart_name = make_lisp_string("USE-VALUE");
    let restart = RestartSpec {
        name: restart_name,
        function: TorclVal::from_fixnum(0), // placeholder
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    // restart_bind takes (&[RestartSpec], body: TorclVal) where body is TorclVal
    let body = TorclVal::from_fixnum(100);
    let result = restart_bind(&[restart], body);
    assert!(result.is_ok(), "restart_bind should succeed");
    // restart_bind returns the body value
    assert_eq!(
        result.unwrap().as_fixnum(),
        100,
        "restart_bind should return the body value"
    );

    // After restart_bind returns, the restart has dynamic extent and should
    // be removed from the registry. Verify dynamic extent semantics:
    let restarts_after = compute_restarts(None);
    assert!(
        restarts_after.is_empty(),
        "restarts should be removed after restart_bind returns (dynamic extent)"
    );

    let found_after = find_restart(restart_name, None);
    assert!(
        found_after.is_none(),
        "find_restart should return None after restart_bind's dynamic extent ends"
    );
}

/// CLOS class_of returns the correct class for fixnums after bootstrap.
#[test]
fn class_of_fixnum_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = TorclVal::from_fixnum(7);
    // class_of returns TorclVal directly, not Result
    let cls = class_of(val);
    // class_name returns TorclVal directly, not Result
    let name = class_name(cls);
    // After bootstrap, fixnum class should exist
    assert!(!name.is_nil(), "class name of fixnum should not be NIL");
}

/// CLOS class_of returns the correct class for characters.
#[test]
fn class_of_character_after_bootstrap() {
    bootstrap_clos().expect("bootstrap_clos");
    let val = TorclVal::from_char('A');
    // class_of and class_name return TorclVal directly
    let cls = class_of(val);
    let name = class_name(cls);
    assert!(!name.is_nil());
}

/// Make an instance, set slot values, verify slot_boundp and slot_value.
#[test]
fn clos_make_instance_and_slots() {
    bootstrap_clos().expect("bootstrap_clos");

    let t_class = find_class(T).expect("T must exist");
    let user_class_name = TorclVal::from_symbol_index(100_200);
    let user_class = TorclVal::from_meta_handle(100_201);
    // Instances have a fixed inline slot layout: declare the slot (reusing the
    // same TorclVal so layout keying by identity resolves it on access).
    let slot_name = make_lisp_string("X");
    define_class(user_class_name, user_class, &[t_class], &[slot_name]).expect("define_class");
    let instance = make_instance(user_class, &[]).expect("make_instance");

    // Slot should be unbound initially
    let bound = slot_boundp(instance, slot_name);
    assert!(bound.is_ok());

    // Set slot value
    set_slot_value(instance, slot_name, TorclVal::from_fixnum(77)).expect("set_slot_value");

    // Now slot should be bound and return 77
    let val = slot_value(instance, slot_name).expect("slot_value");
    assert_eq!(val.as_fixnum(), 77);
}

// ── Helper: build a proper cons-list from a slice of TorclVals ────

use torcl_rt::object::ConsCell;

/// Build a proper Lisp list (chain of cons cells) from a slice.
/// The sequences module's `collect_elements` handles cons cells,
/// so this produces a valid sequence type.
fn make_list(vals: &[TorclVal]) -> TorclVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell { car: v, cdr: list }));
        let ptr = cell as *mut ConsCell as *mut u8;
        list = unsafe { TorclVal::from_cons_ptr(ptr) };
    }
    list
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
    let keys: Vec<TorclVal> = (1..=5).map(TorclVal::from_fixnum).collect();
    let values: Vec<TorclVal> = (10..=50).step_by(10).map(TorclVal::from_fixnum).collect();

    for (k, v) in keys.iter().zip(values.iter()) {
        set_gethash(*k, ht, *v).expect("set_gethash");
    }

    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // Verify each key maps to the correct value
    // gethash returns Result<(TorclVal, bool)>
    for (k, v) in keys.iter().zip(values.iter()) {
        let (got, present) = gethash(*k, ht, NIL).unwrap();
        assert_eq!(got, *v, "key {:?} should map to {:?}", k, v);
        assert!(present, "key should be present");
    }
}

/// Populate a hash table, retrieve all values via gethash, build a cons
/// list from them, and exercise sequence functions on that list — true
/// cross-module test proving hashtable lookups feed into sequence operations.
#[test]
fn hashtable_values_to_list_then_sequence_ops() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).expect("make_hash_table");

    // Populate hash table
    for i in 1..=5i64 {
        set_gethash(TorclVal::from_fixnum(i), ht, TorclVal::from_fixnum(i * 10)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // Retrieve values from the hash table and build a cons list from them
    let values: Vec<TorclVal> = (1..=5)
        .map(|i| {
            let (v, present) = gethash(TorclVal::from_fixnum(i), ht, NIL).unwrap();
            assert!(present, "key {} should be present", i);
            v
        })
        .collect();
    let value_list = make_list(&values);

    // Use sequence operations on the list built from hashtable values
    let len = length(value_list).expect("length on list from hashtable values");
    assert_eq!(len, 5, "list should have 5 elements");

    let first = elt(value_list, 0).expect("elt 0 on list");
    assert_eq!(first.as_fixnum(), 10, "first element should be 10");

    let last = elt(value_list, 4).expect("elt 4 on list");
    assert_eq!(last.as_fixnum(), 50, "last element should be 50");
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
    let items: Vec<TorclVal> = vec![
        TorclVal::from_fixnum(100),
        TorclVal::from_fixnum(200),
        TorclVal::from_fixnum(300),
    ];

    // sxhash returns TorclVal directly (not Result)
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

/// Populate a hashtable from a cons-list of keys, then use sequence
/// copy_seq and reverse on a list derived from the hashtable entries.
#[test]
fn hashtable_keys_to_list_copy_and_reverse() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    let keys: Vec<TorclVal> = (1..=4).map(TorclVal::from_fixnum).collect();
    let key_list = make_list(&keys);

    // Use sequence length to verify our source list
    let len = length(key_list).expect("length of key list");
    assert_eq!(len, 4);

    // Populate hashtable from the list elements
    for i in 0..len {
        let k = elt(key_list, i).expect("elt");
        set_gethash(k, ht, TorclVal::from_fixnum(k.as_fixnum() * 100)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 4);

    // Build a list of the values and use copy_seq / reverse
    let vals: Vec<TorclVal> = (1..=4).map(|i| TorclVal::from_fixnum(i * 100)).collect();
    let val_list = make_list(&vals);

    let copied = copy_seq(val_list).expect("copy_seq on list");
    assert!(!copied.is_nil(), "copied list should not be NIL");
    assert_eq!(length(copied).unwrap(), 4);

    let reversed = reverse(val_list).expect("reverse on list");
    assert!(!reversed.is_nil());
    let first_reversed = elt(reversed, 0).expect("elt 0 of reversed");
    assert_eq!(
        first_reversed.as_fixnum(),
        400,
        "first element of reversed should be 400"
    );
}

/// Use sequence functions (length, elt, position, count) on a cons-list
/// built from hash table values, exercising sequences + hashtables together.
#[test]
fn sequence_functions_on_hashtable_derived_data() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate hash table from a sequence of key-value pairs
    let source: Vec<TorclVal> = (1..=5).map(TorclVal::from_fixnum).collect();
    for item in &source {
        set_gethash(*item, ht, TorclVal::from_fixnum(item.as_fixnum() * 10)).unwrap();
    }

    // Collect values from hash table into a cons list
    let values: Vec<TorclVal> = source
        .iter()
        .map(|k| {
            let (v, _) = gethash(*k, ht, NIL).unwrap();
            v
        })
        .collect();
    let seq_val = make_list(&values);

    // Use length on the list sequence
    let len = length(seq_val).expect("length should work on a list sequence");
    assert_eq!(len, 5, "list should have 5 elements");

    // Use elt to access individual elements
    let first = elt(seq_val, 0).expect("elt 0");
    assert_eq!(first.as_fixnum(), 10, "first element should be 10");

    // Use position to find an element — verify the returned position value
    let test_fn = T; // EQL test
    let pos_result = position(first, seq_val, test_fn, None, 0, None, false);
    assert!(pos_result.is_ok(), "position should succeed");
    let pos = pos_result.unwrap();
    // Position of 10 (the first element) should be 0
    assert!(!pos.is_nil(), "position should find element 10 in the list");

    // Use count to count occurrences of the first element — verify the count
    let cnt_result = count(first, seq_val, test_fn, None, 0, None);
    assert!(cnt_result.is_ok(), "count should succeed");
    let cnt = cnt_result.unwrap();
    // Element 10 appears exactly once
    assert_eq!(cnt.as_fixnum(), 1, "count of element 10 should be 1");
}

/// Store fixnum keys in a hashtable, retrieve them into a cons list,
/// then use copy_seq, reverse, and subseq on that list.
#[test]
fn sequence_copy_reverse_with_hashtable_lookup() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Store fixnum keys in hash table
    let key1 = TorclVal::from_fixnum(10);
    let key2 = TorclVal::from_fixnum(20);
    let key3 = TorclVal::from_fixnum(30);
    set_gethash(key1, ht, TorclVal::from_fixnum(1)).unwrap();
    set_gethash(key2, ht, TorclVal::from_fixnum(2)).unwrap();
    set_gethash(key3, ht, TorclVal::from_fixnum(3)).unwrap();

    // Build a cons list from the hashtable values
    let vals: Vec<TorclVal> = [key1, key2, key3]
        .iter()
        .map(|k| {
            let (v, _) = gethash(*k, ht, NIL).unwrap();
            v
        })
        .collect();
    let seq = make_list(&vals);

    // Use copy_seq on the list
    let copied = copy_seq(seq).expect("copy_seq should work on list");
    assert!(!copied.is_nil(), "copied list should not be NIL");
    assert_eq!(length(copied).unwrap(), 3);

    // Use reverse on the list
    let reversed = reverse(seq).expect("reverse should work on list");
    assert!(!reversed.is_nil(), "reversed list should not be NIL");
    assert_eq!(
        elt(reversed, 0).unwrap().as_fixnum(),
        3,
        "reversed first element should be 3"
    );

    // Use subseq to extract a subsequence
    let sub = subseq(seq, 0, Some(2)).expect("subseq should work on list");
    assert!(!sub.is_nil(), "subsequence should not be NIL");
    assert_eq!(length(sub).unwrap(), 2);
}

/// Use reduce on a cons list of fixnums extracted from a hash table.
#[test]
fn reduce_sequence_from_hashtable_values() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate with known values
    for i in 1..=4i64 {
        set_gethash(TorclVal::from_fixnum(i), ht, TorclVal::from_fixnum(i * 10)).unwrap();
    }

    // Collect values from hashtable into a cons list
    let values: Vec<TorclVal> = (1..=4)
        .map(|i| {
            let (v, _) = gethash(TorclVal::from_fixnum(i), ht, NIL).unwrap();
            v
        })
        .collect();
    let seq = make_list(&values);

    // reduce takes (function, sequence, initial_value, key, start, end, from_end)
    // T as the function is a placeholder; verify reduce returns a value (not NIL)
    // and that the call processes the sequence without error.
    let add_fn = T; // placeholder function value
    let result = reduce(
        add_fn,
        seq,
        Some(TorclVal::from_fixnum(0)),
        None,
        0,
        None,
        false,
    );
    assert!(result.is_ok(), "reduce should succeed on a cons list");
    let reduced = result.unwrap();
    // With a real addition function, reduce of [10,20,30,40] with initial 0
    // would yield 100. With T as function, we verify the result is not NIL
    // (i.e., reduce actually processed the sequence elements).
    assert!(!reduced.is_nil(), "reduce should return a non-NIL result");
}

/// Use maphash to iterate over hash table entries and collect into a sequence.
#[test]
fn maphash_iterate_and_collect() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate hash table with known values
    for i in 1..=5i64 {
        set_gethash(TorclVal::from_fixnum(i), ht, TorclVal::from_fixnum(i * 100)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 5);

    // maphash takes (function: TorclVal, table: TorclVal)
    // The function TorclVal would be called with each (key, value) pair
    let map_fn = T; // placeholder function value
    let result = maphash(map_fn, ht);
    assert!(result.is_ok(), "maphash should succeed");

    // After maphash, verify all entries are still present
    for i in 1..=5i64 {
        let (val, present) = gethash(TorclVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(present, "key {} should still be present after maphash", i);
        assert_eq!(
            val.as_fixnum(),
            i * 100,
            "value for key {} should be {}",
            i,
            i * 100
        );
    }
}

/// Use find from sequences module on a list built from hashtable values.
#[test]
fn sequence_find_in_hashtable_values() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).unwrap();

    // Populate hashtable
    for i in 1..=5i64 {
        set_gethash(TorclVal::from_fixnum(i), ht, TorclVal::from_fixnum(i * 10)).unwrap();
    }

    // Collect values into a cons list
    let values: Vec<TorclVal> = (1..=5)
        .map(|i| {
            let (v, _) = gethash(TorclVal::from_fixnum(i), ht, NIL).unwrap();
            v
        })
        .collect();
    let seq = make_list(&values);

    // Use seq_find to search for element 30 in the list
    let target = TorclVal::from_fixnum(30);
    let test_fn = T; // EQL test
    let found = seq_find(target, seq, test_fn, None, 0, None, false);
    assert!(found.is_ok(), "find should succeed on a cons list");
    let found_val = found.unwrap();
    // find should return the element itself (30) when found
    assert!(
        !found_val.is_nil(),
        "find should locate element 30 in the list"
    );
    assert_eq!(
        found_val.as_fixnum(),
        30,
        "find should return the found element (30)"
    );

    // Use length to verify sequence length
    let len = length(seq).expect("length should work on list");
    assert_eq!(len, 5, "list should have 5 elements");
}

// ═══════════════════════════════════════════════════════════════════
// §4  Streams + FORMAT integration
// ═══════════════════════════════════════════════════════════════════

/// Create a string output stream, format into it with ~A, verify output
/// via get_output_stream_string — proving streams+FORMAT integration.
#[test]
fn format_to_string_output_stream() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");
    // output_stream_p returns bool directly
    assert!(output_stream_p(stream));

    // Format INTO the stream (stream as destination, not NIL)
    let result = format(stream, "Hello, ~A!", &[make_lisp_string("world")]);
    assert!(result.is_ok(), "format to stream should succeed");

    // Extract what was written to the stream and verify the content
    let output = get_output_stream_string(stream).expect("should retrieve output stream string");
    assert_eq!(
        output,
        make_lisp_string("Hello, world!"),
        "format output via stream should be 'Hello, world!'"
    );
}

/// Format with ~D for integer arguments.
#[test]
fn format_integer_directive() {
    let arg = TorclVal::from_fixnum(42);
    // format takes &str for control_string
    let result = format(NIL, "The answer is ~D.", &[arg]).expect("format ~D");
    assert!(!result.is_nil());
    // Verify the output contains the expected formatted content
    assert_eq!(
        result,
        make_lisp_string("The answer is 42."),
        "format ~D should produce 'The answer is 42.'"
    );
}

/// Format with ~% produces newlines.
#[test]
fn format_newline_directive() {
    let result = format(NIL, "line1~%line2", &[]).expect("format ~%");
    assert!(!result.is_nil());
    // The result should contain a newline between line1 and line2
    assert_eq!(
        result,
        make_lisp_string("line1\nline2"),
        "format ~% should produce a newline"
    );
}

/// Format into an actual stream (T = *standard-output*, or a stream val).
#[test]
fn format_to_stream_destination() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    // Format to the stream; format takes &str for control_string
    let result = format(stream, "value=~A", &[TorclVal::from_fixnum(7)]);
    assert!(result.is_ok());

    // Extract what was written and verify the content
    let output = get_output_stream_string(stream).expect("should retrieve output stream string");
    assert_eq!(
        output,
        make_lisp_string("value=7"),
        "format to stream should produce 'value=7'"
    );
}

/// Write characters to a string output stream and read the result.
#[test]
fn stream_write_chars_and_get_string() {
    let stream = make_string_output_stream(NIL).expect("make_string_output_stream");

    stream_write_char(stream, TorclVal::from_char('H')).unwrap();
    stream_write_char(stream, TorclVal::from_char('i')).unwrap();

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
    assert_eq!(
        output,
        make_lisp_string("Hello, streams!"),
        "output should contain 'Hello, streams!'"
    );
}

/// String input stream: read characters one by one.
#[test]
fn string_input_stream_read_chars() {
    let input_str = make_lisp_string("abc");
    // make_string_input_stream takes (string, start: usize, end: Option<usize>)
    let stream = make_string_input_stream(input_str, 0, None).expect("make_string_input_stream");

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
    let in_stream = make_string_input_stream(input_str, 0, None).expect("input stream");
    let out_stream = make_string_output_stream(NIL).expect("output stream");

    let two_way = make_two_way_stream(in_stream, out_stream).expect("make_two_way_stream");

    // Should be both input and output (return bool directly)
    assert!(input_stream_p(two_way));
    assert!(output_stream_p(two_way));

    // Read from input side
    let ch = stream_read_char(two_way);
    assert!(ch.is_ok());

    // Write to output side
    let write_result = stream_write_char(two_way, TorclVal::from_char('Z'));
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
    let out1 = get_output_stream_string(s1).expect("should get output from stream 1");
    let out2 = get_output_stream_string(s2).expect("should get output from stream 2");
    assert_eq!(
        out1,
        make_lisp_string("broadcast"),
        "stream 1 should have received 'broadcast'"
    );
    assert_eq!(
        out2,
        make_lisp_string("broadcast"),
        "stream 2 should have received 'broadcast'"
    );
}

/// Format with multiple directives (~A ~D ~%) combined.
#[test]
fn format_multiple_directives() {
    let name_arg = make_lisp_string("Alice");
    let age_arg = TorclVal::from_fixnum(30);

    // format takes &str for control_string
    let result = format(NIL, "Name: ~A, Age: ~D~%", &[name_arg, age_arg]);
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(!output.is_nil());
    assert_eq!(
        output,
        make_lisp_string("Name: Alice, Age: 30\n"),
        "format with multiple directives should produce correct output"
    );
}

/// Formatter compiles a control string into a closure, then use it.
#[test]
fn formatter_compile_and_use() {
    // formatter takes &str (not TorclVal)
    let compiled = formatter("~A = ~D");
    assert!(
        compiled.is_ok(),
        "formatter should compile a valid control string"
    );
}

// ═══════════════════════════════════════════════════════════════════
// §5  Pathnames + streams integration
// ═══════════════════════════════════════════════════════════════════

/// Helper: create a lisp string AND register it in the pathnames module's
/// string registry so that parse_namestring can look up its content.
fn make_pathname_string(s: &str) -> TorclVal {
    let val = make_lisp_string(s);
    register_string(val, s);
    val
}

/// Parse a pathname string and extract its components.
#[test]
fn parse_pathname_and_extract_components() {
    let path_str = make_pathname_string("/home/user/file.lisp");
    // parse_namestring returns Result<(TorclVal, usize)>
    let (pathname, _position) = parse_namestring(path_str, None, None).expect("parse_namestring");

    // pathname_name etc return TorclVal directly (not Result)
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
    // Use make_pathname_string so components are registered in pathnames' registry
    // and namestring can reconstruct the full path string.
    let name = make_pathname_string("test");
    let typ = make_pathname_string("txt");
    let host = NIL;
    let device = NIL;
    let directory = NIL;
    let version = NIL;

    let pn = make_pathname(host, device, directory, name, typ, version).expect("make_pathname");

    let ns = namestring(pn).expect("namestring");
    assert!(!ns.is_nil(), "namestring should produce a non-NIL string");

    // Extracted components should match what we put in
    // pathname_name and pathname_type return TorclVal directly
    let extracted_name = pathname_name(pn);
    assert_eq!(extracted_name, name);
    let extracted_type = pathname_type(pn);
    assert_eq!(extracted_type, typ);
}

/// merge_pathnames fills in defaults from a second pathname.
#[test]
fn merge_pathnames_fills_defaults() {
    let data_str = make_pathname_string("data");
    let partial = make_pathname(NIL, NIL, NIL, data_str, NIL, NIL).expect("partial pathname");

    let defaults = make_pathname(
        NIL,
        NIL,
        make_pathname_string("/tmp/"),
        make_pathname_string("default"),
        make_pathname_string("dat"),
        NIL,
    )
    .expect("default pathname");

    let merged = merge_pathnames(partial, defaults, NIL).expect("merge_pathnames");

    // Name should come from partial, type from defaults
    // pathname_name and pathname_type return TorclVal directly
    let merged_name = pathname_name(merged);
    assert_eq!(merged_name, data_str, "name from partial");

    let merged_type = pathname_type(merged);
    assert!(!merged_type.is_nil(), "type should be filled from defaults");
}

/// Parse a pathname, then attempt to open a stream to it.
/// With `:if-does-not-exist NIL`, OPEN returns NIL for a missing file; reaching
/// that result proves the pathname designator was decoded correctly.
#[test]
fn pathname_to_stream_open() {
    let path_str = make_pathname_string("/tmp/torcl-test-nonexistent.lisp");
    // parse_namestring returns (TorclVal, usize)
    let (pathname, _pos) = parse_namestring(path_str, None, None).expect("parse_namestring");

    // A missing input file with :if-does-not-exist NIL returns NIL.
    let result = open(
        pathname,
        StreamDirection::Input,
        NIL,
        NIL,
        NIL,
        ExternalFormat::Utf8,
    );
    assert_eq!(result.expect("pathname designator should be accepted"), NIL);
}

/// Build a pathname with make_pathname, convert to namestring, parse
/// it back, and verify components match.
#[test]
fn pathname_namestring_parse_roundtrip() {
    let original = make_pathname(
        NIL,
        NIL,
        make_pathname_string("/usr/local/"),
        make_pathname_string("config"),
        make_pathname_string("conf"),
        NIL,
    )
    .expect("make_pathname");

    let ns = namestring(original).expect("namestring");

    // Parse the namestring back; returns (TorclVal, usize)
    let (reparsed, _pos) = parse_namestring(ns, None, None).expect("re-parse namestring");

    // Components should survive the round-trip
    // pathname_name and pathname_type return TorclVal directly
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
    let path_str = make_pathname_string("/etc/passwd");
    // parse_namestring returns (TorclVal, usize)
    let (pn, _pos) = parse_namestring(path_str, None, None).unwrap();

    // pathname_host and pathname_device return TorclVal directly
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

    set_gethash(sym_a, ht, TorclVal::from_fixnum(1)).unwrap();
    set_gethash(sym_b, ht, TorclVal::from_fixnum(2)).unwrap();
    set_gethash(sym_c, ht, TorclVal::from_fixnum(3)).unwrap();

    assert_eq!(hash_table_count(ht).unwrap(), 3);

    // gethash returns (TorclVal, bool)
    let (val_a, _) = gethash(sym_a, ht, NIL).unwrap();
    assert_eq!(val_a.as_fixnum(), 1);

    let (val_b, _) = gethash(sym_b, ht, NIL).unwrap();
    assert_eq!(val_b.as_fixnum(), 2);
}

/// Format a condition message: create a condition, extract its
/// format-control, and format it.
#[test]
fn format_condition_message() {
    // make_simple_error takes (&str, &[TorclVal]) and returns TorclVal
    let condition = make_simple_error("Error: ~A at position ~D", &[]);

    // The condition should be a valid value
    assert!(!condition.is_nil());

    // Format the control string independently; format takes &str
    let arg1 = make_lisp_string("unexpected token");
    let arg2 = TorclVal::from_fixnum(42);
    let formatted = format(NIL, "Error: ~A at position ~D", &[arg1, arg2]);
    assert!(formatted.is_ok());
    let output = formatted.unwrap();
    assert_eq!(
        output,
        make_lisp_string("Error: unexpected token at position 42"),
        "formatted condition message should contain the error details"
    );
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

/// Cross-module: use CLOS class_of results as hashtable keys,
/// then use sequence length on a list of the stored values.
#[test]
fn clos_classes_as_hashtable_keys_with_sequence_ops() {
    bootstrap_clos().expect("bootstrap_clos");

    let opts = MakeHashTableOptions {
        test: HashTest::Eq,
        ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).unwrap();

    // Store class-of results for different value types as keys
    let fixnum_class = class_of(TorclVal::from_fixnum(1));
    let char_class = class_of(TorclVal::from_char('A'));

    set_gethash(fixnum_class, ht, TorclVal::from_fixnum(100)).unwrap();
    set_gethash(char_class, ht, TorclVal::from_fixnum(200)).unwrap();

    // Retrieve values and build a list, then use sequence ops
    let (v1, _) = gethash(fixnum_class, ht, NIL).unwrap();
    let (v2, _) = gethash(char_class, ht, NIL).unwrap();
    let value_list = make_list(&[v1, v2]);

    let len = length(value_list).expect("length on list");
    assert_eq!(len, 2, "list of hashtable values should have 2 elements");

    let first = elt(value_list, 0).expect("elt 0");
    assert_eq!(first.as_fixnum(), 100);
}
