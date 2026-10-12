# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Parser checks only: never launch a Lisp process or measure performance."""
import unittest

from fib_self import parse_trial


class EvidenceTests(unittest.TestCase):
    def transcript(self, side="after"):
        if side == "sbcl":
            listing = "; disassembly for FIB\n; 00: L0: POP RBP\n; 05: E8FFFFFF CALL L0\n; 10: E8FFFFFF CALL L0\n"
        else:
            listing = ("FIB-CODE-BASE 4096\n; FIB\n; T2 — native\n"
                       "; Ordinary x86-64 Lisp ABI — installed native code.\n"
                       "; 128 bytes of x86-64:\n"
                       "  +0000: cmp rcx,1\n  +0004: je 0x1010\n"
                       "  +0006: mov r11,0x9999\n  +000c: jmp r11\n"
                       "  +0010: push rbp\n  +0011: mov rbp,rsp\n"
                       "  +0020: call 0x1010\n  +0030: call 0x1010\n")
        block = "FIB-DISASSEMBLY-BEGIN\n" + listing + "FIB-DISASSEMBLY-END\n"
        return block + "FIB-SAMPLE 0 123 1000 9227465\n" + block

    def test_ordinary_relative_calls_and_clock_units(self):
        parsed = parse_trial(self.transcript(), "after", 1)
        self.assertEqual(parsed["samples"][0]["seconds"], .123)
        self.assertEqual(parsed["self_call_target_offset"], 16)

    def test_sbcl_compiled_self_calls(self):
        self.assertEqual(parse_trial(self.transcript("sbcl"), "sbcl", 1)["self_call_count"], 2)

    def test_early_return_prefix_at_accepted_ordinary_entry(self):
        text = self.transcript().replace(
            "+0010: push rbp\n  +0011: mov rbp,rsp",
            "+0010: test dil,7\n  +0014: jne 0x101c\n"
            "  +0016: mov rax,rdi\n  +0019: clc\n  +001a: ret\n"
            "  +001c: push rbp\n  +001d: mov rbp,rsp")
        self.assertEqual(parse_trial(text, "after", 1)["self_call_target_offset"], 16)

    def test_reject_calls_to_unrelated_internal_prologue(self):
        text = self.transcript().replace("je 0x1010", "je 0x1040")
        text = text.replace("+0030: call 0x1010", "+0030: call 0x1010\n  +0040: push rbp")
        with self.assertRaisesRegex(ValueError, "self calls"):
            parse_trial(text, "after", 1)

    def test_reject_changed_code(self):
        with self.assertRaisesRegex(ValueError, "changed"):
            parse_trial(self.transcript().replace("+0030", "+0031", 1), "after", 1)

    def test_reject_external_calls(self):
        with self.assertRaisesRegex(ValueError, "self calls"):
            parse_trial(self.transcript().replace("0x1010", "0x9999"), "after", 1)

    def test_reject_bad_checksum(self):
        with self.assertRaisesRegex(ValueError, "sample"):
            parse_trial(self.transcript().replace("9227465", "0"), "after", 1)

    def test_reject_mapped_after(self):
        with self.assertRaisesRegex(ValueError, "ordinary"):
            parse_trial(self.transcript().replace("Ordinary x86-64 Lisp", "Mapped native transfer"), "after", 1)

    def test_before_does_not_require_self_calls(self):
        parse_trial(self.transcript().replace("0x1010", "0x9999"), "before", 1)


if __name__ == "__main__":
    unittest.main()
