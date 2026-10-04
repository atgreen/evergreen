#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Dump and restart applications using a staged or extracted RPM installation."""
import argparse
import os
from pathlib import Path
import platform
import subprocess
import tempfile

# The *FEATURES an egcl built for this host must report. `native` and `static`
# follow the build machine (build.py's HOST_ARCH); every other entry is a fixed
# cross target. Keyed by `uname -m`, matching egcl.spec's ExclusiveArch.
HOST_FEATURES = {
    'x86_64': (':x86-64', ':linux'),
    'ppc64le': (':ppc64le', ':little-endian', ':linux'),
    # Mirrors the cross entries below for the same architectures.
    's390x': (':s390x', ':big-endian', ':linux'),
    'aarch64': (':arm64', ':linux'),
}
# Linux/rpm architecture names to Rust's spelling, for the --runtime-info
# triple check below. Only ppc64le actually differs; the rest pass through.
RUST_ARCH = {'ppc64le': 'powerpc64le'}
HOST_MACHINE = platform.machine()
TARGETS = {
    'native': HOST_FEATURES.get(HOST_MACHINE, (':linux',)),
    'static': HOST_FEATURES.get(HOST_MACHINE, (':linux',)),
    's390x-linux': (':s390x', ':big-endian'),
    'aarch64-linux': (':arm64', ':linux'),
    'ppc64le-linux': (':ppc64le', ':little-endian', ':linux'),
    's390x-linux-static': (':s390x', ':big-endian', ':linux'),
    'aarch64-linux-static': (':arm64', ':linux'),
    'ppc64le-linux-static': (':ppc64le', ':little-endian', ':linux'),
    'windows': (':x86-64', ':windows'),
    'android': (':arm64', ':android'),
}


def verify_linux_linkage(command, *, static):
    env = dict(os.environ, LC_ALL='C')
    headers = subprocess.check_output(['readelf', '-lW', str(command)], text=True, env=env)
    dependencies = subprocess.check_output(['readelf', '-dW', str(command)], text=True, env=env)
    if static:
        if 'INTERP' in headers or '(NEEDED)' in dependencies:
            raise RuntimeError(f'{command}: egcl-static must have no interpreter or shared dependencies')
    elif 'INTERP' not in headers or 'libc.so.6' not in dependencies:
        raise RuntimeError(f'{command}: egcl must be dynamically linked against glibc')


