#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Executable contracts for the explicit coverage-debt policy."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("spec-coverage.py")


class DebtGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "spec").mkdir()
        (self.root / "crates/example/tests").mkdir(parents=True)
        (self.root / "spec/stages.json").write_text(json.dumps({
            "current_stage": 5, "files": {"requirements.md": 5}, "stages": []
        }))
        (self.root / "spec/requirements.md").write_text(
            "| R1.01 | first capability | MUST |\n"
            "| R1.02 | second capability | MUST |\n"
            "| R1.03 | future capability [S6] | MUST |\n"
            "| R1.04 | optional capability | SHOULD |\n"
        )
        self.citations = self.root / "crates/example/tests/check.rs"
        self.citations.write_text("R1.01")
        self.baseline = self.root / "spec/coverage-debt.json"
        self.entry = {
            "requirement": "R1.02", "bead": "bliss-example",
            "kind": "implementation", "reason": "The behavior remains unimplemented."
        }
        self.write_baseline([self.entry])

    def write_baseline(self, entries):
        self.baseline.write_text(json.dumps({"version": 1, "entries": entries}))

    def run_gate(self, *extra, baseline=True):
        command = [sys.executable, str(SCRIPT), "--repo", str(self.root), "--gate"]
        if baseline:
            command += ["--debt-baseline", "spec/coverage-debt.json"]
        return subprocess.run(command + list(extra), text=True, capture_output=True)

    def test_known_debt_passes_without_becoming_coverage(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1/2 (50%)", result.stdout)
        self.assertIn("in-scope UNCOVERED : 1", result.stdout)
        self.assertIn("known debt         : 1 (not covered)", result.stdout)
        self.assertIn("R1.02 [implementation] bliss-example", result.stdout)
        self.assertIn("PASS WITH DEBT", result.stdout)

    def test_default_gate_remains_strict(self):
        result = self.run_gate(baseline=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("GATE FAILED", result.stderr)

    def test_new_gap_fails(self):
        self.citations.write_text("")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("R1.01", result.stderr)

    def test_fixed_debt_must_be_removed(self):
        self.citations.write_text("R1.01 R1.02")
        result = self.run_gate()
        self.assertEqual(result.returncode, 2)
        self.assertIn("stale", result.stderr)
        self.write_baseline([])
        self.assertEqual(self.run_gate().returncode, 0)
        self.citations.write_text("R1.01")
        self.assertEqual(self.run_gate().returncode, 1)

    def test_unknown_optional_and_deferred_entries_are_rejected(self):
        for rid in ["R9.99", "R1.04", "R1.03"]:
            with self.subTest(rid=rid):
                self.write_baseline([{**self.entry, "requirement": rid}])
                self.assertEqual(self.run_gate().returncode, 2)

    def test_invalid_schema_and_duplicates_are_rejected(self):
        invalid = [
            {"version": 2, "entries": [self.entry]},
            {"version": True, "entries": [self.entry]},
            {"version": 1, "entries": [self.entry, self.entry]},
            {"version": 1, "entries": [{**self.entry, "bead": ""}]},
            {"version": 1, "entries": [{**self.entry, "reason": " "}]},
            {"version": 1, "entries": [{**self.entry, "kind": "covered"}]},
            {"version": 1, "entries": [{**self.entry, "extra": "typo"}]},
            {"version": 1, "entries": ["R1.02"]},
            {"version": 1, "entries": {}},
        ]
        for data in invalid:
            with self.subTest(data=data):
                self.baseline.write_text(json.dumps(data))
                result = self.run_gate()
                self.assertEqual(result.returncode, 2)
                self.assertIn("invalid debt baseline", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_missing_or_malformed_baseline_is_not_ignored(self):
        self.baseline.unlink()
        self.assertEqual(self.run_gate().returncode, 2)
        self.baseline.write_text("{")
        self.assertEqual(self.run_gate().returncode, 2)

    def test_stage_override_does_not_expand_debt(self):
        result = self.run_gate("--stage", "6")
        self.assertEqual(result.returncode, 1)
        self.assertIn("R1.03", result.stderr)
        self.assertEqual(self.run_gate("--stage", "4").returncode, 0)

    def test_whole_scope_gate_still_rejects_future_gaps(self):
        result = self.run_gate("--all")
        self.assertEqual(result.returncode, 1)
        self.assertIn("R1.03", result.stderr)


if __name__ == "__main__":
    unittest.main()
