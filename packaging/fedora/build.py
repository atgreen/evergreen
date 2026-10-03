#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Build container-free Fedora RPM payloads from this checkout."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
# `native` and `static` are built FOR the machine running the build, not for a
# fixed architecture: x86_64 for the cross-targeting release, ppc64le for the
# POWER-native one. Keyed by `uname -m`, which agrees with rpm's %{_arch} for
# every architecture egcl.spec's ExclusiveArch allows. An unknown host falls
# through unmapped so cargo names the missing target rather than silently
# building for the wrong one.
HOST_RUST_ARCH = {'x86_64': 'x86_64', 'ppc64le': 'powerpc64le'}
HOST_MACHINE = platform.machine()
HOST_ARCH = HOST_RUST_ARCH.get(HOST_MACHINE, HOST_MACHINE)
TARGETS = {
    'native': f'{HOST_ARCH}-unknown-linux-gnu',
    'static': f'{HOST_ARCH}-unknown-linux-musl',
    's390x-linux': 's390x-unknown-linux-gnu',
    'aarch64-linux': 'aarch64-unknown-linux-gnu',
    'ppc64le-linux': 'powerpc64le-unknown-linux-gnu',
    's390x-linux-static': 's390x-unknown-linux-musl',
    'aarch64-linux-static': 'aarch64-unknown-linux-musl',
    'ppc64le-linux-static': 'powerpc64le-unknown-linux-musl',
    'windows': 'x86_64-pc-windows-gnu',
    'android': 'aarch64-linux-android',
}
GROUPS = {'native': ['native', 'static']}
# The cross groups need x86_64-hosted toolchains -- Fedora's cross GCC and
# sysroot RPMs from prepare-tools.sh, MinGW, and the Android NDK -- so they
# exist only on x86_64. This mirrors egcl.spec's `%ifarch x86_64` guard around
# the matching subpackages; the two must stay in step, or `--group all` would
# build payloads the spec then refuses to package.
if HOST_MACHINE == 'x86_64':
    GROUPS.update({
        's390x': ['s390x-linux', 's390x-linux-static'],
        'aarch64': ['aarch64-linux', 'aarch64-linux-static'],
        'ppc64le': ['ppc64le-linux', 'ppc64le-linux-static'],
        'windows': ['windows'],
        'android': ['android'],
    })


