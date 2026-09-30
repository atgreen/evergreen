#!/usr/bin/python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Package a EGCL Android project using RPM-supplied native runtimes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

HOSTS = {'aarch64-linux-android': ('arm64-v8a', 183), 'x86_64-linux-android': ('x86_64', 62)}
NS = '{http://schemas.android.com/apk/res/android}'
COMMANDLINE_TOOLS_URL = 'https://dl.google.com/android/repository/commandlinetools-linux-15859902_latest.zip'
COMMANDLINE_TOOLS_SHA256 = '4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583'


def run(command, **kwargs):
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def newest(directory):
    choices = [p for p in directory.glob('*') if p.is_dir() and re.fullmatch(r'[0-9.]+', p.name)]
    return max(choices, key=lambda p: tuple(map(int, p.name.split('.')))) if choices else directory / 'missing'


def bootstrap_sdkmanager(sdk):
    parent = sdk / 'cmdline-tools'
    destination = parent / 'latest'
    if destination.exists():
        raise ValueError(f'No executable sdkmanager in {destination}; repair or move that directory first')
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.download-', dir=parent) as temporary:
        directory = Path(temporary)
        archive = directory / 'tools.zip'
        print(f'Downloading {COMMANDLINE_TOOLS_URL}', flush=True)
        digest = hashlib.sha256()
        with urllib.request.urlopen(COMMANDLINE_TOOLS_URL, timeout=60) as response, archive.open('wb') as output:
            while chunk := response.read(1024 * 1024):
                digest.update(chunk)
                output.write(chunk)
        if digest.hexdigest() != COMMANDLINE_TOOLS_SHA256:
            raise ValueError('Android command-line tools checksum mismatch; download was not installed')
        with zipfile.ZipFile(archive) as zipped:
            for name in zipped.namelist():
                path = Path(name)
                if path.is_absolute() or '..' in path.parts or not path.parts or path.parts[0] != 'cmdline-tools':
                    raise ValueError(f'Invalid command-line tools archive path: {name}')
            zipped.extractall(directory / 'unpacked')
        unpacked = directory / 'unpacked/cmdline-tools'
        if not (unpacked / 'bin/sdkmanager').is_file():
            raise ValueError('Command-line tools archive has no sdkmanager')
        for executable in (unpacked / 'bin').iterdir():
            if executable.is_file(): executable.chmod(0o755)
        unpacked.rename(destination)
    return destination / 'bin/sdkmanager'


def install_tools(args, manifest):
    if not all(shutil.which(tool) for tool in ('java', 'keytool')):
        raise ValueError('Missing JDK; on Fedora run: sudo dnf install java-21-openjdk-devel')
    if not re.fullmatch(r'\d+\.\d+\.\d+', args.sdk_build_tools):
        raise ValueError('SDK_BUILD_TOOLS must be a version such as 35.0.0')
    target = int(manifest.find('uses-sdk').get(NS + 'targetSdkVersion'))
    sdk = args.sdk.expanduser().resolve()
    candidates = [sdk / 'cmdline-tools/latest/bin/sdkmanager',
                  *sorted(sdk.glob('cmdline-tools/*/bin/sdkmanager'), reverse=True)]
    if found := shutil.which('sdkmanager'): candidates.append(Path(found))
    manager = next((p for p in candidates if p.is_file() and os.access(p, os.X_OK)), None)
    if manager is None: manager = bootstrap_sdkmanager(sdk)
    # Inherit the terminal so SDK licenses remain the user's explicit choice.
    run([manager, f'--sdk_root={sdk}', '--install', 'platform-tools',
         f'platforms;android-{target}', f'build-tools;{args.sdk_build_tools}'])
    print('SDK manager finished. Run make doctor to check application build prerequisites.')


