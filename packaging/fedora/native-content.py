#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Build and stage the native RPM's Java system and offline HTML manual."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess


def install(source, manual, stage, *, libdir, datadir, docdir):
    installer = stage / 'usr/bin/install-egcl-forks'
    installer.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source / 'scripts/install-egcl-forks', installer)
    installer.chmod(0o755)
    system = stage / datadir.lstrip('/') / 'common-lisp/source/egcl-jvm'
    library = stage / libdir.lstrip('/') / 'egcl/libegcl_jvm.so'
    system.mkdir(parents=True, exist_ok=True)
    library.parent.mkdir(parents=True, exist_ok=True)
    for name in ('egcl-jvm.asd', 'package.lisp', 'jvm.lisp', 'api.lisp'):
        shutil.copy2(source / 'lib/egcl-jvm' / name, system / name)
    shutil.copy2(source / 'lib/egcl-jvm/build/libegcl_jvm.so', library)
    link = system / library.name
    link.unlink(missing_ok=True)
    link.symlink_to(os.path.relpath(library, system))
    destination = stage / docdir.lstrip('/') / 'egcl/manual'
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(manual, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--libdir', default='/usr/lib64')
    parser.add_argument('--datadir', default='/usr/share')
    parser.add_argument('--docdir', default='/usr/share/doc')
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[2]
    subprocess.run(['make', '-C', str(source / 'lib/egcl-jvm')], check=True)
    manual = source / 'build/rpm-manual'
    # File URLs need explicit index.html links, rather than web-server redirects.
    config = source / 'mkdocs-rpm.yml'
    config.write_text('INHERIT: mkdocs.yml\nuse_directory_urls: false\n')
    try:
        subprocess.run(['mkdocs', 'build', '--strict', '--config-file', str(config),
                        '--site-dir', str(manual)], cwd=source, check=True)
    finally:
        config.unlink()
    install(source, manual, args.stage.resolve(), libdir=args.libdir,
            datadir=args.datadir, docdir=args.docdir)


if __name__ == '__main__':
    main()
