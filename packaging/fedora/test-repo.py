#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
from pathlib import Path
import re
import tempfile
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('egcl_repo', Path(__file__).with_name('repo.py'))
repo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repo)

WORKFLOW = Path(__file__).resolve().parents[2] / '.github/workflows/release.yml'
REPOSITORY_WORKFLOW = WORKFLOW.with_name('repository.yml')


class PathTests(unittest.TestCase):
    def test_dist_becomes_the_directory_fc_releasever_expands_to(self):
        self.assertEqual(repo.dist_directory('.fc44'), 'fc44')
        self.assertEqual(repo.dist_directory('.fc45'), 'fc45')

    def test_a_malformed_dist_is_refused(self):
        """A bad dist would silently publish into a path no client looks in."""
        for value in ('fc44', '', '.', '.fc 44', '.fc44/../..'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                repo.dist_directory(value)

    def test_base_url_points_at_the_release_that_holds_the_packages(self):
        self.assertEqual(
            repo.base_url('atgreen/evergreen', 'v0.0.1'),
            'https://github.com/atgreen/evergreen/releases/download/v0.0.1/')


class ArchitectureTests(unittest.TestCase):
    def arches(self, names):
        directory = Path(tempfile.mkdtemp())
        for name in names:
            (directory / name).write_bytes(b'x')

        def check_output(command, **kwargs):
            return 'x86_64' if 'x86_64' in Path(command[-1]).name else 'aarch64'

        with patch.object(repo.subprocess, 'check_output', side_effect=check_output):
            return repo.architectures(sorted(directory.iterdir()))

    def test_the_source_rpm_is_not_put_in_a_binary_repository(self):
        found = self.arches(['egcl-1.x86_64.rpm', 'egcl-1.src.rpm'])
        self.assertEqual(sorted(found), ['x86_64'])
        self.assertEqual([path.name for path in found['x86_64']], ['egcl-1.x86_64.rpm'])

    def test_packages_are_grouped_per_architecture(self):
        found = self.arches(['a.x86_64.rpm', 'b.x86_64.rpm', 'c.aarch64.rpm'])
        self.assertEqual({arch: len(paths) for arch, paths in found.items()},
                         {'x86_64': 2, 'aarch64': 1})


class StoreTests(unittest.TestCase):
    def store_with(self, channel, tags):
        store = Path(tempfile.mkdtemp())
        for tag in tags:
            (store / 'per-release' / channel / 'fc44' / 'x86_64' / tag / 'repodata').mkdir(parents=True)
            # stored_releases orders by mtime, so the fixture must too.
            time.sleep(0.01)
        return store

    def test_releases_are_ordered_oldest_first(self):
        store = self.store_with('release', ['v0.0.1', 'v0.0.2', 'v0.0.3'])
        self.assertEqual(repo.stored_releases(store, 'release', '.fc44', 'x86_64'),
                         ['v0.0.1', 'v0.0.2', 'v0.0.3'])

    def test_a_directory_without_repodata_is_not_a_release(self):
        store = self.store_with('release', ['v0.0.1'])
        (store / 'per-release/release/fc44/x86_64/half-written').mkdir()
        self.assertEqual(repo.stored_releases(store, 'release', '.fc44', 'x86_64'), ['v0.0.1'])

    def test_an_empty_store_has_no_releases(self):
        self.assertEqual(repo.stored_releases(Path(tempfile.mkdtemp()),
                                              'release', '.fc44', 'x86_64'), [])

    def test_pruning_keeps_the_newest_and_drops_the_rest(self):
        store = self.store_with('test', ['one', 'two', 'three', 'four'])
        self.assertEqual(repo.prune(store, 'test', '.fc44', 'x86_64', 2), ['one', 'two'])
        self.assertEqual(repo.stored_releases(store, 'test', '.fc44', 'x86_64'),
                         ['three', 'four'])

    def test_pruning_within_the_limit_removes_nothing(self):
        store = self.store_with('test', ['one', 'two'])
        self.assertEqual(repo.prune(store, 'test', '.fc44', 'x86_64', 3), [])


class MergeTests(unittest.TestCase):
    def store_with(self, tags):
        store = Path(tempfile.mkdtemp())
        for tag in tags:
            data = store / 'per-release/release/fc44/x86_64' / tag / 'repodata'
            data.mkdir(parents=True)
            (data / 'repomd.xml').write_text(f'<repomd>{tag}</repomd>')
            time.sleep(0.01)
        return store

    def test_one_release_is_copied_rather_than_merged(self):
        """mergerepo_c needs two or more repositories; one needs no merge."""
        store = self.store_with(['v0.0.1'])
        output = Path(tempfile.mkdtemp())
        with patch.object(repo.subprocess, 'run') as run:
            destination = repo.merge(store, 'release', '.fc44', 'x86_64', output)
        run.assert_not_called()
        self.assertEqual((destination / 'repodata' / 'repomd.xml').read_text(),
                         '<repomd>v0.0.1</repomd>')

    def test_several_releases_are_merged_with_every_version_kept(self):
        store = self.store_with(['v0.0.1', 'v0.0.2'])
        output = Path(tempfile.mkdtemp())
        with patch.object(repo.subprocess, 'run') as run:
            repo.merge(store, 'release', '.fc44', 'x86_64', output)
        command = run.call_args[0][0]
        self.assertEqual(command[0], 'mergerepo_c')
        # Without --all, mergerepo_c keeps one version per name and dnf can
        # neither downgrade nor pin.
        self.assertIn('--all', command)
        self.assertEqual(sum(1 for part in command if part.startswith('--repo=')), 2)

    def test_merging_an_empty_channel_publishes_nothing(self):
        self.assertIsNone(repo.merge(Path(tempfile.mkdtemp()), 'test', '.fc44',
                                     'x86_64', Path(tempfile.mkdtemp())))


class ClientFileTests(unittest.TestCase):
    def files(self):
        output = Path(tempfile.mkdtemp())
        repo.write_client_files(output, 'atgreen/evergreen', 'https://atgreen.github.io/evergreen/')
        return {path.name: path.read_text() for path in (output / 'repo').glob('*.repo')}

    def test_each_key_is_declared_exactly_once(self):
        """Two `enabled` lines would leave the channel's state to last-wins."""
        for name, text in self.files().items():
            for key in ('enabled', 'gpgcheck', 'repo_gpgcheck', 'baseurl', 'gpgkey'):
                with self.subTest(file=name, key=key):
                    self.assertEqual(len(re.findall(rf'^{key}=', text, re.M)), 1)

    def test_the_release_channel_is_on_and_the_test_channel_is_opt_in(self):
        files = self.files()
        self.assertIn('enabled=1', files['egcl.repo'])
        self.assertIn('enabled=0', files['egcl-testing.repo'])

    def test_both_package_and_metadata_signatures_are_required(self):
        """repo_gpgcheck is what stops swapped metadata offering an older
        correctly-signed package; measured to be fatal in dnf5, not advisory."""
        for name, text in self.files().items():
            with self.subTest(file=name):
                self.assertIn('gpgcheck=1', text)
                self.assertIn('repo_gpgcheck=1', text)

    def test_dnf_variables_carry_the_architecture_and_release(self):
        """So a new architecture needs a directory, not a new .repo file."""
        for name, text in self.files().items():
            with self.subTest(file=name):
                self.assertIn('fc$releasever/$basearch', text)

    def test_the_public_key_ships_beside_the_repo_files(self):
        output = Path(tempfile.mkdtemp())
        written = repo.write_client_files(output, 'atgreen/evergreen', 'https://example.test/x')
        self.assertTrue((written / repo.release.PUBLIC_KEY.name).exists())
        self.assertIn(f'gpgkey=https://example.test/x/repo/{repo.release.PUBLIC_KEY.name}',
                      (written / 'egcl.repo').read_text())


class GenerateTests(unittest.TestCase):
    def test_an_unknown_channel_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'channel'):
            repo.generate([], Path(tempfile.mkdtemp()), 'nightly', '.fc44', 'v1',
                          'atgreen/evergreen')

    def test_only_metadata_is_kept_not_the_packages(self):
        """The store holds 16 KB per release, not 101 MiB of RPMs."""
        assets = Path(tempfile.mkdtemp())
        (assets / 'egcl-1.x86_64.rpm').write_bytes(b'package')
        store = Path(tempfile.mkdtemp())

        def check_output(command, **kwargs):
            return 'x86_64'

        def run(command, **kwargs):
            # Stand in for createrepo_c, which writes repodata/ beside the RPMs.
            (Path(command[-1]) / 'repodata').mkdir(exist_ok=True)
            return None

        with patch.object(repo.subprocess, 'check_output', side_effect=check_output), \
                patch.object(repo.subprocess, 'run', side_effect=run) as runner:
            written = repo.generate(sorted(assets.iterdir()), store, 'release', '.fc44',
                                    'v0.0.1', 'atgreen/evergreen')
        self.assertIn('--baseurl', runner.call_args[0][0])
        self.assertEqual(list(written[0].glob('*.rpm')), [])
        self.assertTrue((written[0] / 'repodata').is_dir())


