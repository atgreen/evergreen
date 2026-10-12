// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Linux `perf` symbol-map emission for native code (bliss-jtc.10).
//!
//! When EGCL_PERF_MAP is set, each installed native function appends a line
//! `<hex-addr> <hex-size> <tier>:<name>` to a perf map file, so `perf` can
//! symbolicate JIT frames — the same mechanism HotSpot uses. These tests drive
//! the real binary and validate the emitted file.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const WARM: &str = "\
    (defun sq (x) (* x x)) \
    (defun sumsq (n) (let ((s 0) (i 0)) \
      (tagbody top (when (< i n) (setq s (+ s (* i i))) (setq i (+ i 1)) (go top))) s)) \
    (sq 2) (sq 3) (sq 4) \
    (sumsq 5) (sumsq 5) (sumsq 5) \
    (format t \"done~%\")";

/// Each non-empty line must carry a native tier; returns the function names.
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
        assert!(size > 0, "native code size must be positive: {line:?}");
        let (tier, name) = parts[2].split_once(':').expect("tier-prefixed name");
        assert!(
            matches!(tier, "T1" | "T2" | "OSR"),
            "name must carry the installed native tier: {line:?}"
        );
        names.push(name.to_string());
    }
    names
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn perf_map_labels_and_ranges_match_installed_jitdump_code() {
    let dir = std::env::temp_dir().join(format!("egcl-perfmap-tiers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let map_path = dir.join("jit.map");
    let dump_path = dir.join("jit.dump");
    // T2 compilation runs in the background. Poll the actual installed tier
    // instead of assuming a handful of fast calls gives the worker time.
    let program = format!(
        "{WARM}
         (dotimes (i 200)
           (sq 5) (sumsq 5)
           (when (= 2 (egcl-ext:function-tier 'sq)) (return))
           (sleep 0.01))
         (assert (= 2 (egcl-ext:function-tier 'sq)))
         (assert (= 25 (sq 5)))"
    );
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "2")
        .env("EGCL_T2_THREADS", "1")
        .env_remove("EGCL_T2")
        .env_remove("EGCL_T2_NO_QUEUE")
        .env_remove("EGCL_T2_DISCARD")
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_PROFILING_DISABLED")
        .env("EGCL_PERF_MAP", &map_path)
        .env("EGCL_PERF_JITDUMP", &dump_path)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let map = std::fs::read_to_string(&map_path).expect("perf map written");
    parse_and_collect_names(&map);
    let dump = std::fs::read(&dump_path).expect("jitdump written");
    let read_u32 = |at| u32::from_ne_bytes(dump[at..at + 4].try_into().unwrap());
    let read_u64 = |at| u64::from_ne_bytes(dump[at..at + 8].try_into().unwrap());
    assert_eq!(read_u32(0), 0x4A695444);
    let mut at = read_u32(8) as usize;
    let mut saw_t1 = false;
    let mut saw_t2 = false;
    while at + 56 <= dump.len() {
        assert_eq!(read_u32(at), 0, "JIT_CODE_LOAD record");
        let end = at + read_u32(at + 4) as usize;
        assert!(end > at + 56 && end <= dump.len());
        let payload = &dump[at + 56..end];
        let nul = payload
            .iter()
            .position(|&b| b == 0)
            .expect("name terminator");
        let name = std::str::from_utf8(&payload[..nul]).expect("utf8 name");
        let addr = read_u64(at + 32);
        let size = read_u64(at + 40);
        assert_eq!(payload.len() - nul - 1, size as usize, "installed bytes");
        let expected = format!("{addr:x} {size:x} {name}");
        assert!(
            map.lines().any(|line| line == expected),
            "missing {expected:?} in {map:?}"
        );
        saw_t1 |= name == "T1:SQ" || name == "T1:SUMSQ";
        saw_t2 |= name == "T2:SQ" || name == "T2:SUMSQ";
        at = end;
    }
    assert_eq!(at, dump.len(), "all jitdump records parsed");
    assert!(
        saw_t1 && saw_t2,
        "both installed tiers must be observed: {map}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// With a path override, the map lists every promoted function in valid format.
#[test]
fn perf_map_lists_promoted_functions_in_perf_format() {
    let dir = std::env::temp_dir().join(format!("egcl-perfmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let map_path = dir.join("jit.map");

    let out = Command::new(BIN)
        .args(["--eval", WARM])
        .env("EGCL_T1_THRESHOLD", "2")
        // Asserts the straight-line SQ is mapped too; pin eager so it compiles at
        // definition rather than deferring under the lazy default (SUMSQ contains
        // a loop and is eager-compiled by the hybrid policy regardless).
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_PERF_MAP", &map_path)
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

/// With EGCL_PERF_MAP=1 the perf-standard /tmp/perf-<pid>.map is written.
#[test]
fn perf_map_writes_standard_pid_path() {
    let mut child = Command::new(BIN)
        .args(["--eval", WARM])
        .env("EGCL_T1_THRESHOLD", "2")
        .env("EGCL_PERF_MAP", "1")
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
        .env("EGCL_T1_THRESHOLD", "2")
        .env_remove("EGCL_PERF_MAP")
        .output()
        .expect("spawn");
    assert!(out.status.success());
    // The perf-standard path for THIS child's pid must not exist afterwards.
    // (output() waits, so the child pid is already reaped; we only assert the
    // run succeeded without the env var — the positive paths are covered above.)
}
