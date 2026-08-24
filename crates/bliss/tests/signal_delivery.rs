//! Behavioral signal delivery tests for the real `bliss-cli` binary.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

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
        .expect("spawn bliss-cli");

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
        .env("BLISS_GC_STRESS", "100")
        .args(["--no-init", "--eval", program])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bliss-cli");

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
        .expect("spawn bliss-cli");

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
        .expect("spawn bliss-cli");

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
        .expect("spawn bliss-cli");

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
