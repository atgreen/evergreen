// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

use egcl_compiler::reader::read_from_string;
use egcl_rt::image::{load_image, validate_image_header};
use egcl_rt::value::NIL;
use egcl_stdlib::format::format as egcl_format;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn read(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn assert_has(haystack: &str, needle: &str, context: &str) {
    assert!(
        haystack.contains(needle),
        "{context} should contain {needle:?}, got:\n{haystack}"
    );
}

fn cargo_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// The `--target` this test binary was built for, so a nested cargo run
/// builds and runs the same target instead of the host default: the triple
/// is the path segment before the profile directory in CARGO_BIN_EXE_egcl
/// (`.../<triple>/<profile>/egcl`), which `.cargo/config.toml`'s pinned
/// target and an explicit `--target` both produce. Absent that segment the
/// nested run inherits the default, as before.
fn cargo_target_args() -> Vec<String> {
    let bin = Path::new(env!("CARGO_BIN_EXE_egcl"));
    bin.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| name.matches('-').count() >= 2 && *name != "target")
        .map(|triple| vec!["--target".to_string(), triple.to_string()])
        .unwrap_or_default()
}

fn egcl_bin_path() -> &'static Path {
    // Cargo builds the `egcl` binary for this test's own target and profile
    // before running the test and hands its path over in CARGO_BIN_EXE_egcl.
    // Rebuilding here with a nested `cargo build -p egcl` added nothing and
    // broke every native non-x86-64 run: the nested build targets the host
    // default (the musl x86-64 pin in .cargo/config.toml) and fails outright.
    Path::new(env!("CARGO_BIN_EXE_egcl"))
}

fn run(mut command: Command, context: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("{context}: {e}"));
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn expected_markers(script: &str) -> Vec<String> {
    read(script)
        .lines()
        .filter_map(|line| line.strip_prefix("; EXPECT: "))
        .map(str::to_string)
        .collect()
}

#[test]
fn ansi_expected_failures_and_ci_contracts() {
    // Per R10.01 and R10.02, ANSI expected failures are tracked and gated in CI on main.
    // Per R10.17 and R10.18, GitHub Actions must run cargo check/test/clippy and ansi-test
    // on Linux x86-64 and macOS aarch64.
    // Per R5.48, R5.49, and R5.50, the standard-library validation layer must
    // track ANSI conformance, EGCL-specific regression coverage, and the
    // benchmark/validation contracts that CI enforces.
    let expected = read("tests/ansi-test/expected-failures.txt");
    assert_has(&expected, "ANSI expected failures", "expected-failures.txt");
    assert_has(&expected, "R10.01", "expected-failures.txt rationale");

    let ci = read(".github/workflows/ci.yml");
    for needle in [
        "branches: [main]",
        "cargo check --workspace",
        "cargo test --workspace",
        "ubuntu-latest",
    ] {
        assert_has(&ci, needle, "ci.yml");
    }
    // Follow the actual command chain: CI runs the gate driver, which invokes
    // the Makefile target carrying -D warnings. The driver's failure-injection
    // test checks that failures propagate while the remaining gates still run.
    assert_has(&ci, "run: bash scripts/ci-lint.sh", "ci.yml");
    let driver = read("scripts/ci-lint.sh");
    assert_has(&driver, "make clippy", "lint driver");
    assert_has(
        &driver,
        "cargo test -p egcl --test spec_validation_infra ansi_expected_failures_and_ci_contracts",
        "ANSI expectations contract command",
    );
    assert_has(
        &read("Makefile"),
        "clippy --workspace --all-targets -- -D warnings",
        "Makefile clippy target",
    );
    // NOT asserted: a macOS runner. The leg was removed deliberately — there is
    // no macOS target in the port set, and .cargo/config.toml pins the musl
    // target unconditionally, so a macOS job cannot link. ci.yml's header
    // records this. Re-add the assertion only alongside a real macOS target.
}

