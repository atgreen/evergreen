//! Synchronous process capture preserves separate output streams and status.
use egcl_stdlib::process::{ProcessCommand, run_program};

#[test]
fn shell_captures_both_outputs_and_nonzero_status() {
    #[cfg(windows)]
    let text = "echo process-out & echo process-err 1>&2 & exit /b 7";
    #[cfg(unix)]
    let text = "printf process-out; printf process-err >&2; exit 7";
    let output = run_program(ProcessCommand::Shell(text.into())).unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("process-out")
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("process-err")
    );
}

#[test]
fn empty_or_missing_executable_is_an_error() {
    assert!(run_program(ProcessCommand::Argv(vec![])).is_err());
    assert!(
        run_program(ProcessCommand::Argv(vec![
            "egcl-nonexistent-executable-9f761e".into()
        ]))
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn direct_arguments_are_not_interpreted_by_a_shell() {
    let argument = "spaces 'quotes' \"double\" \\slash $HOME &| café";
    let output = run_program(ProcessCommand::Argv(vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$1\"".into(),
        "child".into(),
        argument.into(),
    ]))
    .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), argument);
    assert!(output.stderr.is_empty());
}
