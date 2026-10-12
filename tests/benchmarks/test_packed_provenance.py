#!/usr/bin/env python3
"""Synthetic transport fixtures test rejection; they are not CI receipts."""
import copy
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile

import packed_kernel as bench


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.name = 'target/x86_64-unknown-linux-musl/release/egcl'
        self.binary = self.root / self.name
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'test binary, never executed')
        self.manifest = {'revision': 'a' * 40, 'source_digest': 'b' * 64,
                         'profile': 'release', 'target': 'x86_64-unknown-linux-musl',
                         'artifacts': {self.name: bench.digest(self.binary)}}
        self.manifest_name = 'target/ci-tests/manifest.json'
        manifest_path = self.root / self.manifest_name
        manifest_path.parent.mkdir(parents=True)
        manifest_path.write_text(json.dumps(self.manifest))
        compressed = io.BytesIO()
        with tarfile.open(fileobj=compressed, mode='w:gz') as tar:
            for name in (self.name, self.manifest_name):
                tar.add(self.root / name, arcname=name)
        self.archive = self.root / 'artifact.zip'
        with zipfile.ZipFile(self.archive, 'w') as archive:
            archive.writestr('ci-tests.tar.gz', compressed.getvalue())
        self.run = {'id': 12, 'run_attempt': 1, 'head_sha': 'a' * 40, 'event': 'push',
                    'status': 'completed', 'conclusion': 'success',
                    'repository': {'full_name': 'atgreen/evergreen'},
                    'path': '.github/workflows/ci.yml'}
        self.artifact = {'id': 34, 'expired': False,
                         'digest': 'sha256:' + bench.digest(self.archive),
                         'workflow_run': {'id': 12, 'head_sha': 'a' * 40}}

    def verify(self, **changes):
        args = dict(root=self.root, archive=self.archive, run=self.run,
                    artifact=self.artifact, run_id=12, artifact_id=34,
                    source={'revision': 'a' * 40, 'source_digest': 'b' * 64})
        args.update(changes)
        return bench.verify_ci_receipt(**args)

    def test_verified_archive_binds_binary_and_source(self):
        receipt = self.verify()
        self.assertEqual(receipt['revision'], 'a' * 40)
        self.assertEqual(receipt['archive_sha256'], bench.digest(self.archive))

    def test_mismatched_run_artifact_head_and_source_rejected(self):
        for field, value in [('id', 13), ('head_sha', 'c' * 40),
                             ('run_attempt', 2), ('run_attempt', None),
                             ('event', 'pull_request'), ('conclusion', 'failure')]:
            bad = copy.deepcopy(self.run)
            bad[field] = value
            with self.subTest(run_field=field), self.assertRaises(ValueError):
                self.verify(run=bad)
        for bad in [dict(self.artifact, id=35), dict(self.artifact, expired=True),
                    dict(self.artifact, workflow_run={'id': 13, 'head_sha': 'a' * 40}),
                    dict(self.artifact, workflow_run={'id': 12, 'head_sha': 'c' * 40}),
                    dict(self.artifact, digest='sha256:' + '0' * 64)]:
            with self.subTest(artifact=bad), self.assertRaises(ValueError):
                self.verify(artifact=bad)
        with self.assertRaises(ValueError):
            self.verify(source={'revision': 'a' * 40, 'source_digest': 'c' * 64})

    def test_changed_extracted_binary_or_manifest_rejected(self):
        self.binary.write_bytes(b'changed')
        with self.assertRaises(ValueError):
            self.verify()
        self.binary.write_bytes(b'test binary, never executed')
        path = self.root / self.manifest_name
        path.write_text(path.read_text() + '\n')
        with self.assertRaises(ValueError):
            self.verify()

    def test_github_metadata_preserves_caller_authentication(self):
        with mock.patch.dict(os.environ,
                             {'GH_TOKEN': 'synthetic-test-token', 'GITHUB_TOKEN': 'synthetic-other-token'}), \
                mock.patch('packed_provenance.subprocess.check_output', return_value='{}') as call:
            bench.github_metadata('runs', 12)
        self.assertEqual(call.call_args.kwargs['env']['GH_TOKEN'], 'synthetic-test-token')
        self.assertEqual(call.call_args.kwargs['env']['GITHUB_TOKEN'], 'synthetic-other-token')

    def test_locally_rehashed_binary_cannot_replace_archived_binary(self):
        self.binary.write_bytes(b'locally replaced')
        self.manifest['artifacts'][self.name] = bench.digest(self.binary)
        (self.root / self.manifest_name).write_text(json.dumps(self.manifest))
        with self.assertRaises(ValueError):
            self.verify()


if __name__ == '__main__':
    unittest.main()
