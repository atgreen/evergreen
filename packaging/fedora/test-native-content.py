#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""The installed Java system must relocate with an extracted native RPM."""
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('native_content', HERE / 'native-content.py')
content = importlib.util.module_from_spec(spec)
spec.loader.exec_module(content)


class NativeContentTests(unittest.TestCase):
    def test_installs_complete_system_and_relocatable_bridge_without_build_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            (source / 'scripts').mkdir(parents=True)
            shutil.copy2(HERE.parents[1] / 'scripts/install-egcl-forks',
                         source / 'scripts/install-egcl-forks')
            system = source / 'lib/egcl-jvm'
            (system / 'build').mkdir(parents=True)
            for name in ('egcl-jvm.asd', 'package.lisp', 'jvm.lisp', 'api.lisp'):
                (system / name).write_text(name)
            (system / 'build/libegcl_jvm.so').write_bytes(b'native bridge')
            (system / 'Makefile').write_text('must not ship')
            (system / 'native.c').write_text('must not ship')
            deliver = source / 'lib/egcl-deliver'
            deliver.mkdir(parents=True)
            for name in ('egcl-deliver-asdf.asd', 'asdf-integration.lisp', 'README.md'):
                (deliver / name).write_text(name)
            manual = root / 'manual'
            (manual / 'java').mkdir(parents=True)
            (manual / 'assets').mkdir()
            (manual / 'index.html').write_text('manual')
            (manual / 'java/index.html').write_text('Java API')
            (manual / 'assets/local.css').write_text('local assets')
            stage = root / 'stage'
            content.install(source, manual, stage, libdir='/usr/lib64', datadir='/usr/share', docdir='/usr/share/doc')
            installed = stage / 'usr/share/common-lisp/source/egcl-jvm'
            self.assertEqual({p.name for p in installed.iterdir()},
                             {'egcl-jvm.asd', 'package.lisp', 'jvm.lisp', 'api.lisp', 'libegcl_jvm.so'})
            self.assertTrue((installed / 'libegcl_jvm.so').is_symlink())
            self.assertEqual({p.name for p in (stage / 'usr/share/common-lisp/source/egcl-deliver').iterdir()},
                             {'egcl-deliver-asdf.asd', 'asdf-integration.lisp', 'README.md'})
            self.assertFalse((installed / 'libegcl_jvm.so').readlink().is_absolute())
            relocated = root / 'extracted'
            stage.rename(relocated)
            self.assertEqual((relocated / 'usr/share/common-lisp/source/egcl-jvm/libegcl_jvm.so').read_bytes(), b'native bridge')
            self.assertEqual((relocated / 'usr/share/doc/egcl/manual/java/index.html').read_text(), 'Java API')
            self.assertTrue((relocated / 'usr/share/doc/egcl/manual/assets/local.css').is_file())
            installer = relocated / 'usr/bin/install-egcl-forks'
            # Fedora's brp-mangle-shebangs must leave the verified stage intact.
            self.assertEqual(installer.read_bytes().splitlines()[0], b'#!/usr/bin/sh')
            result = subprocess.run([str(installer), '--dry-run', str(root)],
                                    text=True, capture_output=True, check=True)
            self.assertIn('ocicl install git+https://github.com/atgreen/', result.stdout)
            self.assertFalse((root / 'ocicl.csv').exists())


if __name__ == '__main__':
    unittest.main()
