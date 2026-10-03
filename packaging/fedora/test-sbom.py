#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('egcl_sbom', Path(__file__).with_name('sbom.py'))
sbom = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sbom)

# A workspace crate depending on one shipped library, one build tool and one
# test-only crate, which in turn pulls a transitive dependency of its own.
METADATA = {
    'workspace_members': ['egcl 0.0.1'],
    'packages': [
        {'id': 'egcl 0.0.1', 'name': 'egcl', 'version': '0.0.1', 'license': 'GPL-3.0-or-later'},
        {'id': 'shipped 1.0.0', 'name': 'shipped', 'version': '1.0.0',
         'license': 'MIT OR Apache-2.0', 'description': 'linked into the runtime'},
        {'id': 'buildtool 2.0.0', 'name': 'buildtool', 'version': '2.0.0', 'license': 'MIT'},
        {'id': 'testonly 3.0.0', 'name': 'testonly', 'version': '3.0.0', 'license': 'MIT'},
        {'id': 'testdep 4.0.0', 'name': 'testdep', 'version': '4.0.0', 'license': 'MIT'},
    ],
    'resolve': {'nodes': [
        {'id': 'egcl 0.0.1', 'deps': [
            {'pkg': 'shipped 1.0.0', 'dep_kinds': [{'kind': None}]},
            {'pkg': 'buildtool 2.0.0', 'dep_kinds': [{'kind': 'build'}]},
            {'pkg': 'testonly 3.0.0', 'dep_kinds': [{'kind': 'dev'}]},
        ]},
        {'id': 'testonly 3.0.0', 'deps': [
            {'pkg': 'testdep 4.0.0', 'dep_kinds': [{'kind': None}]},
        ]},
        {'id': 'shipped 1.0.0', 'deps': []},
        {'id': 'buildtool 2.0.0', 'deps': []},
        {'id': 'testdep 4.0.0', 'deps': []},
    ]},
}


class RustComponentTests(unittest.TestCase):
    def components(self, metadata=None):
        with patch.object(sbom.subprocess, 'check_output',
                          return_value=json.dumps(metadata or METADATA)):
            return sbom.rust_components()

    def test_dev_dependencies_and_their_subtrees_are_excluded(self):
        names = [component['name'] for component in self.components()]
        self.assertIn('shipped', names)
        # Build tools execute during the build, so they are supply chain.
        self.assertIn('buildtool', names)
        self.assertNotIn('testonly', names)
        # The point of walking the graph rather than listing Cargo.lock: a
        # dev-only crate's own dependencies are not in the packages either.
        self.assertNotIn('testdep', names)

    def test_workspace_members_are_not_listed_as_their_own_dependencies(self):
        self.assertNotIn('egcl', [component['name'] for component in self.components()])

    def test_a_crate_reached_by_both_a_dev_and_a_real_edge_is_kept(self):
        metadata = json.loads(json.dumps(METADATA))
        metadata['resolve']['nodes'][0]['deps'].append(
            {'pkg': 'testdep 4.0.0', 'dep_kinds': [{'kind': None}]})
        self.assertIn('testdep', [component['name'] for component in self.components(metadata)])

    def test_components_carry_a_purl_and_are_ordered(self):
        components = self.components()
        self.assertEqual([component['purl'] for component in components],
                         ['pkg:cargo/buildtool@2.0.0', 'pkg:cargo/shipped@1.0.0'])


class DocumentTests(unittest.TestCase):
    def test_native_libraries_are_read_from_the_scripts_that_pin_them(self):
        components = {component['name']: component for component in sbom.native_components()}
        self.assertEqual(set(components), {'musl', 'libunwind', 'android-ndk'})
        musl = sbom.runpy.run_path(str(Path(__file__).with_name('prepare-musl.py')))
        self.assertEqual(components['musl']['version'], musl['MUSL_VERSION'])
        self.assertEqual(components['musl']['hashes'][0]['content'], musl['MUSL_SHA256'])
        self.assertEqual(components['libunwind']['hashes'][0]['content'], musl['UNWIND_SHA256'])
        for component in components.values():
            self.assertIn('licenses', component)

    def test_a_renamed_or_unpinned_ndk_fails_instead_of_being_omitted(self):
        with tempfile.NamedTemporaryFile('w', suffix='.sh', delete=False) as stream:
            stream.write('curl https://example.invalid/ndk.zip\n')
            path = Path(stream.name)
        self.addCleanup(path.unlink)
        with self.assertRaisesRegex(ValueError, 'NDK'):
            sbom.ndk_release(path)

    def test_document_is_deterministic_and_declares_cyclonedx(self):
        # SOURCE_DATE_EPOCH has to be cleared, not merely assumed absent. rpmbuild
        # sets it for reproducible builds, so inheriting the ambient value made
        # this case assert the opposite of the next one and fail only inside the
        # spec's %check -- green on a bare runner, red in every binaries job.
        with patch.object(sbom.subprocess, 'check_output', return_value=json.dumps(METADATA)), \
                patch.dict(sbom.os.environ):
            sbom.os.environ.pop('SOURCE_DATE_EPOCH', None)
            first = sbom.build('0.0.1')
            second = sbom.build('0.0.1')
        self.assertEqual(json.dumps(first), json.dumps(second))
        self.assertEqual((first['bomFormat'], first['specVersion']), ('CycloneDX', '1.6'))
        self.assertEqual(first['metadata']['component']['version'], '0.0.1')
        # A random serial number or a wall-clock timestamp would make two SBOMs
        # of identical inputs differ, and nothing could be checked against them.
        self.assertNotIn('serialNumber', first)
        self.assertNotIn('timestamp', first['metadata'])

    def test_source_date_epoch_supplies_a_reproducible_timestamp(self):
        with patch.object(sbom.subprocess, 'check_output', return_value=json.dumps(METADATA)), \
                patch.dict(sbom.os.environ, {'SOURCE_DATE_EPOCH': '1700000000'}):
            document = sbom.build('0.0.1')
        self.assertEqual(document['metadata']['timestamp'], '2023-11-14T22:13:20Z')


class RealTreeTests(unittest.TestCase):
    """Against the actual workspace, so the metadata shape cannot silently change."""

    def test_the_real_workspace_produces_a_usable_document(self):
        try:
            document = sbom.build('0.0.1')
        except (subprocess.CalledProcessError, FileNotFoundError) as error:
            self.skipTest(f'cargo metadata unavailable: {error}')
        names = {component['name'] for component in document['components']}
        self.assertLessEqual({'musl', 'libunwind', 'android-ndk'}, names)
        self.assertNotIn('proptest', names, 'dev-only crates must not be listed as shipped')
        for component in document['components']:
            self.assertTrue(component['purl'].startswith('pkg:'))
            self.assertIn('licenses', component)


if __name__ == '__main__':
    unittest.main()
