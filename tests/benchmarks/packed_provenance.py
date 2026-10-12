"""Verify a downloaded GitHub artifact, never an operator-supplied ID label."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import zipfile


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def github_metadata(kind, identity):
    if not str(identity).isdigit() or int(identity) < 1:
        raise ValueError('GitHub run/artifact IDs must be positive integers')
    env = dict(os.environ)
    result = subprocess.check_output(
        ['gh', 'api', f'repos/atgreen/evergreen/actions/{kind}/{identity}'],
        env=env, timeout=60, text=True)
    return json.loads(result)


def source_identity(checkout, revision):
    # Read committed blobs, not this runner's working tree. Match ci-tests.py's
    # sorted path-NUL-content digest, including symlink target bytes.
    if len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('expected full source commit SHA')
    listing = subprocess.check_output(
        ['git', 'ls-tree', '-rz', '--full-tree', revision], cwd=checkout)
    entries = []
    for item in listing.split(b'\0'):
        if not item:
            continue
        metadata, name = item.split(b'\t', 1)
        mode, kind, oid = metadata.split()
        if kind != b'blob':
            raise ValueError('source identity does not support submodules')
        entries.append((name, oid))
    identities = b'\n'.join(oid for _, oid in entries) + b'\n'
    raw = subprocess.check_output(['git', 'cat-file', '--batch'], input=identities, cwd=checkout)
    position, files = 0, {}
    for name, _ in entries:
        end = raw.index(b'\n', position)
        size = int(raw[position:end].split()[-1])
        files[name] = raw[end + 1:end + 1 + size]
        position = end + size + 2
    source = hashlib.sha256()
    for name, contents in sorted(files.items()):
        source.update(name + b'\0')
        source.update(contents)
    inputs = {os.fsdecode(name): hashlib.sha256(contents).hexdigest()
              for name, contents in files.items()
              if name in (b'Cargo.lock', b'Cargo.toml', b'rust-toolchain.toml',
                          b'.github/workflows/ci.yml', b'scripts/ci-tests.py')
              or name.startswith(b'.cargo/')}
    return {'revision': revision, 'source_digest': source.hexdigest(), 'build_input_sha256': inputs}


def verify_ci_receipt(*, root, archive, run, artifact, run_id, artifact_id, source):
    if (run.get('id') != int(run_id) or artifact.get('id') != int(artifact_id)
            or run.get('repository', {}).get('full_name') != 'atgreen/evergreen'
            or run.get('path') != '.github/workflows/ci.yml'
            or run.get('event') not in ('push', 'workflow_dispatch')
            or run.get('run_attempt') != 1
            or run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or artifact.get('expired') is not False
            or artifact.get('workflow_run', {}).get('id') != run.get('id')
            or artifact.get('workflow_run', {}).get('head_sha') != run.get('head_sha')):
        raise ValueError('artifact/run linkage requires a successful first-attempt direct-source CI run; PR merge runs and reruns unsupported')
    archive_hash = digest(archive)
    if artifact.get('digest') != 'sha256:' + archive_hash:
        raise ValueError('downloaded ZIP differs from GitHub artifact digest')
    root = Path(root)
    manifest_name = 'target/ci-tests/manifest.json'
    manifest_bytes = (root / manifest_name).read_bytes()
    manifest = json.loads(manifest_bytes)
    if (manifest.get('revision') != run.get('head_sha')
            or manifest.get('revision') != source.get('revision')
            or manifest.get('source_digest') != source.get('source_digest')
            or manifest.get('profile') != 'release'
            or manifest.get('target') != 'x86_64-unknown-linux-musl'):
        raise ValueError('manifest source/profile differs from verified CI source')
    name = 'target/x86_64-unknown-linux-musl/release/egcl'
    binary_hash = digest(root / name)
    if manifest.get('artifacts', {}).get(name) != binary_hash:
        raise ValueError('extracted binary differs from manifest')
    with zipfile.ZipFile(archive) as outer:
        if outer.namelist() != ['ci-tests.tar.gz']:
            raise ValueError('expected CI artifact ZIP containing only ci-tests.tar.gz')
        with outer.open('ci-tests.tar.gz') as compressed, tarfile.open(fileobj=compressed, mode='r|gz') as tar:
            found = set()
            for member in tar:
                if member.name not in (manifest_name, name):
                    continue
                if not member.isfile() or member.name in found:
                    raise ValueError('duplicate or non-file manifest/binary in archive')
                found.add(member.name)
                stream = tar.extractfile(member)
                if member.name == manifest_name:
                    if member.size != len(manifest_bytes) or stream.read() != manifest_bytes:
                        raise ValueError('extracted manifest differs from downloaded archive')
                elif hashlib.file_digest(stream, 'sha256').hexdigest() != binary_hash:
                    raise ValueError('extracted binary differs from downloaded archive')
            if found != {manifest_name, name}:
                raise ValueError('downloaded archive lacks manifest/binary')
    return {'revision': manifest['revision'], 'archive_sha256': archive_hash,
            'manifest_sha256': digest(root / manifest_name), 'sha256': binary_hash,
            'run_metadata': run, 'artifact_metadata': artifact, 'source': source}
