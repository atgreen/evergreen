#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
cd "$(dirname "$0")/../.."
podman build -t egcl-fedora-release -f packaging/fedora/Containerfile packaging/fedora
container=$(podman run --detach --systemd=always --security-opt label=disable \
    --volume "$PWD:/workspace:rw" egcl-fedora-release)
cleanup() {
    podman logs "$container"
    podman rm --force "$container" >/dev/null
}
trap cleanup EXIT
deadline=$((SECONDS + 60))
until podman exec "$container" systemctl start user@0.service; do
    if (( SECONDS >= deadline )); then
        echo 'Fedora systemd user manager did not start' >&2
        exit 1
    fi
    sleep 1
done
podman exec "$container" bash packaging/fedora/ci-build.sh
