#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Generate a CycloneDX SBOM for the shipped EGCL packages.

The packages bundle more than the Rust tree: musl, LLVM's libunwind and NDK
output are linked in statically, so "what version of musl is in this RPM" is
otherwise answerable only by reading the build scripts. Every fact here comes
from the files that already pin those inputs -- Cargo.lock through
`cargo metadata`, and the checksum constants in prepare-musl.py and
prepare-tools.sh -- so the SBOM cannot drift from what is actually built.

No SBOM generator is installed to produce this. Adding `cargo install
cargo-cyclonedx` to the release path would reintroduce exactly the unpinned
third-party download that bliss-9qu9p removed.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[2]
PREPARE_TOOLS = Path(__file__).resolve().with_name('prepare-tools.sh')
# CycloneDX 1.6; the schema URL is part of the format, not a fetched resource.
SCHEMA = 'http://cyclonedx.org/schema/bom-1.6.schema.json'


def rust_components(root=ROOT):
    """Every Rust package that ships or builds, from the locked dependency graph.

    Dev-dependencies are excluded: proptest and friends never reach an RPM, and
    listing them would overstate what the packages contain. Build-dependencies
    are kept -- they do not ship either, but they execute during the build and
    so are part of the supply chain the SBOM exists to describe.
    """
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--offline', '--format-version', '1'],
        cwd=root, text=True))
    packages = {package['id']: package for package in metadata['packages']}
    shipped, pending = set(), list(metadata['workspace_members'])
    while pending:
        identifier = pending.pop()
        if identifier in shipped:
            continue
        shipped.add(identifier)
        for node in metadata['resolve']['nodes']:
            if node['id'] != identifier:
                continue
            for dependency in node['deps']:
                kinds = {kind.get('kind') for kind in dependency['dep_kinds']}
                if kinds <= {'dev'}:
                    continue
                pending.append(dependency['pkg'])
    members = set(metadata['workspace_members'])
    components = []
    for identifier in sorted(shipped - members):
        package = packages[identifier]
        components.append({
            'type': 'library',
            'name': package['name'],
            'version': package['version'],
            'purl': f'pkg:cargo/{package["name"]}@{package["version"]}',
            **({'licenses': [{'expression': package['license']}]} if package.get('license') else {}),
            **({'description': package['description']} if package.get('description') else {}),
        })
    return components


def ndk_release(prepare_tools=PREPARE_TOOLS):
    """The NDK version and checksum, read from the script that pins them."""
    text = prepare_tools.read_text()
    version = re.search(r'android-ndk-(r\w+)-linux\.zip', text)
    digest = re.search(r"printf '%s  %s\\n' ([0-9a-f]{40})", text)
    if not version or not digest:
        raise ValueError(f'Could not read the pinned NDK version and sha1 from {prepare_tools}')
    return version.group(1), digest.group(1)


def native_components(root=ROOT, prepare_tools=PREPARE_TOOLS):
    """The C libraries linked into the shipped runtimes, with their pinned digests."""
    musl = runpy.run_path(str(Path(__file__).resolve().with_name('prepare-musl.py')))
    ndk_version, ndk_sha1 = ndk_release(prepare_tools)
    return [
        {'type': 'library', 'name': 'musl', 'version': musl['MUSL_VERSION'],
         'purl': f'pkg:generic/musl@{musl["MUSL_VERSION"]}',
         'licenses': [{'expression': 'MIT'}],
         'hashes': [{'alg': 'SHA-256', 'content': musl['MUSL_SHA256']}],
         'description': 'C library linked statically into the musl runtimes'},
        {'type': 'library', 'name': 'libunwind', 'version': musl['UNWIND_VERSION'],
         'purl': f'pkg:generic/libunwind@{musl["UNWIND_VERSION"]}',
         'licenses': [{'expression': 'Apache-2.0 WITH LLVM-exception'}],
         'hashes': [{'alg': 'SHA-256', 'content': musl['UNWIND_SHA256']}],
         'description': "LLVM unwinder for the s390x musl runtime"},
        # The expression is the one egcl.spec already declares for
        # egcl-target-android, i.e. the licence of the NDK parts that are
        # actually redistributed, not of the NDK distribution as a whole.
        {'type': 'framework', 'name': 'android-ndk', 'version': ndk_version,
         'purl': f'pkg:generic/android-ndk@{ndk_version}',
         'licenses': [{'expression': 'BSD-2-Clause AND BSD-3-Clause'}],
         'hashes': [{'alg': 'SHA-1', 'content': ndk_sha1}],
         'description': 'Toolchain and bionic libraries for the Android runtimes'},
    ]


def build(version, root=ROOT, prepare_tools=PREPARE_TOOLS):
    """Assemble the document.

    No timestamp unless SOURCE_DATE_EPOCH says what it should be: it is
    optional in CycloneDX, and a wall clock would make two SBOMs of identical
    inputs differ, which is the one property that makes an SBOM checkable.

    serialNumber is DERIVED, not random, for that same reason. It cannot simply
    be omitted: actions/attest requires bomFormat, specVersion AND serialNumber
    to recognise a document as CycloneDX at all, and rejected ours with
    "Unsupported SBOM format. Must be valid SPDX or CycloneDX JSON." A UUIDv5
    over the finished document gives the attestable field while keeping two
    builds of identical inputs byte-identical.
    """
    metadata = {
        'component': {
            'type': 'application',
            'name': 'egcl',
            'version': version,
            'purl': f'pkg:generic/egcl@{version}',
            'licenses': [{'expression': 'GPL-3.0-or-later WITH Classpath-exception-2.0'}],
        },
    }
    if epoch := os.environ.get('SOURCE_DATE_EPOCH'):
        metadata['timestamp'] = datetime.datetime.fromtimestamp(
            int(epoch), datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    document = {
        '$schema': SCHEMA,
        'bomFormat': 'CycloneDX',
        'specVersion': '1.6',
        'version': 1,
        'metadata': metadata,
        'components': native_components(root, prepare_tools) + rust_components(root),
    }
    # Hash the document that exists so far, so the serial number is a function
    # of the contents and nothing else. sort_keys makes it independent of the
    # insertion order above, so reordering a field cannot change the serial
    # without changing what the SBOM says.
    digest = json.dumps(document, sort_keys=True, separators=(',', ':'))
    document['serialNumber'] = f'urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, digest)}'
    return document


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', help='defaults to the workspace version')
    parser.add_argument('--output', type=Path, default=Path('target/egcl-sbom.cdx.json'))
    args = parser.parse_args()
    version = args.version or tomllib.loads(
        (ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    args.output.parent.mkdir(parents=True, exist_ok=True)
    document = build(version)
    args.output.write_text(json.dumps(document, indent=2) + '\n')
    print(f'{args.output}: {len(document["components"])} components')


if __name__ == '__main__':
    main()
