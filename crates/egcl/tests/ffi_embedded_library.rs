// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared libraries embedded in a saved executable (bliss-rhsy4.1/.2/.9). On
//! glibc the `c-ffi` backend loads them through a sealed memfd: a saved
//! executable runs its embedded libraries after the original files are gone,
//! an embedded DT_NEEDED dependency is found by SONAME, a payload altered in
//! the file is refused, and a SONAME already loaded from elsewhere is reported
//! rather than silently shadowed. The executable is also a zip archive, so
//! Info-ZIP can replace, remove and add libraries in it.
#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The memfd loader needs the dlopen backend; a gnu build without
/// `egcl-rt/c-ffi` uses the static ELF loader, which refuses embedded loads.
fn dlopen_backend() -> bool {
    let present = egcl_rt::image::RUNTIME_BUILD_FEATURES.contains(&"egcl-rt/c-ffi");
    if !present {
        eprintln!("skipping: built without egcl-rt/c-ffi");
    }
    present
}

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

fn cc(dir: &Path, args: &[&str]) {
    let status = Command::new("cc")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run cc");
    assert!(status.success(), "cc {args:?} failed");
}

/// `dir/libegcldep.so.1`, whose `dep_value` returns `value`.
fn build_dep(dir: &Path, value: i32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("dep.c"),
        format!("const char *dep_marker(void) {{ return \"{MARKER}\"; }}\nint dep_value(void) {{ return {value}; }}\n"),
    )
    .unwrap();
    cc(
        dir,
        &[
            "-shared",
            "-fPIC",
            "-Wl,-soname,libegcldep.so.1",
            "dep.c",
            "-o",
            "libegcldep.so.1",
        ],
    );
    dir.join("libegcldep.so.1")
}

