//! Spec-derived tests for package bootstrap and registry semantics.
//!
//! These tests target the real `bliss_stdlib::packages` public API and cite
//! the normative requirements they are intended to enforce.
//! Coverage umbrella: R5.01, R5.02, R5.03, R5.04, R5.05, R5.06, R5.07,
//! R5.08, R5.09, R5.51, R5.52, R5.53, R5.54, R5.55, R5.56, R5.57, R5.59,
//! R5.60, R5.61, R5.62, R5.63, R5.64, R5.65.

use std::sync::mpsc;
use std::thread;

use bliss_stdlib::packages::{
    InternStatus, PackageRegistry, export, find_symbol, import, intern, shadowing_import,
    use_package,
};
use bliss_stdlib::{gethash, hash_table_p, make_lisp_string};

fn fresh_registry() -> PackageRegistry {
    let mut registry = PackageRegistry::new();
    registry
        .init_standard_packages()
        .expect("bootstrap package init should succeed");
    registry
}

#[test]
fn bootstrap_initializes_required_packages_nicknames_and_cl_user_inheritance() {
    // Per R5.05, R5.55, and R5.61, bootstrap must create COMMON-LISP,
    // COMMON-LISP-USER, KEYWORD, and BLISS-INTERNAL before loading CL source,
    // and global package lookups must remain direct by name/nickname.
    // Per §5.1.1, COMMON-LISP has nickname CL and COMMON-LISP-USER has
    // nickname CL-USER. Per §5.2.2.1, COMMON-LISP-USER uses CL.
    let registry = fresh_registry();

    let common_lisp = registry
        .find_package("COMMON-LISP")
        .expect("COMMON-LISP must exist after bootstrap");
    assert_eq!(
        registry.find_package("CL"),
        Some(common_lisp),
        "COMMON-LISP must be reachable through nickname CL",
    );

    let cl_user = registry
        .find_package("COMMON-LISP-USER")
        .expect("COMMON-LISP-USER must exist after bootstrap");
    assert_eq!(
        registry.find_package("CL-USER"),
        Some(cl_user),
        "COMMON-LISP-USER must be reachable through nickname CL-USER",
    );

    assert!(
        registry.find_package("KEYWORD").is_some(),
        "KEYWORD must exist after bootstrap",
    );
    assert!(
        registry.find_package("BLISS-INTERNAL").is_some(),
        "BLISS-INTERNAL must exist after bootstrap",
    );

    let (shared, _) = intern("BOOTSTRAP-SHARED", common_lisp).expect("intern in CL");
    export(&[shared], common_lisp).expect("export from CL");
    assert_eq!(
        find_symbol("BOOTSTRAP-SHARED", cl_user)
            .expect("find-symbol in CL-USER")
            .map(|(_, status)| status),
        Some(InternStatus::Inherited),
        "CL-USER must inherit exported symbols from COMMON-LISP",
    );
}

#[test]
fn bootstrap_is_idempotent_and_does_not_duplicate_packages() {
    // Per R5.59, R5.60, and R5.65, the bootstrap substrate must be sufficient
    // to initialize itself repeatedly without re-creating package state.
    let mut registry = fresh_registry();
    let before = registry.list_all_packages();

    registry
        .init_standard_packages()
        .expect("second bootstrap call should be a no-op");

    let after = registry.list_all_packages();
    assert_eq!(
        after.len(),
        before.len(),
        "idempotent bootstrap must not duplicate standard packages",
    );
    assert_eq!(
        registry.find_package("COMMON-LISP"),
        registry.find_package("CL"),
        "bootstrap must preserve nickname lookups across repeated initialization",
    );
}

#[test]
fn package_membership_lives_in_heap_object_hash_table_cells() {
    let mut registry = fresh_registry();
    let package = registry
        .make_package("HEAP-MEMBERSHIP-PACKAGE", &[], &[])
        .expect("package creation");
    assert!(bliss_rt::types::packagep(package));
    assert!(!package.is_meta_handle());

    let (symbol, _) = intern("CELL-SYMBOL", package).expect("intern symbol");
    let (internal, external) = bliss_rt::packages::symbol_tables(package)
        .expect("package object must expose symbol table cells");
    assert!(hash_table_p(internal));
    assert!(hash_table_p(external));
    assert_eq!(
        gethash(
            make_lisp_string("CELL-SYMBOL"),
            internal,
            bliss_rt::value::NIL,
        )
        .expect("lookup package object table"),
        (symbol, true),
    );

    export(&[symbol], package).expect("export symbol");
    assert_eq!(
        gethash(
            make_lisp_string("CELL-SYMBOL"),
            external,
            bliss_rt::value::NIL,
        )
        .expect("lookup exported package object table"),
        (symbol, true),
    );
}

