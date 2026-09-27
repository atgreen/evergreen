#!/usr/bin/env python3
"""Verify s390x native execution, OSR and moving-GC safety through the CLI.

Run under scripts/torcl-limited.sh, passing the binary or QEMU command after --.
The tier assertions are essential: correct bytecode fallback is not a JIT pass.
"""
import os
import selectors
import time
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


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

    with tempfile.TemporaryDirectory(prefix="torcl-s390x-jit-") as directory:
        dump_path = Path(directory) / "jit.dump"
        listing = run("""
          (defun jit-diagnostics (x) (+ x 1))
          (jit-diagnostics 2)
          (if (= (torcl-ext:function-tier 'jit-diagnostics) 1) nil
              (error "diagnostic fixture did not compile"))
          (disassemble 'jit-diagnostics)
        """, TORCL_FORCE_TIER="t1", TORCL_PERF_JITDUMP=str(dump_path))
        assert "bytes of s390x" in listing and "bytes of x86" not in listing, listing
        assert "+0000:" in listing and ".byte 0xeb, 0x6f" in listing, listing
        # jitdump fields use the producer's native byte order, not the host
        # Python process's byte order when this test drives QEMU.
        dump = dump_path.read_bytes()
        magic, version, header_size, machine = struct.unpack_from(">IIII", dump)
        assert (magic, version, header_size, machine) == (0x4A695444, 1, 40, 22)
        listed_bytes = bytearray()
        for line in listing.splitlines():
            offset, separator, byte_text = line.partition(":  .byte ")
            if separator:
                assert int(offset.strip().removeprefix("+"), 16) == len(listed_bytes)
                listed_bytes.extend(int(value, 16) for value in byte_text.split(", "))
        at = header_size
        found = False
        while at + 56 <= len(dump):
            record, size = struct.unpack_from(">II", dump, at)
            assert record == 0 and size > 56 and at + size <= len(dump)
            name, _, code = dump[at + 56:at + size].partition(b"\x00")
            if b"JIT-DIAGNOSTICS" in name:
                assert listed_bytes == code, (listed_bytes, code)
                found = True
            at += size
        assert found, "no code-load record for JIT-DIAGNOSTICS"
    print("s390x: native listings and perf jitdump identify System Z", flush=True)

    optimized = r"""
      (defun jit-t2-add (x) (+ x 1))
      (defun jit-t2-branch (x) (if (< x 0) (- x 1) (+ x 1)))
      (defun jit-t2-neg (x) (- x))
      (defun jit-t2-literal (x) (if (< x 0) '(constant negative) '(constant positive)))
      (jit-t2-add 10) (jit-t2-add 11)
      (jit-t2-branch -2) (jit-t2-branch 2)
      (jit-t2-neg 1) (jit-t2-literal 1)
      (format t "~S~%" (list (jit-t2-add 41) (jit-t2-branch -5) (jit-t2-branch 5)
                             (jit-t2-neg -4) (jit-t2-literal -1) (jit-t2-literal 1)))
    """
    tiers = r"""
      (if (= (torcl-ext:function-tier 'jit-t2-add) 2) nil (error "ADD missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-branch) 2) nil (error "BRANCH missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-neg) 2) nil (error "NEG missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-literal) 2) nil (error "LITERAL missed T2"))
    """
    traps = r"""
      (format t "~S~%" (jit-t2-add 1152921504606846975))
      (format t "~S~%" (jit-t2-branch 1.5))
      (format t "~S~%" (jit-t2-neg -1152921504606846976))
    """
    reference = run(optimized + traps, TORCL_FORCE_TIER="t0")
    native = run(optimized + tiers + traps, TORCL_FORCE_TIER="t2")
    stressed = run(optimized + tiers + traps, TORCL_FORCE_TIER="t2",
                   TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert reference == native == stressed, (reference, native, stressed)
    print("s390x: optimized T2 arithmetic, branches and guard exits match T0", flush=True)

    calls = r"""
      (defun jit-t2-roots (x)
        (let ((a (list x x)))
          (list a (list x) a x)))
      (defun jit-t2-wide (a b c d e f g h i j k l)
        (list l k j i h g f e d c b a))
      (defun jit-t2-spill-roots (x)
        (let ((a (list x 1)) (b (list x 2)) (c (list x 3)) (d (list x 4))
              (e (list x 5)) (f (list x 6)) (g (list x 7)) (h (list x 8))
              (i (list x 9)) (j (list x 10)) (k (list x 11)) (l (list x 12)))
          (list a b c d e f g h i j k l (list x) a b c d e f g h i j k l)))
      (setq *jit-t2-effects* 0)
      (defun jit-t2-effect (x)
        (setq *jit-t2-effects* (1+ *jit-t2-effects*))
        (+ x 1))
      (defun jit-t2-error (x)
        (length x)
        (setq *jit-t2-effects* 999))
      (defun jit-t2-values (x)
        (multiple-value-bind (a b) (values (list x) (list x x))
          (list a b x)))
      (defun jit-t2-many-values (x)
        (multiple-value-bind (a b c d e f g h i j k l) (values x 2 3 4 5 6 7 8 9 10 11 12)
          (list a b c d e f g h i j k l)))
      (defun jit-t2-target (x) (+ x 1))
      (defun jit-t2-resolve (x) (funcall #'jit-t2-target x))
      (defun jit-t2-nlx-helper (x) (if x (throw 'out (list x)) nil))
      (defun jit-t2-nlx (fn x) (funcall fn x) (setq *jit-t2-effects* 999))
      (jit-t2-roots '(warm root))
      (jit-t2-wide 1 2 3 4 5 6 7 8 9 10 11 12)
      (jit-t2-spill-roots '(warm root))
      (jit-t2-values '(warm root))
      (jit-t2-many-values '(warm root))
      (format t "~S~%" (jit-t2-roots '(input root)))
      (format t "~S~%" (jit-t2-wide 1 2 3 4 5 6 7 8 9 10 11 12))
      (format t "~S~%" (jit-t2-spill-roots '(input root)))
      (format t "~S~%" (jit-t2-values '(input root)))
      (format t "~S~%" (jit-t2-many-values '(input root)))
      (jit-t2-effect 1) (jit-t2-effect 2)
      (jit-t2-error '(valid input)) (jit-t2-error '(valid input))
      (jit-t2-resolve 1) (jit-t2-resolve 2)
      (jit-t2-nlx #'jit-t2-nlx-helper nil) (jit-t2-nlx #'jit-t2-nlx-helper nil)
      (setq *jit-t2-effects* 0)
    """
    call_tiers = r"""
      (if (= (torcl-ext:function-tier 'jit-t2-roots) 2) nil (error "ROOTS missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-wide) 2) nil (error "WIDE missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-spill-roots) 2) nil (error "SPILL-ROOTS missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-values) 2) nil (error "VALUES missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-many-values) 2) nil (error "MANY-VALUES missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-effect) 2) nil (error "EFFECT missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-error) 2) nil (error "ERROR missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-resolve) 2) nil (error "RESOLVE missed T2"))
      (if (= (torcl-ext:function-tier 'jit-t2-nlx) 2) nil (error "NLX missed T2"))
    """
    effects = r"""
      (format t "~S~%" (jit-t2-effect 1152921504606846975))
      (format t "~S~%" *jit-t2-effects*)
      (handler-case (jit-t2-error 17) (type-error () (format t "T2-ERROR-CAUGHT~%")))
      (format t "~S~%" *jit-t2-effects*)
      (format t "~S~%" (catch 'out (jit-t2-nlx #'jit-t2-nlx-helper 23)))
      (format t "~S~%" *jit-t2-effects*)
      (defun jit-t2-target (x) (+ x 10))
      (format t "~S~%" (jit-t2-resolve 5))
    """
    reference = run(calls + effects, TORCL_FORCE_TIER="t0")
    native = run(calls + call_tiers + effects, TORCL_FORCE_TIER="t2")
    stressed = run(calls + call_tiers + effects, TORCL_FORCE_TIER="t2",
                   TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert reference == native == stressed, (reference, native, stressed)
    print("s390x: T2 calls, live roots, multiple values and committed effects match T0", flush=True)

    loops = r"""
      (setq *jit-loop-effects* 0)
      (defun jit-t2-loop (n initial x)
        (setq *jit-loop-effects* (1+ *jit-loop-effects*))
        (let ((i 0) (sum initial))
          (block done (tagbody top
            (if (>= i n) (return-from done (list sum x)))
            (if (= i 100000) (setq x (list x)))
            (setq sum (1+ sum)) (setq i (1+ i)) (go top)))))
      (jit-t2-loop 0 0 '(live root))
      (format t "~S~%" (jit-t2-loop 200000 1152921504606646985 '(live root)))
      (format t "~S~%" *jit-loop-effects*)
    """
    loop_tiers = r"""
      (if (= (torcl-ext:function-tier 'jit-t2-loop) 2) nil (error "LOOP missed T2"))
      (if (> (torcl-ext:function-osr-count 'jit-t2-loop) 0) nil (error "LOOP missed live T2 OSR"))
      (if (> (torcl-ext:deopt-count) 0) nil (error "LOOP missed overflow deopt"))
    """
    # Only loop heat can promote this activation. The overflowing increment
    # occurs near its end, after the compiler has time to publish an OSR entry.
    # One allocation halfway through also exercises GC in the grown frame.
    settings = dict(TORCL_T0_T1_THRESHOLD="1", TORCL_T1_T2_INVOKE_THRESHOLD="1000000",
                    TORCL_OSR_THRESHOLD="1000000")
    reference = run(loops, TORCL_FORCE_TIER="t0")
    native = run(loops + loop_tiers, **settings)
    stressed = run(loops + loop_tiers, TORCL_GC_STRESS="1", TORCL_GC_POISON="1", **settings)
    assert reference == native == stressed, (reference, native, stressed)
    print("s390x: live T1-to-T2 OSR, polling and late overflow preserve roots and effects", flush=True)

    allocating_loop = r"""
      (defun jit-t2-alloc-loop (n x)
        (let ((i 0) (r x))
          (block done (tagbody top
            (if (>= i n) (return-from done (list i r x)))
            (setq r (list x i)) (setq i (1+ i)) (go top)))))
      (jit-t2-alloc-loop 0 '(root)) (jit-t2-alloc-loop 0 '(root))
      (format t "~S~%" (jit-t2-alloc-loop 100 '(root)))
    """
    allocation_tier = "(if (= (torcl-ext:function-tier 'jit-t2-alloc-loop) 2) nil (error \"allocating loop missed T2\"))"
    reference = run(allocating_loop, TORCL_FORCE_TIER="t0")
    native = run(allocating_loop + allocation_tier, TORCL_FORCE_TIER="t2")
    stressed = run(allocating_loop + allocation_tier, TORCL_FORCE_TIER="t2",
                   TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
    assert reference == native == stressed, (reference, native, stressed)
    print("s390x: optimized allocating loops preserve moving-GC roots", flush=True)

    spin = r"""
      (defun jit-t2-spin (n)
        (let ((i 0)) (block done (tagbody top
          (if (= i n) (return-from done i))
          (setq i (1+ i)) (go top)))))
      (jit-t2-spin 0) (jit-t2-spin 0)
      (if (= (torcl-ext:function-tier 'jit-t2-spin) 2) nil (error "SPIN missed T2"))
      (format t "READY~%") (force-output)
      (jit-t2-spin -1)
      (format t "FELL-THROUGH~%")
    """
    # Slow allocating iterations can put sampled back-edge polls farther apart
    # than the shutdown deadline. Runtime-call boundaries must poll too.
    allocating_spin = spin.replace("(setq i (1+ i))", "(list i i) (setq i (1+ i))")
    for name, source, stress in [
        ("call-free", spin, {}),
        ("allocating stress", allocating_spin, {"TORCL_GC_STRESS": "1", "TORCL_GC_POISON": "1"}),
    ]:
        env = dict(base, TORCL_FORCE_TIER="t2", TORCL_LAZY_COMPILE="0", **stress)
        child = subprocess.Popen(command + ["--no-init", "--no-bootstrap", "--eval", source],
                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            with selectors.DefaultSelector() as ready:
                ready.register(child.stdout, selectors.EVENT_READ)
                assert ready.select(timeout=30), "optimized spin did not become ready"
                assert child.stdout.readline().strip() == "READY", "optimized spin failed before ready"
            # Let it enter the native loop; shutdown must beat the runtime's 5s
            # hard SIGTERM deadline, which alone would not prove cooperative polls.
            time.sleep(0.1)
            child.terminate()
            output, errors = child.communicate(timeout=2)
            assert child.returncode >= 0 and "FELL-THROUGH" not in output, (child.returncode, output, errors)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate()
        print(f"s390x: {name} T2 loop responds promptly to SIGTERM", flush=True)


if __name__ == "__main__":
    main()
