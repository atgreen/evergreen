#!/usr/bin/env python3
"""Verify s390x native execution, OSR and moving-GC safety through the CLI.

Run under scripts/torcl-limited.sh, passing the binary or QEMU command after --.
The tier assertions are essential: correct bytecode fallback is not a JIT pass.
"""
import os
import subprocess
import sys


PROGRAM = r"""
(defun jit-check (x) (if x t (error "s390x JIT check failed")))
(defun jit-sum (n)
  (let ((i 0) (sum 0))
    (block done (tagbody top
      (if (>= i n) (return-from done sum))
      (setq sum (+ sum i)) (setq i (1+ i)) (go top)))))
(defun jit-roots ()
  (let ((x (list 1 2)))
    (multiple-value-bind (a b) (values (list x x) (list x))
      (list a b x '(constant root)))))
(defun jit-wide (a b c d e f g h) (list h g f e d c b a))
(defun jit-recursive (n)
  (if (= n 0) nil (cons (list n) (jit-recursive (1- n)))))
(setq *jit-effects* 0)
(defun jit-overflow (x)
  (setq *jit-effects* (1+ *jit-effects*)) (+ x 1))
(defun jit-error () (car 17) (setq *jit-effects* 99))
(format t "~S~%" (jit-sum 1000))
(format t "~S~%" (jit-roots))
(format t "~S~%" (jit-wide 1 2 3 4 5 6 7 8))
(format t "~S~%" (jit-recursive 8))
(format t "~S~%" (jit-overflow 1152921504606846975))
(jit-check (= *jit-effects* 1))
(handler-case (jit-error) (type-error () (format t "ERROR-CAUGHT~%")))
(jit-check (= *jit-effects* 1))
(format t "JIT-OK~%")
"""


def main():
    command = sys.argv[1:]
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise SystemExit("usage: s390x-jit-smoke.py -- [qemu-s390x -L sysroot] torcl")
    base = {k: v for k, v in os.environ.items() if not k.startswith("TORCL_")}

    def run(source, **settings):
        env = dict(base, TORCL_LAZY_COMPILE="0", TORCL_T1_T2_BACKEDGE_THRESHOLD="4", **settings)
        # --eval prints the last form's value. Give every variant the same
        # final value even when it appends extra tier assertions.
        result = subprocess.run(command + ["--no-init", "--no-bootstrap", "--eval", source + "\nnil"],
                                env=env, capture_output=True, text=True, timeout=180)
        if result.returncode:
            raise RuntimeError(f"exit {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result.stdout

    reference = run(PROGRAM, TORCL_FORCE_TIER="t0")
    assert "JIT-OK" in reference, reference
    native_checks = """
      (jit-check (= (torcl-ext:function-tier 'jit-sum) 1))
      (jit-check (= (torcl-ext:function-tier 'jit-roots) 1))
      (jit-check (= (torcl-ext:function-tier 'jit-wide) 1))
      (jit-check (= (torcl-ext:function-tier 'jit-recursive) 1))
      (jit-check (> (torcl-ext:deopt-count) 0))
    """
    native = run(PROGRAM + native_checks, TORCL_FORCE_TIER="t1")
    stressed = run(PROGRAM + native_checks, TORCL_FORCE_TIER="t1",
                   TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert native == stressed == reference, (reference, native, stressed)
    print("s390x: native T1, deopt, calls, errors and GC stress match T0", flush=True)

    # Keep invocation promotion cold so this must enter through an OSR stub.
    osr = PROGRAM + "(jit-check (> (torcl-ext:function-osr-count 'jit-sum) 0))"
    output = run(osr, TORCL_T0_T1_THRESHOLD="1000000", TORCL_OSR_THRESHOLD="2",
                 TORCL_DISABLE_T2="1")
    stressed = run(osr, TORCL_T0_T1_THRESHOLD="1000000", TORCL_OSR_THRESHOLD="2",
                   TORCL_DISABLE_T2="1", TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert output == stressed == reference, (reference, output, stressed)
    print("s390x: live T0-to-native OSR and GC stress match T0", flush=True)

    trap = r"""
      (defun jit-check (x) (if x t (error "OSR trap check failed")))
      (defun jit-protected-loop (initial)
        (handler-case
          (let ((i 0) (sum initial))
            (block done (tagbody top
              (if (>= i 10) (return-from done sum))
              (setq sum (+ sum 1)) (setq i (1+ i)) (go top))))
          (error () :bad)))
      (jit-check (= (jit-protected-loop 0) 10))
      (jit-check (> (torcl-ext:function-osr-count 'jit-protected-loop) 0))
      (setq jit-before (torcl-ext:function-osr-count 'jit-protected-loop))
      (jit-check (= (jit-protected-loop 1152921504606846970) 1152921504606846980))
      (jit-check (> (torcl-ext:function-osr-count 'jit-protected-loop) jit-before))
      (setq *scope-effects* 0)
      (defun jit-expired-catch ()
        (catch 'expired
          (let ((i 0))
            (block done (tagbody top
              (if (>= i 10) (return-from done i))
              (setq i (1+ i)) (go top)))))
        (setq *scope-effects* (1+ *scope-effects*))
        (throw 'expired 99))
      (handler-case (jit-expired-catch)
        (control-error () (jit-check (= *scope-effects* 1))))
      (jit-check (= *scope-effects* 1))
      (jit-check (> (torcl-ext:function-osr-count 'jit-expired-catch) 0))
      (setq *cleanups* 0)
      (defun jit-unwinding-go ()
        (let ((i 0))
          (block done (tagbody top
            (if (>= i 10) (return-from done i))
            (unwind-protect (progn (setq i (1+ i)) (go top))
              (setq *cleanups* (1+ *cleanups*)))))))
      (jit-check (= (jit-unwinding-go) 10))
      (jit-check (= *cleanups* 10))
      (defun jit-outer-go ()
        (let ((i 0) (visits 0))
          (block done (tagbody outer
            (setq visits (1+ visits))
            (if (>= visits 3) (return-from done (list i visits)))
            (tagbody inner
              (if (>= i 10) (go outer))
              (setq i (1+ i)) (go inner))))))
      (jit-check (equal (jit-outer-go) '(10 3)))
      (jit-check (> (torcl-ext:function-osr-count 'jit-outer-go) 0))
      (format t "OSR-TRAP-OK~%")
    """
    ordinary = run(trap, TORCL_T0_T1_THRESHOLD="1000000", TORCL_OSR_THRESHOLD="2",
                   TORCL_DISABLE_T2="1", TORCL_OSR_TRAPS="1")
    stressed = run(trap, TORCL_T0_T1_THRESHOLD="1000000", TORCL_OSR_THRESHOLD="2",
                   TORCL_DISABLE_T2="1", TORCL_OSR_TRAPS="1",
                   TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert ordinary == stressed and "OSR-TRAP-OK" in ordinary, (ordinary, stressed)
    print("s390x: OSR overflow and uncommon traps preserve active handlers", flush=True)

    auto = PROGRAM + """
      (jit-sum 10) (jit-sum 10)
      (jit-check (= (torcl-ext:function-tier 'jit-sum) 1))
    """
    assert run(auto, TORCL_T0_T1_THRESHOLD="2", TORCL_DISABLE_T2="1") == reference
    print("s390x: automatic invocation promotion reaches T1", flush=True)


if __name__ == "__main__":
    main()
