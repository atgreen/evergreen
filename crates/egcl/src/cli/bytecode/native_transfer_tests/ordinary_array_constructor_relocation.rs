// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exercise the new collecting adapter with actual nursery relocation. All
//! inputs are built before entry, so vector allocation is the body's first
//! Lisp allocation. CONS then keeps the returned array live across another GC.

use super::*;

thread_local! {
    static ARGUMENTS: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static EXPECTED: Cell<[u64; 3]> = const { Cell::new([0; 3]) };
    static ENTERED: Cell<bool> = const { Cell::new(false) };
    static MINORS_AT_ENTRY: Cell<u64> = const { Cell::new(0) };
}

fn arm_at_ordinary_entry() {
    let pointer = ARGUMENTS.with(Cell::get);
    assert!(!pointer.is_null());
    // Addresses are comparisons only; the actual arguments live in a rooted
    // Vec and the generated activation's authoritative tagged homes.
    let actual = unsafe {
        [
            (*pointer).to_raw(),
            (*pointer.add(1)).to_raw(),
            (*pointer.add(2)).to_raw(),
        ]
    };
    assert_eq!(
        actual,
        EXPECTED.with(Cell::get),
        "inputs must remain young until ordinary entry"
    );
    ENTERED.with(|entered| entered.set(true));
    MINORS_AT_ENTRY.with(|count| count.set(egcl_rt::gc::heap_stats().minor_gc_count));
    egcl_rt::gc::stress_arm_after_init();
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_array_constructor_relocates_prebuilt_arguments_and_live_result() {
    const CHILD: &str = "EGCL_TEST_NATIVE_ARRAY_RELOCATION_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::array_constructor_relocation::ordinary_array_constructor_relocates_prebuilt_arguments_and_live_result";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
            .output()
            .expect("run isolated constructor relocation test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "constructor child failed: {}\n{stdout}\n{stderr}",
            output.status
        );
        assert!(
            stdout.contains("NATIVE-ARRAY-ARGUMENTS-MOVED"),
            "exact relocation fixture did not run:\n{stdout}\n{stderr}"
        );
        assert!(
            stderr.contains("[native-array-constructor] fast count=1"),
            "private allocating adapter was not executed:\n{stderr}"
        );
        assert!(
            !stderr.contains("[native-array-constructor] fallback"),
            "valid UB8 fixture unexpectedly used public fallback:\n{stderr}"
        );
        return;
    }

    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install(
        "ORDINARY-ARRAY-RELOCATION",
        "(dimensions marker descriptor)",
        "((let ((array (make-array dimensions :element-type descriptor))) (cons array marker)))",
        &mut env,
    );
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "fixture must enter the ordinary T2 ABI"
    );

    let unsigned_byte = EgclVal::from_symbol_index(egcl_rt::symbols::intern("UNSIGNED-BYTE"));
    // Finish compilation/registration and evacuate their allocations before
    // creating a small fresh nursery region with no pinned symbol objects.
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(tail = crate::cli::arena_cons(EgclVal::from_fixnum(8), NIL));
    egcl_rt::rooted!(descriptor = crate::cli::arena_cons(unsigned_byte, *tail));
    egcl_rt::rooted!(dimensions = crate::cli::arena_cons(EgclVal::from_fixnum(3), NIL));
    egcl_rt::rooted!(marker = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(args = vec![*dimensions, *marker, *descriptor]);
    let before = [args[0].to_raw(), args[1].to_raw(), args[2].to_raw()];
    ARGUMENTS.with(|pointer| pointer.set(args.as_ptr()));
    EXPECTED.with(|expected| expected.set(before));
    ENTERED.with(|entered| entered.set(false));
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    let deopts = mapped.deopt_count();
    egcl_rt::rooted!(
        result =
            native_transfer_entry::with_ordinary_entry_hook_for_test(arm_at_ordinary_entry, || {
                run_native(&code, symbol, &args, &mut env)
            })
            .unwrap()
    );
    ARGUMENTS.with(|pointer| pointer.set(std::ptr::null()));
    assert!(
        ENTERED.with(Cell::get),
        "stress must be armed at the real ordinary entry"
    );
    assert!(
        egcl_rt::gc::heap_stats().minor_gc_count > MINORS_AT_ENTRY.with(Cell::get),
        "the collecting call must really run a minor collection"
    );
    for (index, label) in [(0, "dimensions"), (1, "marker"), (2, "descriptor")] {
        assert_ne!(
            args[index].to_raw(),
            before[index],
            "{label} must actually relocate"
        );
    }
    assert_eq!(
        mapped.deopt_count(),
        deopts,
        "no interpreter recovery may substitute for native execution"
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
        1,
        "only the final public CONS may enter a named call; MAKE-ARRAY must use its private adapter"
    );
    egcl_rt::rooted!(array = cp(*result).0);
    egcl_rt::rooted!(returned_marker = cp(*result).1);
    assert_eq!(
        *returned_marker, args[1],
        "native homes must reload the moved marker after allocation"
    );
    assert_eq!(cp(*returned_marker).0.as_fixnum(), 71);
    assert_eq!(cp(args[0]).0.as_fixnum(), 3);
    assert_eq!(cp(args[2]).0, unsigned_byte);
    assert_eq!(cp(cp(args[2]).1).0.as_fixnum(), 8);
    assert_eq!(egcl_stdlib::sequences::array_total_size(*array), Some(3));
    for index in 0..3 {
        assert_eq!(
            egcl_stdlib::sequences::aref(*array, index).unwrap(),
            EgclVal::from_fixnum(0)
        );
    }
    println!("NATIVE-ARRAY-ARGUMENTS-MOVED");
}