#[test]
fn package_creation_lookup_and_deletion_work_through_names_and_nicknames() {
    // Per R5.51 and R5.54, the registry must support coherent lookup,
    // creation, and deletion by name and nickname while keeping related
    // indexes in sync for later concurrent readers.
    let mut registry = fresh_registry();

    let created = registry
        .make_package("SPEC-PKG", &["SP"], &[])
        .expect("package creation");
    assert_eq!(registry.find_package("SPEC-PKG"), Some(created));
    assert_eq!(registry.find_package("SP"), Some(created));

    registry
        .delete_package("SP")
        .expect("deletion by nickname should resolve to the package");
    assert!(registry.find_package("SPEC-PKG").is_none());
    assert!(registry.find_package("SP").is_none());
}

#[test]
fn use_package_exposes_only_directly_used_external_symbols() {
    // Per R5.52 and R5.56, FIND-SYMBOL and package visibility must follow ANSI
    // package semantics, and inherited iteration/lookup snapshots must remain
    // coherent while use-lists change.
    let mut registry = fresh_registry();

    let provider = registry.make_package("PROVIDER", &[], &[]).unwrap();
    let (visible, _) = intern("VISIBLE", provider).unwrap();
    export(&[visible], provider).unwrap();

    let middle = registry
        .make_package("MIDDLE", &[], &["PROVIDER"])
        .expect("middle package");
    let leaf = registry
        .make_package("LEAF", &[], &["MIDDLE"])
        .expect("leaf package");

    assert_eq!(
        find_symbol("VISIBLE", middle)
            .unwrap()
            .map(|(_, status)| status),
        Some(InternStatus::Inherited),
        "a package must inherit exported symbols from packages on its direct use-list",
    );
    assert_eq!(
        find_symbol("VISIBLE", leaf).unwrap(),
        None,
        "use-package inheritance must not become transitively visible through another package",
    );
}

#[test]
fn import_conflict_leaves_existing_binding_unchanged() {
    // Per R5.52 and R5.54, IMPORT must preserve coherent symbol-table
    // semantics when a conflicting present symbol already exists.
    let mut registry = fresh_registry();

    let source = registry.make_package("SOURCE", &[], &[]).unwrap();
    let target = registry.make_package("TARGET", &[], &[]).unwrap();

    let (foreign_symbol, _) = intern("CLASH", source).unwrap();
    export(&[foreign_symbol], source).unwrap();

    let (original_symbol, _) = intern("CLASH", target).unwrap();
    let err = import(&[foreign_symbol], target).expect_err("conflicting import must fail");
    let err_text = format!("{err:?}");
    assert!(
        err_text.contains("conflict") || err_text.contains("Conflict"),
        "import failure should report a name conflict: {err_text}",
    );

    let found = find_symbol("CLASH", target)
        .unwrap()
        .expect("conflicting name must still resolve in target");
    assert_eq!(
        found,
        (original_symbol, InternStatus::Internal),
        "failed import must not replace or delete the target package's original binding",
    );
}

#[test]
fn shadowing_import_overrides_inherited_symbol_with_local_binding() {
    // Per R5.52 and R5.54, SHADOWING-IMPORT must allow a package to resolve a
    // symbol conflict by installing its own present symbol.
    let mut registry = fresh_registry();

    let provider = registry.make_package("PROVIDER-A", &[], &[]).unwrap();
    let consumer = registry.make_package("CONSUMER-A", &[], &[]).unwrap();
    let alternative = registry.make_package("PROVIDER-B", &[], &[]).unwrap();

    let (inherited, _) = intern("CONFLICT", provider).unwrap();
    export(&[inherited], provider).unwrap();
    use_package(&[provider], consumer).unwrap();

    let (replacement, _) = intern("CONFLICT", alternative).unwrap();
    shadowing_import(&[replacement], consumer).unwrap();

    let found = find_symbol("CONFLICT", consumer)
        .unwrap()
        .expect("shadowing import should install a local binding");
    assert_eq!(
        found,
        (replacement, InternStatus::Internal),
        "shadowing-import must override inherited visibility with a present symbol",
    );
    assert_ne!(
        found.0, inherited,
        "consumer must no longer resolve the inherited symbol after shadowing-import",
    );
}

