#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('rpm_release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseNotesTests(unittest.TestCase):
    changelog = ('# Changelog\n\n## Unreleased\n\n- Future change.\n\n'
                 '## 0.0.2 - 2026-10-06\n\n### Fixed\n\n'
                 '- This release ([#24](https://github.com/atgreen/evergreen/pull/24)).\n\n'
                 '## 0.0.1 - 2026-10-04\n\n- Old release.\n')
    stable = {'version': '0.0.2', 'prerelease': False}

    def test_stable_notes_include_only_exact_version_section(self):
        notes = release.release_notes(self.changelog, self.stable)
        self.assertEqual(notes, '## 0.0.2 - 2026-10-06\n\n### Fixed\n\n'
                         '- This release ([#24](https://github.com/atgreen/evergreen/pull/24)).\n')
        self.assertIn('[#24](https://github.com/atgreen/evergreen/pull/24)', notes)
        self.assertNotIn('Future change', notes)
        self.assertNotIn('Old release', notes)

    def test_version_matching_is_exact_and_missing_notes_fail(self):
        for text in ('## Unreleased\n\n- Pending.\n',
                     '## 0.0.20 - 2026-10-06\n\n- Wrong version.\n'):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, '0.0.2'):
                release.release_notes(text, self.stable)

    def test_empty_and_duplicate_stable_sections_fail(self):
        for text in ('## 0.0.2\n\n## 0.0.1\n\n- Old.\n',
                     '## 0.0.2\n\n- One.\n\n## 0.0.2 - 2026-10-06\n\n- Two.\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                release.release_notes(text, self.stable)

    def test_test_and_build_plans_use_only_unreleased(self):
        for mode in ('test', 'build'):
            plan = release.make_plan('0.0.1', 'workflow_dispatch',
                                     'refs/heads/main', '123', '1', mode)
            self.assertEqual(release.release_notes(self.changelog, plan),
                             '## Unreleased\n\n- Future change.\n')
            self.assertEqual(release.release_notes('## Unreleased\n', plan),
                             '## Unreleased\n\nNo unreleased changes recorded.\n')
            with self.assertRaisesRegex(ValueError, 'Unreleased'):
                release.release_notes('## 0.0.1\n- Old.\n', plan)

    def test_fenced_examples_do_not_start_or_end_sections(self):
        for fence in ('```', '~~~~'):
            body = f'- Example:\n\n{fence}markdown\n## 0.0.1\n## 0.0.2\n{fence}\n'
            text = f'## 0.0.2\n\n{body}\n## 0.0.1\n\n- Old.\n'
            self.assertEqual(release.release_notes(text, self.stable),
                             f'## 0.0.2\n\n{body}')

    def test_plan_rejects_missing_notes_before_writing_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            version = release.RPM_RELEASE[0]
            (root / 'Cargo.toml').write_text(f'[workspace.package]\nversion = "{version}"\n')
            (root / 'CHANGELOG.md').write_text('## Unreleased\n\n- Pending.\n')
            plan = root / 'plan.json'
            output = root / 'github-output'
            environment = {'GITHUB_EVENT_NAME': 'push', 'GITHUB_REF': f'refs/tags/v{version}',
                           'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '1',
                           'GITHUB_OUTPUT': str(output)}
            with patch.object(release, 'ROOT', root), patch.dict('os.environ', environment), \
                    patch('sys.argv', ['release.py', 'plan', '--plan', str(plan)]):
                with self.assertRaisesRegex(ValueError, version):
                    release.main()
            self.assertFalse(plan.exists())
            self.assertFalse(output.exists())

    def test_workflow_publishes_selected_notes_not_entire_changelog(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        self.assertIn('--notes-file RELEASE_NOTES.md', workflow)
        self.assertNotIn('--notes-file CHANGELOG.md', workflow)


def complete_records(version='0.0.1', rpm_release='0.test.123.1', dist='.fc44'):
    """Every published package: exactly what a release must carry."""
    return [(name, version, f'{rpm_release}{dist}', arch)
            for arch, names in sorted(release.RELEASE_PACKAGES_BY_ARCH.items())
            for name in sorted(names)]


def write_rpm_tree(root):
    """A built tree in rpmbuild's shape: one RPMS/<arch>/ directory per arch.

    `egcl` and `egcl-static` exist for both architectures, so the arch has to
    be in the filename as well as the directory -- otherwise the two builds
    would collide the moment the collector copies them into one asset folder.
    """
    directories = []
    for arch, names in sorted(release.RELEASE_PACKAGES_BY_ARCH.items()):
        directory = root / 'rpms' / arch
        directory.mkdir(parents=True)
        for name in sorted(names):
            (directory / f'{name}.{arch}.rpm').write_bytes(f'{name}.{arch}'.encode())
        directories.append(directory)
    return directories


def rpm_identity(source_rpm, version='0.0.1', rpm_release='0.test.123.1.fc44'):
    """Stub `rpm -qp --queryformat`, reading name and arch back from the filename."""
    def identity(command, **kwargs):
        path = Path(command[-1])
        if path == source_rpm:
            return f'egcl\t{version}\t{rpm_release}\t1'
        name, arch = path.name.removesuffix('.rpm').rsplit('.', 1)
        return f'{name}\t{version}\t{rpm_release}\t{arch}'
    return identity


class ReleaseTests(unittest.TestCase):
    def test_release_requires_riscv_payload_and_provenance(self):
        package = 'egcl-target-riscv64-linux-static'
        self.assertIn(package, release.PACKAGES)
        self.assertIn('riscv64-linux-static', release.RUNTIMES)
        self.assertIn(('riscv64', 'riscv64', 'x86_64'), release.RELEASE_BUILDERS)
        records = [record for record in complete_records() if record[0] != package]
        with self.assertRaisesRegex(ValueError, 'Expected all 11 x86_64 RPMs'):
            release.validate_packages(records, '0.0.1', '0.test.123.1', '.fc44')

    def test_release_metadata_matches_workspace_version(self):
        root = release.ROOT
        version = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']
        self.assertEqual(release.RPM_RELEASE[0], version)
        self.assertIn(f'(defun lisp-implementation-version () "{version}")',
                      (root / 'lib/boot.lisp').read_text())
        self.assertRegex((root / 'CITATION.cff').read_text(),
                         rf'(?m)^version: {re.escape(version)}$')
        self.assertIn(f':apk-runtime-version "{version}"',
                      (root / 'examples/android-egl/android-egl.asd').read_text())
        lock = tomllib.loads((root / 'Cargo.lock').read_text())
        for package in lock['package']:
            if 'source' not in package:
                with self.subTest(package=package['name']):
                    self.assertEqual(package['version'], version)

    def test_published_architectures_match_enabled_workflow_builders(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        self.assertIn('matrix: ${{ fromJSON(needs.plan.outputs.builders_json) }}', workflow)
        self.assertIn("if: needs.plan.outputs.build_group == 'all'", workflow)
        builders = release.builder_matrix('all')['include']
        self.assertEqual({(b['name'], b['group'], b['arch']) for b in builders},
                         release.RELEASE_BUILDERS)
        self.assertEqual({b['arch'] for b in builders}, set(release.RELEASE_PACKAGES_BY_ARCH))
        for builder in builders:
            self.assertEqual(builder['timeout'], 120)
            self.assertEqual(builder['platform'], '')

    def test_partial_build_selects_only_riscv_and_cannot_publish(self):
        self.assertEqual(release.builder_matrix('riscv64'), {'include': [
            dict(name='riscv64', group='riscv64', arch='x86_64', platform='', timeout=120)]})
        plan = release.make_plan('0.0.4', 'workflow_dispatch', 'refs/heads/main',
                                 '123', '1', 'build', 'riscv64')
        self.assertFalse(plan['publish'])
        for event, mode, group in [('push', 'build', 'riscv64'),
                                   ('workflow_dispatch', 'test', 'riscv64'),
                                   ('workflow_dispatch', 'build', 'unknown')]:
            with self.subTest(event=event, mode=mode, group=group), self.assertRaises(ValueError):
                release.make_plan('0.0.4', event, 'refs/tags/v0.0.4', '123', '1', mode, group)

    def test_tag_must_match_workspace_version(self):
        version = release.RPM_RELEASE[0]
        plan = release.make_plan(version, 'push', f'refs/tags/v{version}', '123', '1', '')
        self.assertEqual(plan['tag'], f'v{version}')
        self.assertFalse(plan['prerelease'])
        with self.assertRaisesRegex(ValueError, 'version'):
            release.make_plan(version, 'push', 'refs/tags/v99.0.0', '123', '1', '')

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
        records = complete_records()
        release.validate_packages(records, '0.0.1', '0.test.123.1', '.fc44')
        for bad in [records[:-1], records + [records[0]],
                    records[:-1] + [('egcl-static', '0.0.2', '6.fc44', 'x86_64')],
                    # An unknown architecture is not quietly accepted.
                    records + [('egcl', '0.0.1', '0.test.123.1.fc44', 'riscv64')]]:
            with self.subTest(records=bad), self.assertRaises(ValueError):
                release.validate_packages(bad, '0.0.1', '0.test.123.1', '.fc44')
        # Packages built for another Fedora are not this release's packages.
        with self.assertRaises(ValueError):
            release.validate_packages(records, '0.0.1', '0.test.123.1', '.fc45')

    def test_every_architecture_must_be_present(self):
        """A release is complete or it is not published: one arch is not enough."""
        for arch in release.RELEASE_PACKAGES_BY_ARCH:
            partial = [record for record in complete_records() if record[3] != arch]
            with self.subTest(dropped=arch), \
                    self.assertRaisesRegex(ValueError, f'No packages at all for: {arch}'):
                release.validate_packages(partial, '0.0.1', '0.test.123.1', '.fc44')

    def test_provenance_rejects_mixed_source_rpms_and_missing_or_duplicate_runtimes(self):
        record = {'git': 'abc', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc44',
                  'dist': '.fc44', 'srpm_sha256': 'source-hash', 'rpms': [],
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
            rpms = write_rpm_tree(root)
            changelog = '## Unreleased\n\n- Pending.\n\n## 0.0.1\n\n- Old.\n'
            (root / 'CHANGELOG.md').write_text(changelog)
            provenance = root / 'metadata/all.json'
            provenance.parent.mkdir(parents=True)
            source_rpm = root / 'egcl.src.rpm'
            source_rpm.write_bytes(b'source archive')
            provenance.write_text(json.dumps({
                'git': 'test-commit', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc44',
                'dist': '.fc44',
                'artifacts': {name: name for name in release.RUNTIMES}, 'rpms': [],
                'srpm_sha256': hashlib.sha256(source_rpm.read_bytes()).hexdigest(),
            }))

            with patch.object(release, 'ROOT', root), patch.object(
                    release.subprocess, 'check_output',
                    side_effect=rpm_identity(source_rpm)):
                destination = root / 'assets'
                release.collect(rpms, destination, plan, source_rpm, provenance.parent)
                self.assertEqual(json.loads((destination / 'release.json').read_text()), plan)
                self.assertEqual((destination / 'CHANGELOG.md').read_text(), changelog)
                self.assertEqual((destination / 'RELEASE_NOTES.md').read_text(),
                                 '## Unreleased\n\n- Pending.\n')
                entries = (destination / 'SHA256SUMS').read_text().splitlines()
                # Every binary RPM, the SRPM, CHANGELOG, release notes, the public key,
                # build.json and release.json.
                self.assertEqual(len(entries), release.PACKAGE_COUNT + 6)
                self.assertIn(release.PUBLIC_KEY.name,
                              [entry.split('  ')[1] for entry in entries])
                for entry in entries:
                    digest, name = entry.split('  ')
                    self.assertEqual(digest, hashlib.sha256((destination / name).read_bytes()).hexdigest())
                with self.assertRaisesRegex(ValueError, 'empty'):
                    release.collect(rpms, destination, plan, source_rpm, provenance.parent)
                (root / 'rpms/x86_64/egcl-static.x86_64.rpm').unlink()
                with self.assertRaisesRegex(ValueError, '11 x86_64 RPMs'):
                    release.collect(rpms, root / 'incomplete', plan, source_rpm, provenance.parent)
                self.assertFalse((root / 'incomplete').exists())


    def test_an_sbom_is_carried_into_the_release_and_checksummed(self):
        plan = release.make_plan('0.0.1', 'workflow_dispatch', 'refs/heads/main', '123', '1', 'build')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rpms = write_rpm_tree(root)
            (root / 'CHANGELOG.md').write_text('## Unreleased\n\n- Notes.\n')
            source_rpm = root / 'egcl.src.rpm'
            source_rpm.write_bytes(b'source archive')
            sbom = root / 'egcl-sbom.cdx.json'
            sbom.write_text('{"bomFormat": "CycloneDX"}\n')
            provenance = root / 'metadata/all.json'
            provenance.parent.mkdir(parents=True)
            provenance.write_text(json.dumps({
                'git': 'c', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc44', 'dist': '.fc44',
                'artifacts': {name: name for name in release.RUNTIMES}, 'rpms': [],
                'srpm_sha256': hashlib.sha256(source_rpm.read_bytes()).hexdigest()}))

            with patch.object(release, 'ROOT', root), patch.object(
                    release.subprocess, 'check_output',
                    side_effect=rpm_identity(source_rpm)):
                destination = root / 'assets'
                release.collect(rpms, destination, plan, source_rpm, provenance.parent, sbom)
            manifest = dict(line.split('  ')[::-1]
                            for line in (destination / 'SHA256SUMS').read_text().splitlines())
            # In the manifest, so it is covered by SHA256SUMS.asc as well.
            self.assertIn(sbom.name, manifest)
            self.assertEqual(manifest[sbom.name],
                             hashlib.sha256(sbom.read_bytes()).hexdigest())
            # As the no-SBOM case, plus the SBOM itself.
            self.assertEqual(len(manifest), release.PACKAGE_COUNT + 7)

    def test_stable_release_is_paired_with_its_version(self):
        version, number = release.RPM_RELEASE
        self.assertEqual(release.stable_release(version), number)
        # A version bump must fail loudly rather than reuse the old release.
        with self.assertRaisesRegex(ValueError, 'resets Release'):
            release.stable_release('99.0.0')
        with self.assertRaisesRegex(ValueError, 'resets Release'):
            release.make_plan('99.0.0', 'push', 'refs/tags/v99.0.0', '1', '1', '')

    def test_spec_fallback_release_matches_release_py(self):
        """The two must not drift: a local rpmbuild would produce a different NVR."""
        spec = Path(__file__).with_name('egcl.spec').read_text()
        self.assertIn(f'%{{!?egcl_release:%global egcl_release {release.RPM_RELEASE[1]}}}', spec)

    def test_dist_comes_from_the_builders_not_a_literal(self):
        """A Fedora 45 build must validate against .fc45 without a code change."""
        record = {'git': 'abc', 'rustc': 'rustc 1.94.1', 'sysroot_release': 'fc45',
                  'dist': '.fc45', 'srpm_sha256': 'h', 'rpms': [],
                  'artifacts': {name: name for name in release.RUNTIMES}}
        self.assertEqual(release.merge_provenance([record], 'h')['dist'], '.fc45')
        release.validate_packages(complete_records('0.0.1', '6', '.fc45'), '0.0.1', '6', '.fc45')
        # Builders that disagree about their environment must not be merged.
        with self.assertRaisesRegex(ValueError, 'dist'):
            release.merge_provenance([record, record | {'dist': '.fc44'}], 'h')


class SigningTests(unittest.TestCase):
    """rpmsign and rpmkeys are stubbed; the container probes cover the real tools."""

    def assets(self, directory, count=None):
        # The signer expects every binary RPM of every architecture plus the
        # shared SRPM, so derive the count rather than restating it.
        count = release.PACKAGE_COUNT + 1 if count is None else count
        assets = Path(directory)
        for index in range(count):
            (assets / f'package{index}.rpm').write_bytes(f'package{index}'.encode())
        (assets / 'CHANGELOG.md').write_text('notes\n')
        (assets / 'SHA256SUMS').write_text('stale manifest\n')
        return assets

    def fake_rpm(self, report, assets=None):
        def run(command, **kwargs):
            self.calls.append(command)
            # Stand in for gpg --detach-sign, which writes its output file.
            if '--detach-sign' in command:
                Path(command[command.index('--output') + 1]).write_text('signature\n')
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
            self.assertTrue((assets / 'SHA256SUMS.asc').exists())
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
                with self.assertRaisesRegex(ValueError, f'{release.PACKAGE_COUNT + 1} packages'):
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


    def test_manifest_signature_is_verified_against_only_the_public_key(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            (assets / 'SHA256SUMS').write_text('digest  file\n')
            self.calls = []
            with patch.object(release.subprocess, 'run', side_effect=self.fake_rpm('')):
                signature = release.sign_manifest(assets, Path('/tmp/pass'),
                                                  public_key=Path('/tmp/key'))
            self.assertEqual(signature, assets / 'SHA256SUMS.asc')
            detach, import_key, verify = self.calls
            self.assertIn('--detach-sign', detach)
            self.assertIn(release.GPG_KEY_NAME, detach)
            self.assertIn('--pinentry-mode', detach)
            # Verification must run in a throwaway GNUPGHOME holding one key,
            # or it would also pass for a signature made by any other key in
            # the signing keyring.
            self.assertIn('--import', import_key)
            self.assertIn('--verify', verify)

    def test_a_failing_manifest_verification_is_not_swallowed(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            (assets / 'SHA256SUMS').write_text('digest  file\n')

            def run(command, **kwargs):
                if '--detach-sign' in command:
                    Path(command[command.index('--output') + 1]).write_text('bad\n')
                    return subprocess.CompletedProcess(command, 0, stdout='', stderr='')
                if '--verify' in command:
                    raise subprocess.CalledProcessError(1, command)
                return subprocess.CompletedProcess(command, 0, stdout='', stderr='')

            with patch.object(release.subprocess, 'run', side_effect=run):
                with self.assertRaises(subprocess.CalledProcessError):
                    release.sign_manifest(assets, Path('/tmp/pass'), public_key=Path('/tmp/key'))


if __name__ == '__main__':
    unittest.main()
