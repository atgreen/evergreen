// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! COMPILE-FILE of a top-level MACROLET whose expansion is
//! `(defgeneric (setf NAME) ...)` (GitHub #441). The pre-read walked the
//! DEFGENERIC as a function call, so `(setf NAME)` read as a SETF form with
//! an odd argument count, signalled PROGRAM-ERROR, and the file was written
//! as a source-only artifact. zpb-ttf's bounding-box.lisp has this shape.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const FILE_SOURCE: &str = r#"
(defpackage :mlg441 (:use :cl))
(in-package :mlg441)
(defclass box () ((x :initform 0)))
;; The reporter's shape.
(macrolet ((acc (name) `(defgeneric (setf ,name) (new-value object))))
  (acc xmin))
(defmethod (setf xmin) (new (o box)) (setf (slot-value o 'x) new))
;; A :METHOD option whose body uses another local macro of the MACROLET.
(macrolet ((acc (name)
             `(defgeneric (setf ,name) (new o)
                (:documentation "doubling writer")
                (:method (new (o box)) (setf (slot-value o 'x) (twice new)))))
           (twice (v) `(* 2 ,v)))
  (acc xdouble))
;; Neighbouring definers that already worked.
(macrolet ((acc (name) `(defmethod (setf ,name) (new (o box)) (setf (slot-value o 'x) (+ new 1)))))
  (acc xplus))
(macrolet ((acc (name) `(defsetf ,name (o) (v) `(setf (slot-value ,o 'x) (- ,v)))))
  (acc xneg))
(defun run ()
  (let ((b (make-instance 'box)))
    (list (progn (setf (xmin b) 3) (slot-value b 'x))
          (progn (setf (xdouble b) 5) (slot-value b 'x))
          (progn (setf (xplus b) 7) (slot-value b 'x))
          (progn (setf (xneg b) 9) (slot-value b 'x)))))
"#;

fn egcl(program: &str) -> (bool, String) {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn compile_file_compiles_a_macrolet_expanding_to_defgeneric_setf() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let source = dir.join(format!("egcl-gh441-{}.lisp", std::process::id()));
    let fasl = dir.join(format!("egcl-gh441-{}.fasl", std::process::id()));
    std::fs::write(&source, FILE_SOURCE).expect("write source");

    let (ok, compile_text) = egcl(&format!(
        r#"(progn (compile-file "{src}" :output-file "{out}") (format t "COMPILED~%"))"#,
        src = source.display(),
        out = fasl.display()
    ));
    assert!(ok && compile_text.contains("COMPILED"), "{compile_text}");
    assert!(
        !compile_text.contains("could not pre-read"),
        "the pre-read fell back to a source-only artifact: {compile_text}"
    );

    // Hide the source so a source-only artifact could not re-read it, then
    // load the fasl in a fresh process.
    let hidden = source.with_extension("hidden");
    std::fs::rename(&source, &hidden).expect("hide source");
    let (ok, load_text) = egcl(&format!(
        r#"(progn (load "{out}")
             (format t "FBOUNDP=~S~%" (fboundp (list 'setf (find-symbol "XMIN" "MLG441"))))
             (format t "RESULT=~S~%" (funcall (find-symbol "RUN" "MLG441"))))"#,
        out = fasl.display()
    ));
    let _ = std::fs::remove_file(&hidden);
    let _ = std::fs::remove_file(&fasl);
    assert!(ok, "{load_text}");
    assert!(load_text.contains("FBOUNDP=T"), "{load_text}");
    assert!(load_text.contains("RESULT=(3 10 8 -9)"), "{load_text}");
}
