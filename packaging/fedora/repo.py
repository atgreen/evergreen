#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Build the dnf repositories published on GitHub Pages.

Pages carries only metadata. Each package's <location> keeps an xml:base
pointing at the GitHub release it was published in, so the RPMs are served by
the release CDN that already holds them -- 16 KB of repodata per release
against 101 MiB of packages.

Two channels: `release` accumulates every tagged release, so dnf can downgrade
and pin; `test` holds the most recent test builds and is capped to match the
prerelease pruning in release.yml, since a channel referencing a deleted
release would 404.

Accumulation works because mergerepo_c PRESERVES each source repo's xml:base:
one merged repository can therefore carry packages from many releases, each
pointing at its own. Per-release metadata is generated once, where the packages
already are, and kept in the store; nothing is ever re-downloaded.

Layout, with $releasever and $basearch left for dnf to substitute so a new
architecture needs a new directory and no change to the .repo files:

    <store>/per-release/<channel>/<dist>/<arch>/<tag>/repodata/   generated once
    <output>/repo/<channel>/<dist>/<arch>/repodata/               merged, signed
    <output>/repo/RPM-GPG-KEY-egcl
    <output>/repo/egcl.repo, egcl-testing.repo
"""
import argparse
import importlib.util
from pathlib import Path
import re
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
CHANNELS = ('release', 'test')
# Matches release.yml's prune, which keeps the newest three test prereleases.
TEST_CHANNEL_KEEP = 3


def _release_module():
    spec = importlib.util.spec_from_file_location('egcl_release', HERE / 'release.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = _release_module()


def dist_directory(dist):
    """`.fc44` -> `fc44`, the directory name `fc$releasever` expands to."""
    if not re.fullmatch(r'\.\w+', dist):
        raise ValueError(f'Unexpected dist suffix: {dist!r}')
    return dist.lstrip('.')


def base_url(repository, tag):
    return f'https://github.com/{repository}/releases/download/{tag}/'


def architectures(rpms):
    """The binary architectures present, ignoring the source RPM.

    The SRPM stays a release asset: a source repository is a separate thing
    and dnf does not want it in a binary repo.
    """
    found = {}
    for rpm in sorted(rpms):
        if rpm.name.endswith('.src.rpm'):
            continue
        arch = subprocess.check_output(
            ['rpm', '-qp', '--queryformat', '%{ARCH}', str(rpm)],
            text=True, stderr=subprocess.DEVNULL).strip()
        found.setdefault(arch, []).append(rpm)
    return found


def generate(rpms, store, channel, dist, tag, repository):
    """Record this release's metadata, one repository per architecture."""
    if channel not in CHANNELS:
        raise ValueError(f'Unknown channel {channel!r}; expected one of {CHANNELS}')
    written = []
    for arch, packages in architectures(rpms).items():
        staging = store / 'per-release' / channel / dist_directory(dist) / arch / tag
        if staging.exists():
            shutil.rmtree(staging)
        staging.mkdir(parents=True)
        for package in packages:
            # Hard-link where possible: createrepo_c reads the files, and the
            # publish job has them already.
            shutil.copy2(package, staging / package.name)
        subprocess.run(['createrepo_c', '--baseurl', base_url(repository, tag), str(staging)],
                       check=True, stdout=subprocess.DEVNULL)
        for package in staging.glob('*.rpm'):
            package.unlink()
        written.append(staging)
    return written


def stored_releases(store, channel, dist, arch):
    """Release tags held for a channel, newest-committed last.

    Sorted by the per-release directory's mtime rather than by tag text: test
    tags carry a run id and stable tags a version, so there is no one ordering
    that sorts both.
    """
    root = store / 'per-release' / channel / dist_directory(dist) / arch
    if not root.exists():
        return []
    return [path.name for path in sorted(root.iterdir(), key=lambda p: p.stat().st_mtime)
            if (path / 'repodata').is_dir()]


def prune(store, channel, dist, arch, keep):
    """Drop the oldest per-release metadata beyond `keep`."""
    tags = stored_releases(store, channel, dist, arch)
    root = store / 'per-release' / channel / dist_directory(dist) / arch
    removed = tags[:max(0, len(tags) - keep)]
    for tag in removed:
        shutil.rmtree(root / tag)
    return removed


