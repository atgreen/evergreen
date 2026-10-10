#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Exercise the CI planner against real, disposable Git histories."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PLANNER = Path(__file__).with_name("ci-plan.py")
ALL_AREAS = ["runtime", "compiler", "reader"]


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "CI planner test")
        self.git("config", "user.email", "ci@example.invalid")
        self.write("README.md", "base\n")
        self.write("crates/egcl/src/existing.rs", "base\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True).strip()

    def write(self, name, content="change\n"):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)

    def commit(self):
        self.git("add", "--all")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def plan(self, *args, base=True):
        command = [sys.executable, str(PLANNER), "--event", "pull_request"]
        if base:
            command += ["--base", self.base]
        result = subprocess.run(command + list(args), cwd=self.repo,
                                text=True, capture_output=True, check=True)
        return json.loads(result.stdout)

    def changed(self, *names):
        for name in names:
            self.write(name)
        return self.commit()

    def test_docs_only(self):
        self.changed("README.md", "CHANGELOG.md", "AGENTS.md", "CITATION.cff",
                     "docs/a guide.md", "docs/image.png")
        self.assertEqual(self.plan(), dict(runtime=True, full=False, areas=[], pgo=False))

    def test_unknown_and_spec_markdown_require_runtime(self):
        for name in ("spec/01-reader.md", "notes.md", "unknown.file", "Cargo.toml",
                     "build.rs", "scripts/check.py", ".github/workflows/ci.yml"):
            with self.subTest(path=name):
                self.changed(name)
                result = self.plan()
                self.assertTrue(result["runtime"])
                self.assertEqual(result["areas"], ALL_AREAS)
                self.assertFalse(result["full"])

    def test_mixed_docs_and_code(self):
        self.changed("docs/help.md", "crates/egcl/src/cli.rs")
        self.assertEqual(self.plan()["areas"], ["runtime"])

    def test_compiler_and_reader_areas(self):
        self.changed("crates/egcl-compiler/src/ir.rs")
        self.assertEqual(self.plan()["areas"], ["runtime", "compiler"])
        self.changed("crates/egcl-compiler/src/reader.rs")
        self.assertEqual(self.plan()["areas"], ALL_AREAS)

    def test_cli_reader_area(self):
        self.changed("crates/egcl/tests/reader_dispatch_cli.rs")
        self.assertEqual(self.plan()["areas"], ALL_AREAS)

    def test_rename_checks_both_paths(self):
        self.git("mv", "crates/egcl/src/existing.rs", "docs-renamed.md")
        self.commit()
        self.assertTrue(self.plan()["runtime"])
        # In particular, a source-to-docs rename must not become docs-only.
        self.git("reset", "--hard", self.base)
        (self.repo / "docs").mkdir()
        self.git("mv", "crates/egcl/src/existing.rs", "docs/example.md")
        self.commit()
        self.assertEqual(self.plan()["areas"], ["runtime"])

    def test_spaces_and_newlines_are_paths_not_output(self):
        self.changed("docs/a\nruntime=true.md", "docs/a space.md")
        output = self.repo / "outputs"
        result = self.plan("--output", str(output))
        self.assertFalse(result["runtime"])
        self.assertEqual(output.read_text(),
                         'runtime=false\nfull=false\nareas=[]\npgo=false\n')
        self.changed("crates/egcl/src/a\nfull=true.rs")
        self.assertTrue(self.plan()["runtime"])

    def test_missing_zero_invalid_and_untrusted_base_fail_safe(self):
        self.changed("docs/help.md")
        self.assertEqual(self.plan(base=False)["areas"], ALL_AREAS)
        for value in ("0" * 40, "f" * 40, "main", "--output=x", "a" * 40 + "\n",
                      "$(touch injected)", "HEAD; touch injected"):
            with self.subTest(base=value):
                result = self.plan("--base=" + value)
                self.assertTrue(result["runtime"])
                self.assertEqual(result["areas"], ALL_AREAS)
        self.assertFalse((self.repo / "injected").exists())

    def test_invalid_head_fails_safe(self):
        self.changed("docs/help.md")
        for value in ("0" * 40, "f" * 40, "HEAD\n", "main", "--help"):
            with self.subTest(head=value):
                self.assertTrue(self.plan("--head=" + value)["runtime"])

    def test_exact_head_ignores_working_tree(self):
        head = self.changed("docs/help.md")
        self.write("crates/egcl/src/cli.rs")
        self.assertFalse(self.plan("--head", head)["runtime"])

    def test_empty_diff_fails_safe(self):
        self.assertEqual(self.plan()["areas"], ALL_AREAS)

    def test_full_triggers(self):
        self.changed("docs/help.md")
        for event in ("schedule", "merge_group"):
            with self.subTest(event=event):
                self.assertEqual(self.plan("--event", event),
                                 dict(runtime=True, full=True, areas=ALL_AREAS, pgo=True))
        for event in ("pull_request", "push", "workflow_dispatch", "workflow_call"):
            with self.subTest(event=event):
                self.assertTrue(self.plan("--event", event, "--suite", "full")["full"])
                self.assertFalse(self.plan("--event", event)["full"])

    def test_pgo_paths(self):
        for name in ("Makefile", "scripts/build-pgo-image.sh", "scripts/pgo-workload.lisp",
                     "scripts/test-pgo-build.py", "scripts/test-pgo-workload.sh",
                     "scripts/build-image.lisp"):
            with self.subTest(path=name):
                self.git("reset", "--hard", self.base)
                self.changed(name)
                self.assertTrue(self.plan()["pgo"])
        self.git("reset", "--hard", self.base)
        self.changed("crates/egcl/src/cli.rs")
        self.assertFalse(self.plan()["pgo"])

    def test_output_matches_json_and_appends(self):
        self.changed("spec/01-reader.md")
        output = self.repo / "outputs"
        output.write_text("existing=true\n")
        result = self.plan("--output", str(output))
        lines = output.read_text().splitlines()
        self.assertEqual(lines[0], "existing=true")
        parsed = {key: json.loads(value) for key, value in
                  (line.split("=", 1) for line in lines[1:])}
        self.assertEqual(parsed, result)


if __name__ == "__main__":
    unittest.main()
