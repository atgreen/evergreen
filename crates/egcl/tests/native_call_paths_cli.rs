// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled calls and FUNCALLs (bliss-gg3i1.7.3): a named call to a compiled
//! DEFUN, FUNCALL of a compiled closure (one that reads and writes its
//! captured variables), of a symbol naming a compiled function and of a
//! function object, with up to five arguments, from a once-called loop and
//! from a warmed caller, answer exactly as the interpreter does in every tier.
//! The misses are covered too: a redefinition in the middle of a loop, a
//! wrong argument count, multiple values and a THROW across the call.

use std::process::Command;

const PROGRAM: &str = r#"
(defun cp-add2 (a b) (+ a b))
(defun cp-add5 (a b c d e) (+ a b c d e))
(defun cp-opt (a &optional (b 1)) (+ a b))
(defun cp-key (a &key (b 2)) (+ a b))
(defun cp-rest (a &rest r) (+ a (length r)))
(defun cp-mv (a) (values a (1+ a) (+ a 2)))
(defun cp-thrower (a) (if (> a 5) (throw 'cp-tag (list :thrown a)) a))
(defun cp-make-adder (k) (lambda (x) (+ x k)))
(defun cp-make-counter ()
  (let ((n 0)) (lambda (d) (setq n (+ n d)) n)))
(defun cp-make-many (a b c)
  (lambda (x y z p q) (list (+ x a) (+ y b) (+ z c) p q)))
(dotimes (i 40) (cp-make-adder i) (cp-make-counter) (cp-make-many i i i))
(defun cp-sum-calls (n)
  (let ((s 0)) (dotimes (i n s) (setq s (cp-add2 s (cp-add5 1 0 0 0 i))))))
(defun cp-sum-funcall (n f) (let ((s 0)) (dotimes (i n s) (setq s (funcall f s)))))
(defun cp-sum-symbol (n) (let ((s 0)) (dotimes (i n s) (setq s (funcall 'cp-add2 s 1)))))
(defun cp-sum-fn5 (n f) (let ((s 0)) (dotimes (i n s) (setq s (funcall f s 1 0 0 i)))))
(defun cp-counter-loop (n f) (let ((s 0)) (dotimes (i n s) (setq s (funcall f 1)))))
(defun cp-alternate (n f g h)
  (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f i) (funcall g 1) (funcall h -1))))))
(defun cp-many-loop (n f) (let ((r nil)) (dotimes (i n r) (setq r (funcall f i 1 2 3 4)))))
(defun cp-opt-loop (n) (let ((s 0)) (dotimes (i n s) (setq s (+ (cp-opt s) (cp-key 0) (cp-rest 0 1 2))))))
(defun cp-mv-loop (n f) (let ((r nil)) (dotimes (i n r) (setq r (multiple-value-list (funcall f i))))))
(defun cp-throw-loop (n f)
  (catch 'cp-tag (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f i)))))))
(defun cp-warm-call (s) (cp-add2 s 1))
(defun cp-warm-funcall (f s) (funcall f s))
(defun cp-warm-symbol (s) (funcall 'cp-add2 s 1))
(defun cp-redefine-loop (n)
  (let ((r nil))
    (dotimes (i n r)
      (when (= i (floor n 2))
        (setf (symbol-function 'cp-victim) (lambda (x) (list :new x))))
      (push (cp-victim i) r)
      (push (funcall 'cp-victim i) r))))
(defun cp-victim (x) (list :old x))
(defun cp-try (f &rest args)
  (handler-case (apply f args) (error (e) (list :error (type-of e)))))
(defun cp-wrong-count (f) (cp-try (lambda () (funcall f 1 2 3))))
(defun cp-designators (i)
  (list (funcall 'funcall #'cp-add2 i 1) (funcall #'funcall 'cp-mv i)
        (multiple-value-list (funcall 'funcall 'cp-mv i))
        (apply 'apply #'list i '((2 3))) (apply #'funcall #'cp-add2 (list i 2))
        (funcall 'vector i 1) (multiple-value-list (funcall 'values i 1 2))
        (multiple-value-list (funcall 'vector i))
        (cp-try 'funcall) (cp-try 'apply #'list) (cp-try 'apply #'list 1 2)))
(defun cp-designator-loop (n) (let ((r nil)) (dotimes (i n r) (setq r (cp-designators i)))))
(defun cp-run (n)
  (let ((adder (cp-make-adder 3)) (counter (cp-make-counter)) (many (cp-make-many 10 20 30)))
    (list (cp-sum-calls n) (cp-sum-funcall n adder) (cp-sum-symbol n)
          (cp-sum-fn5 n #'cp-add5) (cp-counter-loop n counter) (funcall counter 0)
          (cp-alternate n adder (cp-make-adder 100) (cp-make-adder 7))
          (let ((c2 (cp-make-counter))) (list (cp-alternate n counter c2 c2) (funcall c2 0)))
          (cp-many-loop n many) (cp-opt-loop n) (cp-mv-loop n #'cp-mv)
          (cp-mv-loop n 'cp-mv) (cp-throw-loop n #'cp-thrower) (cp-throw-loop 3 'cp-thrower)
          (let ((s 0)) (dotimes (i n s) (setq s (cp-warm-call s))))
          (let ((s 0)) (dotimes (i n s) (setq s (cp-warm-funcall adder s))))
          (let ((s 0)) (dotimes (i n s) (setq s (cp-warm-symbol s))))
          (cp-wrong-count adder) (cp-wrong-count #'cp-add2) (cp-wrong-count 'cp-add2)
          (cp-try #'cp-sum-funcall 2 #'cp-add5) (cp-try #'cp-sum-symbol 0)
          (cp-designator-loop n))))
(format t "RESULT ~S~%" (cp-run 20))
(dotimes (i 30) (cp-run 50))
(format t "RESULT ~S~%" (cp-run 2000))
(format t "REDEFINED ~S~%" (cp-redefine-loop 6))
(defun cp-add2 (a b) (list :redefined a b))
(format t "REDEFINED ~S~%" (list (cp-sum-symbol 1) (cp-warm-symbol 5) (cp-sum-calls 1)))
"#;

fn run(tier: &str, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .arg("--no-init")
        .args(["--eval", &format!("(progn {PROGRAM})")]);
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_T2",
        "EGCL_NO_LEAF_BUILTINS",
    ] {
        command.env_remove(name);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    if tier.is_empty() {
        command.env_remove("EGCL_FORCE_TIER");
    } else {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier {tier:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .filter(|line| line.starts_with("RESULT ") || line.starts_with("REDEFINED "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn compiled_calls_and_funcalls_agree_with_the_interpreter_in_every_tier() {
    let expected = run("interp", false);
    assert!(expected.contains("PROGRAM-ERROR)"), "{expected}");
    assert!(expected.contains("(:THROWN 6)"), "{expected}");
    assert!(expected.contains("(:NEW 5)"), "{expected}");
    assert!(expected.contains("(:REDEFINED 0 1)"), "{expected}");
    for tier in ["t0", "t1", "t2", ""] {
        assert_eq!(run(tier, false), expected, "tier {tier:?}");
    }
    assert_eq!(run("", true), expected, "default tiers under GC stress");
}
