// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check(program: &str, expected: &str) {
    for interpreted in [false, true] {
        let form = if interpreted {
            format!("(eval '{program})")
        } else {
            program.into()
        };
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &form])
            .output()
            .expect("run package case probe");
        assert!(
            output.status.success(),
            "interpreted={interpreted}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            expected,
            "interpreted={interpreted}"
        );
    }
}

#[test]
fn export_and_reexport_keep_case_distinct_identities() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (upper (intern "LOW" source))
                    (lower (intern "low" source)))
               (export (list upper lower) source)
               (use-package source target)
               (let ((inherited (nth-value 1 (find-symbol "low" target))))
                 (export lower target)
                 (list (eq upper (find-symbol "LOW" source))
                       (eq lower (find-symbol "low" source))
                       inherited
                       (eq upper (find-symbol "LOW" target))
                       (eq lower (find-symbol "low" target))
                       (nth-value 1 (find-symbol "low" target))
                       (eq (symbol-package lower) source))))"#,
        "(T T :INHERITED T T :EXTERNAL T)",
    );
}

#[test]
fn import_keeps_case_distinct_names_and_existing_uppercase_symbols() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (upper (intern "LOW" target))
                    (lower (intern "low" source)))
               (import lower target)
               (list (eq upper (find-symbol "LOW" target))
                     (eq lower (find-symbol "low" target))
                     (nth-value 1 (find-symbol "low" target))
                     (eq (symbol-package lower) source)
                     (handler-case (import (make-symbol "low") target)
                       (package-error () :conflict))))"#,
        "(T T :INTERNAL T :CONFLICT)",
    );
}

#[test]
fn import_homes_an_uninterned_mixed_case_symbol_without_renaming_it() {
    check(
        r#"(let* ((target (make-package "CASE-TARGET" :use nil))
                    (symbol (make-symbol "MiXeD")))
               (import symbol target)
               (list (eq symbol (find-symbol "MiXeD" target))
                     (null (find-symbol "MIXED" target))
                     (eq (symbol-package symbol) target)
                     (symbol-name symbol)))"#,
        "(T T T \"MiXeD\")",
    );
}

#[test]
fn shadowing_import_replaces_only_the_exact_name() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (upper (intern "LOW" target))
                    (old-lower (intern "low" target))
                    (lower (intern "low" source)))
               (shadowing-import lower target)
               (list (eq upper (find-symbol "LOW" target))
                     (eq lower (find-symbol "low" target))
                     (not (eq old-lower (find-symbol "low" target)))
                     (equal (package-shadowing-symbols target) (list lower))))"#,
        "(T T T T)",
    );
}

#[test]
fn export_does_not_conflict_with_a_differently_cased_name() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (upper (intern "LOW" target))
                    (lower (intern "low" source)))
               (use-package source target)
               (export lower source)
               (list (eq upper (find-symbol "LOW" target))
                     (eq lower (find-symbol "low" target))
                     (nth-value 1 (find-symbol "low" target))))"#,
        "(T T :INHERITED)",
    );
}

#[test]
fn differently_cased_shadow_does_not_hide_an_export_conflict() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (lower (intern "low" source))
                    (other (intern "low" target)))
               (shadow "LOW" target)
               (use-package source target)
               (list (handler-case (export lower source)
                       (package-error () :conflict))
                     (eq other (find-symbol "low" target))
                     (nth-value 1 (find-symbol "low" source))))"#,
        "(:CONFLICT T :INTERNAL)",
    );
}

#[test]
fn exactly_cased_shadow_allows_export_without_changing_the_shadow() {
    check(
        r#"(let* ((source (make-package "CASE-SOURCE" :use nil))
                    (target (make-package "CASE-TARGET" :use nil))
                    (lower (intern "low" source))
                    (other (intern "low" target)))
               (shadow "low" target)
               (use-package source target)
               (export lower source)
               (list (eq other (find-symbol "low" target))
                     (eq lower (find-symbol "low" source))
                     (nth-value 1 (find-symbol "low" source))))"#,
        "(T T :EXTERNAL)",
    );
}

