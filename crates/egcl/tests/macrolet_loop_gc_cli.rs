// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-macrolet-loop-gc-{}-{serial}.lisp",
        std::process::id()
    ));
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&script)
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .output()
        .unwrap();
    fs::remove_file(script).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("MACROLET-LOOP-GC-OK"), "{stdout}");
    stdout
}

#[test]
fn source_compilation_preserves_local_macros_in_loop_bodies() {
    let program = r#"
(defun simple-direct (n)
 (macrolet ((set-result (form) `(setf result ,form)))
  (let ((result -1))
   (loop for i from 0 below n do (set-result i))
   result)))
(assert (= (simple-direct 5) 4))
(compile 'simple-direct)
(assert (= (simple-direct 5) 4))
(write-line "MACROLET-LOOP-GC-OK")
"#;
    assert_eq!(run(program, false), run(program, true));
}

#[test]
fn recursive_expansion_retains_rewritten_loop_subtrees() {
    let program = r#"
(defparameter *expansion*
  (egcl-ext:macroexpand-all
    '(macrolet ((set-result (form) `(setf result ,form)))
       (let ((result -1))
         (loop for i from 0 below 5 do (set-result i))
         result))))
(assert (= (eval *expansion*) 4))
(write-line "MACROLET-LOOP-GC-OK")
"#;
    assert_eq!(run(program, false), run(program, true));
}
