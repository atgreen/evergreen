//! Evaluated-argument value/sequence calls retain CL semantics (R5.06, R5.131).

use std::process::Command;

fn run(program: &str, tier: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--no-bootstrap", "--eval", program])
        .env("TORCL_FORCE_TIER", tier)
        .env("TORCL_T1_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tier {tier}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn value_and_sequence_calls_agree_across_tiers() {
    let program = r#"
      (defun check (value) (unless value (error "bridge check failed")))
      (defun mv-direct (a b) (values a b))
      (defun mv-funcall (a b) (funcall #'values a b))
      (defun mv-apply (xs) (apply 'values xs))
      (defun vl-direct (xs) (values-list xs))
      (defun vl-funcall (xs) (funcall #'values-list xs))
      (defun vl-apply (xs) (apply 'values-list (list xs)))
      (defun rev (xs) (reverse xs))
      (defun end (xs) (endp xs))
      (dotimes (i 20)
        (check (equal '(1 2) (multiple-value-list (mv-direct 1 2))))
        (check (equal '(1 2) (multiple-value-list (mv-funcall 1 2))))
        (dolist (xs '(nil (1) (1 2 3)))
          (check (equal xs (multiple-value-list (mv-apply xs))))
          (check (equal xs (multiple-value-list (vl-direct xs))))
          (check (equal xs (multiple-value-list (vl-funcall xs))))
          (check (equal xs (multiple-value-list (vl-apply xs)))))
        (check (equal '(3 2 1) (rev '(1 2 3))))
        (check (equalp #(3 2 1) (rev #(1 2 3))))
        (check (equal "cba" (rev "abc")))
        (check (equalp #*1010 (rev #*0101)))
        (check (end nil))
        (check (not (end '(1 . 2)))))
      (format t "BRIDGES ~S~%"
        (list (multiple-value-list (values (values 1 2)))
              (multiple-value-list (reverse (values '(1 2) :stale)))
              (multiple-value-list (endp (values nil :stale)))
              (multiple-value-list (values-list (values '(4 5) :stale)))))
      (format t "LEXICAL ~S~%"
        (flet ((reverse (x) :r) (endp (x) :e) (values-list (x) :v))
          (list (reverse nil) (endp nil) (values-list nil))))
      (defun reverse (x) :r)
      (defun endp (x) :e)
      (defun values-list (x) :v)
      (format t "REBOUND ~S~%" (list (rev nil) (end nil) (vl-direct nil)
                                   (vl-funcall nil) (vl-apply nil)))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let out = run(program, tier);
        for expected in [
            "BRIDGES ((1) ((2 1)) (T) (4 5))",
            "LEXICAL (:R :E :V)",
            "REBOUND (:R :E :V :V :V)",
        ] {
            assert!(
                out.contains(expected),
                "tier {tier}: expected {expected}, got {out}"
            );
        }
    }
}

#[test]
fn ordered_comparisons_preserve_exact_numbers_and_errors() {
    let program = r#"
      (defun check (value) (unless value (error "comparison check failed")))
      (defun ordered (a b c) (list (< a b c) (<= a b c) (> c b a) (>= c b a)))
      (dotimes (i 20)
        (let* ((big (ash 1 100)) (tiny (/ 1 big)))
          (check (equal '(t t t t) (ordered (- tiny) 0 tiny)))
          (check (equal '(t t t t) (ordered big (+ big 1) (+ big 2))))
          (check (equal '(nil t nil t) (ordered 1 1.0 1d0)))))
      (format t "ORDER ~S~%" (list (< 1) (funcall #'<= 1 2 3) (apply '>= '(3 2 1))
        (multiple-value-list (< (values 1 9) 2 3))))
      (format t "ERRORS ~S~%" (list
        (handler-case (<) (program-error () :arity))
        (handler-case (funcall #'<=) (program-error () :arity))
        (handler-case (apply '> nil) (program-error () :arity))
        (handler-case (< :bad) (type-error () :type))
        (handler-case (funcall #'>= 3 2 :bad) (type-error () :type))
        (handler-case (apply '< '(1 :bad 3)) (type-error () :type))
        (handler-case (values-list '(1 . 2)) (type-error () :type))
        (handler-case (funcall #'values-list) (program-error () :arity))
        (handler-case (apply 'values-list '(nil nil)) (program-error () :arity))
        (handler-case (reverse 1) (type-error () :type))
        (handler-case (endp 1) (type-error () :type))))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let out = run(program, tier);
        assert!(out.contains("ORDER (T T T (T))"), "tier {tier}: {out}");
        assert!(
            out.contains(
                "ERRORS (:ARITY :ARITY :ARITY :TYPE :TYPE :TYPE :TYPE :ARITY :ARITY :TYPE :TYPE)"
            ),
            "tier {tier}: {out}"
        );
    }
}
