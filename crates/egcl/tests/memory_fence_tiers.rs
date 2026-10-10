// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Memory barriers must remain callable at every tier, with the same result
//! as the interpreter. Their source-level calls retain live function bindings;
//! direct native fence expansions guard those bindings first.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn memory_barriers_compile_at_every_tier() {
    let program = "(progn \
        (defun fenced (x) \
          (egcl-ext:memory-barrier :full) \
          (egcl-ext:load-barrier) \
          (egcl-ext:store-barrier) \
          (list x (egcl-ext:memory-barrier) (egcl-ext:memory-barrier :read))) \
        (dotimes (i 40) (fenced i)) \
        (format t \"RESULT ~S ~A~%\" (fenced 7) (egcl-ext:function-tier (quote fenced))))";
    for (tier, expected_tier) in [("t0", "0"), ("t1", "1"), ("t2", "2")] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "egcl failed at {tier}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = stdout
            .lines()
            .find(|l| l.starts_with("RESULT"))
            .unwrap_or("");
        assert!(
            line.starts_with("RESULT (7 NIL NIL) "),
            "tier {tier}: {line:?}"
        );
        if cfg!(any(target_arch = "x86_64", target_arch = "s390x")) {
            assert_eq!(
                line,
                format!("RESULT (7 NIL NIL) {expected_tier}"),
                "the fenced function must reach {tier}"
            );
        }
    }
}
