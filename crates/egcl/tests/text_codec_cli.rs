// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn text_codecs_cover_unicode_slices_termination_and_invalid_bytes() {
    let source = r#"
      (assert (eq :external (nth-value 1 (find-symbol "STRING-TO-OCTETS" :egcl-ext))))
      (assert (eq :external (nth-value 1 (find-symbol "OCTETS-TO-STRING" :egcl-ext))))
      (let* ((text (coerce (list #\A (code-char #xe9) (code-char #x1f600) #\Z) 'string))
             (bytes (egcl-ext:string-to-octets text)))
        (assert (typep bytes '(simple-array (unsigned-byte 8) (*))))
        (assert (equalp bytes #(65 195 169 240 159 152 128 90)))
        (assert (string= text (egcl-ext:octets-to-string bytes)))
        (assert (equalp #(195 169 240 159 152 128 0)
                       (egcl-ext:string-to-octets text :start 1 :end 3 :null-terminate t)))
        (assert (string= (subseq text 1 3)
                        (egcl-ext:octets-to-string bytes :start 1 :end 7))))
      (assert (equalp #(65 0) (egcl-ext:string-to-octets "A" :external-format :ascii :null-terminate t)))
      (assert (string= "ABC" (egcl-ext:octets-to-string #(65 66 67) :external-format :ascii)))
      (assert (= #xfffd (char-code (char (egcl-ext:octets-to-string #(255)) 0))))
      (assert (= #xfffd (char-code (char (egcl-ext:octets-to-string #(255) :external-format :ascii) 0))))
      (assert (equalp #() (egcl-ext:string-to-octets "")))
      (assert (string= "" (egcl-ext:octets-to-string #())))
      (assert (handler-case
                  (progn (egcl-ext:string-to-octets (string (code-char #xe9)) :external-format :ascii) nil)
                (error () t)))
      (assert (handler-case
                  (progn (egcl-ext:octets-to-string #(256)) nil)
                (type-error () t)))
      (assert (handler-case
                  (progn (egcl-ext:string-to-octets "A" :external-format :unknown) nil)
                (error () t)))
      (let* ((base (egcl-ext:string-to-octets "xABCy"))
             (slice (make-array 3 :element-type '(unsigned-byte 8) :displaced-to base :displaced-index-offset 1)))
        (assert (string= "ABC" (egcl-ext:octets-to-string slice))))
      (format t "TEXT-CODEC-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("TEXT-CODEC-OK"));
}
