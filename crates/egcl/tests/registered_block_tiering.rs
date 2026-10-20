// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A BLOCK or TAGBODY is registered by name only when something created in its
//! body can name it from outside the activation (bliss-1c3iw).
//!
//! The lowerer used to register a block whenever its body established a
//! HANDLER-CASE, HANDLER-BIND or RESTART-CASE, treating those bodies as opaque.
//! ASSERT and IGNORE-ERRORS put one in a DEFUN's implicit block, so
//! ASDF's MAKE-ACTION-STATUS, UIOP's TRUENAMIZE and FEATUREP and others logged
//! `T1 declined (PushBlock needs an executing identity)` and stayed in the
//! interpreter, paying three allocations per call for a name nothing could
//! look up. HANDLER-CASE clauses are inline code with local transfers, and
//! handler and restart functions are bodies the lowerer can inspect, so only a
//! closure or restart clause that really exits by name keeps a registration.
//!
//! The corpus pairs those shapes with ones that do register: closures that
//! RETURN-FROM or GO out through native frames holding UNWIND-PROTECT cleanups
//! and special bindings, deep in the stack, via a funarg, through a handler and
//! from a HANDLER-CASE clause, and a THROW across the same frames. Every tier
//! must print what the interpreter prints. A RETURN-FROM to a block that has
//! already exited is left out: every tier, the interpreter included, reports an
//! internal error there instead of CONTROL-ERROR (bliss-hce1h).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(defvar *log* nil)
(defvar *dyn* :top)
(define-condition probe-signal (condition) ())

;; ASDF MAKE-ACTION-STATUS: &key, CHECK-TYPE and ASSERT in the implicit block.
(defun mas (&key bits stamp (level 0) index)
  (check-type bits (integer 0 7))
  (check-type level (integer 0 1000))
  (assert (or stamp (integerp bits)) () "bad ~S" bits)
  (block nil
    (when (and (null index) (zerop level))
      (case bits (0 (return :void)) (1 (return :todo))))
    (list bits stamp level index)))

;; UIOP TRUENAMIZE: IGNORE-ERRORS inside BLOCK NIL with early RETURNs.
(defun tru (x)
  (block nil
    (when (null x) (return :null))
    (or (ignore-errors (parse-integer x)) :bad)))

