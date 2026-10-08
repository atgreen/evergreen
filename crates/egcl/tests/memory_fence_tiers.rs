// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `EGCL-EXT:MEMORY-BARRIER` and its load/store variants lower to the
//! `MemoryFence` bytecode on the targets whose native emitters compile it,
//! and a function using them must reach T1 and T2 there with the same answer
//! the interpreter gives. A full native test run on s390x found the lowering
//! gated to x86-64, so the bfasl persistence test failed and the barrier was
//! a generic call in native code.

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
