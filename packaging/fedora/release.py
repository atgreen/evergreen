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
# Fedora requires Release to restart at 1 when Version changes, so the stable
# release number is PAIRED with the version it belongs to: bumping the workspace
# version without updating this fails `release.py plan` instead of quietly
# shipping 0.0.2-6. egcl.spec's %egcl_release fallback must match the number
# here, which test-release.py checks.
#
# Not rpmautospec's %autorelease, and not commit counting: the release jobs use
# actions/checkout at its default depth of 1, where counting commits since the
# version last changed cannot work. A pinned pair fails loudly instead.
RPM_RELEASE = ('0.0.4', '1')
# Architectures the source RPM knows how to build. Keep this capability map in
# step with egcl.spec's ExclusiveArch and its `%ifarch x86_64` guard.
PACKAGES_BY_ARCH = {
    'x86_64': {'egcl', 'egcl-static', 'egcl-target-s390x-linux', 'egcl-target-aarch64-linux',
               'egcl-target-ppc64le-linux', 'egcl-target-windows', 'egcl-target-android',
               'egcl-target-s390x-linux-static', 'egcl-target-aarch64-linux-static',
               'egcl-target-ppc64le-linux-static', 'egcl-target-riscv64-linux-static'},
    'ppc64le': {'egcl', 'egcl-static'},
}
# The release workflow consumes builder_matrix() from its validated plan.
# This shared set keeps scheduled builders and expected release assets aligned.
RELEASE_BUILDERS = {
    ('native', 'native', 'x86_64'),
    ('s390x', 's390x', 'x86_64'),
    ('aarch64', 'aarch64', 'x86_64'),
    ('ppc64le', 'ppc64le', 'x86_64'),
    ('riscv64', 'riscv64', 'x86_64'),
    ('windows', 'windows', 'x86_64'),
    ('android', 'android', 'x86_64'),
}
# A complete published release is every package for every architecture that
# has an enabled builder. POWER-native packages remain supported by the SRPM,
# but are not release assets until the disabled builder can run with native
# OpenJDK. A failed enabled build group still blocks publication.
RELEASE_PACKAGES_BY_ARCH = {
    arch: PACKAGES_BY_ARCH[arch]
    for arch in {builder[2] for builder in RELEASE_BUILDERS}
}
PACKAGES = set().union(*RELEASE_PACKAGES_BY_ARCH.values())
PACKAGE_COUNT = sum(len(names) for names in RELEASE_PACKAGES_BY_ARCH.values())
# Provenance artifact keys. The x86_64 builders name their payloads after the
# build.py target; a non-x86_64 builder prefixes its arch, because `native` and
# `static` mean a different binary on each host and the collector merges every
# builder's record into one build.json.
RUNTIMES = {'native', 'static'} | {name.removeprefix('egcl-target-')
                                 for name in PACKAGES if name.startswith('egcl-target-')} \
         | {f'{arch}-{name}' for arch in RELEASE_PACKAGES_BY_ARCH if arch != 'x86_64'
            for name in ('native', 'static')}


def builder_matrix(group):
    """Select hosted cross builders; every entry retains its execution budget."""
    if group not in ('all', 'riscv64'):
        raise ValueError('Build group must be all or riscv64')
    return {'include': [dict(name=name, group=build_group, arch=arch,
                             platform='', timeout=120)
                        for name, build_group, arch in sorted(RELEASE_BUILDERS)
                        if group == 'all' or build_group == group]}


def make_plan(version, event, ref, run_id, attempt, mode, build_group='all'):
    builder_matrix(build_group)
    if build_group != 'all' and (event != 'workflow_dispatch' or mode != 'build'):
        raise ValueError('A partial package group requires manual build-only mode')
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Workspace version must be major.minor.patch')
    if event == 'push':
        if ref != f'refs/tags/v{version}':
            raise ValueError('Release tag must match the workspace version')
        return {'tag': f'v{version}', 'version': version, 'prerelease': False,
                'publish': True, 'rpm_release': stable_release(version)}
    if event != 'workflow_dispatch' or mode not in ('test', 'build'):
        raise ValueError('Use a version tag, or manually select test/build')
    if not re.fullmatch(r'[1-9]\d*', run_id) or not re.fullmatch(r'[1-9]\d*', attempt):
        raise ValueError('Run ID and attempt must be positive integers')
    return {'tag': f'test-v{version}-{run_id}-{attempt}', 'version': version,
            'prerelease': True, 'publish': mode == 'test',
            'rpm_release': f'0.test.{run_id}.{attempt}'}


