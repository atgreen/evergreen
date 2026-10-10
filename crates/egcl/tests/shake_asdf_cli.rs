// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shaking described in a system definition: `asdf:make` on a system of class
//! `egcl-shake-asdf:shaken-application` writes the specification from the
//! system's slots, saves a core in a child process, runs shake on it, and
//! leaves a standalone executable under build/. This drives the whole route
//! from a fixture project, the way an application author would.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct Project {
    directory: PathBuf,
}

impl Project {
    fn new(name: &str, asd: &str, source: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("egcl-shake-asdf-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("fixture.asd"), asd).unwrap();
        fs::write(directory.join("fixture.lisp"), source).unwrap();
        Self { directory }
    }

    /// Run egcl in the project with the registry pointing at the integration
    /// in this checkout and at the project itself, as a user's
    /// CL_SOURCE_REGISTRY would.
    fn egcl(&self, forms: &[&str]) -> std::process::Output {
        let registry = format!(
            "(:source-registry (:directory {:?}) (:directory {:?}) :ignore-inherited-configuration)",
            repo_root().join("lib/egcl-shake/").to_str().unwrap(),
            self.directory.to_str().unwrap()
        );
        let mut command = Command::new(BIN);
        command
            .current_dir(&self.directory)
            .env("CL_SOURCE_REGISTRY", registry)
            .env_remove("EGCL_BACKEND")
            .arg("--no-init");
        for form in forms {
            command.arg("--eval").arg(form);
        }
        command.output().unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const SOURCE: &str = r#"
(defpackage :fixture (:use :cl))
(in-package :fixture)
(defun greeting () "Hello from a shaken system!")
(defun unused-helper () "Development only")
(defun main () (format t "~A~%" (greeting)))
"#;

#[test]
fn asdf_make_shakes_an_executable_from_the_system_definition() {
    let project = Project::new(
        "make",
        r#"
(asdf:defsystem "fixture" :components ((:file "fixture")))
(asdf:defsystem "fixture/shake"
  :defsystem-depends-on ("egcl-shake-asdf")
  :class "egcl-shake-asdf:shaken-application"
  :build-operation "egcl-shake-asdf:shake-op"
  :depends-on ("fixture")
  :shake-entry "fixture::main"
  :shake-prune-packages ("FIXTURE")
  :shake-dynamic :explicit)
"#,
        SOURCE,
    );
    let output = project.egcl(&[
        "(require :asdf)",
        "(asdf:load-asd (truename \"fixture.asd\"))",
        "(asdf:make \"fixture/shake\")",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    let build = project.directory.join("build");
    let exe = build.join("fixture");
    assert!(exe.is_file(), "no executable:\n{}", text(&output));
    // The specification is generated from the slots, and left for inspection.
    let spec = fs::read_to_string(build.join("fixture.shake")).unwrap();
    for line in [
        "version = 1",
        "entry = FIXTURE::MAIN",
        "prune-package = FIXTURE",
        "dynamic = explicit",
        "runtime = full",
        "max-tier = t2",
    ] {
        assert!(
            spec.lines().any(|l| l == line),
            "missing {line:?} in:\n{spec}"
        );
    }
    // The shaken program runs its entry point without the runtime that built it.
    let run = Command::new(&exe).output().unwrap();
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Hello from a shaken system!\n"
    );
    // And the tree shaker actually ran: the report names the pruned helper.
    let manifest = fs::read_to_string(build.join("fixture.manifest")).unwrap();
    assert!(
        manifest.contains("UNUSED-HELPER"),
        "manifest does not mention the pruned helper:\n{manifest}"
    );
}

#[test]
fn the_specification_is_generated_from_the_slots_without_building() {
    let project = Project::new(
        "spec",
        r#"
(asdf:defsystem "fixture" :components ((:file "fixture")))
(asdf:defsystem "fixture/shake"
  :defsystem-depends-on ("egcl-shake-asdf")
  :class "egcl-shake-asdf:shaken-application"
  :build-operation "egcl-shake-asdf:shake-op"
  :depends-on ("fixture")
  :entry-point "fixture:main"
  :shake-prune-packages (:fixture "*")
  :shake-keep ("fixture::greeting" "fixture::unused-helper")
  :shake-dynamic :explicit
  :shake-runtime :specialized
  :shake-max-tier :t1
  :shake-runtime-keep (:disassembly))
"#,
        SOURCE,
    );
    let output = project.egcl(&[
        "(require :asdf)",
        "(asdf:load-asd (truename \"fixture.asd\"))",
        "(asdf:load-system \"egcl-shake-asdf\")",
        "(princ (egcl-shake-asdf:shake-spec (asdf:find-system \"fixture/shake\")))",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    let spec = String::from_utf8_lossy(&output.stdout);
    // ASDF's own :entry-point stands in for :shake-entry; keeps and packages
    // are spelled the way the specification wants whatever the .asd used. (A
    // .asd is read before the application's packages exist, so keeps are
    // strings there; a symbol works for a package that already exists.)
    for line in [
        "entry = FIXTURE::MAIN",
        "prune-package = FIXTURE",
        "prune-package = *",
        "keep = FIXTURE::GREETING",
        "keep = FIXTURE::UNUSED-HELPER",
        "dynamic = explicit",
        "runtime = specialized",
        "max-tier = t1",
        "runtime-keep = disassembly",
    ] {
        assert!(
            spec.lines().any(|l| l == line),
            "missing {line:?} in:\n{spec}"
        );
    }
}

#[test]
fn a_misspelled_choice_is_rejected_before_anything_is_built() {
    let project = Project::new(
        "reject",
        r#"
(asdf:defsystem "fixture" :components ((:file "fixture")))
(asdf:defsystem "fixture/shake"
  :defsystem-depends-on ("egcl-shake-asdf")
  :class "egcl-shake-asdf:shaken-application"
  :build-operation "egcl-shake-asdf:shake-op"
  :depends-on ("fixture")
  :shake-entry "fixture::main"
  :shake-dynamic :explicitly)
"#,
        SOURCE,
    );
    let output = project.egcl(&[
        "(require :asdf)",
        "(asdf:load-asd (truename \"fixture.asd\"))",
        "(asdf:make \"fixture/shake\")",
    ]);
    assert!(!output.status.success());
    let text = text(&output);
    assert!(text.contains(":shake-dynamic"), "{text}");
    // ASDF creates the output directory before PERFORM runs; what matters is
    // that no specification, core or executable was written into it.
    let build = project.directory.join("build");
    let written: Vec<_> = fs::read_dir(&build)
        .map(|entries| entries.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(written.is_empty(), "{written:?}\n{text}");
}
