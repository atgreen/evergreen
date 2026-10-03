// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native operations for the independently implemented EGCL-POSIX module.
use egcl_rt::{EgclError, EgclVal};

#[cfg(not(unix))]
pub fn call(_args: &[EgclVal]) -> Result<[EgclVal; 3], EgclError> {
    Err(EgclError::ProgramError(
        "EGCL-POSIX requires a Unix host".into(),
    ))
}

#[cfg(unix)]
pub fn call(args: &[EgclVal]) -> Result<[EgclVal; 3], EgclError> {
    use egcl_rt::value::{NIL, T};
    let Some(operation) = args.first() else {
        return Err(EgclError::ProgramError("missing POSIX operation".into()));
    };
    let name = egcl_rt::symbols::symbol_name_of(*operation).unwrap_or_default();
    let name = name.rsplit(':').next().unwrap_or_default();
    let args = &args[1..];
    let integer = |value: EgclVal| -> Result<i32, EgclError> {
        if value.is_fixnum() {
            if let Ok(value) = i32::try_from(value.as_fixnum()) {
                return Ok(value);
            }
        }
        Err(EgclError::TypeError {
            datum: value,
            expected: "(SIGNED-BYTE 32)".into(),
        })
    };
    let fix = |value: i32| EgclVal::from_fixnum(i64::from(value));
    let mut secondary = NIL;
    let mut errno = NIL;
    let result = match (name, args) {
        #[cfg(target_os = "linux")]
        ("RAW-SYSCALL", [number, arguments @ ..]) if arguments.len() <= 6 => {
            use egcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer};
            let word = |value: EgclVal| -> Result<libc::c_long, EgclError> {
                let bits = (std::mem::size_of::<libc::c_long>() * 8) as u8;
                marshal_to_c(value, &AlienType::Int { signed: true, bits })
                    .or_else(|_| {
                        marshal_to_c(
                            value,
                            &AlienType::Int {
                                signed: false,
                                bits,
                            },
                        )
                    })
                    .map(|bits| bits as libc::c_long)
            };
            let number = word(*number)?;
            let mut words = [0 as libc::c_long; 6];
            for (slot, value) in words.iter_mut().zip(arguments) {
                *slot = if ForeignPointer::is_pointer(*value) {
                    ForeignPointer::from_lisp(*value)?.call_address()? as libc::c_long
                } else {
                    word(*value)?
                };
            }
            let (result, error) = crate::process::process_operation("RAW-SYSCALL", move || {
                // SAFETY: callers select the Linux syscall ABI and own all foreign
                // buffers until return. Only native words enter the blocking worker.
                let result = unsafe {
                    libc::syscall(
                        number, words[0], words[1], words[2], words[3], words[4], words[5],
                    )
                };
                // Capture on the same native thread, before any other operation.
                let error = if result == -1 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((result, error))
            })?;
            errno = error.map(fix).unwrap_or(NIL);
            egcl_rt::bignum::BigInt::from_i64(result).to_val()
        }
        ("STRERROR", [code]) => {
            let code = integer(*code)?;
            let mut buffer = vec![0u8; 128];
            let text = loop {
                // SAFETY: strerror_r writes at most buffer.len() bytes to this
                // owned buffer. Unlike strerror it does not share static storage.
                let status =
                    unsafe { libc::strerror_r(code, buffer.as_mut_ptr().cast(), buffer.len()) };
                if status == libc::ERANGE {
                    buffer.resize(buffer.len() * 2, 0);
                    continue;
                }
                let length = buffer
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(buffer.len());
                break if length == 0 {
                    format!("Unknown error {code}")
                } else {
                    String::from_utf8_lossy(&buffer[..length]).into_owned()
                };
            };
            // All data needed after this allocation is native owned storage;
            // no unrooted Lisp value or shared libc buffer crosses it.
            crate::streams::make_lisp_string_fresh(&text)
        }
        // These libc calls have no pointer arguments and cannot fail.
        ("GETPID", []) => fix(unsafe { libc::getpid() }),
        ("GETPPID", []) => fix(unsafe { libc::getppid() }),
        ("GETPAGESIZE", []) => {
            // SAFETY: sysconf has no pointer arguments; this queries the host,
            // including kernels configured with non-4-KiB pages.
            let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if size == -1 {
                errno = fix(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL));
            }
            egcl_rt::bignum::BigInt::from_i64(size as i64).to_val()
        }
        ("MMAP", [address, length, protection, flags, fd, offset]) => {
            use egcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer};
            let address = if address.is_nil() {
                0
            } else {
                ForeignPointer::from_lisp(*address)?.call_address()?
            };
            let length = marshal_to_c(
                *length,
                &AlienType::Int {
                    signed: false,
                    bits: usize::BITS as u8,
                },
            )? as usize;
            let protection = integer(*protection)?;
            let flags = integer(*flags)?;
            let fd = integer(*fd)?;
            let offset = marshal_to_c(
                *offset,
                &AlienType::Int {
                    signed: true,
                    bits: (std::mem::size_of::<libc::off_t>() * 8) as u8,
                },
            )? as libc::off_t;
            let (mapped, error) = crate::process::process_operation("MMAP", move || {
                // SAFETY: native scalar arguments follow mmap's ABI. Like the
                // FFI, callers own the lifetime and any fixed-address choice.
                let mapped =
                    unsafe { libc::mmap(address as *mut _, length, protection, flags, fd, offset) };
                let error = if mapped == libc::MAP_FAILED {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((mapped as usize, error))
            })?;
            if let Some(error) = error {
                errno = fix(error);
                fix(-1)
            } else {
                ForeignPointer::from_address(mapped).into_lisp()?
            }
        }
        ("MUNMAP", [address, length]) => {
            use egcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer};
            let address = ForeignPointer::from_lisp(*address)?.call_address()?;
            let length = marshal_to_c(
                *length,
                &AlienType::Int {
                    signed: false,
                    bits: usize::BITS as u8,
                },
            )? as usize;
            let (result, error) = crate::process::process_operation("MUNMAP", move || {
                // SAFETY: the caller owns the mapping and its lifetime.
                let result = unsafe { libc::munmap(address as *mut _, length) };
                let error = if result == -1 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((result, error))
            })?;
            errno = error.map(fix).unwrap_or(NIL);
            fix(result)
        }
        ("STAT", [path]) => {
            use std::os::unix::fs::MetadataExt;
            if !path.is_string() {
                return Err(EgclError::TypeError {
                    datum: *path,
                    expected: "STRING".into(),
                });
            }
            let path = path.as_string();
            let metadata = crate::process::process_operation("STAT", move || {
                Ok(std::fs::metadata(path)
                    .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO)))
            })?;
            match metadata {
                Ok(metadata) => {
                    let fields = [
                        metadata.dev() as i128,
                        metadata.ino() as i128,
                        metadata.mode() as i128,
                        metadata.nlink() as i128,
                        metadata.uid() as i128,
                        metadata.gid() as i128,
                        metadata.rdev() as i128,
                        metadata.size() as i128,
                        metadata.atime() as i128,
                        metadata.mtime() as i128,
                        metadata.ctime() as i128,
                        metadata.blksize() as i128,
                        metadata.blocks() as i128,
                    ];
                    stat_fields_vector(&fields)
                }
                Err(error) => {
                    errno = fix(error);
                    fix(-1)
                }
            }
        }
        ("OPEN", [path, flags, mode]) => {
            if !path.is_string() {
                return Err(EgclError::TypeError {
                    datum: *path,
                    expected: "STRING".into(),
                });
            }
            let path = std::ffi::CString::new(path.as_string()).map_err(|_| {
                EgclError::ProgramError("POSIX path contains a null character".into())
            })?;
            let flags = integer(*flags)?;
            let mode = integer(*mode)? as libc::mode_t;
            let (result, error) = crate::process::process_operation("OPEN", move || {
                // SAFETY: the owned path is null-terminated; mode is supplied
                // even when flags do not require the variadic third argument.
                let result = unsafe { libc::open(path.as_ptr(), flags, mode) };
                let error = if result == -1 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((result, error))
            })?;
            errno = error.map(fix).unwrap_or(NIL);
            fix(result)
        }
        ("CHMOD", [path, mode]) => {
            // The reason this exists: an APK signing key must land 0600 no
            // matter what umask the caller carries, and OPEN's mode argument
            // can only be narrowed by the umask, never widened. Without a way
            // to set the mode outright, the only safe key writer was a shell
            // wrapper that set umask 077 first.
            if !path.is_string() {
                return Err(EgclError::TypeError {
                    datum: *path,
                    expected: "STRING".into(),
                });
            }
            let path = std::ffi::CString::new(path.as_string()).map_err(|_| {
                EgclError::ProgramError("POSIX path contains a null character".into())
            })?;
            let mode = integer(*mode)? as libc::mode_t;
            let (result, error) = crate::process::process_operation("CHMOD", move || {
                // SAFETY: the owned path is null-terminated and mode is a
                // plain scalar; chmod takes no other pointer.
                let result = unsafe { libc::chmod(path.as_ptr(), mode) };
                let error = if result == -1 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((result, error))
            })?;
            errno = error.map(fix).unwrap_or(NIL);
            fix(result)
        }
        ("CLOSE", [fd]) => {
            let fd = integer(*fd)?;
            let (result, error) = crate::process::process_operation("CLOSE", move || {
                // SAFETY: invalid descriptors are reported through errno.
                // Do not retry EINTR: on Linux the descriptor is already closed.
                let result = unsafe { libc::close(fd) };
                let error = if result == -1 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                Ok((result, error))
            })?;
            errno = error.map(fix).unwrap_or(NIL);
            fix(result)
        }
        ("KILL", [pid, signal]) => {
            let pid = integer(*pid)?;
            let signal = integer(*signal)?;
            // SAFETY: libc accepts every pid/signal integer, reporting invalid
            // values through errno. Capture errno before any other OS work.
            let result = unsafe { libc::kill(pid, signal) };
            if result == -1 {
                errno = fix(std::io::Error::last_os_error().raw_os_error().unwrap());
            }
            fix(result)
        }
        ("WAITPID", [pid, options]) => {
            let pid = integer(*pid)?;
            let options = integer(*options)?;
            // Reuse the process library's native-blocking/fiber-worker policy.
            // Only native integers cross the blocking boundary; no Lisp values
            // or GC roots are read by the worker.
            let (result, status, error) =
                crate::process::process_operation("WAITPID", move || {
                    let mut status = 0;
                    // SAFETY: status points to a live writable libc int.
                    let result = unsafe { libc::waitpid(pid, &mut status, options) };
                    let error = if result == -1 {
                        std::io::Error::last_os_error().raw_os_error()
                    } else {
                        None
                    };
                    Ok((result, status, error))
                })?;
            secondary = fix(status);
            errno = error.map(fix).unwrap_or(NIL);
            fix(result)
        }
        ("WIFEXITED", [status]) => {
            if libc::WIFEXITED(integer(*status)?) {
                T
            } else {
                NIL
            }
        }
        ("WIFSIGNALED", [status]) => {
            if libc::WIFSIGNALED(integer(*status)?) {
                T
            } else {
                NIL
            }
        }
        ("WIFSTOPPED", [status]) => {
            if libc::WIFSTOPPED(integer(*status)?) {
                T
            } else {
                NIL
            }
        }
        ("WEXITSTATUS", [status]) => fix(libc::WEXITSTATUS(integer(*status)?)),
        ("WTERMSIG", [status]) => fix(libc::WTERMSIG(integer(*status)?)),
        ("WSTOPSIG", [status]) => fix(libc::WSTOPSIG(integer(*status)?)),
        ("WNOHANG", []) => fix(libc::WNOHANG),
        ("O-RDONLY", []) => fix(libc::O_RDONLY),
        ("PROT-READ", []) => fix(libc::PROT_READ),
        ("PROT-WRITE", []) => fix(libc::PROT_WRITE),
        ("PROT-NONE", []) => fix(libc::PROT_NONE),
        ("MAP-SHARED", []) => fix(libc::MAP_SHARED),
        ("MAP-PRIVATE", []) => fix(libc::MAP_PRIVATE),
        ("MAP-ANON", []) => fix(libc::MAP_ANON),
        ("WUNTRACED", []) => fix(libc::WUNTRACED),
        ("SIGSTOP", []) => fix(libc::SIGSTOP),
        ("SIGCONT", []) => fix(libc::SIGCONT),
        ("SIGTERM", []) => fix(libc::SIGTERM),
        ("SIGKILL", []) => fix(libc::SIGKILL),
        _ => {
            return Err(EgclError::ProgramError(format!(
                "invalid POSIX operation/arguments: {name}"
            )));
        }
    };
    Ok([result, secondary, errno])
}

