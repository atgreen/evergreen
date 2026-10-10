// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! DISASSEMBLE on ppc64le decodes the installed native code in process, through
//! the Common Lisp decoder in lib/disassembler.lisp (bliss-eegul). The decoder is
//! checked against the same words the assembler's encoding tests pin against GNU
//! as, run backwards; then a promoted function's listing must come out as
//! mnemonics, not as the raw-word fallback.
#![cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]
use std::process::Command;

fn run(program: &str) -> String {
    run_with_t1_threshold(program, "2")
}

/// A function's bytecode body exists only once it has been called enough times
/// to be compiled (about eight), and the baseline tier replaces it at the T1
/// threshold; a listing of T0 bytecode therefore needs a high threshold.
fn run_with_t1_threshold(program: &str, threshold: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_T0_T1_THRESHOLD", threshold)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

/// `(mnemonic, word)` pairs from asm_ppc64le.rs's encoding tests, plus the
/// branch, load/store and VSX forms the emitters use that those tests reach
/// through labels rather than fixed words.
const ORACLE: &[(&str, u32)] = &[
    ("mr 3, 4", 0x7C832378),
    ("li 3, -5", 0x3860FFFB),
    ("lis 3, 0x1234", 0x3C601234),
    ("ori 3, 3, 0x5678", 0x60635678),
    ("oris 3, 3, 0x9abc", 0x64639ABC),
    ("addi 3, 4, 100", 0x38640064),
    ("add 3, 4, 5", 0x7C642A14),
    ("subf 3, 4, 5", 0x7C642850),
    ("neg 3, 4", 0x7C6400D0),
    ("and 3, 4, 5", 0x7C832838),
    ("or 3, 4, 5", 0x7C832B78),
    ("xor 3, 4, 5", 0x7C832A78),
    ("nand 3, 4, 4", 0x7C8323B8),
    ("mulld 3, 4, 5", 0x7C6429D2),
    ("mulhd 3, 4, 5", 0x7C642892),
    ("extsw 3, 4", 0x7C8307B4),
    ("sradi 3, 4, 3", 0x7C831E74),
    ("sldi 3, 4, 3", 0x78831F24),
    ("sldi 3, 3, 32", 0x786307C6),
    ("srdi 3, 4, 3", 0x7883E8C2),
    ("cmpd 0, 3, 4", 0x7C232000),
    ("cmpdi 0, 3, 7", 0x2C230007),
    ("fadd 1, 2, 3", 0xFC22182A),
    ("fsub 1, 2, 3", 0xFC221828),
    ("fmul 1, 2, 3", 0xFC2200F2),
    ("fdiv 1, 2, 3", 0xFC221824),
    ("fadds 1, 2, 3", 0xEC22182A),
    ("fsubs 1, 2, 3", 0xEC221828),
    ("fmuls 1, 2, 3", 0xEC2200F2),
    ("fcmpu 0, 1, 2", 0xFC011000),
    ("mtvsrd 1, 3", 0x7C230166),
    ("mfvsrd 3, 1", 0x7C230066),
    ("xscvspdpn 3, 4", 0xF060252C),
    ("xscvdpspn 1, 2", 0xF020142C),
    ("mflr 0", 0x7C0802A6),
    ("mtlr 0", 0x7C0803A6),
    ("mtctr 12", 0x7D8903A6),
    ("bctr", 0x4E800420),
    ("bctrl", 0x4E800421),
    ("blr", 0x4E800020),
    ("li 3, 7", 0x38600007),
    ("std 0, 16(1)", 0xF8010010),
    ("stdu 1, -192(1)", 0xF821FF41),
    ("ld 2, 24(1)", 0xE8410018),
    ("lwz 3, 0(11)", 0x806B0000),
    ("stw 3, 0(11)", 0x906B0000),
    ("isel 3, 5, 6, 2", 0x7C65309E),
    ("nop", 0x60000000),
];

#[test]
fn decoder_matches_the_assembler_oracle() {
    let table: String = ORACLE
        .iter()
        .map(|(text, word)| format!("(\"{text}\" . #x{word:08X})"))
        .collect::<Vec<_>>()
        .join(" ");
    let out = run(&format!(
        r#"(dolist (row '({table}))
             (let ((got (egcl-disasm:decode (cdr row))))
               (unless (string= got (car row))
                 (format t "MISMATCH ~a: expected ~s got ~s~%" (cdr row) (car row) got))))
           (print :ORACLE-DONE)"#
    ));
    assert!(out.contains("ORACLE-DONE"), "{out}");
    assert!(!out.contains("MISMATCH"), "{out}");
}

#[test]
fn relative_branches_resolve_to_in_function_offsets() {
    // `b` forward by 8 from offset 0x10, `beq` back by 4 from 0x20, `bne cr1` forward.
    let out = run(r#"(print (list (egcl-disasm:decode #x48000008 #x10)
                        (egcl-disasm:decode #x4182FFFC #x20)
                        (egcl-disasm:decode #x40860008 #x00)))"#);
    assert!(
        out.contains("(\"b +0018\" \"beq +001c\" \"bne cr1, +0008\")"),
        "{out}"
    );
}

#[test]
fn disassemble_prints_mnemonics_for_promoted_native_code() {
    let out = run(r#"(defun g (a b) (+ a b))
           (dotimes (k 30) (g k 1))
           (disassemble 'g)
           (print :LISTING-DONE)"#);
    assert!(out.contains("LISTING-DONE"), "{out}");
    assert!(out.contains("bytes of powerpc64:"), "{out}");
    for expected in [
        "mflr 0",
        "std 0, 16(1)",
        "stdu 1, -",
        "blr",
        "mtctr 12",
        "bctrl",
    ] {
        assert!(out.contains(expected), "missing {expected:?}:\n{out}");
    }
    assert!(out.contains("; return"), "{out}");
    assert!(!out.contains("Raw bytes"), "{out}");
    assert!(
        !out.contains(".long"),
        "undecoded word in emitted code:\n{out}"
    );
}

#[test]
fn disassemble_accepts_a_function_object() {
    let out = run(r#"(defun h (x) (* x 2))
           (dotimes (k 30) (h k))
           (disassemble #'h)
           (print :DONE)"#);
    assert!(out.contains("DONE"), "{out}");
    assert!(out.contains("; H — 1 arg(s)"), "{out}");
    assert!(out.contains("mflr 0"), "{out}");
}

#[test]
fn disassemble_still_lists_bytecode_for_a_function_without_native_code() {
    let out = run_with_t1_threshold(
        r#"(defun cold (x) x)
           (dotimes (k 12) (cold k))
           (disassemble 'cold)
           (print :DONE)"#,
        "100",
    );
    assert!(out.contains("DONE"), "{out}");
    assert!(
        out.contains("T0 — tree-walked forms compiled to stack bytecode"),
        "{out}"
    );
    assert!(!out.contains("powerpc64"), "{out}");
}
