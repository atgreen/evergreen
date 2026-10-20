// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, process::Command};

// Capturing MakeClosure and captured environment setup are not admitted by
// Linux mapped entries yet; their portable bytecode implementation remains the
// exact fallback. Non-capturing closures use a mapped helper (bliss-ku9w).
fn factory_tier() -> u8 {
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        0
    } else {
        1
    }
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "s390x"))]
fn source_loop_factory_compiles_callbacks_with_independent_captures() {
    let program = r#"
      (defun eager-counter (seed)
        (dotimes (i 1) (incf seed))
        (lambda (delta) (incf seed delta)))
      (dotimes (i 40) (eager-counter i))
      NATIVE-FACTORY-CHECK
      (let ((a (eager-counter 10)) (b (eager-counter 100)))
        (assert (integerp (egcl-ext:function-tier a)))
        (assert (integerp (egcl-ext:function-tier b)))
        (dotimes (i 40)
          (assert (= (funcall a 2) (+ 13 (* i 2))))
          (assert (= (funcall b 3) (+ 104 (* i 3)))))
        (egcl-ext:gc)
        (assert (= (funcall a 4) 95))
        (assert (= (funcall b 5) 226)))
      (format t "EAGER-CAPTURES-OK~%")
    "#;
    for tier in ["t0", "t1", "t2"] {
        let program = program.replace("NATIVE-FACTORY-CHECK", if tier == "t0" {
            ""
        } else if factory_tier() == 0 {
            "(assert (= (egcl-ext:function-tier 'eager-counter) 0))"
        } else {
            "(assert (> (egcl-ext:function-tier 'eager-counter) 0))"
        });
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{tier}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("EAGER-CAPTURES-OK"));
    }
}

