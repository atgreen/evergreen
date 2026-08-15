//! End-to-end `.bfasl` compile/load through the CLI (bliss-lb6.6, spec §6.11):
//! compile a source file to a `.bfasl`, load it in a *fresh* process, and verify
//! version-mismatch rejection.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

fn workdir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("bliss-bfasl-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(program: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn bliss-cli")
}

fn bfasl_section(bytes: &[u8], wanted: u16) -> Option<&[u8]> {
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    let mut pos = 24usize;
    let checksum_at = bytes.len().checked_sub(4)?;
    for _ in 0..count {
        if pos + 6 > checksum_at {
            return None;
        }
        let kind = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[pos + 2..pos + 6].try_into().unwrap()) as usize;
        pos += 6;
        if pos + len > checksum_at {
            return None;
        }
        if kind == wanted {
            return Some(&bytes[pos..pos + len]);
        }
        pos += len;
    }
    None
}

fn bbu_counts(bytes: &[u8]) -> (u32, u32, u32) {
    let bbu = bfasl_section(bytes, 12).expect("compile-file must emit BYTECODE_UNIT");
    assert_eq!(&bbu[..4], b"BBU\0", "BYTECODE_UNIT has BBU magic");
    (
        u32::from_le_bytes(bbu[12..16].try_into().unwrap()),
        u32::from_le_bytes(bbu[16..20].try_into().unwrap()),
        u32::from_le_bytes(bbu[20..24].try_into().unwrap()),
    )
}

#[test]
fn compile_file_then_load_round_trips_in_a_fresh_process() {
    let dir = workdir("roundtrip");
    let src = dir.join("u.lisp");
    let out = dir.join("u.bfasl");
    fs::write(&src, "(defun bf-sq (x) (* x x))\n(defvar *bf-g* 7)\n").unwrap();

    // Process 1: compile source → .bfasl.
    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(out.exists(), "compile-file produced no .bfasl");
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..6], b"BFASL\0", "output is a real .bfasl");
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(function_count > 0, "BYTECODE_UNIT contains bytecode functions");
    assert!(load_action_count > 0, "BYTECODE_UNIT contains a load plan");

    // Process 2 (fresh runtime): load the .bfasl and call the compiled function.
    let l = run(&format!(
        "(progn (load \"{}\") (list (bf-sq 12) *bf-g*))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(144 7)",
        "loaded .bfasl must define the function and the variable"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compile_file_prepass_handles_eval_when_and_read_time_constants() {
    let dir = workdir("eval-when");
    let src = dir.join("e.lisp");
    let out = dir.join("e.bfasl");
    fs::write(
        &src,
        "(eval-when (:compile-toplevel) (defparameter +cf-read+ 12))
         (defun cf-readtime () #.(+ +cf-read+ 5))
         (defun cf-limit () #.most-positive-fixnum)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: stdout={} stderr={}",
        String::from_utf8_lossy(&c.stdout),
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(function_count >= 2, "expected both functions in BYTECODE_UNIT");
    assert!(load_action_count >= 2, "expected load actions for both functions");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compile_file_prepass_registers_define_package_nicknames() {
    let dir = workdir("define-package");
    let src = dir.join("p.lisp");
    let out = dir.join("p.bfasl");
    fs::write(
        &src,
        "(define-package :bf/pkg (:nicknames :bf-pkg) (:use :common-lisp) (:export #:pkg-value))
         (in-package :bf-pkg)
         (defun pkg-value () 42)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: stdout={} stderr={}",
        String::from_utf8_lossy(&c.stdout),
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(function_count > 0, "BYTECODE_UNIT contains bytecode functions");
    assert!(load_action_count > 0, "BYTECODE_UNIT contains a load plan");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn version_mismatch_is_rejected() {
    let dir = workdir("version");
    let src = dir.join("v.lisp");
    let out = dir.join("v.bfasl");
    fs::write(&src, "(defun bf-id (x) x)\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success());

    // Corrupt the major version byte (u16 LE at offset 6; high byte at 7).
    let mut bytes = fs::read(&out).unwrap();
    bytes[7] = bytes[7].wrapping_add(1);
    fs::write(&out, &bytes).unwrap();

    // Loading the incompatible file must be rejected — caught here as an error.
    let l = run(&format!(
        "(handler-case (load \"{}\") (error (e) e (print :rejected)))",
        out.display()
    ));
    let stdout = String::from_utf8_lossy(&l.stdout);
    assert!(
        stdout.to_uppercase().contains("REJECTED") || !l.status.success(),
        "a version-incompatible .bfasl must be rejected; stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&l.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}