def stable_release(version):
    """The release number for a stable tag, refusing a stale pairing."""
    paired_version, release = RPM_RELEASE
    if version != paired_version:
        raise ValueError(
            f'RPM_RELEASE is pinned to version {paired_version}, but the workspace '
            f'is {version}. Fedora resets Release on a version change: set '
            f"RPM_RELEASE = ('{version}', '1') and egcl.spec's %egcl_release to 1.")
    return release


def validate_packages(records, version, rpm_release, dist):
    """Fail before publishing if even one package is missing or from another build."""
    by_arch = {}
    for name, actual_version, actual_release, arch in records:
        if (actual_version, actual_release) != (version, f'{rpm_release}{dist}'):
            raise ValueError(f'Unexpected RPM identity: {name} {actual_version}-{actual_release}.{arch}')
        if arch not in RELEASE_PACKAGES_BY_ARCH:
            raise ValueError(f'Unexpected RPM architecture: {name} {actual_version}-{actual_release}.{arch}')
        by_arch.setdefault(arch, []).append(name)
    if missing := sorted(set(RELEASE_PACKAGES_BY_ARCH) - set(by_arch)):
        raise ValueError(f'No packages at all for: {", ".join(missing)}')
    for arch, names in sorted(by_arch.items()):
        expected = RELEASE_PACKAGES_BY_ARCH[arch]
        # The length test is not redundant with the set test: it is what
        # catches the same package appearing twice for one architecture.
        if len(names) != len(expected) or set(names) != expected:
            raise ValueError(f'Expected all {len(expected)} {arch} RPMs, got: {sorted(names)}')


def merge_provenance(records, srpm_sha256):
    merged = {'artifacts': {}, 'rpms': [], 'android_ndk': None, 'srpm_sha256': srpm_sha256}
    inputs = set()
    for record in records:
        if record['srpm_sha256'] != srpm_sha256:
            raise ValueError('Builders used different source RPMs')
        for key in ('git', 'rustc', 'sysroot_release', 'dist'):
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
        missing = sorted(RUNTIMES - set(merged['artifacts']))
        raise ValueError(f'Provenance must cover all {len(RUNTIMES)} runtimes; '
                         f'missing: {", ".join(missing) or "none"}')
    merged['rpms'] = sorted(inputs)
    return merged


def unwrap_release_prose(body):
    """Remove source wrapping from prose; leave Markdown blocks and hard breaks."""
    output = []
    paragraph_indent = None
    list_indent = 0
    fence = None
    fence_indent = 0
    literal_block = False
    for line in body.splitlines():
        expanded = line.expandtabs(4)
        indent = len(expanded) - len(expanded.lstrip(' '))
        marker = re.match(r'^ *(`{3,}|~{3,})(.*)$', expanded)
        if fence:
            output.append(line)
            if (marker and indent <= fence_indent + 3 and marker[1][0] == fence[0] and len(marker[1]) >= len(fence)
                    and not marker[2].strip()):
                fence = None
            continue
        if not line.strip():
            output.append(line)
            paragraph_indent = None
            literal_block = False
            continue
        if indent < list_indent:
            list_indent = indent
        # Four spaces beyond the surrounding list content is indented code.
        # Keep ambiguous blocks (quotes, tables, HTML, link definitions) intact.
        if literal_block or indent >= list_indent + 4:
            output.append(line)
            paragraph_indent = None
            continue
        if marker:
            output.append(line)
            paragraph_indent = None
            fence = marker[1]
            fence_indent = list_indent
            continue
        if '|' in line or re.match(r'^ *(?:>|<|\[[^]]+\]:)', expanded):
            output.append(line)
            paragraph_indent = None
            literal_block = True
            continue
        if re.match(r'^ *(?:#{1,6}(?:\s|$)|(?:[-*_]\s*){3,}$|(?:=+|-+)\s*$)', expanded):
            output.append(line)
            paragraph_indent = None
            continue
        if item := re.match(r'^ *(?:[-+*]|[0-9]+[.)])[ \t]+', expanded):
            output.append(line)
            list_indent = paragraph_indent = item.end()
            continue
        previous = output[-1] if output else ''
        hard_break = (previous.endswith('  ')
                      or (len(previous) - len(previous.rstrip('\\'))) % 2 == 1)
        if (paragraph_indent is not None and not hard_break
                and paragraph_indent <= indent < paragraph_indent + 4):
            output[-1] = previous.rstrip(' \t') + ' ' + line.lstrip(' \t')
        else:
            output.append(line)
            paragraph_indent = indent
    return '\n'.join(output)