#[test]
fn integration_acceptance_scripts_execute_through_real_cli_entrypoints() {
    // Per R10.04 and R10.06, integration tests must drive end-to-end CL
    // evaluation through the real CLI/REPL and assert observable output.
    // Per R10.70, the checked-in integration scripts define their expectations
    // via EXPECT annotations that the runner must honor.
    let eval_script = "tests/integration/acceptance_eval.lisp";
    let eval_expected = expected_markers(eval_script);
    let eval = run(
        {
            let mut cmd = Command::new(egcl_bin_path());
            cmd.current_dir(repo_root()).args(["--load", eval_script]);
            cmd
        },
        "run acceptance_eval.lisp",
    );
    let eval_stdout = String::from_utf8_lossy(&eval.stdout);
    for expected in &eval_expected {
        assert!(
            eval_stdout.contains(expected),
            "acceptance_eval.lisp missing expected output {expected:?}: {eval_stdout}"
        );
    }

    let load_script = "tests/integration/acceptance_load.lisp";
    let load_expected = expected_markers(load_script);
    let load = run(
        {
            let mut cmd = Command::new(egcl_bin_path());
            cmd.current_dir(repo_root()).args(["--load", load_script]);
            cmd
        },
        "run acceptance_load.lisp",
    );
    let load_stdout = String::from_utf8_lossy(&load.stdout);
    for expected in &load_expected {
        assert!(
            load_stdout.contains(expected),
            "acceptance_load.lisp missing expected output {expected:?}: {load_stdout}"
        );
    }

    let repl_script = "tests/integration/acceptance_repl.lisp";
    let repl_expected = expected_markers(repl_script);
    let script_body = read(repl_script);
    let mut repl = Command::new(egcl_bin_path());
    // --no-init keeps the REPL test hermetic against the developer's ~/.egclrc,
    // which would otherwise inject its output/latency (and any errors) into this
    // stdin-driven REPL session.
    repl.arg("--no-init")
        .current_dir(repo_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = repl.spawn().expect("spawn REPL");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(format!("{script_body}\n(quit)\n").as_bytes())
        .expect("write REPL script");
    let repl_output = child.wait_with_output().expect("wait for REPL");
    assert!(
        repl_output.status.success(),
        "run acceptance_repl.lisp failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&repl_output.stdout),
        String::from_utf8_lossy(&repl_output.stderr)
    );
    let repl_stdout = String::from_utf8_lossy(&repl_output.stdout);
    for expected in &repl_expected {
        assert!(
            repl_stdout.contains(expected),
            "acceptance_repl.lisp missing expected output {expected:?}: {repl_stdout}"
        );
    }
}

#[test]
fn regression_inputs_are_executable_through_standard_test_entrypoints() {
    // Per R10.16 and R10.34, minimized crash inputs must be checked in and
    // re-run by the normal cargo-test workflow rather than existing only as
    // inert repository artifacts.
    let reader_regression =
        fs::read(repo_root().join("fuzz/regression/fuzz_reader/crash-min-001.lisp"))
            .expect("reader regression input");
    let reader_source = String::from_utf8_lossy(&reader_regression);
    let _ = read_from_string(reader_source.as_ref());

    let compile_regression =
        fs::read(repo_root().join("fuzz/regression/fuzz_compile/crash-min-001.lisp"))
            .expect("compile regression input");
    let compile_source = String::from_utf8_lossy(&compile_regression);
    let _ = read_from_string(compile_source.as_ref());

    let eval_regression =
        fs::read(repo_root().join("fuzz/regression/fuzz_eval/crash-min-001.lisp"))
            .expect("eval regression input");
    let eval_source = String::from_utf8_lossy(&eval_regression);
    let _ = read_from_string(eval_source.as_ref());

    let format_regression =
        fs::read_to_string(repo_root().join("fuzz/regression/fuzz_format/crash-min-001.lisp"))
            .expect("format regression input");
    let _ = egcl_format(NIL, &format_regression, &[]);

    let image_regression = repo_root().join("fuzz/regression/fuzz_image_load/crash-min-001.bimg");
    let _ = validate_image_header(image_regression.to_str().expect("utf8 path"));
    let _ = load_image(image_regression.to_str().expect("utf8 path"));
}

#[test]
fn stress_and_regression_scenarios_run_via_real_test_binaries() {
    // Per R10.07 and R10.08, GC stress paths and thread-safety scenarios must
    // execute real collector and concurrent mutation code paths.
    // Per R10.20, R10.21, and R10.22, regression coverage must execute the
    // deoptimisation and image round-trip tests through the standard runner.
    let _guard = cargo_lock().lock().unwrap_or_else(|e| e.into_inner());

    run(
        {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(repo_root())
                .arg("test")
                .args(cargo_target_args())
                .args([
                "-p",
                "egcl-rt",
                "--test",
                "spec_memory_gc",
                "spec_gc_large_objects_minor_gc_and_full_gc_use_real_collector_paths",
                "--",
                "--exact",
                "--nocapture",
            ]);
            cmd
        },
        "run GC stress regression",
    );

    run(
        {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(repo_root())
                .arg("test")
                .args(cargo_target_args())
                .args([
                "-p",
                "egcl-stdlib",
                "--test",
                "spec_packages_bootstrap",
                "concurrent_bootstrap_and_mutation_on_separate_threads_remain_isolated",
                "--",
                "--exact",
                "--nocapture",
            ]);
            cmd
        },
        "run package concurrency regression",
    );

    run(
        {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(repo_root())
                .arg("test")
                .args(cargo_target_args())
                .args([
                "-p",
                "egcl-rt",
                "--test",
                "spec_image_ops",
                "spec_image_round_trip_restores_heap_and_entry_state",
                "--",
                "--exact",
                "--nocapture",
            ]);
            cmd
        },
        "run image round-trip regression",
    );

    let output = run(
        {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(repo_root())
                .arg("test")
                .args(cargo_target_args())
                .args([
                "-p",
                "egcl-compiler",
                "--lib",
                "t2::deopt::tests::lowers_and_reconstructs_every_inlined_scope_in_order",
                "--",
                "--exact",
                "--nocapture",
            ]);
            cmd
        },
        "run deoptimisation regression",
    );
    assert_has(
        &String::from_utf8_lossy(&output.stdout),
        "test result: ok. 1 passed; 0 failed; 0 ignored;",
        "the exact deoptimisation regression must execute, not merely match zero tests",
    );
}
