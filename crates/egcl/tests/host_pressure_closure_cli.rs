// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Closures the loop creates: enough that dead ones left unreclaimed until the
/// heap is exhausted would push the process far past `PEAK_RSS_LIMIT`.
const CLOSURES: u64 = 2_000_000;
/// Every 100,000th closure escapes into a list and must stay callable.
const KEEP_EVERY: u64 = 100_000;
/// Measured peak RSS is about 60 MB (captured frames) and 75-90 MB (heap
/// environments) when dead closures are reclaimed; leaving them all to the
/// final collection measured over 300 MB at this count.
const PEAK_RSS_LIMIT: usize = 256 * 1024 * 1024;

fn closure_pressure_keeps_escaped_values(binding: &str) {
    let helper = if binding == "labels" {
        "(if (zerop n) x (f (1- n)))"
    } else {
        "(progn n x)"
    };
    // Each closure answers its own X: the loop sums 0..CLOSURES, and the
    // escaped ones sum the multiples of KEEP_EVERY below CLOSURES.
    let sum = CLOSURES * (CLOSURES - 1) / 2;
    let kept = CLOSURES / KEEP_EVERY;
    let kept_sum = KEEP_EVERY * kept * (kept - 1) / 2;
    let program = format!(
        r#"
(format *error-output* "HOST-PRESSURE-WORKLOAD-BEGIN~%")
(room nil)
(let ((sum 0) (kept '()))
  (dotimes (i {CLOSURES})
    (let ((x i))
      ({binding} ((f (n) {helper}))
        (incf sum (f 1))
        (when (zerop (mod i {KEEP_EVERY})) (push #'f kept)))))
  (assert (= sum {sum}))
  (assert (= (length kept) {kept}))
  (assert (= (reduce #'+ (mapcar (lambda (f) (funcall f 1)) kept)) {kept_sum}))
  (format t "REGISTRY ~D~%" (first (egcl::%bc-registry-sizes)))
  (format t "HOST-PRESSURE-LIVENESS-OK~%"))
(room nil)
(format *error-output* "HOST-PRESSURE-WORKLOAD-END~%")
"#
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .env("EGCL_GC_CENSUS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while the child runs: census diagnostics must not fill
    // stderr and turn this pressure test into an artificial blocked process.
    let mut child_stdout = child.stdout.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut text = String::new();
        child_stdout.read_to_string(&mut text).unwrap();
        text
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        child_stderr.read_to_string(&mut text).unwrap();
        text
    });
    let started = Instant::now();
    let mut peak_rss = 0;
    let mut exceeded_limit = false;
    let mut timed_out = false;
    let status = loop {
        if started.elapsed() > Duration::from_secs(180) {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", child.id()))
            && let Some(kib) = status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()
            })
        {
            peak_rss = peak_rss.max(kib * 1024);
            // Dead closures must be reclaimed while the loop runs, not left
            // for the heap to fill: bound the whole process.
            if peak_rss > PEAK_RSS_LIMIT {
                exceeded_limit = true;
                child.kill().unwrap();
                break child.wait().unwrap();
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_reader.join().unwrap();
    let stderr = stderr_reader.join().unwrap();
    assert!(
        status.success() && !exceeded_limit && !timed_out,
        "{binding}: peak RSS {peak_rss} bytes; status {status}\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("HOST-PRESSURE-LIVENESS-OK"), "{stdout}");
    // Legacy closures with host state and closures with control scopes remain
    // pinned and request majors. Control-free heap-environment instances are
    // nursery objects: minor collections reclaim them within the same RSS cap.
    // The unit regression additionally proves that the callable itself moves.
    let requests_during_workload = stderr
        .split_once("HOST-PRESSURE-WORKLOAD-BEGIN")
        .and_then(|(_, tail)| tail.split_once("HOST-PRESSURE-WORKLOAD-END"))
        .map(|(workload, _)| {
            workload.matches("request-major reason=host-bytes").count()
                + workload
                    .matches("request-major reason=pinned-closures")
                    .count()
        })
        .unwrap_or(0);
    let minor_counts: Vec<u64> = stdout
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("GCs:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect();
    assert_eq!(
        minor_counts.len(),
        2,
        "missing before/after ROOM counts\n{stdout}"
    );
    let minor_collections = minor_counts[1].saturating_sub(minor_counts[0]);
    assert!(
        requests_during_workload > 0 || minor_collections > 0,
        "closure pressure neither requested a major nor collected the nursery during the workload\n{stdout}\n{stderr}"
    );
    assert!(
        !stderr.contains("request-major reason=legacy-count"),
        "CLI unexpectedly used the untracked-library count fallback\n{stderr}"
    );
    #[cfg(target_os = "linux")]
    assert!(peak_rss > 0, "Linux RSS monitor did not observe the child");
    let registry: usize = stdout
        .lines()
        .find_map(|line| line.strip_prefix("REGISTRY "))
        .unwrap()
        .parse()
        .unwrap();
    eprintln!(
        "{binding}: major requests {requests_during_workload}, minor collections {minor_collections}, registry {registry}, peak RSS {peak_rss}"
    );
    assert!(
        registry < 300000,
        "{binding}: retained {registry} entries after {CLOSURES} closures; peak RSS {peak_rss}\n{stdout}"
    );
}

#[test]
fn host_pressure_reclaims_flet_closures_and_preserves_escaped_values() {
    closure_pressure_keeps_escaped_values("flet");
}

#[test]
fn host_pressure_reclaims_labels_closures_and_preserves_escaped_values() {
    closure_pressure_keeps_escaped_values("labels");
}
