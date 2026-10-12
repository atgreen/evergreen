#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import ordinary_bridge as bench


class EvidenceTests(unittest.TestCase):
    def output(self, ordinary=True):
        blocks = []
        for phase in ('before', 'after'):
            for name in bench.NAMES:
                abi = 'Ordinary x86-64 Lisp ABI' if ordinary and name in bench.ORDINARY else 'Mapped native transfer ABI'
                blocks.append(f'BRIDGE-CODE-BEGIN {phase} {name}\n; {name}\n; T2 — code\n; {abi} — installed native code.\n0000 call rax\nBRIDGE-CODE-END')
        return '\n'.join(blocks) + '\nBRIDGE-SAMPLE 0 direct 100 45\nBRIDGE-SAMPLE 0 forward 120 45\n'

    def test_complete_paired_evidence(self):
        rows = bench.parse_trial(self.output(), '', 10, 1, True)
        self.assertEqual(rows, [{'pair': 0, 'direct_ns': 100, 'forward_ns': 120, 'increment_ns': 20}])

    def test_reject_wrong_sum_missing_samples_changed_code_or_missing_abi(self):
        bad = [self.output().replace('120 45', '120 44'),
               self.output().replace('BRIDGE-SAMPLE 0 forward 120 45', ''),
               self.output().replace('0000 call rax', '0000 nop', 1),
               self.output(ordinary=False)]
        for output in bad:
            with self.subTest(output=output), self.assertRaises(ValueError):
                bench.parse_trial(output, '', 10, 1, True)

    def test_baseline_mapped_code_does_not_need_new_abi(self):
        self.assertEqual(len(bench.parse_trial(self.output(False), '', 10, 1, False)), 1)

    def test_reject_named_deoptimization(self):
        with self.assertRaises(ValueError):
            bench.parse_trial(self.output(), 'ORDINARY-BRIDGE-FORWARD: deoptimized', 10, 1, True)

    def test_artifact_hash_and_profile_are_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'target/x86_64-unknown-linux-musl/release/egcl'
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b'fixture')
            manifest = {'revision': 'abc', 'source_digest': '123', 'profile': 'release',
                        'target': 'x86_64-unknown-linux-musl', 'features': '',
                        'artifacts': {str(binary.relative_to(root)): hashlib.sha256(b'fixture').hexdigest()}}
            path = root / 'manifest.json'
            path.write_text(json.dumps(manifest))
            self.assertEqual(bench.artifact(root, path)['revision'], 'abc')
            binary.write_bytes(b'changed')
            with self.assertRaises(ValueError):
                bench.artifact(root, path)
            manifest['profile'] = 'debug'
            path.write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):
                bench.artifact(root, path)

    def test_matched_profile_configuration(self):
        before = {'profile': 'release', 'target': 'x86_64-unknown-linux-musl', 'features': ''}
        after = dict(before, features='different')
        with self.assertRaises(ValueError):
            bench.match_configuration(before, after)


if __name__ == '__main__':
    unittest.main()
