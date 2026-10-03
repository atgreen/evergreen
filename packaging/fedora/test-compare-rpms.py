#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'egcl_compare_rpms', Path(__file__).with_name('compare-rpms.py'))
compare_rpms = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare_rpms)


def fake_rpm(contents, names=None):
    """Answer `rpm -qp` out of a {filename: {tag/file: value}} fixture."""
    def check_output(command, **kwargs):
        path = Path(command[-1])
        package = contents[path.name]
        if '--dump' in command:
            return ''.join(f'{line}\n' for line in package['files'])
        requested = command[command.index('--queryformat') + 1].split('\n')
        if requested == ['%{NAME}']:
            return (names or {}).get(path.name, package.get('NAME', 'pkg'))
        return ''.join(f'{tag.strip("%{}")}={package.get(tag.strip("%{}").split("=")[-1], "")}\n'
                       for tag in [line.split('=')[0] for line in requested])
    return check_output


class ComparisonTests(unittest.TestCase):
    def directories(self, first_files, second_files, first_tags=None, second_tags=None):
        self.first = Path(tempfile.mkdtemp())
        self.second = Path(tempfile.mkdtemp())
        (self.first / 'pkg.rpm').write_bytes(b'a')
        (self.second / 'pkg.rpm').write_bytes(b'b')
        self.contents = {'pkg.rpm': {'NAME': 'pkg', 'files': first_files, **(first_tags or {})}}
        self.second_contents = {'pkg.rpm': {'NAME': 'pkg', 'files': second_files,
                                            **(second_tags or {})}}

        def check_output(command, **kwargs):
            directory = Path(command[-1]).parent
            source = self.contents if directory == self.first else self.second_contents
            return fake_rpm(source)(command, **kwargs)

        return patch.object(compare_rpms.subprocess, 'check_output', side_effect=check_output)

    def test_identical_content_reports_nothing(self):
        files = ['/usr/bin/egcl 10 1790985600 abc 0100755 root root 0 0 0 X']
        with self.directories(files, list(files)):
            self.assertEqual(compare_rpms.compare(self.first, self.second), {})

    def test_a_changed_file_digest_is_reported(self):
        before = ['/usr/bin/egcl 10 1790985600 aaa 0100755 root root 0 0 0 X']
        after = ['/usr/bin/egcl 10 1790985600 bbb 0100755 root root 0 0 0 X']
        with self.directories(before, after):
            report = compare_rpms.compare(self.first, self.second)
        self.assertEqual(list(report), ['pkg'])
        self.assertTrue(any(line.startswith('-FILE') and 'aaa' in line for line in report['pkg']))
        self.assertTrue(any(line.startswith('+FILE') and 'bbb' in line for line in report['pkg']))

    def test_a_changed_mode_is_reported(self):
        """Permissions are content: a file turning executable must not slip by."""
        before = ['/usr/bin/egcl 10 1 abc 0100644 root root 0 0 0 X']
        after = ['/usr/bin/egcl 10 1 abc 0100755 root root 0 0 0 X']
        with self.directories(before, after):
            self.assertIn('pkg', compare_rpms.compare(self.first, self.second))

    def test_a_package_present_in_only_one_build_is_reported(self):
        first = Path(tempfile.mkdtemp())
        second = Path(tempfile.mkdtemp())
        (first / 'pkg.rpm').write_bytes(b'a')
        with patch.object(compare_rpms.subprocess, 'check_output',
                          side_effect=fake_rpm({'pkg.rpm': {'NAME': 'pkg', 'files': []}})):
            report = compare_rpms.compare(first, second)
        self.assertEqual(report, {'pkg': ['only in the first build']})


class IndexTests(unittest.TestCase):
    def test_two_packages_of_one_name_is_an_error_not_a_silent_overwrite(self):
        directory = Path(tempfile.mkdtemp())
        (directory / 'one.rpm').write_bytes(b'a')
        (directory / 'two.rpm').write_bytes(b'b')
        contents = {'one.rpm': {'NAME': 'pkg', 'files': []},
                    'two.rpm': {'NAME': 'pkg', 'files': []}}
        with patch.object(compare_rpms.subprocess, 'check_output',
                          side_effect=fake_rpm(contents)):
            with self.assertRaisesRegex(ValueError, 'two packages named pkg'):
                compare_rpms.index(directory)


class TagSelectionTests(unittest.TestCase):
    def test_signature_tags_are_not_compared(self):
        """Signing rewrites the header; two identical builds must still match."""
        for tag in ('SIGPGP', 'SIGGPG', 'RSAHEADER', 'DSAHEADER', 'SHA1HEADER',
                    'SHA256HEADER', 'SIGSIZE'):
            self.assertNotIn(tag, compare_rpms.TAGS)

    def test_buildtime_is_reported_but_not_counted(self):
        """Measured on rpm 6.0.2: BUILDTIME takes the wall clock, mtimes do not."""
        self.assertNotIn('BUILDTIME', compare_rpms.TAGS)
        self.assertIn('BUILDTIME', compare_rpms.VARIABLE_TAGS)

    def test_content_tags_that_must_be_compared(self):
        for tag in ('NAME', 'VERSION', 'RELEASE', 'ARCH', 'LICENSE', 'REQUIRES', 'PROVIDES'):
            self.assertIn(tag, compare_rpms.TAGS)


if __name__ == '__main__':
    unittest.main()
