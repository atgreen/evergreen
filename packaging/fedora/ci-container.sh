#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
cd "$(dirname "$0")/../.."
# EGCL_CONTAINER_PLATFORM selects a foreign architecture, e.g. linux/ppc64le for
# the POWER-native packages. The Containerfile's pinned Fedora 44 digest is an
# OCI image index covering amd64, arm64, ppc64le and s390x, so the SAME pin
# serves every architecture and reproducibility is unaffected. The caller must
# have registered a binfmt_misc handler for the target (qemu-user-static);
# without one, podman fails on the first RUN with an exec format error.
platform=${EGCL_CONTAINER_PLATFORM:-}
# A tag per platform: otherwise a second architecture's build would silently
# replace the first one's image under the same name.
image=egcl-fedora-release
select_platform=()
# Emulated systemd takes appreciably longer to bring up the user manager than a
# native one, so the wait is generous when a platform is forced.
startup_seconds=60
if [[ -n $platform ]]; then
    image+="-${platform##*/}"
    select_platform=(--platform "$platform")
    startup_seconds=600
fi
podman build "${select_platform[@]}" -t "$image" \
    -f packaging/fedora/Containerfile packaging/fedora
container=$(podman run --detach --systemd=always --security-opt label=disable \
    "${select_platform[@]}" --volume "$PWD:/workspace:rw" "$image")
cleanup() {
    podman logs "$container"
    podman rm --force "$container" >/dev/null
}
trap cleanup EXIT
deadline=$((SECONDS + startup_seconds))
until podman exec "$container" systemctl start user@0.service; do
    if (( SECONDS >= deadline )); then
        echo 'Fedora systemd user manager did not start' >&2
        exit 1
    fi
    sleep 1
done
podman exec "$container" bash packaging/fedora/ci-build.sh "$@"
