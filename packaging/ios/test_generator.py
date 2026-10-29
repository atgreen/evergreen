#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Offline project/build-boundary assertions; these do not validate Xcode."""
import importlib.util
import io
import json
import plistlib
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent
REPOSITORY = ROOT.parents[1]


class GeneratorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.project = Path(temporary.name) / 'Application with spaces'

    def generate(self, *arguments):
        return subprocess.run([sys.executable, str(ROOT / 'egcl-ios-new'),
                               str(self.project), '--runtime-source', str(REPOSITORY),
                               *arguments], capture_output=True, text=True)

    def test_bundle_resources_shared_scheme_and_signing_settings(self):
        result = self.generate('--bundle-id', 'org.example.lispprobe',
                               '--name', ' Lisp \"友\" & Friends ', '--team', 'ABCDEFGHIJ')
        self.assertEqual(result.returncode, 0, result.stderr)
        settings = (self.project / 'Project.xcconfig').read_text()
        self.assertIn('PRODUCT_BUNDLE_IDENTIFIER = org.example.lispprobe', settings)
        self.assertIn('DEVELOPMENT_TEAM = ABCDEFGHIJ', settings)
        with (self.project / 'Info.plist').open('rb') as stream:
            self.assertEqual(plistlib.load(stream)['CFBundleDisplayName'], ' Lisp \"友\" & Friends ')
        configuration = json.loads((self.project / 'RuntimeSource.json').read_text())
        self.assertEqual(configuration, {'version': 1, 'runtime_source': str(REPOSITORY)})
        self.assertIn('ARCHS = arm64', settings)
        self.assertIn('-legcl_ios', settings)
        self.assertTrue((self.project / 'Lisp/app.lisp').is_file())
        project = (self.project / 'LispApp.xcodeproj/project.pbxproj').read_text()
        self.assertIn('lastKnownFileType = folder; path = Lisp;', project)
        scheme = ET.parse(self.project / 'LispApp.xcodeproj/xcshareddata/xcschemes/LispApp.xcscheme')
        target = scheme.find('.//BuildActionEntry/BuildableReference')
        self.assertEqual(target.attrib['BlueprintName'], 'LispApp')
        self.assertEqual(target.attrib['BlueprintIdentifier'], 'A00000000000000000000010')

    def test_runtime_path_is_json_data_not_an_xcode_setting(self):
        runtime = self.project.parent / 'Runtime "日本語" $value'
        for relative in ['crates/egcl-ios/Cargo.toml',
                         'crates/egcl-ios/include/egcl_ios.h',
                         'examples/ios/EGCLIOSDemo.swift',
                         'examples/ios/EGCLIOSDemo-Bridging-Header.h',
                         'examples/ios/Lisp/app.lisp']:
            output = runtime / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPOSITORY / relative, output)
        result = self.generate('--runtime-source', str(runtime))
        self.assertEqual(result.returncode, 0, result.stderr)
        configuration = json.loads((self.project / 'RuntimeSource.json').read_text())
        self.assertEqual(configuration['runtime_source'], str(runtime))
        self.assertNotIn(str(runtime), (self.project / 'Project.xcconfig').read_text())

    def test_existing_destination_is_preserved(self):
        self.project.mkdir()
        sentinel = self.project / 'keep'
        sentinel.write_text('original')
        self.assertNotEqual(self.generate().returncode, 0)
        self.assertEqual(sentinel.read_text(), 'original')

    def test_invalid_settings_create_nothing(self):
        for arguments in [('--bundle-id', 'bad/name'), ('--team', 'abc'),
                          ('--name', 'bad$(SETTING)'), ('--name', 'bad//name'),
                          ('--runtime-source', '/missing/$source')]:
            with self.subTest(arguments=arguments):
                self.assertNotEqual(self.generate(*arguments).returncode, 0)
                self.assertFalse(self.project.exists())


class RuntimeBoundaryTests(unittest.TestCase):
    def test_selected_sdk_target_and_exact_native_flags_are_forwarded(self):
        spec = importlib.util.spec_from_file_location('ios_build_runtime', ROOT / 'templates/BuildRuntime.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / 'Runtime \"日本語\" $value'
            source.mkdir()
            configuration = directory / 'RuntimeSource.json'
            configuration.write_text(json.dumps({'version': 1, 'runtime_source': str(source)}))
            for platform, target in [('iphoneos', 'aarch64-apple-ios'),
                                     ('iphonesimulator', 'aarch64-apple-ios-sim')]:
                with self.subTest(platform=platform):
                    cargo_dir = directory / platform
                    archive = cargo_dir / target / 'release/libegcl_ios.a'
                    archive.parent.mkdir(parents=True)
                    archive.write_bytes(b'old archive')
                    output = directory / (platform + '.a')
                    response = directory / (platform + '.rsp')
                    arguments = ['BuildRuntime.py', '--config', str(configuration),
                                 '--sdk', '/Xcode SDK with spaces', '--platform', platform,
                                 '--arch', 'arm64', '--deployment-target', '17.0',
                                 '--target-dir', str(cargo_dir), '--output', str(output),
                                 '--response', str(response)]

                    def cargo(command, **options):
                        self.assertFalse(archive.exists())
                        self.assertEqual(options['cwd'], source)
                        self.assertEqual(command[command.index('--target') + 1], target)
                        self.assertEqual(options['env']['SDKROOT'], '/Xcode SDK with spaces')
                        self.assertEqual(options['env']['IPHONEOS_DEPLOYMENT_TARGET'], '17.0')
                        self.assertIn('staticlib', command)
                        self.assertIn('egcl-rt/c-ffi', command)
                        archive.write_bytes(b'new archive')
                        return subprocess.CompletedProcess(command, 0, '',
                            "note: native-static-libs: -lSystem -framework Security -F '/SDK/日本語 space' -lSystem\n")

                    with patch.object(sys, 'argv', arguments), \
                         patch.object(module.subprocess, 'run', side_effect=cargo), \
                         patch.object(sys, 'stdout', io.StringIO()), \
                         patch.object(sys, 'stderr', io.StringIO()):
                        self.assertEqual(module.main(), 0)
                    self.assertEqual(output.read_bytes(), b'new archive')
                    self.assertEqual(response.read_text(),
                                     '"-lSystem"\n"-framework"\n"Security"\n"-F"\n"/SDK/日本語 space"\n"-lSystem"\n')


if __name__ == '__main__':
    unittest.main()
