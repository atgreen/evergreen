#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('rpm_release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def test_tag_must_match_workspace_version(self):
        plan = release.make_plan('0.0.1', 'push', 'refs/tags/v0.0.1', '123', '1', '')
        self.assertEqual(plan['tag'], 'v0.0.1')
        self.assertFalse(plan['prerelease'])
        with self.assertRaisesRegex(ValueError, 'version'):
            release.make_plan('0.0.1', 'push', 'refs/tags/v0.0.2', '123', '1', '')

    def test_manual_test_has_unique_tag_and_lower_rpm_release(self):
        plan = release.make_plan('0.0.1', 'workflow_dispatch', 'refs/heads/main', '123', '2', 'test')
        self.assertEqual(plan['tag'], 'test-v0.0.1-123-2')
        self.assertTrue(plan['prerelease'])
        self.assertTrue(plan['publish'])
        self.assertEqual(plan['rpm_release'], '0.test.123.2')

    def test_build_only_does_not_publish(self):
        plan = release.make_plan('0.0.1', 'workflow_dispatch', 'refs/heads/main', '123', '1', 'build')
        self.assertFalse(plan['publish'])

    def test_rejects_unexpected_events_and_untrusted_identifiers(self):
        for args in [
            ('0.0.1', 'pull_request', 'refs/pull/1/merge', '123', '1', 'test'),
            ('0.0.1', 'workflow_dispatch', 'refs/heads/main', '123', '1', 'release'),
            ('0.0.1', 'workflow_dispatch', 'refs/heads/main', 'bad\noutput=true', '1', 'test'),
        ]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                release.make_plan(*args)

    def test_missing_duplicate_or_wrong_build_rpm_blocks_publication(self):
        records = [(name, '0.0.1', '0.test.123.1.fc44', 'x86_64')
                   for name in sorted(release.PACKAGES)]
        release.validate_packages(records, '0.0.1', '0.test.123.1')
        for bad in [records[:-1], records + [records[0]],
                    records[:-1] + [('egcl-static', '0.0.2', '6.fc44', 'x86_64')]]:
            with self.subTest(records=bad), self.assertRaises(ValueError):
                release.validate_packages(bad, '0.0.1', '0.test.123.1')

    def test_provenance_rejects_mixed_source_rpms_and_missing_or_duplicate_runtimes(self):
        record = {'git': 'abc', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc44',
                  'srpm_sha256': 'source-hash', 'rpms': [],
                  'artifacts': {name: name for name in release.RUNTIMES}}
        release.merge_provenance([record], 'source-hash')
        for records in ([record], [], [record, record],
                        [record | {'artifacts': {'native': 'native'}}]):
            digest = 'different-source' if records == [record] else 'source-hash'
            with self.subTest(records=records), self.assertRaises(ValueError):
                release.merge_provenance(records, digest)

    def test_collect_checks_complete_set_and_hashes_every_asset(self):
        plan = release.make_plan('0.0.1', 'workflow_dispatch', 'refs/heads/main', '123', '1', 'build')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rpms = root / 'rpms'
            rpms.mkdir()
            for name in release.PACKAGES:
                (rpms / f'{name}.rpm').write_bytes(name.encode())
            (root / 'CHANGELOG.md').write_text('Release notes\n')
            provenance = root / 'metadata/all.json'
            provenance.parent.mkdir(parents=True)
            source_rpm = root / 'egcl.src.rpm'
            source_rpm.write_bytes(b'source archive')
            provenance.write_text(json.dumps({
                'git': 'test-commit', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc44',
                'artifacts': {name: name for name in release.RUNTIMES}, 'rpms': [],
                'srpm_sha256': hashlib.sha256(source_rpm.read_bytes()).hexdigest(),
            }))

            def identity(command, **kwargs):
                if Path(command[-1]) == source_rpm:
                    return 'egcl\t0.0.1\t0.test.123.1.fc44\t1'
                return f'{Path(command[-1]).stem}\t0.0.1\t0.test.123.1.fc44\tx86_64'

            with patch.object(release, 'ROOT', root), patch.object(
                    release.subprocess, 'check_output', side_effect=identity):
                destination = root / 'assets'
                release.collect(rpms, destination, plan, source_rpm, provenance.parent)
                self.assertEqual(json.loads((destination / 'release.json').read_text()), plan)
                entries = (destination / 'SHA256SUMS').read_text().splitlines()
                self.assertEqual(len(entries), 15)
                self.assertIn(release.PUBLIC_KEY.name,
                              [entry.split('  ')[1] for entry in entries])
                for entry in entries:
                    digest, name = entry.split('  ')
                    self.assertEqual(digest, hashlib.sha256((destination / name).read_bytes()).hexdigest())
                with self.assertRaisesRegex(ValueError, 'empty'):
                    release.collect(rpms, destination, plan, source_rpm, provenance.parent)
                (rpms / 'egcl-static.rpm').unlink()
                with self.assertRaisesRegex(ValueError, '10 RPMs'):
                    release.collect(rpms, root / 'incomplete', plan, source_rpm, provenance.parent)
                self.assertFalse((root / 'incomplete').exists())


