#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Plan Fedora releases and validate the complete RPM set before publishing."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PACKAGES = {'egcl', 'egcl-static', 'egcl-target-s390x-linux', 'egcl-target-aarch64-linux',
            'egcl-target-ppc64le-linux', 'egcl-target-windows', 'egcl-target-android',
            'egcl-target-s390x-linux-static', 'egcl-target-aarch64-linux-static',
            'egcl-target-ppc64le-linux-static'}


def make_plan(version, event, ref, run_id, attempt, mode):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Workspace version must be major.minor.patch')
    if event == 'push':
        if ref != f'refs/tags/v{version}':
            raise ValueError('Release tag must match the workspace version')
        return {'tag': f'v{version}', 'version': version, 'prerelease': False,
                'publish': True, 'rpm_release': '6'}
    if event != 'workflow_dispatch' or mode not in ('test', 'build'):
        raise ValueError('Use a version tag, or manually select test/build')
    if not re.fullmatch(r'[1-9]\d*', run_id) or not re.fullmatch(r'[1-9]\d*', attempt):
        raise ValueError('Run ID and attempt must be positive integers')
    return {'tag': f'test-v{version}-{run_id}-{attempt}', 'version': version,
            'prerelease': True, 'publish': mode == 'test',
            'rpm_release': f'0.test.{run_id}.{attempt}'}


def validate_packages(records, version, rpm_release):
    """Fail before publishing if even one package is missing or from another build."""
    names = []
    for name, actual_version, actual_release, arch in records:
        if (actual_version, actual_release, arch) != (version, f'{rpm_release}.fc44', 'x86_64'):
            raise ValueError(f'Unexpected RPM identity: {name} {actual_version}-{actual_release}.{arch}')
        names.append(name)
    if len(names) != len(PACKAGES) or set(names) != PACKAGES:
        raise ValueError(f'Expected all {len(PACKAGES)} RPMs, got: {sorted(names)}')


def collect(rpm_dir, destination, plan):
    rpms = sorted(rpm_dir.glob('*.rpm'))
    records = []
    for rpm in rpms:
        identity = subprocess.check_output(
            ['rpm', '-qp', '--queryformat', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}', str(rpm)],
            text=True)
        records.append(identity.split('\t'))
    validate_packages(records, plan['version'], plan['rpm_release'])
    destination.mkdir(parents=True, exist_ok=True)
    # A fresh destination prevents stale files being attached to a new release.
    if any(destination.iterdir()):
        raise ValueError(f'Release destination must be empty: {destination}')
    for rpm in rpms:
        shutil.copy2(rpm, destination / rpm.name)
    shutil.copy2(ROOT / 'CHANGELOG.md', destination / 'CHANGELOG.md')
    provenance = ROOT / 'target/fedora-rpm/stage/usr/share/doc/egcl/build.json'
    shutil.copy2(provenance, destination / 'build.json')
    (destination / 'release.json').write_text(json.dumps(plan, indent=2) + '\n')
    checksums = []
    for path in sorted(destination.iterdir()):
        with path.open('rb') as source:
            digest = hashlib.file_digest(source, 'sha256').hexdigest()
        checksums.append(f'{digest}  {path.name}\n')
    (destination / 'SHA256SUMS').write_text(''.join(checksums))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('plan', 'collect'))
    parser.add_argument('--plan', type=Path, default=Path('target/release-plan.json'))
    parser.add_argument('--rpm-dir', type=Path, default=Path('target/fedora-rpm/RPMS/x86_64'))
    parser.add_argument('--destination', type=Path, default=Path('target/release-assets'))
    args = parser.parse_args()
    if args.command == 'plan':
        version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
        plan = make_plan(version, os.environ['GITHUB_EVENT_NAME'], os.environ['GITHUB_REF'],
                         os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT'],
                         os.environ.get('RELEASE_MODE', ''))
        args.plan.parent.mkdir(parents=True, exist_ok=True)
        args.plan.write_text(json.dumps(plan, indent=2) + '\n')
        if output := os.environ.get('GITHUB_OUTPUT'):
            with open(output, 'a') as stream:
                for key, value in plan.items():
                    stream.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')
        print(json.dumps(plan, indent=2))
    else:
        collect(args.rpm_dir, args.destination, json.loads(args.plan.read_text()))


if __name__ == '__main__':
    main()
