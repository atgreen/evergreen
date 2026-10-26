// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn native_branch_keeps_its_bound_when_raw_edge_boxing_reuses_the_operand_home() {
    let program = r#"
        (defvar *fusion-effects* 0)
        (defvar *fusion-entries* 0)
        (defun fusion-lane-loop (initial base width limit)
          (incf *fusion-entries*)
          (let ((lane initial) (checksum 0))
            (do ((i base (1+ i)))
                ((>= i (min (+ base width) limit)))
              (incf *fusion-effects*)
              (incf checksum (ldb (byte 8 0) lane))
              (setf lane (ash lane -8)))
            (values lane checksum)))
        (defun check-fusion-lane (base width limit trips expected checksum)
          (let ((effects *fusion-effects*) (entries *fusion-entries*))
            (multiple-value-bind (lane sum)
                (fusion-lane-loop 18446744073709551615 base width limit)
              (assert (eql lane expected))
              (assert (= sum checksum)))
            (assert (= (- *fusion-effects* effects) trips))
            (assert (= (- *fusion-entries* entries) 1))))
        ;; Mixed integer observations produce a raw peeled remainder. Boxing
        ;; its lane for an exit edge can reuse the dead comparison operand's
        ;; physical home, while the comparison Boolean remains live.
        (dotimes (i 160) (check-fusion-lane 0 8 8 8 0 2040))
        (assert (= 2 (egcl-ext:function-tier 'fusion-lane-loop)))
        ;; The broken deferred comparison executes eight shifts here instead
        ;; of three: it reads the boxed lane instead of the MIN result.
        (check-fusion-lane 3 8 6 3 1099511627775 765)
        (check-fusion-lane 0 8 3 3 1099511627775 765)
        (check-fusion-lane 0 1 8 1 72057594037927935 255)
        (check-fusion-lane 4 8 4 0 18446744073709551615 0)
        (check-fusion-lane -2 3 8 3 1099511627775 765)
        (check-fusion-lane 0 8 20 8 0 2040)
        (format t "COMPARISON-FUSION-OK EFFECTS ~D ENTRIES ~D~%"
                *fusion-effects* *fusion-entries*)
    "#;
    let mut outputs = Vec::new();
    for stress in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_DISABLE_INVARIANT_BOUNDS", "1")
            .env("EGCL_DISABLE_INTEGER_SHIFT", "1")
            .env("EGCL_T2_LOG", "stderr");
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().expect("run comparison fusion regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("COMPARISON-FUSION-OK"),
            "{stdout}\n{stderr}"
        );
        assert!(
            stderr.lines().any(|line| {
                line.contains("FUSION-LANE-LOOP:")
                    && line.contains(" T2 emitted;")
                    && line
                        .split("raw_fixnum_values=")
                        .nth(1)
                        .and_then(|count| count.parse::<usize>().ok())
                        .is_some_and(|count| count > 0)
            }),
            "the raw native artifact was not installed:\n{stderr}"
        );
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1], "GC stress changes branch semantics");
}
