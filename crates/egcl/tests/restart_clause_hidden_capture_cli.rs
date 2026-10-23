// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A compiled RESTART-CASE clause reaches an enclosing lexical variable that
//! only a macro expansion names. The clause read and wrote the global value
//! instead, at T0 and from a fasl (bliss-uamr7). The expected values are SBCL
//! 2.6.9's.
use std::path::PathBuf;
use std::process::Command;

const DEFINITIONS: &str = r#"
(defmacro hidden-x () 'x)
(defmacro set-hidden-x (v) (list 'setq 'x v))
(defun restart-read ()
  (let ((x :lexical))
    (restart-case (invoke-restart 'finish)
      (finish () (hidden-x)))))
(defun restart-write ()
  (let ((x :lexical))
    (list (restart-case (invoke-restart 'finish)
            (finish () (set-hidden-x :written) (hidden-x)))
          x
          (symbol-value 'x))))
"#;

const PROBE: &str = r#"(progn
  (setf (symbol-value 'x) :global)
  (dotimes (i 30) (restart-read) (restart-write))
  (setf (symbol-value 'x) :global)
  (format t "~&RESULT ~s~%" (list (restart-read) (restart-write))))"#;

const EXPECTED: &str = "RESULT (:LEXICAL (:WRITTEN :WRITTEN :GLOBAL))";

fn run(tier: Option<&str>, args: &[&str]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init").args(args);
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{tier:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn restart_clauses_reach_macro_hidden_lexicals_in_every_tier() {
    let program = format!("(progn {DEFINITIONS} {PROBE})");
    for tier in ["interp", "t0", "t1", "t2"] {
        let stdout = run(Some(tier), &["--eval", &program]);
        assert!(stdout.contains(EXPECTED), "{tier}: {stdout}");
    }
}

#[test]
fn restart_clauses_reach_macro_hidden_lexicals_from_a_fasl() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("restart-hidden-capture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("restart-hidden.lisp");
    std::fs::write(&source, DEFINITIONS).unwrap();
    let compiled = run(
        None,
        &["--eval", &format!(r#"(compile-file "{}")"#, source.display())],
    );
    assert!(!compiled.contains("caught ERROR"), "{compiled}");
    let fasl = dir.join("restart-hidden.fasl");
    let stdout = run(
        None,
        &["--eval", &format!(r#"(progn (load "{}") {PROBE})"#, fasl.display())],
    );
    assert!(stdout.contains(EXPECTED), "{stdout}");
}
