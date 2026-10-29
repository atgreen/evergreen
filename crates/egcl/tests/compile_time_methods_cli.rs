// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! COMPILE-FILE does not install DEFGENERIC or DEFMETHOD forms while it
//! compiles (CLHS 3.2.3.1.1 gives them no compile-time effect), as SBCL does
//! not. ASDF-FLV defines an ASDF:PERFORM :AROUND method next to the DEFVAR it
//! reads; installed at compile time, it ran for the next load operation before
//! the DEFVAR had been loaded and found it unbound (bliss-459lt).
use std::path::PathBuf;
use std::process::Command;

#[test]
fn compiling_a_file_does_not_install_its_methods() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("compile-time-methods-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("around.lisp");
    std::fs::write(
        &source,
        "(defvar *file-local* (list :bound))\n\
         (defmethod run-op :around ((x t)) (list (car *file-local*) (call-next-method)))\n\
         (defgeneric fresh-gf (x))\n\
         (defmethod fresh-gf ((x t)) :fresh)\n",
    )
    .unwrap();
    let program = format!(
        r#"(progn
             (defgeneric run-op (x))
             (defmethod run-op ((x t)) :base)
             (compile-file "{source}")
             (format t "~&COMPILED ~s~%" (list (run-op 1) (fboundp 'fresh-gf) (boundp '*file-local*)))
             (load "{fasl}")
             (format t "~&LOADED ~s~%" (list (run-op 1) (fresh-gf 1))))"#,
        source = source.display(),
        fasl = dir.join("around.fasl").display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("COMPILED (:BASE NIL NIL)"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("LOADED ((:BOUND :BASE) :FRESH)"),
        "{stdout}\n{stderr}"
    );
}
