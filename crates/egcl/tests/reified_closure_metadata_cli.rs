// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_interpreted(program: &str, expected: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "interp")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("run reified closure regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == expected),
        "expected {expected:?}: {}\n{stdout}\n{stderr}",
        output.status
    );
}

#[test]
fn installed_closure_keeps_lexical_block() {
    check_interpreted(
        r#"
      (format t "DIRECT ~S~%"
        (block outer (funcall (lambda () (return-from outer :correct)))))
      (format t "REIFIED ~S~%"
        (block outer
          (setf (symbol-function 'installed-escape)
            (lambda () (return-from outer :correct)))
          (installed-escape)))
    "#,
        "REIFIED :CORRECT",
    );
}

#[test]
fn installed_closure_keeps_lexical_tagbody() {
    check_interpreted(
        r#"
      (format t "DIRECT ~S~%"
        (block done (tagbody
          (funcall (lambda () (go finish)))
          (return-from done :wrong)
          finish (return-from done :correct))))
      (format t "REIFIED ~S~%"
        (block done (tagbody
          (setf (symbol-function 'installed-jump) (lambda () (go finish)))
          (installed-jump)
          (return-from done :wrong)
          finish (return-from done :correct))))
    "#,
        "REIFIED :CORRECT",
    );
}

#[test]
fn installed_closure_keeps_lexical_function_namespace() {
    check_interpreted(
        r#"
      (defparameter *direct* (flet ((local-target () :correct))
        (lambda () (local-target))))
      (format t "DIRECT ~S~%" (funcall *direct*))
      (flet ((local-target () :correct))
        (setf (symbol-function 'installed-local) (lambda () (local-target))))
      (format t "REIFIED ~S~%" (installed-local))
    "#,
        "REIFIED :CORRECT",
    );
}

#[test]
fn installed_closure_keeps_special_declarations() {
    check_interpreted(
        r#"
      (setf (symbol-value 'metadata-value) :dynamic)
      (let ((metadata-value :lexical))
        (locally (declare (special metadata-value))
          (setq *direct* (lambda () metadata-value))
          (setf (symbol-function 'installed-special) (lambda () metadata-value))))
      (format t "DIRECT ~S~%" (funcall *direct*))
      (format t "REIFIED ~S~%" (installed-special))
    "#,
        "REIFIED :DYNAMIC",
    );
}

#[test]
fn saved_reified_closures_keep_shared_function_scopes_and_declarations() {
    let directory =
        std::env::temp_dir().join(format!("egcl-reified-metadata-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let image = directory.join("scopes.core");
    let setup = format!(
        r#"
      (setf (symbol-value 'metadata-special) :saved-global)
      (let ((shared (list 10)))
        (flet ((outer-add (n) (incf (car shared) n)))
          (flet ((local-add (n) (outer-add n)))
            (setq *direct* (lambda (n) (values (local-add n) shared)))
            (setf (symbol-function 'installed-shared)
              (lambda (n) (values (local-add n) shared))))))
      (labels ((local-sum (n) (if (zerop n) 0 (+ n (local-sum (1- n))))))
        (setf (symbol-function 'installed-recursive) (lambda (n) (local-sum n))))
      (let ((metadata-special :wrong-lexical))
        (locally (declare (special metadata-special))
          (setf (symbol-function 'installed-special) (lambda () metadata-special))))
      (defparameter *saved-reified* (symbol-function 'installed-shared))
      (setf (symbol-function 'installed-shared) (lambda (n) (declare (ignore n)) :replacement))
      (egcl-ext:save-lisp-and-die {image:?})
    "#
    );
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &setup])
        .env("EGCL_FORCE_TIER", "interp")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--image"])
        .arg(&image)
        .args([
            "--eval",
            r#"
          (format t "DIRECT ~S~%" (multiple-value-list (funcall *direct* 1)))
          (format t "REIFIED ~S~%" (multiple-value-list (funcall *saved-reified* 2)))
          (format t "RECURSIVE ~S REPLACEMENT ~S~%" (installed-recursive 4) (installed-shared 0))
          (let ((metadata-special :dynamic))
            (declare (special metadata-special))
            (format t "SPECIAL ~S~%" (installed-special)))
        "#,
        ])
        .env("EGCL_FORCE_TIER", "interp")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&restored.stdout);
    let stderr = String::from_utf8_lossy(&restored.stderr);
    assert!(restored.status.success(), "{stdout}\n{stderr}");
    for expected in [
        "DIRECT (11 (11))",
        "REIFIED (13 (13))",
        "RECURSIVE 10 REPLACEMENT :REPLACEMENT",
        "SPECIAL :DYNAMIC",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "missing {expected:?}: {stdout}\n{stderr}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_caller_and_symbol_designators_keep_saved_closure_metadata() {
    if !cfg!(all(target_arch = "x86_64", unix)) {
        return;
    }
    for native in ["0", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args([
                "--no-init",
                "--eval",
                r#"
(eval '(flet ((local-value (x) (values (+ x 10) :captured)))
         (setf (symbol-function 'installed-local) (lambda (x) (local-value x)))))
(defparameter *saved-local* (symbol-function 'installed-local))
(defun metadata-entry (f x) (funcall f x))
(dotimes (i 40) (metadata-entry #'identity i))
(format t "TIER ~D~%" (egcl-ext:function-tier 'metadata-entry))
(format t "QUOTED ~S APPLY ~S~%"
  (multiple-value-list (funcall 'installed-local 1))
  (multiple-value-list (apply 'installed-local '(2))))
(setf (symbol-function 'installed-local) (lambda (x) (declare (ignore x)) :replacement))
(fmakunbound 'installed-local)
(format t "SAVED ~S~%" (multiple-value-list (metadata-entry *saved-local* 3)))
"#,
            ])
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_NATIVE_TRANSFER", native)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "native={native}: {stdout}\n{stderr}"
        );
        for expected in [
            "TIER 2",
            "QUOTED (11 :CAPTURED) APPLY (12 :CAPTURED)",
            "SAVED (13 :CAPTURED)",
        ] {
            assert!(
                stdout.lines().any(|line| line == expected),
                "native={native}: missing {expected:?}: {stdout}\n{stderr}"
            );
        }
    }
}
