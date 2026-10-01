#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
cd /workspace
phase=${1:?Expected source or binary}
group=${2:-all}
git config --global --add safe.directory "$PWD"
export XDG_RUNTIME_DIR=/run/user/0
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
scripts/egcl-limited.sh true

toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
curl --fail --location --retry 3 https://sh.rustup.rs -o /tmp/egcl-rustup.sh
sh /tmp/egcl-rustup.sh -y --profile minimal --default-toolchain "$toolchain"
export PATH="/root/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN="$toolchain"
case "$phase" in
    source)
        rustup component add rust-src
        cargo fetch --locked
        std_manifest=$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.toml
        RUSTC_BOOTSTRAP=1 cargo fetch --locked --manifest-path "$std_manifest"
        python3 packaging/fedora/source-rpm.py create
        ;;
    binary)
        srpm=${3:?Expected the shared SRPM path}
        triple_text=$(python3 - "$group" <<'PY'
import runpy, sys
builder = runpy.run_path('packaging/fedora/build.py')
for name in builder['GROUPS'][sys.argv[1]]:
    print(builder['TARGETS'][name])
if sys.argv[1] == 'android':
    print('x86_64-linux-android')
PY
        )
        mapfile -t triples <<< "$triple_text"
        for triple in "${triples[@]}"; do
            if [[ $triple == s390x-unknown-linux-musl ]]; then
                rustup component add rust-src
            else
                rustup target add "$triple"
            fi
        done
        python3 packaging/fedora/source-rpm.py rebuild "$group" "$srpm"
        ;;
    *) echo "Unknown build phase: $phase" >&2; exit 2 ;;
esac