def runtime_libraries(root, hosts, config):
    metadata_path = root / 'runtime.json'
    if not metadata_path.is_file():
        raise ValueError(f'Missing Android runtime metadata: {metadata_path}; install egcl-target-android or set RUNTIME')
    metadata = json.loads(metadata_path.read_text())
    if metadata['api'] != config['runtime_api']:
        raise ValueError('Incompatible runtime API; regenerate the project with this RPM')
    if config['runtime_version'] not in ('source', metadata['version']):
        raise ValueError(f"Project expects runtime {config['runtime_version']}, installed {metadata['version']}; review and update app.json")
    libraries = {}
    for host in hosts:
        abi, machine = HOSTS[host]
        path = root / host / 'libegcl_android.so'
        data = path.read_bytes()
        if data[:6] != b'\x7fELF\x02\x01' or int.from_bytes(data[18:20], 'little') != machine:
            raise ValueError(f'Runtime is not a 64-bit {abi} ELF: {path}')
        if hashlib.sha256(data).hexdigest() != metadata['hosts'][host]['sha256']:
            raise ValueError(f'Runtime checksum mismatch: {path}')
        libraries[abi] = path
    return libraries


def sdk_tools(args, manifest):
    platform = manifest.find('uses-sdk')
    target = int(platform.get(NS + 'targetSdkVersion'))
    minimum = int(platform.get(NS + 'minSdkVersion'))
    if minimum < 28:
        raise ValueError('The packaged EGCL runtime requires minSdkVersion >= 28')
    build_tools = Path(args.build_tools) if args.build_tools else newest(args.sdk / 'build-tools')
    jar = Path(args.android_jar) if args.android_jar else args.sdk / f'platforms/android-{target}/android.jar'
    tools = {name: build_tools / name for name in ('aapt2', 'zipalign', 'apksigner')}
    for name, path in tools.items():
        if not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError(f'Missing {name}: {path}; install Android SDK build-tools or set BUILD_TOOLS')
    if not jar.is_file():
        raise ValueError(f'Missing {jar}; install platforms;android-{target} or set ANDROID_JAR')
    if not shutil.which('keytool'):
        raise ValueError('Missing keytool; install a JDK and add its bin directory to PATH')
    return tools, jar


def adb_command(args):
    adb = args.sdk / 'platform-tools/adb'
    if not adb.is_file():
        found = shutil.which('adb')
        if not found: raise ValueError('Missing adb; install Android SDK platform-tools')
        adb = Path(found)
    return [str(adb), *(['-s', args.serial] if args.serial else [])]


def assets(project):
    directory = project / 'assets'
    paths = {}
    for path in sorted(directory.rglob('*')):
        if path.is_symlink(): raise ValueError(f'Asset symlinks are not supported: {path}')
        if not path.is_file(): continue
        name = path.relative_to(directory).as_posix()
        if any(c in name for c in '\r\n') or name == 'egcl-assets.txt':
            raise ValueError(f'Invalid or reserved asset path: {name!r}')
        if path.stat().st_size > 64 * 1024 * 1024:
            raise ValueError(f'Asset exceeds the runtime 64 MiB limit: {name}')
        paths[name] = path
    for required in ('app.lisp', 'android.lisp'):
        if required not in paths: raise ValueError(f'Missing assets/{required}')
    return paths


