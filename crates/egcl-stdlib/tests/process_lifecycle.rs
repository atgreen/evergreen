// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Real child lifecycles: interactive pipes, repeatable waits, and termination.
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};
use egcl_stdlib::process::{ProcessCommand, launch_program};

#[test]
#[ignore = "child fixture selected by lifecycle tests"]
fn interactive_child() {
    println!("READY");
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        println!("reply:{line}");
        eprintln!("err:{line}");
        std::io::stdout().flush().unwrap();
    }
    println!("EOF");
    std::process::exit(7);
}

fn command() -> ProcessCommand {
    fixture("interactive_child")
}

fn fixture(name: &str) -> ProcessCommand {
    ProcessCommand::Argv(vec![
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        "--exact".into(),
        name.into(),
        "--ignored".into(),
        "--nocapture".into(),
    ])
}

#[test]
#[ignore = "child fixture selected by lifecycle tests"]
fn large_output_child() {
    for _ in 0..4 {
        std::io::stdout().write_all(&[b'o'; 65536]).unwrap();
        std::io::stderr().write_all(&[b'e'; 65536]).unwrap();
    }
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(0);
}

#[test]
fn both_large_output_pipes_can_drain_while_waiting() {
    let mut process = launch_program(fixture("large_output_child")).unwrap();
    let mut output = process.stdout.take().unwrap();
    let mut error = process.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        error.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert!(
        process
            .wait(Some(Duration::from_secs(10)))
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(out.join().unwrap().ends_with(&vec![b'o'; 4 * 65536]));
    assert_eq!(err.join().unwrap(), vec![b'e'; 4 * 65536]);
}

fn ready(reader: &mut impl BufRead) {
    let mut line = String::new();
    loop {
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "no child prompt");
        if line.trim() == "READY" {
            return;
        }
        line.clear();
    }
}

#[test]
fn live_child_exchanges_data_and_retains_exit_status() {
    let mut process = launch_program(command()).unwrap();
    let mut input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap());
    let mut error = process.stderr.take().unwrap();
    ready(&mut output);
    assert!(process.try_wait().unwrap().is_none());
    assert!(
        process
            .wait(Some(Duration::from_millis(20)))
            .unwrap()
            .is_none()
    );
    for text in ["first", "spaces \"quotes\" café"] {
        writeln!(input, "{text}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("reply:{text}"));
    }
    drop(input);
    let status = process.wait(Some(Duration::from_secs(5))).unwrap().unwrap();
    assert_eq!(status.code(), Some(7));
    assert_eq!(process.wait(None).unwrap(), Some(status));
    assert_eq!(process.try_wait().unwrap(), Some(status));
    process.terminate().unwrap(); // Completed children remain completed.
    assert_eq!(process.wait(None).unwrap(), Some(status));
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).unwrap();
    assert_eq!(remaining.trim(), "EOF");
    let mut errors = String::new();
    error.read_to_string(&mut errors).unwrap();
    assert_eq!(
        errors.replace('\r', ""),
        "err:first\nerr:spaces \"quotes\" café\n"
    );
}

#[test]
fn termination_unblocks_wait_and_is_repeatable() {
    let mut process = launch_program(command()).unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap());
    ready(&mut output);
    assert!(process.wait(Some(Duration::ZERO)).unwrap().is_none());
    process.terminate().unwrap();
    let status = process.wait(Some(Duration::from_secs(5))).unwrap().unwrap();
    assert!(!status.success());
    process.terminate().unwrap();
    assert_eq!(process.wait(None).unwrap(), Some(status));
}

#[test]
fn dropping_owner_does_not_wait_or_kill_and_child_is_reaped() {
    let mut process = launch_program(command()).unwrap();
    let input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap());
    ready(&mut output);
    let pid = process.id();
    let start = Instant::now();
    drop(process);
    assert!(start.elapsed() < Duration::from_secs(1));
    // Keeping stdin alive keeps the child running after dropping its owner.
    drop(input);
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).unwrap();
    assert_eq!(remaining.trim(), "EOF", "dropping owner killed the child");
    #[cfg(target_os = "linux")]
    {
        // The reaper, not this test, must consume the zombie. Procfs retains
        // an exited-but-unreaped child until its parent waits for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            assert!(Instant::now() < deadline, "abandoned child was not reaped");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[test]
fn launch_rejects_empty_and_missing_commands() {
    assert!(launch_program(ProcessCommand::Argv(vec![])).is_err());
    assert!(
        launch_program(ProcessCommand::Argv(vec![
            "egcl-missing-lifecycle-child-9f761e".into()
        ]))
        .is_err()
    );
}
