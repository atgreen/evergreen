#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import importlib.util
from pathlib import Path
import platform
import stat
import tempfile
import unittest
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


source_rpm = load('egcl_source_rpm', 'source-rpm.py')
release = load('egcl_release', 'release.py')
build = load('egcl_build', 'build.py')
SPEC = Path(__file__).with_name('egcl.spec')


class BuildStageTests(unittest.TestCase):
    def test_a_full_build_starts_with_an_empty_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            stale = output / 'stage/usr/bin/torcl'
            stale.parent.mkdir(parents=True)
            stale.write_text('obsolete payload')

            stage = build.fresh_stage(output)

            self.assertEqual(stage, output / 'stage')
            self.assertTrue(stage.is_dir())
            self.assertEqual(list(stage.iterdir()), [])

    def test_packaging_starts_with_an_empty_binary_rpm_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            stale = output / 'RPMS/x86_64/torcl-0.1.0.rpm'
            stale.parent.mkdir(parents=True)
            stale.write_text('obsolete package')

            destination = build.fresh_binary_rpm_output(output)

            self.assertEqual(destination, output / 'RPMS' / build.HOST_MACHINE)
            self.assertTrue(destination.is_dir())
            self.assertEqual(list((output / 'RPMS').rglob('*.rpm')), [])


class VendoredLispDependencyTests(unittest.TestCase):
    """lib/egcl-apk/ocicl/ is gitignored, so the SRPM must fetch it.

    The RPM build is offline; if these dependencies are not vendored while the
    source RPM is made, it ships a tree that cannot build the Android tooling.
    And since the packages ship those dependencies, they must ship the notices.
    """

    def test_a_project_without_an_ocicl_manifest_is_refused(self):
        # Rather than silently producing an SRPM that looks complete.
        with self.assertRaises(SystemExit):
            build.vendor_lisp_dependencies(Path(__file__).parent)

    def test_the_apk_builder_declares_dependencies_to_vendor(self):
        manifest = build.ROOT / 'lib/egcl-apk/ocicl.csv'
        self.assertTrue(manifest.is_file(), 'the APK builder lost its ocicl.csv')
        systems = [line.split(',')[0] for line in
                   manifest.read_text().splitlines() if line.strip()]
        # Ironclad is the one the signing path cannot do without.
        self.assertIn('ironclad', systems)

    def test_the_android_package_ships_the_collected_notices(self):
        # %license names a directory, so staging the file there is enough --
        # but only if build.py still writes it under that name.
        self.assertIn('%license %{_datadir}/licenses/egcl-target-android',
                      SPEC.read_text())
        self.assertIn(build.BUNDLED_LICENSES,
                      (Path(__file__).with_name('build.py')).read_text())


VENDORED = build.ROOT / 'lib/egcl-apk/ocicl'


