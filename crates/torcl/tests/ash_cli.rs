//! Direct integer shifts, including the pre-bootstrap primitive (R5.06).

use std::process::Command;

fn run(program: &str, extra: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .args(extra)
        .output()
        .expect("run ASH test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ash_is_a_prebootstrap_primitive() {
    let out = run(
        "(print (list (ash 3 4) (ash -7 -1) (ash 1 64)))",
        &["--no-bootstrap"],
    );
    assert!(out.contains("(48 -4 18446744073709551616)"), "{out}");
}

#[test]
fn ash_integer_boundaries_and_single_value() {
    let program = r#"
      (print (list
        (ash 0 1180591620717411303424)
        (ash 1 -1180591620717411303424)
        (ash -1 -1180591620717411303424)
        (ash 1152921504606846975 1)
        (ash -1152921504606846976 1)
        (ash 18446744073709551617 -64)
        (ash -18446744073709551617 -64)
        (ash -18446744073709551616 -64)
        (ash 1 128)
        (multiple-value-list (ash (values -7 :stale) -1))))
    "#;
    let out = run(program, &[]);
    assert!(out.contains("(0 0 -1 2305843009213693950 -2305843009213693952 1 -2 -1 340282366920938463463374607431768211456 (-4))"), "{out}");
}

#[test]
fn ash_validates_types_and_arity() {
    let out = run(
        r#"
      (print (list
        (handler-case (ash "bad" 0) (type-error () :type))
        (handler-case (ash 0 "bad") (type-error () :type))
        (handler-case (ash 1 1/2) (type-error () :type))
        (handler-case (ash 1) (program-error () :arity))
        (handler-case (funcall #'ash 1 2 3) (program-error () :arity))))
    "#,
        &[],
    );
    assert!(out.contains("(:TYPE :TYPE :TYPE :ARITY :ARITY)"), "{out}");
}

#[test]
fn ash_matches_arithmetic_oracle_and_respects_rebinding() {
    let program = r#"
      (defun shifted (n count) (ash n count))
      (let ((ok t))
        (dolist (n '(0 1 -1 7 -7 1152921504606846975 -1152921504606846976
                    18446744073709551616 -18446744073709551616
                    18446744073709551617 -18446744073709551617
                    340282366920938463463374607431768211455
                    -340282366920938463463374607431768211455))
          (dolist (count '(-130 -128 -127 -65 -64 -63 -61 -60 -1 0 1 60 61 63 64 65 127 128 130))
            (let ((expected (if (< count 0)
                                (floor n (expt 2 (- count)))
                                (* n (expt 2 count)))))
              (unless (and (= expected (shifted n count))
                           (= expected (funcall #'ash n count))
                           (= expected (apply 'ash (list n count))))
                (setq ok nil)))))
        (print (list :oracle ok)))
      (print (flet ((ash (a b) (+ a b))) (ash 10 20)))
      (defun ash (a b) (+ a b))
      (print (list (ash 10 20) (shifted 10 20) (funcall #'ash 10 20)))
    "#;
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .env("TORCL_FORCE_TIER", tier)
            .env("TORCL_T1_THRESHOLD", "1")
            .args(["--no-init", "--eval", program])
            .output()
            .expect("run ASH tier comparison");
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{out}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(out.contains("(:ORACLE T)"), "tier {tier}: {out}");
        assert!(out.contains("(30 30 30)"), "tier {tier}: {out}");
        assert!(
            out.lines().any(|line| line.trim() == "30"),
            "tier {tier}: {out}"
        );
    }
}
