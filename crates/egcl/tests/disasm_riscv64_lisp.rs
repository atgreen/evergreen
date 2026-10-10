// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The in-process riscv64 disassembler (lib/disasm-riscv64.lisp,
//! bliss-miro8.10) must decode exactly as GNU objdump does.
//!
//! WHY THIS RUNS ON EVERY HOST: the decoder is pure Common Lisp over a vector
//! of octets, so an x86-64 egcl exercises it just as a native riscv64 one does.
//! Each fixture pairs the hex of some machine code with the listing
//! `objdump -D -b binary -m riscv:rv64` (binutils 2.44, Debian 13, riscv64)
//! produced for it, with addresses and encodings stripped. `general` is a
//! hand-written corpus of RV64IMAFD, Zicsr and Zifencei instructions and every
//! alias objdump prefers (`general.s`, assembled with `.option norvc`);
//! `compressed` covers the C extension (`compressed.s`); `random` is 6 KiB of
//! seeded random bytes, so every compressed shape, reserved encoding and
//! multi-halfword length rule is hit the way a literal pool hits it; and
//! `t1-native`/`t2-native` are the T1 and T2 code of real functions captured
//! on a SpacemiT X60. A new mnemonic goes into the decoder AND into
//! `general.s`, regenerating the `.ref` with objdump on an RV64 host
//! (`fixtures/disasm-riscv64/make-fixtures.sh`); the decoder is never the
//! oracle for itself.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/disasm-riscv64")
}

fn decoder_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/disasm-riscv64.lisp")
}

fn scratch_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("disasm-riscv64-tests");
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_driver(name: &str, source: &str) -> String {
    let driver = scratch_dir().join(format!("{name}-driver.lisp"));
    fs::write(&driver, source).unwrap();
    let output = Command::new(BIN)
        .args(["--no-init", "--load"])
        .arg(&driver)
        .output()
        .expect("run egcl");
    assert!(
        output.status.success(),
        "{name}: egcl failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Decode `name.hex` with the Lisp disassembler in a fresh egcl and return its
/// objdump-shaped listing.
fn decode(name: &str) -> String {
    let output_path = scratch_dir().join(format!("{name}.out"));
    run_driver(
        name,
        &format!(
            r#"(load {decoder:?})
(let* ((hex (with-open-file (s {hex:?}) (read-line s)))
       (n (floor (length hex) 2))
       (v (make-array n :element-type '(unsigned-byte 8))))
  (dotimes (i n)
    (setf (aref v i) (parse-integer hex :start (* 2 i) :end (+ 2 (* 2 i)) :radix 16)))
  (with-open-file (o {out:?} :direction :output :if-exists :supersede)
    (write-string (egcl-disasm-riscv64:format-objdump-lines v) o)))
"#,
            decoder = decoder_path().display().to_string(),
            hex = fixture_dir()
                .join(format!("{name}.hex"))
                .display()
                .to_string(),
            out = output_path.display().to_string(),
        ),
    );
    fs::read_to_string(&output_path).unwrap()
}

fn check(name: &str) {
    let expected = fs::read_to_string(fixture_dir().join(format!("{name}.ref"))).unwrap();
    let actual = decode(name);
    let mismatches: Vec<String> = expected
        .lines()
        .zip(actual.lines())
        .enumerate()
        .filter(|(_, (e, a))| e != a)
        .map(|(i, (e, a))| format!("line {}: objdump {e:?} vs lisp {a:?}", i + 1))
        .collect();
    assert!(
        mismatches.is_empty() && expected.lines().count() == actual.lines().count(),
        "{name}: {} of {} instructions differ from objdump (first few below); \
         line counts {} vs {}\n{}",
        mismatches.len(),
        expected.lines().count(),
        expected.lines().count(),
        actual.lines().count(),
        mismatches
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn general_instruction_corpus_matches_objdump() {
    check("general");
}

#[test]
fn compressed_instruction_corpus_matches_objdump() {
    check("compressed");
}

#[test]
fn random_bytes_match_objdump_length_rules_and_reserved_encodings() {
    check("random");
}

#[test]
fn captured_t1_native_code_matches_objdump() {
    check("t1-native");
}

#[test]
fn captured_t2_native_code_matches_objdump() {
    check("t2-native");
}

/// Truncated trailing bytes must not crash the decoder: an encoding that
/// promises more bytes than remain is printed as `.byte`, and nothing before
/// it drifts.
#[test]
fn truncation_is_safe_and_keeps_earlier_offsets() {
    let stdout = run_driver(
        "truncation",
        &format!(
            r#"(load {decoder:?})
;; addi sp,sp,-48; then a lone 0x13 byte that promises a 32-bit instruction;
;; then, separately, 0x1f 0x00 which promises 48 bits.
(write-string (egcl-disasm-riscv64:format-objdump-lines '(#x13 #x01 #x01 #xfd #x13)))
(write-string (egcl-disasm-riscv64:format-objdump-lines '(#x1f #x00)))
"#,
            decoder = decoder_path().display().to_string(),
        ),
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["addi\tsp,sp,-48", ".byte\t0x13", ".byte\t0x1f,0x00"]
    );
}
