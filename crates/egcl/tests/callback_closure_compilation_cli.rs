// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
;; This place is intentionally absent from the bytecode lowerer's builtin
;; setters: compiling the callback must use its real SETF expansion.
(define-setf-expander callback-compiled-place (object)
  (let ((temp (gensym "CELL")) (store (gensym "VALUE")))
    (values (list temp) (list object) (list store)
            `(progn (setf (aref ,temp 0) ,store) ,store)
            `(aref ,temp 0))))
(setq *callback-compiled-cell* nil *callback-compiled-ticks* nil)
;; EVAL deliberately creates a source tree closure. A compiled creator's
;; MakeClosure would already contain prepared bytecode and miss this regression.
(setq *callback-compiled-original*
  (eval '(let ((cell (vector 0)) (ticks (list 0)))
           (setq *callback-compiled-cell* cell *callback-compiled-ticks* ticks)
           (macrolet ((next-state (value) `(1+ ,value)))
             (lambda (n)
               (if (minusp n)
                   (progn (incf (car ticks)) (error "callback closure error"))
                   (progn
                     (dotimes (i n)
                       (setf (callback-compiled-place cell)
                             (next-state (aref cell 0))))
                     (incf (car ticks))
                     (values (aref cell 0) :private-secondary))))))))
(assert (null (egcl-ext:function-tier *callback-compiled-original*)))
(setq *callback-compiled-handle*
  (egcl-ffi:make-callback *callback-compiled-original* :int '(:int)))
(defun callback-compiled-invoke (n)
  ;; Generated outbound FFI call -> generated C ABI callback entry -> Lisp. This is
  ;; an actual callback transition, not FUNCALL of the underlying Lisp object.
  (egcl-ffi:foreign-call (egcl-ffi:callback-pointer *callback-compiled-handle*)
                         :int '(:int) (list n)))
(assert (equal '(4096) (multiple-value-list (callback-compiled-invoke 4096))))
(assert (= 4096 (aref *callback-compiled-cell* 0)))
(assert (= 1 (car *callback-compiled-ticks*)))
"#;

const FIRST_BYTECODE: &str = r#"
;; A long-loop tree closure follows eager-loop DEFUN policy even when the
;; general lazy-compile threshold is deliberately much higher than one call.
(assert (integerp (egcl-ext:function-tier *callback-compiled-original*)))
"#;

const WARM: &str = r#"
(dotimes (i 40)
  (let ((before (aref *callback-compiled-cell* 0))
        (ticks (car *callback-compiled-ticks*)))
    (assert (= (1+ before) (callback-compiled-invoke 1)))
    (assert (= (1+ ticks) (car *callback-compiled-ticks*)))))
"#;

const NATIVE_READY: &str = r#"
(loop repeat 150
 until (= 2 (or (egcl-ext:function-tier *callback-compiled-original*) -1))
 do (let ((before (aref *callback-compiled-cell* 0))
          (ticks (car *callback-compiled-ticks*)))
      (assert (= (1+ before) (callback-compiled-invoke 1)))
      (assert (= (1+ ticks) (car *callback-compiled-ticks*))))
    (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier *callback-compiled-original*)))
"#;

const CHECK: &str = r#"
(unwind-protect
    (progn
      ;; The closure must keep the authoritative mutable captured vector.
      (setf (aref *callback-compiled-cell* 0) 41)
      (let ((ticks (car *callback-compiled-ticks*)))
        (assert (equal '(43) (multiple-value-list (callback-compiled-invoke 2))))
        (assert (= (1+ ticks) (car *callback-compiled-ticks*))))
      (assert (null (egcl-ffi:callback-error *callback-compiled-handle*)))
      (let ((ticks (car *callback-compiled-ticks*)))
        (assert (eq :contained
          (handler-case (progn (callback-compiled-invoke -1) :missing-error)
            (egcl-ffi:ffi-error () :contained))))
        (assert (= (1+ ticks) (car *callback-compiled-ticks*))))
      (assert (= 43 (aref *callback-compiled-cell* 0)))
      (let ((message (egcl-ffi:callback-error *callback-compiled-handle*)))
        (assert (stringp message))
        (assert (search "callback closure error" message)))
      (assert (null (egcl-ffi:callback-error *callback-compiled-handle*)))
      ;; An error is contained at C, not left pending in a later native entry.
      (let ((ticks (car *callback-compiled-ticks*)))
        (assert (equal '(46) (multiple-value-list (callback-compiled-invoke 3))))
        (assert (= (1+ ticks) (car *callback-compiled-ticks*))))
      (setf (aref *callback-compiled-cell* 0) 100)
      (let ((ticks (car *callback-compiled-ticks*)))
        (assert (= 102 (callback-compiled-invoke 2)))
        (assert (= (1+ ticks) (car *callback-compiled-ticks*))))
      (format t "CALLBACK-CLOSURE-COMPILE-OK ~D~%" (aref *callback-compiled-cell* 0)))
  (egcl-ffi:free-callback *callback-compiled-handle*))
"#;

#[test]
fn callback_tree_closure_expands_captured_macros_and_custom_setf_before_tiering() {
    let mut reference = None;
    // Default lazy mode proves first-call loop compilation independently of a
    // force-tier setting. The other children retain the differential oracle
    // and deterministic native/stress coverage.
    for (tier, stress) in [
        ("interp", false),
        ("default", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "1")
            .env("EGCL_LAZY_THRESHOLD", "1000")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if tier == "default" {
            command.env_remove("EGCL_FORCE_TIER");
        } else {
            command.env("EGCL_FORCE_TIER", tier);
        }
        for key in [
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
            "EGCL_DISABLE_TREE_CLOSURE_EXPANSION",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(key);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let native = tier != "interp";
        let first = if native { FIRST_BYTECODE } else { "" };
        let ready = if native { NATIVE_READY } else { "" };
        let program = format!("{SOURCE}\n{first}\n{WARM}\n{ready}\n{CHECK}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .expect("run callback closure regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("CALLBACK-CLOSURE-COMPILE-OK 102"));
        if native {
            assert!(
                stderr.lines().any(|line| line.contains("<lambda>")
                    && line.contains("optimized worker result installed with ")
                    && (line.contains("ordinary x86-64 Lisp T2 ABI")
                        || line.contains("mapped T2 ABI"))),
                "the original private callback closure must install actual native T2\n{stderr}"
            );
            assert!(
                !stderr.lines().any(
                    |line| line.contains("<lambda>") && line.contains("setf:unsupported-place")
                ),
                "custom SETF preparation must precede closure lowering\n{stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected);
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