#[test]
fn shadowing_import_accepts_fresh_symbols_with_mixed_case_names() {
    check(
        r#"(let* ((target (make-package "SHADOW-FRESH" :use nil))
                    (symbol (make-symbol "MiXeD")))
               (list (shadowing-import symbol target)
                     (eq symbol (find-symbol "MiXeD" target))
                     (nth-value 1 (find-symbol "MiXeD" target))
                     (equal (package-shadowing-symbols target) (list symbol))
                     (symbol-package symbol)
                     (symbol-name symbol)))"#,
        "(T T :INTERNAL T NIL \"MiXeD\")",
    );
}

#[test]
fn shadowing_import_accepts_a_previously_uninterned_symbol() {
    check(
        r#"(let* ((source (make-package "SHADOW-SOURCE" :use nil))
                    (target (make-package "SHADOW-TARGET" :use nil))
                    (symbol (intern "FORMER" source)))
               (unintern symbol source)
               (list (shadowing-import symbol target)
                     (eq symbol (find-symbol "FORMER" target))
                     (nth-value 1 (find-symbol "FORMER" target))
                     (equal (package-shadowing-symbols target) (list symbol))
                     (symbol-package symbol)))"#,
        "(T T :INTERNAL T NIL)",
    );
}

#[test]
fn find_symbol_preserves_literal_colons_in_imported_names() {
    check(
        r#"(let ((target (make-package "COLON-TARGET" :use nil)))
               (mapcar (lambda (name)
                         (let ((symbol (make-symbol name)))
                           (import symbol target)
                           (list (eq symbol (find-symbol name target))
                                 (nth-value 1 (find-symbol name target))
                                 (null (find-symbol (string-upcase name) target)))))
                       '("MiXeD:A::B" ":MiXeD" "KEYWORD:MiXeD" "λ:a")))"#,
        "((T :INTERNAL T) (T :INTERNAL T) (T :INTERNAL T) (T :INTERNAL T))",
    );
}

#[test]
fn shadowing_imported_literal_name_is_findable() {
    check(
        r#"(let* ((target (make-package "COLON-TARGET" :use nil))
                    (symbol (make-symbol "MiXeD:A::B")))
               (shadowing-import symbol target)
               (list (eq symbol (find-symbol "MiXeD:A::B" target))
                     (nth-value 1 (find-symbol "MiXeD:A::B" target))
                     (equal (package-shadowing-symbols target) (list symbol))
                     (symbol-package symbol)))"#,
        "(T :INTERNAL T NIL)",
    );
}

#[test]
fn literal_name_identity_survives_export_inheritance_reading_and_reexport() {
    check(
        r#"(let* ((source (make-package "COLON-SOURCE" :use nil))
                    (target (make-package "COLON-TARGET" :use nil))
                    (symbol (make-symbol "MiXeD:A::B")))
               (import symbol source)
               (export symbol source)
               (use-package source target)
               (list (eq symbol (find-symbol "MiXeD:A::B" target))
                     (nth-value 1 (find-symbol "MiXeD:A::B" target))
                     (eq symbol (read-from-string "COLON-SOURCE:|MiXeD:A::B|"))
                     (let ((*package* target))
                       (eq symbol (read-from-string "|MiXeD:A::B|")))
                     (export symbol target)
                     (nth-value 1 (find-symbol "MiXeD:A::B" target))))"#,
        "(T :INHERITED T T T :EXTERNAL)",
    );
}

#[test]
fn find_symbol_preserves_literal_colons_in_keyword_names() {
    check(
        r#"(let ((symbol (read-from-string ":|MiXeD:A::B|")))
               (list (eq symbol (find-symbol "MiXeD:A::B" :keyword))
                     (nth-value 1 (find-symbol "MiXeD:A::B" :keyword))
                     (null (find-symbol "mixed:a::b" :keyword))))"#,
        "(T :EXTERNAL T)",
    );
}

