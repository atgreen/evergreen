//! Behavioral signal delivery tests for the real `egcl` binary.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn wait_for_child(mut child: std::process::Child, timeout: Duration) -> std::process::Output {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait().expect("poll child") {
            return child.wait_with_output().expect("collect child output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child
                .wait_with_output()
                .expect("collect killed child output");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sigint_is_delivered_as_catchable_interrupt_condition() {
    let program = "\
        (handler-case \
            (progn \
              (format t \"READY~%\") \
              (force-output) \
              (loop)) \
          (interrupt-condition () \
            (format t \"CAUGHT~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY");

    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(kill.success(), "kill -INT should succeed");

    let output = wait_for_child(child, Duration::from_secs(2));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "SIGINT handler-case should exit normally; output:\n{text}"
    );
    assert!(
        text.contains("CAUGHT"),
        "SIGINT must become INTERRUPT-CONDITION; output:\n{text}"
    );
}

#[test]
fn sigint_during_gc_stress_allocation_is_deferred_and_catchable() {
    let program = "\
        (handler-case \
            (progn \
              (format t \"READY~%\") \
              (force-output) \
              (loop (list 1 2 3 4 5 6 7 8 9 10))) \
          (interrupt-condition () \
            (format t \"CAUGHT~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .env("EGCL_GC_STRESS", "100")
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY");

    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(kill.success(), "kill -INT should succeed");

    let output = wait_for_child(child, Duration::from_secs(3));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "SIGINT during allocation/GC stress should exit through handler-case; output:\n{text}"
    );
    assert!(
        text.contains("CAUGHT"),
        "SIGINT during allocation/GC stress must become INTERRUPT-CONDITION; output:\n{text}"
    );
}

#[test]
fn sigterm_requests_shutdown_not_interrupt_condition() {
    let program = "\
        (handler-case \
            (progn \
              (format t \"READY~%\") \
              (force-output) \
              (loop)) \
          (interrupt-condition () \
            (format t \"INTERRUPT~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY");

    let kill = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM should succeed");

    let output = wait_for_child(child, Duration::from_secs(2));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        !text.contains("INTERRUPT"),
        "SIGTERM must request shutdown, not signal INTERRUPT-CONDITION; output:\n{text}"
    );
    assert!(
        output.status.code().is_some(),
        "SIGTERM should be handled as orderly process shutdown, not signal death; output:\n{text}"
    );
}

#[test]
fn sigterm_stops_a_hot_native_loop() {
    // bliss-7rdu: a hot NATIVE loop (T1/OSR) must still honor SIGTERM. The T0
    // interpreter polls every instruction, but compiled loops only re-enter Rust
    // at sampled back-edges — and OSR code emitted no back-edge poll at all, so a
    // call-free native loop ignored SIGTERM (and GC stop-the-world) forever. The
    // warmup promotes `spin` to native before the long loop runs, so this
    // exercises the native back-edge signal poll, not the interpreter. Before the
    // fix the child spins past SIGTERM and is SIGKILLed at the deadline, leaving
    // `status.code()` == None; the fix makes it shut down with an exit code.
    //
    // T2 is disabled here: the back-edge signal poll lives in the T0/T1/OSR
    // emitter, so the framed T2 emitter still lacks it (a T2 loop that finishes
    // background compilation stays uninterruptible — tracked as a separate
    // follow-up). Disabling T2 makes this test deterministic and scoped to the
    // tier the fix actually covers.
    let program = "\
        (handler-case \
            (progn \
              (defun spin (n) \
                (let ((s 0)) (dotimes (i n s) (setq s (the fixnum (+ s i)))))) \
              (dotimes (w 20) (spin 100000)) \
              (format t \"READY~%\") \
              (force-output) \
              (spin 1000000000000) \
              (format t \"FELL-THROUGH~%\") \
              (force-output)) \
          (interrupt-condition () \
            (format t \"INTERRUPT~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_DISABLE_T2", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY", "child should reach the hot loop");

    let kill = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM should succeed");

    // Generous window: the loop must be terminated by the SIGTERM shutdown, not
    // by wait_for_child's SIGKILL at the deadline.
    let output = wait_for_child(child, Duration::from_secs(8));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        !text.contains("FELL-THROUGH"),
        "the loop cannot complete 1e12 iterations; it must be interrupted; output:\n{text}"
    );
    assert!(
        output.status.code().is_some(),
        "a hot native loop must shut down on SIGTERM (exit code), not be SIGKILLed \
         at the deadline (bliss-7rdu); output:\n{text}"
    );
}