def sign(tools, unsigned, output, release, project):
    if release:
        required = ('KEYSTORE', 'KEY_ALIAS', 'KEYSTORE_PASSWORD')
        missing = [key for key in required if not os.environ.get(key)]
        if missing: raise ValueError('Release signing requires environment variables: ' + ', '.join(missing))
        key = Path(os.environ['KEYSTORE']).expanduser().resolve()
        alias = os.environ['KEY_ALIAS']
        store_password = 'env:KEYSTORE_PASSWORD'
        key_password = 'env:KEY_PASSWORD' if os.environ.get('KEY_PASSWORD') else store_password
    else:
        key = project / '.debug.keystore'
        alias = 'androiddebugkey'
        store_password = key_password = 'pass:android'
        if not key.exists():
            run(['keytool', '-genkeypair', '-noprompt', '-keystore', key, '-alias', alias,
                 '-storepass', 'android', '-keypass', 'android', '-keyalg', 'RSA', '-keysize', '2048',
                 '-validity', '10000', '-dname', 'CN=Android Debug,O=Android,C=US'])
            key.chmod(0o600)
    run([tools['apksigner'], 'sign', '--ks', key, '--ks-key-alias', alias,
         '--ks-pass', store_password, '--key-pass', key_password, '--out', output, unsigned])
    run([tools['apksigner'], 'verify', output])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('apk', 'release', 'verify', 'doctor', 'install-tools', 'install', 'run', 'logcat'))
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--hosts', required=True)
    parser.add_argument('--sdk', type=Path, required=True)
    parser.add_argument('--build-tools', default='')
    parser.add_argument('--sdk-build-tools', default='35.0.0')
    parser.add_argument('--android-jar', default='')
    parser.add_argument('--serial', default='')
    args = parser.parse_args()
    hosts = sorted(set(args.hosts.split()))
    if not hosts or any(host not in HOSTS for host in hosts):
        parser.error('--hosts must contain aarch64-linux-android and/or x86_64-linux-android')
    project = Path.cwd()
    config = json.loads((project / 'app.json').read_text())
    tree = ET.parse(project / 'AndroidManifest.xml')
    manifest = tree.getroot()
    if args.command == 'install-tools':
        install_tools(args, manifest)
        return
    package = manifest.get('package', '')
    if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)+', package):
        raise ValueError('Invalid package ID in AndroidManifest.xml')
    if args.command == 'run':
        run([*adb_command(args), 'shell', 'am', 'start', '-W', '-n', f'{package}/android.app.NativeActivity'])
        return
    if args.command == 'logcat':
        run([*adb_command(args), 'logcat', 'egcl:V', 'AndroidRuntime:E', 'libc:F', '*:S'])
        return
    libraries = runtime_libraries(args.runtime, hosts, config)
    tools, jar = sdk_tools(args, manifest)
    inputs = assets(project)
    if args.command == 'doctor':
        print('Ready: ' + ', '.join(hosts) + f'; SDK {args.sdk}; runtime {args.runtime}')
        return
    if args.command == 'install':
        supported = subprocess.check_output([*adb_command(args), 'shell', 'getprop', 'ro.product.cpu.abilist'], text=True).strip().split(',')
        if not set(supported).intersection(libraries):
            raise ValueError(f"Device supports {', '.join(supported)}; APK contains {', '.join(libraries)}. Select HOST or HOSTS accordingly.")
    release = args.command == 'release'
    build = project / 'build' / '+'.join(hosts) / ('release' if release else 'debug')
    build.mkdir(parents=True, exist_ok=True)
    # The source manifest remains editable; debug/release mode controls only this copy.
    manifest.find('application').set(NS + 'debuggable', 'false' if release else 'true')
    generated_manifest = build / 'AndroidManifest.xml'
    ET.register_namespace('android', NS[1:-1])
    tree.write(generated_manifest, encoding='utf-8', xml_declaration=True)
    unsigned, aligned, output = (build / name for name in ('unsigned.apk', 'aligned.apk', 'app.apk'))
    resource_args = []
    if (project / 'res').is_dir():
        compiled = build / 'resources.zip'
        run([tools['aapt2'], 'compile', '--dir', project / 'res', '-o', compiled])
        resource_args = ['-R', compiled, '--auto-add-overlay']
    run([tools['aapt2'], 'link', '-o', unsigned, '--manifest', generated_manifest, '-I', jar, *resource_args])
    with zipfile.ZipFile(unsigned, 'a', compression=zipfile.ZIP_DEFLATED) as archive:
        # Compressed native libraries are extracted by Android; ELF load segments
        # are 16 KiB aligned by the RPM build. ZIP mmap alignment is not required.
        for abi, library in libraries.items(): archive.write(library, f'lib/{abi}/libegcl_android.so')
        for name, path in inputs.items(): archive.write(path, f'assets/{name}')
        archive.writestr('assets/egcl-assets.txt', 'egcl-android-assets-v1\n' + '\n'.join(inputs) + '\n')
    run([tools['zipalign'], '-f', '4', unsigned, aligned])
    sign(tools, aligned, output, release, project)
    if args.command == 'verify':
        run([tools['zipalign'], '-c', '4', output])
        run([tools['aapt2'], 'dump', 'badging', output])
    if args.command == 'install': run([*adb_command(args), 'install', '-r', output])
    print(f'APK: {output}')


if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, KeyError, ET.ParseError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        sys.exit(f'egcl-android: {error}')
