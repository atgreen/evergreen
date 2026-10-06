// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#[cfg(target_arch = "x86_64")]
#[test]
fn constant_eq_values_and_branches_reach_t2() {
    let cases = [
        ("same", "(eq :same :same)", "T"),
        ("different", "(eq :left :right)", "NIL"),
        ("nil", "(eq nil nil)", "T"),
        ("boolean", "(eq nil t)", "NIL"),
        ("fixnum", "(eq 7 7)", "T"),
        ("character", "(eq #\\a #\\b)", "NIL"),
        ("true-branch", "(if (eq :same :same) (+ x 1) (+ x 2))", "42"),
        (
            "false-branch",
            "(if (eq :left :right) (+ x 1) (+ x 2))",
            "43",
        ),
        (
            "live-value",
            "(list x (eq :same :same) x (eq :left :right) x)",
            "(41 T 41 NIL 41)",
        ),
    ];
    let mut program = String::new();
    for (name, body, _) in cases {
        program.push_str(&format!(
            "(defun constant-eq-{name} (x) {body})
             (dotimes (i 128) (constant-eq-{name} 41))
             (format t \"BEGIN:{name}~%\")
             (disassemble #'constant-eq-{name})
             (format t \"RESULT:{name}:~S~%\" (constant-eq-{name} 41))
             (format t \"END:{name}~%\")"
        ));
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "t2")
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run constant EQ native checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for (name, _, expected) in cases {
        let section = stdout
            .split(&format!("BEGIN:{name}\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("missing {name}: {stdout}\n{stderr}"))
            .split(&format!("END:{name}\n"))
            .next()
            .unwrap();
        assert!(
            section.contains("; T2"),
            "{name} did not reach T2: {section}"
        );
        assert!(
            section
                .lines()
                .any(|line| line == format!("RESULT:{name}:{expected}")),
            "{name}: {section}"
        );
    }
}