def release_notes(changelog, plan):
    """Select one level-two changelog section, ignoring headings in fenced examples."""
    wanted = 'Unreleased' if plan['prerelease'] else plan['version']
    lines = changelog.splitlines(keepends=True)
    headings = []
    fence = None
    for index, line in enumerate(lines):
        marker = re.match(r'^ {0,3}(`{3,}|~{3,})(.*)$', line)
        if fence:
            if (marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence)
                    and not marker[2].strip()):
                fence = None
            continue
        if marker:
            fence = marker[1]
            continue
        if heading := re.fullmatch(r'##[ \t]+(.+?)\s*', line):
            headings.append((index, heading[1]))
    matches = []
    for offset, (start, title) in enumerate(headings):
        if re.fullmatch(re.escape(wanted) + r'(?: - \d{4}-\d{2}-\d{2})?', title):
            end = headings[offset + 1][0] if offset + 1 < len(headings) else len(lines)
            matches.append((lines[start].rstrip(), ''.join(lines[start + 1:end]).strip('\r\n')))
    if len(matches) != 1:
        raise ValueError(f'CHANGELOG.md must contain exactly one ## {wanted} section '
                         f'(found {len(matches)})')
    heading, body = matches[0]
    if not body.strip():
        if not plan['prerelease']:
            raise ValueError(f'CHANGELOG.md section {wanted} must not be empty')
        body = 'No unreleased changes recorded.'
    return f'{heading}\n\n{unwrap_release_prose(body)}\n'


