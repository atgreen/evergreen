// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Header-bound motion through the production T2 pipeline. A mixed integer
//! lane supplies the peeled fast loop; changing bounds remain observable.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn native_invariant_bounds_preserve_values_effects_and_error_continuations() {
    let program = r#"
        (defvar *bound-effects* 0)
        (defvar *bound-entries* 0)
        (defvar *bound-reads* 0)
        (defun invariant-bound-loop (initial base width limit)
          (incf *bound-entries*)
          (let ((lane initial) (checksum 0))
            (do ((i base (1+ i)))
                ((>= i (min (+ base width) limit)))
              (incf *bound-effects*)
              (incf checksum (ldb (byte 8 0) lane))
              (setf lane (ash lane -8)))
            (values lane checksum)))
        (defun check-bound-loop (initial base width limit trips expected checksum)
          (let ((effects *bound-effects*) (entries *bound-entries*))
            (multiple-value-bind (lane sum)
                (invariant-bound-loop initial base width limit)
              (assert (eql lane expected))
              (assert (= sum checksum)))
            (assert (= (- *bound-effects* effects) trips))
            (assert (= (- *bound-entries* entries) 1))))
        ;; The initial wide lane narrows after one iteration. Actual operand
        ;; observations make the general peeling pass admit the native remainder.
        (dotimes (i 160)
          (check-bound-loop 18446744073709551615 0 8 8 8 0 2040))
        (assert (= 2 (egcl-ext:function-tier 'invariant-bound-loop)))
        (dolist (case '((0 8 8 8 0 2040)
                        (3 8 6 3 1099511627775 765)
                        (0 8 3 3 1099511627775 765)
                        (2 0 20 0 18446744073709551615 0)
                        (2 -3 20 0 18446744073709551615 0)
                        (4 8 4 0 18446744073709551615 0)
                        (-2 3 8 3 1099511627775 765)))
          (apply #'check-bound-loop 18446744073709551615 case))
        (check-bound-loop :untouched 0 0 8 0 :untouched 0)
        (check-bound-loop -1 0 8 8 8 -1 2040)
        (check-bound-loop (ash 1 200) 0 8 8 8 (ash 1 136) 0)
        (check-bound-loop (- (ash 1 200)) 0 8 8 8 (- (ash 1 136)) 0)

        ;; DO evaluates its end test on every visit (unlike LOOP arithmetic
        ;; bounds, which are evaluated once). The provider's effect and
        ;; changing result must survive even if the independent addition moves.
        (declaim (notinline changing-bound))
        (defun changing-bound (index limit)
          (incf *bound-reads*)
          (if (< index 3) limit index))
        (defun varying-bound-loop (initial limit)
          (let ((lane initial))
            (do ((i 0 (1+ i)))
                ((>= i (min (+ 0 8) (changing-bound i limit))))
              (incf *bound-effects*)
              (setf lane (ash lane -8)))
            lane))
        (dotimes (i 160)
          (let ((effects *bound-effects*) (reads *bound-reads*))
            (assert (= (varying-bound-loop 18446744073709551615 8) 1099511627775))
            (assert (= (- *bound-effects* effects) 3))
            (assert (= (- *bound-reads* reads) 4))))
        (assert (= 2 (egcl-ext:function-tier 'varying-bound-loop)))

        ;; These exercise native arithmetic recovery after the entry effect.
        ;; They need not fail a hoisted fast-loop guard: the original/peeled
        ;; header can encounter the same invalid operation first.
        (let ((before (egcl-ext:deopt-count)))
          (check-bound-loop 18446744073709551615 1152921504606846975 1 0
                            0 18446744073709551615 0)
          (assert (> (egcl-ext:deopt-count) before)))
        (dolist (arguments '((0 "bad-width" 8) ("bad-base" 8 8) (0 8 "bad-limit")))
          (let ((effects *bound-effects*) (entries *bound-entries*) (caught nil))
            (handler-case
                (apply #'invariant-bound-loop 18446744073709551615 arguments)
              (type-error () (setf caught t)))
            (assert caught)
            (assert (= *bound-effects* effects))
            (assert (= (- *bound-entries* entries) 1))))
        (format t "INVARIANT-BOUNDS-OK EFFECTS ~D ENTRIES ~D READS ~D~%"
                *bound-effects* *bound-entries* *bound-reads*)
    "#;
    let mut outputs = Vec::new();
    for enabled in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            command.env_remove("EGCL_DISABLE_INVARIANT_BOUNDS");
        } else {
            command.env("EGCL_DISABLE_INVARIANT_BOUNDS", "1");
        }
        let output = command.output().expect("run invariant bounds case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("INVARIANT-BOUNDS-OK"), "{stdout}\n{stderr}");
        if enabled {
            assert!(
                stderr.lines().any(|line| {
                    line.contains("INVARIANT-BOUND-LOOP: invariant bounds hoisted: ")
                        && line
                            .rsplit(": ")
                            .next()
                            .and_then(|count| count.parse::<usize>().ok())
                            .is_some_and(|count| count > 0)
                }),
                "the intended native optimization did not run:\n{stderr}"
            );
        }
        outputs.push(stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "hoisting changes observable results"
    );
}
