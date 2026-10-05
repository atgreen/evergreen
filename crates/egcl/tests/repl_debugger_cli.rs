// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run_in_pty(input: &[u8]) -> String {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());

    let master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let mut writer = master.try_clone().unwrap();
    let reader = std::thread::spawn(move || {
        let mut master = master;
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
        output
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--no-init")
        .env(
            "EGCL_INIT_FILE",
            std::env::temp_dir().join("egcl-repl-debugger-no-init.lisp"),
        )
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    writer.write_all(input).unwrap();
    writer.flush().unwrap();

    let timeout = if std::env::var("EGCL_GC_STRESS").as_deref() == Ok("1") {
        // Full-stress startup alone can take roughly twelve minutes on the
        // project development host because every allocation collects.
        Duration::from_secs(900)
    } else {
        Duration::from_secs(20)
    };
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(writer);
    let output = String::from_utf8_lossy(&reader.join().unwrap()).into_owned();
    assert!(
        !timed_out,
        "PTY debugger child timed out after {timeout:?}:\n{output}"
    );
    assert!(status.success(), "{output}");
    output
}

#[test]
fn repl_debugger_uses_the_errors_pre_unwind_lisp_backtrace() {
    let output = run_in_pty(
        br#"
(defun repl-bt-leaf (value) (error "PTY debugger failure"))
(defun repl-bt-outer (value) (repl-bt-leaf value))
(repl-bt-outer (list "snapshot-value" 42))
frame 0
eval (+ 1 1)
bt 20
:abort
(format t "PTY-RECOVERED~%")
(quit)
"#,
    );
    let debugger = output
        .split_once("Debugger entered:")
        .map(|(_, debugger)| debugger)
        .unwrap_or_else(|| panic!("debugger did not start:\n{output}"));
    let sections: Vec<_> = debugger.split("DEBUG[1]> ").collect();
    assert!(
        sections.len() >= 5,
        "missing debugger command responses:\n{debugger}"
    );
    let initial = sections[0];
    let frame = sections[1];
    let evaluation = sections[2];
    let backtrace = sections[3];

    let leaf = initial
        .find("REPL-BT-LEAF")
        .unwrap_or_else(|| panic!("initial backtrace missing Lisp leaf:\n{initial}"));
    let outer = initial
        .find("REPL-BT-OUTER")
        .unwrap_or_else(|| panic!("initial backtrace missing Lisp caller:\n{initial}"));
    assert!(
        leaf < outer,
        "initial backtrace must print the innermost frame first:\n{initial}"
    );
    assert!(
        initial.contains("\"snapshot-value\" 42") && initial.contains("historical snapshot"),
        "initial backtrace must retain arguments and identify the snapshot:\n{initial}"
    );

    let leaf = backtrace
        .find("REPL-BT-LEAF")
        .unwrap_or_else(|| panic!("explicit backtrace missing Lisp leaf:\n{backtrace}"));
    let outer = backtrace
        .find("REPL-BT-OUTER")
        .unwrap_or_else(|| panic!("explicit backtrace missing Lisp caller:\n{backtrace}"));
    assert!(
        leaf < outer,
        "explicit backtrace must print the innermost frame first:\n{backtrace}"
    );
    assert!(
        backtrace.contains("\"snapshot-value\" 42") && backtrace.contains("historical snapshot"),
        "explicit backtrace must retain arguments and identify the snapshot:\n{backtrace}"
    );
    assert!(
        frame.contains("historical snapshot"),
        "frame selection must identify an unwound frame:\n{frame}"
    );
    assert!(
        evaluation.contains("Frame evaluation unavailable"),
        "the debugger must not claim historical frames are live:\n{evaluation}"
    );
    assert!(
        output.contains("PTY-RECOVERED"),
        "abort must resume the REPL:\n{output}"
    );
}
