#!/usr/bin/env python3
"""Independent Android/Python oracles; these are not APK build dependencies."""
import pathlib
import struct
import subprocess
import sys
import zipfile

root, tools = map(pathlib.Path, sys.argv[1:])
def run(tool, *args):
    return subprocess.check_output([str(tools / tool), *map(str, args)], stderr=subprocess.STDOUT, text=True)

fingerprints = []
for name in ('signed.apk', 'signed-again.apk', 'demo.apk'):
    apk = root / name
    if name == 'demo.apk' and not apk.exists():
        continue
    report = run('apksigner', 'verify', '--verbose', '--print-certs', apk)
    assert 'Verified using v2 scheme (APK Signature Scheme v2): true' in report, report
    fingerprint = next(line for line in report.splitlines() if 'certificate SHA-256 digest:' in line)
    fingerprints.append(fingerprint)
    print(name, fingerprint)
    run('zipalign', '-c', '4', apk)
    badging = run('aapt2', 'dump', 'badging', apk)
    assert "sdkVersion:'28'" in badging and 'android.app.NativeActivity' in badging, badging
    with zipfile.ZipFile(apk) as z, apk.open('rb') as f:
        assert z.testzip() is None
        for info in z.infolist():
            f.seek(info.header_offset + 26)
            name_len, extra_len = struct.unpack('<HH', f.read(4))
            offset = info.header_offset + 30 + name_len + extra_len
            assert offset % (16384 if info.filename.endswith('.so') else 4) == 0
        if name == 'demo.apk':
            assert 'lib/arm64-v8a/libegcl_android.so' in z.namelist()
            assert b'egcl-android-assets-v1\n' in z.read('assets/egcl-assets.txt')
            assert 'assets/scene.lisp' in z.namelist()
assert len(set(fingerprints)) == 1, 'Signing identity changed between builds'
# A byte in a signed asset must invalidate v2, even though the ZIP remains parseable.
data = bytearray((root / 'signed.apk').read_bytes())
with zipfile.ZipFile(root / 'signed.apk') as z:
    entry = z.getinfo('assets/payload.bin')
    name_len, extra_len = struct.unpack_from('<HH', data, entry.header_offset + 26)
    data[entry.header_offset + 30 + name_len + extra_len + 1048576] ^= 1
bad = root / 'tampered.apk'
bad.write_bytes(data)
result = subprocess.run([str(tools / 'apksigner'), 'verify', str(bad)], capture_output=True)
assert result.returncode != 0, 'Tampered APK accepted'
print('APK oracle checks passed; tampered payload rejected')
