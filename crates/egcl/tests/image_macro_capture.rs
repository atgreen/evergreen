// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

/// A macro defined inside a LET keeps its captured binding across a saved image,
/// as a DEFUN or DEFMETHOD in the same LET always did (GitHub #426, bliss-6kts9).
///
/// The image carried only a macro's `(name, params, body)` and rebound every
/// restored expander to the fresh root frame, so an expansion that mentioned a
/// captured variable signalled UNBOUND-VARIABLE. anaphora defines SYMBOLIC and
/// ANAPHORIC inside `(with-unique-names ...)`, so every anaphoric macro in a
/// restored core failed this way.
#[test]
fn saved_macros_preserve_their_captured_frames() {
    let dir = std::env::temp_dir().join(format!("egcl-macro-capture-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let core = dir.join("macro-capture.core");
    let source = format!(r#"
      ;; The reported shape, beside the DEFUN and DEFMETHOD that already worked.
      (let ((x 42)) (defun get-x () x))
      (let ((y (list 1 2))) (defmacro with-y () `',y))
      (let ((z 7)) (defgeneric gz (a)) (defmethod gz ((a t)) (+ a z)))
      ;; One frame shared by two macros and a function: the restore must SHARE it,
      ;; not hand each definition a copy.
      (let ((shared (list :a)))
        (defmacro m-read () `',shared)
        (defun get-shared () shared)
        (defun set-shared (v) (setq shared v)))
      ;; A parent chain: the expander's own frame holds INNER, its parent OUTER.
      (let ((outer 10)) (let ((inner 5)) (defmacro m-nested () `(+ ,outer ,inner))))
      ;; The anaphora shape: a gensym captured in a binding, whose identity the
      ;; expansion depends on.
      (let ((tag (gensym "TAG-"))) (defmacro m-tag () `',tag) (defun tag-fn () tag))
      ;; A capture mutated after the macro is defined, before the save.
      (let ((counter 0)) (defmacro m-counter () `',counter) (defun bump () (setq counter (+ counter 1))))
      (bump)
      (bump)
      ;; A top-level macro captures nothing and must still restore (the frame-id
      ;; u32::MAX path).
      (defmacro m-top () '(list :top))
      (egcl-ext:save-lisp-and-die {core:?})
    "#);
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source]).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--image"]).arg(&core)
        .args(["--eval", r#"
          (assert (eql (get-x) 42))
          (assert (eql (gz 1) 8))
          (assert (equal (eval '(with-y)) '(1 2)))
          (assert (equal (eval '(m-read)) '(:a)))
          (assert (eq (eval '(m-read)) (get-shared)))
          (assert (eql (eval '(m-nested)) 15))
          (assert (eq (eval '(m-tag)) (tag-fn)))
          (assert (eql (eval '(m-counter)) 2))
          (assert (equal (eval '(m-top)) '(:top)))
          ;; Decisive for sharing rather than copying: mutate the binding through
          ;; the function and the macro's expansion must see it. A per-definition
          ;; copy passes every assertion above and fails this one.
          (set-shared (list :b))
          (assert (equal (eval '(m-read)) '(:b)))
          (format t "RESTORED-MACRO-CAPTURES-OK~%")
        "#]).output().unwrap();
    assert!(restored.status.success(), "{}\n{}",
        String::from_utf8_lossy(&restored.stdout), String::from_utf8_lossy(&restored.stderr));
    assert!(String::from_utf8_lossy(&restored.stdout).contains("RESTORED-MACRO-CAPTURES-OK"));
}
