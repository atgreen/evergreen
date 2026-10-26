// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn ordinary_min_max_bounds_and_packed_load_preserve_layout_fallback_and_effects() {
    let program = r#"
        (defvar *ordinary-array-effects* 0)
        (defun ordinary-numeric-window (array start limit)
          (declare (type fixnum start limit))
          (let ((end (max start (min (+ start 8) limit))) (sum 0d0))
            (loop for i fixnum from start below end do
              (let ((element (aref array i)))
                (incf *ordinary-array-effects*)
                (incf sum element)))
            sum))
        (let ((packed (make-array 8 :element-type 'double-float :initial-element 1.25d0)))
          (dotimes (i 120) (ordinary-numeric-window packed 0 8))
          (assert (= 2 (egcl-ext:function-tier 'ordinary-numeric-window)))
          (assert (= (ordinary-numeric-window packed 0 20) 10d0))
          (assert (= (ordinary-numeric-window packed 2 6) 5d0))
          (setf *ordinary-array-effects* 0)
          (assert (= (ordinary-numeric-window packed 0 -1) 0d0))
          (assert (= *ordinary-array-effects* 0))
          ;; The first read succeeds; the second must fail before committing
          ;; its following effect, even if recovery re-enters the interpreter.
          (let ((caught nil))
            (handler-case (ordinary-numeric-window packed 7 20)
              (error () (setf caught t)))
            (assert caught) (assert (= *ordinary-array-effects* 1))))
        ;; The ordinary raw candidate retains its general vector arm. Numeric
        ;; types outside its guarded fast path retain the original arithmetic.
        (assert (= (ordinary-numeric-window (make-array 8 :initial-element 2d0) 0 8) 16d0))
        (assert (= (ordinary-numeric-window (make-array 8 :initial-element 3) 0 8) 24d0))
        (setf *ordinary-array-effects* 0)
        (let ((caught nil))
          (handler-case (ordinary-numeric-window (vector "not-a-number") 0 1)
            (type-error () (setf caught t)))
          (assert caught) (assert (= *ordinary-array-effects* 1)))
        ;; A fresh array can move during escape boxing/allocating callbacks;
        ;; the loop must derive each address again from its live tagged root.
        (assert (= (ordinary-numeric-window
                    (make-array 8 :element-type 'double-float :initial-element 4d0) 0 8) 32d0))
        (format t "ORDINARY-NUMERIC-ARRAYS-OK~%")
    "#;
    let mut outputs = Vec::new();
    for stress in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run native numeric array case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("ORDINARY-NUMERIC-WINDOW:")
                    && line.contains("ordinary x86-64 Lisp T2 emitted;")
                    && line
                        .split("raw_f64_values=")
                        .nth(1)
                        .and_then(|tail| tail.split(',').next())
                        .and_then(|count| count.parse::<usize>().ok())
                        .is_some_and(|count| count > 0)),
            "the ordinary raw array version was not installed:\n{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("ORDINARY-NUMERIC-WINDOW:")
                    && line.contains("installed with ordinary x86-64 Lisp T2 ABI")),
            "{stderr}"
        );
        assert!(stdout.contains("ORDINARY-NUMERIC-ARRAYS-OK"));
        outputs.push(stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "GC stress changes numeric array semantics"
    );
}
