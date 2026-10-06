// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[cfg(target_arch = "x86_64")]
fn assert_fence_listings(stdout: &str, prefix: &str) {
    for (name, expected) in [
        ("FULL-FENCE", "mfence"),
        ("READ-FENCE", "lfence"),
        ("WRITE-FENCE", "sfence"),
        ("DATA-FENCE", "lfence"),
    ] {
        let marker = format!("BEGIN-FENCE {prefix}{name}\n");
        let listing = stdout
            .split_once(&marker)
            .and_then(|(_, rest)| rest.split_once("END-FENCE"))
            .map(|(listing, _)| listing)
            .unwrap_or_else(|| panic!("missing listing for {marker}: {stdout}"));
        for mnemonic in ["mfence", "lfence", "sfence"] {
            assert_eq!(
                listing.matches(mnemonic).count(),
                usize::from(mnemonic == expected),
                "wrong {mnemonic} count for {prefix}{name}: {listing}"
            );
        }
        assert!(!listing.contains("call (slice ABI)"), "{listing}");
    }
}

#[test]
fn memory_barriers_are_callable_in_compiled_functions() {
    let source = r#"
      (assert (eq :external (nth-value 1 (find-symbol "MEMORY-BARRIER" :egcl-ext))))
      (dolist (kind '(:read :write :full :data-dependency))
        (assert (null (egcl-ext:memory-barrier kind))))
      (assert (null (egcl-ext:memory-barrier)))
      (assert (null (egcl-ext:load-barrier)))
      (assert (null (egcl-ext:store-barrier)))
      (assert (handler-case
                  (progn (egcl-ext:memory-barrier :unknown) nil)
                (program-error () t)))
      (defun publish-value (buffer value)
        (setf (aref buffer 0) value)
        (egcl-ext:store-barrier)
        (setf (aref buffer 1) value)
        (egcl-ext:load-barrier)
        (aref buffer 0))
      (compile 'publish-value)
      (let ((buffer (vector 0 0)))
        (dotimes (i 1000)
          (assert (= i (publish-value buffer i)))
          (assert (= i (aref buffer 1)))))
      (format t "MEMORY-BARRIER-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("MEMORY-BARRIER-OK"));
}

#[cfg(target_arch = "x86_64")]
#[test]
fn constant_barriers_lower_to_t1_fence_instructions() {
    let source = r#"
      (defun full-fence () (egcl-ext:memory-barrier :full))
      (defun read-fence () (egcl-ext:load-barrier))
      (defun write-fence () (egcl-ext:store-barrier))
      (defun data-fence () (egcl-ext:memory-barrier :data-dependency))
      (dolist (function '(full-fence read-fence write-fence data-fence))
        (dotimes (i 20) (funcall function))
        (assert (= 1 (egcl-ext:function-tier function)))
        (format t "BEGIN-FENCE ~A~%" function)
        (disassemble function)
        (format t "END-FENCE~%"))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_DISABLE_T2", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_fence_listings(&stdout, "");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn native_barrier_discards_stale_secondary_values() {
    let source = r#"
      (defun fence-values ()
        (values 1 2)
        (egcl-ext:memory-barrier))
      (dotimes (i 20) (fence-values))
      (assert (= 1 (egcl-ext:function-tier 'fence-values)))
      (assert (equal '(nil) (multiple-value-list (fence-values))))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_DISABLE_T2", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn constant_barrier_lowers_to_t2_fence_instruction() {
    let source = r#"
      (defun t2-full-fence ()
        (values 1 2)
        (egcl-ext:memory-barrier :full))
      (defun t2-read-fence () (values 1 2) (egcl-ext:load-barrier))
      (defun t2-write-fence () (values 1 2) (egcl-ext:store-barrier))
      (defun t2-data-fence () (values 1 2) (egcl-ext:memory-barrier :data-dependency))
      (dolist (name '(t2-full-fence t2-read-fence t2-write-fence t2-data-fence))
        (dotimes (i 20) (funcall name))
        (assert (= 2 (egcl-ext:function-tier name)))
        (assert (equal '(nil) (multiple-value-list (funcall name))))
        (format t "BEGIN-FENCE ~A~%" name)
        (disassemble name)
        (format t "END-FENCE~%"))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env_remove("EGCL_T2_NO_QUEUE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_fence_listings(&stdout, "T2-");
}
