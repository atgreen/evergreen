// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A program is entitled to its own definition of a name EGCL also uses for an
//! image-control builtin. Reading SAVE-IMAGE bare inside another package handed
//! back CL-USER's symbol, so `(export 'save-image :its-package)` reported the
//! symbol was not accessible there — which is what stopped swank loading, since
//! swank/backend's DEFINTERFACE ends by exporting the name it just defined
//! (bliss-zfwfo).
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

#[test]
fn an_image_builtin_name_read_in_another_package_interns_there() {
    assert_eq!(
        eval(
            r#"(progn (make-package "PROBE/OWN" :use '("COMMON-LISP"))
                      (let ((*package* (find-package "PROBE/OWN")))
                        (eval (read-from-string
                               "(progn (defun save-image (x) x)
                                       (export 'save-image :probe/own)
                                       (format t \"RESULT:~a\"
                                               (package-name (symbol-package 'save-image))))"))))"#
        ),
        "PROBE/OWN"
    );
}

#[test]
fn every_image_control_spelling_is_package_local() {
    // The four legacy spellings share the rule.
    assert_eq!(
        eval(
            r#"(progn (make-package "PROBE/ALL" :use '("COMMON-LISP"))
                      (let ((*package* (find-package "PROBE/ALL")))
                        (eval (read-from-string
                               "(format t \"RESULT:~s\"
                                  (mapcar (lambda (s) (package-name (symbol-package s)))
                                          (list 'save-image 'save-lisp-and-die
                                                'save-image-and-die '%save-core)))"))))"#
        ),
        "(\"PROBE/ALL\" \"PROBE/ALL\" \"PROBE/ALL\" \"PROBE/ALL\")"
    );
}

#[test]
fn cl_user_keeps_the_legacy_identity() {
    // Unchanged by the fix, and the reason it is narrow: read from CL-USER the
    // name still resolves to the legacy identity the evaluator dispatches on, so
    // a script or the REPL reaches the image-control builtins bare. (It is not
    // FBOUNDP: the evaluator dispatches these by name rather than through a
    // function cell.) Verified identical on the pre-change binary.
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s" (list (package-name (symbol-package 'save-image))
                                           (and (fboundp 'save-image) t)))"#
        ),
        "(\"COMMON-LISP\" NIL)"
    );
}