/// libegcldep.so.1, and libegclmain.so.1 which needs it.
fn build_libraries(dir: &Path) -> (PathBuf, PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
    build_dep(dir, 41);
    std::fs::write(
        dir.join("main.c"),
        "int dep_value(void);\nint main_value(void) { return dep_value() + 1; }\n",
    )
    .unwrap();
    let cc = |args: &[&str]| cc(dir, args);
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
    if !dlopen_backend() {
        return;
    }
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
    if !dlopen_backend() {
        return;
    }
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
    assert!(out.contains("failed its CRC-32 check"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_soname_already_loaded_from_outside_the_image_is_reported() {
    if !dlopen_backend() {
        return;
    }
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

/// Run Info-ZIP's `zip` on `app` from `dir`. It insists on a `.zip` name, so
/// the executable is renamed for the duration. `None` when `zip` is absent.
fn zip_tool(dir: &Path, app: &Path, args: &[&str]) -> Option<()> {
    let archive = app.with_extension("zip");
    std::fs::rename(app, &archive).unwrap();
    let status = Command::new("zip")
        .current_dir(dir)
        .arg("-q")
        .args(args.iter().map(|a| {
            if *a == "{}" {
                archive.as_os_str()
            } else {
                a.as_ref()
            }
        }))
        .status();
    std::fs::rename(&archive, app).unwrap();
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        status => {
            assert!(status.unwrap().success(), "zip {args:?} failed");
            Some(())
        }
    }
}

fn unzip_listing(app: &Path) -> Option<String> {
    let output = Command::new("unzip").arg("-v").arg(app).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn zip_tools_replace_remove_and_add_embedded_libraries() {
    if !dlopen_backend() {
        return;
    }
    let dir = scratch("zip");
    let (dep, main) = build_libraries(&dir.join("lib"));
    let app = dir.join("app");
    save_app(&dep, &main, &app);
    std::fs::remove_dir_all(dir.join("lib")).unwrap();

    // Replace libegcldep.so.1 with a build returning 98. Info-ZIP deflates it,
    // so the runtime inflates it on the way into the memfd.
    let stage = dir.join("stage");
    build_dep(&stage.join("lib"), 98);
    if zip_tool(&stage, &app, &["{}", "lib/libegcldep.so.1"]).is_none() {
        eprintln!("skipping: Info-ZIP `zip` is not installed");
        return;
    }
    if let Some(listing) = unzip_listing(&app) {
        let line = listing
            .lines()
            .find(|l| l.ends_with("lib/libegcldep.so.1"))
            .unwrap_or_else(|| panic!("{listing}"));
        assert!(line.contains("Defl"), "{listing}");
        assert!(listing.contains("egcl/core"), "{listing}");
    }
    let output = egcl(&app, CALL_MAIN);
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(out.contains("MAIN 99"), "{out}");
    // The manifest still gives the replacement its recorded names.
    assert!(out.contains("ALIAS 99"), "{out}");

    // Remove libegclmain.so.1 and add a library the save never knew about.
    std::fs::write(
        stage.join("extra.c"),
        "int extra_value(void) { return 7; }\n",
    )
    .unwrap();
    cc(
        &stage,
        &[
            "-shared",
            "-fPIC",
            "-Wl,-soname,libegclextra.so.1",
            "extra.c",
            "-o",
            "lib/libegclextra.so.1",
        ],
    );
    zip_tool(&stage, &app, &["-d", "{}", "lib/libegclmain.so.1"]).unwrap();
    zip_tool(&stage, &app, &["{}", "lib/libegclextra.so.1"]).unwrap();
    let output = egcl(
        &app,
        r#"(progn
             (format t "EMBEDDED ~s~%" (egcl-ffi:embedded-foreign-libraries))
             (format t "EXTRA ~d~%" (egcl-ffi:foreign-call
                                      (egcl-ffi:foreign-symbol-pointer
                                        "extra_value" (egcl-ffi:load-foreign-library "libegclextra.so.1"))
                                      :int '() '()))
             (format t "MAIN-EMBEDDED ~s~%" (egcl-ffi:embedded-foreign-library-p "libegclmain.so.1")))"#,
    );
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(
        out.contains(r#"EMBEDDED ("libegcldep.so.1" "libegclextra.so.1")"#),
        "{out}"
    );
    assert!(out.contains("EXTRA 7"), "{out}");
    assert!(out.contains("MAIN-EMBEDDED NIL"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_core_file_carrying_libraries_is_an_archive_that_image_loads() {
    if !dlopen_backend() {
        return;
    }
    let dir = scratch("core");
    let (dep, main) = build_libraries(&dir.join("lib"));
    let core = dir.join("app.core");
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:embed-foreign-library {dep:?})
                 (egcl-ffi:embed-foreign-library {main:?} :names '("egclmain"))
                 (egcl-ext:save-lisp-and-die {core:?}))"#
        ),
    );
    assert!(output.status.success(), "{}", text(&output));
    std::fs::remove_dir_all(dir.join("lib")).unwrap();
    assert_eq!(&std::fs::read(&core).unwrap()[..4], b"PK\x03\x04");

    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env(
            "EGCL_INIT_FILE",
            std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
        )
        .arg("--image")
        .arg(&core)
        .args(["--no-init", "--eval", CALL_MAIN])
        .output()
        .expect("run egcl");
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(out.contains("MAIN 42") && out.contains("ALIAS 42"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_replaced_entry_keeps_its_memfd_so_the_next_library_is_not_mistaken_for_it() {
    if !dlopen_backend() {
        return;
    }
    let dir = scratch("reuse");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.c"), "int x_value(void) { return 1; }\n").unwrap();
    std::fs::write(dir.join("y.c"), "int y_value(void) { return 2; }\n").unwrap();
    cc(&dir, &["-shared", "-fPIC", "x.c", "-o", "libx.so"]);
    cc(
        &dir,
        &[
            "-shared",
            "-fPIC",
            "-Wl,-soname,liby.so.1",
            "y.c",
            "-o",
            "liby.so",
        ],
    );
    let (x, y) = (dir.join("libx.so"), dir.join("liby.so"));
    // Loading x materialises a memfd; embedding x again replaces its entry.
    // y's memfd must not get x's descriptor number, or glibc hands back x.
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:embed-foreign-library {x:?})
                 (egcl-ffi:load-foreign-library {x:?})
                 (egcl-ffi:embed-foreign-library {x:?})
                 (egcl-ffi:embed-foreign-library {y:?})
                 (let ((y (egcl-ffi:load-foreign-library "liby.so.1")))
                   (format t "Y ~d~%" (egcl-ffi:foreign-call
                                        (egcl-ffi:foreign-symbol-pointer "y_value" y)
                                        :int '() '()))))"#
        ),
    );
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(out.contains("Y 2"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_library_loaded_from_disk_and_then_embedded_still_loads() {
    if !dlopen_backend() {
        return;
    }
    let dir = scratch("preloaded");
    let (dep, _) = build_libraries(&dir.join("lib"));
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:load-foreign-library {dep:?})
                 (egcl-ffi:embed-foreign-library {dep:?})
                 (dolist (name (list {dep:?} "libegcldep.so.1"))
                   (format t "DEP ~d~%" (egcl-ffi:foreign-call
                                          (egcl-ffi:foreign-symbol-pointer
                                            "dep_value" (egcl-ffi:load-foreign-library name))
                                          :int '() '()))))"#
        ),
    );
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert_eq!(out.matches("DEP 41").count(), 2, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A handle to an embedded library, saved in a core archive, is reopened from
/// the archive at restore (GitHub #440): the embedded set is installed before
/// the core reopens its libraries.
#[test]
fn a_saved_handle_to_an_embedded_library_is_reopened_from_the_archive() {
    if !dlopen_backend() {
        return;
    }
    let dir = scratch("handle");
    let (dep, main) = build_libraries(&dir.join("lib"));
    let core = dir.join("app.core");
    let output = egcl(
        Path::new(env!("CARGO_BIN_EXE_egcl")),
        &format!(
            r#"(progn
                 (egcl-ffi:embed-foreign-library {dep:?})
                 (egcl-ffi:embed-foreign-library {main:?} :names '("egclmain"))
                 (defvar *main* (egcl-ffi:load-foreign-library "egclmain"))
                 (egcl-ext:save-lisp-and-die {core:?}))"#
        ),
    );
    assert!(output.status.success(), "{}", text(&output));
    std::fs::remove_dir_all(dir.join("lib")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env(
            "EGCL_INIT_FILE",
            std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
        )
        .arg("--image")
        .arg(&core)
        .args([
            "--no-init",
            "--eval",
            r#"(format t "SAVED ~d~%" (egcl-ffi:foreign-call
                                        (egcl-ffi:foreign-symbol-pointer "main_value" *main*)
                                        :int '() '()))"#,
        ])
        .output()
        .expect("run egcl");
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(out.contains("SAVED 42"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
