// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap lexical environments (Backpack, bliss-gg3i1.7.15.2/.4/.5).
//!
//! Every binding shape the compile-time rewrite lays out must give the
//! tree-walker's answers at T0, T1 and T2, under a moving collector that runs
//! on every allocation with poisoned nursery memory, and from a compiled
//! BFASL. An eligible unit must do no name-keyed environment operation at any
//! tier: the access counters see slot operations only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/heap_env.lisp")
}

const EXPECTED: &[&str] = &[
    "adder=15",
    "pair=(7 8)",
    "counter=12",
    "let*=(9 100)",
    "let=(7 7)",
    "flet=(21 20)",
    "labels=(NIL T 28 T)",
    "optional=((1 2 3) (1 5 6) (1 5 :C))",
    "key=((1 2) (1 9))",
    "loops=((2 1 0) 3 (C B A) (1 4 9))",
    "accumulators=60",
    "special=(:BOUND 1)",
    "symbol-macro=12",
    "nested=(2 20 2 5 6 7)",
    "late-assign=42",
    "counters=(200 201 202)",
];

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "egcl-heap-env-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str], tier: &str, envs: &[(&str, &str)]) -> String {
    let mut command = Command::new(BIN);
    command
        .args(["--no-init"])
        .args(args)
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_THREADS", "1")
        .env_remove("EGCL_HEAP_ENV")
        .env_remove("EGCL_HEAP_ENV_LIMIT")
        .env_remove("EGCL_HEAP_ENV_SKIP")
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT");
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier={tier} {envs:?}: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn report(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('=') && !line.starts_with(';'))
        .collect()
}

fn assert_expected(stdout: &str, context: &str) {
    assert_eq!(report(stdout), EXPECTED, "{context}:\n{stdout}");
}

#[test]
fn every_binding_shape_agrees_at_every_tier() {
    let fixture = fixture();
    let fixture = fixture.to_str().unwrap();
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_expected(&run(&["--load", fixture], tier, &[]), tier);
    }
}

/// Recursive LABELS functions stay on the name-keyed frames by default (see
/// `heap_env::labels_enabled`); their heap layout must still be right.
#[test]
fn recursive_labels_on_heap_environments_agree_at_every_tier() {
    let fixture = fixture();
    let fixture = fixture.to_str().unwrap();
    for tier in ["t0", "t1", "t2"] {
        assert_expected(
            &run(&["--load", fixture], tier, &[("EGCL_HEAP_ENV_LABELS", "1")]),
            &format!("{tier} with heap LABELS"),
        );
    }
}

/// A collection on every allocation after start-up, with the freed nursery
/// poisoned: a heap environment or a captured value held anywhere the
/// collector cannot see would be read stale. Output must not change.
#[test]
fn every_binding_shape_survives_gc_stress() {
    let fixture = fixture();
    let program = format!(
        "(progn (defvar *hv-warm* 2) (load {:?}))",
        fixture.to_str().unwrap()
    );
    let stress = [
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ("EGCL_GC_POISON", "1"),
    ];
    for tier in ["t0", "t2"] {
        assert_expected(
            &run(&["--eval", &program], tier, &stress),
            &format!("{tier} under GC stress"),
        );
    }
}

/// `(egcl::%env-access-counts)` is (name-ops name-hops slot-ops slot-hops
/// fallbacks). An eligible unit's closures read and write their captured
/// state with slot operations only, at every tier.
#[test]
fn eligible_units_do_no_name_operations() {
    let program = r#"
      (defun make-acc (n)
        (let ((total 0))
          (lambda (k) (incf total (+ k n)))))
      (defun nested-reader (a)
        (let ((b (* a 2)))
          (lambda () (let ((c 1)) (funcall (lambda () (+ a b c)))))))
      (defvar *acc* (make-acc 1))
      (defvar *reader* (nested-reader 5))
      (dotimes (i 200) (funcall *acc* i) (funcall *reader*))
      (egcl::%env-access-counts t)
      (dotimes (i 100) (funcall *acc* i) (funcall *reader*))
      (let ((counts (egcl::%env-access-counts)))
        (egcl::%env-access-counts nil)
        (format t "NAME-OPS=~D~%SLOT-OPS=~D~%" (first counts) (third counts)))
    "#;
    for tier in ["t0", "t1", "t2"] {
        let out = run(&["--eval", &format!("(progn {program})")], tier, &[]);
        assert!(out.contains("NAME-OPS=0"), "tier={tier}: {out}");
        let slot_ops: u64 = out
            .lines()
            .find_map(|line| line.trim().strip_prefix("SLOT-OPS="))
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("tier={tier}: no slot count in {out}"));
        assert!(
            slot_ops >= 300,
            "tier={tier}: {slot_ops} slot operations\n{out}"
        );
    }
}

/// A heap-environment unit survives COMPILE-FILE and loads into a fresh
/// process from its BBU, at T0 and T2.
#[test]
fn heap_environment_units_round_trip_through_compile_file() {
    let dir = scratch();
    let source = dir.join("heap-env.lisp");
    let artifact = dir.join("heap-env.fasl");
    let text = fs::read_to_string(fixture()).unwrap();
    // Compile the definitions only; the run loop executes after loading.
    let definitions = text.split(";; Run enough times").next().unwrap().to_owned();
    fs::write(&source, definitions).unwrap();
    run(
        &[
            "--eval",
            &format!(
                "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
                source.to_str().unwrap(),
                artifact.to_str().unwrap()
            ),
        ],
        "",
        &[],
    );
    let bytes = fs::read(&artifact).unwrap();
    assert!(
        bytes.windows(4).any(|window| window == b"BBU\0"),
        "the artifact must carry bytecode"
    );
    fs::remove_file(&source).unwrap();
    let program = format!(
        "(progn (load {:?}) (dotimes (i 30) (let ((*standard-output* (make-broadcast-stream))) (hv-run))) (hv-run))",
        artifact.to_str().unwrap()
    );
    for tier in ["t0", "t2"] {
        assert_expected(
            &run(&["--eval", &program], tier, &[]),
            &format!("bfasl {tier}"),
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
