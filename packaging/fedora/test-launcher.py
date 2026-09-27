#!/usr/bin/env python3
"""Exercise the installed launcher with recording emulator executables."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).with_name('torcl-cross')


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='torcl launcher ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=f'{self.bin}:/usr/bin:/bin',
                        TORCL_CROSS_ROOT=str(self.root / 'targets'),
                        XDG_DATA_HOME=str(self.root / 'data'),
                        RECORD=str(self.root / 'record'))
        self.env.pop('WINEPREFIX', None)
        for tool in ['qemu-s390x', 'qemu-aarch64', 'wine']:
            path = self.bin / tool
            path.write_text('#!/usr/bin/python3\nimport json, os, sys\n'
                            'open(os.environ["RECORD"], "w").write(json.dumps('
                            '[sys.argv, os.environ.get("WINEPREFIX")]))\n')
            path.chmod(0o755)

    def run_target(self, target, args=(), present=True):
        launcher = self.bin / f'torcl-{target}'
        shutil.copyfile(SOURCE, launcher)
        launcher.chmod(0o755)
        directory = self.root / 'targets' / target
        directory.mkdir(parents=True)
        if present:
            (directory / ('torcl.exe' if target == 'windows' else 'torcl')).touch()
        return subprocess.run([str(launcher), *args], env=self.env,
                              text=True, capture_output=True)

    def test_linux_preserves_arguments_and_selects_sysroot(self):
        for target, emulator in [('s390x-linux', 'qemu-s390x'),
                                 ('aarch64-linux', 'qemu-aarch64')]:
            with self.subTest(target=target):
                args = ['--load', 'a file.lisp', '--', 'literal $HOME; x']
                result = self.run_target(target, args)
                self.assertEqual(result.returncode, 0, result.stderr)
                argv, _ = json.loads((self.root / 'record').read_text())
                base = self.root / 'targets' / target
                self.assertEqual(argv, [str(self.bin / emulator), '-L',
                                        str(base / 'sysroot'), str(base / 'torcl'), *args])

    def test_android_static_needs_no_sysroot(self):
        result = self.run_target('android', ['--eval', '(+ 1 2)'])
        self.assertEqual(result.returncode, 0, result.stderr)
        argv, _ = json.loads((self.root / 'record').read_text())
        self.assertNotIn('-L', argv)
        self.assertEqual(argv[-2:], ['--eval', '(+ 1 2)'])

    def test_windows_uses_private_prefix(self):
        result = self.run_target('windows')
        self.assertEqual(result.returncode, 0, result.stderr)
        argv, prefix = json.loads((self.root / 'record').read_text())
        self.assertTrue(argv[1].endswith('/windows/torcl.exe'))
        self.assertEqual(prefix, str(self.root / 'data/torcl/wine'))

    def test_windows_respects_explicit_prefix(self):
        self.env['WINEPREFIX'] = str(self.root / 'custom wine')
        result = self.run_target('windows')
        self.assertEqual(result.returncode, 0, result.stderr)
        _, prefix = json.loads((self.root / 'record').read_text())
        self.assertEqual(prefix, self.env['WINEPREFIX'])

    def test_missing_emulator_is_actionable(self):
        (self.bin / 'qemu-s390x').unlink()
        self.env['PATH'] = str(self.bin)
        result = self.run_target('s390x-linux')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Missing qemu-s390x', result.stderr)

    def test_emulator_exit_status_is_preserved(self):
        (self.bin / 'qemu-s390x').write_text('#!/bin/sh\nexit 23\n')
        result = self.run_target('s390x-linux')
        self.assertEqual(result.returncode, 23)

    def test_missing_runtime_is_actionable(self):
        result = self.run_target('s390x-linux', present=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('runtime', result.stderr)
        self.assertFalse((self.root / 'record').exists())


if __name__ == '__main__':
    unittest.main()
