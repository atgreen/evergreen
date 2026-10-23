// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A file stream opened through a logical pathname reports that pathname:
//! PATHNAME of the stream is the logical pathname OPEN was given, and
//! LOGICAL-PATHNAME accepts the stream. Both answered the translated
//! physical pathname, or a TYPE-ERROR (bliss-ssc6w; ansi LOGICAL-PATHNAME.3).
//! The file itself is still the physical translation.
use std::process::Command;

#[test]
fn a_stream_opened_through_a_logical_pathname_reports_it() {
    let dir = std::env::temp_dir().join(format!("egcl-lpstream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = format!(
        r#"(progn
(setf (logical-pathname-translations "LPSTREAM") (list (list "**;*.*.*" "{}/**/*.*")))
(with-open-file (s (logical-pathname "LPSTREAM:TEMP.DAT.NEWEST") :direction :output
                   :if-exists :supersede :if-does-not-exist :create)
  (write-line "hi" s)
  (format t "~&OUT ~s ~s ~s~%" (namestring (pathname s))
          (equal (logical-pathname s) (logical-pathname "LPSTREAM:TEMP.DAT.NEWEST"))
          (equal (truename s) (truename (translate-logical-pathname "LPSTREAM:TEMP.DAT")))))
(with-open-file (s "LPSTREAM:TEMP.DAT")
  (format t "~&IN ~s ~s ~s~%" (read-line s) (namestring (pathname s)) (file-length s)))
(with-open-file (s (translate-logical-pathname "LPSTREAM:TEMP.DAT"))
  (format t "~&PHYSICAL ~s~%" (handler-case (logical-pathname s) (type-error () :type-error)))))"#,
        dir.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("OUT \"LPSTREAM:TEMP.DAT.NEWEST\" T T"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("IN \"hi\" \"LPSTREAM:TEMP.DAT\" 3"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("PHYSICAL :TYPE-ERROR"),
        "{stdout}\n{stderr}"
    );
}
