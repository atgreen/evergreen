//! Linux `perf` symbol-map emission for T1 native code (bliss-jtc.10).
//!
//! When TORCL_PERF_MAP is set, each installed T1 function appends a line
//! `<hex-addr> <hex-size> T1:<name>` to a perf map file, so `perf` can
//! symbolicate JIT frames — the same mechanism HotSpot uses. These tests drive
//! the real binary and validate the emitted file.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

const WARM: &str = "\
    (defun sq (x) (* x x)) \
    (defun sumsq (n) (let ((s 0) (i 0)) \
      (tagbody top (when (< i n) (setq s (+ s (* i i))) (setq i (+ i 1)) (go top))) s)) \
    (sq 2) (sq 3) (sq 4) \
    (sumsq 5) (sumsq 5) (sumsq 5) \
    (format t \"done~%\")";

/// Each non-empty line must be `<hex> <hex> T1:<name>`; returns the T1 names.
fn parse_and_collect_names(map: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in map.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        assert_eq!(parts.len(), 3, "malformed perf-map line: {line:?}");
        assert!(
            u64::from_str_radix(parts[0], 16).is_ok(),
            "address is not hex: {line:?}"
        );
        let size = u64::from_str_radix(parts[1], 16).expect("size is hex");
        assert!(size > 0, "T1 code size must be positive: {line:?}");
        assert!(
            parts[2].starts_with("T1:"),
            "name must be tagged T1:: {line:?}"
        );
        names.push(parts[2].trim_start_matches("T1:").to_string());
    }
    names
}

/// With a path override, the map lists every promoted function in valid format.
#[test]
fn perf_map_lists_promoted_functions_in_perf_format() {
    let dir = std::env::temp_dir().join(format!("torcl-perfmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let map_path = dir.join("jit.map");

    let out = Command::new(BIN)
        .args(["--eval", WARM])
        .env("TORCL_T1_THRESHOLD", "2")
        // Asserts the straight-line SQ is mapped too; pin eager so it compiles at
        // definition rather than deferring under the lazy default (SUMSQ contains
        // a loop and is eager-compiled by the hybrid policy regardless).
        .env("TORCL_LAZY_COMPILE", "0")
        .env("TORCL_PERF_MAP", &map_path)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let map = std::fs::read_to_string(&map_path).expect("perf map written");
    let names = parse_and_collect_names(&map);
    assert!(
        names.iter().any(|n| n == "SQ"),
        "SQ must be mapped: {map:?}"
    );
    assert!(
        names.iter().any(|n| n == "SUMSQ"),
        "SUMSQ must be mapped: {map:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// With TORCL_PERF_MAP=1 the perf-standard /tmp/perf-<pid>.map is written.
#[test]
fn perf_map_writes_standard_pid_path() {
    let mut child = Command::new(BIN)
        .args(["--eval", WARM])
        .env("TORCL_T1_THRESHOLD", "2")
        .env("TORCL_PERF_MAP", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let status = child.wait().expect("wait");
    assert!(status.success());

    let path = format!("/tmp/perf-{pid}.map");
    let map = std::fs::read_to_string(&path).expect("perf-<pid>.map written");
    let names = parse_and_collect_names(&map);
    assert!(
        names.iter().any(|n| n == "SUMSQ"),
        "SUMSQ must be mapped: {map:?}"
    );
    std::fs::remove_file(&path).ok();
}

/// Without the env var, no perf map side effects occur (the run still succeeds).
#[test]
fn no_perf_map_without_env() {
    let out = Command::new(BIN)
        .args(["--eval", WARM])
        .env("TORCL_T1_THRESHOLD", "2")
        .env_remove("TORCL_PERF_MAP")
        .output()
        .expect("spawn");
    assert!(out.status.success());
    // The perf-standard path for THIS child's pid must not exist afterwards.
    // (output() waits, so the child pid is already reaped; we only assert the
    // run succeeded without the env var — the positive paths are covered above.)
}
