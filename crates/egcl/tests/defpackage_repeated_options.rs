// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn defpackage_accepts_redundant_names_but_preserves_conflicts() {
    let program = r#"
      (defpackage "REPEAT-SOURCE" (:use) (:nicknames "REPEAT-ALIAS") (:export "ITEM"))
      (defpackage "REPEAT-REEXPORT" (:use) (:import-from "REPEAT-SOURCE" "ITEM") (:export "ITEM"))
      (defpackage "REPEAT-OTHER" (:use) (:export "ITEM"))
      (defpackage "REPEAT-INTERN" (:use) (:intern "SAME" "SAME") (:intern "SAME"))
      (assert (eq :internal (nth-value 1 (find-symbol "SAME" "REPEAT-INTERN"))))
      (defpackage "REPEAT-SHADOW" (:use) (:shadow "SAME" "SAME") (:shadow "SAME")
        (:export "SAME" "SAME") (:export "SAME"))
      (assert (eq :external (nth-value 1 (find-symbol "SAME" "REPEAT-SHADOW"))))
      (assert (= 1 (length (package-shadowing-symbols "REPEAT-SHADOW"))))
      (defpackage "REPEAT-IMPORT" (:use)
        (:import-from "REPEAT-SOURCE" "ITEM" "ITEM")
        (:import-from "REPEAT-ALIAS" "ITEM")
        (:import-from "REPEAT-REEXPORT" "ITEM"))
      (assert (eq (find-symbol "ITEM" "REPEAT-IMPORT") (find-symbol "ITEM" "REPEAT-SOURCE")))
      (defpackage "REPEAT-SHADOWING" (:use)
        (:shadowing-import-from "REPEAT-SOURCE" "ITEM" "ITEM")
        (:shadowing-import-from "REPEAT-ALIAS" "ITEM")
        (:shadowing-import-from "REPEAT-REEXPORT" "ITEM"))
      (assert (eq (find-symbol "ITEM" "REPEAT-SHADOWING") (find-symbol "ITEM" "REPEAT-SOURCE")))
      (assert (equal (package-shadowing-symbols "REPEAT-SHADOWING")
                     (list (find-symbol "ITEM" "REPEAT-SOURCE"))))
      (let ((options '((:shadow "ITEM") (:intern "ITEM")
                       (:import-from "REPEAT-SOURCE" "ITEM")
                       (:shadowing-import-from "REPEAT-SOURCE" "ITEM"))))
        (dolist (left options)
          (dolist (right options)
            (unless (eq left right)
              (assert (handler-case
                        (progn (eval (list 'defpackage "REPEAT-BAD" '(:use) left right)) nil)
                        (program-error () t)))))))
      (assert (handler-case
                (progn (eval '(defpackage "REPEAT-BAD" (:use) (:intern "ITEM") (:export "ITEM"))) nil)
                (program-error () t)))
      (dolist (kind '(:import-from :shadowing-import-from))
        (assert (handler-case
                  (progn (eval (list 'defpackage "REPEAT-CONFLICT" '(:use)
                                     (list kind "REPEAT-SOURCE" "ITEM")
                                     (list kind "REPEAT-OTHER" "ITEM"))) nil)
                  (package-error () t))))
      (assert (not (find-package "REPEAT-BAD")))
      (assert (not (find-package "REPEAT-CONFLICT")))
      (format t "DEFPACKAGE-REPEATS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("DEFPACKAGE-REPEATS-OK"));
}
