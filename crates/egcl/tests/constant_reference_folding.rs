// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::{Command, Output};

fn check(output: Output, label: &str) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{label}:\n{stdout}\n{stderr}");
    stdout
}

#[test]
fn established_constants_are_literals_without_changing_variable_resolution() {
    for tier in ["t0", "t1", "t2"] {
        let stdout = check(
            Command::new(env!("CARGO_BIN_EXE_egcl"))
                .env("EGCL_LAZY_COMPILE", "0")
                .env("EGCL_FORCE_TIER", tier)
                .args([
                    "--no-init",
                    "--eval",
                    r#"
                (defconstant +folded-bound+ 5)
                (defun folded-bound-reader () +folded-bound+)
                (defun folded-bound-loop (n)
                  (let ((total 0))
                    (dotimes (i +folded-bound+ total) (incf total n))))
                ;; Inspect bytecode before warming: a constant reference must
                ;; become a literal, not merely run correctly through a global.
                (disassemble #'folded-bound-reader)
                (disassemble #'folded-bound-loop)
                (dotimes (i 120)
                  (assert (= (folded-bound-reader) 5))
                  (assert (= (folded-bound-loop 3) 15)))
                ;; EQL redefinition is legal and leaves substituted values valid.
                (defconstant +folded-bound+ 5)
                (assert (= (folded-bound-reader) 5))
                (defun ordinary-local (+looks-constant+) +looks-constant+)
                (assert (= (ordinary-local 19) 19))
                (let ((+captured-looking+ 23))
                  (let ((reader (lambda () +captured-looking+)))
                    (assert (= (funcall reader) 23))))
                (defparameter mutable-reference 7)
                (defun special-reference ()
                  (declare (special mutable-reference)) mutable-reference)
                (assert (= (let ((mutable-reference 29))
                             (declare (special mutable-reference))
                             (special-reference)) 29))
                (setq mutable-reference 31)
                (assert (= (special-reference) 31))
                (symbol-macrolet ((+macro-looking+ 37))
                  (assert (= +macro-looking+ 37)))
                ;; A reference compiled before a constant is established keeps
                ;; its runtime lookup; no unbound sentinel is captured.
                (defun late-bound-reader () +late-folded-bound+)
                (let ((caught nil))
                  (handler-case (late-bound-reader)
                    (unbound-variable () (setf caught t)))
                  (assert caught))
                (defconstant +late-folded-bound+ 41)
                (assert (= (late-bound-reader) 41))
                ;; Metadata alone is insufficient when the value is unbound.
                (egcl-internal::%mark-constant '+marked-unbound-folding+)
                (defun marked-unbound-reader () +marked-unbound-folding+)
                (let ((caught nil))
                  (handler-case (marked-unbound-reader)
                    (unbound-variable () (setf caught t)))
                  (assert caught))
                (format t "CONSTANT-REFERENCE-RESOLUTION-OK~%")
            "#,
                ])
                .output()
                .expect("run constant reference case"),
            tier,
        );
        assert!(stdout.contains("Const("), "tier={tier}: {stdout}");
        assert!(
            !stdout.contains("LoadGlobal("),
            "constant still dynamically loaded: {stdout}"
        );
        assert!(
            stdout.contains("CONSTANT-REFERENCE-RESOLUTION-OK"),
            "{stdout}"
        );
    }
}

#[test]
fn heap_constant_literals_survive_allocations_and_share_the_established_object() {
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .args([
                "--no-init",
                "--eval",
                r#"
                (defconstant +owned-reference+ (vector (list 7 11) "owned-constant"))
                (defun owned-reference-reader () +owned-reference+)
                (dotimes (i 120)
                  (make-array 20 :initial-element (list i))
                  (assert (eq (owned-reference-reader) +owned-reference+))
                  (assert (equal (aref (owned-reference-reader) 0) '(7 11)))
                  (assert (string= (aref (owned-reference-reader) 1) "owned-constant")))
                (format t "HEAP-CONSTANT-REFERENCE-OK~%")
            "#,
            ]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let stdout = check(
            command.output().expect("run heap constant case"),
            "heap constant",
        );
        assert!(stdout.contains("HEAP-CONSTANT-REFERENCE-OK"), "{stdout}");
    }
}

#[test]
fn folded_literals_round_trip_through_fresh_process_bfasl_load() {
    let directory =
        std::env::temp_dir().join(format!("egcl-constant-folding-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("constants.lisp");
    let compiled = directory.join("constants.bfasl");
    std::fs::write(
        &source,
        r#"
        (defconstant +portable-folded-bound+ 5)
        (defconstant +portable-owned-reference+ '#(7 11 13))
        (defun portable-bound-reader () +portable-folded-bound+)
        (defun portable-owned-reader () +portable-owned-reference+)
    "#,
    )
    .unwrap();
    let compile = format!("(compile-file {source:?} :output-file {compiled:?})");
    check(
        Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &compile])
            .output()
            .expect("compile literal fixture"),
        "compile fixture",
    );
    assert!(compiled.is_file());
    let program = format!(
        r#"
        (load {compiled:?})
        (dotimes (i 80)
          (make-array 20 :initial-element (list i))
          (assert (= (portable-bound-reader) 5))
          (assert (equalp (portable-owned-reader) #(7 11 13)))
          (assert (eq (portable-owned-reader) +portable-owned-reference+)))
        (format t "PORTABLE-CONSTANT-REFERENCE-OK~%")
    "#
    );
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .args(["--no-init", "--eval", &program]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let stdout = check(
            command.output().expect("load literal fixture"),
            "fresh load",
        );
        assert!(
            stdout.contains("PORTABLE-CONSTANT-REFERENCE-OK"),
            "{stdout}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn cached_global_bytecode_uses_owner_captured_t2_constant_facts() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-cached-constant-folding-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("legacy-reader.lisp");
    let compiled = directory.join("legacy-reader.bfasl");
    // None of these symbols has a constant value in the compiling process.
    // The resulting BFASL therefore models existing cached LoadGlobal code.
    std::fs::write(
        &source,
        r#"
        (defun cached-constant-reader () +cached-established-constant+)
        (defun cached-special-reader () *cached-special-constant*)
        (defun cached-mutable-reader () cached-mutable-value)
    "#,
    )
    .unwrap();
    let compile = format!("(compile-file {source:?} :output-file {compiled:?})");
    check(
        Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &compile])
            .output()
            .unwrap(),
        "compile cached readers",
    );
    let program = format!(
        r#"
        (defconstant +cached-established-constant+ 5)
        (defconstant *cached-special-constant* 7)
        (defparameter cached-mutable-value 11)
        (load {compiled:?})
        (disassemble #'cached-constant-reader)
        (dotimes (i 500)
          (assert (= (cached-constant-reader) 5))
          (assert (= (cached-special-reader) 7))
          (assert (= (cached-mutable-reader) 11)))
        (assert (= (egcl-ext:function-tier 'cached-constant-reader) 2))
        (defconstant +cached-established-constant+ 5)
        (setq cached-mutable-value 13)
        (assert (= (cached-constant-reader) 5))
        (assert (= (cached-mutable-reader) 13))
        (format t "CACHED-CONSTANT-REFERENCE-OK~%")
    "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr")
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("load cached readers");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = check(output, "cached readers");
    assert!(
        stdout.contains("LoadGlobal("),
        "fixture was not cached global bytecode: {stdout}"
    );
    assert!(stdout.contains("CACHED-CONSTANT-REFERENCE-OK"), "{stdout}");
    assert!(
        stderr.contains("CACHED-CONSTANT-READER: captured 1 constant global fixnum binding(s)"),
        "late constant facts were not supplied to T2:\n{stderr}"
    );
    assert!(
        !stderr.contains("CACHED-SPECIAL-READER: captured"),
        "special reference captured:\n{stderr}"
    );
    assert!(
        !stderr.contains("CACHED-MUTABLE-READER: captured"),
        "mutable reference captured:\n{stderr}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn compile_time_disable_switch_preserves_dynamic_bytecode_references() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_DISABLE_CONSTANT_FOLDING", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T2_LOG", "stderr")
        .args([
            "--no-init",
            "--eval",
            r#"
            (defconstant +disabled-folding-value+ 17)
            (defun disabled-folding-reader () +disabled-folding-value+)
            (disassemble #'disabled-folding-reader)
            (dotimes (i 500) (assert (= (disabled-folding-reader) 17)))
            (format t "DISABLED-CONSTANT-FOLDING-OK~%")
        "#,
        ])
        .output()
        .expect("run disabled compiler");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = check(output, "disabled compiler");
    assert!(
        stdout.contains("LoadGlobal("),
        "source folding switch ignored: {stdout}"
    );
    assert!(
        !stderr.contains("DISABLED-FOLDING-READER: captured"),
        "T2 folding switch ignored: {stderr}"
    );
    assert!(stdout.contains("DISABLED-CONSTANT-FOLDING-OK"));
}
