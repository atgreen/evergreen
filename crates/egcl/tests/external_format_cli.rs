// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! OPEN's :EXTERNAL-FORMAT takes effect on file streams: :latin-1 and :ascii
//! beside :utf-8, and STREAM-EXTERNAL-FORMAT answers the designator a file
//! stream was opened with. The keyword was accepted and ignored, so a
//! Latin-1 file -- cl-pdf's font metrics, cl-typesetting's French
//! hyphenation patterns -- was decoded as UTF-8 and its first accented
//! letter was a STREAM-ERROR.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// "café\nb" in Latin-1: the é is the single octet #xE9, which is not UTF-8.
const LATIN1_BYTES: &[u8] = b"caf\xe9\nb";

const PROGRAM: &str = r#"
  (defmacro try (form)
    `(handler-case (list :ok ,form) (error (e) (list :error (type-of e)))))
  (let ((path "FILE_PATH"))
    ;; Reading as LATIN-1: the line, the char after it, the format, EOF.
    (with-open-file (in path :external-format :latin-1)
      (format t "LATIN-1 ~S ~S ~S ~S~%" (stream-external-format in)
              (read-line in) (read-char in) (read-char in nil :eof)))
    ;; The é is one char of code 233, whichever reader asks.
    (with-open-file (in path :external-format :iso-8859-1)
      (read-char in) (read-char in) (read-char in)
      (format t "CODE ~A ~S~%" (char-code (read-char in)) (peek-char nil in)))
    ;; A list designator, as cl-pdf writes for LispWorks.
    (with-open-file (in path :external-format '(:latin-1 :eol-style :lf))
      (format t "LIST ~S~%" (read-line in)))
    ;; As UTF-8 (the default) the same octets are an error; as ASCII too.
    (with-open-file (in path)
      (format t "UTF-8 ~S ~S~%" (stream-external-format in) (try (read-line in))))
    (with-open-file (in path :external-format :ascii)
      (format t "ASCII ~S ~S~%" (stream-external-format in) (try (read-line in))))
    ;; Writing LATIN-1 puts one octet per character; a character outside
    ;; the range is refused, as is a format the layer cannot decode.
    (with-open-file (out path :direction :output :if-exists :supersede
                              :external-format :latin-1)
      (write-string "café" out)
      (format t "WRITE-OUTSIDE ~S~%" (try (write-char (code-char 8364) out))))
    (with-open-file (in path :element-type '(unsigned-byte 8))
      (format t "OCTETS ~S~%" (loop for b = (read-byte in nil) while b collect b)))
    (format t "UTF-16 ~S~%" (try (open path :external-format :utf-16))))
"#;

#[test]
fn open_honors_latin_1_and_ascii_external_formats() {
    let path =
        std::env::temp_dir().join(format!("egcl-external-format-{}.txt", std::process::id()));
    std::fs::write(&path, LATIN1_BYTES).expect("write latin-1 bytes");
    let program = PROGRAM.replace("FILE_PATH", &path.display().to_string());
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "external formats failed: {stdout}\n{stderr}"
    );
    for expected in [
        "LATIN-1 :LATIN-1 \"café\" #\\b :EOF",
        "CODE 233 #\\Newline",
        "LIST \"café\"",
        "UTF-8 :UTF-8 (:ERROR STREAM-ERROR)",
        "ASCII :ASCII (:ERROR STREAM-ERROR)",
        "WRITE-OUTSIDE (:ERROR STREAM-ERROR)",
        "OCTETS (99 97 102 233)",
        "UTF-16 (:ERROR STREAM-ERROR)",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in: {stdout}"
        );
    }
}
