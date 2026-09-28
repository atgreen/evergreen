#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent

def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded

apk = module('apk', 'build-apk.py')
runtime = module('runtime', 'build-runtime.py')

class BuildTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_assets_reject_reserved_index_and_symlinks(self):
        assets = self.root / 'assets'
        assets.mkdir()
        (assets / 'app.lisp').write_text('(print 42)')
        (assets / 'android.lisp').write_text('')
        self.assertEqual(set(apk.assets(self.root)), {'app.lisp', 'android.lisp'})
        link = assets / 'outside'
        link.symlink_to('/etc/passwd')
        with self.assertRaisesRegex(ValueError, 'symlinks'): apk.assets(self.root)
        link.unlink()
        (assets / 'torcl-assets.txt').write_text('bad')
        with self.assertRaisesRegex(ValueError, 'reserved'): apk.assets(self.root)

    def test_runtime_api_mismatch_fails_before_packaging(self):
        (self.root / 'runtime.json').write_text(json.dumps({'api': 3, 'version': '1'}))
        with self.assertRaisesRegex(ValueError, 'Incompatible runtime API'):
            apk.runtime_libraries(self.root, ['x86_64-linux-android'], {'runtime_api': 1})

    def test_elf_validation_checks_machine_load_and_relro_alignment(self):
        data = bytearray(64 + 2 * 56)
        data[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', data, 18, 62)
        struct.pack_into('<Q', data, 32, 64)
        struct.pack_into('<HH', data, 54, 56, 2)
        struct.pack_into('<IIQQQQQQ', data, 64, 1, 5, 0, 0, 0, 0x4000, 0x4000, 16384)
        struct.pack_into('<IIQQQQQQ', data, 120, 0x6474e552, 4, 0, 0x4000, 0, 0x4000, 0x4000, 1)
        path = self.root / 'lib.so'
        path.write_bytes(data)
        runtime.check_elf(path, 62)
        with self.assertRaisesRegex(ValueError, 'architecture'): runtime.check_elf(path, 183)
        struct.pack_into('<Q', data, 64 + 48, 4096)
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'LOAD'): runtime.check_elf(path, 62)
        struct.pack_into('<Q', data, 64 + 48, 16384)
        struct.pack_into('<Q', data, 120 + 40, 0x1000)
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'RELRO'): runtime.check_elf(path, 62)

if __name__ == '__main__': unittest.main()
