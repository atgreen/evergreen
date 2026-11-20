// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A closure whose DOLIST leaves an outer block by RETURN-FROM (bliss-gg3i1.7.24).
//! The shape is SBCL's META-VMDEF coalescing loop: a MAPL step makes a MAPHASH
//! callback that searches a list and exits the step's own block on a match.
//! Nothing can GO to the loop's tags, so they need no registration; before,
//! the named exit kept them registered, the registration's NamedTag kept the
//! callback off native code, and every call from native MAPHASH crossed into
//! the interpreter.
//!
//! The answers must not change at any tier, and with `EGCL_DECLINE_CENSUS`
//! the callback no longer forces crossings: 3,000 coalescing runs crossed
//! about 527,000 times before and about 1,200 times after, almost all of
//! them start-up. The bound below sits between the two.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defvar *table* (let ((h (make-hash-table)))
                    (dotimes (i 50 h)
                      (setf (gethash i h) (list (vector i) (vector (1+ i)))))))
  (defun coalesce (my-val accessor)
    (mapl (lambda (cell)
            (let ((my-vector (car cell)))
              (block found
                (maphash (lambda (name other)
                           (declare (ignore name))
                           (dolist (other-vector (funcall accessor other))
                             (when (equalp other-vector my-vector)
                               (rplaca cell other-vector)
                               (return-from found))))
                         *table*))))
          (copy-list my-val)))
  (defun probe-tags (n)
    (let ((hits 0))
      (dotimes (i n hits)
        (tagbody
         top
           (mapc (lambda (x) (when (eql x i) (incf hits) (go done))) '(1 3 5))
           (go done)
         done))))
  (let (last)
    (dotimes (i RUNS)
      (setq last (coalesce (list (vector 3) (vector 99) (vector 49)) #'identity)))
    (let ((*print-pretty* nil))
      (format t "RESULT ~S ~S ~S~%" last
              (eq (first last) (first (gethash 3 *table*)))
              (probe-tags 10)))))
"#;

const EXPECTED: &str = "RESULT (#(3) #(99) #(49)) NIL 3";

fn run(runs: usize, envs: &[(&str, &str)]) -> String {
    let program = PROGRAM.replace("RUNS", &runs.to_string());
    let mut cmd = Command::new("timeout");
    cmd.args([
        "--kill-after=5",
        "300",
        BIN,
        "--no-init",
        "--eval",
        &program,
    ]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn result_line(out: &str) -> &str {
    out.lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("no RESULT line in:\n{out}"))
}

#[test]
fn loop_named_exits_answer_alike_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = run(300, &[("EGCL_FORCE_TIER", tier)]);
        assert_eq!(result_line(&out), EXPECTED, "tier {tier}");
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn loop_named_exit_callback_stays_native() {
    let dir = std::env::temp_dir().join(format!("egcl-loop-exit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let report = dir.join("decline");
    let out = run(3000, &[("EGCL_DECLINE_CENSUS", report.to_str().unwrap())]);
    assert_eq!(result_line(&out), EXPECTED);
    let text = std::fs::read_to_string(&report).expect("census report");
    let crossings: u64 = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("no crossing total in:\n{text}"));
    assert!(
        crossings < 50_000,
        "{crossings} native/T0 crossings; the callback fell back to T0:\n{text}"
    );
    assert!(
        !text.contains("NamedTag"),
        "a NamedTag decline remains:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
