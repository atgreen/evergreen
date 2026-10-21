// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `EGCL-EXT:MAP-READTABLE-MACRO-CHARACTERS` (GitHub #473) reports exactly the
//! macro characters and dispatch sub-characters a grovel with
//! GET-MACRO-CHARACTER / GET-DISPATCH-MACRO-CHARACTER finds, for standard,
//! copied and modified readtables, without walking `CHAR-CODE-LIMIT` codes; and
//! named-readtables' merge built on it takes milliseconds rather than seconds.
use std::process::Command;

fn run(program: &str, marker: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && stdout.contains(marker),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// `ENUMERATE` collects the entries the export reports; `GROVEL` is
/// named-readtables' portable iterator bounded to the first #x3000 codes plus
/// the non-BMP characters a test installs; `CHECK` asserts they are EQUAL.
const EQUIVALENCE: &str = r##"
(defun enumerate (rt)
  (let ((out '()))
    (egcl-ext:map-readtable-macro-characters
     (lambda (char fn ntp alist) (push (list char fn ntp alist) out))
     rt)
    (nreverse out)))
(defun grovel (rt extra-chars)
  ;; named-readtables' portable iterator, bounded to the first #x3000 codes
  ;; plus EXTRA-CHARS (the non-BMP characters the test installs).
  (let ((chars (append (loop for code below #x3000
                             for c = (code-char code) when c collect c)
                       extra-chars))
        (out '()))
    (dolist (char chars (nreverse out))
      (multiple-value-bind (fn ntp) (get-macro-character char rt)
        (when fn
          (let ((alist (handler-case
                           (loop for sub in chars
                                 for f = (and (not (lower-case-p sub)) ; as non-lowercase-code-char
                                              (get-dispatch-macro-character char sub rt))
                                 when f collect (cons sub f))
                         (error () nil))))
            (push (list char fn ntp alist) out)))))))
(defun check (label rt extra)
  (let ((e (enumerate rt)) (g (grovel rt extra)))
    (unless (equal e g)
      (format t "MISMATCH ~A~% enum ~S~% grov ~S~%" label e g)
      (error "mismatch ~A" label))
    (format t "~A: ~D macro chars agree~%" label (length e))))
(defvar *astral* (code-char #x1F600))
(check "standard (nil)" nil '())
(check "copy-readtable nil" (copy-readtable nil) '())
(let ((rt (copy-readtable nil)))
  (set-macro-character #\! (lambda (s c) (declare (ignore s c)) :bang) t rt)
  (set-macro-character *astral* (lambda (s c) (declare (ignore s c)) :smile) nil rt)
  (set-macro-character #\% (lambda (s c) (declare (ignore s c)) :gone) nil rt)
  (set-macro-character #\% nil nil rt)                 ; deleted again
  (set-dispatch-macro-character #\# #\? (lambda (s c a) (declare (ignore s c a)) :interp) rt)
  (set-dispatch-macro-character #\# #\l (lambda (s c a) (declare (ignore s c a)) :l) rt)
  (make-dispatch-macro-character #\{ nil rt)
  (set-dispatch-macro-character #\{ #\x (lambda (s c a) (declare (ignore s c a)) :x) rt)
  (set-dispatch-macro-character #\{ *astral* (lambda (s c a) (declare (ignore s c a)) :y) rt)
  (set-syntax-from-char #\] #\) rt)
  (check "modified" rt (list *astral*))
  (check "copy of modified" (copy-readtable rt) (list *astral*))
  (assert (member #\{ (enumerate rt) :key #'first))
  (assert (not (member #\% (enumerate rt) :key #'first)))
  (assert (equal (mapcar #'car (fourth (assoc #\{ (enumerate rt)))) (list #\X *astral*)))
  (assert (eq :x (let ((*readtable* rt)) (read-from-string "{x"))))
  (assert (eq :x (let ((*readtable* rt))
                   (funcall (get-macro-character #\{ rt)
                            (make-string-input-stream "x") #\{)))))
"##;

#[test]
fn enumeration_matches_a_grovel_for_standard_copied_and_modified_readtables() {
    run(
        &format!("(progn {EQUIVALENCE} (format t \"EQUIVALENCE-OK~%\"))"),
        "EQUIVALENCE-OK",
    );
}

/// The `#+egcl` arm of named-readtables' `%make-readtable-iterator`
/// (src/cruft.lisp) built on the export: a closure yielding
/// `(values more? char reader-fn disp? alist)`.
const ITERATOR_ARM: &str = r##"
;;; The #+egcl arm a named-readtables port would add to src/cruft.lisp.
(defun egcl-readtable-iterator (readtable)
  (let ((entries '()))
    (egcl-ext:map-readtable-macro-characters
     (lambda (char reader-fn non-terminating-p alist)
       (declare (ignore non-terminating-p))
       (push (list char reader-fn
                   ;; A dispatching character with no sub-characters has a
                   ;; NIL alist; one probe tells it apart.
                   (or (and alist t)
                       (handler-case (progn (get-dispatch-macro-character char #\A readtable) t)
                         (error () nil)))
                   alist)
             entries))
     readtable)
    (setq entries (nreverse entries))
    (lambda ()
      (if entries
          (destructuring-bind (char reader-fn disp? alist) (pop entries)
            (values t char reader-fn disp? alist))
          (values nil nil nil nil nil)))))
"##;

/// named-readtables' `merge-readtables-into` loop (`merge-into` under
/// `do-readtable`, including its conflict check), driven by the arm above.
const MERGE: &str = r##"
(defun merge-into (to from)
  (let ((iter (egcl-readtable-iterator from)))
    (loop
      (multiple-value-bind (more? char reader-fn disp? table) (funcall iter)
        (unless more? (return to))
        (let ((non-terminating-p (nth-value 1 (get-macro-character char from))))
          (assert (get-macro-character char from))
          (let ((to-fn (get-macro-character char to)))
            (assert (or (null to-fn) (eq to-fn reader-fn))))
          (cond ((not disp?)
                 (set-macro-character char reader-fn non-terminating-p to))
                (t
                 (handler-case (get-dispatch-macro-character char #\A to)
                   (error () (make-dispatch-macro-character char non-terminating-p to)))
                 (loop for (subchar . subfn) in table do
                   (assert (get-dispatch-macro-character char subchar from))
                   (set-dispatch-macro-character char subchar subfn to)))))))))
"##;

#[test]
fn named_readtables_style_merge_takes_milliseconds() {
    let stdout = run(
        &format!(
            r##"(progn {ITERATOR_ARM} {MERGE}
      (let ((from (copy-readtable nil)))
        (set-macro-character #\! (lambda (s c) (declare (ignore s c)) :bang) nil from)
        (set-dispatch-macro-character #\# #\? (lambda (s c a) (declare (ignore s c a)) :interp) from)
        (make-dispatch-macro-character #\{{ t from)
        (set-dispatch-macro-character #\{{ #\x (lambda (s c a) (declare (ignore s c a)) :x) from)
        (merge-into (copy-readtable nil) from)
        (let* ((n 100)
               (start (get-internal-real-time))
               (merged (dotimes (i n (merge-into (copy-readtable nil) from))
                         (merge-into (copy-readtable nil) from)))
               (ms (/ (* 1000.0 (- (get-internal-real-time) start))
                      internal-time-units-per-second (1+ n))))
          (format t "MERGE-MS ~,3F~%" ms)
          ;; The portable grovel takes ~9 s per merge; allow a wide margin.
          (assert (< ms 250))
          (let ((*readtable* merged))
            (assert (equalp (read-from-string "(a #?y !b {{x \"s\" #(1) #+(or) z)")
                           '(a :interp y :bang b :x "s" #(1))))
            (assert (equal (read-from-string "#.(+ 1 2)") 3)))))
      (format t "MERGE-OK~%"))"##
        ),
        "MERGE-OK",
    );
    eprintln!(
        "{}",
        stdout
            .lines()
            .find(|l| l.starts_with("MERGE-MS"))
            .unwrap_or("")
    );
}