#[cfg(unix)]
fn stat_fields_vector(fields: &[i128]) -> EgclVal {
    egcl_rt::rooted!(values = Vec::<EgclVal>::with_capacity(fields.len()));
    for &field in fields {
        #[cfg(test)]
        let before = values.first().map(|value| value.to_raw());
        let magnitude = field.unsigned_abs();
        let integer = egcl_rt::bignum::BigInt::from_mag(
            if field < 0 { -1 } else { 1 },
            vec![magnitude as u64, (magnitude >> 64) as u64],
        )
        .to_val();
        #[cfg(test)]
        if before != values.first().map(|value| value.to_raw()) {
            eprintln!("STAT-FIELD-RELOCATED");
        }
        values.push(integer);
    }
    crate::sequences::build_simple_vector(&values)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn large_stat_fields_survive_relocation() {
        const CHILD: &str = "EGCL_STAT_RELOCATION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "posix::tests::large_stat_fields_survive_relocation",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env_remove("EGCL_GC_STRESS_SKIP")
                .env_remove("EGCL_GC_STRESS_AT")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("STAT-FIELD-RELOCATED"));
            return;
        }
        egcl_rt::rooted!(
            vector =
                stat_fields_vector(&[u64::MAX as i128, i64::MIN as i128, 0, i64::MAX as i128,])
        );
        let expected = [
            (1, vec![u64::MAX]),
            (-1, vec![1u64 << 63]),
            (0, vec![]),
            (1, vec![i64::MAX as u64]),
        ];
        for (index, (sign, magnitude)) in expected.into_iter().enumerate() {
            let value = crate::sequences::elt(*vector, index).unwrap();
            let number = egcl_rt::bignum::bigint_from_val(value).unwrap();
            assert_eq!(number.sign, sign);
            assert_eq!(number.mag, magnitude);
        }
    }

    // `child` is never `wait()`ed through its `Child` handle, and that is the
    // point of the test: the WAITPID builtin under test reaps it instead. Adding
    // `child.wait()` would not silence a real leak, it would race the builtin for
    // the same exit status and leave whichever lost reporting ECHILD.
    #[test]
    #[allow(clippy::zombie_processes)]
    fn waitpid_reports_running_and_exited_unmanaged_children() {
        // A pipe keeps the child alive until after WNOHANG, without a timing
        // assumption. This child is deliberately outside the managed reaper.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read value; exit 7"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = EgclVal::from_fixnum(i64::from(child.id()));
        let operation = EgclVal::from_symbol_index(egcl_rt::symbols::intern("WAITPID"));
        let polled = call(&[
            operation,
            pid,
            EgclVal::from_fixnum(i64::from(libc::WNOHANG)),
        ])
        .unwrap();
        assert_eq!(polled[0].as_fixnum(), 0);
        assert!(polled[2].is_nil());

        drop(child.stdin.take());
        let waited = call(&[operation, pid, EgclVal::from_fixnum(0)]).unwrap();
        assert_eq!(waited[0], pid);
        assert!(waited[2].is_nil());
        let status = waited[1].as_fixnum() as i32;
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 7);
    }
}
