// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared libraries embedded in the image (bliss-rhsy4.1/.2). On glibc the
//! `c-ffi` backend loads them through a sealed memfd: a saved executable runs
//! its embedded libraries after the original files are gone, an embedded
//! DT_NEEDED dependency is found by SONAME, a payload altered in the file is
//! refused, and a SONAME already loaded from elsewhere is reported rather than
//! silently shadowed.
#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn egcl(binary: &Path, form: &str) -> Output {
    Command::new(binary)
        .env(
            "EGCL_INIT_FILE",
            std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
        )
        .args(["--no-init", "--eval", form])
        .output()
        .expect("run egcl")
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const MARKER: &str = "EGCL-EMBEDDED-DEP-MARKER";

/// libegcldep.so.1, and libegclmain.so.1 which needs it.
fn build_libraries(dir: &Path) -> (PathBuf, PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("dep.c"),
        format!("const char *dep_marker(void) {{ return \"{MARKER}\"; }}\nint dep_value(void) {{ return 41; }}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("main.c"),
        "int dep_value(void);\nint main_value(void) { return dep_value() + 1; }\n",
    )
    .unwrap();
    let cc = |args: &[&str]| {
        let status = Command::new("cc")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("run cc");
        assert!(status.success(), "cc {args:?} failed");
    };
    cc(&[
        "-shared",
        "-fPIC",
        "-Wl,-soname,libegcldep.so.1",
        "dep.c",
        "-o",
        "libegcldep.so.1",
    ]);
    cc(&[
        "-shared",
        "-fPIC",
        "-Wl,-soname,libegclmain.so.1",
        "main.c",
        "-L.",
        "-l:libegcldep.so.1",
        "-o",
        "libegclmain.so.1",
    ]);
    (dir.join("libegcldep.so.1"), dir.join("libegclmain.so.1"))
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("egcl-embedded-so-{name}-{}", std::process::id()))
}

/// Embed both libraries and save an executable to `app`.
fn save_app(dep: &Path, main: &Path, app: &Path) {
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:embed-foreign-library {dep:?})
                 (egcl-ffi:embed-foreign-library {main:?} :names '("egclmain"))
                 (egcl-ext:save-lisp-and-die {app:?} :executable t))"#
        ),
    );
    assert!(output.status.success(), "{}", text(&output));
}

const CALL_MAIN: &str = r#"(progn
  (format t "EMBEDDED ~s~%" (egcl-ffi:embedded-foreign-libraries))
  (format t "MAIN ~d~%" (egcl-ffi:foreign-call
                          (egcl-ffi:foreign-symbol-pointer
                            "main_value" (egcl-ffi:load-foreign-library "libegclmain.so.1"))
                          :int '() '()))
  (format t "ALIAS ~d~%" (egcl-ffi:foreign-call
                           (egcl-ffi:foreign-symbol-pointer
                             "main_value" (egcl-ffi:load-foreign-library "egclmain"))
                           :int '() '()))
  (with-open-file (maps "/proc/self/maps")
    (loop for line = (read-line maps nil) while line
          when (search "/memfd:libegcl" line)
            do (format t "MAP ~a~%" line))))"#;

#[test]
fn saved_executable_loads_embedded_libraries_after_the_files_are_gone() {
    let dir = scratch("run");
    let (dep, main) = build_libraries(&dir.join("lib"));
    let app = dir.join("app");
    save_app(&dep, &main, &app);
    std::fs::remove_dir_all(dir.join("lib")).unwrap();

    let output = egcl(&app, CALL_MAIN);
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(
        out.contains(r#"EMBEDDED ("libegcldep.so.1" "libegclmain.so.1")"#),
        "{out}"
    );
    assert!(out.contains("MAIN 42"), "{out}");
    assert!(out.contains("ALIAS 42"), "{out}");
    // Both came from memfds, the dependency included: the loader never
    // searched the (now missing) directory for libegcldep.so.1.
    let inodes = |library: &str| {
        let mut inodes: Vec<&str> = out
            .lines()
            .filter(|line| line.starts_with("MAP ") && line.contains(library))
            .map(|line| line.split_whitespace().nth(5).unwrap())
            .collect();
        inodes.dedup();
        inodes
    };
    // Both came from memfds, the dependency included: the loader never
    // searched the (now missing) directory for libegcldep.so.1. Loading under
    // a second name reused the same object instead of mapping another copy.
    assert_eq!(inodes("/memfd:libegclmain.so.1").len(), 1, "{out}");
    assert_eq!(inodes("/memfd:libegcldep.so.1").len(), 1, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_embedded_library_altered_in_the_image_file_is_refused() {
    let dir = scratch("tamper");
    let (dep, main) = build_libraries(&dir.join("lib"));
    let app = dir.join("app");
    save_app(&dep, &main, &app);

    let mut image = std::fs::read(&app).unwrap();
    let at = image
        .windows(MARKER.len())
        .rposition(|w| w == MARKER.as_bytes())
        .expect("embedded payload in the executable");
    image[at] ^= 0x20;
    std::fs::write(&app, &image).unwrap();

    let output = egcl(&app, CALL_MAIN);
    let out = text(&output);
    assert!(!out.contains("MAIN 42"), "{out}");
    assert!(out.contains("failed its SHA-256 check"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_soname_already_loaded_from_outside_the_image_is_reported() {
    let dir = scratch("conflict");
    let (dep, _) = build_libraries(&dir.join("lib"));
    let elsewhere = dir.join("lib/copy-of-dep.so");
    std::fs::copy(&dep, &elsewhere).unwrap();
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:embed-foreign-library {dep:?})
                 (egcl-ffi:load-foreign-library {elsewhere:?})
                 (handler-case (egcl-ffi:load-foreign-library "libegcldep.so.1")
                   (error (e) (format t "REFUSED ~a~%" e))))"#
        ),
    );
    let out = text(&output);
    assert!(
        out.contains("REFUSED") && out.contains("already loaded from outside the image"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
