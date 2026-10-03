#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Plan Fedora releases, sign the packages, and validate the complete RPM set."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PUBLIC_KEY = Path(__file__).resolve().with_name('RPM-GPG-KEY-egcl')
GPG_KEY_NAME = 'EGCL RPM Signing Key'
PACKAGES = {'egcl', 'egcl-static', 'egcl-target-s390x-linux', 'egcl-target-aarch64-linux',
            'egcl-target-ppc64le-linux', 'egcl-target-windows', 'egcl-target-android',
            'egcl-target-s390x-linux-static', 'egcl-target-aarch64-linux-static',
            'egcl-target-ppc64le-linux-static'}
RUNTIMES = {'native', 'static'} | {name.removeprefix('egcl-target-')
                                 for name in PACKAGES if name.startswith('egcl-target-')}


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


def merge_provenance(records, srpm_sha256):
    merged = {'artifacts': {}, 'rpms': [], 'android_ndk': None, 'srpm_sha256': srpm_sha256}
    inputs = set()
    for record in records:
        if record['srpm_sha256'] != srpm_sha256:
            raise ValueError('Builders used different source RPMs')
        for key in ('git', 'rustc', 'sysroot_release'):
            if key in merged and merged[key] != record[key]:
                raise ValueError(f'Builders disagree on {key}')
            merged[key] = record[key]
        for key in ('android_ndk', 's390x_musl'):
            if record.get(key) is not None:
                if merged.get(key) is not None and merged[key] != record[key]:
                    raise ValueError(f'Builders disagree on {key}')
                merged[key] = record[key]
        for name, digest in record['artifacts'].items():
            if name in merged['artifacts'] or name not in RUNTIMES:
                raise ValueError(f'Duplicate or unknown runtime: {name}')
            merged['artifacts'][name] = digest
        inputs.update(record['rpms'])
    if set(merged['artifacts']) != RUNTIMES:
        raise ValueError('Provenance must cover all ten runtimes')
    merged['rpms'] = sorted(inputs)
    return merged


