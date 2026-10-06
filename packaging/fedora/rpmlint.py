#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Run rpmlint against a ratcheted baseline.

rpmlint reports the review-class problems a Fedora reviewer would raise first,
and this package already has known ones (bliss-g8wly). A pass/fail gate on
"zero findings" would therefore have to be switched off, which is the same as
not having it. A ratchet works instead: the findings in the baseline are
tolerated, anything NEW fails, and a baseline entry that stops being reported
also fails so the baseline is tightened rather than left to rot.

Same shape as scripts/gc-root-lint.sh, which ratchets the GC rooting lint.
"""
import argparse
import importlib.util
from pathlib import Path
import re
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
SPEC = HERE / 'egcl.spec'
BASELINE = HERE / 'rpmlint-baseline.txt'
# rpmlint prefixes each finding with the file it came from. The spec is rendered
# to a temporary path, so that prefix is replaced by a stable name before the
# finding is compared to the baseline.
SPEC_NAME = 'egcl.spec'
FINDING = re.compile(r'^(?P<subject>\S+?): (?P<level>[EW]): (?P<rest>.*)$')


def render_spec(version='0.0.2', release='1'):
    """The spec as rpmbuild would see it, with the release macros resolved.

    rpmlint on the checked-in spec would report nothing useful: Version is
    %{egcl_version} and the release macros are undefined there.
    """
    spec_module = importlib.util.spec_from_file_location(
        'egcl_source_rpm', HERE / 'source-rpm.py')
    source_rpm = importlib.util.module_from_spec(spec_module)
    spec_module.loader.exec_module(source_rpm)
    substitute = source_rpm.substitute
    spec = SPEC.read_text()
    spec = substitute(spec, r'Version: %\{egcl_version\}', f'Version: {version}')
    spec = substitute(spec, r'%\{!\?egcl_release:%global egcl_release \d+\}',
                      f'%global egcl_release {release}')
    return spec


def findings(targets):
    """Normalised `LEVEL: check detail` lines, sorted and deduplicated."""
    report = subprocess.run(['rpmlint', *map(str, targets)],
                            text=True, capture_output=True).stdout
    collected = set()
    for line in report.splitlines():
        if match := FINDING.match(line.strip()):
            subject = Path(match['subject']).name
            if subject.endswith('.spec'):
                subject = SPEC_NAME
            collected.add(f'{subject}: {match["level"]}: {match["rest"]}')
    return sorted(collected)


def read_baseline(path=BASELINE):
    if not path.exists():
        return None
    return sorted({line.strip() for line in path.read_text().splitlines()
                   if line.strip() and not line.startswith('#')})


def write_baseline(current, path=BASELINE):
    path.write_text(
        '# rpmlint findings this package is known to produce, tolerated by\n'
        '# packaging/fedora/rpmlint.py. A NEW finding fails; a finding here that\n'
        '# is no longer reported also fails, so fixing one means removing its\n'
        '# line. Regenerate with: python3 packaging/fedora/rpmlint.py --update\n'
        + ''.join(f'{finding}\n' for finding in current))


def compare(current, baseline):
    """New findings and stale baseline entries, either of which fails the gate."""
    return sorted(set(current) - set(baseline)), sorted(set(baseline) - set(current))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('rpms', nargs='*', type=Path,
                        help='packages to lint as well as the rendered spec')
    parser.add_argument('--baseline', type=Path, default=BASELINE)
    parser.add_argument('--update', action='store_true', help='rewrite the baseline')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        rendered = Path(directory) / 'egcl.spec'
        rendered.write_text(render_spec())
        current = findings([rendered, *args.rpms])
    if args.update:
        write_baseline(current, args.baseline)
        print(f'{args.baseline}: {len(current)} findings recorded')
        return 0
    baseline = read_baseline(args.baseline)
    if baseline is None:
        # Report-only until a baseline exists, so adding this gate cannot fail
        # a release before anyone has looked at what it says.
        print(f'No baseline at {args.baseline}; {len(current)} findings, reporting only:')
        print('\n'.join(f'  {finding}' for finding in current))
        return 0
    new, stale = compare(current, baseline)
    for finding in new:
        print(f'NEW      {finding}')
    for finding in stale:
        print(f'STALE    {finding}  (fixed? remove it from the baseline)')
    if not new and not stale:
        print(f'rpmlint: {len(current)} known findings, none new')
        return 0
    print(f'\n{len(new)} new and {len(stale)} stale findings. '
          f'Review, then run: python3 {Path(__file__).name} --update', file=sys.stderr)
    return 1


if __name__ == '__main__':
    sys.exit(main())
