#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Compare two builds of the same packages, for reproducibility checking.

spec R7.12 and the phase-3 roadmap both call for reproducible release
artifacts, and nothing measured whether they are. Byte-comparing the RPM files
answers the wrong question: signing rewrites the header, so two
indistinguishable builds differ as files the moment one is signed. What matters
is whether the same inputs produced the same CONTENT -- the same files, with the
same permissions, digests and timestamps -- so that is what this compares.

Usage:
    python3 packaging/fedora/compare-rpms.py build-one/ build-two/

RPM already sets SOURCE_DATE_EPOCH from the latest %changelog entry and clamps
file mtimes to it, so timestamps should match without any work here; a
difference reported in an mtime means something escaped that clamp.
"""
import argparse
from pathlib import Path
import subprocess
import sys

# Content, deliberately not signatures: SIGPGP, RSAHEADER and friends differ
# between a signed and an unsigned copy of one identical build.
TAGS = ('NAME', 'EPOCH', 'VERSION', 'RELEASE', 'ARCH', 'SIZE',
        'LICENSE', 'SUMMARY', 'PROVIDES', 'REQUIRES', 'CONFLICTS', 'OBSOLETES',
        'PAYLOADFORMAT', 'PAYLOADCOMPRESSOR')
# Reported, but not counted as a content difference. Measured on rpm 6.0.2:
# two builds of one unchanged spec clamp packaged file mtimes to the changelog
# date (so those match) while BUILDTIME takes the wall clock, differing by the
# seconds between the builds. Counting it would make the verdict permanently
# red and say nothing about whether the content is reproducible.
VARIABLE_TAGS = ('BUILDTIME',)


def query(rpm, tags):
    queryformat = '\n'.join(f'{tag}=%{{{tag}}}' for tag in tags)
    return subprocess.check_output(
        ['rpm', '-qp', '--queryformat', queryformat, str(rpm)],
        text=True, stderr=subprocess.DEVNULL).splitlines()


def manifest(rpm):
    """Every packaged file with its size, mtime, digest, mode, owner and group.

    `rpm -qp --dump` is the file-level content of the package, which is what a
    reproducibility claim is actually about.
    """
    return subprocess.check_output(
        ['rpm', '-qp', '--dump', str(rpm)], text=True, stderr=subprocess.DEVNULL).splitlines()


def describe(rpm):
    """Everything a reproducibility verdict is based on."""
    return query(rpm, TAGS) + [f'FILE {line}' for line in manifest(rpm)]


def package_name(rpm):
    return subprocess.check_output(
        ['rpm', '-qp', '--queryformat', '%{NAME}', str(rpm)],
        text=True, stderr=subprocess.DEVNULL).strip()


def index(directory):
    """Packages by NAME, refusing a directory holding two of the same name."""
    packages = {}
    for rpm in sorted(Path(directory).glob('*.rpm')):
        name = package_name(rpm)
        if name in packages:
            raise ValueError(f'{directory}: two packages named {name}: '
                             f'{packages[name].name} and {rpm.name}')
        packages[name] = rpm
    return packages


def compare(first, second):
    """Differences per package, plus packages present in only one build."""
    left, right = index(first), index(second)
    report = {}
    for name in sorted(set(left) | set(right)):
        if name not in right:
            report[name] = ['only in the first build']
            continue
        if name not in left:
            report[name] = ['only in the second build']
            continue
        before, after = describe(left[name]), describe(right[name])
        differences = [f'-{line}' for line in before if line not in after]
        differences += [f'+{line}' for line in after if line not in before]
        if differences:
            report[name] = differences
    return report


def variable_notes(first, second):
    """Differences in fields known to vary, reported without failing."""
    left, right = index(first), index(second)
    notes = {}
    for name in sorted(set(left) & set(right)):
        before, after = query(left[name], VARIABLE_TAGS), query(right[name], VARIABLE_TAGS)
        if before != after:
            notes[name] = [f'{b} -> {a}' for b, a in zip(before, after) if b != a]
    return notes


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('first', type=Path, help='directory of RPMs from one build')
    parser.add_argument('second', type=Path, help='directory of RPMs from the other build')
    parser.add_argument('--limit', type=int, default=20,
                        help='differences to print per package (default 20)')
    args = parser.parse_args()
    report = compare(args.first, args.second)
    packages = len(index(args.first))
    for name, notes in variable_notes(args.first, args.second).items():
        print(f'{name}: varies but not counted: {", ".join(notes)}')
    if not report:
        print(f'{packages} packages compared: identical content')
        return 0
    for name, differences in report.items():
        print(f'\n{name}: {len(differences)} differences')
        for line in differences[:args.limit]:
            print(f'  {line}')
        if len(differences) > args.limit:
            print(f'  ... {len(differences) - args.limit} more')
    print(f'\n{len(report)} of {packages} packages differ', file=sys.stderr)
    return 1


if __name__ == '__main__':
    sys.exit(main())
