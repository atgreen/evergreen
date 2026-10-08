# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Download and execute Alexandria from a pinned public Quicklisp distribution."""

import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("client", type=Path)
    parser.add_argument("lisp", type=Path)
    args = parser.parse_args()
    source = args.client.resolve()
    work = Path(tempfile.mkdtemp(prefix="quicklisp-public-"))
    home = work / "home"
    home.mkdir()
    print(f"Public Quicklisp artifacts: {work}", flush=True)
    for name in ("setup.lisp", "asdf.lisp"):
        shutil.copy2(source / name, home / name)
    shutil.copytree(source / "quicklisp", home / "quicklisp")
    dist_url = "http://beta.quicklisp.org/dist/quicklisp/2026-01-01/distinfo.txt"
    software = str(home / "dists/quicklisp/software") + "/"
    check = (
        f"(load {json.dumps(str(home / 'setup.lisp'))})\n"
        '(setf asdf:*central-registry* nil)\n'
        "(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))\n"
        '(asdf:clear-system "alexandria")\n'
        '(ql:quickload "alexandria" :prompt nil)\n'
        '(assert (equal (alexandria:iota 5 :start 3) \'(3 4 5 6 7)))\n'
        f"(assert (search {json.dumps(software)} "
        '(namestring (asdf:system-source-directory (asdf:find-system "alexandria")))))\n'
        '(write-line "QUICKLISP-PUBLIC-ALEXANDRIA-PASS")\n'
    )
    cold = work / "cold.lisp"
    cold.write_text(
        '(defpackage :quicklisp-quickstart (:use :cl))\n'
        '(defparameter quicklisp-quickstart::*quickstart-parameters* '
        f"'(:initial-dist-url {json.dumps(dist_url)}))\n" + check,
        encoding="utf-8",
    )
    cached = work / "cached.lisp"
    cached.write_text(check, encoding="utf-8")
    for driver in (cold, cached):
        result = subprocess.run(
            [str(args.lisp.resolve()), "--no-init", "--load", str(driver)],
            cwd=home, timeout=300, check=False,
        )
        if result.returncode:
            raise SystemExit(result.returncode)
    assert list((home / "dists/quicklisp/archives").glob("alexandria-*.tgz")), \
        "Quicklisp must download the public release, not use an inherited ASDF copy"


if __name__ == "__main__":
    main()
