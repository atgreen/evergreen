#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Create one source-only SRPM, or rebuild one package group from that SRPM."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
BUILDER = runpy.run_path(str(Path(__file__).with_name('build.py')))
MUSL = runpy.run_path(str(Path(__file__).with_name('prepare-musl.py')))
OUTPUT = ROOT / 'target/fedora-rpm'
# rpmbuild writes RPMS/<arch>/, and the release downloads every builder's
# provenance into one directory -- so a builder on a second architecture has to
# look in its own RPMS directory and name its record distinctly. The POWER
# builder's group is `native`, the same name the x86_64 builder uses.
HOST_MACHINE = BUILDER['HOST_MACHINE']


def substitute(spec, pattern, replacement):
    """Rewrite one spec line, failing if the line it targets is not there.

    str.replace is silent when it matches nothing, which would hand rpmbuild a
    spec still carrying %{egcl_version} or the fallback release number.
    """
    rewritten, count = re.subn(pattern, lambda _: replacement, spec, count=1)
    if count != 1:
        raise ValueError(f'Spec line not found for substitution: {pattern}')
    return rewritten


def create(plan, output=OUTPUT):
    sources = output / 'SOURCES'
    sources.mkdir(parents=True, exist_ok=True)
    BUILDER['source_archive'](output, sources, include_std=True)
    MUSL['source'](sources, f'https://musl.libc.org/releases/musl-{MUSL["MUSL_VERSION"]}.tar.gz',
                   MUSL['MUSL_SHA256'], f'musl-{MUSL["MUSL_VERSION"]}')
    version = MUSL['UNWIND_VERSION']
    MUSL['source'](sources, f'https://github.com/llvm/llvm-project/releases/download/'
                   f'llvmorg-{version}/libunwind-{version}.src.tar.xz',
                   MUSL['UNWIND_SHA256'], f'libunwind-{version}.src')
    spec = Path(__file__).with_name('egcl.spec').read_text()
    spec = substitute(spec, r'Version: %\{egcl_version\}', f'Version: {plan["version"]}')
    # Matches any fallback number, so changing the spec's %egcl_release (which
    # happens on every version bump) cannot silently stop this substitution and
    # leave the SRPM carrying the fallback instead of the planned release.
    spec = substitute(spec, r'%\{!\?egcl_release:%global egcl_release \d+\}',
                      f'%global egcl_release {plan["rpm_release"]}')
    specs = output / 'SPECS'
    specs.mkdir(exist_ok=True)
    path = specs / 'egcl.spec'
    path.write_text(spec)
    subprocess.run(['rpmbuild', '-bs', str(path), '--define', f'_topdir {output}',
                    '--define', 'egcl_rustup 1'], check=True)


def expected_packages(group):
    """The RPM names a group's build must produce.

    The runtime names build.py uses and the package names rpm reports differ
    for exactly two entries, so the mapping is spelled out rather than derived
    from a prefix rule that would quietly mis-name them.
    """
    return [('egcl' if name == 'native' else 'egcl-static' if name == 'static' else
             f'egcl-target-{name}') for name in BUILDER['GROUPS'][group]]


def provenance_name(group):
    """The record filename for a group built on this host.

    Every builder's provenance is downloaded into one directory, so two
    builders must never pick the same name. `native` is built on x86_64 AND on
    POWER, so off x86_64 the arch goes in front -- matching how build.py keys
    its artifact digests and what release.py's RUNTIMES expects.
    """
    return group if HOST_MACHINE == 'x86_64' else f'{HOST_MACHINE}-{group}'


def rebuild(group, srpm, output=OUTPUT, tools=None):
    srpm = srpm.resolve()
    sources = output / 'SOURCES'
    sources.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryFile(dir=output) as archive:
        subprocess.run(['rpm2cpio', str(srpm)], stdout=archive, check=True)
        archive.seek(0)
        subprocess.run(['cpio', '-idmu', '--quiet', '--no-absolute-filenames'],
                        stdin=archive, cwd=sources, check=True)
    tools = tools or output / 'tools'
    tools.mkdir(exist_ok=True)
    # Use the exact musl/unwinder sources carried by this SRPM.
    for name in (f'musl-{MUSL["MUSL_VERSION"]}.tar.gz',
                 f'libunwind-{MUSL["UNWIND_VERSION"]}.src.tar.xz'):
        shutil.copy2(sources / name, tools / name)
    subprocess.run(['bash', str(ROOT / 'packaging/fedora/prepare-tools.sh'), group],
                    env=os.environ | {'EGCL_RPM_TOOLS': str(tools)}, check=True)
    subprocess.run(['rpmbuild', '--rebuild', '--noclean', str(srpm),
                    '--define', f'_topdir {output}', '--define', 'egcl_rustup 1',
                    '--define', f'egcl_build_group {group}', '--define', f'egcl_tools {tools}'], check=True)
    names = [subprocess.check_output(['rpm', '-qp', '--queryformat', '%{NAME}', str(path)], text=True)
             for path in (output / 'RPMS' / HOST_MACHINE).glob('*.rpm')]
    expected = expected_packages(group)
    if sorted(names) != sorted(expected):
        raise RuntimeError(f'Wrong RPM set for {group}: {names}')
    stages = list((output / 'BUILD').glob('**/egcl-source/target/fedora-rpm/stage'))
    if len(stages) != 1:
        raise RuntimeError(f'Expected one source build stage, found {stages}')
    stage = stages[0]
    BUILDER['extract_and_verify'](output, stage, BUILDER['GROUPS'][group])
    metadata = json.loads((stage.parents[2] / 'build-provenance.json').read_text())
    with srpm.open('rb') as stream:
        metadata['srpm_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    destination = ROOT / 'target/build-provenance'
    destination.mkdir(exist_ok=True)
    (destination / f'{provenance_name(group)}.json').write_text(json.dumps(metadata, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=OUTPUT)
    parser.add_argument('--tools', type=Path)
    commands = parser.add_subparsers(dest='command', required=True)
    source = commands.add_parser('create')
    source.add_argument('--plan', type=Path, default=ROOT / 'target/release-plan.json')
    binary = commands.add_parser('rebuild')
    binary.add_argument('group', choices=BUILDER['GROUPS'])
    binary.add_argument('srpm', type=Path)
    args = parser.parse_args()
    if args.command == 'create':
        create(json.loads(args.plan.read_text()), args.output.resolve())
    else:
        rebuild(args.group, args.srpm, args.output.resolve(),
                args.tools.resolve() if args.tools else None)
