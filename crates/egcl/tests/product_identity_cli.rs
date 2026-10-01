// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn executable_advertises_egcl() {
    for flag in ["--help", "--version"] {
        let output = Command::new(BIN).arg(flag).output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        if flag == "--help" {
            assert!(stdout.contains("EGCL"), "{stdout}");
            assert!(stdout.contains("Usage: egcl"), "{stdout}");
        } else {
            assert!(stdout.starts_with("egcl "), "{stdout}");
        }
    }
}

/// The banner and --version identify the build and its terms: version, the exact
/// target triple, copyright, and licence.
///
/// The triple is the load-bearing part. This project ships both a musl and a glibc
/// x86-64 build and they are not interchangeable -- a static musl binary cannot
/// dlopen, so the Python and JVM bindings need the glibc one -- and *FEATURES*
/// carries the architecture and OS but never the environment, so without this a user
/// cannot tell which binary they are running.
#[test]
fn version_and_banner_identify_the_build_and_its_licence() {
    let expected_target = env!("EGCL_TARGET");
    for text in [version_output(), repl_banner()] {
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains(expected_target), "{text}");
        assert!(text.contains("Copyright (C)"), "{text}");
        assert!(text.contains("Anthony Green"), "{text}");
        // The licence the crate metadata and the RPM spec both declare. This test
        // is why the banner cannot drift silently -- it caught the relicensing
        // having updated Cargo.toml and left the banner on the old terms.
        assert!(text.contains("GPL version 3"), "{text}");
        assert!(text.contains("Classpath Exception"), "{text}");
    }
}

fn version_output() -> String {
    let output = Command::new(BIN).arg("--version").output().unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The banner as an interactive session sees it: stdin closes immediately, so the
/// REPL prints its banner and reaches EOF.
fn repl_banner() -> String {
    let output = Command::new(BIN)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn lisp_exposes_the_egcl_implementation_and_extension_packages() {
    let output = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            r#"
          (format t "IDENTITY ~S~%" (lisp-implementation-type))
          (format t "FEATURE ~S~%" (not (null (member :egcl *features*))))
          (format t "PACKAGES ~S~%"
            (mapcar (lambda (name) (not (null (find-package name))))
                    '("EGCL-INTERNAL" "EGCL-EXT" "EGCL-GRAY-STREAMS")))
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in ["IDENTITY \"EGCL\"", "FEATURE T", "PACKAGES (T T T)"] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
}
