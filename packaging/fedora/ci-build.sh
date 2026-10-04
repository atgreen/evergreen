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
# leaving an old release un-rebuildable. One digest PER HOST TRIPLE: the
# container runs on x86_64 for the cross-targeting release and on ppc64le,
# under qemu-user, for the POWER-native one (egcl.spec's ExclusiveArch).
# Originally bliss-9qu9p, when x86_64 was the only host.
#
# To update: read the version from
#   https://static.rust-lang.org/rustup/release-stable.toml
# then the digest from the matching
#   https://static.rust-lang.org/rustup/archive/<version>/<triple>/rustup-init.sha256
rustup_version=1.29.1
case "$(uname -m)" in
    x86_64)
        rustup_triple=x86_64-unknown-linux-gnu
        rustup_sha256=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71 ;;
    ppc64le)
        rustup_triple=powerpc64le-unknown-linux-gnu
        rustup_sha256=86ebc5b46b20013fca57dff4f83aa0c325ee90914626ce1c80ae76e126e48afb ;;
    s390x)
        rustup_triple=s390x-unknown-linux-gnu
        rustup_sha256=312ac9390467fd7870f8aff6557c7cb3bff0a04bb11b263173fdfd81586da641 ;;
    aarch64)
        rustup_triple=aarch64-unknown-linux-gnu
        rustup_sha256=15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433 ;;
    *)
        echo "No pinned rustup-init for $(uname -m); see egcl.spec's ExclusiveArch" >&2
        exit 2 ;;
esac
# The basename must stay rustup-init: the binary is multi-call and dispatches on
# argv[0], so under any other name it exits with "unknown proxy name".
mkdir -p /tmp/egcl-rustup
curl --fail --location --retry 3 \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/$rustup_triple/rustup-init" \
    -o /tmp/egcl-rustup/rustup-init
printf '%s  %s\n' "$rustup_sha256" /tmp/egcl-rustup/rustup-init | sha256sum --check -
chmod +x /tmp/egcl-rustup/rustup-init
/tmp/egcl-rustup/rustup-init -y --profile minimal --default-toolchain "$toolchain"
export PATH="/root/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN="$toolchain"
case "$phase" in
    source)
        # ocicl fetches the Lisp dependencies the APK builder pins in
        # lib/egcl-apk/ocicl.csv. That tree is gitignored, so build.py's
        # vendor_lisp_dependencies installs it into the source snapshot, and
        # creating the SRPM is the one step allowed to reach the network.
        #
        # Only this phase needs it, and only x86_64 has it: the repo publishes
        # el8/el9 x86_64 alone, and the `source` phase is the amd64 job (the
        # POWER-native container runs the `binary` phase). Putting it here
        # rather than in the shared Containerfile keeps the ppc64le image
        # buildable.
        #
        # GPG-verified but not version-pinned, unlike rustup-init above, and the
        # difference is deliberate: ocicl.csv pins every dependency by sha256
        # digest, so which ocicl fetches them cannot change the vendored bytes.
        # The one output that could drift is the wording of BUNDLED-LICENSES.txt
        # from `ocicl collect-licenses`; vendor_lisp_dependencies fails the build
        # if that comes back empty.
        rpm --import https://ocicl.github.io/ocicl/rpm-repo/RPM-GPG-KEY-ocicl
        cat > /etc/yum.repos.d/ocicl.repo <<'OCICL_REPO'
[ocicl]
name=ocicl - OCI-based Common Lisp package manager
baseurl=https://ocicl.github.io/ocicl/rpm-repo
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://ocicl.github.io/ocicl/rpm-repo/RPM-GPG-KEY-ocicl
OCICL_REPO
        dnf -y install ocicl
        ocicl version
        rustup component add rust-src
        cargo fetch --locked
        std_manifest=$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.toml
        RUSTC_BOOTSTRAP=1 cargo fetch --locked --manifest-path "$std_manifest"
        # Fedora's rpmlint with Fedora's config, which is the review this
        # gate is standing in for; Ubuntu's would report a different set.
        python3 packaging/fedora/rpmlint.py
        # Here, not in the collector: this is the only job with cargo and a
        # fetched dependency tree, which `cargo metadata --offline` needs.
        python3 packaging/fedora/sbom.py --output target/egcl-sbom.cdx.json
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
