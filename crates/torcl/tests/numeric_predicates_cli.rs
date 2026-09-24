//! Shared zero/sign predicates: exact numbers, call paths and invalidation (R5.06).

use std::process::Command;

fn run(program: &str, tier: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--no-bootstrap", "--eval", program])
        .env("TORCL_FORCE_TIER", tier)
        .env("TORCL_T1_THRESHOLD", "1")
        .output()
        .expect("run numeric predicates");
    assert!(
        output.status.success(),
        "tier {tier}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn exact_signs_agree_across_call_paths_and_tiers() {
    let program = r#"
      (defun direct-predicates (x) (list (zerop x) (plusp x) (minusp x)))
      (defun funcall-predicates (x)
        (list (funcall #'zerop x) (funcall #'plusp x) (funcall #'minusp x)))
      (defun apply-predicates (x)
        (list (apply 'zerop (list x)) (apply 'plusp (list x)) (apply 'minusp (list x))))
      (let* ((big (ash 1 2000)) (tiny (/ 1 big)) (ok t))
        (dolist (group (list (list '(t nil nil) 0 0.0 -0.0 0d0 -0d0)
                            (list '(nil t nil) 1 1152921504606846975 big tiny
                                  1/2 1.0 1d0 1d-300)
                            (list '(nil nil t) -1 -1152921504606846976 (- big)
                                  (- tiny) -1/2 -1.0 -1d0 -1d-300)))
          (dolist (x (cdr group))
            (dotimes (i 20)
              (unless (and (equal (car group) (direct-predicates x))
                           (equal (car group) (funcall-predicates x))
                           (equal (car group) (apply-predicates x)))
                (setq ok nil)))))
        (format t "~&PREDICATES ~S~%" ok)
        (format t "COMPLEX ~S~%"
                (list (zerop (complex 0.0 -0.0)) (zerop (complex tiny 0))
                      (zerop (complex 0 tiny)) (zerop (complex 1 2)))))
      (format t "MV ~S~%"
              (list (multiple-value-list (zerop (values 0 :stale)))
                    (multiple-value-list (funcall #'plusp (values 1 :stale)))
                    (multiple-value-list (apply 'minusp (values '(-1) :stale)))))
      (format t "LEXICAL ~S~%"
              (flet ((zerop (x) :z) (plusp (x) :p) (minusp (x) :m))
                (list (zerop 0) (plusp 1) (minusp -1))))
      (defun zerop (x) :z)
      (defun plusp (x) :p)
      (defun minusp (x) :m)
      (format t "REBOUND ~S~%"
              (list (direct-predicates 0) (funcall-predicates 0) (apply-predicates 0)))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let out = run(program, tier);
        for expected in [
            "PREDICATES T",
            "COMPLEX (T NIL NIL NIL)",
            "MV ((T) (T) (T))",
            "LEXICAL (:Z :P :M)",
            "REBOUND ((:Z :P :M) (:Z :P :M) (:Z :P :M))",
        ] {
            assert!(
                out.contains(expected),
                "tier {tier}, expected {expected}: {out}"
            );
        }
    }
}

#[test]
fn predicates_validate_types_and_arity_in_all_call_paths() {
    for tier in ["interp", "t0", "t1"] {
        let mut program = String::from("(print (list");
        let mut expected = Vec::new();
        for name in ["zerop", "plusp", "minusp"] {
            for args in ["", "1 2"] {
                for form in [
                    format!("({name} {args})"),
                    format!("(funcall #'{name} {args})"),
                    format!("(apply '{name} '({args}))"),
                ] {
                    program.push_str(&format!("(handler-case {form} (program-error () :arity))"));
                    expected.push(":ARITY");
                }
            }
            for value in ["nil", "\"bad\"", "'(1 2)"] {
                for form in [
                    format!("({name} {value})"),
                    format!("(funcall #'{name} {value})"),
                    format!("(apply '{name} (list {value}))"),
                ] {
                    program.push_str(&format!("(handler-case {form} (type-error () :type))"));
                    expected.push(":TYPE");
                }
            }
        }
        for name in ["plusp", "minusp"] {
            program.push_str(&format!(
                "(handler-case ({name} (complex 0.0 0.0)) (type-error () :type))"
            ));
            expected.push(":TYPE");
        }
        program.push_str("))");
        let out = run(&program, tier);
        assert!(out.contains(&format!("({})", expected.join(" "))), "{out}");
    }
}
