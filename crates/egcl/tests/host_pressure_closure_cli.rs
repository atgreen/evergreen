// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn closure_pressure_keeps_escaped_values(binding: &str) {
    let helper = if binding == "labels" {
        "(if (zerop n) x (f (1- n)))"
    } else {
        "(progn n x)"
    };
    let program = format!(
        r#"
(format *error-output* "HOST-PRESSURE-WORKLOAD-BEGIN~%")
(let ((sum 0) (kept '()))
  (dotimes (i 600000)
    (let ((x i))
      ({binding} ((f (n) {helper}))
        (incf sum (f 1))
        (when (zerop (mod i 100000)) (push #'f kept)))))
  (assert (= sum 179999700000))
  (assert (= (length kept) 6))
  (assert (= (reduce #'+ (mapcar (lambda (f) (funcall f 1)) kept)) 1500000))
  (format t "REGISTRY ~D~%" (first (egcl::%bc-registry-sizes)))
  (format t "HOST-PRESSURE-LIVENESS-OK~%"))
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
            // Bound the complete process as well as the retained registry;
            // this is a regression safety bound, not the policy's heap limit.
            if peak_rss > 768 * 1024 * 1024 {
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
    let requests_during_workload = stderr
        .split_once("HOST-PRESSURE-WORKLOAD-BEGIN")
        .and_then(|(_, tail)| tail.split_once("HOST-PRESSURE-WORKLOAD-END"))
        .map(|(workload, _)| workload.matches("request-major reason=host-bytes").count())
        .unwrap_or(0);
    assert!(
        requests_during_workload > 0,
        "tracked host-byte pressure never requested a major collection during the workload\n{stderr}"
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
        "{binding}: host-byte requests {requests_during_workload}, registry {registry}, peak RSS {peak_rss}"
    );
    assert!(
        registry < 300000,
        "{binding}: retained {registry} entries after 600000 closures; peak RSS {peak_rss}\n{stdout}"
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
