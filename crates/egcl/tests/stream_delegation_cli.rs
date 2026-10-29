// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defvar *delegation-target*)
(defun delegation-error-p (thunk) (handler-case (progn (funcall thunk) nil) (error () t)))
(declaim (notinline native-delegation-write native-delegation-read))
(defun native-delegation-write (stream)
  (write-char #\a stream) (write-string "bc" stream) (write-sequence "de" stream)
  (terpri stream)
  (let ((first (fresh-line stream)))
    (write-char #\f stream)
    (let ((second (fresh-line stream)))
      (finish-output stream) (force-output stream) (list first second))))
(defun native-delegation-read (stream) (read-char stream))
(let ((alias (make-synonym-stream '*delegation-target*)))
  ;; The alias is older than every dynamically selected ordinary leaf.
  (let ((*delegation-target* (make-string-output-stream)))
    (dotimes (i 80)
      (native-delegation-write alias)
      (let ((*delegation-target* (make-string-input-stream "x"))) (native-delegation-read alias)))
    (when *delegation-required-tier*
      (loop repeat 100 until (and (= *delegation-required-tier* (egcl-ext:function-tier 'native-delegation-write))
                                 (= *delegation-required-tier* (egcl-ext:function-tier 'native-delegation-read)))
            do (native-delegation-write alias)
               (let ((*delegation-target* (make-string-input-stream "x"))) (native-delegation-read alias))
               (sleep 0.01))
      (dolist (name '(native-delegation-write native-delegation-read))
        (assert (= *delegation-required-tier* (egcl-ext:function-tier name)))))
    (get-output-stream-string *delegation-target*)
    (assert (output-stream-p alias)) (assert (not (input-stream-p alias)))
    (assert (eq (stream-element-type alias) 'character))
    (assert (equal (native-delegation-write alias) '(nil t)))
    (assert (equal (get-output-stream-string *delegation-target*) (format nil "abcde~%f~%")))
    (write-string "discard" alias) (clear-output alias)
    (assert (equal (get-output-stream-string *delegation-target*) ""))
    (let ((*delegation-target* (make-string-input-stream (format nil "abc~%rest"))))
      (assert (input-stream-p alias)) (assert (not (output-stream-p alias)))
      (assert (char= (peek-char nil alias) #\a))
      (assert (char= (native-delegation-read alias) #\a))
      (unread-char #\a alias) (assert (char= (read-char-no-hang alias) #\a))
      (assert (char= (read-char alias) #\b))
      (assert (equal (read-line alias) "c"))
      (let ((buffer (make-string 4)))
        (assert (= (read-sequence buffer alias) 4)) (assert (equal buffer "rest")))
      (assert (eq (read-char alias nil :end) :end))
      (assert (file-position alias 0))
      (assert (= (file-position alias) 0))
      (assert (char= (read-char alias) #\a)))
    (assert (typep alias 'synonym-stream))
    (assert (eq (synonym-stream-symbol alias) '*delegation-target*))
    (assert (delegation-error-p (lambda () (get-output-stream-string alias))))
    (with-open-file (stream *delegation-binary-path* :direction :output
                            :element-type '(unsigned-byte 8) :if-exists :supersede
                            :if-does-not-exist :create)
      (let ((*delegation-target* stream))
        (assert (equal (stream-element-type alias) '(unsigned-byte 8)))
        (write-byte 65 alias) (write-sequence #(66 67) alias)
        (finish-output alias) (force-output alias)
        (assert (= (file-position alias) 3)) (assert (= (file-length alias) 3))))
    (with-open-file (stream *delegation-binary-path* :direction :input
                            :element-type '(unsigned-byte 8))
      (let ((*delegation-target* stream))
        (assert (= (read-byte alias) 65))
        (let ((buffer (make-array 2 :element-type '(unsigned-byte 8))))
          (assert (= (read-sequence buffer alias) 2))
          (assert (equalp buffer #(66 67))))
        (assert (file-position alias 0)) (assert (= (read-byte alias) 65))))
    (let ((*delegation-target* alias))
      (assert (delegation-error-p (lambda () (read-char alias))))
      (assert (not (input-stream-p alias))) (assert (not (output-stream-p alias)))
      (assert (delegation-error-p (lambda () (write-char #\z alias)))))
    (close alias)
    (assert (open-stream-p *delegation-target*))
    (assert (delegation-error-p (lambda () (write-char #\z alias))))
    (write-char #\q *delegation-target*)))
;; A valid long direct-synonym chain has no arbitrary depth cutoff.
(let* ((leaf (make-string-output-stream)) (chain leaf) (bindings nil))
  (dotimes (i 96)
    (let ((symbol (gensym "STREAM-CHAIN")))
      (set symbol chain) (push symbol bindings)
      (setq chain (make-synonym-stream symbol))))
  (assert (output-stream-p chain)) (assert (not (input-stream-p chain)))
  (assert (eq (stream-element-type chain) 'character))
  (write-string "chain" chain)
  (assert (equal (get-output-stream-string leaf) "chain"))
  (dolist (symbol bindings) (makunbound symbol)))
(write-line "STREAM-DELEGATION-OK")
"##;

#[test]
fn direct_synonyms_resolve_newer_targets_before_io_and_state_queries() {
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let binary_path = std::env::temp_dir().join(format!(
            "egcl-direct-synonym-{}-{tier}.bin",
            std::process::id()
        ));
        let program = format!(
            "(defparameter *delegation-required-tier* {required})(defparameter *delegation-binary-path* {binary_path:?}){PROGRAM}"
        );
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier} stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "STREAM-DELEGATION-OK")
                .count(),
            1
        );
        std::fs::remove_file(binary_path).unwrap();
    }
}