def run(command, *, env=None, cwd=ROOT):
    print('+', shlex.join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True)


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def build(args):
    output = args.output.resolve()
    tools = args.tools.resolve()
    ndk = args.android_ndk.resolve()
    stage = output / 'stage'
    bin_dir = stage / 'usr/bin'
    bin_dir.mkdir(parents=True, exist_ok=True)
    target_dir = ROOT / 'target'
    limited = ROOT / 'scripts/egcl-limited.sh'
    env = dict(os.environ, RUSTUP_TOOLCHAIN=os.environ.get('RUSTUP_TOOLCHAIN', '1.94.1'),
               CARGO_BUILD_JOBS=os.environ.get('CARGO_BUILD_JOBS', '3'),
               EGCL_MEM_MAX=os.environ.get('EGCL_MEM_MAX', '8G'),
               EGCL_TIMEOUT=os.environ.get('EGCL_TIMEOUT', '1800'))
    # No inherited target flags: Android alone uses the static bionic profile.
    env.pop('CARGO_ENCODED_RUSTFLAGS', None)
    env.pop('RUSTFLAGS', None)
    env['CARGO_TARGET_DIR'] = str(target_dir)
    targets = {name: TARGETS[name] for name in (args.target or TARGETS)}
    # `git rev-parse HEAD` alone names a commit the payload may not correspond
    # to: a build from a dirty tree would claim provenance it does not have, and
    # build.json is the only record of what went into the RPM. Mark it.
    revision = ROOT / 'SOURCE-REVISION'
    if revision.exists():
        head = revision.read_text().strip()
    else:
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        if subprocess.run(['git', 'diff', '--quiet', 'HEAD'], cwd=ROOT).returncode != 0:
            head += '-dirty'
    provenance = {'git': head,
                  'rustc': subprocess.check_output(['rustc', '--version'], text=True, env=env).strip(),
                  # The real %dist of the build environment, so the release
                  # collector can check package identity without a hardcoded
                  # Fedora version. NOT sysroot_release, which names the cross
                  # sysroot directory and only happens to read the same.
                  'dist': subprocess.check_output(['rpm', '--eval', '%dist'], text=True).strip(),
                  'sysroot_release': args.sysroot_release,
                  'android_ndk': ((ndk / 'source.properties').read_text()
                                  if 'android' in targets else None), 'artifacts': {}}
    for name, triple in targets.items():
        target_env = env.copy()
        suffix = '.exe' if name == 'windows' else ''
        directory = stage / 'usr/libexec/egcl' / name
        runtime = target_dir / triple / 'release' / f'egcl{suffix}'
        runner = []
        if name.endswith('-linux-static'):
            arch = name.split('-')[0]
            compiler_arch = 'powerpc64le' if arch == 'ppc64le' else arch
            target_env['RUSTFLAGS'] = '-C target-feature=+crt-static'
            if arch == 's390x':
                musl = tools / 's390x-musl'
                linker = musl / 'bin/s390x-linux-musl-gcc'
                target_env['RUSTFLAGS'] += f' -C link-self-contained=no -L native={musl}/lib'
                # Tier 3: opt into build-std only for this pinned-toolchain build.
                target_env['RUSTC_BOOTSTRAP'] = '1'
                provenance['s390x_musl'] = json.loads((musl / 'build.json').read_text())
                shutil.copytree(musl / 'licenses', stage / 'usr/share/licenses' /
                                f'egcl-target-{name}', dirs_exist_ok=True)
            else:
                # These Rust targets ship self-contained musl CRTs and libraries.
                linker = output / f'{arch}-static-link'
                linker.write_text('#!/bin/sh\nexec ' + shlex.join([
                    str(tools / 'usr/bin' / f'{compiler_arch}-linux-gnu-gcc'),
                    '-fuse-ld=bfd', f'-B{tools}/usr/{compiler_arch}-linux-gnu/bin/']) + ' "$@"\n')
                linker.chmod(0o755)
            target_env[f'CARGO_TARGET_{triple.upper().replace("-", "_")}_LINKER'] = str(linker)
            for spelling in (triple, triple.replace('-', '_')):
                target_env[f'CC_{spelling}'] = str(linker)
                target_env[f'AR_{spelling}'] = str(tools / 'usr/bin' / f'{compiler_arch}-linux-gnu-ar')
            runner = [f'qemu-{arch}']
        elif name.endswith('-linux'):
            arch = name.split('-')[0]
            compiler_arch = 'powerpc64le' if arch == 'ppc64le' else arch
            sysroot = tools / 'usr' / f'{arch}-redhat-linux/sys-root' / args.sysroot_release
            crossbin = tools / 'usr' / f'{compiler_arch}-linux-gnu/bin'
            gcc_lib = tools / 'targets' / arch / 'lib64'
            gcc_so = gcc_lib / 'libgcc_s.so'
            if not gcc_so.exists():
                gcc_so.symlink_to('libgcc_s.so.1')
            linker = output / f'{arch}-link'
            compiler = tools / 'usr/bin' / f'{compiler_arch}-linux-gnu-gcc'
            linker.write_text('#!/bin/sh\nexec ' + shlex.join([
                str(compiler), '-fuse-ld=bfd', f'-B{crossbin}/',
                f'--sysroot={sysroot}', f'-L{gcc_lib}']) + ' "$@"\n')
            linker.chmod(0o755)
            env_triple = triple.upper().replace('-', '_')
            target_env[f'CARGO_TARGET_{env_triple}_LINKER'] = str(linker)
            # egcl-rt/build.rs assembles native_transfer/{s390x,ppc64le}.S, so a
            # cross C driver is needed to BUILD and not only to link. cc-rs looks
            # for `<arch>-linux-gnu-gcc` on PATH, and this toolchain is extracted
            # privately under target/fedora-rpm/tools rather than installed -- and
            # the Fedora cross gcc's built-in sysroot is an absolute /usr path this
            # checkout never creates. Hand cc-rs the same wrapper the linker uses,
            # which already supplies --sysroot and -B, plus the matching ar.
            # NOTE the spelling: cargo wants the triple UPPERCASED with
            # underscores, cc-rs wants it lowercase and accepts either
            # separator. Using cargo's spelling for CC is silently ignored and
            # cc-rs falls back to searching PATH, which is how this looked like
            # "the compiler is missing" when it was only misaddressed.
            cc_ar = str(tools / 'usr/bin' / f'{compiler_arch}-linux-gnu-ar')
            for spelling in (triple, triple.replace('-', '_')):
                target_env[f'CC_{spelling}'] = str(linker)
                target_env[f'AR_{spelling}'] = cc_ar
            private = directory / 'sysroot'
            (private / 'lib64').mkdir(parents=True, exist_ok=True)
            if not (private / 'lib').exists():
                (private / 'lib').symlink_to('lib64')
            # Preserve the runtime ABI while omitting headers and link-time archives.
            for library in (sysroot / 'usr/lib64').glob('*.so.*'):
                copy(library, private / 'lib64' / library.name)
            for loader in (sysroot / 'usr/lib').glob('ld*.so*'):
                copy(loader, private / 'lib64' / loader.name)
            copy(gcc_lib / 'libgcc_s.so.1', private / 'lib64/libgcc_s.so.1')
            license_dir = stage / 'usr/share/licenses' / f'egcl-target-{name}'
            shutil.copytree('/usr/share/licenses/glibc', license_dir / 'glibc', dirs_exist_ok=True)
            shutil.copytree(tools / 'targets' / arch / 'usr/share/licenses/libgcc',
                            license_dir / 'libgcc', dirs_exist_ok=True)
            runner = [f'qemu-{arch}', '-L', str(private)]
        elif name == 'windows':
            runner = ['wine']
            target_env['WINEPREFIX'] = str(output / 'wine-build')
            target_env['WINEDEBUG'] = '-all'
        elif name == 'android':
            target_env['CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER'] = str(
                ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android28-clang')
            target_env['RUSTFLAGS'] = '-C target-feature=+crt-static'
            runner = ['qemu-aarch64']
            copy(ndk / 'NOTICE', stage / 'usr/share/licenses/egcl-target-android/NOTICE')
        command = ['cargo', 'build', '--locked', '--release', '--target', triple, '-p', 'egcl']
        if triple == 's390x-unknown-linux-musl':
            command += ['-Z', 'build-std=std,panic_unwind']
        if name not in ('windows', 'static') and not name.endswith('-static'):
            command += ['--features', 'egcl-rt/c-ffi']
        run([limited, *command], env=target_env)
        # Strip BEFORE dumping; stripping an appended image destroys its trailer.
        stripped = output / 'stripped' / f'{name}{suffix}'
        copy(runtime, stripped)
        strip_tools = {
            'native': 'strip',
            'static': 'strip',
            'windows': 'x86_64-w64-mingw32-strip',
            'android': ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip',
            'ppc64le-linux': tools / 'usr/bin/powerpc64le-linux-gnu-strip',
            's390x-linux': tools / 'usr/bin/s390x-linux-gnu-strip',
            'aarch64-linux': tools / 'usr/bin/aarch64-linux-gnu-strip',
        }
        strip = strip_tools[name.removesuffix('-static')]
        run([strip, '--strip-debug', stripped])
        if name in ('native', 'static'):
            destination = bin_dir / ('egcl' if name == 'native' else 'egcl-static')
        else:
            destination = directory / f'egcl{suffix}'
        destination.parent.mkdir(parents=True, exist_ok=True)
        dump_env = target_env | {'EGCL_IMAGE_OUT': str(destination)}
        if name == 'windows':
            # Wine's Z: drive exposes host absolute paths to the guest.
            dump_env['EGCL_IMAGE_OUT'] = 'Z:' + str(destination)
        run([limited, *runner, stripped, '--no-init', '--load', 'scripts/build-image.lisp'], env=dump_env)
        if name == 'windows':
            run(['wineserver', '-k'], env=target_env)
        destination.chmod(0o755)
        if name not in ('native', 'static'):
            launcher = bin_dir / f'egcl-{name}'
            copy(Path(__file__).with_name('egcl-cross'), launcher)
            launcher.chmod(0o755)
        provenance['artifacts'][name] = hashlib.sha256(destination.read_bytes()).hexdigest()
    provenance['rpms'] = sorted(p.name for p in (tools / 'rpms').glob('*.rpm'))
    docs = stage / 'usr/share/doc/egcl'
    docs.mkdir(parents=True, exist_ok=True)
    (docs / 'build.json').write_text(json.dumps(provenance, indent=2) + '\n')
    copy(ROOT / 'docs/fedora-rpm.md', docs / 'fedora-rpm.md')
    verification = ['python3', Path(__file__).with_name('verify.py'), stage]
    for name in targets:
        verification += ['--target', name]
    run(verification, env=env)
    return stage


def source_archive(output, sources, *, include_std=False):
    """Include current source plus locked, vendored crates for RPM %build."""
    snapshot = output / 'rpm-source' / 'egcl-source'
    snapshot.mkdir(parents=True, exist_ok=True)
    for name in ('Cargo.toml', 'Cargo.lock', 'mkdocs.yml', 'rust-toolchain.toml'):
        copy(ROOT / name, snapshot / name)
    for name in ('crates', 'lib', 'scripts', 'packaging/android', 'packaging/fedora', 'docs/manual'):
        destination = snapshot / name
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(ROOT / name, destination,
                        ignore=shutil.ignore_patterns('__pycache__', '*.pyc', 'build', '*.fasl'))
    copy(ROOT / 'docs/hooks.py', snapshot / 'docs/hooks.py')
    copy(ROOT / 'docs/fedora-rpm.md', snapshot / 'docs/fedora-rpm.md')
    # Workspace membership includes the linter even though only egcl-android
    # is built. Cargo still needs every member manifest when reading the lock.
    shutil.copytree(ROOT / 'tools/gc-root-lint', snapshot / 'tools/gc-root-lint',
                    dirs_exist_ok=True)
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    if subprocess.run(['git', 'diff', '--quiet', 'HEAD'], cwd=ROOT).returncode != 0:
        revision += '-dirty'
    (snapshot / 'SOURCE-REVISION').write_text(revision + '\n')
    command = ['cargo', 'vendor', '--locked', '--offline', '--versioned-dirs', 'vendor']
    env = dict(os.environ)
    if include_std:
        sysroot = subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip()
        command += ['--sync', str(Path(sysroot) / 'lib/rustlib/src/rust/library/Cargo.toml')]
        env['RUSTC_BOOTSTRAP'] = '1'
    config = subprocess.check_output(command, cwd=snapshot, text=True, env=env)
    (snapshot / '.cargo').mkdir(exist_ok=True)
    (snapshot / '.cargo/config.toml').write_text(config)
    with tarfile.open(sources / 'egcl-source.tar.gz', 'w:gz') as archive:
        archive.add(snapshot, arcname='egcl-source')


def package(output, stage, ndk, release=None):
    if release is not None and not re.fullmatch(r'[0-9]+(?:\.[A-Za-z0-9]+)*', release):
        raise ValueError('RPM release must contain only dot-separated alphanumeric components')
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    sources = output / 'SOURCES'
    sources.mkdir(parents=True, exist_ok=True)
    copy(ROOT / 'docs/fedora-rpm.md', stage / 'usr/share/doc/egcl/fedora-rpm.md')
    with tarfile.open(sources / 'egcl-payload.tar.gz', 'w:gz') as archive:
        archive.add(stage, arcname='payload')
    source_archive(output, sources)
    command = ['rpmbuild', '-bb', ROOT / 'packaging/fedora/egcl.spec',
         '--define', f'_topdir {output}', '--define', f'egcl_version {version}',
         '--define', 'egcl_prebuilt 1',
         '--define', 'egcl_rustup 1',
         '--define', f'android_ndk {ndk.resolve()}']
    if release is not None:
        command += ['--define', f'egcl_release {release}']
    run(command)
    extract_and_verify(output, stage)


def extract_and_verify(output, stage, targets=None):
    extracted = output / 'extracted'
    if extracted.exists():
        shutil.rmtree(extracted)
    extracted.mkdir(parents=True, exist_ok=True)
    for rpm in sorted((output / 'RPMS/x86_64').glob('*.rpm')):
        # cpio can exit before rpm2cpio has flushed its archive padding, giving
        # a harmless SIGPIPE on a direct pipe. A disk-backed archive avoids it.
        with tempfile.TemporaryFile(dir=output) as archive:
            run_to_archive = subprocess.run(['rpm2cpio', str(rpm)], stdout=archive)
            run_to_archive.check_returncode()
            archive.seek(0)
            subprocess.run(['cpio', '-idmu', '--quiet'], stdin=archive,
                           cwd=extracted, check=True)
    # Check every file: RPM postprocessing must not silently alter an image.
    for path in stage.rglob('*'):
        if path.is_file() and not path.is_symlink():
            installed = extracted / path.relative_to(stage)
            if path.read_bytes() != installed.read_bytes():
                raise RuntimeError(f'RPM changed payload file: {path}')
    verification = ['python3', ROOT / 'packaging/fedora/verify.py', extracted]
    for target in (targets or TARGETS):
        verification += ['--target', target]
    run(verification)
    if targets is None or 'native' in targets:
        run(['python3', ROOT / 'packaging/fedora/verify-native-content.py', extracted])
    if targets is not None and 'android' not in targets:
        print(f'RPMs built and verified: {output / "RPMS/x86_64"}')
        return
    metadata = json.loads((extracted / 'usr/libexec/egcl/android/runtime.json').read_text())
    checker_spec = importlib.util.spec_from_file_location(
        'android_runtime', ROOT / 'packaging/android/build-runtime.py')
    checker = importlib.util.module_from_spec(checker_spec)
    checker_spec.loader.exec_module(checker)
    for host, (_, machine) in checker.HOSTS.items():
        library = extracted / 'usr/libexec/egcl/android' / host / 'libegcl_android.so'
        checker.check_elf(library, machine)
        if hashlib.sha256(library.read_bytes()).hexdigest() != metadata['hosts'][host]['sha256']:
            raise RuntimeError(f'RPM changed Android library: {library}')
    run(['python3', extracted / 'usr/bin/egcl-android-new', '--help'])
    print(f'RPMs built and verified: {output / "RPMS/x86_64"}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/fedora-rpm')
    parser.add_argument('--tools', type=Path, default=ROOT / 'target/fedora-rpm/tools')
    parser.add_argument('--android-ndk', type=Path, required=True)
    parser.add_argument('--sysroot-release', default='fc44')
    parser.add_argument('--release', help='Override RPM release (e.g. 0.test.123.1 for a prerelease)')
    parser.add_argument('--package-only', action='store_true', help='Repackage an existing stage; still verify the extracted RPMs')
    parser.add_argument('--stage-only', action='store_true', help='Build and verify payloads without assembling RPMs')
    parser.add_argument('--target', action='append', choices=TARGETS,
                        help='With --stage-only, build just this payload (repeatable)')
    parser.add_argument('--group', choices=['all', *GROUPS],
                        help='With --stage-only, build an RPM package group')
    args = parser.parse_args()
    if args.group:
        if args.target:
            parser.error('--group and --target are mutually exclusive')
        args.target = list(TARGETS) if args.group == 'all' else GROUPS[args.group]
    if args.target and not args.stage_only:
        parser.error('--target requires --stage-only; RPM releases must contain every target')
    if args.stage_only and args.package_only:
        parser.error('--stage-only and --package-only are mutually exclusive')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    stage = output / 'stage' if args.package_only else build(args)
    if not args.stage_only:
        package(output, stage, args.android_ndk, args.release)
