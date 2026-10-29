// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(unix, not(target_os = "ios")))]

fn child(case: &str) -> bool {
    const MARKER: &str = "EGCL_EMBEDDED_EXIT_CASE";
    if std::env::var(MARKER).as_deref() == Ok(case) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env(MARKER, case)
        .env("EGCL_FORCE_TIER", "t0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
fn args(program: String) -> Vec<String> {
    vec![
        "egcl".into(),
        "--no-init".into(),
        "--no-image".into(),
        "--eval".into(),
        program,
    ]
}
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn embedded_quit_preserves_cleanup_and_the_host_process() {
    const NAME: &str = "embedded_quit_preserves_cleanup_and_the_host_process";
    if !child(NAME) {
        return;
    }
    let path = std::env::temp_dir().join(format!("egcl-embedded-cleanup-{}", std::process::id()));
    let name = quote(path.to_str().unwrap());
    let code = egcl::run_embedded(&args(format!("(unwind-protect (egcl-ext:quit 7) (with-open-file (out {name} :direction :output :if-exists :supersede :if-does-not-exist :create) (write-string \"cleanup completed\" out)))"))).unwrap();
    assert_eq!(code, 7);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "cleanup completed");
    std::fs::remove_file(path).unwrap();
    assert!(matches!(
        egcl::run_embedded(&args("(+ 1 2)".into())),
        Err(egcl_rt::EgclError::ProgramError(_))
    ));
}

#[test]
fn real_cleanup_errors_supersede_the_exit_request() {
    const NAME: &str = "real_cleanup_errors_supersede_the_exit_request";
    if !child(NAME) {
        return;
    }
    let result = egcl::run_embedded(&args(
        "(unwind-protect (egcl-ext:quit 7) (error \"cleanup failure\"))".into(),
    ));
    assert!(result.is_err());
}

#[test]
fn application_sigterm_and_alarm_handlers_are_preserved() {
    const NAME: &str = "application_sigterm_and_alarm_handlers_are_preserved";
    if !child(NAME) {
        return;
    }
    unsafe extern "C" fn application_handler(_signal: libc::c_int) {}
    // SAFETY: a valid static host handler; inspect both dispositions after Lisp.
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGALRM] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = application_handler as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
        }
    }
    assert_eq!(egcl::run_embedded(&args("(+ 20 22)".into())).unwrap(), 0);
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGALRM] {
            let mut actual: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut actual), 0);
            assert_eq!(
                actual.sa_sigaction,
                application_handler as *const () as usize
            );
        }
    }
}

#[test]
fn ordinary_cli_quit_still_owns_its_process() {
    const NAME: &str = "ordinary_cli_quit_still_owns_its_process";
    const MARKER: &str = "EGCL_EMBEDDED_EXIT_CASE";
    if std::env::var(MARKER).as_deref() == Ok(NAME) {
        let _ = egcl::run(&args("(egcl-ext:quit 9)".into()));
        panic!("ordinary CLI QUIT returned to its caller");
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(MARKER, NAME)
        .env("EGCL_FORCE_TIER", "t0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(9));
}
