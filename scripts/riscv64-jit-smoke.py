#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Verify riscv64 native T1 execution, OSR and moving-GC safety through the CLI.

Run natively on RV64 hardware (or under qemu-riscv64), passing the binary or
QEMU command after --. The tier assertions are essential: correct bytecode
fallback is not a JIT pass. The T2 sections of scripts/s390x-jit-smoke.py are
deliberately absent: riscv64 has no optimizing emitter yet (bliss-miro8.3).
"""
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


PROGRAM = r"""
(defun jit-check (x) (if x t (error "riscv64 JIT check failed")))
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
(defun jit-compare (a b) (list (< a b) (> a b) (<= a b) (>= a b) (= a b)))
(setq *jit-effects* 0)
(defun jit-overflow (x)
  (setq *jit-effects* (1+ *jit-effects*)) (+ x 1))
(defun jit-underflow (x) (- x 1))
(defun jit-negate (x) (- x))
(defun jit-error () (car 17) (setq *jit-effects* 99))
(format t "~S~%" (jit-sum 1000))
(format t "~S~%" (jit-roots))
(format t "~S~%" (jit-wide 1 2 3 4 5 6 7 8))
(format t "~S~%" (jit-recursive 8))
(format t "~S~%" (list (jit-compare 1 2) (jit-compare 2 2) (jit-compare 3 2)))
(format t "~S~%" (jit-overflow 1152921504606846975))
(format t "~S~%" (list (jit-underflow -1152921504606846976) (jit-negate -1152921504606846976)))
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
        raise SystemExit("usage: riscv64-jit-smoke.py -- [qemu-riscv64 -L sysroot] egcl")
    base = {k: v for k, v in os.environ.items() if not k.startswith("EGCL_")}

    def run(source, **settings):
        env = dict(base, EGCL_LAZY_COMPILE="0", EGCL_T1_T2_BACKEDGE_THRESHOLD="4", **settings)
        # --eval prints the last form's value. Give every variant the same
        # final value even when it appends extra tier assertions.
        result = subprocess.run(command + ["--no-init", "--no-bootstrap", "--eval", source + "\nnil"],
                                env=env, capture_output=True, text=True, timeout=300)
        if result.returncode:
            raise RuntimeError(f"exit {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result.stdout

    reference = run(PROGRAM, EGCL_FORCE_TIER="t0")
    assert "JIT-OK" in reference, reference
    native_checks = """
      (jit-check (= (egcl-ext:function-tier 'jit-sum) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-roots) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-wide) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-recursive) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-compare) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-overflow) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-underflow) 1))
      (jit-check (= (egcl-ext:function-tier 'jit-negate) 1))
      (jit-check (> (egcl-ext:deopt-count) 0))
    """
    native = run(PROGRAM + native_checks, EGCL_FORCE_TIER="t1")
    stressed = run(PROGRAM + native_checks, EGCL_FORCE_TIER="t1",
                   EGCL_GC_STRESS="1", EGCL_GC_POISON="1")
    assert native == stressed == reference, (reference, native, stressed)
    print("riscv64: native T1, deopt, calls, errors and GC stress match T0", flush=True)

    # Keep invocation promotion cold so this must enter through an OSR stub.
    osr = PROGRAM + "(jit-check (> (egcl-ext:function-osr-count 'jit-sum) 0))"
    output = run(osr, EGCL_T0_T1_THRESHOLD="1000000", EGCL_OSR_THRESHOLD="2",
                 EGCL_DISABLE_T2="1")
    stressed = run(osr, EGCL_T0_T1_THRESHOLD="1000000", EGCL_OSR_THRESHOLD="2",
                   EGCL_DISABLE_T2="1", EGCL_GC_STRESS="1", EGCL_GC_POISON="1")
    assert output == stressed == reference, (reference, output, stressed)
    print("riscv64: live T0-to-native OSR and GC stress match T0", flush=True)

    trap = (Path(__file__).resolve().parents[1] /
            "crates/egcl/tests/fixtures/native-transfer-osr.lisp").read_text()
    ordinary = run(trap, EGCL_T0_T1_THRESHOLD="1000000", EGCL_OSR_THRESHOLD="2",
                   EGCL_DISABLE_T2="1", EGCL_OSR_TRAPS="1")
    stressed = run(trap, EGCL_T0_T1_THRESHOLD="1000000", EGCL_OSR_THRESHOLD="2",
                   EGCL_DISABLE_T2="1", EGCL_OSR_TRAPS="1",
                   EGCL_GC_STRESS="1", EGCL_GC_POISON="1")
    assert ordinary == stressed and "OSR-TRAP-OK" in ordinary, (ordinary, stressed)
    print("riscv64: OSR overflow and uncommon traps preserve active handlers", flush=True)

    auto = PROGRAM + """
      (jit-sum 10) (jit-sum 10)
      (jit-check (= (egcl-ext:function-tier 'jit-sum) 1))
    """
    assert run(auto, EGCL_T0_T1_THRESHOLD="2", EGCL_DISABLE_T2="1") == reference
    print("riscv64: automatic invocation promotion reaches T1", flush=True)

    with tempfile.TemporaryDirectory(prefix="egcl-riscv64-jit-") as directory:
        dump_path = Path(directory) / "jit.dump"
        listing = run("""
          (defun jit-diagnostics (x) (+ x 1))
          (jit-diagnostics 2)
          (if (= (egcl-ext:function-tier 'jit-diagnostics) 1) nil
              (error "diagnostic fixture did not compile"))
          (disassemble 'jit-diagnostics)
        """, EGCL_FORCE_TIER="t1", EGCL_PERF_JITDUMP=str(dump_path))
        assert "bytes of riscv64" in listing and "bytes of x86" not in listing, listing
        # The prologue's first word is `addi sp, sp, -48`.
        assert "+0000:" in listing and ".byte 0x13, 0x01, 0x01, 0xfd" in listing, listing
        dump = dump_path.read_bytes()
        magic, version, header_size, machine = struct.unpack_from("<IIII", dump)
        assert (magic, version, header_size, machine) == (0x4A695444, 1, 40, 243)
        listed_bytes = bytearray()
        for line in listing.splitlines():
            offset, separator, byte_text = line.partition(":  .byte ")
            if separator:
                assert int(offset.strip().removeprefix("+"), 16) == len(listed_bytes)
                listed_bytes.extend(int(value, 16) for value in byte_text.split(", "))
        at = header_size
        found = False
        while at + 56 <= len(dump):
            record, size = struct.unpack_from("<II", dump, at)
            assert record == 0 and size > 56 and at + size <= len(dump)
            name, _, code = dump[at + 56:at + size].partition(b"\x00")
            if b"JIT-DIAGNOSTICS" in name:
                assert listed_bytes == code, (listed_bytes, code)
                found = True
            at += size
        assert found, "no code-load record for JIT-DIAGNOSTICS"
    print("riscv64: native listings and perf jitdump identify RISC-V", flush=True)


if __name__ == "__main__":
    main()
