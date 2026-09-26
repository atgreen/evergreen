//! Shared evaluated-argument kernels preserve sequence and CLOS semantics.
use std::process::Command;

fn run(program: &str, tier: &str) -> String {
    run_with_bootstrap(program, tier, false)
}

fn run_with_bootstrap(program: &str, tier: &str, bootstrap: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_torcl"));
    if !bootstrap {
        command.arg("--no-bootstrap");
    }
    let output = command
        .args(["--no-init", "--eval", program])
        .env("TORCL_FORCE_TIER", tier)
        .env("TORCL_T1_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{tier}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn sequence_and_clos_values_agree_across_tiers() {
    let program = r#"
      (defun check (value) (unless value (error "sequence/CLOS bridge check failed")))
      (defun join (a b) (append a b))
      (defun convert (x type) (coerce x type))
      (defun slice (x a b) (subseq x a b))
      (defclass bridge-box () ((item :initarg :item) (shared :allocation :class :initarg :shared)))
      (defclass bridge-child (bridge-box) ())
      (defmethod bridge-method ((x bridge-box) n) (values (slot-value x 'item) n))
      (defmethod bridge-method ((x bridge-child) n) (call-next-method))
      (defmethod bridge-method :around ((x bridge-child) n) (call-next-method x (+ n 1)))
      (defun store-item (x value) (setf (slot-value x 'item) value))
      (let ((x (make-instance 'bridge-child :item 1 :shared 9)))
        (dotimes (i 20)
          (let* ((head (list i (+ i 1))) (tail (list (+ i 2))) (result (join head tail)))
            (check (equal result (list i (+ i 1) (+ i 2))))
            (check (not (eq result head)))
            (check (eq (cdr (cdr result)) tail))
            (check (eq head (funcall #'append head)))
            (check (equal (apply 'append (list head :tail)) (cons i (cons (+ i 1) :tail)))))
          (check (equal "ab" (convert '(#\a #\b) 'string)))
          (check (equalp #(1 2) (funcall #'coerce '(1 2) 'vector)))
          (check (= 42 (funcall (convert '(lambda (x) (+ x 1)) 'function) 41)))
          (check (equal '(1 2) (funcall (convert 'join 'function) '(1) '(2))))
          (check (equal "bcd" (slice "abcde" 1 4)))
          (check (equal "bcde" (slice "abcde" 1 nil)))
          (check (equal '(2 3) (apply 'subseq (list '(1 2 3) 1 nil))))
          (check (equalp #(2 3) (funcall #'subseq #(1 2 3) 1 nil)))
          (check (handler-case (progn (slice "abc" 0.0 nil) nil)
                   (type-error () t)))
          (check (handler-case (progn (slice "abc" 0 1/2) nil)
                   (type-error () t)))
          (check (equal '(2 3) (apply 'subseq (list '(1 2 3 4) 1 3))))
          (check (equalp #(2 3) (funcall #'subseq #(1 2 3 4) 1 3)))
          (check (= i (store-item x i)))
          (check (equal (list i 5) (multiple-value-list (bridge-method x 4)))))
        (check (= 9 (slot-value x 'shared)))
        (check (= 10 (setf (slot-value x 'shared) 10)))
        (check (= 10 (slot-value (make-instance 'bridge-box) 'shared))))
      (let ((n 0))
        (check (equal '(1 2) (join (list (setq n (+ n 1))) (list (setq n (+ n 1))))))
        (check (= n 2))
        (check (= 3 (convert (setq n (+ n 1)) (progn (setq n (+ n 1)) 't))))
        (check (= n 4))
        (check (equal '(2) (slice (progn (setq n (+ n 1)) '(1 2 3))
                                 (progn (setq n (+ n 1)) 1) (progn (setq n (+ n 1)) 2))))
        (check (= n 7)))
      (format t "RESULTS ~S~%" (list
        (append) (append nil 7)
        (multiple-value-list (append (values nil :stale)))
        (multiple-value-list (coerce (values 7 :stale) 't))
        (multiple-value-list (subseq (values '(1 2) :stale) 0 1))))
      (format t "ERRORS ~S~%" (list
        (handler-case (funcall #'append '(1 . 2) nil) (type-error () :type))
        (handler-case (apply 'coerce '(1)) (program-error () :arity))
        (handler-case (funcall #'coerce 1 'cons) (type-error () :type))
        (handler-case (funcall #'subseq nil) (program-error () :arity))
        (handler-case (funcall #'subseq 7 0 1) (type-error () :type))))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let output = run(program, tier);
        assert!(
            output.contains("RESULTS (NIL 7 (NIL) (7) ((1)))"),
            "{tier}: {output}"
        );
        assert!(
            output.contains("ERRORS (:TYPE :ARITY :TYPE :ARITY :TYPE)"),
            "{tier}: {output}"
        );
    }
}

#[test]
fn coerce_aliases_share_the_value_kernel() {
    let program = r#"
      (deftype bridge-string () '(simple-array character (*)))
      (deftype bridge-callable () 'function)
      (defun convert-alias (x type) (coerce x type))
      (format t "ALIASES ~S~%"
        (list (convert-alias '(#\a #\b) 'bridge-string)
              (funcall (convert-alias '(lambda (x) (+ x 1)) 'bridge-callable) 41)
              (functionp (convert-alias 'list 'bridge-callable))))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let output = run_with_bootstrap(program, tier, true);
        assert!(output.contains("ALIASES (\"ab\" 42 T)"), "{tier}: {output}");
    }
}

#[test]
fn sequence_value_kernels_respect_replaced_functions() {
    let program = r#"
      (defun join (a b) (append a b))
      (defun convert (x type) (coerce x type))
      (defun slice (x a b) (subseq x a b))
      (dotimes (i 20) (join nil nil) (convert i 't) (slice '(1 2) 0 1))
      (format t "LEXICAL ~S~%"
        (flet ((append (&rest args) :append) (coerce (x type) :coerce)
               (subseq (x a b) :subseq))
          (list (append nil nil) (coerce 1 't) (subseq nil 0 0))))
      (defun append (&rest args) :append)
      (defun coerce (x type) :coerce)
      (defun subseq (x a b) :subseq)
      (format t "REBOUND ~S~%" (list (join nil nil) (convert 1 't) (slice nil 0 0)
                                    (funcall #'append nil nil) (apply 'coerce '(1 t))))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let output = run(program, tier);
        assert!(
            output.contains("LEXICAL (:APPEND :COERCE :SUBSEQ)"),
            "{tier}: {output}"
        );
        assert!(
            output.contains("REBOUND (:APPEND :COERCE :SUBSEQ :APPEND :COERCE)"),
            "{tier}: {output}
        "
        );
    }
}
