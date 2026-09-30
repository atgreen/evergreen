#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Exercise installed Java APIs without compilers or repository discovery."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def verify(stage):
    stage = stage.resolve()
    system = stage / 'usr/share/common-lisp/source/egcl-jvm'
    assert (system / 'libegcl_jvm.so').is_file(), 'Missing native JVM bridge'
    assert not (system / 'Makefile').exists(), 'Installed system must not need compilation'
    manual = stage / 'usr/share/doc/egcl/manual'
    for name in ('index.html', 'java.html', 'search/search_index.json'):
        assert (manual / name).is_file(), f'Missing manual file: {name}'
    with tempfile.TemporaryDirectory(prefix='egcl-installed-java-') as temporary:
        work = Path(temporary)
        blocked = work / 'bin'
        blocked.mkdir()
        for name in ('make', 'cc', 'gcc', 'javac'):
            command = blocked / name
            command.write_text('#!/bin/sh\necho "Unexpected build tool invocation" >&2\nexit 99\n')
            command.chmod(0o755)
        script = work / 'check.lisp'
        script.write_text('''(require :asdf)
(asdf:initialize-source-registry '(:source-registry :default-registry (:ignore-inherited-configuration)))
(asdf:initialize-output-translations
 `(:output-translations (t (,(uiop:getenv "XDG_CACHE_HOME") :implementation))
   :ignore-inherited-configuration))
(asdf:load-system :egcl-jvm)
(unless (equal (truename (asdf:system-source-directory :egcl-jvm))
               (truename (uiop:getenv "EGCL_EXPECTED_JVM_SYSTEM")))
  (error "ASDF loaded a different Java system"))
(let ((vm (java:start-jvm :options '("-Xcheck:jni" "-Xmx128m"))))
  (assert (= 42 (java:static "java.lang.Integer" "parseInt" "42")))
  (assert (= 42 (egcl-jvm:call-static "java.lang.Math" "abs" "(I)I" -42)))
  (java:with-scope ()
    (let ((callback (java:lambda "java.util.function.IntUnaryOperator" (x)
                      (java:static "java.lang.Math" "addExact" x 1))))
      (assert (= 42 (java:call callback "applyAsInt" 41)))))
  (java:stop-jvm vm))
(format t "INSTALLED-JAVA-PASS~%")
''')
        env = os.environ.copy()
        env.update(PATH=str(blocked) + os.pathsep + env['PATH'],
                   XDG_DATA_DIRS=str(stage / 'usr/share'),
                   XDG_DATA_HOME=str(work / 'data'),
                   XDG_CONFIG_HOME=str(work / 'config'),
                   XDG_CONFIG_DIRS=str(work / 'config'),
                   XDG_CACHE_HOME=str(work / 'cache'),
                   EGCL_EXPECTED_JVM_SYSTEM=str(system))
        result = subprocess.run([str(ROOT / 'scripts/egcl-limited.sh'),
                                 str(stage / 'usr/bin/egcl'), '--no-init', '--load', str(script)],
                                cwd=work, env=env, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT)
        print(result.stdout)
        result.check_returncode()
        assert 'INSTALLED-JAVA-PASS' in result.stdout.splitlines(), 'Java probe did not finish'
        assert 'WARNING in native method' not in result.stdout, 'JNI check failed'
        assert 'WARNING: JNI' not in result.stdout, 'JNI check failed'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', type=Path)
    verify(parser.parse_args().stage)
