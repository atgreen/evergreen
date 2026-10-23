// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LOOP's `it` anaphor (CLHS 6.1.6) is clause syntax, not a variable: only a
//! bare `it`/`:it` written as the operand of the first clause after a
//! WHEN/IF/UNLESS test denotes the test value. An `it` inside a compound form,
//! quoted, or in a later AND/ELSE clause is an ordinary reference, so it sees
//! an enclosing lexical IT (or is unbound). Compiled LOOP lowering used to
//! substitute every nested IT, returning ((1 . 1) (2 . 2)) for
//! `when x collect (cons it it)` where SBCL and the interpreter signal
//! UNBOUND-VARIABLE, and capturing a legitimate outer IT (bliss-bo2uz).
//! Every expectation below was taken from SBCL.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// (name, body over the list XS, argument, SBCL result).
const CASES: &[(&str, &str, &str, &str)] = &[
    (
        "it-bare",
        "(loop for x in xs when x collect it)",
        "'(1 nil 2)",
        "(1 2)",
    ),
    (
        "it-keyword",
        "(loop for x in xs when x collect :it)",
        "'(1 nil 2)",
        "(1 2)",
    ),
    (
        "it-lexical",
        "(let ((it :outer)) (loop for x in xs when x collect (list it x)))",
        "'(1 nil 2)",
        "((:OUTER 1) (:OUTER 2))",
    ),
    (
        "it-shadowed",
        "(let ((it :outer)) (loop for x in xs when x collect (let ((it :inner)) (list it x))))",
        "'(1 nil 2)",
        "((:INNER 1) (:INNER 2))",
    ),
    (
        "it-quoted",
        "(loop for x in xs when x collect 'it)",
        "'(1 nil 2)",
        "(IT IT)",
    ),
    (
        "it-and",
        "(let ((it :outer)) (loop for x in xs when x collect it and collect it))",
        "'(a nil b)",
        "(A :OUTER B :OUTER)",
    ),
    (
        "it-else",
        "(let ((it :outer)) (loop for x in xs when x collect it else collect it))",
        "'(a nil b)",
        "(A :OUTER B)",
    ),
    (
        "it-unless",
        "(let ((it :outer)) (loop for x in xs unless x collect it))",
        "'(1 nil 2)",
        "(NIL)",
    ),
    (
        "it-sum",
        "(loop for x in xs if x sum it)",
        "'(1 nil 2)",
        "3",
    ),
    (
        "it-sum-compound",
        "(let ((it 10)) (loop for x in xs when x sum (+ it x)))",
        "'(1 2 3)",
        "36",
    ),
    (
        "it-count",
        "(loop for x in xs when (evenp x) count it)",
        "'(1 2 3)",
        "1",
    ),
    (
        "it-return",
        "(loop for x in xs when (and x (> x 1)) return it)",
        "'(1 nil 2 3)",
        "T",
    ),
    (
        "it-nested-test",
        "(loop for x in xs when x when it collect it)",
        "'(1 nil 2)",
        "(1 2)",
    ),
    (
        "it-nested-owner",
        "(loop for x in xs when x when (cdr x) collect it)",
        "'((1) nil (2 . 3))",
        "(3)",
    ),
    (
        "it-nested-unbound",
        "(handler-case (loop for x in xs when x collect (cons it it)) \
           (unbound-variable () :unbound))",
        "'(1 nil 2)",
        ":UNBOUND",
    ),
];

fn definitions() -> String {
    let mut out = String::new();
    for (name, body, _, _) in CASES {
        out.push_str(&format!("(defun {name} (xs) {body})\n"));
    }
    out
}

/// The same cases as top-level forms. A loaded or evaluated top-level form is
/// lowered directly from its LOOP source, the path that substituted nested IT.
fn toplevel() -> String {
    let mut out = String::new();
    for (name, body, arg, _) in CASES {
        out.push_str(&format!(
            "(format t \"TOP ~A ~S~%\" '{name} (let ((xs {arg})) {body}))\n"
        ));
    }
    out
}

fn calls(warm: usize) -> String {
    let mut out = String::new();
    for (name, _, arg, _) in CASES {
        out.push_str(&format!(
            "(dotimes (i {warm}) ({name} {arg}))\n\
             (format t \"RESULT ~A ~S~%\" '{name} ({name} {arg}))\n"
        ));
    }
    out
}

