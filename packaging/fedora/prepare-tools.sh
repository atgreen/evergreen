#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# Download and privately extract official build inputs; never run containers.
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD/target/fedora-rpm/tools
mkdir -p "$root/rpms"
# Limit repository selection to Fedora; unrelated third-party repos are irrelevant.
dnf --repo=fedora --repo=updates download --destdir="$root/rpms" \
    gcc-s390x-linux-gnu gcc-aarch64-linux-gnu gcc-powerpc64le-linux-gnu \
    binutils-s390x-linux-gnu binutils-aarch64-linux-gnu binutils-powerpc64le-linux-gnu \
    sysroot-s390x-fc44-glibc sysroot-aarch64-fc44-glibc sysroot-ppc64le-fc44-glibc
for package in "$root"/rpms/gcc-*.rpm "$root"/rpms/binutils-*.rpm "$root"/rpms/sysroot-*.rpm; do
    rpmkeys --checksig "$package"
    rpm2cpio "$package" | (cd "$root" && cpio -idmu --quiet)
done
for arch in s390x aarch64 ppc64le; do
    dnf --repo=fedora --repo=updates --forcearch="$arch" download \
        --destdir="$root/rpms" libgcc
    mkdir -p "$root/targets/$arch"
    for package in "$root"/rpms/libgcc-*."$arch".rpm; do
        rpmkeys --checksig "$package"
        rpm2cpio "$package" | (cd "$root/targets/$arch" && cpio -idmu --quiet)
    done
done
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
