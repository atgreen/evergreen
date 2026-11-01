// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

const PROGRAM: &str = r#"
(in-package :cl-user)
(declaim (notinline __MAKER__ churn-phase churn-warm))
;; Preserve the original issue's nested 1000-iteration construct-and-call shape.
(defun __MAKER__ (n)
  (let ((s 0))
    (dotimes (i n s)
      (dotimes (j 1000) (incf s (funcall __LAMBDA__ j))))))
;; Warm this same named wrapper before opening the event window. A top-level
;; LOOP warmup in the window creates unrelated private helper compilations.
(defun churn-phase (n measure)
  (when measure (egcl-ext:events-reset) (egcl-ext:events-start))
  ;; Collect in the middle of the measured window: closure creation itself
  ;; no longer forces major collections (bliss-gg3i1.7.15.3).
  (let ((answer (if measure
                    (+ (__MAKER__ (floor n 2))
                       (progn (egcl-ext:gc :full t) (__MAKER__ (- n (floor n 2)))))
                    (__MAKER__ n))))
    (when measure (egcl-ext:events-stop))
    (assert (= answer (* n 500500)))
    (when measure
      (format t "CHURN-CHECKSUM ~D~%" answer)
      (egcl-ext:events-summary)
      (egcl-ext:events-json)))
  t)
(defun churn-warm () (churn-phase 1 nil))
(egcl-ext:events-reset) (egcl-ext:events-start)
(dotimes (i 200) (churn-warm))
(loop repeat 150
      until (every (lambda (s) (= 2 (egcl-ext:function-tier s)))
                   '(__MAKER__ churn-phase churn-warm))
      do (churn-warm) (sleep 0.01))
(dolist (s '(__MAKER__ churn-phase churn-warm))
  (assert (= 2 (egcl-ext:function-tier s))))
;; Finish pending child compilation before measuring. The named factory stays
;; live; no returned closure object or first compilation owner is retained.
(egcl-ext:gc :full t)
(churn-warm)
(sleep 0.1)
(egcl-ext:events-stop)
(format t "CHURN-CHILD-EVIDENCE-BEGIN~%")
(egcl-ext:events-json)
(format t "CHURN-CHILD-EVIDENCE-END~%")
(format t "CHURN-READY __MAKER__ TIER2~%") (finish-output)
;; Cover measured host-memory pressure as well as the legacy closure-count
;; trigger. Keep the major-GC event assertion: a collection before this window
;; does not prove that warmed child code survives reclamation during churn.
(churn-phase 400 t)
(assert (= 2 (egcl-ext:function-tier '__MAKER__)))
(format t "CHURN-SEMANTIC-PASS __MAKER__~%")
"#;

// Each function record in EVENTS-JSON has exactly one t0 string followed by
// its optional native representations. Split at that field so a different
// helper's T2 cannot satisfy the child-body assertion. The instruction strings
// below match the complete tiny child, not a generic CLOSURE display name.
fn child_has_native_t2(snapshot: &str, capturing: bool) -> bool {
    snapshot.split("\"t0\":\"").skip(1).any(|record| {
        // The capturing child reads J through its captured frame or, where the
        // unit uses heap environments (Backpack; `EGCL_HEAP_ENV`), from its
        // closure's own environment, which its prologue caches in local 1.
        let frame_capture = record.starts_with("; CLOSURE — 1 arg(s), 1 local(s),")
            && record.contains(
                "0: LoadLocal(0)\\n     1: LoadEnvVar(0)\\n     2: CallNamed { sym: +, nargs: 2 }",
            )
            && record.contains("\\n     3: Return\\n");
        let heap_capture = record.starts_with("; CLOSURE — 1 arg(s), 2 local(s),")
            && record.contains(
                "0: ClosureEnv\\n     1: StoreLocal(1)\\n     2: LoadLocal(0)\\n     \
                 3: LoadLocal(1)\\n     4: EnvRef { depth: 0, idx: 0, name: 0 }    ; J\\n     \
                 5: CallNamed { sym: +, nargs: 2 }",
            )
            && record.contains("\\n     6: Return\\n");
        let body_matches = if capturing {
            frame_capture || heap_capture
        } else {
            record.starts_with("; CLOSURE — 1 arg(s), 1 local(s),")
                && record.contains("0: LoadLocal(0)\\n     1: CallNamed { sym: 1+, nargs: 1 }")
                && record.contains("\\n     2: Return\\n")
        };
        body_matches && record.contains(",\"t2\":\"") && !record.contains(",\"t2\":\"\"")
    })
}

#[test]
fn original_closure_loops_retain_compiled_children_across_major_gc() {
    for (name, lambda) in [
        ("make-noncapturing", "#'(lambda (k) (1+ k))"),
        // Preserve the original capturing loop's argument 1 and capture j.
        ("make-capturing", "#'(lambda (k) (+ k j))"),
    ] {
        let mut program = PROGRAM
            .replace("__MAKER__", &name.to_uppercase())
            .replace("__LAMBDA__", lambda);
        if name == "make-capturing" {
            program = program.replace(
                "(funcall #'(lambda (k) (+ k j)) j)",
                "(funcall #'(lambda (k) (+ k j)) 1)",
            );
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_LAZY_COMPILE",
            "EGCL_T2_NO_QUEUE",
            "EGCL_PROFILING_DISABLED",
            "EGCL_GC_DISABLE",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_POISON",
            "EGCL_GC_VERIFY",
        ] {
            command.env_remove(key);
        }
        // Every iteration must produce a fresh noncapturing object too; cached
        // object reuse would hide the dead-first-instance ownership failure.
        let output = command
            .env("EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE", "1")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{name}\n{stdout}\n{stderr}");
        let (warm, measured) = stdout.split_once("CHURN-READY ").expect("ready marker");
        let child_snapshot = warm
            .lines()
            .find(|line| line.starts_with("{\"events\":"))
            .expect("prewindow child snapshot");
        assert!(
            child_has_native_t2(child_snapshot, name == "make-capturing"),
            "actual child T2 representation missing before window: {name}\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("CHURN-CHECKSUM 200200000"),
            "{name}\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("CHURN-READY {} TIER2", name.to_uppercase())),
            "{name}\n{stdout}"
        );
        assert!(
            measured.contains("\"kind\":\"gc-major\""),
            "must exercise normal major GC: {name}\n{stdout}"
        );
        // Only the already-warmed named wrapper and original maker execute in
        // the event window. Thus every compile here is a regression, rather
        // than noise from a newly compiled top-level warming helper.
        assert!(
            !measured.contains("\"kind\":\"compile\""),
            "compiled child retired across GC: {name}\n{stdout}\n{stderr}"
        );
        assert!(
            measured.contains("Compiles: 0   Deopts: 0"),
            "{name}\n{stdout}"
        );
        for installed in [
            name.to_uppercase(),
            "CHURN-PHASE".to_string(),
            "CHURN-WARM".to_string(),
        ] {
            assert!(
                stderr.lines().any(|line| line.contains(&format!(
                    "{installed}: optimized worker result installed with "
                ))),
                "actual native installation missing for {installed}\n{stderr}"
            );
        }
    }
}
