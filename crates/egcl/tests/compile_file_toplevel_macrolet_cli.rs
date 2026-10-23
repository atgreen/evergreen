// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A top-level MACROLET's body forms are top-level forms (CLHS 3.2.3.1), so an
//! EVAL-WHEN (:COMPILE-TOPLEVEL) they expand into runs during COMPILE-FILE. It
//! only ran when the fasl was loaded, and CFFI's `types.lisp` stopped compiling
//! (GitHub #527).
use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn egcl(program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

fn run(program: &str) -> String {
    let output = egcl(program);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

#[test]
fn toplevel_macrolet_body_is_processed_at_compile_time() {
    let dir = scratch("toplevel-macrolet-eval-when");
    let source = dir.join("macrolet-eval-when.lisp");
    std::fs::write(
        &source,
        r#"(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *noticed* '()))
(macrolet ((notice (k) `(eval-when (:compile-toplevel :load-toplevel :execute)
                          (pushnew ,k *noticed*))))
  (notice :direct))
(defmacro notice-globally (k)
  `(eval-when (:compile-toplevel :load-toplevel :execute) (pushnew ,k *noticed*)))
(macrolet ((several (&rest ks) `(progn ,@(loop for k in ks collect `(notice-globally ,k)))))
  (several :via-progn-a :via-progn-b))
(macrolet ()
  (locally (notice-globally :via-locally)))
(eval-when (:compile-toplevel :load-toplevel :execute) (pushnew :plain *noticed*))
(defmacro noticed-at-compile-time () `',(reverse *noticed*))
(defun compile-time-view () (noticed-at-compile-time))
"#,
    )
    .unwrap();
    let compiled = run(&format!(
        r#"(multiple-value-bind (out warnings-p failure-p) (compile-file "{}")
             (format t "~&RESULT ~s ~s ~s~%" (and out t) warnings-p failure-p))"#,
        source.display()
    ));
    assert!(compiled.contains("RESULT T NIL NIL"), "{compiled}");

    let loaded = run(&format!(
        r#"(progn (load "{}")
             (format t "~&COMPILE ~s~%LOAD ~s~%" (compile-time-view) (reverse *noticed*)))"#,
        dir.join("macrolet-eval-when.fasl").display()
    ));
    let expected = "(:DIRECT :VIA-PROGN-A :VIA-PROGN-B :VIA-LOCALLY :PLAIN)";
    assert!(loaded.contains(&format!("COMPILE {expected}")), "{loaded}");
    assert!(loaded.contains(&format!("LOAD {expected}")), "{loaded}");
}
