// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A function with a loop must reach T2, and its sampled back-edge poll must
//! keep heap values alive across the collection it may trigger.
//!
//! The ppc64le emitter once marked every backward edge as a poll site but
//! computed GC root sets only for call-type instructions, so the poll's root
//! sync found none and every loop was declined wholesale (bliss-83icg). The
//! x86-64 emitter never had that gap, so on x86-64 this test documents the
//! contract rather than a fix.
#![cfg(all(
    target_pointer_width = "64",
    any(
        all(unix, target_arch = "x86_64"),
        all(
            target_os = "linux",
            target_env = "gnu",
            any(
                target_arch = "aarch64",
                all(target_arch = "powerpc64", target_endian = "little"),
                target_arch = "s390x"
            )
        )
    )
))]
use std::process::Command;

/// Keep calling until the background T2 compile is installed (bounded), then
/// take the checked answers from the T2 code. The tier is polled through the
/// public query rather than inferred from a call count, because the compile
/// runs on another thread and a loaded machine can take a while to publish it.
const CONSING_LOOP: &str = r#"
  (defun consing-loop (n)
    (let ((acc nil))
      (dotimes (i n) (setq acc (cons (list i i) acc)))
      (length acc)))
  (loop repeat 3000
        do (consing-loop 20)
        until (eql (egcl-ext:function-tier 'consing-loop) 2))
  (print (egcl-ext:function-tier 'consing-loop))
  (print (list (consing-loop 100) (consing-loop 7)))"#;

fn run(program: &str, extra: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", program])
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr");
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

fn assert_reached_t2(stdout: &str, stderr: &str) {
    assert!(
        stderr.contains("CONSING-LOOP: T2 INSTALLED"),
        "the loop never reached T2:\n{stdout}\n{stderr}"
    );
    assert!(
        !stderr.contains("CONSING-LOOP: emit_framed failed"),
        "{stderr}"
    );
    assert!(
        stdout.contains("\n2 "),
        "tier query did not report T2:\n{stdout}"
    );
    assert!(stdout.contains("(100 7)"), "{stdout}\n{stderr}");
}

#[test]
fn consing_loop_reaches_t2_with_identical_result() {
    let (stdout, stderr) = run(CONSING_LOOP, &[]);
    assert_reached_t2(&stdout, &stderr);
}

#[test]
fn consing_loop_at_t2_survives_gc_stress_with_poison() {
    // Each iteration allocates three objects, so a collection every 25
    // allocations still fires on roughly every eighth back edge and poisons
    // what it moves; a stale root would read poisoned memory or change the
    // count. Every-allocation stress also passes but takes about twenty
    // minutes on a loaded POWER9, which is too slow for a routine test.
    let (stdout, stderr) = run(
        CONSING_LOOP,
        &[("EGCL_GC_STRESS", "25"), ("EGCL_GC_POISON", "1")],
    );
    assert_reached_t2(&stdout, &stderr);
}
