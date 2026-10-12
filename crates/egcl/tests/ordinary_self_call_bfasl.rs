// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Source self-call proofs survive source-free BFASL and respect boot declarations.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::{fs, process::Command};

fn run(source: &str) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", source])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env_remove("EGCL_NO_DIRECT_SELF_CALL")
        .env_remove("EGCL_NATIVE_TRANSFER")
        .output()
        .expect("run ordinary self-call CLI regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

fn listing_program(name: &str) -> String {
    format!(
        r#"
        (dotimes (i 40) (assert (= ({name} 10) 55)))
        (dotimes (attempt 500)
          (when (= (egcl-ext:function-tier '{name}) 2) (return))
          (assert (= ({name} 10) 55))
          (sleep 0.02))
        (assert (= (egcl-ext:function-tier '{name}) 2))
        (assert (= ({name} 12) 144))
        (format t "SELF-LISTING-BEGIN~%")
        (disassemble #'{name})
        (format t "SELF-LISTING-END~%")
        "#
    )
}

fn ordinary_listing(output: &str) -> &str {
    let listing = output
        .split_once("SELF-LISTING-BEGIN\n")
        .expect("listing begins")
        .1
        .split_once("SELF-LISTING-END")
        .expect("listing ends")
        .0;
    assert!(listing.contains("; T2 —"), "{listing}");
    assert!(
        listing.contains("Ordinary x86-64 Lisp ABI — installed native code."),
        "{listing}"
    );
    listing
}

/// FIB's only near CALL instructions are its two recursive edges. Ordinary
/// dynamic dispatch and cold runtime adapters call through registers instead.
/// Decode the numeric operand rather than relying on an ABI label alone.
fn direct_call_targets(listing: &str) -> Vec<u64> {
    listing
        .lines()
        .filter_map(|line| {
            let (_, instruction) = line.trim().split_once(":  ")?;
            let operand = instruction
                .strip_prefix("call ")?
                .split_whitespace()
                .next()?;
            u64::from_str_radix(operand.strip_suffix('h')?, 16).ok()
        })
        .collect()
}

fn bfasl_section(bytes: &[u8], wanted: u16) -> Option<&[u8]> {
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut pos = 24;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[pos + 2..pos + 6].try_into().unwrap()) as usize;
        pos += 6;
        let section = &bytes[pos..pos + len];
        if kind == wanted {
            return Some(section);
        }
        pos += len;
    }
    None
}

fn compile_and_load_source_free(name: &str, program: &str) -> String {
    let directory = std::env::temp_dir().join(format!(
        "egcl-ordinary-self-bfasl-{}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("recursive.lisp");
    let fasl = directory.join("recursive.bfasl");
    fs::write(&source, program).unwrap();
    run(&format!("(compile-file {source:?} :output-file {fasl:?})"));
    let bytes = fs::read(&fasl).expect("compile-file produced BFASL");
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed source fallback"
    );
    assert!(
        bfasl_section(&bytes, 12).is_some(),
        "must contain bytecode unit"
    );
    fs::remove_file(&source).unwrap();
    // A fresh process must recover policy from the artifact, not the compile
    // process's registry or recompilation of its original source.
    let output = run(&format!("(load {fasl:?}) {}", listing_program(name)));
    fs::remove_dir_all(directory).unwrap();
    output
}

#[test]
fn source_free_bfasl_retains_direct_ordinary_recursive_edges() {
    let output = compile_and_load_source_free(
        "persisted-self-fib",
        "(defun persisted-self-fib (n) (if (< n 2) n (+ (persisted-self-fib (- n 1)) (persisted-self-fib (- n 2)))))",
    );
    let targets = direct_call_targets(ordinary_listing(&output));
    assert_eq!(
        targets.len(),
        2,
        "both recursive edges must call native self: {output}"
    );
    assert_eq!(
        targets[0], targets[1],
        "both edges enter the same retained body"
    );
}

#[test]
fn source_free_bfasl_retains_declaim_notinline_with_docstring() {
    let output = compile_and_load_source_free(
        "persisted-notinline-fib",
        r#"
        (declaim (notinline persisted-notinline-fib))
        (defun persisted-notinline-fib (n)
          "The declaration is established before this definition is compiled."
          (if (< n 2) n (+ (persisted-notinline-fib (- n 1))
                           (persisted-notinline-fib (- n 2)))))
    "#,
    );
    let listing = ordinary_listing(&output);
    assert!(
        direct_call_targets(listing).is_empty(),
        "BFASL must retain NOTINLINE: {listing}"
    );
    assert!(
        listing.contains("call r11"),
        "dynamic ordinary dispatch remains: {listing}"
    );
}

#[test]
fn boot_declaim_and_proclaim_notinline_keep_ordinary_calls_dynamic() {
    for declaration in [
        "(declaim (notinline declared-self-fib))",
        "(proclaim '(notinline declared-self-fib))",
    ] {
        let output = run(&format!(
            r#"{declaration}
            (defun declared-self-fib (n)
              "Declarations remain effective after this docstring."
              (if (< n 2) n (+ (declared-self-fib (- n 1)) (declared-self-fib (- n 2)))))
            {}"#,
            listing_program("declared-self-fib")
        ));
        let listing = ordinary_listing(&output);
        assert!(
            direct_call_targets(listing).is_empty(),
            "{declaration}: {listing}"
        );
        assert!(
            listing.contains("call r11"),
            "dynamic ordinary dispatch remains: {listing}"
        );
    }
}
