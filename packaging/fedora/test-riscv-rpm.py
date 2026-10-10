#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Exercise the RISC-V cross-build plumbing without downloading or compiling."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent


def load(filename):
    spec = importlib.util.spec_from_file_location(filename, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


build = load('build.py')
verify = load('verify.py')


class RiscvRpmTests(unittest.TestCase):
    def test_cross_build_stages_and_verifies_the_same_static_runtime(self):
        target = 'riscv64-linux-static'
        triple = 'riscv64gc-unknown-linux-musl'
        self.assertEqual(build.TARGETS[target], triple)
        self.assertEqual(verify.RUST_ARCH['riscv64'], 'riscv64gc')
        self.assertIn(':riscv64', verify.TARGETS[target])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'SOURCE-REVISION').write_text('riscv-fixture-source')
            (root / 'docs').mkdir()
            (root / 'docs/fedora-rpm.md').write_text('fixture documentation')
            runtime = root / 'target' / triple / 'release/egcl'
            runtime.parent.mkdir(parents=True)
            runtime.write_bytes(b'fixture unstripped runtime')
            args = argparse.Namespace(output=root / 'output', tools=root / 'tools',
                                      android_ndk=root / 'unused-ndk', target=[target],
                                      sysroot_release='fc44')
            calls = []

            def run(command, *, env=None, **kwargs):
                command = list(map(str, command))
                calls.append((command, env))
                if 'EGCL_IMAGE_OUT' in (env or {}):
                    Path(env['EGCL_IMAGE_OUT']).write_bytes(b'fixture saved runtime')

            def metadata(command, **kwargs):
                return '.fc44' if command[0] == 'rpm' else 'rustc fixture'

            with patch.object(build, 'ROOT', root), patch.object(build, 'run', run), \
                    patch.object(build.subprocess, 'check_output', metadata):
                stage = build.build(args)
            cargo, environment = calls[0]
            self.assertEqual(cargo[1:7], ['cargo', 'build', '--locked', '--release', '--target', triple])
            self.assertNotIn('c-ffi', cargo)
            self.assertEqual(environment['RUSTFLAGS'], '-C target-feature=+crt-static')
            linker = str(args.output / 'riscv64-static-link')
            self.assertEqual(environment['CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_LINKER'], linker)
            self.assertEqual(environment[f'CC_{triple}'], linker)
            self.assertIn('riscv64-linux-gnu-gcc', Path(linker).read_text())
            self.assertTrue(calls[1][0][0].endswith('/riscv64-linux-gnu-strip'))
            self.assertEqual(calls[2][0][1], 'qemu-riscv64')
            self.assertEqual(calls[-1][0][-2:], ['--target', target])
            self.assertTrue((stage / f'usr/bin/egcl-{target}').is_file())
            payload = stage / f'usr/libexec/egcl/{target}/egcl'
            provenance = json.loads((stage / 'usr/share/doc/egcl/build.json').read_text())
            self.assertEqual(provenance['artifacts'],
                             {target: hashlib.sha256(payload.read_bytes()).hexdigest()})
            self.assertEqual(provenance['git'], 'riscv-fixture-source')

    def test_tool_preparation_checks_signed_cross_tools_without_glibc_sysroot(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / 'commands'
            commands = root / 'bin'
            commands.mkdir()
            stub = '''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
name = Path(sys.argv[0]).name
with open(os.environ['COMMAND_LOG'], 'a') as log:
    log.write(name + ' ' + ' '.join(sys.argv[1:]) + '\\n')
if name == 'dnf':
    destination = Path(next(arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--destdir=')))
    for package in sys.argv[sys.argv.index('download') + 1:]:
        if not package.startswith('-'):
            (destination / (package + '-1.x86_64.rpm')).touch()
'''
            for command in ('dnf', 'rpmkeys', 'rpm2cpio', 'cpio'):
                executable = commands / command
                executable.write_text(stub)
                executable.chmod(0o755)
            env = os.environ | {'PATH': str(commands) + os.pathsep + os.environ['PATH'],
                                'COMMAND_LOG': str(log), 'EGCL_RPM_TOOLS': str(root / 'tools')}
            subprocess.run(['bash', str(HERE / 'prepare-tools.sh'), 'riscv64'],
                           env=env, check=True, capture_output=True, text=True)
            lines = log.read_text().splitlines()
            self.assertEqual(len([line for line in lines if line.startswith('dnf ')]), 1)
            self.assertIn('gcc-riscv64-linux-gnu binutils-riscv64-linux-gnu', lines[0])
            self.assertNotIn('sysroot-', '\n'.join(lines))
            self.assertNotIn('libgcc', '\n'.join(lines))
            self.assertEqual(len([line for line in lines if line.startswith('rpmkeys --checksig ')]), 2)
            self.assertEqual(len([line for line in lines if line.startswith('rpm2cpio ')]), 2)


if __name__ == '__main__':
    unittest.main()
