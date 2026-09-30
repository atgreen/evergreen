#!/usr/bin/env bash
# Cross-build final CLI binaries; optionally verify their OS ABI and Lisp behavior.
set -euo pipefail
cd "$(dirname "$0")/.."

action=${1:-build}
architecture=${2:-all}
if [[ $# -gt 2 || ! $action =~ ^(build|test)$ || ! $architecture =~ ^(all|aarch64|ppc64le|s390x)$ ]]; then
    echo "usage: $0 [build|test] [all|aarch64|ppc64le|s390x]" >&2
    exit 2
fi
cross=${CROSS:-cross}
# The 0.2.5 cross images carry an older host glibc: Rust 1.94.1's build
# scripts require symbols those images do not provide. Keep the repository's
# normal 1.94.1 pin untouched, but use the newest compatible compiler for the
# foreign-target container. Callers can override this with RUSTUP_TOOLCHAIN.
export RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-1.93.0}
export CROSS_CONTAINER_ENGINE=${CROSS_CONTAINER_ENGINE:-podman}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
# Containers can have their own cgroup outside the egcl-limited scope.
export CROSS_CONTAINER_OPTS="${CROSS_CONTAINER_OPTS:-} --memory=${EGCL_MEM_MAX:-4G} --memory-swap=${EGCL_MEM_MAX:-4G}"
command -v "$cross" >/dev/null
command -v "$CROSS_CONTAINER_ENGINE" >/dev/null
architectures=($architecture)
if [[ $architecture == all ]]; then
    architectures=(aarch64 ppc64le s390x)
fi

for arch in "${architectures[@]}"; do
    case $arch in
        aarch64) target=aarch64-unknown-linux-gnu; toolchain=aarch64-linux-gnu ;;
        ppc64le) target=powerpc64le-unknown-linux-gnu; toolchain=powerpc64le-linux-gnu ;;
        s390x) target=s390x-unknown-linux-gnu; toolchain=s390x-linux-gnu ;;
    esac
    "$cross" build --locked --release -p egcl --bin egcl --target "$target" --features egcl-rt/c-ffi
    binary="${CARGO_TARGET_DIR:-target}/$target/release/egcl"
    echo "Built $binary"
    if [[ $action == build ]]; then
        continue
    fi
    command -v "qemu-$arch" >/dev/null
    scripts/egcl-limited.sh "$cross" test --locked -p egcl-rt --target "$target" \
        --features c-ffi --test portable_os

    # Match the libraries used by the linker. Use the host's current QEMU for
    # the CLI so no binfmt registration or foreign guest installation is needed.
    image=$(python3 - "$target" <<'PY'
import sys, tomllib
with open("Cross.toml", "rb") as f:
    print(tomllib.load(f)["target"][sys.argv[1]]["image"])
PY
    )
    sysroot=$(mktemp -d "${TMPDIR:-/tmp}/egcl-$arch-sysroot.XXXXXX")
    trap 'rm -rf -- "$sysroot"' EXIT
    "$CROSS_CONTAINER_ENGINE" run --rm --entrypoint tar "$image" \
        -C "/usr/$toolchain" -cf - lib | tar -C "$sysroot" -xf -
    if [[ $arch == ppc64le ]]; then
        ln -s lib "$sysroot/lib64"
    fi
    scripts/egcl-limited.sh python3 scripts/portability-smoke.py "$arch" -- \
        "qemu-$arch" -L "$sysroot" "$binary"
    if [[ $arch == s390x ]]; then
        scripts/egcl-limited.sh python3 scripts/s390x-jit-smoke.py -- \
            qemu-s390x -L "$sysroot" "$binary"
    fi
    rm -rf -- "$sysroot"
    trap - EXIT
done
