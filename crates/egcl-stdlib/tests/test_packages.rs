// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for egcl-stdlib packages module (spec §5.1).
use egcl_stdlib::packages::*;

fn fresh_registry() -> PackageRegistry {
    let mut reg = PackageRegistry::new();
    reg.init_standard_packages()
        .expect("init_standard_packages");
    reg
}

#[test]
fn registry_new_creates_empty() {
    assert!(PackageRegistry::new().list_all_packages().is_empty());
}

#[test]
fn init_standard_packages_populates() {
    let reg = fresh_registry();
    assert!(reg.find_package("COMMON-LISP").is_some());
    assert!(reg.find_package("CL-USER").is_some());
    assert!(reg.find_package("KEYWORD").is_some());
}

#[test]
fn find_package_by_nickname() {
    let reg = fresh_registry();
    assert!(reg.find_package("CL").is_some());
    assert_eq!(reg.find_package("COMMON-LISP"), reg.find_package("CL"));
}

#[test]
fn find_package_nonexistent() {
    assert!(fresh_registry().find_package("NO-SUCH").is_none());
}

#[test]
fn make_and_find_package() {
    let mut reg = fresh_registry();
    reg.make_package("MY-PKG", &[], &[]).unwrap();
    assert!(reg.find_package("MY-PKG").is_some());
}

#[test]
fn make_package_with_nickname() {
    let mut reg = fresh_registry();
    reg.make_package("LONG-NAME", &["LN"], &[]).unwrap();
    assert_eq!(reg.find_package("LONG-NAME"), reg.find_package("LN"));
}

#[test]
fn make_package_duplicate_name_errors() {
    let mut reg = fresh_registry();
    reg.make_package("DUP", &[], &[]).unwrap();
    assert!(reg.make_package("DUP", &[], &[]).is_err());
}

#[test]
fn make_package_nickname_conflict_errors() {
    let mut reg = fresh_registry();
    reg.make_package("FIRST", &[], &[]).unwrap();
    assert!(reg.make_package("SECOND", &["FIRST"], &[]).is_err());
}

#[test]
fn delete_package_removes() {
    let mut reg = fresh_registry();
    reg.make_package("DOOMED", &[], &[]).unwrap();
    reg.delete_package("DOOMED").unwrap();
    assert!(reg.find_package("DOOMED").is_none());
}

#[test]
fn delete_nonexistent_errors() {
    assert!(fresh_registry().delete_package("GHOST").is_err());
}

#[test]
fn list_all_packages_grows() {
    let mut reg = fresh_registry();
    let before = reg.list_all_packages().len();
    reg.make_package("EXTRA", &[], &[]).unwrap();
    assert_eq!(reg.list_all_packages().len(), before + 1);
}

#[test]
fn intern_fresh_returns_new() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("IT", &[], &[]).unwrap();
    assert_eq!(intern("FOO", pkg).unwrap().1, InternStatus::New);
}

#[test]
fn intern_existing_returns_internal() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("IT2", &[], &[]).unwrap();
    intern("BAR", pkg).unwrap();
    assert_eq!(intern("BAR", pkg).unwrap().1, InternStatus::Internal);
}

#[test]
fn find_symbol_absent_and_present() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("FS", &[], &[]).unwrap();
    assert!(find_symbol("NOPE", pkg).unwrap().is_none());
    let (sym, _) = intern("HI", pkg).unwrap();
    let (found, status) = find_symbol("HI", pkg).unwrap().unwrap();
    assert_eq!(found, sym);
    assert_eq!(status, InternStatus::Internal);
}

#[test]
fn export_makes_external() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("EXP", &[], &[]).unwrap();
    let (sym, _) = intern("X", pkg).unwrap();
    export(&[sym], pkg).unwrap();
    assert_eq!(
        find_symbol("X", pkg).unwrap().unwrap().1,
        InternStatus::External
    );
}

#[test]
fn unexport_reverts() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("UNEXP", &[], &[]).unwrap();
    let (sym, _) = intern("Y", pkg).unwrap();
    export(&[sym], pkg).unwrap();
    unexport(&[sym], pkg).unwrap();
    assert_eq!(
        find_symbol("Y", pkg).unwrap().unwrap().1,
        InternStatus::Internal
    );
}

#[test]
fn unintern_removes_and_absent_false() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("UNI", &[], &[]).unwrap();
    let (sym, _) = intern("GONE", pkg).unwrap();
    assert!(unintern(sym, pkg).unwrap());
    assert!(find_symbol("GONE", pkg).unwrap().is_none());
    assert!(!unintern(sym, pkg).unwrap());
}

