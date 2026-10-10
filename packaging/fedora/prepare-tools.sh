#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# Download and privately extract official build inputs; never run containers.
set -euo pipefail
cd "$(dirname "$0")/../.."
root=${EGCL_RPM_TOOLS:-$PWD/target/fedora-rpm/tools}
mkdir -p "$root/rpms"
group=${1:-all}
case "$group" in
    all) arches=(s390x aarch64 ppc64le riscv64) ;;
    s390x|aarch64|ppc64le|riscv64) arches=("$group") ;;
    native|windows|android|package) arches=() ;;
    *) echo "Unknown build group: $group" >&2; exit 2 ;;
esac
# Limit repository selection to Fedora; unrelated third-party repos are irrelevant.
for arch in "${arches[@]}"; do
    compiler_arch=$arch
    [[ $arch != ppc64le ]] || compiler_arch=powerpc64le
    packages=("gcc-$compiler_arch-linux-gnu" "binutils-$compiler_arch-linux-gnu")
    # RISC-V ships the self-contained Rust musl runtime only; it needs no
    # Fedora target glibc sysroot or foreign libgcc package.
    [[ $arch == riscv64 ]] || packages+=("sysroot-$arch-fc44-glibc")
    dnf --repo=fedora --repo=updates download --destdir="$root/rpms" "${packages[@]}"
    for package in "$root"/rpms/gcc-"$compiler_arch"-*.rpm \
                   "$root"/rpms/binutils-"$compiler_arch"-*.rpm; do
        rpmkeys --checksig "$package"
        rpm2cpio "$package" | (cd "$root" && cpio -idmu --quiet)
    done
    [[ $arch != riscv64 ]] || continue
    for package in "$root"/rpms/sysroot-"$arch"-*.rpm; do
        rpmkeys --checksig "$package"
        rpm2cpio "$package" | (cd "$root" && cpio -idmu --quiet)
    done
    dnf --repo=fedora --repo=updates --forcearch="$arch" download \
        --destdir="$root/rpms" libgcc
    mkdir -p "$root/targets/$arch"
    for package in "$root"/rpms/libgcc-*."$arch".rpm; do
        rpmkeys --checksig "$package"
        rpm2cpio "$package" | (cd "$root/targets/$arch" && cpio -idmu --quiet)
    done
done
if [[ $group == all || $group == s390x ]]; then
    python3 packaging/fedora/prepare-musl.py --tools "$root"
fi
if [[ $group != all && $group != android && $group != package ]]; then
    exit 0
fi
if [[ -z ${ANDROID_NDK_HOME:-} ]]; then
    archive=$root/android-ndk-r27d-linux.zip
    if [[ ! -f $archive ]]; then
        curl --fail --location --retry 3 \
            https://dl.google.com/android/repository/android-ndk-r27d-linux.zip \
            -o "$archive.part"
        mv "$archive.part" "$archive"
    fi
    # Published at https://developer.android.com/ndk/downloads (r27d LTS).
    printf '%s  %s\n' 22105e410cf29afcf163760cc95522b9fb981121 "$archive" | sha1sum -c -
    if [[ ! -d $root/android-ndk-r27d ]]; then
        unzip -q "$archive" -d "$root"
    fi
    echo "NDK: $root/android-ndk-r27d"
else
    test -f "$ANDROID_NDK_HOME/source.properties"
    echo "Using existing NDK: $ANDROID_NDK_HOME"
fi
