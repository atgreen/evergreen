// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline logical-fill-render logical-fill-formatter))
(defun logical-fill-render (control &rest arguments)
  (apply #'format nil control arguments))
(defun logical-fill-formatter (&rest arguments)
  (with-output-to-string (stream)
    (apply (formatter "~:@<~@{~A~^ ~}~:@>") stream arguments)))
(dotimes (i 80)
  (logical-fill-render "~:@<~@{~A~^ ~}~:@>" 1 2 3)
  (logical-fill-formatter 1 2 3))
(when *fill-required-tier*
  (dolist (name '(logical-fill-render logical-fill-formatter))
    (loop repeat 100 until (= *fill-required-tier* (egcl-ext:function-tier name))
          do (funcall name "x") (sleep 0.01))
    (assert (= *fill-required-tier* (egcl-ext:function-tier name)))))
(let ((*print-pretty* t) (*print-readably* nil) (*print-escape* nil)
      (*print-miser-width* nil))
  (let ((*print-right-margin* 2))
    (let ((expected (format nil "(1~% 2~% 3)")))
      (assert (equal expected (logical-fill-render "~:@<~@{~A~^ ~}~:@>" 1 2 3)))
      (assert (equal expected (logical-fill-formatter 1 2 3)))
      (assert (equal expected (logical-fill-render "~:@<~@{~A~^            ~}~:@>" 1 2 3)))
      (assert (equal expected (logical-fill-render "~:<~{~A~^ ~}~:@>" '((1 2 3))))))
    ;; A closing colon alone adds no automatic break; argument text is opaque.
    (assert (equal "(1 2 3)" (logical-fill-render "~:@<~@{~A~^ ~}~:>" 1 2 3)))
    (assert (equal "a b" (logical-fill-render "~@<~A~:@>" "a b")))
    (assert (equal (logical-fill-render "~@<~@<a b~:>~:@>")
                   (logical-fill-render "~@<~@<a ~:_b~:>~:>")))
    (assert (equal "a b" (logical-fill-render "~@<~?~:@>" "a b" nil)))
    (assert (equal "a b" (logical-fill-render "~@<~{~}~:@>" "a b" '(nil)))))
  (let ((*print-right-margin* 10))
    (let ((expected (format nil "(1 2 3 4~% 5 6 7 8~% 9 10)")))
      (assert (equal expected (logical-fill-render "~:@<~@{~A~^ ~}~:@>" 1 2 3 4 5 6 7 8 9 10)))
      (assert (equal expected (with-output-to-string (s) (pprint-fill s '(1 2 3 4 5 6 7 8 9 10)))))
      (assert (equal expected (logical-fill-render "~:@<~@{~A~^ ~:_~}~:>" 1 2 3 4 5 6 7 8 9 10)))))
  (let ((*print-right-margin* 3))
    (assert (equal (format nil "**1~%**2~%**3")
                   (logical-fill-render "~@<**~@;~@{~A~^       ~}~:@>" 1 2 3)))
    (assert (equal (format nil "**1~%**2~%**3XX")
                   (logical-fill-render "~@<**~@;~@{~A~^       ~}~;XX~:@>" 1 2 3)))
    (assert (equal (format nil "**1~%**2~%**3)")
                   (logical-fill-render "~:@<**~@;~@{~A~^       ~}~:@>" 1 2 3))))
  ;; Compare literal conditional/case bodies with the corresponding explicit
  ;; fill events. Spaces in ~' parameters are directive data, not body blanks.
  (let ((*print-right-margin* 4))
    (assert (equal (logical-fill-render "~@<~:[aa bb~;cc dd~]~:@>" nil)
                   (logical-fill-render "~@<~:[aa ~:_bb~;cc ~:_dd~]~:>" nil)))
    (assert (equal (logical-fill-render "~@<~(AA BB~)~:@>")
                   (logical-fill-render "~@<~(AA ~:_BB~)~:>")))
    (assert (equal "a   " (logical-fill-render "~@<~4,1,0,' A~:@>" "a"))))
  ;; The newline directive's retained indentation is exempt from automatic fill.
  ;; Ordinary literal blanks later in the body still have fill semantics.
  (dolist (newline-control '("~" "~:" "~@"))
    (let* ((*print-right-margin* 2)
           (automatic (concatenate 'string "~@<a" newline-control (string #\Newline) "   b c~:@>"))
           (explicit (concatenate 'string "~@<a" newline-control (string #\Newline) "   b ~:_c~:>")))
      (assert (equal (logical-fill-render automatic) (logical-fill-render explicit)))))
  ;; Fresh-line and explicit linear/miser/mandatory directives retain their
  ;; own policies; automatic fill adds only the literal separator's event.
  (let ((*print-right-margin* 4))
    (dolist (directive '("~&" "~_" "~@_" "~:@_"))
      (assert (equal (logical-fill-render (concatenate 'string "~@<aa " directive "bb~:@>"))
                     (logical-fill-render (concatenate 'string "~@<aa ~:_" directive "bb~:>"))))))
  (let ((*print-right-margin* 1))
    (assert (equal "pre fixa bsuf fix"
                   (logical-fill-render "~@<pre fix~;~A~;suf fix~:@>" "a b")))))
(let ((*print-pretty* nil) (*print-readably* nil) (*print-right-margin* 2))
  (assert (equal "(1 2 3)" (logical-fill-render "~:@<~@{~A~^ ~}~:@>" 1 2 3))))

;; Outer lexical fill includes nested metadata and justification literals.
;; Generated directives in these positions must be rejected, not flattened away.
(dolist (control '("~@<~@<pre fix~;body~:>~:@>"
                   "~@<~@<body~;middle~;suf fix~:>~:@>"
                   "~@<~<aa bb~>~:@>"))
  (assert (handler-case (progn (logical-fill-render control) nil) (error () t))))
;; Dynamic strings remain separate lexical controls, with their own fill policy.
(let ((*print-pretty* t) (*print-readably* nil) (*print-right-margin* 2))
  (assert (equal (logical-fill-render "~?" "~@<a b~:@>" nil)
                 (logical-fill-render "~@<a ~:_b~:>")))
  (assert (equal (logical-fill-render "~@<~?~:@>" "a b" nil) "a b")))
(let ((*print-pretty* t) (*print-readably* nil) (*print-right-margin* 2))
  (assert (equal (logical-fill-render "~@<~@<a b~:@>~:@>")
                 (logical-fill-render "~@<~@<a ~:_~:_b~:>~:>"))))
(write-line "FORMAT-LOGICAL-FILL-OK")
"##;

#[test]
fn logical_block_closing_at_sign_inserts_fill_newlines_only_for_body_blanks() {
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for variable in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(variable);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command
            .args([
                "--no-init",
                "--eval",
                &format!("(defparameter *fill-required-tier* {required}){PROGRAM}"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}/stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| *line == "FORMAT-LOGICAL-FILL-OK")
                .count(),
            1
        );
    }
}
