#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Run under egcl-limited.sh; uses Wine and a cross-built child test fixture."""
import concurrent.futures
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile


def lisp_string(text):
    # Common Lisp uses backslash escapes, not JSON's \n or \uNNNN notation.
    return '"' + text.replace('\\', '\\\\').replace('"', '\\"') + '"'


def windows_path(path):
    return 'Z:' + Path(path).resolve().as_posix()


def main():
    binary, child = sys.argv[1:]
    env = os.environ.copy()
    env['EGCL_FORCE_TIER'] = 't0'

    def run(form, stress=False, bootstrap=True):
        local_env = env.copy()
        if stress:
            local_env.update(EGCL_GC_STRESS='1', EGCL_GC_POISON='1')
        # Loading UTF-8 source avoids relying on the Windows command-line decoder
        # for the Unicode values whose child-process transport we are testing.
        with tempfile.NamedTemporaryFile(mode='w', suffix='.lisp', encoding='utf-8') as source:
            source.write(form)
            source.flush()
            command = ['wine', binary, '--no-init']
            if not bootstrap:
                command.append('--no-bootstrap')
            result = subprocess.run(command + ['--load', windows_path(source.name)],
                                    env=local_env, text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
        return result.stdout

    shell = Path(__file__).with_name('windows-process.lisp').read_text()
    for tier in ['interp', 't0']:
        env['EGCL_FORCE_TIER'] = tier
        assert 'WINDOWS-PROCESS-OK' in run(shell)
    print('win64: native shell, redirections and exit status: OK', flush=True)

    with tempfile.TemporaryDirectory(prefix='egcl child ') as directory:
        executable = Path(directory) / 'child with spaces.exe'
        shutil.copyfile(child, executable)
        args = ['', 'two words', 'quote"inside', 'C:\\path with space\\', '&|<>^%', 'café-λ']
        argv = '(list ' + ' '.join(map(lisp_string, [windows_path(executable)] + args)) + ')'
        expected = ''.join(f'{len(arg.encode("utf-8"))}:{arg}\n' for arg in args)
        form = f'''(multiple-value-bind (status out err) (egcl-ext:run-program {argv})
                    (assert (= status 23))
                    (assert (string= out {lisp_string(expected)}))
                    (assert (string= err "")))
                   (format t "ARGV-OK~%")'''
        for tier in ['interp', 't0']:
            env['EGCL_FORCE_TIER'] = tier
            assert 'ARGV-OK' in run(form)
        quoted_shell = f'"{windows_path(executable)}" "two words"'
        assert 'QUOTED-SHELL-OK' in run(f'''
            (multiple-value-bind (status out err) (egcl-ext:run-program {lisp_string(quoted_shell)})
              (assert (= status 23)) (assert (search "9:two words" out)) (assert (string= err "")))
            (format t "QUOTED-SHELL-OK~%")''')
        print('win64: direct argv preserves spaces, quotes, backslashes and Unicode: OK', flush=True)

        # No prelude: stress every allocating step that returns the two strings.
        pipes = f'''(multiple-value-bind (status out err)
                     (egcl-ext:run-program (list {lisp_string(windows_path(executable))} "pipes"))
                     (if (and (= status 23) (= (length out) 131072) (= (length err) 131072)
                              (char= (aref out 131071) #\\O) (char= (aref err 131071) #\\E))
                         (format t "PIPES-OK~%") (error "pipe capture mismatch")))'''
        reference = run(pipes, bootstrap=False)
        assert 'PIPES-OK' in reference and run(pipes, stress=True, bootstrap=False) == reference
        print('win64: simultaneous output pipes and GC-stressed multiple values: OK', flush=True)

    for stress in [False, True]:
        with socket.socket() as listener, concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            listener.bind(('127.0.0.1', 0))
            listener.listen()
            listener.settimeout(30)

            def serve():
                peer, _ = listener.accept()
                with peer:
                    peer.settimeout(30)
                    peer.sendall(bytes([65, 200]))
                    assert peer.recv(1) == bytes([42])
                    peer.sendall(bytes([255]))

            peer = executor.submit(serve)
            port = listener.getsockname()[1]
            source = f'''
              (defun check-io (v) (if v t (error "socket check failed")))
              (let ((s (egcl::%socket-connect "127.0.0.1" {port} 2000)))
                (check-io (egcl::%socket-wait-for-input s 2000))
                (check-io (= (egcl::%socket-read-timeout s 2000) 2000))
                (check-io (= (read-byte s) 65))
                (check-io (= (read-byte s) 200))
                (write-byte 42 s) (finish-output s)
                (check-io (= (read-byte s) 255))
                (check-io (eq (read-byte s nil :end) :end))
                (close s) (check-io (not (open-stream-p s)))
                (format t "SOCKET-OK~%"))'''
            output = run(source, stress=stress, bootstrap=False)
            peer.result(timeout=5)
            assert 'SOCKET-OK' in output
    print('win64: incremental CLI socket exchange and GC stress: OK', flush=True)


if __name__ == '__main__':
    main()
