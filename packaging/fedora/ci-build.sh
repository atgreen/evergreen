#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
cd /workspace
# The hosted checkout belongs to the runner UID; builds run as container root.
git config --global --add safe.directory "$PWD"
export XDG_RUNTIME_DIR=/run/user/0
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus

# Fail early if this runner cannot enforce the project's resource limits.
scripts/egcl-limited.sh true
toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
curl --fail --location --retry 3 https://sh.rustup.rs -o /tmp/egcl-rustup.sh
sh /tmp/egcl-rustup.sh -y --profile minimal --default-toolchain "$toolchain"
export PATH="/root/.cargo/bin:$PATH"
rustup target add --toolchain "$toolchain" \
    x86_64-unknown-linux-gnu x86_64-unknown-linux-musl \
    s390x-unknown-linux-gnu aarch64-unknown-linux-gnu \
    powerpc64le-unknown-linux-gnu x86_64-pc-windows-gnu \
    aarch64-linux-android x86_64-linux-android
rustup target add --toolchain "$toolchain" aarch64-unknown-linux-musl powerpc64le-unknown-linux-musl
# s390x-musl has no prebuilt Rust std; build it from this exact toolchain's source.
rustup component add --toolchain "$toolchain" rust-src
cargo fetch --locked
bash packaging/fedora/prepare-tools.sh
rpm_release=$(python3 -c 'import json; print(json.load(open("target/release-plan.json"))["rpm_release"])')
python3 packaging/fedora/build.py \
    --android-ndk target/fedora-rpm/tools/android-ndk-r27d --release "$rpm_release"
python3 packaging/fedora/release.py collect