;; A tagged loop whose HANDLER-BIND names a global handler: no NamedTag needed.
(defun note-signal (c) (declare (ignore c)) (push :signalled *log*))
(defun signal-loop (n)
  (let ((i 0) (acc nil))
    (tagbody
     top
       (when (>= i n) (go done))
       (handler-bind ((probe-signal #'note-signal))
         (when (oddp i) (signal 'probe-signal)))
       (push i acc)
       (setq i (1+ i))
       (go top)
     done)
    (nreverse acc)))

;; Crossed by non-local exits: cleanups and a special binding in each frame.
(defun h (fn depth)
  (check-type depth (integer 0 100))
  (let ((*dyn* (list :h depth)))
    (unwind-protect
         (if (zerop depth) (funcall fn) (h fn (1- depth)))
      (push (list :cleanup depth *dyn*) *log*))))
(defun via-mapcar (fn xs)
  (check-type xs list)
  (let ((*dyn* :mapcar))
    (unwind-protect (mapcar fn xs)
      (push (list :mapcar-cleanup *dyn*) *log*))))
(defun call-stale (fn)
  (check-type fn function)
  (handler-case (funcall fn 1)
    (control-error () (list :control-error *dyn*))))
(defun h2 (x)
  (check-type x integer)
  (let ((*dyn* :h2))
    (unwind-protect (error "boom ~A" x)
      (push (list :h2-cleanup *dyn*) *log*))))
(defun h3 (fn)
  (check-type fn function)
  (let ((*dyn* :h3))
    (handler-case (error "inner")
      (error () (funcall fn (list :from-handler *dyn*))))))

;; These keep their registered blocks: a closure returns from each.
(defun deep (n) (block deep (h (lambda () (return-from deep (list :escaped n *dyn*))) 3) :not-reached))
(defun funarg (xs) (block funarg (via-mapcar (lambda (x) (when (evenp x) (return-from funarg (list :even x *dyn*))) x) xs)))
(defun make-escaper () (block esc (lambda (v) (return-from esc v))))
(defun in-handler (x)
  (block in-handler
    (handler-bind ((error (lambda (c) (return-from in-handler (list :handled (type-of c) *dyn*)))))
      (h2 x))))
(defun from-clause () (block from-clause (h3 (lambda (v) (return-from from-clause v))) :not-reached))
(defun go-escape () (tagbody (h (lambda () (go out)) 1) (return-from go-escape :not-reached) out) (list :went-out *dyn*))
(defun thrown () (catch 'tag (h (lambda () (throw 'tag (list :thrown *dyn*))) 2)))

;; Speculated arithmetic after ASSERT: later calls leave the fixnum profile.
(defun scale (x)
  (assert (numberp x))
  (block nil (when (> x 100) (return :big)) (+ x 1)))
;; A hot loop after ASSERT, entered once.
(defun sum-to (n)
  (assert (integerp n))
  (let ((s 0)) (dotimes (i n s) (setq s (+ s i)))))

(defmacro case30 (form)
  `(let ((r nil))
     (dotimes (i 30)
       (setq *log* nil)
       (let ((v (handler-case ,form (error (e) (list :error (type-of e))))))
         (when (= i 0) (setq r (list v (reverse *log*) *dyn*)))))
     (prin1 r) (terpri)))
(case30 (list (mas :bits 0) (mas :bits 1) (mas :bits 2 :stamp t) (mas :bits 3 :level 2 :index 4)))
(case30 (mas :bits 9))
(case30 (list (tru nil) (tru "12") (tru "zz")))
(case30 (signal-loop 4))
(case30 (deep 7))
(case30 (funarg '(1 3 4 5)))
(case30 (funarg '(1 3)))
(case30 (in-handler 5))
(case30 (from-clause))
(case30 (thrown))
(case30 (go-escape))
(let ((r nil))
  (dotimes (i 40) (setq r (scale i)))
  (let ((before (egcl-ext:deopt-count)))
    (prin1 (list r (scale 1.5) (scale (expt 2 70)) (scale 200.0) (scale 7))) (terpri)
    (format t "DEOPTS ~D~%" (- (egcl-ext:deopt-count) before))))
(prin1 (list (sum-to 5000) (sum-to 10))) (terpri)
(format t "TIERS ~S~%"
  (mapcar #'egcl-ext:function-tier
          '(mas tru signal-loop scale sum-to h via-mapcar h2 h3 deep funarg in-handler from-clause go-escape)))
(format t "DONE~%")
"#;

/// Functions whose blocks or tags used to be registered for nothing, in TIERS order.
const ELIDED: [&str; 5] = ["MAS", "TRU", "SIGNAL-LOOP", "SCALE", "SUM-TO"];

fn run(envs: &[(&str, &str)]) -> (String, String) {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            &format!("(progn {PROGRAM} (values))"),
        ])
        .env("EGCL_T0_T1_THRESHOLD", "5")
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_OSR_THRESHOLD", "1000")
        .envs(envs.iter().copied())
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: {}\n{stdout}\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("DONE"),
        "{envs:?}: corpus did not finish:\n{stdout}\n{stderr}"
    );
    (stdout, stderr)
}

/// The output without the tier report and deopt count, which vary by tier.
fn results(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| !line.starts_with("TIERS ") && !line.starts_with("DEOPTS "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tiers(stdout: &str) -> Vec<u32> {
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("TIERS ("))
        .unwrap_or_else(|| panic!("no tier report:\n{stdout}"));
    line.trim_end_matches(')')
        .split_whitespace()
        .map(|tier| tier.parse().expect("numeric tier"))
        .collect()
}

#[test]
fn non_local_exits_match_the_interpreter_at_every_tier() {
    let (oracle, _) = run(&[("EGCL_FORCE_TIER", "interp")]);
    // Pin the transfers themselves, so an oracle that drifted with the code
    // under test cannot hide a lost cleanup or binding.
    for expected in [
        "((:VOID :TODO (2 T 0 NIL) (3 NIL 2 4)) NIL :TOP)",
        "((:NULL 12 :BAD) NIL :TOP)",
        "((0 1 2 3) (:SIGNALLED :SIGNALLED) :TOP)",
        "((:ESCAPED 7 (:H 0)) ((:CLEANUP 0 (:H 0)) (:CLEANUP 1 (:H 1)) \
         (:CLEANUP 2 (:H 2)) (:CLEANUP 3 (:H 3))) :TOP)",
        "((:EVEN 4 :MAPCAR) ((:MAPCAR-CLEANUP :MAPCAR)) :TOP)",
        "((:HANDLED SIMPLE-ERROR :H2) ((:H2-CLEANUP :H2)) :TOP)",
        "((:FROM-HANDLER :H3) NIL :TOP)",
        "((:THROWN (:H 0)) ((:CLEANUP 0 (:H 0)) (:CLEANUP 1 (:H 1)) (:CLEANUP 2 (:H 2))) :TOP)",
        "((:WENT-OUT :TOP) ((:CLEANUP 0 (:H 0)) (:CLEANUP 1 (:H 1))) :TOP)",
        "(40 2.5 :BIG :BIG 8)",
        "(12497500 45)",
    ] {
        assert!(
            oracle.contains(expected),
            "interpreter lost {expected}:\n{oracle}"
        );
    }
    for tier in ["t0", "t1", "t2"] {
        if tier != "t0" && !cfg!(target_arch = "x86_64") {
            continue;
        }
        let (stdout, log) = run(&[("EGCL_FORCE_TIER", tier)]);
        assert_eq!(
            results(&oracle),
            results(&stdout),
            "{tier} differs from the interpreter:\n{log}"
        );
    }
    let (stdout, log) = run(&[]);
    assert_eq!(
        results(&oracle),
        results(&stdout),
        "automatic tiering differs:\n{log}"
    );
}

/// On the mapped native path the elided shapes install at the forced tier,
/// including through a deopt out of speculated arithmetic, and automatic
/// promotion takes the invocation-hot ones to T2 without the old decline.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn unregistered_blocks_and_tags_reach_t1_and_t2() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let (stdout, log) = run(&[("EGCL_FORCE_TIER", tier)]);
        let tiers = tiers(&stdout);
        for (name, actual) in ELIDED.iter().zip(&tiers) {
            assert_eq!(
                *actual, installed,
                "{name} at forced {tier}:\n{stdout}\n{log}"
            );
        }
        if tier == "t2" {
            let deopts = stdout
                .lines()
                .find_map(|line| line.strip_prefix("DEOPTS "))
                .and_then(|count| count.trim().parse::<u32>().ok())
                .unwrap_or_else(|| panic!("no deopt count:\n{stdout}"));
            assert!(
                deopts > 0,
                "SCALE never left its fixnum speculation:\n{log}"
            );
        }
    }
    let (stdout, log) = run(&[("EGCL_T2_LOG", "stderr")]);
    let tiers = tiers(&stdout);
    // SUM-TO runs twice, below the invocation gate; the others are hot.
    for (name, actual) in ELIDED.iter().zip(&tiers).take(4) {
        assert_eq!(*actual, 2, "{name} did not reach T2:\n{log}");
        assert!(
            log.contains(&format!("[T2] {name}: optimized worker result installed")),
            "{name} did not install at T2:\n{log}"
        );
    }
    assert!(
        !log.contains("needs an executing identity"),
        "a corpus function still registers a block or tag:\n{log}"
    );
}
