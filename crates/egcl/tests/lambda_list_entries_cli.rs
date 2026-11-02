// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary T2 entries for lambda-list shapes, wide calls and multiple-value
//! returns (bliss-gg3i1.7.15.10.4). Every shape must print the same results
//! under the tree-walker, T0, T1, T2 and natural tiering, and under a moving
//! collector with poisoned nursery memory. The natural run also checks that
//! the callees really installed the ordinary x86-64 ABI.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

/// The callees, and small callers that each make one call so they are
/// themselves ordinary T2 code: a caller forwarding a callee's complete
/// result exercises the multiple-value descriptor, not just the buffer.
const PROGRAM: &str = r#"
(defun s-opt-dep (a &optional (b (+ a 1)) (c (* b 2) cp)) (list a b c cp))
(defun s-opt5 (a b c &optional (d (+ a b)) (e (list d c) ep)) (list a b c d e ep))
(defun s-rest (a &rest r) (list a r))
(defun s-rest4 (a b c d &rest r) (list a b c d r))
(defun s-key (a &key (b 1 bp) (c 2)) (list a b bp c))
(defun s-key-aok (a &key b &allow-other-keys) (list a b))
(defun s-rest-key (a &rest r &key (b 1) c) (list a r b c))
(defun s-key5 (&key a b c d e) (list a b c d e))
(defun s-fix5 (a b c d e) (list a b c d e))
(defun s-fix7 (a b c d e f g) (list a b c d e f g))
(defun s-mv (a) (values a (+ a 1) (list a)))
(defun s-mv0 (a) (declare (ignore a)) (values))
(defun s-mv1 (a) (values (+ a 1)))
(defun s-mv-branch (a) (if (evenp a) (values a (* a 2)) (values)))
(defun s-nlx (a &optional (b 2) &rest r) (throw 'out (list a b r)))
(defun s-err-key (a &key b) (list a b))

(defun c-mv (k) (s-mv k))
(defun c-mv0 (k) (s-mv0 k))
(defun c-mv1 (k) (s-mv1 k))
(defun c-mv-branch (k) (s-mv-branch k))
(defun c-mv-primary (k) (+ 1 (s-mv k)))
(defun c-mv-discard (k) (s-mv k) (s-mv1 k))
(defun c-opt-dep (k) (s-opt-dep k))
(defun c-opt-dep3 (k) (s-opt-dep k 10 20))
(defun c-opt5 (k) (s-opt5 k 1 2))
(defun c-opt5-full (k) (s-opt5 k 1 2 3 4))
(defun c-rest (k) (s-rest k 1 2 3 4 5))
(defun c-rest0 (k) (s-rest k))
(defun c-rest4 (k) (s-rest4 k 1 2 3 4 5 6))
(defun c-key (k) (s-key k :c 7 :b 5))
(defun c-key-dup (k) (s-key k :b 1 :b 2))
(defun c-key-aok (k) (s-key-aok k :zz 3 :b k))
(defun c-key-aok-call (k) (s-key k :c 3 :allow-other-keys t :zz 1))
(defun c-rest-key (k) (s-rest-key k :b 2 :c 3))
(defun c-key5 (k) (s-key5 :c k :d 2 :b 3 :a 4 :e 5))
(defun c-fix5 (k) (s-fix5 k 1 2 3 4))
(defun c-fix7 (k) (s-fix7 k 1 2 3 4 5 6))
(defun c-nlx (k) (catch 'out (s-nlx k 5 6 7)))
(defun c-funcall7 (k) (funcall 's-fix7 k 1 2 3 4 5 6))
(defun c-funcall-rest (k) (funcall #'s-rest k 1 2 3 4))
(defun c-funcall-key (k) (funcall #'s-key5 :e k :a 1 :c 3))
(defun s-wide (a b c d e f g h i j) (list j i h g f e d c b a))
(defun c-wide (k) (s-wide k 2 3 4 5 6 7 8 9 10))

(defun drive (k)
  (list
   (multiple-value-list (c-mv k)) (multiple-value-list (c-mv0 k))
   (multiple-value-list (c-mv1 k)) (multiple-value-list (c-mv-branch k))
   (multiple-value-list (c-mv-branch (1+ k))) (c-mv-primary k)
   (multiple-value-list (c-mv-discard k))
   (c-opt-dep k) (c-opt-dep3 k) (c-opt5 k) (c-opt5-full k)
   (c-rest k) (c-rest0 k) (c-rest4 k)
   (c-key k) (c-key-dup k) (c-key-aok k) (c-key-aok-call k) (c-rest-key k)
   (c-key5 k) (c-fix5 k) (c-fix7 k) (c-nlx k)
   (c-funcall7 k) (c-funcall-rest k) (c-funcall-key k) (c-wide k)
   (handler-case (s-err-key k :zz 1) (error () :unknown-key))
   (handler-case (funcall 's-err-key k :b) (error () :odd))
   (handler-case (funcall 's-fix5 k 1) (error () :few))
   (handler-case (funcall 's-opt-dep) (error () :none))
   (handler-case (funcall 's-opt-dep 1 2 3 4) (error () :many))))

(defvar *first* (drive 3))
(dotimes (i WARM) (drive i))
(sleep 0.5)
(dotimes (i WARM) (drive i))
(format t "RESULT ~s~%" (drive 3))
(format t "STABLE ~:[NO~;YES~]~%" (equal *first* (drive 3)))
"#;

fn run(warm: usize, envs: &[(&str, &str)]) -> (String, String) {
    let program = PROGRAM.replace("WARM", &warm.to_string());
    let dir = std::env::temp_dir().join(format!(
        "egcl-lambda-list-entries-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("shapes.lisp");
    std::fs::write(&file, program).unwrap();
    let mut command = Command::new("timeout");
    command
        .args([
            "--kill-after=5",
            "900",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--load",
        ])
        .arg(&file);
    for name in [
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_FORCE_TIER",
        "EGCL_LEAN_CALLS",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_T2_LOG",
        "EGCL_DISABLE_ORDINARY_VALUES",
    ] {
        command.env_remove(name);
    }
    command.envs(envs.iter().copied());
    let output = command.output().expect("run egcl");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "egcl {envs:?} failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("STABLE YES"), "{envs:?}\n{stdout}");
    let result = stdout
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("no result: {stdout}"))
        .to_string();
    (result, stderr)
}

fn reference() -> String {
    run(2, &[("EGCL_FORCE_TIER", "interp")]).0
}

#[test]
fn every_tier_agrees_on_each_lambda_list_and_values_shape() {
    let expected = reference();
    for tier in ["t0", "t1", "t2"] {
        assert_eq!(run(40, &[("EGCL_FORCE_TIER", tier)]).0, expected, "{tier}");
    }
    let (natural, log) = run(
        400,
        &[("EGCL_T2_THRESHOLD", "20"), ("EGCL_T2_LOG", "stderr")],
    );
    assert_eq!(natural, expected, "natural tiering");
    // The values producers, their forwarding callers and the binding
    // entries must really run through the ordinary entry, not a mapped one.
    for name in [
        "S-MV",
        "S-MV0",
        "S-MV1",
        "S-MV-BRANCH",
        "C-MV",
        "C-MV0",
        "C-MV-BRANCH",
        "S-OPT5",
        "S-REST",
        "S-REST4",
        "S-KEY",
        "S-KEY-AOK",
        "S-REST-KEY",
        "S-KEY5",
        "S-FIX7",
        "S-WIDE",
        "C-WIDE",
        "C-FUNCALL7",
    ] {
        assert!(
            log.lines().any(|line| line.starts_with(&format!(
                "[T2] {name}: optimized worker result installed with ordinary"
            ))),
            "{name} did not install an ordinary entry:\n{}",
            log.lines()
                .filter(|line| line.contains(&format!(" {name}:")))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    // The mapped comparison prints the same.
    let (mapped, _) = run(
        400,
        &[
            ("EGCL_T2_THRESHOLD", "20"),
            ("EGCL_DISABLE_ORDINARY_VALUES", "1"),
        ],
    );
    assert_eq!(mapped, expected, "mapped values");
}

#[test]
fn shapes_agree_under_a_moving_poisoned_collector() {
    let expected = reference();
    let (stressed, _) = run(
        30,
        &[
            ("EGCL_T2_THRESHOLD", "12"),
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    );
    assert_eq!(stressed, expected);
}