/// bliss-siv7: with T2 ENABLED, a hot loop that finishes background T2
/// compilation runs native code with no back-edge signal poll at all, so the
/// cooperative SIGTERM flag is never seen. The interim fix arms a hard
/// deadline in the SIGTERM handler (alarm + SIGALRM exit_group(143)), so the
/// process terminates within the grace period instead of spinning until an
/// external SIGKILL. (The full fix — a real T2 back-edge safepoint poll with
/// a stack map — is bliss-eeyj.)
#[test]
fn sigterm_terminates_a_t2_loop_within_the_grace_period() {
    let program = "\
        (progn \
          (defun spin (n) \
            (let ((s 0)) (dotimes (i n s) (setq s (the fixnum (+ s i)))))) \
          (dotimes (w 60) (spin 200000)) \
          (format t \"READY~%\") \
          (force-output) \
          (spin 1000000000000) \
          (format t \"FELL-THROUGH~%\") \
          (force-output))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY", "child should reach the hot loop");
    // Give background T2 compilation a moment to install the native loop.
    std::thread::sleep(Duration::from_secs(2));

    let kill = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM should succeed");

    // The SIGTERM grace deadline is 5s; the child must be gone well before
    // wait_for_child's 15s SIGKILL backstop, with a real exit status (143 from
    // the deadline, or an orderly code if a poll caught the flag first).
    let output = wait_for_child(child, Duration::from_secs(15));
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.code().is_some(),
        "a T2 loop must terminate on SIGTERM within the grace period (exit code), \
         not be SIGKILLed at the deadline (bliss-siv7); stderr:\n{text}"
    );
}

#[test]
fn sigfpe_is_delivered_as_catchable_arithmetic_error() {
    let program = "\
        (handler-case \
            (progn \
              (format t \"READY~%\") \
              (force-output) \
              (loop)) \
          (arithmetic-error () \
            (format t \"ARITHMETIC~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY");

    let kill = Command::new("kill")
        .args(["-FPE", &child.id().to_string()])
        .status()
        .expect("send SIGFPE");
    assert!(kill.success(), "kill -FPE should succeed");

    let output = wait_for_child(child, Duration::from_secs(2));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "SIGFPE handler-case should exit normally; output:\n{text}"
    );
    assert!(
        text.contains("ARITHMETIC"),
        "SIGFPE must become ARITHMETIC-ERROR; output:\n{text}"
    );
}

#[test]
fn sigpipe_is_delivered_as_stream_error_on_next_output() {
    let program = "\
        (handler-case \
            (progn \
              (format t \"READY~%\") \
              (force-output) \
              (loop (force-output))) \
          (stream-error () \
            (format t \"STREAM~%\") \
            (force-output)))";

    let mut child = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("child should print readiness")
        .expect("read readiness");
    assert_eq!(ready, "READY");

    let kill = Command::new("kill")
        .args(["-PIPE", &child.id().to_string()])
        .status()
        .expect("send SIGPIPE");
    assert!(kill.success(), "kill -PIPE should succeed");

    let output = wait_for_child(child, Duration::from_secs(2));
    let mut text = String::new();
    text.push_str(&ready);
    text.push('\n');
    for line in lines.map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "SIGPIPE handler-case should exit normally; output:\n{text}"
    );
    assert!(
        text.contains("STREAM"),
        "SIGPIPE must become STREAM-ERROR on the next output operation; output:\n{text}"
    );
}
