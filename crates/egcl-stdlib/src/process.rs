// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Owned subprocesses and synchronous capture for EGCL-EXT.
use std::io;
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use egcl_rt::error::EgclError;
use egcl_rt::sync::{BlockingMode, EgclSemaphore, blocking_mode};

pub enum ProcessCommand {
    Shell(String),
    Argv(Vec<String>),
}

fn command(spec: ProcessCommand) -> Result<Command, EgclError> {
    Ok(match spec {
        ProcessCommand::Shell(text) => shell_command(&text),
        ProcessCommand::Argv(parts) => {
            let (program, arguments) = parts
                .split_first()
                .ok_or_else(|| EgclError::ProgramError("empty process command".into()))?;
            let mut command = Command::new(program);
            command.args(arguments);
            command
        }
    })
}

/// Capture both output pipes concurrently through std::process, avoiding the
/// deadlock caused by draining stdout before a child fills its stderr pipe.
pub fn run_program(spec: ProcessCommand) -> Result<Output, EgclError> {
    let mut command = command(spec)?;
    process_operation("RUN-PROGRAM", move || command.output())
}

/// A child with separately owned pipes. Taking a pipe transfers its lifetime to
/// the caller (normally a Lisp stream). Drop closes only pipes still held here;
/// it neither waits for nor kills the child. A native reaper owns the child until
/// exit, including when the public owner has been abandoned.
pub struct Process {
    child: Arc<Mutex<Child>>,
    id: u32,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

const WAIT_INTERVAL: Duration = Duration::from_millis(2);

impl Process {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Observe and cache exit status without consuming it or closing the pipes.
    pub fn try_wait(&self) -> Result<Option<ExitStatus>, EgclError> {
        self.child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|e| EgclError::FileError(format!("process status: {e}")))
    }

    /// Wait for exit, leaving the child running on timeout. Repeated and
    /// concurrent waits see the same cached status. Callers must drain output
    /// and close stdin themselves when required by the child's protocol.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Option<ExitStatus>, EgclError> {
        let start = Instant::now();
        if let Some(status) = self.try_wait()? {
            return Ok(Some(status));
        }
        if timeout == Some(Duration::ZERO) {
            return Ok(None);
        }
        let mode = blocking_mode("PROCESS-WAIT")?;
        // SAFETY: this scope uses only Rust-owned child state and timestamps.
        // No Lisp values or root slots are read until native blocking ends.
        let _blocked = (mode == BlockingMode::Native)
            .then(|| unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() });
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            let pause = match timeout {
                Some(limit) => match limit.checked_sub(start.elapsed()) {
                    Some(remaining) if !remaining.is_zero() => remaining.min(WAIT_INTERVAL),
                    _ => return Ok(None),
                },
                None => WAIT_INTERVAL,
            };
            match mode {
                BlockingMode::Native => std::thread::sleep(pause),
                BlockingMode::Fiber => egcl_rt::sync::fiber_sleep(pause)?,
            }
        }
    }

    /// Request immediate termination of this child, not its descendants.
    /// Wait separately to observe completion and drain any buffered output.
    pub fn terminate(&self) -> Result<(), EgclError> {
        let mut child = self.child.lock().unwrap();
        let result = child.try_wait().and_then(|status| {
            if status.is_some() {
                Ok(())
            } else {
                child.kill()
            }
        });
        result.map_err(|e| EgclError::FileError(format!("process terminate: {e}")))
    }
}

/// Launch with three owned pipes, preserving direct argv versus shell syntax.
/// No pipe is drained automatically: callers may exchange data before exit.
pub fn launch_program(spec: ProcessCommand) -> Result<Process, EgclError> {
    let command = command(spec)?;
    process_operation("LAUNCH-PROGRAM", move || spawn_process(command))
}

fn spawn_process(mut command: Command) -> io::Result<Process> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let id = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let reaped = Arc::clone(&child);
    let reaper = std::thread::Builder::new()
        .name("egcl-process-reaper".into())
        .spawn(move || {
            loop {
                let status = reaped.lock().unwrap().try_wait();
                match status {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(WAIT_INTERVAL),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) =>
                    {
                        std::thread::sleep(WAIT_INTERVAL);
                    }
                    // ECHILD (e.g. a host SIGCHLD handler reaped it) and other
                    // permanent failures cannot make progress by polling. The
                    // public owner can still report the OS error on status/wait.
                    Err(_) => break,
                }
            }
        });
    if let Err(error) = reaper {
        // Launch runs on a helper or inside NativeBlockingScope. Failure to
        // establish ownership must not leave an unreported child or zombie.
        let mut child = child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(Process {
        child,
        id,
        stdin,
        stdout,
        stderr,
    })
}

pub(crate) fn process_operation<T: Send + 'static>(
    operation: &'static str,
    action: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, EgclError> {
    // Apply the pinned policy before starting a child with observable effects.
    let output = match blocking_mode(operation)? {
        BlockingMode::Native => {
            // SAFETY: command/output contain owned Rust data only. No Lisp
            // values, root slots, or callbacks are accessed while waiting.
            let _blocked = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
            action()
        }
        BlockingMode::Fiber => on_worker(action)?,
    };
    output.map_err(|e| EgclError::FileError(format!("{operation}: {e}")))
}

/// std::process drains both pipes with blocking OS operations. Keep those
/// operations on a helper and park the caller through the runtime semaphore.
fn on_worker<T: Send + 'static>(
    action: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<io::Result<T>, EgclError> {
    let completed = Arc::new(EgclSemaphore::new(None, 0)?);
    let result = Arc::new(Mutex::new(None));
    let worker_completed = Arc::clone(&completed);
    let worker_result = Arc::clone(&result);
    std::thread::Builder::new()
        .name("egcl-process-operation".into())
        .spawn(move || {
            // A panic must publish a failure too, rather than strand the fiber.
            let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action))
                .unwrap_or_else(|_| Err(io::Error::other("process worker panicked")));
            *worker_result.lock().unwrap() = Some(output);
            worker_completed
                .signal(1)
                .expect("single completion permit");
        })
        .map_err(|e| EgclError::FileError(format!("process worker: {e}")))?;
    completed.wait(None)?;
    // The helper has published all its data and only releases owned Rust
    // storage after signaling. Joining its OS teardown would block the carrier.
    result
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| EgclError::Internal("process worker completed without a result".into()))
}

#[cfg(unix)]
fn shell_command(text: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(text);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaper_releases_native_owner_after_exit() {
        let process = launch_program(ProcessCommand::Shell("exit 0".into())).unwrap();
        let owner = Arc::downgrade(&process.child);
        drop(process);
        let deadline = Instant::now() + Duration::from_secs(5);
        while owner.strong_count() != 0 {
            assert!(Instant::now() < deadline, "reaper retained completed child");
            std::thread::sleep(WAIT_INTERVAL);
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn externally_reaped_child_does_not_leak_reaper() {
        if std::env::var_os("EGCL_AUTO_REAP_CHILD").is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process::tests::externally_reaped_child_does_not_leak_reaper",
                    "--nocapture",
                ])
                .env("EGCL_AUTO_REAP_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // This isolated process models a host that requests automatic child
        // reaping. Linux SIGCHLD=17, SIG_IGN=1; the parent's disposition is intact.
        unsafe { egcl_rt::syscall::rt_sigaction(17, 1, 0).unwrap() };
        reaper_releases_native_owner_after_exit();
    }
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
