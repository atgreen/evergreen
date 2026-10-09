// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Build-host files carried in the image as ASDF components (bliss-ceyqq):
//! `(:embedded-file ...)` and `(:embedded-tree ...)` in a system's components
//! embed when the system loads, and the saved image finds them on a host that
//! has no such files.

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
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("egcl-embed-asdf-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        Self { directory }
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.directory.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn path(&self, relative: &str) -> String {
        self.directory.join(relative).to_string_lossy().into_owned()
    }

    fn egcl(&self, args: &[&str]) -> (bool, String) {
        let registry = format!(
            "(:source-registry (:directory {:?}) (:directory {:?}) :ignore-inherited-configuration)",
            repo_root().join("lib/egcl-embed/").to_str().unwrap(),
            self.directory.to_str().unwrap()
        );
        let output = Command::new("timeout")
            .args(["--kill-after=5", "300", BIN, "--no-init"])
            .args(args)
            .current_dir(&self.directory)
            .env("CL_SOURCE_REGISTRY", registry)
            .env_remove("EGCL_BACKEND")
            .output()
            .unwrap();
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// A run-time directory no host has.
const ZONE: &str = "/nowhere/egcl-embed-asdf/zoneinfo";

#[test]
fn embedded_components_carry_build_host_files_into_the_saved_image() {
    let project = Project::new("tree");
    // A zoneinfo-shaped tree: two regions and a stray file :only leaves out.
    project.write("zoneinfo/UTC", b"TZif-UTC");
    project.write("zoneinfo/America/New_York", b"TZif-NY");
    project.write("zoneinfo/Europe/Paris", b"TZif-Paris");
    project.write("zoneinfo/Asia/Tokyo", b"TZif-Tokyo");
    project.write("zoneinfo/leapseconds.txt", b"not a zone");
    project.write("deploy/config.sexp", b"(:greeting \"hello from config\")");
    project.write(
        "fixture.lisp",
        b"(defpackage :fixture (:use :cl) (:export :report))
          (in-package :fixture)
          (defun report ()
            (format t \"REPORT ~S~%\"
                    (list (with-open-file (s \"/nowhere/egcl-embed-asdf/zoneinfo/UTC\") (read-line s))
                          (with-open-file (s \"/nowhere/egcl-embed-asdf/zoneinfo/Europe/Paris\") (read-line s))
                          (probe-file \"/nowhere/egcl-embed-asdf/zoneinfo/Asia/Tokyo\")
                          (probe-file \"/nowhere/egcl-embed-asdf/zoneinfo/leapseconds.txt\")
                          (sort (mapcar #'namestring (directory \"/nowhere/egcl-embed-asdf/zoneinfo/**/*.*\")) #'string<)
                          (with-open-file (s \"/etc/nowhere-egcl-embed/config.sexp\") (read s)))))",
    );
    project.write(
        "fixture.asd",
        format!(
            r#"
(asdf:defsystem "fixture"
  :defsystem-depends-on ("egcl-embed-asdf")
  :components
  ((:embedded-tree "zoneinfo"
     :source "zoneinfo/"
     :path "{ZONE}/"
     :only ("UTC" "America/*" "Europe/*"))
   ("egcl-embed-asdf:embedded-file" "config"
     :source "deploy/config.sexp"
     :path "/etc/nowhere-egcl-embed/config.sexp")
   (:file "fixture")))
"#
        )
        .as_bytes(),
    );
    let core = project.path("app.core");
    let (success, text) = project.egcl(&[
        "--eval",
        "(require :asdf)",
        "--eval",
        "(asdf:load-asd (truename \"fixture.asd\"))",
        "--eval",
        "(asdf:load-system \"fixture\")",
        "--eval",
        "(fixture:report)",
        "--eval",
        &format!("(egcl-ext:save-lisp-and-die {core:?})"),
    ]);
    assert!(success, "{text}");
    let expected = format!(
        "REPORT (\"TZif-UTC\" \"TZif-Paris\" NIL NIL (\"{ZONE}/America/New_York\" \"{ZONE}/Europe/Paris\" \"{ZONE}/UTC\") (:GREETING \"hello from config\"))"
    );
    assert!(text.contains(&expected), "missing {expected:?} in:\n{text}");

    // The build host's files are gone; the restored image still has them.
    fs::remove_dir_all(project.directory.join("zoneinfo")).unwrap();
    fs::remove_dir_all(project.directory.join("deploy")).unwrap();
    let (success, text) = project.egcl(&["--image", &core, "--eval", "(fixture:report)"]);
    assert!(success, "{text}");
    assert!(
        text.contains(&expected),
        "missing {expected:?} after restore in:\n{text}"
    );
}

#[test]
fn a_missing_source_fails_the_load_instead_of_shipping_without_it() {
    let project = Project::new("missing");
    project.write("fixture.lisp", b"(defpackage :fixture (:use :cl))");
    project.write(
        "fixture.asd",
        format!(
            r#"
(asdf:defsystem "fixture"
  :defsystem-depends-on ("egcl-embed-asdf")
  :components
  ((:embedded-file "absent" :source "absent.bin" :path "{ZONE}/absent")
   (:file "fixture")))
"#
        )
        .as_bytes(),
    );
    let (success, text) = project.egcl(&[
        "--eval",
        "(require :asdf)",
        "--eval",
        "(asdf:load-asd (truename \"fixture.asd\"))",
        "--eval",
        "(asdf:load-system \"fixture\")",
        "--eval",
        "(print (egcl-ext:embedded-files))",
    ]);
    assert!(!success, "{text}");
    assert!(text.contains("absent.bin"), "{text}");
}