fn check(label: &str, prefixes: &[&str], output: std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{label}: {stdout}\n{stderr}");
    for prefix in prefixes {
        for (name, _, _, expected) in CASES {
            let line = format!("{prefix} {} {expected}", name.to_uppercase());
            assert!(
                stdout.lines().any(|l| l.trim_end() == line),
                "{label}: expected `{line}`\n{stdout}\n{stderr}"
            );
        }
    }
    stdout
}

fn run(label: &str, prefixes: &[&str], envs: &[(&str, &str)], program: &str) -> String {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    command.env_remove("EGCL_FORCE_TIER");
    for (k, v) in envs {
        command.env(k, v);
    }
    check(label, prefixes, command.output().unwrap())
}

#[test]
fn loop_it_eval_matches_sbcl() {
    let mut program = String::new();
    for (name, body, arg, _) in CASES {
        program.push_str(&format!(
            "(format t \"RESULT ~A ~S~%\" '{name} (eval '(let ((xs {arg})) {body})))\n"
        ));
    }
    run(
        "eval",
        &["RESULT"],
        &[("EGCL_FORCE_TIER", "interp")],
        &program,
    );
    run("eval-default", &["RESULT"], &[], &program);
}

/// Each forced tier must actually install that tier for every case, so a
/// passing result cannot come from a fallback to a lower tier.
fn check_tier(tier: &str) {
    let mut program = definitions();
    program.push_str(&calls(2));
    program.push_str(&toplevel());
    for (name, _, _, _) in CASES {
        program.push_str(&format!("(disassemble '{name})\n"));
    }
    let stdout = run(
        tier,
        &["RESULT", "TOP"],
        &[("EGCL_FORCE_TIER", tier)],
        &program,
    );
    if tier != "interp" {
        let lines: Vec<&str> = stdout.lines().collect();
        let marker = format!("; {} \u{2014}", tier.to_uppercase());
        for (name, _, _, _) in CASES {
            let header = format!("; {} \u{2014}", name.to_uppercase());
            let at = lines.iter().position(|l| l.starts_with(&header));
            let installed = at
                .and_then(|i| lines.get(i + 1))
                .map(|l| l.starts_with(&marker));
            assert_eq!(
                installed,
                Some(true),
                "{tier}: {name} is not {marker}\n{stdout}"
            );
        }
    }
}

#[test]
fn loop_it_interpreted_defun_matches_sbcl() {
    check_tier("interp");
}

#[test]
fn loop_it_t0_matches_sbcl() {
    check_tier("t0");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn loop_it_t1_matches_sbcl() {
    check_tier("t1");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn loop_it_t2_matches_sbcl() {
    check_tier("t2");
}

/// Default tiering: warm each function past the T0 -> T1 invocation gate.
#[test]
fn loop_it_warmed_defun_matches_sbcl() {
    let mut program = definitions();
    program.push_str(&calls(64));
    program.push_str(&toplevel());
    run(
        "warmed",
        &["RESULT", "TOP"],
        &[("EGCL_T1_THRESHOLD", "10")],
        &program,
    );
}

#[test]
fn loop_it_compile_file_matches_sbcl() {
    let dir: PathBuf = std::env::temp_dir().join(format!("egcl-loop-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("loop-it.lisp");
    std::fs::write(&source, definitions() + &toplevel()).unwrap();
    let mut program = format!("(load (compile-file {:?}))\n", source.to_string_lossy());
    program.push_str(&calls(16));
    run("compile-file", &["RESULT", "TOP"], &[], &program);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bootstrap-free tree-walker LOOP (`--no-bootstrap`, before lib/loop.lisp
/// defines the LOOP macro) bound a lexical IT in the loop frame, visible to
/// compound forms and AND clauses. RETURN clauses avoid the accumulation
/// library functions that image lacks. Expectations are SBCL's.
#[test]
fn loop_it_bootstrap_interpreter_matches_sbcl() {
    let program = r#"
      (print (list :raw
        (loop for x in '(nil 2 3) when x return it)
        (loop for x in '(nil 2 3) when x return :it)
        (let ((it :outer)) (loop for x in '(nil 2 3) when x return (list it x)))
        (let ((it :outer)) (loop for x in '(nil 2 3) when x do (setq x x) and return it))
        (let ((it :outer)) (loop for x in '(nil 2 3) unless x return it))
        (loop for x in '(nil (2 . 5)) when x when (cdr x) return it)
        (loop for x in '(nil 4) when x when it return it)))
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--no-bootstrap", "--eval", program])
        .env_remove("EGCL_FORCE_TIER")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("(:RAW 2 2 (:OUTER 2) :OUTER NIL 5 4)"),
        "{stdout}\n{stderr}"
    );
}
