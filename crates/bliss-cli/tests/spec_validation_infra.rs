use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn ansi_expected_failures_and_ci_contracts() {
    // Per R10.01 and R10.02, ANSI expected failures are tracked and gated in CI on main.
    // Per R10.17 and R10.18, GitHub Actions must run cargo check/test/clippy and ansi-test
    // on Linux x86-64 and macOS aarch64.
    let expected = read("tests/ansi-test/expected-failures.txt");
    assert_has(&expected, "ANSI expected failures", "expected-failures.txt");
    assert_has(&expected, "R10.01", "expected-failures.txt rationale");

    let ci = read(".github/workflows/ci.yml");
    for needle in [
        "branches: [main]",
        "cargo check --workspace",
        "cargo test --workspace",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "ansi-test",
        "ubuntu-latest",
        "macos-14",
    ] {
        assert_has(&ci, needle, "ci.yml");
    }
}

#[test]
fn integration_acceptance_scripts_are_present_and_annotated() {
    // Per R10.04 and R10.06, tests/integration/ contains end-to-end evaluation scripts
    // with expected observable output through real user-facing entrypoints.
    // Per R10.20, R10.21, and R10.22, regression-oriented acceptance coverage must
    // include deopt/image-sensitive scenarios as observable scripts, not just unit seams.
    for script in [
        "tests/integration/acceptance_eval.lisp",
        "tests/integration/acceptance_load.lisp",
        "tests/integration/acceptance_repl.lisp",
    ] {
        let text = read(script);
        assert_has(&text, "EXPECT:", script);
        assert_has(&text, "(format t", script);
    }
}

#[test]
fn fuzz_targets_corpora_and_regressions_match_spec_contracts() {
    // Per R10.13, R10.14, and R10.15, reader/compiler/eval/ffi/image fuzzing is required.
    // Per R10.16, R10.23, R10.24, R10.25, R10.26, R10.28, R10.29, R10.31, R10.32,
    // R10.33, R10.34, and R10.35, dedicated targets, checked-in corpora, regression
    // inputs, coverage, minimisation, and crash-triage workflow artifacts must exist.
    let targets = [
        ("fuzz/fuzz_targets/fuzz_reader.rs", "read_from_string"),
        ("fuzz/fuzz_targets/fuzz_macroexpand.rs", "macroexpand"),
        ("fuzz/fuzz_targets/fuzz_compile.rs", "codegen"),
        ("fuzz/fuzz_targets/fuzz_eval.rs", "compare_eval_outputs"),
        ("fuzz/fuzz_targets/fuzz_format.rs", "format_to_string"),
        ("fuzz/fuzz_targets/fuzz_ffi.rs", "call_foreign"),
        ("fuzz/fuzz_targets/fuzz_image_load.rs", "load_image"),
    ];
    for (path, needle) in targets {
        let text = read(path);
        assert_has(&text, "fuzz_target!", path);
        assert_has(&text, needle, path);
    }

    for dir in [
        "fuzz/corpus/fuzz_reader",
        "fuzz/corpus/fuzz_compile",
        "fuzz/corpus/fuzz_eval",
        "fuzz/corpus/fuzz_format",
        "fuzz/corpus/fuzz_ffi",
        "fuzz/corpus/fuzz_image_load",
        "fuzz/regression/fuzz_reader",
        "fuzz/regression/fuzz_compile",
        "fuzz/regression/fuzz_eval",
        "fuzz/regression/fuzz_format",
        "fuzz/regression/fuzz_ffi",
        "fuzz/regression/fuzz_image_load",
    ] {
        let mut entries = fs::read_dir(repo_root().join(dir))
            .unwrap_or_else(|e| panic!("{dir}: {e}"))
            .filter_map(Result::ok);
        assert!(entries.next().is_some(), "{dir} should not be empty");
    }

    let nightly = read(".github/workflows/nightly-fuzz.yml");
    for needle in [
        "cargo fuzz run fuzz_reader -- -max_total_time=60",
        "cargo fuzz cmin fuzz_reader fuzz/corpus/fuzz_reader",
        "cargo llvm-cov",
        "RUSTFLAGS: -C instrument-coverage",
    ] {
        assert_has(&nightly, needle, "nightly-fuzz.yml");
    }
}

#[test]
fn sanitizer_and_differential_artifacts_are_checked_in() {
    // Per R10.07 and R10.08, the integration runner needs stress and concurrency hooks.
    // Per R10.09, R10.10, R10.11, and R10.12, differential/perf artifacts are archived.
    // Per R10.19, nightly sanitizer workflows and suppression files must exist.
    let sanitizers = read(".github/workflows/sanitizers.yml");
    for needle in [
        "sanitizer=address",
        "sanitizer=thread",
        "sanitizer=memory",
        "miri test",
    ] {
        assert_has(&sanitizers, needle, "sanitizers.yml");
    }

    for supp in [
        "tests/sanitizers/asan.supp",
        "tests/sanitizers/msan.supp",
        "tests/sanitizers/tsan.supp",
    ] {
        assert_has(&read(supp), "interceptor", supp);
    }

    let known_diffs = read("tests/differential/known-diffs.toml");
    assert_has(&known_diffs, "sbcl_reference", "known-diffs.toml");
    assert_has(&known_diffs, "image_round_trip", "known-diffs.toml");
}
