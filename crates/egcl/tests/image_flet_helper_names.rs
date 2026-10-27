// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A non-capturing FLET compiles to a call to a global helper named
//! `__FLET___<n>`, where `n` comes from a per-process counter, and the name
//! resolves through the global symbol registry. A process restored from an
//! image restarted that counter at 0, so it re-minted helper names the image
//! already used and REDEFINED those saved local functions. In the `make image`
//! egcl, loading the ocicl runtime replaced PARSE-INTEGER's whitespace test
//! (`__FLET___480`), after which `(parse-integer "1")` signalled "not an
//! integer" and ASDF could not parse a version string.
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Keep a named FLET helper: this test exercises image helper-name allocation,
/// so expanding the small helper inline would remove the behavior under test.
fn flet_definition(package: &str, name: &str) -> String {
    format!(
        "(defpackage #:{package} (:use #:cl))\n(in-package #:{package})\n\
         (defun {name} (s)\n\
           (flet ((ws-p (c) (member c '(#\\Space))))\n\
             (declare (notinline ws-p))\n\
             (loop for c across s count (ws-p c))))\n"
    )
}

/// The helper number in `__FLET___<n>` from a DISASSEMBLE listing.
fn helper_number(listing: &str) -> u64 {
    let start = listing
        .find("__FLET___")
        .unwrap_or_else(|| panic!("FLET compiled to a named helper:\n{listing}"))
        + 9;
    listing[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

fn egcl(args: &[&str], image: Option<&PathBuf>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    let output = command.args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn restored_image_never_reuses_a_saved_flet_helper_name() {
    let dir = std::env::temp_dir().join(format!("egcl-flet-helpers-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let core = dir.join("flet.core");
    let saved_source = dir.join("saved.lisp");
    let fresh_source = dir.join("fresh.lisp");
    fs::write(&saved_source, flet_definition("SAVED-FLET", "COUNT-SPACES")).unwrap();
    fs::write(&fresh_source, flet_definition("FRESH-FLET", "COUNT-SPACES")).unwrap();

    let saved_listing = egcl(
        &[
            "--eval",
            &format!("(load {saved_source:?})"),
            "--eval",
            "(disassemble 'saved-flet::count-spaces)",
            "--eval",
            &format!("(egcl-ext:save-lisp-and-die {core:?})"),
        ],
        None,
    );
    let saved = helper_number(&saved_listing);

    let restored = egcl(
        &[
            "--eval",
            &format!("(load {fresh_source:?})"),
            "--eval",
            "(disassemble 'fresh-flet::count-spaces)",
            "--eval",
            "(format t \"~&SAVED-RESULT ~D~%\" (saved-flet::count-spaces \"a b c\"))",
        ],
        Some(&core),
    );
    let fresh = helper_number(&restored);
    assert!(
        fresh > saved,
        "the restored process minted __FLET___{fresh}, at or below the image's \
         __FLET___{saved}, so it can redefine a saved local function"
    );
    assert!(restored.contains("SAVED-RESULT 2"), "{restored}");
}
