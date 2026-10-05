// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::path::Path;
use std::process::Command;

#[test]
fn drakma_stream_and_type_prerequisites() {
    let scenarios = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/drakma");
    for backend in ["bytecode", "tree-walker"] {
        for (file, marker) in [
            ("type-aliases.lisp", "ARRAY-TYPE-ALIASES-OK"),
            ("byte-types.lisp", "BYTE-TYPES-OK"),
            ("gray-eof.lisp", "GRAY-EOF-OK"),
            ("gray-sequence.lisp", "GRAY-SEQUENCE-OK"),
        ] {
            let output = Command::new("timeout")
                .args(["--kill-after=5", "60", env!("CARGO_BIN_EXE_egcl")])
                .args(["--no-init", "--load"])
                .arg(scenarios.join(file))
                .env("EGCL_BACKEND", backend)
                .output()
                .expect("run Drakma prerequisite regression");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{backend} {file}: {stdout}\n{stderr}");
            assert!(stdout.lines().any(|line| line == marker), "{stdout}\n{stderr}");
        }
    }
}
