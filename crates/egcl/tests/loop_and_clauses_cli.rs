// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LOOP's `for A from ... and B from ...` is compiled rather than left to the
//! tree-walker (bliss-5fn6m). Babel's single-byte encoders and decoders step
//! two counters this way; one such loop made `compile-file` keep Babel's whole
//! table of encoders as source, so every `string-to-octets` ran interpreted.
//! The answers below are SBCL's, including FINALLY's view of a variable
//! stepped in parallel by the iteration that ends the loop.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = "(list (loop for i from 0 below 3 and di from 10 finally (return di)) \
      (loop for i from 0 below 0 and di from 10 finally (return (list i di))) \
      (loop for i fixnum from 2 below 9 by 2 and di fixnum from 5 by 3 collect (list i di)) \
      (loop for i from 0 below 3 and di downfrom 10 to 9 finally (return (list i di))) \
      (loop for i from 0 below 3 and j from 0 and k from 100 do (setf k (+ k j)) \
            finally (return (list i j k))) \
      (loop for x in '(1 2) and y from 0 finally (return (list x y))) \
      (loop for x = 1 then y and y = 2 then x repeat 3 collect (list x y)))";

const SBCL: &str =
    "(13 (0 10) ((2 5) (4 8) (6 11) (8 14)) (2 8) (3 3 106) (2 1) ((1 2) (2 1) (1 2)))";

fn run(env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut command = Command::new(BIN);
    command.args(args);
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().expect("spawn egcl")
}

fn last_line(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn parallel_for_clauses_match_sbcl_in_every_tier() {
    let program = format!("(defun f () {PROGRAM})");
    for tier in [None, Some("t1"), Some("t2")] {
        let env: Vec<(&str, &str)> = tier.map(|t| ("EGCL_FORCE_TIER", t)).into_iter().collect();
        let out = run(&env, &["--no-init", "--eval", &program, "--eval", "(f)"]);
        assert_eq!(last_line(&out), SBCL, "tier {tier:?}");
    }
    let out = run(&[], &["--no-init", "--eval", PROGRAM]);
    assert_eq!(last_line(&out), SBCL, "top level");
}

/// The numeric `and` loop is portably compiled, so `compile-file` writes it as
/// bytecode instead of falling back to source.
#[test]
fn compile_file_compiles_a_numeric_and_loop() {
    let dir = std::env::temp_dir().join(format!("egcl-loop-and-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("and.lisp");
    std::fs::write(
        &source,
        "(defparameter *f* (lambda (v start end d-start) \
           (loop for i fixnum from start below end and di fixnum from d-start \
                 do (setf (aref v di) i) finally (return (- di d-start)))))\n",
    )
    .unwrap();
    let fasl = dir.join("and.fasl");
    let program = format!(
        "(progn (load (compile-file {:?} :output-file {:?})) \
                (let ((v (make-array 5 :initial-element nil))) (list (funcall *f* v 0 3 2) v)))",
        source.display().to_string(),
        fasl.display().to_string()
    );
    let out = run(
        &[("EGCL_BFASL_FORM", "1")],
        &["--no-init", "--eval", &program],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("not portably compilable"), "{stderr}");
    assert_eq!(last_line(&out), "(3 #(NIL NIL 0 1 2))");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arithmetic_groups_compile_without_interpreter_fallback() {
    for body in [
        "(loop for i fixnum from 2 below n by 2 and di fixnum from 5 by 3 collect (list i di))",
        "(loop for i from 0 below n and di from 10 finally (return di))",
        "(loop for i from 0 below n and j from 0 and k from 100 do (setf k (+ k j)) finally (return (list i j k)))",
    ] {
        let program =
            format!("(defun numeric-and (n) {body}) (disassemble #'numeric-and) (numeric-and 3)");
        let out = run(&[], &["--no-init", "--eval", &program]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("; NUMERIC-AND —"), "{body}: {stdout}");
    }
}

#[test]
fn arithmetic_groups_match_under_gc_stress_and_poison() {
    let program = "(defun numeric-and (n) (loop for i fixnum from 0 below n and di fixnum from 10 collect (list i di))) (numeric-and 3)";
    let args = ["--no-init", "--eval", program];
    let normal = last_line(&run(&[], &args));
    let stress = last_line(&run(
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
        &args,
    ));
    assert_eq!(normal, "((0 10) (1 11) (2 12))");
    assert_eq!(stress, normal);
}

#[test]
fn driver_calls_and_symbol_macros_keep_parallel_initialization() {
    let program = r#"
      (defparameter *loop-start* 10)
      (defun read-loop-start () *loop-start*)
      (list
        (loop for *loop-start* from 0 below 1 and j from (read-loop-start) collect j)
        (let ((i 100)) (loop for i from 0 below 2 and j from i collect j))
        (let ((i 100)) (symbol-macrolet ((start i))
          (loop for i from 0 below 2 and j from start collect j))))
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_eq!(
            last_line(&run(
                &[("EGCL_FORCE_TIER", tier)],
                &["--no-init", "--eval", program]
            )),
            "((10) (100 101) (100 101))",
            "tier {tier}"
        );
    }
}

#[test]
fn allocating_parallel_initializers_survive_gc_stress() {
    let program = r#"
      (loop for i from (/ 1 3) below (/ 7 3)
            and j from (progn (make-list 50) (/ 2 3))
            collect (list i j))
    "#;
    let args = ["--no-init", "--eval", program];
    let normal = last_line(&run(&[("EGCL_FORCE_TIER", "interp")], &args));
    let stressed = last_line(&run(
        &[
            ("EGCL_FORCE_TIER", "interp"),
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
        &args,
    ));
    assert_eq!(normal, "((1/3 2/3) (4/3 5/3))");
    assert_eq!(stressed, normal);
}

#[test]
fn numeric_group_limits_and_sequential_scope_match_sbcl() {
    let program = r#"
      (list
        (loop for i from 0 below 3 and j from 10 finally (return (list i j)))
        (loop for i from 0 below 3 and j downfrom 10 to 9 finally (return (list i j)))
        (loop for i from 0 below 0 and j from 10 finally (return (list i j)))
        (let ((i 100)) (loop for i from 0 below 2 for j from i collect j))
        (loop for nil from 0 below 3 and j from 10 finally (return j))
        (let ((h (make-hash-table)))
          (setf (gethash :x h) 42)
          (loop for k being the hash-keys of h using (hash-value v)
                for i from 0 below 1 and j from 10 collect (list k v i j))))
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_eq!(
            last_line(&run(
                &[("EGCL_FORCE_TIER", tier)],
                &["--no-init", "--eval", program]
            )),
            "((3 13) (2 8) (0 10) (0 1) 13 ((:X 42 0 10)))",
            "tier {tier}"
        );
    }
}
