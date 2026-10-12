# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
import unittest

from generated_poll import parse_trial, summarize


class TrialEvidence(unittest.TestCase):
    def setUp(self):
        self.stdout = """POLL-DISASSEMBLY-BEGIN
; POLL-GUARD-BENCH — 1 arg(s)
; T2 — profile-guided native
POLL-DISASSEMBLY-END
POLL-SAMPLE 0 100 45
POLL-SAMPLE 1 200 45
POLL-DISASSEMBLY-BEGIN
; POLL-GUARD-BENCH — 1 arg(s)
; T2 — profile-guided native
POLL-DISASSEMBLY-END
"""
        self.log = "[T2] POLL-GUARD-BENCH: optimized worker result installed with mapped T2 ABI; bytes=600"

    def parse(self, stdout=None, log=None):
        return parse_trial(self.stdout if stdout is None else stdout,
                           self.log if log is None else log, 10, 2)

    def test_retains_every_sample(self):
        self.assertEqual(self.parse(), [100, 200])

    def test_requires_named_mapped_publication(self):
        with self.assertRaises(ValueError):
            self.parse(log=self.log.replace("POLL-GUARD-BENCH", "OTHER"))

    def test_rejects_fallback_or_missing_final_disassembly(self):
        for output in (self.stdout.replace("; T2", "; T1"),
                       self.stdout.rsplit("POLL-DISASSEMBLY-BEGIN", 1)[0]):
            with self.assertRaises(ValueError):
                self.parse(stdout=output)

    def test_rejects_bad_checksum_missing_or_duplicate_sample(self):
        for output in (self.stdout.replace("200 45", "200 46"),
                       self.stdout.replace("POLL-SAMPLE 1 200 45", ""),
                       self.stdout.replace("POLL-SAMPLE 1", "POLL-SAMPLE 0")):
            with self.assertRaises(ValueError):
                self.parse(stdout=output)

    def test_reports_distribution_per_iteration(self):
        result = summarize([100, 200, 300], 10)
        self.assertEqual(result["median_ns_per_iteration"], 20)
        self.assertEqual(result["p05_ns_per_iteration"], 11)
        self.assertEqual(result["p95_ns_per_iteration"], 29)


if __name__ == "__main__":
    unittest.main()
