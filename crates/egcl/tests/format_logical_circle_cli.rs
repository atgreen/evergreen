// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline logical-circle-format logical-circle-formatter))
(defun logical-circle-format (control object) (format nil control object))
(defparameter *logical-circle-compiled-formatter*
  (compile nil (formatter "~:<~@{~A~^ ~}~:>")))
(assert (compiled-function-p *logical-circle-compiled-formatter*))
(defun logical-circle-formatter (object)
  (format nil *logical-circle-compiled-formatter* object))
(defun logical-circle-reference (object)
  (with-output-to-string (stream)
    (pprint-logical-block (stream object :prefix "(" :suffix ")")
      (pprint-exit-if-list-exhausted)
      (loop (write (pprint-pop) :stream stream :escape nil)
            (pprint-exit-if-list-exhausted)
            (write-char #\Space stream)))))
(dotimes (i 80)
  (logical-circle-format "~:<~@{~A~^ ~}~:>" '(0 1))
  (logical-circle-formatter '(0 1)))
(when *logical-circle-required-tier*
  (dolist (name '(logical-circle-format logical-circle-formatter))
    (loop repeat 100 until (= (egcl-ext:function-tier name) *logical-circle-required-tier*)
          do (logical-circle-format "~:<~A~:>" '(0))
             (logical-circle-formatter '(0)) (sleep 0.01))
    (assert (= (egcl-ext:function-tier name) *logical-circle-required-tier*)))
  ;; This asserts the actual compiled FORMATTER leaf, not only its caller.
  (loop repeat 100 until (= (egcl-ext:function-tier *logical-circle-compiled-formatter*)
                            *logical-circle-required-tier*)
        do (logical-circle-formatter '(0)) (sleep 0.01))
  (assert (= (egcl-ext:function-tier *logical-circle-compiled-formatter*)
             *logical-circle-required-tier*)))
(defun check-logical-circle (object expected)
  (assert (equal (logical-circle-format "~:<~@{~A~^ ~}~:>" object) expected))
  (assert (equal (logical-circle-formatter object) expected))
  (assert (equal (logical-circle-reference object) expected))
  (assert (equal (with-output-to-string (stream)
                   (format stream "~:<~@{~A~^ ~}~:>" object)) expected)))
(with-standard-io-syntax
  (let ((*print-circle* t) (*print-readably* nil) (*print-pretty* t)
        (*print-escape* nil) (*print-right-margin* 1000))
    ;; The reporter's shared tail is a cons identity, not another list element.
    (let* ((tail (list 0)) (object (cons tail tail)))
      (check-logical-circle object "(#1=(0) . #1#)")
      (let ((copy (read-from-string (logical-circle-formatter object))))
        (assert (eq (car copy) (cdr copy))))
      (let ((*print-length* 1)) (check-logical-circle object "((0) ...)")))
    ;; An already-labelled root must be marked before its prefix/body executes.
    (let ((object (list 0)))
      (setf (cdr object) object)
      (check-logical-circle object "#1=(0 . #1#)")
      (let ((copy (read-from-string (logical-circle-formatter object))))
        (assert (eq copy (cdr copy))))
      (let ((*print-length* 500)) (check-logical-circle object "#1=(0 . #1#)"))
      (let ((*print-length* 1)) (check-logical-circle object "(0 ...)"))
      (let ((*print-length* 0)) (check-logical-circle object "(...)")))
    (check-logical-circle '(a . b) "(A . B)")
    (check-logical-circle nil "()")
    (check-logical-circle 7 "7")
    (let ((shared (list 0)))
      (assert (equal (logical-circle-format "~:<~:<~A~:> ~:<~A~:>~:>" (list shared shared))
                     "(#1=(0) #1#)")))
    (let ((*print-level* 0)) (check-logical-circle '(0 1) "#"))
    ;; A normally terminating finite control need not consume the full list.
    (assert (equal (logical-circle-format "~:<~A~:>" '(0 1 2)) "(0)"))
    (assert (equal (logical-circle-format "~:<~A~0^unreachable~:>" '(0 1)) "(0)"))
    (assert (equal (logical-circle-format "~:<~A~^ ~A~:>" '(0)) "(0)"))
    (assert (equal (logical-circle-format "~:<~:[~A~;~A~]~:>" '(nil 42)) "(42)"))
    (assert (equal (logical-circle-format "~:<~@[~A~]~:>" '(nil)) "()"))
    (let ((*print-length* 1))
      ;; The truth test POPs once; the true clause reuses that positional arg
      ;; but its own extraction is the second logical POP and is abbreviated.
      (assert (equal (logical-circle-format "~:<~@[~A~]~:>" '(7)) "(...)")))
    (let ((*print-length* 2))
      (assert (equal (logical-circle-format "~:<~@[~A~]~:>" '(7)) "(7)")))
    ;; V consumes an argument before the value; ordinary ~? uses its own
    ;; argument list while ~@? uses the remaining enclosing argument values.
    (assert (equal (logical-circle-format "~:<~vA~:>" '(3 "x")) "(x  )"))
    (let ((*print-length* 1))
      ;; The colon pluralization reuses its prior argument without POPping
      ;; again; saved SBCL directive source expands it as a prior-value read.
      (assert (equal (logical-circle-format "~:<~D item~:P~:>" '(2)) "(2 items)")))
    (assert (handler-case
                (progn (logical-circle-format "~:<~:P~:>" '(2)) nil)
              (control-error () t)))
    (assert (equal (logical-circle-format "~:<~?~:>" '("~A" (42))) "(42)"))
    (assert (equal (logical-circle-format "~:<~@?~:>" '("~A" 42)) "(42)"))))
(with-standard-io-syntax
  (let ((*print-circle* t) (*print-readably* nil) (*print-pretty* t)
        (*print-length* 1) (*print-right-margin* 1000))
    ;; # observes all remaining arguments, without counting as a logical POP.
    (assert (equal (logical-circle-format "~:<~#[zero~;one~;two~:;many~]~A~:>" '(1 2 3))
                   "(many1)")))
  (let ((object (list 0)) (*print-circle* t) (*print-readably* nil))
    (setf (cdr object) object)
    ;; A nonfinite count has a deliberate bounded formatter failure; this is
    ;; separate from normal circular POP, which the earlier controls render.
    (assert (handler-case
                (progn (logical-circle-format "~:<~#D~:>" object) nil)
              (control-error () t)))))
(defclass logical-circle-mutation ()
  ((action :initarg :action :reader logical-circle-action)))
(defmethod print-object ((object logical-circle-mutation) stream)
  (funcall (logical-circle-action object))
  (write-string "M" stream))
(with-standard-io-syntax
  (let ((*print-circle* t) (*print-readably* nil) (*print-pretty* t)
        (*print-right-margin* 1000))
    (let* ((tail (list 1))
           (object (make-instance 'logical-circle-mutation
                     :action (lambda () (setf (car tail) 99)))))
      ;; The next CAR is read after the callback, rather than from a flattening
      ;; snapshot or stale checkpoint cache.
      (assert (equal (logical-circle-format "~:<~@{~A~^ ~}~:>" (cons object tail)) "(M 99)")))
    (let* ((source (list nil 1))
           (object (make-instance 'logical-circle-mutation
                     :action (lambda () (setf (cdr source) (list 404))))))
      (setf (car source) object)
      ;; POP saves its old CDR before invoking user code. Replacing that link
      ;; does not retroactively change the continuation already held by POP.
      (assert (equal (logical-circle-format "~:<~@{~A~^ ~}~:>" source) "(M 1)")))))
(let ((control (compile nil (formatter "~A~?"))))
  (with-output-to-string (stream)
    (assert (equal (funcall control stream 1 "~A" '(2) 3) '(3)))
    (assert (equal (get-output-stream-string stream) "12"))))
(with-standard-io-syntax
  (let* ((*print-pretty* t) (*print-readably* nil) (*print-right-margin* 6)
         (raw "~:<~:<a b c~:@>~:@>")
         (formatter (formatter "~:<~:<a b c~:@>~:@>"))
         (compiled (compile nil formatter))
         (expected (format nil raw '(nil))))
    ;; Raw constructor controls are normalized ONCE per independent entry.
    ;; Preserve nested875 context and792 fill multiplicity in both call paths.
    (assert (equal (format nil formatter '(nil)) expected))
    (assert (equal (format nil compiled '(nil)) expected))
    (assert (equal (format nil compiled '(nil)) expected))))
(defun logical-circle-column (stream object colon at &rest parameters)
  (declare (ignore object colon at parameters))
  (format stream "[~D]" (egcl-gray-streams:stream-line-column stream)))
(with-standard-io-syntax
  (let* ((*print-pretty* t) (*print-readably* nil) (*print-right-margin* 100)
         (function (formatter "A~:@_~/logical-circle-column/")))
    ;; A bare mandatory pretty newline is inert at a plain root. The callback
    ;; observes column1, not the speculative logical-body reset to column0.
    (assert (equal (format nil "~@?" function nil) "A[1]"))
    (assert (equal (format nil "~?" function '(nil)) "A[1]"))
    (assert (equal (format nil "~{~}" function '(nil)) "A[1]"))
    ;; Conversely an actual logical body's context must survive indirection.
    (assert (equal (logical-circle-format "~:<~@?~:>" (list function nil))
                   (logical-circle-format "~:<A~:@_~/logical-circle-column/~:>" '(nil))))))
(let ((values (list 0)))
  (setf (cdr values) values)
  ;; A finite iteration bound requires only its actually consumed CARs. It
  ;; neither asks for a total count nor insists on reaching a proper-list end.
  (assert (equal (format nil "~1{~A~}" values) "0"))
  (assert (equal (logical-circle-format "~:<~1{~A~}~:>" (list values)) "(0)")))
(assert (equal (format nil "~1{~A~}" '(0 . 99)) "0"))
(let ((sublists (list (list 7))))
  (setf (cdr sublists) sublists)
  (assert (equal (format nil "~1:{~A~}" sublists) "7")))
(with-standard-io-syntax
  (let ((*print-pretty* t) (*print-readably* nil) (*print-right-margin* 100))
    ;; Opening-@ consumes its continuation even when a dotted atom skips BODY.
    (assert (equal (logical-circle-format "~:<~A~@<ignored~:>~^BAD~:>" '(1 . 2)) "(12)"))
    ;; The colon exit belongs to the enclosing colon iteration, not its nested
    ;; logical/case collector. Already emitted body and suffix survive it.
    (assert (equal (logical-circle-format "~:{~<~A~:^~:>Z~}Q" '(((1)) ((2)))) "1Z2Q"))
    (assert (equal (logical-circle-format "~:{~<~(~A~:^~)~:>Z~}Q" '((("A")) (("B")))) "aZbQ"))
    ;; String indirection inherits the current eligible colon owner.
    (assert (equal (logical-circle-format "~:{~?Z~}Q" '(("~A~:^" (1)) ("~A~:^" (2)))) "1Z2Q"))
    (assert (equal (logical-circle-format "~:{~@?Z~}Q" '(("~A~:^" 1) ("~A~:^" 2))) "1Z2Q"))
    ;; An inner colon loop catches only its OWN token, leaving the outer loop
    ;; free to print its following Z and continue to its next sublist.
    (assert (equal (logical-circle-format "~:{~:{~A~:^X~}Z~}Q" '((((1))) (((2))))) "1Z2ZQ"))
    (let ((function (compile nil (formatter "~:{~A~:^X~}Q"))))
      (assert (compiled-function-p function))
      (assert (equal (format nil function '((1) (2))) "1X2Q"))
      ;; The function's own colon target is caught inside its private entry;
      ;; the caller's following colon exit still belongs to the outer loop.
      (assert (equal (logical-circle-format "~:{~?~:^Z~}R"
                        (list (list function (list '((1) (2)))))) "1X2QR")))
    ;; A foreign nonlocal exit discards the unfinished justification collector.
    (assert (equal (logical-circle-format "~:{~<~A~;~A~:^~>Z~}Q" '((1 2) (3 4))) "12ZQ"))
    ;; The current NONcolon iteration masks any enclosing colon eligibility.
    (assert (handler-case
                (progn (logical-circle-format "~:{~{~A~:^~}Z~}" '(((1)))) nil)
              (control-error () t)))
    (assert (handler-case
                (progn (logical-circle-format "~:{~{~}Z~}" '(("~A~:^" (1)))) nil)
              (control-error () t)))
    ;; An independently constructed FORMATTER cannot borrow a caller's token.
    (assert (handler-case (progn (formatter "~v:^") nil) (control-error () t)))))
(defun logical-circle-independent-valid (stream object colon at &rest parameters)
  (declare (ignore colon at parameters))
  (format stream "~A" object))
(defun logical-circle-independent-invalid (stream object colon at &rest parameters)
  (declare (ignore object colon at parameters))
  (format stream "~v:^" 0))
(assert (equal (logical-circle-format "~:{~/logical-circle-independent-valid/~:^Z~}Q" '((1) (2))) "1Z2Q"))
(assert (handler-case
            (progn (logical-circle-format "~:{~/logical-circle-independent-invalid/~}" '((nil))) nil)
          (control-error () t)))
;; The invalid nested public call must restore the enclosing operation state.
(assert (equal (logical-circle-format "~:{~<~A~:^~:>Z~}Q" '(((1)) ((2)))) "1Z2Q"))
(declaim (notinline logical-circle-setf-function))
(defun logical-circle-setf-function () #'(setf logical-circle-formatter-setf-alias))
(let* ((name '(setf logical-circle-formatter-setf-alias))
       (old (formatter "~:<~@{~A~^ ~}~:>")))
  (unwind-protect
      (progn
        ;; Explicit-definition compilation installs a SETF function name via
        ;; the same authoritative cell path as SETF FDEFINITION.
        (multiple-value-bind (result warnings failure) (compile name old)
          (assert (equal result name))
          (assert (null warnings))
          (assert (null failure)))
        (assert (compiled-function-p (fdefinition name)))
        (dotimes (i 80) (logical-circle-setf-function))
        (when *logical-circle-required-tier*
          (loop repeat 100 until (= *logical-circle-required-tier*
                                   (egcl-ext:function-tier 'logical-circle-setf-function))
                do (logical-circle-setf-function) (sleep 0.01))
          (assert (= *logical-circle-required-tier*
                     (egcl-ext:function-tier 'logical-circle-setf-function))))
        (assert (eq (logical-circle-setf-function) (fdefinition name)))
        (let ((first (fdefinition name)))
          ;; Named recompilation must LOOK UP the owned formatter through
          ;; FDEFINITION, then publish the new exact-self descriptor correctly.
          (multiple-value-bind (result warnings failure) (compile name)
            (assert (equal result name))
            (assert (null warnings))
            (assert (null failure)))
          (assert (compiled-function-p (fdefinition name)))
          (assert (eq (logical-circle-setf-function) (fdefinition name)))
          (assert (eq #'(setf logical-circle-formatter-setf-alias) (fdefinition name)))
          (with-standard-io-syntax
            (let* ((*print-circle* t) (*print-pretty* t) (*print-readably* nil)
                   (*print-escape* nil) (*print-right-margin* 1000)
                   (tail (list 0)) (object (cons tail tail)))
              ;; Retained old and recompiled functions keep independent protocol
              ;; ownership, while FUNCTION/FDEFINITION/FUNCALL agree on the cell.
              (assert (equal (format nil old object) "(#1=(0) . #1#)"))
              (assert (equal (format nil first object) "(#1=(0) . #1#)"))
              (assert (equal (format nil (fdefinition name) object) "(#1=(0) . #1#)"))
              (assert (equal (with-output-to-string (stream)
                               (funcall #'(setf logical-circle-formatter-setf-alias) stream object))
                             "(#1=(0) . #1#)"))))))
    (fmakunbound name)))
(defclass logical-circle-transfer () ())
(defmethod print-object ((object logical-circle-transfer) stream)
  (declare (ignore stream))
  (copy-seq "allocate before nonlocal transfer")
  (throw 'logical-circle-exit :escaped))
(assert (eq (catch 'logical-circle-exit
              (logical-circle-format "~:<~A~:>" (list (make-instance 'logical-circle-transfer))))
            :escaped))
;; Cleanup must restore graph ownership and label numbering after the callback.
(let ((*print-readably* nil) (*print-circle* t) (*print-pretty* t)
      (*print-right-margin* 1000) (object (list 0)))
  (setf (cdr object) object)
  (check-logical-circle object "#1=(0 . #1#)"))
(write-line "FORMAT-LOGICAL-CIRCLE-OK")
"##;

#[test]
fn direct_logical_blocks_preserve_shared_tails_and_circular_list_identity() {
    for (tier, stress, required) in [
        ("interp", false, "nil"),
        ("t1", false, "1"),
        ("t2", false, "2"),
        ("t2", true, "2"),
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
        let program = format!("(defparameter *logical-circle-required-tier* {required}){PROGRAM}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}/{stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| *line == "FORMAT-LOGICAL-CIRCLE-OK")
                .count(),
            1
        );
    }
}

#[test]
fn formatter_owned_self_descriptor_survives_source_free_loading_and_fresh_core() {
    let dir = std::env::temp_dir().join(format!("egcl-formatter-self-core-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("formatter.lisp");
    let fasl = dir.join("formatter.fasl");
    let image = dir.join("formatter.core");
    std::fs::write(
        &source,
        "(defparameter *saved-circle-formatter* (compile nil (formatter \"~@{~A~^ ~}\")))\n",
    )
    .unwrap();
    let invoke = |program: &str, core: Option<&std::path::Path>, tier: &str| {
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
        if let Some(core) = core {
            command.arg("--image").arg(core);
        }
        command
            .args(["--no-init", "--eval", program])
            .output()
            .unwrap()
    };
    let compiled = invoke(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        None,
        "interp",
    );
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    std::fs::remove_file(&source).unwrap();
    let saved = invoke(
        &format!("(load {fasl:?})(egcl-ext:save-lisp-and-die {image:?})"),
        None,
        "interp",
    );
    assert!(
        saved.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&saved.stdout),
        String::from_utf8_lossy(&saved.stderr)
    );
    std::fs::remove_file(&fasl).unwrap();
    let checks = r##"
(assert (functionp *saved-circle-formatter*))
(assert (not (eq (formatter "~A") (formatter "~A"))))
(let* ((old *saved-circle-formatter*)
       (compiled (compile nil old)))
  (assert (compiled-function-p compiled))
  (assert (equal (format nil old 7) "7"))
  (assert (equal (format nil compiled 7) "7"))
  (setf *saved-circle-formatter* compiled))
(let ((name (gensym "SAVED-FORMATTER-ALIAS")))
  (unwind-protect
      (progn
        (setf (fdefinition name) *saved-circle-formatter*)
        (assert (eq (compile name) name))
        (assert (compiled-function-p (fdefinition name)))
        (assert (equal (format nil *saved-circle-formatter* 7) "7"))
        (assert (equal (format nil (fdefinition name) 7) "7"))
        (setf *saved-circle-formatter* (fdefinition name)))
    (fmakunbound name)))
(declaim (notinline restored-logical-formatter))
(defun restored-logical-formatter (object)
  (format nil "~:<~@?~:>" object))
(defun restored-logical-reference (object)
  (with-output-to-string (stream)
    (pprint-logical-block (stream object :prefix "(" :suffix ")")
      (pprint-exit-if-list-exhausted)
      (pprint-pop) ; control-function consumption is itself one logical POP
      (loop (pprint-exit-if-list-exhausted)
            (write (pprint-pop) :stream stream :escape nil)
            (pprint-exit-if-list-exhausted)
            (write-char #\Space stream)))))
(dotimes (i 80)
  (restored-logical-formatter (list *saved-circle-formatter* 7))
  (assert (equal (format nil *saved-circle-formatter* 7) "7")))
(when *restored-required-tier*
  (loop repeat 100 until (= *restored-required-tier* (egcl-ext:function-tier 'restored-logical-formatter))
        do (restored-logical-formatter (list *saved-circle-formatter* 7)) (sleep 0.01))
  (assert (= *restored-required-tier* (egcl-ext:function-tier 'restored-logical-formatter)))
  (loop repeat 100 until (= *restored-required-tier* (egcl-ext:function-tier *saved-circle-formatter*))
        do (format nil *saved-circle-formatter* 7) (sleep 0.01))
  (assert (= *restored-required-tier* (egcl-ext:function-tier *saved-circle-formatter*))))
(with-standard-io-syntax
  (let ((*print-circle* t) (*print-readably* nil) (*print-pretty* t)
        (*print-escape* nil) (*print-right-margin* 1000))
    ;; The restored constructor callable is recognized before finite APPLY
    ;; transport, so a circular continuation reaches ordinary lazy POP.
    (let ((tail (list 0)))
      (setf (cdr tail) tail)
      (let ((object (cons *saved-circle-formatter* tail)))
        (assert (equal (restored-logical-formatter object)
                       (restored-logical-reference object)))))
    (let ((*print-length* 1))
      (assert (equal (restored-logical-formatter (list *saved-circle-formatter* 7)) "(...)")))
    ;; An unrelated formatter called FIRST by an arbitrary function control
    ;; must not steal that function's scoped argument continuation.
    (let* ((other (formatter "~A"))
           (wrapper (lambda (stream &rest args)
                      (funcall other stream "noise")
                      (apply *saved-circle-formatter* stream args))))
      (assert (equal (restored-logical-formatter (list wrapper 7)) "(noise7)")))))
(write-line "FORMATTER-SELF-CORE-OK")
"##;
    for tier in ["interp", "t2"] {
        let required = if tier == "t2" { "2" } else { "nil" };
        let output = invoke(
            &format!("(defparameter *restored-required-tier* {required}){checks}"),
            Some(&image),
            tier,
        );
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| *line == "FORMATTER-SELF-CORE-OK")
                .count(),
            1
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