#[test]
fn unintern_conflict_preserves_the_present_symbol_and_shadow() {
    let mut reg = fresh_registry();
    let first = reg.make_package("UNINTERN-FIRST", &[], &[]).unwrap();
    let second = reg.make_package("UNINTERN-SECOND", &[], &[]).unwrap();
    let target = reg.make_package("UNINTERN-TARGET", &[], &[]).unwrap();
    let (a, _) = intern("low", first).unwrap();
    let (b, _) = intern("low", second).unwrap();
    export(&[a], first).unwrap();
    export(&[b], second).unwrap();
    shadow(&["low"], target).unwrap();
    use_package(&[first, second], target).unwrap();
    let symbol = find_symbol("low", target).unwrap().unwrap().0;
    let other = egcl_rt::symbols::make_uninterned("low");
    assert!(!unintern(other, target).unwrap());
    assert!(matches!(
        unintern(symbol, target),
        Err(egcl_rt::error::EgclError::PackageError(_))
    ));
    assert_eq!(find_symbol("low", target).unwrap().unwrap().0, symbol);
    assert_eq!(package_shadowing_symbols(target), vec![symbol]);
}

#[test]
fn unintern_allows_revealing_one_symbol_exported_from_multiple_packages() {
    let mut reg = fresh_registry();
    let first = reg.make_package("UNINTERN-SHARED-FIRST", &[], &[]).unwrap();
    let second = reg
        .make_package("UNINTERN-SHARED-SECOND", &[], &[])
        .unwrap();
    let target = reg
        .make_package("UNINTERN-SHARED-TARGET", &[], &[])
        .unwrap();
    let (symbol, _) = intern("low", first).unwrap();
    export(&[symbol], first).unwrap();
    import(&[symbol], second).unwrap();
    export(&[symbol], second).unwrap();
    shadow(&["low"], target).unwrap();
    use_package(&[first, second], target).unwrap();
    let shadowing = find_symbol("low", target).unwrap().unwrap().0;
    assert!(!unintern(symbol, target).unwrap());
    assert!(unintern(shadowing, target).unwrap());
    assert_eq!(find_symbol("low", target).unwrap().unwrap().0, symbol);
    assert!(package_shadowing_symbols(target).is_empty());
}

#[test]
fn unintern_does_not_count_the_removed_export_as_an_inherited_conflict() {
    let mut reg = fresh_registry();
    let source = reg.make_package("UNINTERN-SELF-SOURCE", &[], &[]).unwrap();
    let target = reg.make_package("UNINTERN-SELF-TARGET", &[], &[]).unwrap();
    let (inherited, _) = intern("low", source).unwrap();
    export(&[inherited], source).unwrap();
    shadow(&["low"], target).unwrap();
    let symbol = find_symbol("low", target).unwrap().unwrap().0;
    export(&[symbol], target).unwrap();
    use_package(&[target, source], target).unwrap();
    assert!(unintern(symbol, target).unwrap());
    assert_eq!(find_symbol("low", target).unwrap().unwrap().0, inherited);
}

#[test]
fn use_package_inherits() {
    let mut reg = fresh_registry();
    let prov = reg.make_package("PROV", &[], &[]).unwrap();
    let (sym, _) = intern("SHARED", prov).unwrap();
    export(&[sym], prov).unwrap();
    let cons = reg.make_package("CONS", &[], &[]).unwrap();
    use_package(&[prov], cons).unwrap();
    assert_eq!(
        find_symbol("SHARED", cons).unwrap().unwrap().1,
        InternStatus::Inherited
    );
}

#[test]
fn unuse_package_removes_inheritance() {
    let mut reg = fresh_registry();
    let prov = reg.make_package("P2", &[], &[]).unwrap();
    let (sym, _) = intern("VIS", prov).unwrap();
    export(&[sym], prov).unwrap();
    let cons = reg.make_package("C2", &[], &[]).unwrap();
    use_package(&[prov], cons).unwrap();
    unuse_package(&[prov], cons).unwrap();
    assert!(find_symbol("VIS", cons).unwrap().is_none());
}

#[test]
fn import_conflict_errors() {
    let mut reg = fresh_registry();
    let a = reg.make_package("A", &[], &[]).unwrap();
    let (sym_a, _) = intern("CLASH", a).unwrap();
    let b = reg.make_package("B", &[], &[]).unwrap();
    intern("CLASH", b).unwrap();
    assert!(import(&[sym_a], b).is_err());
}

#[test]
fn shadowing_import_resolves_conflict() {
    let mut reg = fresh_registry();
    let a = reg.make_package("SA", &[], &[]).unwrap();
    let (sym_a, _) = intern("CLASH", a).unwrap();
    let b = reg.make_package("SB", &[], &[]).unwrap();
    intern("CLASH", b).unwrap();
    shadowing_import(&[sym_a], b).unwrap();
    assert_eq!(find_symbol("CLASH", b).unwrap().unwrap().0, sym_a);
}