def merge(store, channel, dist, arch, output):
    """Merge a channel's per-release metadata into one published repository."""
    tags = stored_releases(store, channel, dist, arch)
    if not tags:
        return None
    sources = [store / 'per-release' / channel / dist_directory(dist) / arch / tag
               for tag in tags]
    destination = output / 'repo' / channel / dist_directory(dist) / arch
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    if len(sources) == 1:
        # mergerepo_c wants two or more repositories; one release needs no merge.
        shutil.copytree(sources[0] / 'repodata', destination / 'repodata')
    else:
        subprocess.run(
            ['mergerepo_c', '--all', *(f'--repo={source}' for source in sources),
             f'--outputdir={destination}'], check=True, stdout=subprocess.DEVNULL)
    return destination


def sign_repomd(repodata, passphrase_file, gpg='gpg'):
    """Detached-sign repomd.xml, which is what repo_gpgcheck=1 verifies.

    Signing the packages is not enough on its own: unsigned metadata can be
    swapped to hide an update or offer an older vulnerable package that is
    still correctly signed.
    """
    repomd = repodata / 'repomd.xml'
    signature = repodata / 'repomd.xml.asc'
    signature.unlink(missing_ok=True)
    subprocess.run(
        [gpg, '--batch', '--yes', '--pinentry-mode', 'loopback',
         '--passphrase-file', str(passphrase_file), '--local-user', release.GPG_KEY_NAME,
         '--detach-sign', '--armor', '--output', str(signature), str(repomd)], check=True)
    release.verify_manifest(repomd, signature)
    return signature


def write_client_files(output, repository, pages_url):
    """The .repo files users install, and the key they verify everything with."""
    repo = output / 'repo'
    repo.mkdir(parents=True, exist_ok=True)
    shutil.copy2(release.PUBLIC_KEY, repo / release.PUBLIC_KEY.name)
    site = pages_url.rstrip('/')
    channels = (
        # The test channel ships disabled: it tracks whatever was last built,
        # so it is opt-in per command (--enablerepo) or by editing the file.
        ('release', 'egcl.repo', 'egcl', 'Evergreen Common Lisp', 1),
        ('test', 'egcl-testing.repo', 'egcl-testing',
         'Evergreen Common Lisp (test builds)', 0),
    )
    for channel, filename, identifier, description, enabled in channels:
        (repo / filename).write_text('\n'.join([
            f'[{identifier}]',
            f'name={description} - $basearch',
            # $releasever and $basearch are dnf's: one file serves every Fedora
            # release and architecture published here.
            f'baseurl={site}/repo/{channel}/fc$releasever/$basearch',
            f'enabled={enabled}',
            # Both checks matter. gpgcheck verifies each package; repo_gpgcheck
            # verifies the metadata that decides which package you are offered,
            # without which signed-but-older packages could be served instead.
            'gpgcheck=1',
            'repo_gpgcheck=1',
            f'gpgkey={site}/repo/{release.PUBLIC_KEY.name}',
            '',
        ]))
    return repo


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    record = commands.add_parser('record', help="store one release's metadata")
    record.add_argument('--assets', type=Path, required=True)
    record.add_argument('--store', type=Path, required=True)
    record.add_argument('--channel', choices=CHANNELS, required=True)
    record.add_argument('--dist', required=True)
    record.add_argument('--tag', required=True)
    record.add_argument('--repository', required=True)
    build = commands.add_parser('build', help='merge and sign the published repositories')
    build.add_argument('--store', type=Path, required=True)
    build.add_argument('--output', type=Path, required=True)
    build.add_argument('--dist', required=True)
    build.add_argument('--passphrase-file', type=Path, required=True)
    build.add_argument('--repository', required=True)
    build.add_argument('--pages-url', required=True)
    args = parser.parse_args()
    if args.command == 'record':
        for staging in generate(sorted(args.assets.glob('*.rpm')), args.store, args.channel,
                                args.dist, args.tag, args.repository):
            print(f'recorded {staging.relative_to(args.store)}')
        if args.channel == 'test':
            for arch in architectures(sorted(args.assets.glob('*.rpm'))):
                for dropped in prune(args.store, 'test', args.dist, arch, TEST_CHANNEL_KEEP):
                    print(f'pruned test metadata for {dropped}')
        return 0
    write_client_files(args.output, args.repository, args.pages_url)
    for channel in CHANNELS:
        root = args.store / 'per-release' / channel / dist_directory(args.dist)
        for arch in sorted(path.name for path in root.iterdir()) if root.exists() else []:
            destination = merge(args.store, channel, args.dist, arch, args.output)
            if destination is None:
                continue
            sign_repomd(destination / 'repodata', args.passphrase_file)
            tags = stored_releases(args.store, channel, args.dist, arch)
            print(f'{channel}/{dist_directory(args.dist)}/{arch}: '
                  f'{len(tags)} releases merged and signed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
