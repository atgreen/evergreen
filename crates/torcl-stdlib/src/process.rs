//! Synchronous subprocess execution for TORCL-EXT:RUN-PROGRAM.
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use torcl_rt::error::TorclError;
use torcl_rt::sync::{BlockingMode, TorclSemaphore, blocking_mode};

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
    // Apply the pinned policy before starting a child with observable effects.
    let output = match blocking_mode("RUN-PROGRAM")? {
        BlockingMode::Native => {
            // SAFETY: command/output contain owned Rust data only. No Lisp
            // values, root slots, or callbacks are accessed while waiting.
            let _blocked = unsafe { torcl_rt::safepoint::NativeBlockingScope::enter() };
            command.output()
        }
        BlockingMode::Fiber => capture_on_worker(command)?,
    };
    output.map_err(|e| TorclError::FileError(format!("run-program: {e}")))
}

/// std::process drains both pipes with blocking OS operations. Keep those
/// operations on a helper and park the caller through the runtime semaphore.
fn capture_on_worker(mut command: Command) -> Result<std::io::Result<Output>, TorclError> {
    let completed = Arc::new(TorclSemaphore::new(None, 0)?);
    let result = Arc::new(Mutex::new(None));
    let worker_completed = Arc::clone(&completed);
    let worker_result = Arc::clone(&result);
    std::thread::Builder::new()
        .name("torcl-process-capture".into())
        .spawn(move || {
            // A panic must publish a failure too, rather than strand the fiber.
            let output =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| command.output()))
                    .unwrap_or_else(|_| {
                        Err(std::io::Error::other("process capture worker panicked"))
                    });
            *worker_result.lock().unwrap() = Some(output);
            worker_completed
                .signal(1)
                .expect("single completion permit");
        })
        .map_err(|e| TorclError::FileError(format!("run-program: capture worker: {e}")))?;
    completed.wait(None)?;
    // The helper has published all its data and only releases owned Rust
    // storage after signaling. Joining its OS teardown would block the carrier.
    result
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| TorclError::Internal("process capture completed without a result".into()))
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