class SigningTests(unittest.TestCase):
    """rpmsign and rpmkeys are stubbed; the container probes cover the real tools."""

    def assets(self, directory, count=11):
        assets = Path(directory)
        for index in range(count):
            (assets / f'package{index}.rpm').write_bytes(f'package{index}'.encode())
        (assets / 'CHANGELOG.md').write_text('notes\n')
        (assets / 'SHA256SUMS').write_text('stale manifest\n')
        return assets

    def fake_rpm(self, report):
        def run(command, **kwargs):
            self.calls.append(command)
            return subprocess.CompletedProcess(command, 0, stdout=report, stderr='')
        return run

    def test_sign_passes_the_unattended_gpg_defines_and_rewrites_the_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = self.assets(directory)
            rpms = sorted(assets.glob('*.rpm'))
            report = ''.join(f'{rpm}: digests signatures OK\n' for rpm in rpms)
            self.calls = []
            with patch.object(release.subprocess, 'run', side_effect=self.fake_rpm(report)):
                signed = release.sign(assets, Path('/tmp/pass'), public_key=Path('/tmp/key'))
            self.assertEqual(signed, rpms)
            sign_call = self.calls[0]
            self.assertEqual(sign_call[0], 'rpmsign')
            defines = ' '.join(sign_call)
            # Ubuntu ships no /usr/bin/gpg2, and CI has no tty for a passphrase.
            self.assertIn('__gpg /usr/bin/gpg', defines)
            self.assertIn(f'_gpg_name {release.GPG_KEY_NAME}', defines)
            self.assertIn('--pinentry-mode loopback', defines)
            # SHA256SUMS must describe the SIGNED bytes, not the stale manifest.
            entries = (assets / 'SHA256SUMS').read_text().splitlines()
            self.assertEqual(len(entries), len(rpms) + 1)
            for entry in entries:
                digest, name = entry.split('  ')
                self.assertNotEqual(name, 'SHA256SUMS')
                self.assertEqual(digest, hashlib.sha256((assets / name).read_bytes()).hexdigest())

    def test_sign_refuses_an_incomplete_package_set(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = self.assets(directory, count=10)
            self.calls = []
            with patch.object(release.subprocess, 'run', side_effect=self.fake_rpm('')):
                with self.assertRaisesRegex(ValueError, '11 packages'):
                    release.sign(assets, Path('/tmp/pass'), public_key=Path('/tmp/key'))
            self.assertEqual(self.calls, [])

    def test_verify_rejects_a_package_rpmkeys_reports_only_digests_for(self):
        """`rpmkeys --checksig` exits 0 on an UNSIGNED package, so read the output."""
        rpms = [Path('/tmp/signed.rpm'), Path('/tmp/unsigned.rpm')]
        report = '/tmp/signed.rpm: digests signatures OK\n/tmp/unsigned.rpm: digests OK\n'
        self.calls = []
        with patch.object(release.subprocess, 'run', side_effect=self.fake_rpm(report)):
            with self.assertRaisesRegex(ValueError, 'unsigned.rpm'):
                release.verify_signatures(rpms, public_key=Path('/tmp/key'))
            release.verify_signatures(rpms[:1], public_key=Path('/tmp/key'))


if __name__ == '__main__':
    unittest.main()
