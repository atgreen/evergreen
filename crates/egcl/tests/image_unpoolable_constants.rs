// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled functions whose constants a fasl cannot encode — a package, a hash
//! table or an instance from LOAD-TIME-VALUE — survive a saved image with those
//! exact objects. They used to be left out of the image, so their restored stubs
//! silently returned NIL: ironclad's MASSAGE-SYMBOL found no digest named :SHA3
//! in a saved ocicl executable.
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn saved_image_keeps_functions_with_unpoolable_constants() {
    let dir = std::env::temp_dir().join(format!("egcl-unpoolable-image-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let source = dir.join("constants.lisp");
    let core = dir.join("constants.core");
    fs::write(
        &source,
        r#"(defstruct point x y)
(defun keyword-package () (load-time-value (find-package :keyword)))
(defun keyword-symbol-p (symbol)
  (eq (symbol-package symbol) (load-time-value (find-package :keyword))))
(defun cache () (load-time-value (make-hash-table :test 'equal)))
(defun origin () (load-time-value (make-point :x 0 :y 0)))
"#,
    )
    .unwrap();
    let build = format!(
        r#"(progn
      (load (compile-file {source:?}))
      (setf (gethash "k" (cache)) 42)
      (egcl-ext:save-lisp-and-die {core:?}))"#
    );
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &build])
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
        .arg(&core)
        .args([
            "--eval",
            r#"(progn
          (assert (eq (keyword-package) (find-package :keyword)))
          (assert (keyword-symbol-p :sha3))
          (assert (not (keyword-symbol-p 'car)))
          (assert (hash-table-p (cache)))
          (assert (eql (gethash "k" (cache)) 42))
          (assert (eq (cache) (cache)))
          (assert (point-p (origin)))
          (assert (eql (point-x (origin)) 0))
          (format t "UNPOOLABLE-CONSTANTS-OK~%"))"#,
        ])
        .output()
        .unwrap();
    assert!(
        restored.status.success()
            && String::from_utf8_lossy(&restored.stdout).contains("UNPOOLABLE-CONSTANTS-OK"),
        "{}\n{}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
}

/// A function compiled from source after it ran hot, whose capturing handler
/// lambda the portable lowering leaves to MakeClosureEnv — UIOP's
/// CALL-WITH-MUFFLED-CONDITIONS — is saved, not left out of the image.
#[test]
fn saved_image_keeps_functions_that_build_closures_from_source() {
    let dir = std::env::temp_dir().join(format!("egcl-closure-env-image-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let source = dir.join("muffled.lisp");
    let core = dir.join("muffled.core");
    fs::write(
        &source,
        r#"(defun call-muffled (thunk conditions)
  (handler-bind ((t #'(lambda (c) (when (member (type-of c) conditions)
                                    (muffle-warning c)))))
    (funcall thunk)))
"#,
    )
    .unwrap();
    let build = format!(
        r#"(progn
      (load {source:?})
      (dotimes (i 50) (call-muffled (lambda () i) '(simple-warning)))
      (egcl-ext:save-lisp-and-die {core:?}))"#
    );
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &build])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{stderr}");
    assert!(!stderr.contains("left out"), "{stderr}");
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--image"])
        .arg(&core)
        .args([
            "--eval",
            r#"(progn
          (assert (eq (call-muffled (lambda () (warn "muffled") :ran) '(simple-warning)) :ran))
          (format t "CLOSURE-ENV-OK~%"))"#,
        ])
        .output()
        .unwrap();
    assert!(
        restored.status.success()
            && String::from_utf8_lossy(&restored.stdout).contains("CLOSURE-ENV-OK"),
        "{}\n{}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
}

/// A fasl never carries a saved heap reference (only an image's own restore can
/// give the address meaning): its LOAD-TIME-VALUE forms are evaluated again
/// when it loads.
#[test]
fn fasl_load_time_package_constants_still_load() {
    let dir = std::env::temp_dir().join(format!("egcl-unpoolable-fasl-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let source = dir.join("fasl.lisp");
    fs::write(
        &source,
        "(defun fasl-keyword-package () (load-time-value (find-package :keyword)))\n",
    )
    .unwrap();
    let program = format!(
        r#"(progn
      (compile-file {source:?})
      (load (compile-file-pathname {source:?}))
      (assert (eq (fasl-keyword-package) (find-package :keyword)))
      (format t "FASL-OK~%"))"#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("FASL-OK"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
