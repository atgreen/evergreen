#!/usr/bin/env python3
"""Build container-free Fedora RPM payloads from this checkout."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]


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
    limited = ROOT / 'scripts/torcl-limited.sh'
    env = dict(os.environ, RUSTUP_TOOLCHAIN=os.environ.get('RUSTUP_TOOLCHAIN', '1.94.1'),
               CARGO_BUILD_JOBS=os.environ.get('CARGO_BUILD_JOBS', '3'),
               TORCL_MEM_MAX=os.environ.get('TORCL_MEM_MAX', '8G'),
               TORCL_TIMEOUT=os.environ.get('TORCL_TIMEOUT', '1800'))
    # No inherited target flags: Android alone uses the static bionic profile.
    env.pop('CARGO_ENCODED_RUSTFLAGS', None)
    env.pop('RUSTFLAGS', None)
    env['CARGO_TARGET_DIR'] = str(target_dir)
    targets = [('native', 'x86_64-unknown-linux-gnu'),
               ('s390x-linux', 's390x-unknown-linux-gnu'),
               ('aarch64-linux', 'aarch64-unknown-linux-gnu'),
               ('ppc64le-linux', 'powerpc64le-unknown-linux-gnu'),
               ('windows', 'x86_64-pc-windows-gnu'),
               ('android', 'aarch64-linux-android')]
    provenance = {'git': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                  'rustc': subprocess.check_output(['rustc', '--version'], text=True, env=env).strip(),
                  'sysroot_release': args.sysroot_release,
                  'android_ndk': (ndk / 'source.properties').read_text(), 'artifacts': {}}
    for name, triple in targets:
        target_env = env.copy()
        suffix = '.exe' if name == 'windows' else ''
        directory = stage / 'usr/libexec/torcl' / name
        runtime = target_dir / triple / 'release' / f'torcl{suffix}'
        runner = []
        if name.endswith('-linux'):
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
            # torcl-rt/build.rs assembles native_transfer/{s390x,ppc64le}.S, so a
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
            license_dir = stage / 'usr/share/licenses' / f'torcl-target-{name}'
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
            copy(ndk / 'NOTICE', stage / 'usr/share/licenses/torcl-target-android/NOTICE')
        command = ['cargo', 'build', '--locked', '--release', '--target', triple, '-p', 'torcl']
        if name != 'windows':
            command += ['--features', 'torcl-rt/c-ffi']
        run([limited, *command], env=target_env)
        # Strip BEFORE dumping; stripping an appended image destroys its trailer.
        stripped = output / 'stripped' / f'{name}{suffix}'
        copy(runtime, stripped)
        strip_tools = {
            'native': 'strip',
            'windows': 'x86_64-w64-mingw32-strip',
            'android': ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip',
            'ppc64le-linux': tools / 'usr/bin/powerpc64le-linux-gnu-strip',
            's390x-linux': tools / 'usr/bin/s390x-linux-gnu-strip',
            'aarch64-linux': tools / 'usr/bin/aarch64-linux-gnu-strip',
        }
        strip = strip_tools[name]
        run([strip, '--strip-debug', stripped])
        destination = bin_dir / 'torcl' if name == 'native' else directory / f'torcl{suffix}'
        destination.parent.mkdir(parents=True, exist_ok=True)
        dump_env = target_env | {'TORCL_IMAGE_OUT': str(destination)}
        if name == 'windows':
            # Wine's Z: drive exposes host absolute paths to the guest.
            dump_env['TORCL_IMAGE_OUT'] = 'Z:' + str(destination)
        run([limited, *runner, stripped, '--no-init', '--load', 'scripts/build-image.lisp'], env=dump_env)
        if name == 'windows':
            run(['wineserver', '-k'], env=target_env)
        destination.chmod(0o755)
        if name != 'native':
            launcher = bin_dir / f'torcl-{name}'
            copy(Path(__file__).with_name('torcl-cross'), launcher)
            launcher.chmod(0o755)
        provenance['artifacts'][name] = hashlib.sha256(destination.read_bytes()).hexdigest()
    provenance['rpms'] = sorted(p.name for p in (tools / 'rpms').glob('*.rpm'))
    docs = stage / 'usr/share/doc/torcl'
    docs.mkdir(parents=True, exist_ok=True)
    (docs / 'build.json').write_text(json.dumps(provenance, indent=2) + '\n')
    copy(ROOT / 'docs/fedora-rpm.md', docs / 'fedora-rpm.md')
    run(['python3', Path(__file__).with_name('verify.py'), stage], env=env)
    return stage


def source_archive(output, sources):
    """Include current source plus locked, vendored crates for RPM %build."""
    snapshot = output / 'rpm-source' / 'torcl-source'
    snapshot.mkdir(parents=True, exist_ok=True)
    for name in ('Cargo.toml', 'Cargo.lock', 'mkdocs.yml'):
        copy(ROOT / name, snapshot / name)
    for name in ('crates', 'lib', 'packaging/android', 'packaging/fedora', 'docs/manual'):
        destination = snapshot / name
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(ROOT / name, destination,
                        ignore=shutil.ignore_patterns('__pycache__', '*.pyc', 'build', '*.fasl'))
    copy(ROOT / 'docs/hooks.py', snapshot / 'docs/hooks.py')
    # Workspace membership includes the linter even though only torcl-android
    # is built. Cargo still needs every member manifest when reading the lock.
    shutil.copytree(ROOT / 'tools/gc-root-lint', snapshot / 'tools/gc-root-lint',
                    dirs_exist_ok=True)
    config = subprocess.check_output(
        ['cargo', 'vendor', '--locked', '--offline', '--versioned-dirs', 'vendor'],
        cwd=snapshot, text=True)
    (snapshot / '.cargo').mkdir(exist_ok=True)
    (snapshot / '.cargo/config.toml').write_text(config)
    with tarfile.open(sources / 'torcl-source.tar.gz', 'w:gz') as archive:
        archive.add(snapshot, arcname='torcl-source')


def package(output, stage, ndk):
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    sources = output / 'SOURCES'
    sources.mkdir(parents=True, exist_ok=True)
    copy(ROOT / 'docs/fedora-rpm.md', stage / 'usr/share/doc/torcl/fedora-rpm.md')
    with tarfile.open(sources / 'torcl-payload.tar.gz', 'w:gz') as archive:
        archive.add(stage, arcname='payload')
    source_archive(output, sources)
    run(['rpmbuild', '-bb', ROOT / 'packaging/fedora/torcl.spec',
         '--define', f'_topdir {output}', '--define', f'torcl_version {version}',
         '--define', 'torcl_rustup 1',
         '--define', f'android_ndk {ndk.resolve()}'])
    extract_and_verify(output, stage)


def extract_and_verify(output, stage):
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
    run(['python3', ROOT / 'packaging/fedora/verify.py', extracted])
    run(['python3', ROOT / 'packaging/fedora/verify-native-content.py', extracted])
    metadata = json.loads((extracted / 'usr/libexec/torcl/android/runtime.json').read_text())
    checker_spec = importlib.util.spec_from_file_location(
        'android_runtime', ROOT / 'packaging/android/build-runtime.py')
    checker = importlib.util.module_from_spec(checker_spec)
    checker_spec.loader.exec_module(checker)
    for host, (_, machine) in checker.HOSTS.items():
        library = extracted / 'usr/libexec/torcl/android' / host / 'libtorcl_android.so'
        checker.check_elf(library, machine)
        if hashlib.sha256(library.read_bytes()).hexdigest() != metadata['hosts'][host]['sha256']:
            raise RuntimeError(f'RPM changed Android library: {library}')
    run(['python3', extracted / 'usr/bin/torcl-android-new', '--help'])
    print(f'RPMs built and verified: {output / "RPMS/x86_64"}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/fedora-rpm')
    parser.add_argument('--tools', type=Path, default=ROOT / 'target/fedora-rpm/tools')
    parser.add_argument('--android-ndk', type=Path, required=True)
    parser.add_argument('--sysroot-release', default='fc44')
    parser.add_argument('--package-only', action='store_true', help='Repackage an existing stage; still verify the extracted RPMs')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    stage = output / 'stage' if args.package_only else build(args)
    package(output, stage, args.android_ndk)
