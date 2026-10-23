// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The builtins that change a symbol's value cell reject constants: NIL, T,
//! keywords and DEFCONSTANT names (bliss-6rl1j). MAKUNBOUND made PI unbound
//! and accepted T, NIL and keywords; EGCL-EXT:CAS replaced PI's value; CAS and
//! ATOMIC-INCF on NIL or T panicked in the runtime. Ordinary special
//! variables keep working.
use std::process::Command;

const PROGRAM: &str = r#"(progn
  (defvar *counter* 1)
  (defmacro try (form) `(handler-case (progn ,form :returned) (error () :error)))
  (defun probe ()
    (list (try (makunbound 'pi)) (try (makunbound t)) (try (makunbound nil))
          (try (makunbound :key))
          (try (egcl-ext:cas (symbol-value 'pi) pi 1)) (try (egcl-ext:cas (symbol-value nil) nil 1))
          (try (egcl-ext:cas t t 1))
          (try (egcl-ext:atomic-incf t)) (try (egcl-ext:atomic-decf nil 2))
          (try (egcl-ext:atomic-incf (symbol-value 'pi)))
          (list pi (symbol-value :key) t)))
  (dotimes (i 20) (probe))
  (setq *counter* 1)
  (format t "~&RESULT ~s~%"
          (list (probe)
                (egcl-ext:cas *counter* 1 2) (egcl-ext:atomic-incf *counter*) *counter*
                (makunbound '*counter*) (boundp '*counter*))))"#;

#[test]
fn constants_cannot_be_unbound_or_atomically_updated() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(!stderr.contains("panicked"), "{tier}: {stderr}");
        assert!(
            stdout.contains(
                "RESULT ((:ERROR :ERROR :ERROR :ERROR :ERROR :ERROR :ERROR :ERROR :ERROR :ERROR \
                 (3.141592653589793d0 :KEY T)) 1 2 3 *COUNTER* NIL)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
