// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A RESTART-CASE clause can GO to an enclosing TAGBODY tag or RETURN-FROM an
//! enclosing BLOCK, and the function containing it compiles (bliss-38r94).
//! Such functions used to stay tree-walked: the clause was compiled without
//! the enclosing tags and blocks, and once it knew them, the bytecode entry
//! cleared the exits its restart had installed. Expected values are SBCL's.
use std::path::PathBuf;
use std::process::Command;

const DEFINITIONS: &str = r#"
(defun restart-go ()
  (tagbody
     (restart-case (invoke-restart 'finish)
       (finish () (go done)))
     (return-from restart-go :bad)
   done)
  :ok)
(defun restart-return ()
  (block outer
    (restart-case (invoke-restart 'finish)
      (finish () (return-from outer :returned)))
    :bad))
(defun restart-loop (n)
  (let ((k 0))
    (tagbody
     again
       (incf k)
       (restart-case (if (< k n) (invoke-restart 'retry) (go done))
         (retry () (go again)))
     done)
    k))
(defun restart-inner ()
  (restart-case (invoke-restart 'r)
    (r () (block in
            (dolist (x (list 1 2 3))
              (when (= x 2) (return-from in (list :inner x))))))))
"#;

const PROBE: &str = r#"(progn
  (dotimes (i 20) (restart-go) (restart-return) (restart-loop 3) (restart-inner))
  (format t "~&RESULT ~s~%" (list (restart-go) (restart-return) (restart-loop 5) (restart-inner))))"#;

const EXPECTED: &str = "RESULT (:OK :RETURNED 5 (:INNER 2))";

fn run(tier: Option<&str>, program: &str) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
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
fn restart_clauses_transfer_to_enclosing_exits_in_every_tier() {
    let program = format!("(progn {DEFINITIONS} {PROBE})");
    for tier in ["interp", "t0", "t1", "t2"] {
        let stdout = run(Some(tier), &program);
        assert!(stdout.contains(EXPECTED), "{tier}: {stdout}");
    }
}

#[test]
fn functions_with_exiting_restart_clauses_compile() {
    let stdout = run(
        Some("t0"),
        &format!("(progn {DEFINITIONS} (restart-go) (restart-loop 2) (disassemble 'restart-go) (disassemble 'restart-loop))"),
    );
    assert!(stdout.contains("RESTART-GO —"), "{stdout}");
    assert!(stdout.contains("RESTART-LOOP —"), "{stdout}");
    assert!(!stdout.contains("not a compiled"), "{stdout}");
}

#[test]
fn exiting_restart_clauses_work_from_a_fasl() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("restart-clause-exits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("restart-exits.lisp");
    std::fs::write(&source, DEFINITIONS).unwrap();
    run(None, &format!(r#"(compile-file "{}")"#, source.display()));
    let stdout = run(
        None,
        &format!(r#"(progn (load "{}") {PROBE})"#, dir.join("restart-exits.fasl").display()),
    );
    assert!(stdout.contains(EXPECTED), "{stdout}");
}
