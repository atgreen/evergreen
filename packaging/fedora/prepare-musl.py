#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Build the s390x musl sysroot and LLVM unwinder missing from Rust's tier-3 target."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import urllib.request

MUSL_VERSION = '1.2.5'
UNWIND_VERSION = '21.1.8'
MUSL_SHA256 = 'a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4'
UNWIND_SHA256 = '03e8adc6c3bdde657dcaedc94886ea70d1f7d551d622fcd8a36a8300e5c36cbc'


def run(command, **kwargs):
    print('+', shlex.join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def source(tools, url, digest, directory):
    archive = tools / url.rsplit('/', 1)[1]
    if not archive.exists():
        partial = archive.with_suffix(archive.suffix + '.part')
        with urllib.request.urlopen(url) as response, partial.open('wb') as output:
            shutil.copyfileobj(response, output)
        partial.rename(archive)
    with archive.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != digest:
            raise RuntimeError(f'Source checksum mismatch: {archive}')
    destination = tools / directory
    if not destination.exists():
        with tarfile.open(archive) as compressed:
            compressed.extractall(tools, filter='data')
    return destination


def prepare(tools):
    tools = tools.resolve()
    tools.mkdir(parents=True, exist_ok=True)
    musl = source(tools, f'https://musl.libc.org/releases/musl-{MUSL_VERSION}.tar.gz',
                  MUSL_SHA256, f'musl-{MUSL_VERSION}')
    unwind = source(tools, 'https://github.com/llvm/llvm-project/releases/download/'
                    f'llvmorg-{UNWIND_VERSION}/libunwind-{UNWIND_VERSION}.src.tar.xz',
                    UNWIND_SHA256, f'libunwind-{UNWIND_VERSION}.src')
    prefix = tools / 's390x-musl'
    build = tools / 's390x-musl-build'
    build.mkdir(exist_ok=True)
    compiler = tools / 'usr/bin/s390x-linux-gnu-gcc'
    ar = tools / 'usr/bin/s390x-linux-gnu-ar'
    compiler_command = [str(compiler), '-fuse-ld=bfd', f'-B{tools}/usr/s390x-linux-gnu/bin/']
    cc = build / 'cc'
    cc.write_text('#!/bin/sh\nexec ' + shlex.join(compiler_command) + ' "$@"\n')
    cc.chmod(0o755)
    env = os.environ | {'CC': str(cc), 'AR': str(ar),
                       'RANLIB': str(tools / 'usr/bin/s390x-linux-gnu-ranlib')}
    run([musl / 'configure', '--target=s390x-linux-musl', f'--prefix={prefix}',
         '--disable-shared'], cwd=build, env=env)
    run(['make', '-j', os.environ.get('CARGO_BUILD_JOBS', '3')], cwd=build)
    run(['make', 'install'], cwd=build)
    specs = prefix / 'lib/musl-gcc.specs'
    with specs.open('w') as output:
        run(['sh', musl / 'tools/musl-gcc.specs.sh', prefix / 'include', prefix / 'lib',
             '/lib/ld-musl-s390x.so.1'], stdout=output)
    (prefix / 'bin').mkdir(exist_ok=True)
    linker = prefix / 'bin/s390x-linux-musl-gcc'
    linker.write_text('#!/bin/sh\nexec ' + shlex.join(compiler_command + [f'-specs={specs}']) + ' "$@"\n')
    linker.chmod(0o755)

    # Build only libunwind's static sources, with no C++ runtime dependency.
    # GCC's Fedora cross unwinder is not a substitute: it fails panic unwinding
    # with musl. Keep LLVM's normal thread support and DWARF unwind tables.
    flags = ['--target=s390x-linux-musl', f'--sysroot={prefix}', '-O2', '-fPIC',
             '-fno-stack-protector', '-funwind-tables', '-D_LIBUNWIND_IS_NATIVE_ONLY',
             '-D_LIBUNWIND_DISABLE_VISIBILITY_ANNOTATIONS', f'-I{unwind}/include',
             f'-I{unwind}/src', '-isystem', str(prefix / 'include')]
    objects = []
    for filename in ('libunwind.cpp', 'Unwind-EHABI.cpp', 'Unwind-seh.cpp',
                     'UnwindLevel1.c', 'UnwindLevel1-gcc-ext.c', 'Unwind-sjlj.c',
                     'Unwind-wasm.c', 'UnwindRegistersRestore.S', 'UnwindRegistersSave.S'):
        obj = build / f'{filename}.o'
        if filename.endswith('.cpp'):
            command = ['clang++', *flags, '-std=c++17', '-nostdinc++', '-fno-exceptions', '-fno-rtti']
        else:
            command = ['clang', *flags, '-fexceptions']
        run([*command, '-c', unwind / 'src' / filename, '-o', obj])
        objects.append(obj)
    library = prefix / 'lib/libunwind.a'
    library.unlink(missing_ok=True)
    run([ar, 'crs', library, *objects])
    licenses = prefix / 'licenses'
    licenses.mkdir(exist_ok=True)
    shutil.copy2(musl / 'COPYRIGHT', licenses / 'musl-COPYRIGHT')
    shutil.copy2(unwind / 'LICENSE.TXT', licenses / 'LLVM-LICENSE.TXT')
    (prefix / 'build.json').write_text(json.dumps({
        'musl': {'version': MUSL_VERSION, 'sha256': MUSL_SHA256},
        'libunwind': {'version': UNWIND_VERSION, 'sha256': UNWIND_SHA256},
    }, indent=2) + '\n')
    print(f's390x musl toolchain ready: {linker}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tools', type=Path, default=Path('target/fedora-rpm/tools'))
    prepare(parser.parse_args().tools)
