#!/usr/bin/env python3
"""Build both Android NativeActivity libraries with a local NDK, without containers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
HOSTS = {'aarch64-linux-android': ('arm64-v8a', 183), 'x86_64-linux-android': ('x86_64', 62)}


def check_elf(path, machine):
    """Check ELF64 machine and 16 KiB load/RELRO layout without a host binutils dependency."""
    import struct
    data = path.read_bytes()
    if data[:6] != b'\x7fELF\x02\x01' or int.from_bytes(data[18:20], 'little') != machine:
        raise ValueError(f'Wrong Android ELF architecture: {path}')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    loads = 0
    for i in range(count):
        kind, flags, offset, address, physical, filesz, memsz, align = struct.unpack_from('<IIQQQQQQ', data, phoff + i * size)
        if kind == 1:
            loads += 1
            if align < 16384 or (address - offset) % 16384:
                raise ValueError(f'ELF LOAD is not 16 KiB aligned: {path}')
        if kind == 0x6474e552 and (address + memsz) % 16384:
            raise ValueError(f'ELF RELRO end is not 16 KiB aligned: {path}')
    if not loads: raise ValueError(f'No ELF LOAD segments: {path}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ndk', type=Path, required=True)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'target/android-runtime')
    parser.add_argument('--offline', action='store_true')
    args = parser.parse_args()
    ndk = args.ndk.resolve()
    stage = args.stage.resolve()
    target = args.target_dir.resolve()
    tools = ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin'
    runtime = stage / 'usr/libexec/torcl/android'
    runtime.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.pop('CARGO_ENCODED_RUSTFLAGS', None)
    env['RUSTFLAGS'] = '-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384'
    env.setdefault('CARGO_BUILD_JOBS', '3')
    metadata = {'api': 1, 'version': tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version'],
                'min_sdk': 28, 'ndk': (ndk / 'source.properties').read_text(), 'hosts': {}}
    for host, (abi, machine) in HOSTS.items():
        compiler = tools / f'{host}28-clang'
        if not compiler.is_file(): raise ValueError(f'Missing NDK compiler: {compiler}')
        env[f'CARGO_TARGET_{host.upper().replace("-", "_")}_LINKER'] = str(compiler)
        env[f'CC_{host.replace("-", "_")}'] = str(compiler)
        env[f'AR_{host.replace("-", "_")}'] = str(tools / 'llvm-ar')
        command = ['cargo', 'rustc', '--locked', '--release', '--manifest-path', str(ROOT / 'Cargo.toml'),
                   '--target-dir', str(target), '-p', 'torcl-android', '--target', host, '--crate-type', 'cdylib']
        if args.offline: command.append('--offline')
        subprocess.run(command, cwd=ROOT, env=env, check=True)
        destination = runtime / host / 'libtorcl_android.so'
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(target / host / 'release/libtorcl_android.so', destination)
        subprocess.run([tools / 'llvm-strip', '--strip-unneeded', destination], check=True)
        check_elf(destination, machine)
        metadata['hosts'][host] = {'abi': abi, 'sha256': hashlib.sha256(destination.read_bytes()).hexdigest()}
    (runtime / 'runtime.json').write_text(json.dumps(metadata, indent=2) + '\n')
    shutil.copytree(ROOT / 'packaging/android/templates', runtime / 'templates', dirs_exist_ok=True)
    shutil.copy2(ROOT / 'packaging/android/build-apk.py', runtime / 'build-apk.py')
    (stage / 'usr/bin').mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / 'packaging/android/torcl-android-new', stage / 'usr/bin/torcl-android-new')
    (stage / 'usr/bin/torcl-android-new').chmod(0o755)
    license_dir = stage / 'usr/share/licenses/torcl-target-android'
    license_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ndk / 'NOTICE', license_dir / 'NOTICE')
    print(f'Android runtimes and generator staged in {stage}')

if __name__ == '__main__': main()
