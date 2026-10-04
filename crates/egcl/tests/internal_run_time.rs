// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CPU time must advance during computation, but exclude sleeping (bliss-vuc5n).
use std::process::Command;

#[test]
fn internal_run_time_measures_cpu_work_instead_of_sleep() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args([
                "--no-init",
                "--eval",
                r#"
              (let* ((real-start (get-internal-real-time))
                     (run-start (get-internal-run-time)))
                (sleep 0.25)
                (let ((slept-real (- (get-internal-real-time) real-start))
                      (slept-run (- (get-internal-run-time) run-start))
                      (work-start (get-internal-run-time))
                      (deadline (+ (get-internal-real-time) 10000)))
                  ;; CPU clocks may have coarse accounting ticks. Keep doing
                  ;; checked work until a tick advances, with a wall-clock bound
                  ;; so a broken constant CPU clock fails instead of hanging.
                  (let ((sum 0))
                    (loop
                      (setq sum (loop for i below 5000 sum (logand i 255)))
                      (when (>= (- (get-internal-run-time) work-start) 20)
                        (return))
                      (assert (< (get-internal-real-time) deadline)))
                    (format t "CLOCKS: ~D ~D ~D ~D ~D~%"
                            internal-time-units-per-second slept-real slept-run
                            (- (get-internal-run-time) work-start) sum))))
            "#,
            ])
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_BACKEND")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_POISON")
            .output()
            .expect("run CPU-clock probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        let values: Vec<i64> = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CLOCKS: "))
            .unwrap_or_else(|| panic!("{tier}: missing clock sample: {stdout}\n{stderr}"))
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect();
        assert_eq!(values.len(), 5, "{tier}: {stdout}");
        assert_eq!(values[0], 1000, "{tier}: time units");
        assert!(values[1] >= 200, "{tier}: sleep did not elapse: {values:?}");
        assert!(
            values[2] >= 0 && values[2] < values[1] / 2,
            "{tier}: sleeping charged as CPU time: {values:?}"
        );
        assert!(
            values[3] >= 20,
            "{tier}: CPU work did not advance clock: {values:?}"
        );
        assert_eq!(values[4], 629_340, "{tier}: computation changed");
    }
}
