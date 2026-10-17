// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A class the collector can reach through nothing but `class_meta`'s key must
//! stay alive and usable (bliss-nn6wx, a prerequisite of bliss-rj5o).
//!
//! `(setf (find-class 'x) nil)` is required by CLHS to remove the name
//! association. It drops the `class_registry` and `class_by_name` entries and
//! leaves the `class_meta` entry alone, so the class value is then reachable
//! only as that map's KEY — and the CLOS root scanner visited no key at all.
//! An obsolete pre-redefinition class reaches the same state.
//!
//! This passes before the scanner change as well as after, because a class
//! metaobject is an immediate today and marking one is a no-op. It is here to
//! pin the SCENARIO: once a class becomes a real heap object (bliss-vsjn1),
//! this is the shape that silently loses a class, and a plain saved core will
//! not show it — only a shaken one, which retains just what it can reach.

use std::fs;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// An orphaned class, exercised through an instance that outlives the name.
const PROGRAM: &str = r#"
  (defclass orphan () ((a :initform 7)))
  (defvar *i* (make-instance 'orphan))
  (defclass keeper () ((b :initform 9)))
  (defvar *k* (make-instance 'keeper))
  (setf (find-class 'orphan) nil)
  (format t "GONE ~a~%" (find-class 'orphan nil))
  ;; Churn the nursery so collections run while the class is key-only. Kept
  ;; small on purpose: under EGCL_GC_STRESS every one of these allocations
  ;; forces a collection at ~1.5-2ms (bliss-v2b6o), so a big loop here costs
  ;; minutes and measures the stress harness rather than the scanner.
  (dotimes (i 2000) (list i i i))
  (format t "SLOT ~a~%" (slot-value *i* 'a))
  (format t "NAME ~a~%" (class-name (class-of *i*)))
  ;; TYPE-OF answers the CLASS, not the name: `(setf (find-class 'orphan) nil)`
  ;; took away the name, and CLHS gives the class itself when it has no proper
  ;; name. This row said ORPHAN before bliss-vsjn1, when a class WAS its name
  ;; symbol and the two answers were the same object (ansi-test TYPE-OF.9 is
  ;; the same point).
  (format t "TYPE ~a~%" (eq (type-of *i*) (class-of *i*)))
  (format t "KEEPER ~a~%" (slot-value *k* 'b))
  ;; Deliberately NOT asserted here: that the old instance keeps slot A after
  ;; a new (defclass orphan ...) under the freed name. A conforming CL gives
  ;; the redefinition a DISTINCT class and leaves the old instance on the
  ;; orphaned one, but a class IS its name symbol here, so the redefinition
  ;; reuses the same class value -- measured, (eq (class-of *i*)
  ;; (find-class 'orphan)) => T -- and the old instance loses slot A. That is
  ;; bliss-rj5o, not this change, and it fails identically on main.
  (format t "ROOT-MARKING-OK~%")
"#;

fn run(env: &[(&str, &str)], args: &[&str]) -> (String, bool) {
    let mut command = Command::new("timeout");
    command.args(["--kill-after=10", "600", BIN]).args(args);
    command.env_remove("EGCL_GC_STRESS").env_remove("EGCL_GC_POISON");
    for (k, v) in env {
        command.env(k, v);
    }
    let output = command.output().expect("run egcl");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        output.status.success(),
    )
}

fn expect(text: &str, rows: &[&str]) {
    for row in rows {
        assert!(text.contains(row), "missing {row:?} in:\n{text}");
    }
}

const ROWS: &[&str] = &[
    "GONE NIL",
    "SLOT 7",
    "NAME ORPHAN",
    "TYPE T",
    "KEEPER 9",
    "ROOT-MARKING-OK",
];

#[test]
fn an_orphaned_class_stays_usable_through_its_instances() {
    let (text, ok) = run(&[], &["--no-init", "--eval", PROGRAM]);
    assert!(ok, "{text}");
    expect(&text, ROWS);
}

/// Under forced collections the scanner's key marking runs on essentially
/// every allocation, so this is where a mistake in it shows up.
///
/// `EGCL_GC_STRESS_AFTER_INIT` is deliberate: the bootstrap allocates ~100k
/// objects and stressing all of them costs minutes before the program runs
/// (bliss-v2b6o), and the bootstrap is not what this probes.
#[test]
fn an_orphaned_class_survives_forced_collections() {
    let (text, ok) = run(
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
        &["--no-init", "--eval", PROGRAM],
    );
    assert!(ok, "{text}");
    // Compared against the non-stress rows, not merely "it did not crash": a
    // value orphaned before it is stored makes the program compute a quietly
    // wrong answer with no segfault at all.
    expect(&text, ROWS);
}

/// The saved-core half. A plain core keeps everything the heap holds; the
/// point of recording this is that a SHAKEN core keeps only what
/// `reachable_heap_objects` can reach, which is where a key-only class would
/// be dropped while the restored `class_meta` still named it.
#[test]
fn an_orphaned_class_round_trips_through_a_saved_core() {
    let dir = std::env::temp_dir().join(format!("egcl-nn6wx-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let core = dir.join("orphan.core");
    let core_arg = core.display().to_string();

    let (text, ok) = run(
        &[],
        &[
            "--no-init",
            "--eval",
            r#"(progn
                 (defclass orphan () ((a :initform 7)))
                 (defvar *i* (make-instance 'orphan))
                 (setf (find-class 'orphan) nil))"#,
            "--eval",
            &format!("(egcl-ext:save-lisp-and-die {core_arg:?})"),
        ],
    );
    assert!(ok, "saving the core failed:\n{text}");

    let (text, ok) = run(
        &[],
        &[
            "--image",
            &core_arg,
            "--eval",
            r#"(progn
                 (format t "GONE ~a~%" (find-class 'orphan nil))
                 (format t "SLOT ~a~%" (slot-value *i* 'a))
                 (format t "NAME ~a~%" (class-name (class-of *i*)))
                 (format t "RESTORED-OK~%"))"#,
        ],
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(ok, "{text}");
    expect(&text, &["GONE NIL", "SLOT 7", "NAME ORPHAN", "RESTORED-OK"]);
}
