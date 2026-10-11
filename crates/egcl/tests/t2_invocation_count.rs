// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(defun counter-fib (n)
  (if (< n 2) n (+ (counter-fib (- n 1)) (counter-fib (- n 2)))))
(defun warm-counter-fib ()
  (loop repeat 2000
        do (counter-fib 8)
        when (= 2 (egcl-ext:function-tier 'counter-fib)) return t
        do (sleep 0.001)
        finally (error "Fibonacci did not reach T2")))
(warm-counter-fib)
(format t "T2-INSTALLED~%")
(let ((before (egcl-ext:function-invoke-count 'counter-fib))
      (f #'counter-fib))
  (assert (= 144 (counter-fib 12)))
  (assert (= 144 (funcall f 12)))
  (assert (= 144 (apply f '(12))))
  (format t "COUNTS ~D ~D~%" before (egcl-ext:function-invoke-count 'counter-fib))
  (assert (= before (egcl-ext:function-invoke-count 'counter-fib))))
(format t "T2-COUNT-FROZEN~%")
(egcl-ext:profile 'counter-fib)
(assert (= 0 (egcl-ext:function-tier 'counter-fib)))
(let ((before (egcl-ext:function-invoke-count 'counter-fib)))
  (assert (= 144 (counter-fib 12)))
  (assert (= 465 (- (egcl-ext:function-invoke-count 'counter-fib) before))))
(egcl-ext:profile-reset)
(assert (= 144 (funcall #'counter-fib 12)))
(egcl-ext:profile-report-calls)
(egcl-ext:unprofile 'counter-fib)
(warm-counter-fib)
(let ((before (egcl-ext:function-invoke-count 'counter-fib)))
  (assert (= 144 (counter-fib 12)))
  (assert (= before (egcl-ext:function-invoke-count 'counter-fib))))
;; The saved old object still reports its former tier after redefinition;
;; counting must follow the private body actually selected by dispatch.
(let ((old #'counter-fib))
  (defun counter-fib (n) (+ n 100))
  (let ((before (egcl-ext:function-invoke-count old)))
    (assert (= 0 (funcall old 0)))
    (assert (= (+ before 1) (egcl-ext:function-invoke-count old)))))
(format t "T2-COUNTER-POLICY-OK~%")
"#;

#[test]
fn t2_counter_stops_and_explicit_profile_counts_recursive_calls() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_egcl"));
    cmd.args(["--no-init", "--eval", PROGRAM]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_DISABLE_T2",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_NATIVE_DEPTH_CAP",
        "EGCL_NO_DIRECT_SELF_CALL",
    ] {
        cmd.env_remove(key);
    }
    let out = cmd
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "2")
        .env("EGCL_T2", "1")
        .output()
        .expect("run T2 counter regression");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("T2-COUNTER-POLICY-OK"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.split_whitespace().next() == Some("465")
                && line.contains("COUNTER-FIB")),
        "exact profile report: {stdout}"
    );
}
