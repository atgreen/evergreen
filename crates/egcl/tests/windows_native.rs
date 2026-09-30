#![cfg(all(windows, target_arch = "x86_64"))]

use std::process::Command;

fn run(program: &str, env: &[(&str, &str)]) -> String {
    run_with_stderr(program, env).0
}

fn run_with_stderr(program: &str, env: &[(&str, &str)]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--no-bootstrap", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_DISABLE_T2", "1")
        .envs(env.iter().copied())
        .output()
        .expect("run Windows CLI");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap().replace('\r', ""),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn windows_t2_allocations_and_precise_deopt_preserve_roots_and_effects() {
    let program = r#"
      (setq *native-t2-count* 0)
      (defun native-t2-alloc (x)
        (let ((tail (cons x nil)))
          (setq *native-t2-count* (+ *native-t2-count* 1))
          (cons (+ x 1) tail)))
      (dotimes (i 40) (native-t2-alloc i))
      (format t "~D~%" (egcl-ext:function-tier 'native-t2-alloc))
      (setq *native-t2-count* 0)
      (let ((before (egcl-ext:deopt-count)))
        (format t "~S ~D ~S~%" (native-t2-alloc 0.5) *native-t2-count*
          (> (egcl-ext:deopt-count) before)))
    "#;
    for stress in ["0", "1"] {
        assert_eq!(
            run(
                program,
                &[
                    ("EGCL_DISABLE_T2", "0"),
                    ("EGCL_FORCE_TIER", "t2"),
                    ("EGCL_GC_STRESS", stress),
                    ("EGCL_GC_POISON", "1"),
                ]
            ),
            "2\n(1.5 0.5) 1 T\nNIL\n"
        );
    }
}

#[test]
fn windows_direct_native_call_stops_at_a_callee_error() {
    let program = r#"
      (defun native-pair (x) (cons x nil))
      (native-pair 0)
      (defun native-pair-caller (x) (native-pair x))
      (format t "~S~%" (native-pair-caller 9))
      (setq *native-after* 0)
      (defun native-char (x) (char-code x))
      (native-char #\A)
      (defun native-through (x) (native-char x) (setq *native-after* 1))
      (native-through #\B)
      (setq *native-after* 0)
      (format t "~S ~D ~D~%"
        (handler-case (native-through 1) (type-error () :caught))
        *native-after* (egcl-ext:function-tier 'native-through))
    "#;
    for stress in ["0", "1"] {
        let (stdout, stderr) = run_with_stderr(
            program,
            &[
                ("EGCL_FORCE_TIER", "t1"),
                ("EGCL_NN_DIRECT", "1"),
                ("EGCL_LOG", "compile=trace"),
                ("EGCL_GC_STRESS", stress),
                ("EGCL_GC_POISON", "1"),
            ],
        );
        assert_eq!(stdout, "(9)\n:CAUGHT 0 1\nNIL\n");
        for callee in ["NATIVE-CHAR", "NATIVE-PAIR"] {
            assert!(
                stderr.contains(&format!("direct call to {callee} [T1]")),
                "must exercise a direct native call: {stderr}"
            );
        }
    }
}

#[test]
fn windows_t1_executes_calls_allocations_and_precise_deopt() {
    let program = r#"
      (setq *native-count* 0)
      (defun native-inner (x) (cons x (cons (+ x 1) nil)))
      (defun native-outer (x)
        (setq *native-count* (+ *native-count* 1))
        (native-inner x))
      (format t "~S~%" (native-outer 40))
      (format t "~D ~D~%" (egcl-ext:function-tier 'native-inner)
                            (egcl-ext:function-tier 'native-outer))
      (format t "~S ~D~%" (native-outer 0.5) *native-count*)
      (defun native-mv (x) (values (cons x nil) 7))
      (defun native-mv-caller (x)
        (multiple-value-bind (a b) (native-mv x) (cons a (cons b nil))))
      (format t "~S~%" (native-mv-caller 3))
    "#;
    let expected = "(40 41)\n1 1\n(0.5 1.5) 2\n((3) 7)\nNIL\n";
    assert_eq!(run(program, &[("EGCL_FORCE_TIER", "t1")]), expected);
    assert_eq!(
        run(
            program,
            &[
                ("EGCL_FORCE_TIER", "t1"),
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_POISON", "1")
            ]
        ),
        expected
    );
}

#[test]
fn windows_osr_enters_native_loop_and_preserves_deopt_state() {
    let program = r#"
      (defun native-loop (n)
        (let ((sum 0))
          (dotimes (i n sum)
            (setq sum (+ sum (if (= i 200) 0.5 1))))))
      (format t "~S~%" (native-loop 500))
      (format t "~S~%" (> (egcl-ext:function-osr-count 'native-loop) 0))
      (defun native-alloc-loop (n)
        (let ((head nil))
          (dotimes (i n head) (setq head (cons i head)))))
      (setq *native-list* (native-alloc-loop 300))
      (format t "~S ~S ~S~%" (car *native-list*) (car (cdr *native-list*))
        (> (egcl-ext:function-osr-count 'native-alloc-loop) 0))
    "#;
    for stress in ["0", "1"] {
        assert_eq!(
            run(
                program,
                &[
                    ("EGCL_T1_THRESHOLD", "1000000"),
                    ("EGCL_OSR_THRESHOLD", "50"),
                    ("EGCL_GC_STRESS", stress),
                    ("EGCL_GC_POISON", "1"),
                ]
            ),
            "499.5\nT\n299 298 T\nNIL\n"
        );
    }
}
