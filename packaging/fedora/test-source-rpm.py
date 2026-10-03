#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
from pathlib import Path
import unittest


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


source_rpm = load('egcl_source_rpm', 'source-rpm.py')
release = load('egcl_release', 'release.py')
SPEC = Path(__file__).with_name('egcl.spec')


class SubstituteTests(unittest.TestCase):
    """str.replace is silent when it matches nothing; substitute must not be."""

    SPEC_LINES = ('Version: %{egcl_version}\n'
                  '%{!?egcl_release:%global egcl_release 6}\n'
                  'Release: %{egcl_release}%{?dist}\n')
    VERSION = r'Version: %\{egcl_version\}'
    RELEASE = r'%\{!\?egcl_release:%global egcl_release \d+\}'

    def test_rewrites_the_targeted_line_only(self):
        result = source_rpm.substitute(self.SPEC_LINES, self.VERSION, 'Version: 0.0.1')
        self.assertIn('Version: 0.0.1\n', result)
        # The Release line references the macro and must survive untouched.
        self.assertIn('Release: %{egcl_release}%{?dist}\n', result)

    def test_matches_any_fallback_release_number(self):
        """A version bump changes that number; the rewrite must still happen.

        Keyed on the literal 6, the substitution silently stopped matching and
        the SRPM carried the fallback release instead of the planned one.
        """
        for fallback in ('6', '1', '27'):
            spec = self.SPEC_LINES.replace('egcl_release 6', f'egcl_release {fallback}')
            with self.subTest(fallback=fallback):
                result = source_rpm.substitute(spec, self.RELEASE, '%global egcl_release 0.test.9.1')
                self.assertIn('%global egcl_release 0.test.9.1\n', result)

    def test_raises_when_the_line_is_absent(self):
        for pattern in (self.VERSION, self.RELEASE):
            with self.subTest(pattern=pattern), self.assertRaisesRegex(ValueError, 'not found'):
                source_rpm.substitute('Name: egcl\n', pattern, 'replacement')

    def test_replacement_text_is_taken_literally(self):
        """A replacement is not a regex template: backslashes and \\1 are data."""
        result = source_rpm.substitute('Version: %{egcl_version}\n', self.VERSION,
                                       r'Version: 0.0.1\1')
        self.assertIn(r'Version: 0.0.1\1', result)

    def test_both_rewrites_apply_to_the_real_spec(self):
        """The drift guard: egcl.spec must still contain both targeted lines."""
        spec = SPEC.read_text()
        spec = source_rpm.substitute(spec, self.VERSION, 'Version: 9.9.9')
        spec = source_rpm.substitute(spec, self.RELEASE, '%global egcl_release 42')
        self.assertIn('Version: 9.9.9\n', spec)
        self.assertIn('%global egcl_release 42\n', spec)
        self.assertNotIn('%{egcl_version}', spec)


class ExpectedPackageTests(unittest.TestCase):
    def test_runtime_names_map_to_package_names(self):
        self.assertEqual(source_rpm.expected_packages('native'), ['egcl', 'egcl-static'])
        self.assertEqual(source_rpm.expected_packages('windows'), ['egcl-target-windows'])
        self.assertEqual(source_rpm.expected_packages('s390x'),
                         ['egcl-target-s390x-linux', 'egcl-target-s390x-linux-static'])

    def test_the_groups_together_produce_exactly_the_published_package_set(self):
        """Cross-file invariant: build groups and release.py's PACKAGES agree.

        A group gaining a runtime without release.py learning its package name
        would otherwise only surface as a failed release, after a full build.
        """
        produced = [name for group in source_rpm.BUILDER['GROUPS']
                    for name in source_rpm.expected_packages(group)]
        self.assertEqual(len(produced), len(set(produced)), 'a package is built by two groups')
        self.assertEqual(set(produced), release.PACKAGES)

    def test_every_group_covers_only_known_runtimes(self):
        for group, runtimes in source_rpm.BUILDER['GROUPS'].items():
            with self.subTest(group=group):
                self.assertTrue(set(runtimes) <= set(source_rpm.BUILDER['TARGETS']))


if __name__ == '__main__':
    unittest.main()