class WorkflowAgreementTests(unittest.TestCase):
    # The SRPM's source tree is an allowlist (build.py source_archive) and does
    # not ship .github: CI workflows are not part of the software. So this
    # repository-level invariant cannot be checked from inside the spec's
    # %check, where it raised FileNotFoundError and failed every binaries job.
    # Skip there rather than pass silently; the release workflow's own plan job
    # runs this file on a full checkout, which is where the invariant is real.
    @unittest.skipUnless(WORKFLOW.is_file(), 'release.yml is not shipped in the source RPM')
    def test_the_test_channel_cap_matches_the_prerelease_prune(self):
        """Cross-file invariant: a channel referencing a pruned release 404s.

        release.yml deletes test prereleases beyond the newest N; this module
        caps the test channel at TEST_CHANNEL_KEEP. If they disagree, the
        published metadata points at releases that no longer exist.
        """
        workflow = WORKFLOW.read_text()
        kept = re.search(r'\|\s*\.\[(\d+):\]', workflow)
        self.assertIsNotNone(kept, 'could not find the prune slice in release.yml')
        self.assertEqual(int(kept.group(1)), repo.TEST_CHANNEL_KEEP)

    @unittest.skipUnless(WORKFLOW.is_file(), 'release.yml is not shipped in the source RPM')
    def test_publishing_a_release_dispatches_the_repository_workflow(self):
        workflow = WORKFLOW.read_text()
        self.assertIn('actions: write', workflow)
        self.assertIn('gh workflow run repository.yml', workflow)
        self.assertIn('-f tag="$RELEASE_TAG"', workflow)

    @unittest.skipUnless(WORKFLOW.is_file(), 'workflows are not shipped in the source RPM')
    def test_repository_workflow_verifies_builds_and_deploys_the_repository(self):
        self.assertTrue(REPOSITORY_WORKFLOW.is_file())
        workflow = REPOSITORY_WORKFLOW.read_text()
        for contract in (
                'gh release download',
                'sha256sum --check SHA256SUMS',
                'gpg --batch --verify',
                '--checksig assets/*.rpm',
                'python3 packaging/fedora/repo.py record',
                'python3 packaging/fedora/repo.py build',
                '.repo-store export-ignore',
                'actions/deploy-pages@',
        ):
            with self.subTest(contract=contract):
                self.assertIn(contract, workflow)


if __name__ == '__main__':
    unittest.main()
