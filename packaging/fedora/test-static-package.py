#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Reject dynamically linked payloads accidentally installed as egcl-static."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('rpm_verify', Path(__file__).with_name('verify.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class StaticPackageTests(unittest.TestCase):
    def test_linkage_check_distinguishes_real_elf_binaries(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            source = work / 'main.c'
            source.write_text('int main(void) { return 0; }\n')
            dynamic = work / 'dynamic'
            static = work / 'static'
            subprocess.run(['cc', str(source), '-o', str(dynamic)], check=True)
            subprocess.run(['cc', '-nostdlib', '-static', '-fno-stack-protector',
                            '-Wl,-e,main', str(source), '-o', str(static)], check=True)
            verify.verify_linux_linkage(dynamic, static=False)
            verify.verify_linux_linkage(static, static=True)
            with self.assertRaisesRegex(RuntimeError, 'static'):
                verify.verify_linux_linkage(dynamic, static=True)
            with self.assertRaisesRegex(RuntimeError, 'glibc'):
                verify.verify_linux_linkage(static, static=False)


if __name__ == '__main__':
    unittest.main()