def collect(rpm_dir, destination, plan, source_rpm, provenance_dir):
    rpms = sorted(rpm_dir.glob('*.rpm'))
    records = []
    for rpm in rpms:
        identity = subprocess.check_output(
            ['rpm', '-qp', '--queryformat', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}', str(rpm)],
            text=True)
        records.append(identity.split('\t'))
    validate_packages(records, plan['version'], plan['rpm_release'])
    identity = subprocess.check_output(
        ['rpm', '-qp', '--queryformat', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{SOURCEPACKAGE}', str(source_rpm)], text=True)
    if identity.split('\t') != ['egcl', plan['version'], f'{plan["rpm_release"]}.fc44', '1']:
        raise ValueError(f'Unexpected source RPM identity: {identity}')
    with source_rpm.open('rb') as stream:
        source_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    provenance = merge_provenance(
        [json.loads(path.read_text()) for path in sorted(provenance_dir.glob('*.json'))], source_digest)
    destination.mkdir(parents=True, exist_ok=True)
    # A fresh destination prevents stale files being attached to a new release.
    if any(destination.iterdir()):
        raise ValueError(f'Release destination must be empty: {destination}')
    for rpm in rpms:
        shutil.copy2(rpm, destination / rpm.name)
    shutil.copy2(source_rpm, destination / source_rpm.name)
    shutil.copy2(ROOT / 'CHANGELOG.md', destination / 'CHANGELOG.md')
    shutil.copy2(PUBLIC_KEY, destination / PUBLIC_KEY.name)
    (destination / 'build.json').write_text(json.dumps(provenance, indent=2) + '\n')
    (destination / 'release.json').write_text(json.dumps(plan, indent=2) + '\n')
    write_checksums(destination)


def write_checksums(destination):
    """Rewrite SHA256SUMS over every other asset, replacing any earlier manifest."""
    manifest = destination / 'SHA256SUMS'
    manifest.unlink(missing_ok=True)
    checksums = []
    for path in sorted(destination.iterdir()):
        with path.open('rb') as source:
            digest = hashlib.file_digest(source, 'sha256').hexdigest()
        checksums.append(f'{digest}  {path.name}\n')
    manifest.write_text(''.join(checksums))


def sign(assets, passphrase_file, public_key=None, gpg='/usr/bin/gpg'):
    """Sign every package in place, prove it, then refresh the checksum manifest.

    Signing rewrites the RPM header, so this has to run before SHA256SUMS is
    final -- and the caller should verify the pre-signing manifest first, so the
    hand-off from the build jobs is still checked.
    """
    public_key = public_key or PUBLIC_KEY
    rpms = sorted(assets.glob('*.rpm'))
    if len(rpms) != len(PACKAGES) + 1:
        raise ValueError(f'Expected {len(PACKAGES) + 1} packages to sign, got {len(rpms)}')
    subprocess.run(
        ['rpmsign',
         # Debian/Ubuntu's rpm defaults %__gpg to /usr/bin/gpg2, which Ubuntu
         # does not ship, and rpmsign then fails with "Could not exec gpg".
         '--define', f'__gpg {gpg}',
         '--define', f'_gpg_name {GPG_KEY_NAME}',
         # Unattended signing: no tty and no agent prompt on a CI runner.
         '--define', f'_gpg_sign_cmd_extra_args '
                     f'--pinentry-mode loopback --passphrase-file {passphrase_file}',
         '--addsign', *map(str, rpms)], check=True)
    verify_signatures(rpms, public_key)
    write_checksums(assets)
    return rpms


def verify_signatures(rpms, public_key=None):
    """Fail unless every package carries a signature made by our key.

    A keyring holding only our public key answers "signed by us"; the output has
    to be read as well, because `rpmkeys --checksig` EXITS 0 FOR AN UNSIGNED
    PACKAGE -- it reports "digests OK" and says nothing about signatures, so
    trusting the exit status alone would wave unsigned RPMs through.
    """
    public_key = public_key or PUBLIC_KEY
    with tempfile.TemporaryDirectory() as keyring:
        subprocess.run(['rpmkeys', '--dbpath', keyring, '--import', str(public_key)], check=True)
        report = subprocess.run(
            ['rpmkeys', '--dbpath', keyring, '--checksig', *map(str, rpms)],
            check=True, text=True, capture_output=True).stdout
    verified = {line.split(':')[0] for line in report.splitlines() if 'signatures OK' in line}
    unverified = [rpm.name for rpm in rpms if not any(name.endswith(rpm.name) for name in verified)]
    if unverified:
        raise ValueError(f'Packages are not signed by {GPG_KEY_NAME}: {unverified}\n{report}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('plan', 'collect', 'sign'))
    parser.add_argument('--plan', type=Path, default=Path('target/release-plan.json'))
    parser.add_argument('--rpm-dir', type=Path, default=Path('target/fedora-rpm/RPMS/x86_64'))
    parser.add_argument('--destination', type=Path, default=Path('target/release-assets'))
    parser.add_argument('--source-rpm', type=Path)
    parser.add_argument('--provenance-dir', type=Path)
    parser.add_argument('--assets', type=Path, default=Path('target/release-assets'))
    parser.add_argument('--passphrase-file', type=Path)
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
                stream.write(f'plan_json={json.dumps(plan)}\n')
                for key, value in plan.items():
                    stream.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')
        print(json.dumps(plan, indent=2))
    elif args.command == 'sign':
        if not args.passphrase_file:
            parser.error('sign requires --passphrase-file')
        for rpm in sign(args.assets, args.passphrase_file):
            print(f'signed {rpm.name}')
    else:
        if not args.source_rpm or not args.provenance_dir:
            parser.error('collect requires --source-rpm and --provenance-dir')
        collect(args.rpm_dir, args.destination, json.loads(args.plan.read_text()),
                args.source_rpm, args.provenance_dir)


if __name__ == '__main__':
    main()