class ApkBuilderStagingTests(unittest.TestCase):
    """egcl-target-android must ship the APK builder, not just the runtimes.

    egcl-apk-asdf lets an application describe its APK in its own .asd, so it is
    useless unless the Android package installs the builder and the dependencies
    ocicl.csv pins. They go in /usr/share/common-lisp/source, which ASDF's
    default system source registry already searches as a (:TREE ...), so
    `asdf:make' finds all of them by name with no launcher and no configuration.
    """

    def test_a_source_tree_without_vendored_dependencies_is_refused(self):
        """Rather than staging a builder that cannot load Ironclad."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'lib/egcl-apk').mkdir(parents=True)
            with patch.object(build, 'ROOT', root), self.assertRaises(SystemExit):
                build.stage_apk_builder(root / 'stage')

    @unittest.skipUnless(VENDORED.is_dir(), 'run `ocicl install` in lib/egcl-apk')
    def test_staging_puts_the_builder_and_its_dependencies_in_asdfs_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            build.stage_apk_builder(stage)
            builder = stage / 'usr/share/common-lisp/source/egcl-apk'
            for name in ('egcl-apk.asd', 'egcl-apk-asdf.asd', 'asdf-integration.lisp',
                         'signing.lisp', 'apk.lisp', 'manifest.lisp', 'binary.lisp'):
                with self.subTest(name=name):
                    self.assertTrue((builder / name).is_file(), f'{name} was not staged')
            pinned = [line.split(',')[2].strip().split('/')[0] for line
                      in (builder / 'ocicl.csv').read_text().splitlines() if line.strip()]
            self.assertIn('ironclad-20240503-6da010f', pinned, 'ocicl.csv lost ironclad')
            for system in pinned:
                with self.subTest(system=system):
                    self.assertTrue((builder / 'ocicl' / system).is_dir())

    @unittest.skipUnless(VENDORED.is_dir(), 'run `ocicl install` in lib/egcl-apk')
    def test_nothing_is_staged_that_only_the_source_tree_needs(self):
        """No launcher, no ASDF, and no bootstrap that registers a vendored tree.

        asdf.lisp is already in the installed egcl's appended image, and
        load.lisp exists only to register an ocicl/ directory ASDF does not
        search -- which is the problem this layout removes. Staging either one
        would mean the installed builder had a second, divergent way to load.
        """
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            build.stage_apk_builder(stage)
            self.assertFalse((stage / 'usr/bin').exists(), 'a launcher was staged')
            builder = stage / 'usr/share/common-lisp/source/egcl-apk'
            for name in ('asdf.lisp', 'load.lisp', 'tests'):
                with self.subTest(name=name):
                    self.assertFalse((builder / name).exists(), f'{name} should not ship')

    def test_the_android_package_owns_the_builder_directory(self):
        """Staging files the spec does not list fails the build, but the %dir
        entries are the easy thing to lose: egcl owns those two directories and
        is built in a different rpmbuild."""
        spec = SPEC.read_text()
        for line in ('%dir %{_datadir}/common-lisp\n',
                     '%dir %{_datadir}/common-lisp/source\n',
                     '%{_datadir}/common-lisp/source/egcl-apk\n'):
            with self.subTest(line=line.strip()):
                self.assertIn(line, spec)
        self.assertNotIn('%{_bindir}/egcl-apk', spec, 'the launcher is gone; so is its %files entry')

    def test_creating_a_signing_key_does_not_depend_on_the_callers_umask(self):
        """What made a shell wrapper necessary at all.

        create-identity writes an unencrypted P-256 private key. It used to be
        refused unless a caller had set umask 077 first, because EGCL had no
        chmod; it now sets the mode itself, so a plain `asdf:make' is safe.
        """
        signing = (build.ROOT / 'lib/egcl-apk/signing.lisp').read_text()
        self.assertIn('restrict-to-owner', signing)
        self.assertIn('#o600', signing)
        integration = (build.ROOT / 'lib/egcl-apk/asdf-integration.lisp').read_text()
        self.assertNotIn('*allow-identity-creation*', integration)


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
    # `native` and `static` are the two runtime names that do not simply take an
    # egcl-target- prefix, which is the mapping worth pinning down.
    NAMES = {
        'native': ['egcl', 'egcl-static'],
        'windows': ['egcl-target-windows'],
        'riscv64': ['egcl-target-riscv64-linux-static'],
        's390x': ['egcl-target-s390x-linux', 'egcl-target-s390x-linux-static'],
    }

    def test_runtime_names_map_to_package_names(self):
        # Only the native group exists off x86_64 (build.py mirrors egcl.spec's
        # %ifarch guard), and this file also runs in the spec's %check on the
        # POWER builder -- so assert each group where that group is offered.
        for group, packages in self.NAMES.items():
            if group not in source_rpm.BUILDER['GROUPS']:
                continue
            with self.subTest(group=group):
                self.assertEqual(source_rpm.expected_packages(group), packages)
        self.assertIn('native', source_rpm.BUILDER['GROUPS'], 'every host builds the native group')

    def test_the_groups_together_produce_exactly_the_published_package_set(self):
        """Cross-file invariant: build groups and release.py's packages agree.

        A group gaining a runtime without release.py learning its package name
        would otherwise only surface as a failed release, after a full build.

        Compared against THIS host's package set, because both sides are
        host-dependent: build.py offers only the native group off x86_64, and
        release.py expects only the two native packages from such a host. This
        test also runs inside the spec's %check, on the POWER builder included.
        """
        produced = [name for group in source_rpm.BUILDER['GROUPS']
                    for name in source_rpm.expected_packages(group)]
        self.assertEqual(len(produced), len(set(produced)), 'a package is built by two groups')
        self.assertEqual(set(produced), release.PACKAGES_BY_ARCH[platform.machine()])

    def test_every_group_covers_only_known_runtimes(self):
        for group, runtimes in source_rpm.BUILDER['GROUPS'].items():
            with self.subTest(group=group):
                self.assertTrue(set(runtimes) <= set(source_rpm.BUILDER['TARGETS']))


if __name__ == '__main__':
    unittest.main()
