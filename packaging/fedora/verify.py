#!/usr/bin/env python3
"""Dump and restart applications using a staged or extracted RPM installation."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

TARGETS = {
    'native': (':x86-64', ':linux'),
    's390x-linux': (':s390x', ':big-endian'),
    'aarch64-linux': (':arm64', ':linux'),
    'windows': (':x86-64', ':windows'),
    'android': (':arm64', ':android'),
}


def verify(root, limited):
    root = root.resolve()
    env = dict(os.environ, TORCL_CROSS_ROOT=str(root / 'usr/libexec/torcl'),
               WINEDEBUG='-all')
    # Wine prefixes are large; keep them on the build filesystem, not /tmp's tmpfs.
    with tempfile.TemporaryDirectory(prefix='verify-', dir=root.parent) as temporary:
        cwd = Path(temporary)
        env['WINEPREFIX'] = str(cwd / 'wine')
        try:
            for target in TARGETS:
                command = root / 'usr/bin' / ('torcl' if target == 'native' else f'torcl-{target}')
                if not command.is_file():
                    raise RuntimeError(f'Missing packaged command: {command}')
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
                output = run([command, '--no-init', '--eval',
                              '(progn (assert (stringp (asdf:asdf-version)))' + checks +
                              '(format t "RPM-EVAL-OK~%"))'])
                if 'RPM-EVAL-OK' not in output:
                    raise RuntimeError(f'{target}: unexpected evaluation output: {output}')
                run([command, '--no-init', '--load', 'dump.lisp'])
                if not exe.is_file():
                    raise RuntimeError(f'{target}: executable was not dumped')
                if target == 'native':
                    runner = []
                elif target == 'windows':
                    runner = ['wine']
                elif target == 'android':
                    runner = ['qemu-aarch64']
                else:
                    runner = ['qemu-' + target.split('-')[0], '-L',
                              str(root / 'usr/libexec/torcl' / target / 'sysroot')]
                output = run([*runner, exe, '--no-init'])
                if 'RPM-IMAGE-OK:42' not in output:
                    raise RuntimeError(f'{target}: dumped image did not restart correctly: {output}')
                probe = [command, '--no-init', '--eval',
                         '(let ((result nil)) (dotimes (i 300) '
                         '(setq result (cons i result))) (list (length result) (car result)))']
                normal = run(probe)
                stressed = run(probe, {'TORCL_GC_STRESS': '100', 'TORCL_GC_POISON': '1'})
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
    parser.add_argument('--limited', type=Path, default=Path(__file__).resolve().parents[2] / 'scripts/torcl-limited.sh')
    args = parser.parse_args()
    verify(args.root, args.limited.resolve())
