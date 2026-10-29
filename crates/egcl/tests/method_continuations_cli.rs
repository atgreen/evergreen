// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(defmacro generated-next-functions ()
  '(list #'cl:call-next-method #'cl:next-method-p))
(defmacro generated-next-direct () '(cl:call-next-method))
(defmacro generated-next-lambda () '(lambda () (cl:call-next-method)))
(defgeneric global-macro-functions (x))
(defmethod global-macro-functions ((x t)) (values x :global-functions))
(defmethod global-macro-functions ((x integer)) (generated-next-functions))
(let ((functions (global-macro-functions 61)))
  (assert (funcall (second functions)))
  (assert (equal (multiple-value-list (funcall (first functions))) '(61 :global-functions))))
(defgeneric global-macro-direct (x))
(defmethod global-macro-direct ((x t)) (values x :global-direct))
(defmethod global-macro-direct ((x integer)) (generated-next-direct))
(assert (equal (multiple-value-list (global-macro-direct 62)) '(62 :global-direct)))
(defgeneric global-macro-lambda (x))
(defmethod global-macro-lambda ((x t)) (values x :global-lambda))
(defmethod global-macro-lambda ((x integer)) (generated-next-lambda))
(let ((function (global-macro-lambda 63)))
  (assert (equal (multiple-value-list (funcall function)) '(63 :global-lambda))))
;; These MACROLET names, rather than a literal next-method reference, are the
;; complete method bodies. The returned functions outlive the defining scope.
(defgeneric local-macro-functions (x))
(defmethod local-macro-functions ((x t)) (values x :local-functions))
(defgeneric local-macro-direct (x))
(defmethod local-macro-direct ((x t)) (values x :local-direct))
(defgeneric local-macro-lambda (x))
(defmethod local-macro-lambda ((x t)) (values x :local-lambda))
(macrolet ((local-functions () '(list #'cl:call-next-method #'cl:next-method-p))
           (local-direct () '(cl:call-next-method))
           (local-lambda () '(lambda () (cl:call-next-method))))
  (defmethod local-macro-functions ((x integer)) (local-functions))
  (defmethod local-macro-direct ((x integer)) (local-direct))
  (defmethod local-macro-lambda ((x integer)) (local-lambda)))
(let ((functions (local-macro-functions 71)))
  (assert (funcall (second functions)))
  (assert (equal (multiple-value-list (funcall (first functions))) '(71 :local-functions))))
(assert (equal (multiple-value-list (local-macro-direct 72)) '(72 :local-direct)))
(let ((function (local-macro-lambda 73)))
  (assert (equal (multiple-value-list (funcall function)) '(73 :local-lambda))))
(defvar *saved-next*)
(defvar *saved-next-p*)
(defvar *method-effects* nil)
(defgeneric retained-next (x &optional y))
(defmethod retained-next ((x t) &optional y) (values x y :base))
(defmethod retained-next ((x integer) &optional y)
  (let ((original x))
    (setq x 999 y :modified)
    (setf *saved-next* #'call-next-method *saved-next-p* #'next-method-p)
    ;; No arguments means entry arguments, not the modified parameter values.
    (assert (equal (multiple-value-list (call-next-method)) (list original nil :base)))
    (values *saved-next* *saved-next-p*)))
(retained-next 7)
(assert (functionp *saved-next*))
(assert (funcall *saved-next-p*))
(assert (equal (multiple-value-list (funcall *saved-next*)) '(7 nil :base)))
(assert (equal (multiple-value-list (funcall *saved-next* 8 :new)) '(8 :new :base)))
(assert (handler-case (progn (funcall *saved-next-p* 1) nil) (program-error () t)))
;; Another invocation and a replacement method cannot retarget an escaped
;; continuation's original method functions, entry arguments or candidate set.
(let ((old *saved-next*))
  (retained-next 19)
  (defmethod retained-next ((x t) &optional y) (declare (ignore x y)) :replacement)
  (assert (equal (multiple-value-list (funcall old)) '(7 nil :base))))

(defgeneric changed-next (x))
(defmethod changed-next ((x t)) :base)
(defmethod changed-next :before ((x (eql 0))) (push :before *method-effects*))
(defmethod changed-next ((x integer)) (call-next-method 1))
;; The primary chain stays INTEGER,T, but removing a BEFORE method changes the
;; full applicable set. Its already-completed effect must occur exactly once.
(setf *method-effects* nil)
(assert (handler-case (progn (changed-next 0) nil) (error () t)))
(assert (equal *method-effects* '(:before)))
(assert (eq :base (changed-next 3)))
(defmethod changed-next ((x integer)) (call-next-method :not-an-integer))
(assert (handler-case (progn (changed-next 3) nil) (error () t)))

(defgeneric after-set-next (x))
(defmethod after-set-next ((x t)) :base)
(defmethod after-set-next :after ((x (eql 0))) (push :after *method-effects*))
(defmethod after-set-next ((x integer)) (call-next-method 1))
(setf *method-effects* nil)
(assert (handler-case (progn (after-set-next 0) nil) (error () t)))
(assert (null *method-effects*))
(assert (eq :base (after-set-next 2)))
(defgeneric precedence-next (left right &optional change)
  (:argument-precedence-order right left))
(defmethod precedence-next ((left integer) right &optional change)
  (declare (ignore change)) (list :left left right))
(defmethod precedence-next (left (right integer) &optional change)
  (declare (ignore change)) (list :right left right))
(defmethod precedence-next ((left integer) (right integer) &optional change)
  (call-next-method (if change :changed left) right change))
(assert (equal (precedence-next 10 20) '(:right 10 20)))
(assert (handler-case (progn (precedence-next 10 20 t) nil) (error () t)))
(defvar *reentrant-saved*)
(defgeneric reentrant-next (x))
(defmethod reentrant-next ((x t)) (values x :reentrant-base))
(defmethod reentrant-next ((x integer))
  (let ((ours #'call-next-method))
    (when (= x 1) (assert (equal (multiple-value-list (reentrant-next 2)) '(2 :reentrant-base))))
    (setf *reentrant-saved* ours)
    (funcall ours)))
(assert (equal (multiple-value-list (reentrant-next 1)) '(1 :reentrant-base)))
(let ((outer *reentrant-saved*))
  (reentrant-next 3)
  (assert (equal (multiple-value-list (funcall outer)) '(1 :reentrant-base))))
(defvar *later-replaced* nil)
(defgeneric retire-after-prefix (x))
(defmethod retire-after-prefix (x) (values :old x))
(defparameter *retained-old-primary* (find-method #'retire-after-prefix nil (list (find-class 't))))
(defun replace-primary-after-prefix ()
  (unless *later-replaced*
    (setf *later-replaced* t)
    (remove-method #'retire-after-prefix *retained-old-primary*)
    (defmethod retire-after-prefix (x) (values :new x))))
(defmethod retire-after-prefix :before (x) (replace-primary-after-prefix))
(assert (equal (multiple-value-list (retire-after-prefix 81)) '(:old 81)))
(assert (equal (multiple-value-list (retire-after-prefix 82)) '(:new 82)))
;; Escaped old invocation data is independent of a new BEFORE method and its
;; replacement of the old primary; explicit changed args use the frozen set.
(defgeneric retained-through-prefix (x))
(defmethod retained-through-prefix ((x t)) (values x :old-retained))
(defmethod retained-through-prefix ((x integer)) #'call-next-method)
(let ((old (retained-through-prefix 91)))
  (defmethod retained-through-prefix :before ((x integer))
    (defmethod retained-through-prefix ((x t)) (values x :new-retained)))
  (let ((during (retained-through-prefix 92)))
    (assert (equal (multiple-value-list (funcall during)) '(92 :old-retained))))
  (assert (equal (multiple-value-list (funcall old 93)) '(93 :old-retained))))
(defgeneric terminal-next (x))
(defmethod no-next-method ((gf (eql #'terminal-next)) method &rest args)
  (declare (ignore method)) (values :no-next args))
(defmethod terminal-next (x)
  (assert (not (next-method-p)))
  (setf *saved-next* #'call-next-method *saved-next-p* #'next-method-p)
  (call-next-method))
(assert (equal (multiple-value-list (terminal-next 31)) '(:no-next (31))))
(assert (not (funcall *saved-next-p*)))
(assert (equal (multiple-value-list (funcall *saved-next*)) '(:no-next (31))))
(defgeneric entry-arguments (x))
(defmethod entry-arguments ((x t)) (values x :entry))
(defmethod entry-arguments ((x integer))
  (setq x 800)
  (call-next-method))
(defmethod entry-arguments :around ((x integer))
  (setq x 900)
  (call-next-method 11))
(assert (equal (multiple-value-list (entry-arguments 3)) '(11 :entry)))
(defgeneric zero-next (x))
(defmethod zero-next ((x t)) (declare (ignore x)) (values))
(defmethod zero-next ((x integer)) (call-next-method))
(assert (null (multiple-value-list (zero-next 3))))
(defgeneric keyword-next (x &key))
(defmethod keyword-next ((x t) &key (answer 17)) (values x answer))
(defmethod keyword-next ((x integer) &key)
  (call-next-method x :answer 23))
(assert (equal (multiple-value-list (keyword-next 7)) '(7 23)))
(defgeneric short-next (x) (:method-combination +))
(defmethod short-next + ((x t)) (declare (ignore x)) 1)
(defmethod short-next + ((x integer)) (declare (ignore x)) 2)
(defmethod short-next :around ((x integer))
  (setf *saved-next* #'call-next-method)
  (call-next-method))
(assert (= 3 (short-next 4)))
(assert (= 3 (funcall *saved-next* 5)))
(assert (handler-case (progn (funcall *saved-next* :changed) nil) (error () t)))
;; Restore the terminal continuation used by the native caller checks.
(terminal-next 31)
;; Lexical shadowing must win over any global/private helper shortcut.
(assert (eq :shadow (flet ((call-next-method () :shadow)) (call-next-method))))
(defun invoke-retained-next (function) (funcall function))
(format t "METHOD-CONTINUATIONS-OK~%")
"#;

const NATIVE: &str = r#"
(dotimes (i 120) (invoke-retained-next *saved-next*))
(loop repeat 200 until (= 2 (egcl-ext:function-tier 'invoke-retained-next))
  do (invoke-retained-next *saved-next*) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'invoke-retained-next)))
(assert (equal (multiple-value-list (invoke-retained-next *saved-next*)) '(:no-next (31))))
"#;

#[test]
fn lexical_next_method_functions_retain_invocation_and_validate_all_methods() {
    let mut reference = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let program = format!("{PROGRAM}\n{}", if tier == "t2" { NATIVE } else { "nil" });
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1");
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_T2_NO_QUEUE",
        ] {
            command.env_remove(key);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("method continuation regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("METHOD-CONTINUATIONS-OK"),
            "{stdout}\n{stderr}"
        );
        if tier == "t2" {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("INVOKE-RETAINED-NEXT")
                        && line.contains("optimized worker result installed with ")),
                "actual native caller installation required: {stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}

#[test]
fn escaped_next_method_context_survives_source_free_load_and_core() {
    let dir =
        std::env::temp_dir().join(format!("egcl-method-continuations-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("continuation.lisp");
    let fasl = dir.join("continuation.bfasl");
    let image = dir.join("continuation.core");
    std::fs::write(
        &source,
        r#"
(defclass continuation-class () ())
(defparameter *continuation-object* (make-instance 'continuation-class))
(defvar *persisted-next*)
(defvar *persisted-next-p*)
(defgeneric persistent-method (x))
(defmethod persistent-method (x) (values x :old-base))
(defmethod persistent-method ((x continuation-class))
  (setf *persisted-next* #'call-next-method *persisted-next-p* #'next-method-p))
(persistent-method *continuation-object*)
(defmethod persistent-method (x) (declare (ignore x)) :replacement)
"#,
    )
    .unwrap();
    let run = |program: String, core: Option<&std::path::Path>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.arg("--no-init").env("EGCL_FORCE_TIER", "interp");
        if let Some(core) = core {
            command.arg("--image").arg(core);
        }
        let output = command.args(["--eval", &program]).output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(
        format!("(compile-file {source:?} :output-file {fasl:?})"),
        None,
    );
    let bytes = std::fs::read(&fasl).unwrap();
    let unit = egcl_rt::bfasl::load(&bytes).unwrap();
    assert!(
        unit.section(egcl_rt::bfasl::section::TOPLEVEL_FORMS)
            .is_none()
    );
    assert!(
        unit.section(egcl_rt::bfasl::section::BYTECODE_UNIT)
            .unwrap()
            .starts_with(b"BBU")
    );
    std::fs::remove_file(&source).unwrap();
    run(
        format!("(load {fasl:?})(egcl-ext:save-lisp-and-die {image:?})"),
        None,
    );
    std::fs::remove_file(&fasl).unwrap();
    run(
        r#"
(assert (funcall *persisted-next-p*))
(multiple-value-bind (object marker) (funcall *persisted-next*)
  (assert (eq object *continuation-object*))
  (assert (eq marker :old-base)))
(assert (eq (class-of *continuation-object*) (find-class 'continuation-class)))
"#
        .into(),
        Some(&image),
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
