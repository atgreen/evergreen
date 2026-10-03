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
# A pinned, checksum-verified rustup-init rather than piping https://sh.rustup.rs:
# that URL is unversioned and unsigned, so it is the one input to the published
# RPMs that could change under us -- silently changing shipped binaries, and
# leaving an old release un-rebuildable. The container is ExclusiveArch x86_64
# (see egcl.spec), so one host triple suffices (bliss-9qu9p).
#
# To update: read the version from
#   https://static.rust-lang.org/rustup/release-stable.toml
# then the digest from the matching
#   https://static.rust-lang.org/rustup/archive/<version>/<triple>/rustup-init.sha256
rustup_version=1.29.1
rustup_sha256=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71
# The basename must stay rustup-init: the binary is multi-call and dispatches on
# argv[0], so under any other name it exits with "unknown proxy name".
mkdir -p /tmp/egcl-rustup
curl --fail --location --retry 3 \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/x86_64-unknown-linux-gnu/rustup-init" \
    -o /tmp/egcl-rustup/rustup-init
printf '%s  %s\n' "$rustup_sha256" /tmp/egcl-rustup/rustup-init | sha256sum --check -
chmod +x /tmp/egcl-rustup/rustup-init
/tmp/egcl-rustup/rustup-init -y --profile minimal --default-toolchain "$toolchain"
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