def collect(rpm_dirs, destination, plan, source_rpm, provenance_dir, sbom=None):
    notes = release_notes((ROOT / 'CHANGELOG.md').read_text(encoding='utf-8'), plan)
    with source_rpm.open('rb') as stream:
        source_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    # Merge first: the builders' agreed %dist is what package identity is
    # checked against, so it has to be known before validating anything.
    provenance = merge_provenance(
        [json.loads(path.read_text()) for path in sorted(provenance_dir.glob('*.json'))], source_digest)
    dist = provenance['dist']
    # One directory per architecture (rpmbuild writes RPMS/<arch>/), so sort by
    # filename across all of them rather than by directory.
    rpms = sorted((rpm for directory in rpm_dirs for rpm in directory.glob('*.rpm')),
                  key=lambda path: path.name)
    if len({rpm.name for rpm in rpms}) != len(rpms):
        raise ValueError('Two build groups produced the same RPM filename')
    records = []
    for rpm in rpms:
        identity = subprocess.check_output(
            ['rpm', '-qp', '--queryformat', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}', str(rpm)],
            text=True)
        records.append(identity.split('\t'))
    validate_packages(records, plan['version'], plan['rpm_release'], dist)
    identity = subprocess.check_output(
        ['rpm', '-qp', '--queryformat', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{SOURCEPACKAGE}', str(source_rpm)], text=True)
    if identity.split('\t') != ['egcl', plan['version'], f'{plan["rpm_release"]}{dist}', '1']:
        raise ValueError(f'Unexpected source RPM identity: {identity}')
    destination.mkdir(parents=True, exist_ok=True)
    # A fresh destination prevents stale files being attached to a new release.
    if any(destination.iterdir()):
        raise ValueError(f'Release destination must be empty: {destination}')
    for rpm in rpms:
        shutil.copy2(rpm, destination / rpm.name)
    shutil.copy2(source_rpm, destination / source_rpm.name)
    shutil.copy2(ROOT / 'CHANGELOG.md', destination / 'CHANGELOG.md')
    (destination / 'RELEASE_NOTES.md').write_text(notes, encoding='utf-8')
    shutil.copy2(PUBLIC_KEY, destination / PUBLIC_KEY.name)
    if sbom:
        shutil.copy2(sbom, destination / sbom.name)
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
    if len(rpms) != PACKAGE_COUNT + 1:
        raise ValueError(f'Expected {PACKAGE_COUNT + 1} packages to sign, got {len(rpms)}')
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
    sign_manifest(assets, passphrase_file, public_key)
    return rpms


def sign_manifest(assets, passphrase_file, public_key=None, gpg='gpg'):
    """Detached-sign SHA256SUMS, so the manifest can be trusted away from GitHub.

    The checksum file is what ties the individual digests together; unsigned, it
    can be replaced alongside the artifacts it describes. Signing it gives a
    check that needs nothing but gpg and our public key -- no network, no
    GitHub API, no attestation service.
    """
    signature = assets / 'SHA256SUMS.asc'
    signature.unlink(missing_ok=True)
    subprocess.run(
        [gpg, '--batch', '--yes', '--pinentry-mode', 'loopback',
         '--passphrase-file', str(passphrase_file), '--local-user', GPG_KEY_NAME,
         '--detach-sign', '--armor', '--output', str(signature),
         str(assets / 'SHA256SUMS')], check=True)
    verify_manifest(assets / 'SHA256SUMS', signature, public_key)
    return signature


def verify_manifest(manifest, signature, public_key=None, gpg='gpg'):
    """Check the detached signature the way a user will: only the public key.

    A separate GNUPGHOME holding one key is the point -- verifying inside the
    signing keyring would also succeed if the signature were made by some other
    key that happens to be present there.
    """
    public_key = public_key or PUBLIC_KEY
    with tempfile.TemporaryDirectory() as home:
        environment = os.environ | {'GNUPGHOME': home}
        subprocess.run([gpg, '--batch', '--quiet', '--import', str(public_key)],
                       check=True, env=environment)
        subprocess.run([gpg, '--batch', '--verify', str(signature), str(manifest)],
                       check=True, env=environment)


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
    # Repeatable: rpmbuild writes RPMS/<arch>/, so a release spanning two
    # architectures hands the collector one directory for each.
    parser.add_argument('--rpm-dir', type=Path, action='append', dest='rpm_dirs',
                        metavar='DIR', help='Directory of built RPMs (repeat per architecture)')
    parser.add_argument('--destination', type=Path, default=Path('target/release-assets'))
    parser.add_argument('--source-rpm', type=Path)
    parser.add_argument('--provenance-dir', type=Path)
    parser.add_argument('--assets', type=Path, default=Path('target/release-assets'))
    parser.add_argument('--passphrase-file', type=Path)
    parser.add_argument('--sbom', type=Path)
    args = parser.parse_args()
    if args.command == 'plan':
        version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
        plan = make_plan(version, os.environ['GITHUB_EVENT_NAME'], os.environ['GITHUB_REF'],
                         os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT'],
                         os.environ.get('RELEASE_MODE', ''),
                         os.environ.get('RELEASE_BUILD_GROUP') or 'all')
        group = os.environ.get('RELEASE_BUILD_GROUP') or 'all'
        plan['build_group'] = group
        release_notes((ROOT / 'CHANGELOG.md').read_text(encoding='utf-8'), plan)
        args.plan.parent.mkdir(parents=True, exist_ok=True)
        args.plan.write_text(json.dumps(plan, indent=2) + '\n')
        if output := os.environ.get('GITHUB_OUTPUT'):
            with open(output, 'a') as stream:
                stream.write(f'plan_json={json.dumps(plan)}\n')
                stream.write(f'builders_json={json.dumps(builder_matrix(group))}\n')
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
        rpm_dirs = args.rpm_dirs or [Path(f'target/fedora-rpm/RPMS/{arch}')
                                     for arch in sorted(RELEASE_PACKAGES_BY_ARCH)]
        collect(rpm_dirs, args.destination, json.loads(args.plan.read_text()),
                args.source_rpm, args.provenance_dir, args.sbom)


if __name__ == '__main__':
    main()
