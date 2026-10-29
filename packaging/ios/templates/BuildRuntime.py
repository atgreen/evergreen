#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Build the selected iOS static runtime and its exact native linker response."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source_options = parser.add_mutually_exclusive_group(required=True)
    source_options.add_argument('--source', type=Path)
    source_options.add_argument('--config', type=Path)
    parser.add_argument('--sdk', required=True)
    parser.add_argument('--platform', required=True, choices=('iphoneos', 'iphonesimulator'))
    parser.add_argument('--arch', required=True, choices=('arm64',))
    parser.add_argument('--deployment-target', required=True)
    parser.add_argument('--target-dir', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--response', required=True, type=Path)
    args = parser.parse_args()
    target = 'aarch64-apple-ios' + ('-sim' if args.platform == 'iphonesimulator' else '')
    configured_source = args.source
    if args.config is not None:
        configuration = json.loads(args.config.read_text(encoding='utf-8'))
        if (not isinstance(configuration, dict) or configuration.get('version') != 1
                or not isinstance(configuration.get('runtime_source'), str)):
            raise ValueError('runtime config requires version 1 and a runtime_source string')
        configured_source = Path(configuration['runtime_source'])
    source = configured_source.resolve(strict=True)
    target_dir = args.target_dir.resolve()
    archive = target_dir / target / 'release' / 'libegcl_ios.a'
    # Cargo must invoke the final crate to report its native link requirements.
    # Only this project's final archive is removed; dependency caches survive.
    archive.unlink(missing_ok=True)
    environment = dict(os.environ, SDKROOT=args.sdk,
                       IPHONEOS_DEPLOYMENT_TARGET=args.deployment_target,
                       CARGO_TERM_COLOR='never')
    command = ['cargo', 'rustc', '--locked', '--release', '--manifest-path',
               str(source / 'Cargo.toml'), '--target-dir', str(target_dir),
               '-p', 'egcl-ios', '--target', target, '--crate-type', 'staticlib',
               '--features', 'egcl-rt/c-ffi', '--', '--print', 'native-static-libs']
    result = subprocess.run(command, cwd=source, env=environment,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode:
        return result.returncode
    notes = [line.split('native-static-libs:', 1)[1].strip()
             for line in (result.stdout + '\n' + result.stderr).splitlines()
             if 'native-static-libs:' in line]
    if len(notes) != 1 or not archive.is_file():
        raise ValueError('expected one rustc native-static-libs report and a static archive')
    # Retain ordering and duplicates: rustc documents both as significant.
    flags = shlex.split(notes[0])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.response.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(archive, args.output)
    # Clang response syntax accepts double-quoted arguments. Do not invoke a
    # shell or guess a platform-dependent list of system libraries/frameworks.
    quoted = ['"' + flag.replace("\\", "\\\\").replace('"', '\\"') + '"' for flag in flags]
    args.response.write_text('\n'.join(quoted) + '\n', encoding='utf-8')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError) as error:
        sys.exit(f'BuildRuntime.py: {error}')
