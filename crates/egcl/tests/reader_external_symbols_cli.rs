// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::path::PathBuf;
use std::process::Command;

struct RejectedSource(PathBuf);

impl Drop for RejectedSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn single_colon_requires_external_symbols_without_interning() {
    let file = RejectedSource(
        std::env::temp_dir().join(format!("egcl-reader-external-{}.lisp", std::process::id())),
    );
    std::fs::write(&file.0, "GH763-TARGET:FROM-FILE\n").unwrap();
    let path = file
        .0
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let cases =
        include_str!("fixtures/reader-external-symbols.lisp").replace("@REJECT-FILE@", &path);
    for interpreted in [false, true] {
        for stress in [false, true] {
            // The direct body exercises bytecode lowering; EVAL exercises the
            // tree evaluator. Both execute real package lookups and READ/LOAD.
            let program = if interpreted {
                format!("(eval '(progn {cases}))")
            } else {
                cases.clone()
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
            let output = command.output().expect("run external-symbol reader cases");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "interpreted={interpreted}, stress={stress}, status={:?}\n{stdout}\n{stderr}",
                output.status
            );
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.contains("READER-EXTERNAL-SYMBOLS-OK"))
                    .count(),
                1,
                "interpreted={interpreted}, stress={stress}\n{stdout}\n{stderr}"
            );
        }
    }
}
