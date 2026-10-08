// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A self-recursive function that also calls other functions keeps its T2
//! register entry on s390x (bliss-of8kz), provided no heap value is live
//! across any call. A call's own arguments count as live, so a fresh heap
//! value can never be passed from such a body; a heap CONSTANT can, since
//! constants are rematerialized from the rooted pool rather than tracked as
//! roots. Its non-self calls stage their arguments in the native frame, which
//! the collector cannot see, so the slice adapter copies them into rooted
//! storage first. The second test passes a string literal through such a call
//! with a collection forced every few allocations and freed memory poisoned.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    assert!(
        out.status.success(),
        "egcl failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.starts_with("RESULT"))
            .unwrap_or("")
            .to_string(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The helper's argument and result are fixnums, so nothing is a root across
/// the call and the body qualifies for the register entry.
#[test]
#[cfg_attr(
    not(target_arch = "s390x"),
    ignore = "the register entry with non-self calls is s390x's"
)]
fn body_with_other_calls_keeps_its_register_entry() {
    let program = "(progn \
        (defun helper (x) (* x 2)) \
        (defun walk (n acc) (if (= n 0) acc (walk (1- n) (+ acc (helper n))))) \
        (dotimes (i 40) (walk 5 0)) \
        (format t \"RESULT ~A ~A~%\" (walk 100 0) (egcl-ext:function-tier (quote walk))))";
    let (line, stderr) = run(
        program,
        &[
            ("EGCL_FORCE_TIER", "t2"),
            ("EGCL_LAZY_COMPILE", "0"),
            ("EGCL_T2_LOG", "stderr"),
        ],
    );
    assert_eq!(line, "RESULT 10100 2");
    let install = stderr
        .lines()
        .find(|l| l.contains("WALK: T2 INSTALLED"))
        .unwrap_or_else(|| panic!("WALK must install T2 code:\n{stderr}"));
    assert!(
        !install.contains("compiled_entry=+0,"),
        "WALK must keep its register entry despite calling HELPER: {install}"
    );
}

/// A string literal is staged in the native frame and copied into rooted
/// storage by the adapter; the answer must survive collections fired by the
/// callee. Stride 5 keeps the two forced-tier runs under a minute each on a
/// z17 while still collecting thousands of times.
#[test]
fn heap_constant_argument_survives_gc_from_a_register_entry_body() {
    let program = "(progn \
        (defun helper (x) (length (concatenate (quote string) x x))) \
        (defun walk (n acc) (if (= n 0) acc (walk (1- n) (+ acc (helper \"abc\"))))) \
        (dotimes (i 40) (walk 5 0)) \
        (format t \"RESULT ~A ~A~%\" (walk 100 0) (egcl-ext:function-tier (quote walk))))";
    for tier in ["t1", "t2"] {
        let (line, _) = run(
            program,
            &[("EGCL_FORCE_TIER", tier), ("EGCL_LAZY_COMPILE", "0")],
        );
        assert_eq!(line, format!("RESULT 600 {}", &tier[1..]), "tier {tier}");
        let (line, _) = run(
            program,
            &[
                ("EGCL_FORCE_TIER", tier),
                ("EGCL_LAZY_COMPILE", "0"),
                ("EGCL_GC_STRESS", "5"),
                ("EGCL_GC_POISON", "1"),
            ],
        );
        assert_eq!(
            line,
            format!("RESULT 600 {}", &tier[1..]),
            "tier {tier} under GC stress"
        );
    }
}
