// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Linux evidence for T0 execution without anonymous executable mappings or
//! executable protection transitions. This is not an iOS emulator or sandbox.
#![cfg(all(
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::os::unix::process::ExitStatusExt;

const CHILD: &str = "EGCL_NOEXEC_CHILD";
const DONE: &str = "NOEXEC-T0-COMPLETED";
const NATIVE_DONE: &str = "NOEXEC-NATIVE-COMPLETED";

fn install_filter() {
    // seccomp_data: nr at 0, arch at 4, args at 16. Both supported ABIs
    // pass these flags in the low 32 bits of their 64-bit argument slots.
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    fn ins(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
    const LD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    const EQ: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    const SET: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
    const RET: u16 = (libc::BPF_RET | libc::BPF_K) as u16;
    let mut filter = vec![
        ins(LD, 0, 0, 4),
        ins(EQ, 1, 0, ARCH),
        ins(RET, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        ins(LD, 0, 0, 0),
        // Reject x32/mixed-ABI numbers too, rather than bypassing native rules.
        ins(SET, 0, 1, 0x4000_0000),
        ins(RET, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        ins(EQ, 0, 0, libc::SYS_mmap as u32),
        ins(EQ, 0, 0, libc::SYS_mprotect as u32),
        ins(EQ, 0, 0, libc::SYS_pkey_mprotect as u32),
        ins(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let mmap = filter.len();
    filter.extend([
        ins(LD, 0, 0, 16 + 3 * 8),
        ins(SET, 1, 0, libc::MAP_ANONYMOUS as u32),
        ins(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
    let protection = filter.len();
    filter.extend([
        ins(LD, 0, 0, 16 + 2 * 8),
        ins(SET, 0, 1, libc::PROT_EXEC as u32),
        ins(RET, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        ins(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
    filter[6].jt = (mmap - 7) as u8;
    filter[7].jt = (protection - 8) as u8;
    filter[8].jt = (protection - 9) as u8;
    let program = libc::sock_fprog {
        len: filter.len().try_into().unwrap(),
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: BPF storage remains alive for the synchronous kernel copy.
    // TSYNC covers existing test-harness threads and subsequently born workers.
    // A failing TID return is a failure too; never silently run unfiltered.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        let result = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &program as *const libc::sock_fprog,
        );
        assert_eq!(
            result,
            0,
            "seccomp install: {}",
            std::io::Error::last_os_error()
        );
    }
}

fn invoke(case: &str, tier: &str, stress: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "noexec_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, case)
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LAZY", "0");
    for key in [
        "EGCL_PROFILING_DISABLED",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_BOOT_FILE",
        "EGCL_CORE",
    ] {
        command.env_remove(key);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    command.output().expect("isolated noexec child")
}

fn success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "status {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn executable_memory_controls_are_killed_by_sigsys() {
    for case in ["mmap", "mprotect", "pkey_mprotect"] {
        let output = invoke(case, "t0", false);
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGSYS),
            "{case}: {:?}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn cold_boot_and_t0_workloads_need_no_executable_memory() {
    for stress in [false, true] {
        let output = invoke("t0", "t0", stress);
        success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == DONE)
                .count(),
            1
        );
    }
}

#[test]
#[cfg(not(egcl_no_t1))]
fn native_compilation_control_is_live_then_denied() {
    let live = invoke("native-unfiltered", "t1", false);
    success(&live);
    assert_eq!(
        String::from_utf8_lossy(&live.stdout)
            .lines()
            .filter(|line| line.trim() == NATIVE_DONE)
            .count(),
        1
    );
    let denied = invoke("native-filtered", "t1", false);
    assert_eq!(
        denied.status.signal(),
        Some(libc::SIGSYS),
        "native control: {:?}\n{}",
        denied.status,
        String::from_utf8_lossy(&denied.stderr)
    );
}

#[test]
fn noexec_child() {
    let Ok(case) = std::env::var(CHILD) else {
        return;
    };
    struct SourceFile(std::path::PathBuf);
    impl Drop for SourceFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let file = (case == "t0").then(|| {
        let file = SourceFile(
            std::env::temp_dir().join(format!("egcl-noexec-{}.lisp", std::process::id())),
        );
        std::fs::write(&file.0, "(defun noexec-loaded (x) (+ x 2))").unwrap();
        file
    });
    let pathname = file
        .as_ref()
        .map(|file| {
            file.0
                .to_str()
                .unwrap()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        })
        .unwrap_or_default();
    let native = case.starts_with("native-");
    let source = if native {
        format!(
            "(progn (defun noexec-hot (x) (+ x 1)) (dotimes (i 80) (assert (= (noexec-hot i) (+ i 1)))) (assert (= (egcl-ext:function-tier 'noexec-hot) 1)) (terpri) (write-line \"{NATIVE_DONE}\"))"
        )
    } else {
        format!(
            r#"(progn
          (load "{pathname}")
          (assert (= (noexec-loaded 40) 42))
          (defun noexec-hot (n) (let ((sum 0)) (dotimes (i n sum) (incf sum i))))
          (dotimes (i 80) (assert (= (noexec-hot 1000) 499500)))
          (assert (= (egcl-ext:function-tier 'noexec-hot) 0))
          (defclass noexec-box () ((value :initarg :value :accessor noexec-value)))
          (let ((box (make-instance 'noexec-box :value 42))) (assert (= (noexec-value box) 42)))
          (assert (= (handler-case (error 'type-error :datum 7 :expected-type 'string)
                       (type-error (c) (type-error-datum c))) 7))
          (assert (= (eval (read-from-string "(+ 20 22)")) 42))
          (assert (= (funcall (compile nil '(lambda (x) (+ x 2))) 40) 42))
          (let ((a (make-array 8 :element-type '(unsigned-byte 8) :initial-element 42)))
            (assert (= (aref a 7) 42)))
          (terpri) (write-line "{DONE}"))"#
        )
    };
    let args = vec![
        "egcl".into(),
        "--no-init".into(),
        "--no-image".into(),
        "--eval".into(),
        source,
    ];
    if case != "native-unfiltered" {
        install_filter();
    }
    if matches!(case.as_str(), "mmap" | "mprotect" | "pkey_mprotect") {
        // SAFETY: test-only mappings; successful executable permission is a
        // failing control, never executed. Kernel should kill before the call.
        unsafe {
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            assert!(page > 0);
            let prot = if case == "mmap" {
                libc::PROT_READ | libc::PROT_EXEC
            } else {
                libc::PROT_READ | libc::PROT_WRITE
            };
            let address = libc::mmap(
                std::ptr::null_mut(),
                page,
                prot,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(address, libc::MAP_FAILED);
            if case == "mprotect" {
                libc::mprotect(address, page, libc::PROT_READ | libc::PROT_EXEC);
            }
            if case == "pkey_mprotect" {
                libc::syscall(
                    libc::SYS_pkey_mprotect,
                    address,
                    page,
                    libc::PROT_READ | libc::PROT_EXEC,
                    -1,
                );
            }
            libc::munmap(address, page);
        }
        panic!("executable-memory control survived");
    }
    let worker = std::thread::Builder::new()
        .name("noexec-lisp".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || -> Result<i32, String> {
            // Error payloads can contain Lisp heap values. Render them while
            // the worker's execution attachment and roots are still alive;
            // its JoinHandle must carry only Rust-owned data across TLS exit.
            match egcl::run(&args) {
                Ok(code) => Ok(code),
                Err(mut error) => {
                    egcl_rt::rooted_ref!(_error_root = &mut error);
                    Err(error.to_string())
                }
            }
        })
        .expect("Lisp worker under the inherited filter");
    assert_eq!(
        worker
            .join()
            .expect("Lisp worker returned")
            .expect("Lisp workload"),
        0
    );
}
