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
