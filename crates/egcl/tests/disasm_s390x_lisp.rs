// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The in-process s390x disassembler (lib/disasm-s390x.lisp, bliss-ehjj1) must
//! decode exactly as GNU objdump does.
//!
//! WHY THIS RUNS ON EVERY HOST: the decoder is pure Common Lisp over a vector
//! of octets, so an x86-64 egcl exercises it just as a native s390x one does.
//! Each fixture pairs the hex of some machine code with the listing objdump
//! 2.44 (Debian 13, s390x) produced for it, with addresses and encodings
//! stripped: `general` is a hand-written corpus of ~430 general and BFP
//! instructions assembled with llvm-mc (`general.s`), `masks` every mask value
//! of every condition-suffixed family (`masks.s`), and `t1-native`/`t2-native`
//! are the T1 and T2 code of real functions captured on an IBM z17. A new
//! mnemonic goes into the table AND into `general.s`, regenerating the `.ref`
//! with objdump; the decoder is never the oracle for itself.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/disasm-s390x")
}

fn decoder_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/disasm-s390x.lisp")
}

/// Decode `name.hex` with the Lisp disassembler in a fresh egcl and return its
/// objdump-shaped listing.
fn decode(name: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("disasm-s390x-tests");
    fs::create_dir_all(&dir).unwrap();
    let driver = dir.join(format!("{name}-driver.lisp"));
    let output_path = dir.join(format!("{name}.out"));
    fs::write(
        &driver,
        format!(
            r#"(load {decoder:?})
(let* ((hex (with-open-file (s {hex:?}) (read-line s)))
       (n (floor (length hex) 2))
       (v (make-array n :element-type '(unsigned-byte 8))))
  (dotimes (i n)
    (setf (aref v i) (parse-integer hex :start (* 2 i) :end (+ 2 (* 2 i)) :radix 16)))
  (with-open-file (o {out:?} :direction :output :if-exists :supersede)
    (write-string (egcl-disasm:format-objdump-lines v) o)))
"#,
            decoder = decoder_path().display().to_string(),
            hex = fixture_dir()
                .join(format!("{name}.hex"))
                .display()
                .to_string(),
            out = output_path.display().to_string(),
        ),
    )
    .unwrap();
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
fn every_condition_mask_spelling_matches_objdump() {
    check("masks");
}

#[test]
fn captured_t1_native_code_matches_objdump() {
    check("t1-native");
}

#[test]
fn captured_t2_native_code_matches_objdump() {
    check("t2-native");
}

/// An unknown opcode must still advance by the architectural length so the
/// instructions after it stay aligned, and truncated trailing bytes must not
/// crash the decoder.
#[test]
fn unknown_opcodes_keep_their_length_and_truncation_is_safe() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("disasm-s390x-tests");
    fs::create_dir_all(&dir).unwrap();
    let driver = dir.join("unknown-driver.lisp");
    fs::write(
        &driver,
        format!(
            r#"(load {decoder:?})
;; b2 fe 00 00: a 4-byte S-format opcode the table does not know; then LGR;
;; then e3 00 00 00 00 ff: a 6-byte RXY opcode it does not know; then BR;
;; then a lone c0 byte that promises 6 bytes which are not there.
(write-string (egcl-disasm:format-objdump-lines
               '(#xb2 #xfe #x00 #x00  #xb9 #x04 #x00 #x82  #xe3 #x00 #x00 #x00 #x00 #xff  #x07 #xfe  #xc0)))
"#,
            decoder = decoder_path().display().to_string(),
        ),
    )
    .unwrap();
    let output = Command::new(BIN)
        .args(["--no-init", "--load"])
        .arg(&driver)
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            ".byte\t0xb2,0xfe,0x00,0x00",
            "lgr\t%r8,%r2",
            ".byte\t0xe3,0x00,0x00,0x00,0x00,0xff",
            "br\t%r14",
            ".byte\t0xc0",
        ]
    );
}
