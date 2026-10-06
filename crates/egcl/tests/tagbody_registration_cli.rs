// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn run(program: &str, tier: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", tier)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run tagbody control checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    stdout.into_owned()
}

const LOCAL_LOOP: &str = r#"
    (defun local-control-loop ()
      (loop for x in '(1 nil 2) when x collect (cons x x)))
"#;

const CHECK_LOCAL_LOOP: &str = r#"(dotimes (i 128)
             (unless (equal (local-control-loop) '((1 . 1) (2 . 2)))
               (error "Loop result changed while warming")))
           (disassemble #'local-control-loop)
           (format t "RESULT:~S~%" (local-control-loop))"#;

fn check_local_loop(stdout: &str, tier: &str) {
    assert!(stdout.contains("RESULT:((1 . 1) (2 . 2))"), "{stdout}");
    assert!(
        stdout.contains(&format!("; {}", tier.to_uppercase())),
        "{stdout}"
    );
    assert!(stdout.contains("; LOCAL-CONTROL-LOOP —"), "{stdout}");
    assert!(!stdout.contains("NamedTag"), "{stdout}");
}

#[test]
fn local_loop_needs_no_named_tags() {
    check_local_loop(&run(&format!("{LOCAL_LOOP}{CHECK_LOCAL_LOOP}"), "t0"), "t0");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn local_loop_reaches_t2() {
    check_local_loop(&run(&format!("{LOCAL_LOOP}{CHECK_LOCAL_LOOP}"), "t2"), "t2");
}

#[test]
fn portable_local_loop_preserves_branch_targets() {
    let dir = std::env::temp_dir().join(format!("egcl-local-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("local-control.lisp");
    let fasl = dir.join("local-control.fasl");
    std::fs::write(&source, LOCAL_LOOP).unwrap();
    run(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "t0",
    );
    let program = format!("(load {fasl:?}) {CHECK_LOCAL_LOOP}");
    check_local_loop(&run(&program, "t0"), "t0");
    #[cfg(target_arch = "x86_64")]
    check_local_loop(&run(&program, "t2"), "t2");
    std::fs::remove_dir_all(dir).unwrap();
}

const HIDDEN_GO: &str = r#"
    (defmacro hidden-go (tag) `(lambda () (go ,tag)))
    (defun macro-go ()
      (let (k)
        (tagbody
          (setq k (hidden-go done))
          (funcall k)
          (return-from macro-go :bad)
         done)
        :ok))
"#;

const CHECK_HIDDEN_GO: &str = r#"
    (disassemble #'macro-go)
    (dotimes (i 32)
      (unless (eq (macro-go) :ok) (error "Hidden GO missed its target")))
    (format t "RESULT:~S~%" (macro-go))
"#;

#[test]
fn macro_hidden_go_keeps_its_tag() {
    let stdout = run(&format!("{HIDDEN_GO}\n{CHECK_HIDDEN_GO}"), "t0");
    assert!(stdout.contains("RESULT::OK"), "{stdout}");
    assert!(stdout.contains("NamedTag"), "{stdout}");
    assert!(stdout.contains("EvalHost"), "{stdout}");
}

#[test]
fn portable_macro_hidden_go_keeps_its_tag() {
    let dir = std::env::temp_dir().join(format!("egcl-hidden-go-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("hidden-go.lisp");
    let fasl = dir.join("hidden-go.fasl");
    std::fs::write(&source, HIDDEN_GO).unwrap();
    run(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "t0",
    );
    let stdout = run(&format!("(load {fasl:?})\n{CHECK_HIDDEN_GO}"), "t0");
    assert!(stdout.contains("RESULT::OK"), "{stdout}");
    assert!(stdout.contains("NamedTag"), "{stdout}");
    assert!(stdout.contains("MakeClosure"), "{stdout}");
    assert!(!stdout.contains("EvalHost"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn captured_tags_preserve_lexical_scope_and_opaque_bodies() {
    let cases = [
        (
            "shadowed",
            r#"(let ((n 0))
          (tagbody
            (tagbody (funcall (hidden-go done)) (setq n 100)
             done (incf n))
            (incf n 2)
           done)
          n)"#,
            "3",
        ),
        (
            "handler",
            r#"(tagbody
          (handler-bind ((error (lambda (c) (declare (ignore c)) (go done))))
            (error "go"))
          (return-from control-handler :bad)
         done) :ok"#,
            ":OK",
        ),
        (
            "restart",
            r#"(tagbody
          (restart-case (invoke-restart 'finish) (finish () :ok))
         done) :ok"#,
            ":OK",
        ),
        (
            "handler-case",
            r#"(tagbody
          (handler-case (error "go") (error () (go done)))
          (return-from control-handler-case :bad)
         done) :ok"#,
            ":OK",
        ),
        (
            "labels",
            r#"(let ((n 0))
          (loop repeat 3 do
            (labels ((done () (loop-finish)))
              (loop repeat 5 do (incf n) (done)))
            finally (incf n 10))
          n)"#,
            "11",
        ),
    ];
    let mut program = String::from("(defmacro hidden-go (tag) `(lambda () (go ,tag)))");
    for (name, body, expected) in cases {
        program.push_str(&format!(
            "(defun control-{name} () {body})
             (disassemble #'control-{name})
             (dotimes (i 32)
               (unless (equal (control-{name}) '{expected})
                 (error \"Control result changed: {name}\")))
             (format t \"RESULT:{name}:~S~%\" (control-{name}))"
        ));
    }
    for tier in ["t0", "t1", "t2"] {
        let stdout = run(&program, tier);
        for (name, _, expected) in cases {
            assert!(
                stdout.contains(&format!("RESULT:{name}:{expected}")),
                "{tier}: {stdout}"
            );
            let section = stdout
                .split(&format!("; CONTROL-{} —", name.to_uppercase()))
                .nth(1)
                .unwrap_or_else(|| panic!("{tier}: control-{name} not compiled: {stdout}"))
                .split(&format!("RESULT:{name}:"))
                .next()
                .unwrap();
            assert!(section.contains("NamedTag"), "{tier}: {section}");
        }
    }
}

#[test]
fn restart_go_preserves_interpreted_fallback() {
    // Restart clauses do not yet compile an outer GO. Keep its runtime
    // behavior covered separately from the compiled metadata assertions.
    let program = r#"
        (defun restart-go ()
          (tagbody
            (restart-case (invoke-restart 'finish) (finish () (go done)))
            (return-from restart-go :bad)
           done)
          :ok)
        (format t "RESULT:~S~%" (restart-go))
    "#;
    for tier in ["t0", "t1", "t2"] {
        assert!(run(program, tier).contains("RESULT::OK"));
    }
}