def verify(root, limited, targets=None):
    root = root.resolve()
    env = dict(os.environ, EGCL_CROSS_ROOT=str(root / 'usr/libexec/egcl'),
               WINEDEBUG='-all')
    # Wine prefixes are large; keep them on the build filesystem, not /tmp's tmpfs.
    with tempfile.TemporaryDirectory(prefix='verify-', dir=root.parent) as temporary:
        cwd = Path(temporary)
        env['WINEPREFIX'] = str(cwd / 'wine')
        try:
            for target in (targets or TARGETS):
                command = root / 'usr/bin' / ('egcl' if target == 'native' else f'egcl-{target}')
                if not command.is_file():
                    raise RuntimeError(f'Missing packaged command: {command}')
                is_static = target == 'static' or target.endswith('-static')
                if is_static or target == 'native':
                    payload = (command if target in ('native', 'static') else
                               root / 'usr/libexec/egcl' / target / 'egcl')
                    verify_linux_linkage(payload, static=is_static)
                exe = cwd / ('application.exe' if target == 'windows' else 'application')
                # Windows sees the same current directory through Wine. Relative
                # paths avoid requiring a drive mapping in the Lisp source.
                script = cwd / 'dump.lisp'
                script.write_text('(assert (stringp (asdf:asdf-version)))\n'
                                  '(defparameter *saved* (list 19 23))\n'
                                  '(defun app-main () (format t "RPM-IMAGE-OK:~D~%" '
                                  '(apply #\'+ *saved*)))\n'
                                  f'(save-lisp-and-die "{exe.name}" :executable t '
                                  ':toplevel #\'app-main)\n')
                def run(args, extra=None):
                    result = subprocess.run([str(limited), *map(str, args)],
                                            cwd=cwd, env=env | (extra or {}),
                                            text=True, stdout=subprocess.PIPE,
                                            stderr=subprocess.STDOUT)
                    if result.returncode:
                        raise RuntimeError(f'{target}: {args}: exit {result.returncode}\n{result.stdout}')
                    return result.stdout
                checks = ''.join(f'(assert (member {feature} *features*))'
                                 for feature in TARGETS[target])
                if is_static or target == 'native':
                    arch = (HOST_MACHINE if target in ('native', 'static')
                            else target.split('-')[0])
                    arch = RUST_ARCH.get(arch, arch)
                    triple = f'{arch}-unknown-linux-' + ('musl' if is_static else 'gnu')
                    if f'target={triple}' not in run([command, '--runtime-info']).splitlines():
                        raise RuntimeError(f'{command}: wrong runtime target, expected {triple}')
                output = run([command, '--no-init', '--eval',
                              '(progn (assert (stringp (asdf:asdf-version)))' + checks +
                              '(format t "RPM-EVAL-OK~%"))'])
                if 'RPM-EVAL-OK' not in output:
                    raise RuntimeError(f'{target}: unexpected evaluation output: {output}')
                if is_static:
                    output = run([command, '--no-init', '--eval',
                                  "(progn (assert (equal '(42) (egcl-fiber:run-fibers "
                                  '(list (egcl-fiber:make-fiber (lambda () '
                                  '(egcl-fiber:fiber-yield) 42))) :carrier-count 1))) '
                                  '(format t "RPM-FIBER-OK~%"))'])
                    if 'RPM-FIBER-OK' not in output:
                        raise RuntimeError(f'{target}: fiber yield/resume failed: {output}')
                run([command, '--no-init', '--load', 'dump.lisp'])
                if not exe.is_file():
                    raise RuntimeError(f'{target}: executable was not dumped')
                if is_static or target == 'native':
                    verify_linux_linkage(exe, static=is_static)
                if target in ('native', 'static'):
                    runner = []
                elif is_static:
                    runner = [f'qemu-{target.split("-")[0]}']
                elif target == 'windows':
                    runner = ['wine']
                elif target == 'android':
                    runner = ['qemu-aarch64']
                else:
                    runner = ['qemu-' + target.split('-')[0], '-L',
                              str(root / 'usr/libexec/egcl' / target / 'sysroot')]
                output = run([*runner, exe, '--no-init'])
                if 'RPM-IMAGE-OK:42' not in output:
                    raise RuntimeError(f'{target}: dumped image did not restart correctly: {output}')
                probe = [command, '--no-init', '--eval',
                         '(let ((result nil)) (dotimes (i 300) '
                         '(setq result (cons i result))) (list (length result) (car result)))']
                normal = run(probe)
                stressed = run(probe, {'EGCL_GC_STRESS': '100', 'EGCL_GC_POISON': '1'})
                # systemd scope notices can differ; compare the Lisp result.
                expected = '(300 299)'
                if expected not in normal or expected not in stressed:
                    raise RuntimeError(f'{target}: GC probe mismatch: {normal!r}, {stressed!r}')
                exe.unlink()
                print(f'{target}: architecture, ASDF, dump/restart, and GC stress passed', flush=True)
        finally:
            if (cwd / 'wine').exists():
                subprocess.run(['wineserver', '-k'], env=env, check=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--limited', type=Path, default=Path(__file__).resolve().parents[2] / 'scripts/egcl-limited.sh')
    parser.add_argument('--target', action='append', choices=TARGETS,
                        help='Verify only this package (repeatable; default: all)')
    args = parser.parse_args()
    verify(args.root, args.limited.resolve(), args.target)
