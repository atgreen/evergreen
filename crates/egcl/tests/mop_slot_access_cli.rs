// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn custom_metaclasses_dispatch_all_slot_access_operations() {
    let fixture = include_str!("fixtures/mop-slot-access.lisp");
    let mut results = Vec::new();
    for interpreted in [false, true] {
        for stress in [false, true] {
            let program = if interpreted {
                format!("(eval '(progn {fixture}))")
            } else {
                fixture.to_owned()
            };
            let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
            command.args(["--no-init", "--eval", &program]);
            for name in [
                "EGCL_GC_STRESS",
                "EGCL_GC_POISON",
                "EGCL_GC_STRESS_AFTER_INIT",
                "EGCL_GC_STRESS_SKIP",
                "EGCL_GC_STRESS_AT",
            ] {
                command.env_remove(name);
            }
            if stress {
                command.envs([
                    ("EGCL_GC_STRESS", "1"),
                    ("EGCL_GC_POISON", "1"),
                    ("EGCL_GC_STRESS_AFTER_INIT", "1"),
                ]);
            }
            let output = command.output().expect("run custom slot access protocol");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success() && stdout.contains("MOP-SLOT-ACCESS-OK"),
                "interpreted={interpreted}, stress={stress}: {}\n{stdout}\n{stderr}",
                output.status
            );
            results.push(
                stdout
                    .lines()
                    .filter(|s| s.contains("MOP-SLOT-ACCESS-OK"))
                    .count(),
            );
        }
    }
    assert_eq!(results, vec![1; 4]);
}
