//! Synchronous subprocess execution for TORCL-EXT:RUN-PROGRAM.
use std::process::{Command, Output};
use torcl_rt::error::TorclError;

pub enum ProcessCommand {
    Shell(String),
    Argv(Vec<String>),
}

/// Capture both output pipes concurrently through std::process, avoiding the
/// deadlock caused by draining stdout before a child fills its stderr pipe.
pub fn run_program(spec: ProcessCommand) -> Result<Output, TorclError> {
    let mut command = match spec {
        ProcessCommand::Shell(text) => shell_command(&text),
        ProcessCommand::Argv(parts) => {
            let (program, arguments) = parts
                .split_first()
                .ok_or_else(|| TorclError::ProgramError("run-program: empty command".into()))?;
            let mut command = Command::new(program);
            command.args(arguments);
            command
        }
    };
    command
        .output()
        .map_err(|e| TorclError::FileError(format!("run-program: {e}")))
}

#[cfg(unix)]
fn shell_command(text: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(text);
    command
}

#[cfg(windows)]
fn shell_command(text: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
    // /S strips these enclosing quotes. The contents are intentionally shell
    // syntax; CRT argv escaping would change embedded quotes and redirections.
    command
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{text}\""));
    command
}