#[test]
fn unintern_removes_only_the_exact_case_and_identity() {
    check(
        r#"(let* ((target (make-package "UNINTERN-TARGET" :use nil))
                    (lower (intern "low" target))
                    (upper (intern "LOW" target)))
               (list (unintern (make-symbol "LOW") target)
                     (eq upper (find-symbol "LOW" target))
                     (unintern lower target)
                     (null (find-symbol "low" target))
                     (eq upper (find-symbol "LOW" target))
                     (symbol-package lower)))"#,
        "(NIL T T T T NIL)",
    );
}

#[test]
fn unintern_rejects_revealing_distinct_exact_name_exports() {
    check(
        r#"(let* ((first (make-package "UNINTERN-FIRST" :use nil))
                    (second (make-package "UNINTERN-SECOND" :use nil))
                    (target (make-package "UNINTERN-TARGET" :use nil)))
               (export (intern "low" first) first)
               (export (intern "low" second) second)
               (shadow "low" target)
               (use-package (list first second) target)
               (let ((symbol (find-symbol "low" target)))
                 (list (handler-case (unintern symbol target)
                         (package-error () :conflict))
                       (eq symbol (find-symbol "low" target))
                       (equal (package-shadowing-symbols target) (list symbol))
                       (eq target (symbol-package symbol)))))"#,
        "(:CONFLICT T T T)",
    );
}

#[test]
fn unintern_absent_identity_does_not_check_another_symbols_conflicts() {
    check(
        r#"(let* ((first (make-package "UNINTERN-FIRST" :use nil))
                    (second (make-package "UNINTERN-SECOND" :use nil))
                    (target (make-package "UNINTERN-TARGET" :use nil)))
               (export (intern "LOW" first) first)
               (export (intern "LOW" second) second)
               (shadow "LOW" target)
               (use-package (list first second) target)
               (let ((symbol (find-symbol "LOW" target)))
                 (list (unintern (make-symbol "LOW") target)
                       (eq symbol (find-symbol "LOW" target))
                       (equal (package-shadowing-symbols target) (list symbol)))))"#,
        "(NIL T T)",
    );
}

#[test]
fn unintern_allows_multiple_inheritance_of_one_identity() {
    check(
        r#"(let* ((first (make-package "UNINTERN-FIRST" :use nil))
                    (second (make-package "UNINTERN-SECOND" :use nil))
                    (target (make-package "UNINTERN-TARGET" :use nil))
                    (inherited (intern "low" first)))
               (export inherited first)
               (import inherited second)
               (export inherited second)
               (shadow "low" target)
               (use-package (list first second) target)
               (let ((symbol (find-symbol "low" target)))
                 (list (unintern inherited target)
                       (unintern symbol target)
                       (eq inherited (find-symbol "low" target))
                       (nth-value 1 (find-symbol "low" target))
                       (package-shadowing-symbols target)
                       (symbol-package symbol))))"#,
        "(NIL T T :INHERITED NIL NIL)",
    );
}

#[test]
fn unintern_preserves_literal_colons_in_names() {
    check(
        r#"(let* ((target (make-package "UNINTERN-TARGET" :use nil))
                    (symbol (make-symbol "MiXeD:A::B")))
               (shadowing-import symbol target)
               (list (unintern symbol target)
                     (null (find-symbol "MiXeD:A::B" target))
                     (package-shadowing-symbols target)
                     (symbol-name symbol)))"#,
        "(T T NIL \"MiXeD:A::B\")",
    );
}

#[test]
fn unintern_reports_invalid_symbols_and_missing_packages() {
    check(
        r#"(let ((target (make-package "UNINTERN-TARGET" :use nil)))
               (list (handler-case (unintern 42 target)
                       (type-error () :type))
                     (handler-case (unintern (make-symbol "X") "UNINTERN-MISSING")
                       (package-error () :package))))"#,
        "(:TYPE :PACKAGE)",
    );
}
