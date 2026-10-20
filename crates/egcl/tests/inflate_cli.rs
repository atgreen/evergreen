// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// EGCL-EXT:INFLATE on data compressed by Python's gzip and zlib modules: each
/// format, a gzip member that records a file name, concatenated members, and
/// the errors for truncated data, a damaged CRC, the wrong wrapper and an
/// unknown format.
#[test]
fn inflate_decompresses_and_checks_every_format() {
    let program = r#"
      (let ((text (map 'vector #'char-code "hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, hello egcl, ")))
        (flet ((same (octets) (equalp octets text))
               (string-of (octets) (map 'string #'code-char octets))
               (fails (octets format)
                 (handler-case (progn (egcl-ext:inflate octets format) :no-error)
                   (stream-error () :stream-error)
                   (type-error () :type-error))))
          (format t "RESULT:~S~%"
                  (list (fboundp 'egcl-ext:inflate)
                        (same (egcl-ext:inflate #(31 139 8 0 0 0 0 0 2 255 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 43 46 196 94 240 0 0 0) :gzip))
                        (same (egcl-ext:inflate #(120 156 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 220 82 82 29) :zlib))
                        (same (egcl-ext:inflate #(203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0)))
                        (same (egcl-ext:inflate #(203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0) :deflate))
                        (string-of (egcl-ext:inflate #(31 139 8 8 0 0 0 0 2 255 110 111 116 101 115 46 116 120 116 0 203 75 204 77 77 1 0 135 204 224 113 5 0 0 0) :gzip))
                        (string-of (egcl-ext:inflate #(31 139 8 0 0 0 0 0 2 255 75 76 2 0 109 72 131 158 2 0 0 0 31 139 8 0 0 0 0 0 2 255 75 78 1 0 218 143 214 69 2 0 0 0) :gzip))
                        (fails #(31 139 8 0 0 0 0 0 2 255 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 43 46 196) :gzip)
                        (fails #(31 139 8 0 0 0 0 0 2 255 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 43 46 197 94 240 0 0 0) :gzip)
                        (fails #(31 139 8 0 0 0 0 0 2 255 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 43 46 196 94 240 0 0 0) :zlib)
                        (fails #(31 139 8 0 0 0 0 0 2 255 203 72 205 201 201 87 72 77 79 206 209 81 200 24 1 108 0 43 46 196 94 240 0 0 0) :bzip2)))))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(
            r#"RESULT:(T T T T T "named" "abcd" :STREAM-ERROR :STREAM-ERROR :STREAM-ERROR :TYPE-ERROR)"#
        ),
        "{stdout}"
    );
}
