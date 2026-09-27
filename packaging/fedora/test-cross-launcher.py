#!/usr/bin/env python3
"""Check cross launchers preserve target paths and application arguments."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent

class CrossLauncherTests(unittest.TestCase):
    def test_power_uses_little_endian_qemu_and_private_sysroot(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            command = root / 'torcl-ppc64le-linux'
            shutil.copy2(HERE / 'torcl-cross', command)
            command.chmod(0o755)
            runtime = root / 'runtimes/ppc64le-linux/torcl'
            runtime.parent.mkdir(parents=True)
            runtime.touch()
            qemu = root / 'qemu-ppc64le'
            qemu.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            qemu.chmod(0o755)
            env = os.environ | {'PATH': str(root) + os.pathsep + os.environ['PATH'],
                                'TORCL_CROSS_ROOT': str(root / 'runtimes')}
            result = subprocess.run([str(command), '--eval', '(+ 19 23)'], env=env,
                                    text=True, capture_output=True, check=True)
            self.assertEqual(result.stdout.splitlines(),
                             ['-L', str(runtime.parent / 'sysroot'), str(runtime), '--eval', '(+ 19 23)'])

if __name__ == '__main__':
    unittest.main()
