// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap sizing and heap exhaustion from the command line (GitHub #471).

use std::process::{Command, Output};

const HEAP_VARS: [&str; 3] = ["EGCL_HEAP_SIZE", "EGCL_HEAP_MB", "EGCL_GC_DISABLE"];

fn egcl(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    for var in HEAP_VARS {
        command.env_remove(var);
    }
    command
        .args(["--no-init"])
        .args(args)
        .envs(env.iter().copied());
    command.output().expect("run egcl")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The `regions: N total` count `room` reports; regions are 1 MiB.
fn regions(args: &[&str], env: &[(&str, &str)]) -> usize {
    let mut all = args.to_vec();
    all.extend(["--eval", "(room)"]);
    let out = stdout_of(&egcl(&all, env));
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("regions:"))
        .unwrap_or_else(|| panic!("no regions line in:\n{out}"));
    line.split_whitespace()
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("unparseable regions line: {line}"))
}

#[test]
fn heap_size_flag_and_environment_size_the_heap() {
    assert_eq!(regions(&[], &[]), 512);
    assert_eq!(regions(&["--heap-size", "64M"], &[]), 64);
    assert_eq!(regions(&[], &[("EGCL_HEAP_SIZE", "96m")]), 96);
    assert_eq!(regions(&[], &[("EGCL_HEAP_MB", "80")]), 80);
    // --heap-size, then EGCL_HEAP_SIZE, then EGCL_HEAP_MB.
    let both = [("EGCL_HEAP_SIZE", "96m"), ("EGCL_HEAP_MB", "80")];
    assert_eq!(regions(&["--heap-size", "64M"], &both), 64);
    assert_eq!(regions(&[], &both), 96);
    // Rounded up to whole regions and raised to the 16 MiB minimum.
    assert_eq!(regions(&["--heap-size", "70000k"], &[]), 69);
    assert_eq!(regions(&["--heap-size", "1"], &[]), 16);
}

#[test]
fn exhausted_heap_signals_storage_condition_and_recovers() {
    // Live data outgrows a 64 MiB heap. A minor collection used to panic
    // ("GC minor evacuation exhausted regions"); it must signal
    // STORAGE-CONDITION, and the program continues once the data is dropped.
    let program = "(progn
       (print (handler-case (let ((l nil)) (loop (setq l (cons 1 l))))
                (storage-condition (c) (list :caught (type-of c)))))
       (print (length (make-list 100000))))";
    let out = stdout_of(&egcl(&["--heap-size", "64M", "--eval", program], &[]));
    assert!(out.contains("(:CAUGHT STORAGE-CONDITION)"), "{out}");
    assert!(out.contains("100000"), "{out}");

    // The same through a builtin that allocates, in the evaluator.
    let program = "(progn
       (print (handler-case (let ((l nil)) (loop (push (make-list 1000) l)))
                (storage-condition () :caught)))
       (print (length (make-list 100000))))";
    let out = stdout_of(&egcl(
        &["--heap-size", "64M", "--eval", program],
        &[
            ("EGCL_T0_T1_THRESHOLD", "4000000000"),
            ("EGCL_OSR_THRESHOLD", "4000000000"),
        ],
    ));
    assert!(out.contains(":CAUGHT"), "{out}");
    assert!(out.contains("100000"), "{out}");
}

#[cfg(target_os = "linux")]
#[test]
fn restored_image_takes_its_heap_size_from_the_restoring_process() {
    let dir = std::env::temp_dir().join(format!("egcl-heap-size-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = dir.join("core");
    let save = format!(
        "(progn (defvar *big* (make-list 1000000 :initial-element 7))
                (egcl-ext:save-lisp-and-die {core:?}))"
    );
    stdout_of(&egcl(&["--heap-size", "1G", "--eval", &save], &[]));
    let core = core.to_str().unwrap();
    let probe = "(progn (print (length *big*)) (room))";

    // A larger heap than the saving process had, or a smaller one.
    let out = stdout_of(&egcl(
        &["--image", core, "--heap-size", "2G", "--eval", probe],
        &[],
    ));
    assert!(out.contains("1000000"), "{out}");
    assert!(out.contains("2048 total"), "{out}");
    let out = stdout_of(&egcl(
        &["--image", core, "--eval", probe],
        &[("EGCL_HEAP_SIZE", "256m")],
    ));
    assert!(out.contains("256 total"), "{out}");

    // A heap too small for the image grows to hold it rather than failing.
    let out = stdout_of(&egcl(
        &[
            "--image",
            core,
            "--heap-size",
            "16M",
            "--eval",
            "(progn (dotimes (i 20) (make-list 10000)) (print (length *big*)))",
        ],
        &[],
    ));
    assert!(out.contains("1000000"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