#[test]
fn shadowing_import_accepts_an_uninterned_literal_name() {
    let mut reg = fresh_registry();
    let target = reg.make_package("SHADOW-FRESH", &[], &[]).unwrap();
    let symbol = egcl_rt::symbols::make_uninterned("MiXeD:A::B");
    shadowing_import(&[symbol], target).unwrap();
    assert_eq!(
        find_symbol("MiXeD:A::B", target).unwrap().unwrap().0,
        symbol
    );
    assert_eq!(package_shadowing_symbols(target), vec![symbol]);
    assert!(
        egcl_rt::symbols::symbol_package(symbol.as_symbol_index())
            .unwrap()
            .is_nil()
    );
}

#[test]
fn shadowing_import_accepts_a_symbol_removed_from_all_packages() {
    let mut reg = fresh_registry();
    let source = reg.make_package("SHADOW-SOURCE", &[], &[]).unwrap();
    let target = reg.make_package("SHADOW-TARGET", &[], &[]).unwrap();
    let (symbol, _) = intern("MiXeD", source).unwrap();
    assert!(unintern(symbol, source).unwrap());
    assert!(find_symbol("MiXeD", source).unwrap().is_none());
    shadowing_import(&[symbol], target).unwrap();
    assert_eq!(find_symbol("MiXeD", target).unwrap().unwrap().0, symbol);
    assert_eq!(package_shadowing_symbols(target), vec![symbol]);
}

#[test]
fn shadow_creates_if_absent() {
    let mut reg = fresh_registry();
    let pkg = reg.make_package("SH", &[], &[]).unwrap();
    shadow(&["NEW-SYM"], pkg).unwrap();
    assert!(find_symbol("NEW-SYM", pkg).unwrap().is_some());
}

// Issue 9: Test make_package with non-empty use_list to verify that
// the new package inherits exported symbols from the used package.
#[test]
fn make_package_with_use_list_inherits() {
    let mut reg = fresh_registry();
    // Create provider package and export a symbol from it
    let prov = reg.make_package("USE-PROV", &[], &[]).unwrap();
    let (exported_sym, _) = intern("INHERITED-SYM", prov).unwrap();
    export(&[exported_sym], prov).unwrap();

    // Create consumer package with use_list referencing the provider
    let cons = reg.make_package("USE-CONS", &[], &["USE-PROV"]).unwrap();

    // The consumer should inherit the exported symbol from the provider
    let found = find_symbol("INHERITED-SYM", cons).unwrap();
    assert!(
        found.is_some(),
        "package created with use_list should inherit exported symbols"
    );
    let (found_sym, status) = found.unwrap();
    assert_eq!(
        found_sym, exported_sym,
        "inherited symbol should be the same as the exported one"
    );
    assert_eq!(
        status,
        InternStatus::Inherited,
        "symbol from use_list should have Inherited status"
    );
}

#[test]
fn dropping_temporary_registry_restores_previous_active_store() {
    let mut outer = fresh_registry();
    let outer_pkg = outer.make_package("OUTER-PKG", &[], &[]).unwrap();
    let (outer_sym, _) = intern("OUTER-SYM", outer_pkg).unwrap();

    {
        let mut inner = fresh_registry();
        let inner_pkg = inner.make_package("INNER-PKG", &[], &[]).unwrap();
        let (inner_sym, _) = intern("INNER-SYM", inner_pkg).unwrap();
        assert_eq!(
            find_symbol("INNER-SYM", inner_pkg).unwrap(),
            Some((inner_sym, InternStatus::Internal))
        );
    }

    assert_eq!(
        find_symbol("OUTER-SYM", outer_pkg).unwrap(),
        Some((outer_sym, InternStatus::Internal))
    );
}

#[test]
fn activation_guard_restores_previous_active_store() {
    let mut outer = fresh_registry();
    let outer_pkg = outer.make_package("GUARD-OUTER", &[], &[]).unwrap();
    let (outer_sym, _) = intern("VISIBLE-OUTER", outer_pkg).unwrap();

    let mut inner = PackageRegistry::new();
    inner.init_standard_packages().unwrap();
    let inner_pkg = inner.make_package("GUARD-INNER", &[], &[]).unwrap();
    let (inner_sym, _) = intern("VISIBLE-INNER", inner_pkg).unwrap();

    {
        let _guard = outer.activate();
        assert_eq!(
            find_symbol("VISIBLE-OUTER", outer_pkg).unwrap(),
            Some((outer_sym, InternStatus::Internal))
        );
    }

    assert_eq!(
        find_symbol("VISIBLE-INNER", inner_pkg).unwrap(),
        Some((inner_sym, InternStatus::Internal))
    );
}
