// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Short-lived FLET closures must be reclaimed without an explicit full GC
//! (bliss-5ndk). Each evaluation of a capturing FLET mints a private closure
//! identity whose side-table entries only a major collection can release.
//! The closure function objects are pinned and the loop allocates nothing in
//! the nursery, so before the fix no collection of any kind ran and the
//! bytecode registry grew by one entry per evaluation, without bound.

use std::process::Command;

#[test]
fn short_lived_flet_closures_are_reclaimed_without_explicit_gc() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
(let ((sum 0) (kept '()))
  (dotimes (i 60000)
    (let ((x i))
      (flet ((f () x))
        (incf sum (f))
        ;; A few closures escape and must survive every reclamation.
        (when (zerop (mod i 1000))
          (push #'f kept)))))
  (format t "SUM ~D~%" sum)
  (format t "KEPT ~D ~D~%" (length kept)
          (reduce #'+ (mapcar #'funcall kept)))
  (format t "REGISTRY ~D~%" (first (egcl::%bc-registry-sizes))))
(let ((x 7))
  (flet ((g () x))
    (format t "EQ ~A~%" (eq #'g #'g))))
"#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("SUM 1799970000"), "stdout:\n{stdout}\nstderr:\n{stderr}");
    // 60 escaped closures for i = 0, 1000, ..., 59000.
    assert!(stdout.contains("KEPT 60 1770000"), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("EQ T"), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let registry: usize = stdout
        .lines()
        .find_map(|line| line.strip_prefix("REGISTRY "))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or_else(|| panic!("no REGISTRY line\nstdout:\n{stdout}\nstderr:\n{stderr}"));
    // Without reclamation this is about 60,000 plus the image's own entries.
    // A major collection is requested every 16,384 private closures, so the
    // live count stays well below that.
    assert!(
        registry < 40_000,
        "bytecode registry kept {registry} entries for 60,000 short-lived closures\nstdout:\n{stdout}"
    );
}