#[test]
fn shared_registry_supports_concurrent_bootstrap_lookup_and_mutation() {
    // Per R5.53, R5.54, and R5.56, package operations must be safe under
    // concurrent access from multiple OS threads, and operations that span
    // multiple packages must not deadlock while readers observe coherent data.
    const THREADS: usize = 6;
    const SYMBOLS_PER_THREAD: usize = 24;

    let root = fresh_registry();
    let shared = root.clone();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();

    for thread_index in 0..THREADS {
        let tx = tx.clone();
        let mut registry = shared.clone();
        handles.push(thread::spawn(move || {
            let _active = registry.activate();
            registry
                .init_standard_packages()
                .expect("shared bootstrap must remain idempotent");

            let package = registry
                .make_package(&format!("THREAD-PKG-{thread_index}"), &[], &[])
                .expect("shared package creation");

            let mut ids = Vec::new();
            for symbol_index in 0..SYMBOLS_PER_THREAD {
                let name = format!("SYM-{symbol_index}");
                let (symbol, status) = intern(&name, package).expect("per-thread intern");
                assert_eq!(status, InternStatus::New);
                export(&[symbol], package).expect("per-thread export");
                // Symbols are heap-registry objects now (bliss-jtc.6), not fixnum
                // handles; use the raw tagged bits as a unique identity for the
                // cross-thread uniqueness check below.
                ids.push(symbol.to_raw());
            }

            let cl_lookup = registry.find_package("CL");
            let cl_user_lookup = registry.find_package("CL-USER");
            let local_lookup = registry.find_package(&format!("THREAD-PKG-{thread_index}"));

            tx.send((thread_index, ids, cl_lookup, cl_user_lookup, local_lookup))
                .expect("send thread result");
        }));
    }
    drop(tx);

    let mut all_symbol_ids = Vec::new();
    for _ in 0..THREADS {
        let (thread_index, ids, cl_lookup, cl_user_lookup, local_lookup) =
            rx.recv().expect("receive thread result");
        assert!(
            cl_lookup.is_some(),
            "COMMON-LISP nickname lookup must work on thread {thread_index}",
        );
        assert!(
            cl_user_lookup.is_some(),
            "COMMON-LISP-USER nickname lookup must work on thread {thread_index}",
        );
        assert!(
            local_lookup.is_some(),
            "shared package must be visible on its creating thread {thread_index}",
        );
        assert_eq!(
            ids.len(),
            SYMBOLS_PER_THREAD,
            "each thread should intern the requested number of symbols",
        );
        all_symbol_ids.extend(ids);
    }

    for handle in handles {
        handle.join().expect("package worker thread must not panic");
    }

    for thread_index in 0..THREADS {
        assert!(
            root.find_package(&format!("THREAD-PKG-{thread_index}")).is_some(),
            "packages created on worker threads must remain visible through the shared registry",
        );
    }

    all_symbol_ids.sort_unstable();
    all_symbol_ids.dedup();
    assert_eq!(
        all_symbol_ids.len(),
        THREADS * SYMBOLS_PER_THREAD,
        "concurrent symbol creation should not duplicate IDs across threads",
    );
}

#[test]
fn package_local_nicknames_shadow_global_names_and_reverse_lookup_tracks_owners() {
    // Per R5.57, package-local nicknames must resolve relative to the owning
    // package, shadow global names, and support reverse-owner queries.
    let mut registry = fresh_registry();

    let owner = registry.make_package("OWNER", &[], &[]).unwrap();
    let actual = registry.make_package("ACTUAL", &[], &[]).unwrap();
    let global = registry.make_package("GLOBAL", &["LOCAL"], &[]).unwrap();

    registry
        .add_package_local_nickname(owner, "LOCAL", actual)
        .expect("install local nickname");

    assert_eq!(
        registry.find_package("LOCAL"),
        Some(global),
        "global lookup must remain unchanged",
    );
    assert_eq!(
        registry.find_package_from(owner, "LOCAL"),
        Some(actual),
        "package-local nickname must shadow the global nickname for its owner",
    );

    let local_nicknames = registry
        .package_local_nicknames(owner)
        .expect("read package-local nicknames");
    assert_eq!(local_nicknames.get("LOCAL"), Some(&actual));

    let owners = registry
        .package_locally_nicknamed_by_list(actual)
        .expect("reverse nickname lookup");
    assert_eq!(owners, vec![owner]);

    assert_eq!(
        registry
            .remove_package_local_nickname(owner, "LOCAL")
            .expect("remove local nickname"),
        Some(actual)
    );
    assert_eq!(
        registry.find_package_from(owner, "LOCAL"),
        Some(global),
        "removing the local nickname must restore global lookup",
    );
}
