#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent

class GeneratorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.project = Path(self.tmp.name) / 'hello'

    def generate(self, *args):
        return subprocess.run(['python3', str(ROOT / 'egcl-android-new'), str(self.project), *args], text=True, capture_output=True)

    def test_host_and_identity_are_written(self):
        result = self.generate('--host=x86_64-linux-android', '--package=org.example.hello', '--name=Hello & World')
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads((self.project / 'app.json').read_text())
        self.assertEqual(config['host'], 'x86_64-linux-android')
        self.assertEqual(config['package'], 'org.example.hello')
        self.assertEqual(config['name'], 'Hello & World')
        self.assertTrue((self.project / 'assets/app.lisp').is_file())
        self.assertIn('HOST', (self.project / 'Makefile').read_text())

    def test_existing_directory_is_never_overwritten(self):
        self.project.mkdir()
        sentinel = self.project / 'app.json'
        sentinel.write_text('keep me')
        self.assertNotEqual(self.generate().returncode, 0)
        self.assertEqual(sentinel.read_text(), 'keep me')

    def test_bad_host_and_package_create_nothing(self):
        for args in [('--host=s390x-linux-gnu',), ('--package=bad/name',)]:
            self.assertNotEqual(self.generate(*args).returncode, 0)
            self.assertFalse(self.project.exists())

    def test_invalid_runtime_path_leaves_no_partial_project(self):
        result = self.generate('--runtime=/missing/$runtime')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.project.exists())

    def test_minimal_template_and_missing_dependencies(self):
        result = self.generate('--template=minimal')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('(load "egl.lisp")', (self.project / 'assets/app.lisp').read_text())
        result = subprocess.run(['make', 'doctor', 'RUNTIME=/nonexistent', 'SDK=/nonexistent'], cwd=self.project, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('runtime', (result.stdout + result.stderr).lower())

if __name__ == '__main__': unittest.main()
