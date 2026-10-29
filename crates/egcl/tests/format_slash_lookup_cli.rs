// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r##"
(defpackage :slash-private (:use :cl) (:nicknames :slash-alias))
(defpackage :slash-inherited (:use :cl :slash-private))
(defpackage :slash-imported (:use :cl))
(defvar *slash-last-arguments* nil)
(defun slash-callback (stream object colon-p at-sign-p &rest params)
  (setf *slash-last-arguments* (list colon-p at-sign-p params))
  (format stream "<~A>" object)
  (values :ignored :also-ignored))
(dolist (name '("PRIVATE-CB" "WITH SPACE" "WITH:COLON" "|WITH BARS|" "WITH\\BACKSLASH"))
  (setf (fdefinition (intern name :slash-private)) #'slash-callback))
(assert (eq (nth-value 1 (find-symbol "PRIVATE-CB" :slash-private)) :internal))
(dolist (control '("~/slash-private:private-cb/" "~/slash-private::private-cb/"
                   "~/SLASH-PRIVATE:PRIVATE-CB/" "~/slash-alias:private-cb/"
                   "~/slash-callback/" "~/slash-private:with space/"
                   "~/slash-private:with:colon/" "~/slash-private:|with bars|/"
                   "~/slash-private:with\\backslash/"))
  (assert (equal (format nil control 42) "<42>")))
;; FORMATTER uses the same lookup, without requiring an exported function.
(assert (equal (format nil (formatter "~/slash-private:private-cb/") 42) "<42>"))
(assert (equal (format nil (formatter "~/slash-private::private-cb/") 42) "<42>"))
(assert (equal (format nil "~v,'q:@/slash-private:private-cb/" 9 42) "<42>"))
(assert (equal *slash-last-arguments* '(t t (9 #\q))))
;; A bare directive always uses CL-USER, independently of dynamic *PACKAGE*.
(let ((*package* (find-package :slash-private)))
  (assert (equal (format nil "~/slash-callback/" 42) "<42>")))
;; Lookup includes accessible imported/inherited symbols, preserving identity.
(let ((symbol (intern "PRIVATE-CB" :slash-private)))
  (import symbol :slash-imported)
  (export symbol :slash-private))
(assert (equal (format nil "~/slash-imported:private-cb/" 42) "<42>"))
(assert (equal (format nil "~/slash-inherited:private-cb/" 42) "<42>"))
;; Name interpretation is literal: |...| does not turn into reader escaping.
(setf (fdefinition (intern "WITH BARS" :slash-private))
      (lambda (&rest ignored) (declare (ignore ignored)) (error "wrong literal name")))
(assert (equal (format nil "~/slash-private:|with bars|/" 42) "<42>"))
(intern "UNBOUND-CB" :slash-private)
(assert (handler-case (progn (format nil "~/slash-private:unbound-cb/" 42) nil)
          (undefined-function () t)))
(write-line "FORMAT-SLASH-LOOKUP-OK")
"##;

#[test]
fn slash_function_names_use_package_lookup_without_reader_access_rules() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run FORMAT slash lookup regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("FORMAT-SLASH-LOOKUP-OK"),
        "{stdout}\n{stderr}"
    );
}
