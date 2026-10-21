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

#[test]
fn heap_size_zero_and_unreservable_sizes_are_reported() {
    let out = egcl(&["--heap-size", "0", "--eval", "1"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("heap size must be greater than 0"),
        "{out:?}"
    );
    let out = egcl(&["--eval", "1"], &[("EGCL_HEAP_SIZE", "0")]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("heap size must be greater than 0"),
        "{out:?}"
    );
    // A pebibyte is beyond any x86-64 address space, so the reservation fails
    // on every machine. It used to panic in whatever allocated first.
    let out = egcl(&["--heap-size", "1048576G", "--eval", "1"], &[]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot reserve 1125899906842624 bytes for the heap"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn exhaustion_in_compiled_code_signals_storage_condition() {
    // BUILD is called enough times to run as native code before the call that
    // exhausts the heap. The pending signal then arrives at a poll inside
    // native code; it surfaced as "invalid native transfer capture".
    let build = "(defun build (n)
                   (let ((l nil)) (dotimes (i n) (setq l (cons i l))) l))";
    let warm = "(dotimes (i 20) (build 10))";
    let program = "(print (handler-case (length (build 10000000))
                     (storage-condition (c) (list :caught (type-of c)))))";
    let out = stdout_of(&egcl(
        &[
            "--heap-size",
            "64M",
            "--eval",
            build,
            "--eval",
            warm,
            "--eval",
            program,
        ],
        &[],
    ));
    assert!(out.contains("(:CAUGHT STORAGE-CONDITION)"), "{out}");

    // Exhaustion while calling the compiled function from a loop, with
    // recovery afterwards.
    let program = "(progn
       (print (handler-case (let ((acc nil)) (loop (push (build 100000) acc)))
                (storage-condition (c) (list :caught (type-of c)))))
       (print (length (build 100000))))";
    for heap in ["16M", "64M"] {
        let out = stdout_of(&egcl(
            &[
                "--heap-size",
                heap,
                "--eval",
                build,
                "--eval",
                warm,
                "--eval",
                program,
            ],
            &[],
        ));
        assert!(out.contains("(:CAUGHT STORAGE-CONDITION)"), "{heap}: {out}");
        assert!(out.contains("100000"), "{heap}: {out}");
    }
}

#[test]
fn heap_recovers_when_handled_data_is_dropped_in_a_small_heap() {
    // After the handler drops the data, the old generation is garbage and the
    // nursery holds the next large allocation's live list. A minor collection
    // cannot place it until a major collection frees the old regions; it used
    // to report out-of-memory again instead of collecting.
    let program = "(progn
       (print (handler-case
                  (let ((l nil)) (loop (push (make-list 500 :initial-element 1) l)))
                (storage-condition (c) (list :caught (type-of c)))))
       (print (length (make-list 100000))))";
    for heap in ["16M", "32M"] {
        let out = stdout_of(&egcl(
            &["--heap-size", heap, "--eval", program],
            &[
                ("EGCL_T0_T1_THRESHOLD", "4000000000"),
                ("EGCL_OSR_THRESHOLD", "4000000000"),
            ],
        ));
        assert!(out.contains("(:CAUGHT STORAGE-CONDITION)"), "{heap}: {out}");
        assert!(out.contains("100000"), "{heap}: {out}");
    }
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
