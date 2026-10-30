// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defun invoke-nsubst-options (options)
  (apply #'nsubst :new :old (list :old) options))
(defun check-nsubst-aliases ()
  ;; A compiler pass retains a cursor while another pass substitutes one arg.
  (let* ((dead (list :ignored)) (arguments (list :stream dead))
         (cursor (cdr arguments))
         (rewritten (nsubst :checked-stream :stream arguments)))
    (setf (car cursor) nil)
    (assert (eq rewritten arguments))
    (assert (eq (cdr rewritten) cursor))
    (assert (equal rewritten '(:checked-stream nil))))
  (let* ((child (list :old)) (tail (cons :keep :old))
         (tree (cons child tail)) (result (nsubst :new :old tree)))
    (assert (eq result tree))
    (assert (eq (car result) child))
    (assert (eq (cdr result) tail))
    (assert (equal tree '((:new) :keep . :new))))
  (let ((old (list :old)) (new (list :replacement)))
    (assert (eq (nsubst new old old) new)))
  (assert (eq (nsubst :new :old :old) :new))
  (let* ((child (cons :alias :keep)) (tree (list child)))
    (assert (eq (nsubst :new :old tree
                       :key (lambda (x) (if (eq x :alias) :old x))) tree))
    (assert (eq (car tree) child))
    (assert (equal child '(:new . :keep))))
  (let ((tree (list (copy-seq "old") :keep)))
    (assert (eq (nsubst :new "OLD" tree :test #'equalp) tree))
    (assert (equal tree '(:new :keep))))
  (let ((tree (cons :keep :other)))
    (assert (eq (nsubst :new :keep tree
                       :test-not (lambda (a b) (or (consp b) (eql a b)))) tree))
    (assert (equal tree '(:keep . :new))))
  (assert (handler-case
              (progn (invoke-nsubst-options (list :test #'eql :test-not #'eql)) nil)
            (error () t)))
  (let* ((child (cons 2 3)) (tree (list 1 child 4)))
    (assert (eq (nsubst-if :odd (lambda (x) (and (integerp x) (oddp x))) tree) tree))
    (assert (eq (cadr tree) child))
    (assert (equal tree '(:odd (2 . :odd) 4))))
  (let* ((child (cons :alias :keep)) (tree (list child)))
    (assert (eq (nsubst-if-not :new (lambda (x) (not (eq x :old))) tree
                              :key (lambda (x) (if (eq x :alias) :old x))) tree))
    (assert (eq (car tree) child))
    (assert (equal child '(:new . :keep))))
  ;; Allocating key/test/predicate callbacks must keep the original graph live.
  (dotimes (iteration 8)
    (let* ((child (cons :old :keep)) (tree (list child :old)) (calls 0))
      (assert (eq (nsubst :new :old tree
                         :key (lambda (x)
                                (incf calls)
                                (let ((scratch (list x))) (car scratch)))
                         :test (lambda (a b)
                                 (let ((scratch (list a b)))
                                   (eql (car scratch) (cadr scratch))))) tree))
      (assert (> calls 0))
      (assert (eq (car tree) child))
      (assert (equal tree '((:new . :keep) :new))))
    (let* ((child (cons :old :keep)) (tree (list child)))
      (assert (eq (nsubst-if :new
                            (lambda (x) (let ((scratch (list x)))
                                          (eq (car scratch) :old))) tree) tree))
      (assert (eq (car tree) child))
      (assert (equal tree '((:new . :keep)))))
    (let* ((child (cons :old :keep)) (tree (list child)))
      (assert (eq (nsubst-if-not :new
                                (lambda (x) (let ((scratch (list x)))
                                              (not (eq (car scratch) :old)))) tree) tree))
      (assert (eq (car tree) child))
      (assert (equal tree '((:new . :keep))))))
  ;; The nondestructive neighbors must still leave retained input aliases alone.
  (dolist (function (list (lambda (tree) (subst :new :old tree))
                         (lambda (tree) (subst-if :new (lambda (x) (eq x :old)) tree))
                         (lambda (tree) (subst-if-not :new (lambda (x) (not (eq x :old))) tree))))
    (let* ((child (list :old)) (tree (list child)) (result (funcall function tree)))
      (assert (equal tree '((:old))))
      (assert (eq (car tree) child))
      (assert (equal result '((:new))))))
  t)
"#;
const CHECKS: &str = "(assert (check-nsubst-aliases)) (write-line \"NSUBST-ALIAS-CHECKS-PASSED\")";

fn run(program: &str, stress: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("NSUBST-ALIAS-CHECKS-PASSED"), "{stdout}");
    stdout
}

#[test]
fn destructive_substitution_preserves_retained_tree_aliases() {
    let program = format!("{DEFINITIONS} {CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn destructive_substitution_preserves_aliases_in_source_free_calls() {
    let directory = std::env::temp_dir().join(format!("egcl-nsubst-alias-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("alias.lisp");
    let artifact = directory.join("alias.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?}) (declare (ignore warnings)) (assert file) (assert (not failure))) (write-line \"NSUBST-ALIAS-CHECKS-PASSED\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..count {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            assert_ne!(kind, 11, "unexpected embedded source fallback");
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "expected portable bytecode");
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
