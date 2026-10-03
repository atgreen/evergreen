// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn eval(source: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn repeated_short_searches_do_not_accumulate_large_closure_metadata() {
    let output = eval(r#"
      (defun report-rss ()
        (with-open-file (s "/proc/self/status")
          (loop for line = (read-line s nil nil) while line
                when (search "VmRSS:" line) do (write-line line))))
      (defun space-p (c) (char= c #\Space))
      (report-rss)
      (dotimes (i 10000)
        (assert (= 16 (position #\Space "0000000000000000 T symbol")))
        (assert (= 16 (position-if #'space-p "0000000000000000 T symbol"))))
      (report-rss)
    "#);
    let rss: Vec<u64> = output.lines()
        .filter_map(|line| line.strip_prefix("VmRSS:"))
        .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(rss.len(), 2, "{output}");
    // Allow generous allocator/JIT variation, while detecting the >100 MiB
    // of per-call helper metadata that made kernel-symbol parsing hit 4 GiB.
    assert!(rss[1].saturating_sub(rss[0]) < 64 * 1024, "{output}");
}

#[test]
fn position_preserves_search_direction_keys_and_test_not() {
    eval(r#"
      (let ((seen nil))
        (assert (= 1 (position 2 #(1 2 3 2 5) :start 1 :end 4
          :key (lambda (x) (push x seen) x))))
        (assert (equal seen '(2)))
        (setq seen nil)
        (assert (= 2 (position-if #'oddp #(1 2 3 2 5) :start 1 :end 4 :from-end t
          :key (lambda (x) (push x seen) x))))
        (assert (equal seen '(3 2))))
      (assert (= 2 (position 2 '(1 2 3 2) :start 1 :test-not #'=)))
      (assert (= 3 (position #\a "abca" :from-end t)))
      (assert (null (position-if (lambda (x) (error "empty range called predicate"))
                                #(1 2) :start 1 :end 1 :from-end t)))
    "#);
}
