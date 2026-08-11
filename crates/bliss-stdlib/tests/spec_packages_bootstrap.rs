//! Spec-derived tests for package bootstrap and registry semantics.
//!
//! These tests target the real `bliss_stdlib::packages` public API and cite
//! the normative requirements they are intended to enforce.

use std::sync::mpsc;
use std::thread;

use bliss_stdlib::packages::{
    InternStatus, PackageRegistry, export, find_symbol, import, intern, shadowing_import,
    use_package,
};

fn fresh_registry() -> PackageRegistry {
    let mut registry = PackageRegistry::new();
    registry
        .init_standard_packages()
        .expect("bootstrap package init should succeed");
    registry
}

#[test]
fn bootstrap_initializes_required_packages_nicknames_and_cl_user_inheritance() {
    // Per R5.61, bootstrap must create COMMON-LISP, COMMON-LISP-USER,
    // KEYWORD, and BLISS-INTERNAL before loading CL source.
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
    // Per R5.65, invoking bootstrap on an already-initialized runtime must be a no-op.
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
fn package_creation_lookup_and_deletion_work_through_names_and_nicknames() {
    // Per R5.51, the registry must support atomic lookup, creation, and deletion
    // by name and by nickname.
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
    // Per R5.52, FIND-SYMBOL and package visibility must follow ANSI package semantics.
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
    // Per R5.52, IMPORT must preserve coherent symbol-table semantics when a
    // conflicting present symbol already exists.
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
    // Per R5.52, shadowing-import must allow a package to resolve a symbol
    // conflict by installing its own present symbol.
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
fn concurrent_bootstrap_and_mutation_on_separate_threads_remain_isolated() {
    // Per R5.53 and R10.08, package operations must be safe under concurrent
    // access from multiple OS threads. This test uses the current public API,
    // which provides a thread-local active registry, and verifies that
    // concurrent bootstrap + mutation does not bleed state across threads.
    const THREADS: usize = 6;
    const SYMBOLS_PER_THREAD: usize = 24;

    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();

    for thread_index in 0..THREADS {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let mut registry = PackageRegistry::new();
            registry
                .init_standard_packages()
                .expect("thread-local bootstrap must succeed");

            let package = registry
                .make_package(&format!("THREAD-PKG-{thread_index}"), &[], &[])
                .expect("per-thread package creation");

            let mut ids = Vec::new();
            for symbol_index in 0..SYMBOLS_PER_THREAD {
                let name = format!("SYM-{symbol_index}");
                let (symbol, status) = intern(&name, package).expect("per-thread intern");
                assert_eq!(status, InternStatus::New);
                export(&[symbol], package).expect("per-thread export");
                ids.push(symbol.as_fixnum());
            }

            let cl_lookup = registry.find_package("CL");
            let cl_user_lookup = registry.find_package("CL-USER");
            let local_lookup = registry.find_package(&format!("THREAD-PKG-{thread_index}"));
            let foreign_lookup =
                registry.find_package(&format!("THREAD-PKG-{}", (thread_index + 1) % THREADS));

            tx.send((
                thread_index,
                ids,
                cl_lookup,
                cl_user_lookup,
                local_lookup,
                foreign_lookup,
            ))
            .expect("send thread result");
        }));
    }
    drop(tx);

    let mut all_symbol_ids = Vec::new();
    for _ in 0..THREADS {
        let (thread_index, ids, cl_lookup, cl_user_lookup, local_lookup, foreign_lookup) =
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
            "thread-local package must be visible on its creating thread {thread_index}",
        );
        assert!(
            foreign_lookup.is_none(),
            "thread-local registries must not see packages created on other threads",
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

    all_symbol_ids.sort_unstable();
    all_symbol_ids.dedup();
    assert_eq!(
        all_symbol_ids.len(),
        THREADS * SYMBOLS_PER_THREAD,
        "concurrent symbol creation should not duplicate IDs across threads",
    );
}