fn run_case(name: &str, definitions: &str, probe: &str, envs: &[(&str, &str)]) -> String {
    let dir = std::env::temp_dir().join(format!("egcl-native-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("factory.lisp");
    fs::write(&source, definitions).unwrap();
    let program = format!("(load (compile-file {source:?}))\n{probe}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.env_remove("EGCL_FORCE_TIER");
    for (key, value) in envs { command.env(key, value); }
    let output = command.args(["--no-init", "--eval", &program]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn fasl_closure_factory_retains_independent_captures() {
    let out = run_case("captures", r#"
      (defun portable-counter (start) (lambda (delta) (incf start delta)))
    "#, r#"
      (dotimes (i 20) (portable-counter i))
      (let ((a (portable-counter 10)) (b (portable-counter 100)))
        (assert (= (funcall a 2) 12))
        (assert (= (funcall b 3) 103))
        (assert (= (funcall a 4) 16)))
      (format t "FACTORY-TIER=~D~%" (egcl-ext:function-tier 'portable-counter))
    "#, &[("EGCL_FORCE_TIER", "t1")]);
    assert!(out.contains(&format!("FACTORY-TIER={}", factory_tier())), "{out}");
}

#[test]
fn closure_factory_keeps_original_nested_body_across_redefinition() {
    let out = run_case("redefine", r#"
      (defun original-factory (callback value)
        (funcall callback)
        (lambda () (list "original literal" value)))
      (defun call-factory (callback value) (original-factory callback value))
    "#, r#"
      (dotimes (i 20)
        (funcall (fdefinition 'original-factory) (lambda () nil) i)
        (call-factory (lambda () nil) i))
      (format t "ORIGINAL-TIER=~D~%" (egcl-ext:function-tier 'original-factory))
      (let ((f (call-factory
                 (lambda ()
                   (setf (symbol-function 'original-factory)
                         (lambda (callback value) (declare (ignore callback value)) :new))
                   (egcl-ext:gc))
                 (list 17 23))))
        (egcl-ext:gc)
        (assert (equal (funcall f) '("original literal" (17 23)))))
      (assert (eq (original-factory nil nil) :new))
      (format t "ORIGINAL-BODY-OK~%")
    "#, &[("EGCL_FORCE_TIER", "t1")]);
    assert!(out.contains(&format!("ORIGINAL-TIER={}", factory_tier())), "{out}");
    assert!(out.contains("ORIGINAL-BODY-OK"), "{out}");
}

#[test]
fn captured_nonlocal_exit_keeps_safe_fallback() {
    // Native handler setup cannot yet represent captured block/tag scopes.
    // Adding native closure allocation must preserve this safety fallback.
    let out = run_case("exit", r#"
      (defun invoke-escape (f) (funcall f))
      (defun escape-factory (value)
        (block destination
          (invoke-escape (lambda () (return-from destination value)))
          :wrong))
    "#, r#"
      (dotimes (i 20) (assert (= (funcall (fdefinition 'escape-factory) i) i)))
      (format t "ESCAPE-TIER=~D~%" (egcl-ext:function-tier 'escape-factory))
    "#, &[("EGCL_FORCE_TIER", "t1")]);
    assert!(out.contains("ESCAPE-TIER=0"), "{out}");
}

#[test]
fn closure_creation_survives_osr_entry() {
    let comparison = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "="
    } else {
        ">"
    };
    let checks = format!(
        r#"
      (let ((before (egcl-ext:deopt-count)))
        (assert (= (closure-loop 1000000) 499999500000))
        (assert ({comparison} (egcl-ext:deopt-count) before)))
      (assert ({comparison} (egcl-ext:function-osr-count 'closure-loop) 0))
      (format t "CLOSURE-OSR-OK~%")
    "#
    );
    let out = run_case(
        "osr",
        r#"
      (defun closure-loop (n)
        (let ((sum 0))
          (dotimes (i n sum)
            (incf sum (funcall (lambda () i))))))
    "#,
        &checks,
        &[
            ("EGCL_LAZY_COMPILE", "0"),
            ("EGCL_T0_T1_THRESHOLD", "10000000"),
            ("EGCL_OSR_THRESHOLD", "100"),
            ("EGCL_DISABLE_T2", "1"),
        ],
    );
    assert!(out.contains("CLOSURE-OSR-OK"), "{out}");
}

#[test]
fn closure_factory_binds_more_captured_parameters_than_stack_slots() {
    let out = run_case("wide-captures", r#"
      (defun capture-five (a b c d e)
        (lambda () (list a b c d e)))
    "#, r#"
      (dotimes (i 20)
        (assert (equal (funcall (capture-five i 2 3 4 5)) (list i 2 3 4 5))))
      (format t "WIDE-CAPTURE-TIER=~D~%" (egcl-ext:function-tier 'capture-five))
    "#, &[("EGCL_FORCE_TIER", "t1")]);
    assert!(out.contains(&format!("WIDE-CAPTURE-TIER={}", factory_tier())), "{out}");
}

#[test]
fn saved_fasl_function_retains_bytecode_after_function_cell_replacement() {
    let out = run_case("saved-fasl-call", r#"
      (defun saved-fasl-target (x) (+ x 1))
      (defun saved-fasl-invoke (f x) (funcall f x))
    "#, r#"
      (defvar *saved-fasl* #'saved-fasl-target)
      (dotimes (i 40) (assert (= (saved-fasl-invoke *saved-fasl* i) (+ i 1))))
      (setf (symbol-function 'saved-fasl-target) (lambda (x) (+ x 100)))
      (dotimes (i 40)
        (assert (= (saved-fasl-target i) (+ i 100)))
        (assert (= (saved-fasl-invoke *saved-fasl* i) (+ i 1))))
      (format t "SAVED-FASL-OK~%")
    "#, &[("EGCL_FORCE_TIER", "t2")]);
    assert!(out.contains("SAVED-FASL-OK"));
}

/// A function that only creates non-capturing closures reaches mapped T2 on
/// Linux, and every tier returns the same results with fresh closure identity
/// (bliss-ku9w). A capturing closure still keeps the interpreter fallback.
#[test]
fn noncapturing_lambda_outer_reaches_mapped_t2() {
    let definitions = r#"
      (defun s-nested-lambda (l) (mapcar (lambda (x) (* x 2)) l))
      (defun s-nocap () (let ((f (lambda (x) (+ x 1)))) (funcall f 1)))
      (defun s-make () (lambda (x) (list x :made)))
      (defun s-closure (n) (let ((f (lambda (x) (+ x n)))) (funcall f 1)))
    "#;
    let probe = r#"
      (dotimes (i 200)
        (s-nested-lambda (list i 2 3)) (s-nocap) (s-make) (s-closure i))
      (dotimes (warm 2000) (egcl-ext:function-tier 's-nested-lambda))
      (format t "TIERS=~S~%"
              (mapcar #'egcl-ext:function-tier '(s-nested-lambda s-nocap s-make s-closure)))
      (format t "RESULTS=~S~%"
              (list (s-nested-lambda (list 1 2 3)) (s-nocap) (funcall (s-make) 7)
                    (eq (s-make) (s-make)) (s-closure 5)))
    "#;
    let results = "RESULTS=((2 4 6) 2 (7 :MADE) NIL 6)";
    for tier in ["t0", "t1", "t2"] {
        let out = run_case(
            &format!("noncapturing-{tier}"),
            definitions,
            probe,
            &[("EGCL_FORCE_TIER", tier)],
        );
        assert!(out.contains(results), "{tier}: {out}");
    }
    let dir = std::env::temp_dir().join(format!("egcl-native-noncapturing-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("factory.lisp");
    fs::write(&source, definitions).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env_remove("EGCL_FORCE_TIER")
        .env("EGCL_T0_T1_THRESHOLD", "5")
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "-")
        .args([
            "--no-init",
            "--eval",
            &format!("(load {source:?})\n{probe}"),
        ])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains(results), "{stdout}");
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        for name in ["S-NESTED-LAMBDA", "S-NOCAP", "S-MAKE"] {
            assert!(
                stderr.contains(&format!(
                    "{name}: optimized worker result installed with mapped T2 ABI"
                )),
                "{name} must install mapped T2: {stderr}"
            );
        }
        assert!(stdout.contains("TIERS=(2 2 2 0)"), "{stdout}\n{stderr}");
        assert!(
            !stderr.contains("S-CLOSURE: optimized worker result installed"),
            "{stderr}"
        );
    }
}

/// A hot nested lambda that inherits a block/tag from its enclosing function
/// and creates a non-capturing closure exiting to it must keep that exit
/// visible at every tier. A native activation does not publish inherited
/// control, so such a creator stays at T0 (bliss-ku9w review).
#[test]
fn natively_created_closure_keeps_inherited_nonlocal_exits() {
    let definitions = r#"
      (defun nlx-call (f) (funcall f))
      (defun nlx-return (l)
        (block b
          (mapcar (lambda (x) (if (eql x 3) (nlx-call (lambda () (return-from b :escaped))) x))
                  l)))
      (defun nlx-go (l)
        (let ((r :none))
          (tagbody
             (mapcar (lambda (x) (if (eql x 3) (nlx-call (lambda () (go out))) x)) l)
             (setq r :fell-through)
             (go done)
           out
             (setq r :went)
           done)
          r))
      (defun nlx-list (n at)
        (let ((l nil)) (dotimes (i n) (push (if (= i at) 3 0) l)) (nreverse l)))
    "#;
    let probe = r#"
      (let ((r nil))
        (dotimes (i 30)
          (setq r (list (nlx-return (nlx-list 400 350)) (nlx-go (nlx-list 400 350)))))
        (format t "NLX=~S~%" r))
    "#;
    let expected = "NLX=(:ESCAPED :WENT)";
    for tier in ["t0", "t1", "t2"] {
        let out = run_case(
            &format!("inherited-nlx-{tier}"),
            definitions,
            probe,
            &[("EGCL_FORCE_TIER", tier)],
        );
        assert!(out.contains(expected), "{tier}: {out}");
    }
    let out = run_case(
        "inherited-nlx-default",
        definitions,
        probe,
        &[
            ("EGCL_T0_T1_THRESHOLD", "5"),
            ("EGCL_T2_THRESHOLD", "20"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(out.contains(expected), "default: {out}");
}
