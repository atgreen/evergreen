// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(declaim (notinline justification-native justification-formatter))
(defun justification-native (control text) (format nil control text))
(defun justification-formatter (text) (format nil (formatter "~24,,7<~A~>") text))
(let ((long (make-string 32 :initial-element #\x))
      (short (make-string 20 :initial-element #\x)))
  (assert (equal (format nil "~24,,7<~A~>" long) long))
  (assert (equal (format nil "~24,,7<~A~>" short) (concatenate 'string "    " short)))
  (assert (equal (format nil "~13,,2<~A~>" (subseq long 0 30)) (subseq long 0 30)))
  (assert (equal (format nil "~24<~A~>" long) long))
  (dotimes (i 120) (justification-native "~24,,7<~A~>" long) (justification-formatter long))
  :NATIVE-READY
  (assert (equal (justification-native "~24,,7<~A~>" long) long))
  (assert (equal (justification-formatter long) long)))
;; The same parameterized control string and text go through stream and
;; generated formatter entrypoints. Only the explicit outer slots use minpad.
(dolist (text (list "" "abc" "λλ" (make-string 32 :initial-element #\x)))
  (dolist (mincol '(0 5 24))
    (dolist (colinc '(1 3))
      (let* ((len (length text))
             (width (if (<= len mincol) mincol
                        (+ mincol (* colinc (ceiling (- len mincol) colinc)))))
             (expected (concatenate 'string (make-string (- width len) :initial-element #\*) text)))
        (dolist (minpad '(0 2 7))
          (let ((control (format nil "~~~D,~D,~D,'*<~~A~~>" mincol colinc minpad)))
            (assert (equal (justification-native control text) expected))
            (assert (equal (with-output-to-string (s)
                             (assert (null (format s control text)))) expected))
            (assert (equal (format nil "~?" control (list text)) expected))))))))
(assert (equal (format nil "~5,3,7,'*:<~A~>" "abc") "********abc"))
(assert (equal (format nil "~5,3,7,'*@<~A~>" "abc") "abc********"))
(assert (equal (format nil "~5,3,7,'*:@<~A~>" "abc") "*******abc*******"))
(assert (equal (format nil "~0,,7,'*<~A~;~A~>" "ab" "cd") "ab*******cd"))
(assert (equal (format nil "~v,v,v,v<~A~>" 5 3 7 #\* "abc") "**abc"))
(assert (equal (format nil "~v,v,v<~A~>" nil nil 7 "abc") "abc"))
(assert (equal (format nil "~?/~A" "~5,,7<~A~>" (list "abc") "after") "  abc/after"))
(assert (equal (format nil "~5,,7<abc~;discard~0^~>") "  abc"))
(let ((text (make-string 20 :initial-element #\x)))
  (assert (equal (format nil "~24,,7<BREAK~0,24:;~A~>" text)
                 (concatenate 'string "    " text)))
  (assert (equal (format nil "P:~24,,7<BREAK~0,24:;~A~>" text)
                 (concatenate 'string "P:BREAK    " text))))
(defvar *justification-effects* nil)
(assert (equal (justification-native
                (progn (push :control *justification-effects*) "~5,,7<~A~>")
                (progn (push :text *justification-effects*) (values "abc" :ignored)))
               "  abc"))
(assert (equal (reverse *justification-effects*) '(:control :text)))
(assert (= 1 (length (multiple-value-list (justification-native "~5,,7<~A~>" "abc")))))
(let ((s (make-string-output-stream)))
  (assert (equal (multiple-value-list (format s "~5,,7<~A~>" "abc")) '(nil)))
  (assert (equal (get-output-stream-string s) "  abc")))
(assert (equal (with-output-to-string (s)
                 (assert (equal (funcall (formatter "~5,,7<~A~>") s "abc" :left) '(:left))))
               "  abc"))
(write-line "FORMAT-SINGLE-JUSTIFICATION-OK")
"#;

const READY: &str = r#"
(dolist (name '(justification-native justification-formatter))
  (loop repeat 100 until (= (egcl-ext:function-tier name) *justification-required-tier*)
        do (justification-native "~24,,7<~A~>" long) (justification-formatter long) (sleep 0.01))
  (assert (= (egcl-ext:function-tier name) *justification-required-tier*)))
"#;

#[test]
fn single_segment_justification_matches_across_interpreter_native_and_streams() {
    let mut reference = None;
    for (tier, stress, required) in [
        ("interp", false, "nil"),
        ("t1", false, "1"),
        ("t2", false, "2"),
        ("t2", true, "2"),
    ] {
        let ready = if tier == "interp" { "nil" } else { READY };
        let program = format!(
            "(defparameter *justification-required-tier* {required})\n{}",
            PROGRAM.replace(":NATIVE-READY", ready)
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run justification regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("FORMAT-SINGLE-JUSTIFICATION-OK"));
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}
