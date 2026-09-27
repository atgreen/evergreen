#!/usr/bin/env python3
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('apk', ROOT / 'build-apk.py')
apk = importlib.util.module_from_spec(spec); spec.loader.exec_module(apk)

class InstallToolsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.sdk = Path(self.tmp.name) / 'sdk'
        self.args = SimpleNamespace(sdk=self.sdk, sdk_build_tools='35.0.0')
        self.manifest = ET.fromstring('<manifest xmlns:android="http://schemas.android.com/apk/res/android"><uses-sdk android:targetSdkVersion="34"/></manifest>')

    def test_existing_manager_installs_only_needed_components_in_selected_sdk(self):
        manager = self.sdk / 'cmdline-tools/latest/bin/sdkmanager'
        manager.parent.mkdir(parents=True); manager.touch(); manager.chmod(0o755)
        with patch.object(apk.shutil, 'which', return_value='/bin/java'), patch.object(apk, 'run') as run:
            apk.install_tools(self.args, self.manifest)
        self.assertEqual(run.call_args.args[0], [manager, f'--sdk_root={self.sdk.resolve()}', '--install', 'platform-tools', 'platforms;android-34', 'build-tools;35.0.0'])
        self.assertNotIn('input', run.call_args.kwargs)  # License prompts inherit the terminal.

    def test_missing_jdk_fails_before_creating_sdk(self):
        with patch.object(apk.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'java-21-openjdk-devel'):
                apk.install_tools(self.args, self.manifest)
        self.assertFalse(self.sdk.exists())

    def test_bad_checksum_does_not_install_download(self):
        with patch.object(apk.urllib.request, 'urlopen', return_value=io.BytesIO(b'not a valid archive')):
            with self.assertRaisesRegex(ValueError, 'checksum'):
                apk.bootstrap_sdkmanager(self.sdk)
        self.assertFalse((self.sdk / 'cmdline-tools/latest').exists())

    def test_verified_archive_is_installed_with_executable_permissions(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('cmdline-tools/bin/sdkmanager', '#!/bin/sh\nexit 0\n')
            z.writestr('cmdline-tools/lib/sdk.jar', 'data')
        data = archive.getvalue()
        with patch.object(apk, 'COMMANDLINE_TOOLS_SHA256', apk.hashlib.sha256(data).hexdigest()), patch.object(apk.urllib.request, 'urlopen', return_value=io.BytesIO(data)):
            manager = apk.bootstrap_sdkmanager(self.sdk)
        self.assertTrue(manager.stat().st_mode & 0o111)
        self.assertTrue((self.sdk / 'cmdline-tools/latest/lib/sdk.jar').is_file())

    def test_generated_make_target_runs_without_runtime_or_sdk_packages(self):
        project = Path(self.tmp.name) / 'project'
        subprocess.run(['python3', ROOT / 'torcl-android-new', project,
                        '--runtime', ROOT], check=True, capture_output=True)
        manager = self.sdk / 'cmdline-tools/latest/bin/sdkmanager'
        manager.parent.mkdir(parents=True)
        manager.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > sdk-arguments.txt\n')
        manager.chmod(0o755)
        binaries = Path(self.tmp.name) / 'bin'
        binaries.mkdir()
        for tool in ('java', 'keytool'):
            path = binaries / tool
            path.write_text('#!/bin/sh\nexit 0\n')
            path.chmod(0o755)
        result = subprocess.run(['make', 'install-tools', f'SDK={self.sdk}', 'SDK_BUILD_TOOLS=34.0.0'],
                                cwd=project, env={**os.environ, 'PATH': f'{binaries}:{os.environ["PATH"]}'},
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((project / 'sdk-arguments.txt').read_text().splitlines(),
                         [f'--sdk_root={self.sdk}', '--install', 'platform-tools',
                          'platforms;android-34', 'build-tools;34.0.0'])

    def test_verified_archive_cannot_escape_destination(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('cmdline-tools/../../escaped', 'data')
        data = archive.getvalue()
        with patch.object(apk, 'COMMANDLINE_TOOLS_SHA256', apk.hashlib.sha256(data).hexdigest()), patch.object(apk.urllib.request, 'urlopen', return_value=io.BytesIO(data)):
            with self.assertRaisesRegex(ValueError, 'archive path'):
                apk.bootstrap_sdkmanager(self.sdk)
        self.assertFalse((self.sdk / 'cmdline-tools/latest').exists())


if __name__ == '__main__': unittest.main()
