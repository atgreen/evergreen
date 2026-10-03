# Fedora RPMs and cross image-dumping tools

This experimental packaging path builds glibc and musl x86-64 EGCL and eight
optional target packages. Local building and installed use are container-free;
GitHub Actions uses a Fedora container on its Ubuntu runner.
It produces local binary RPMs; it is not yet a Fedora-reviewed source RPM.
The initial build baseline is Fedora 44 and Rust 1.94.1.

| Package | Command | Application target | Host runner |
| --- | --- | --- | --- |
| `egcl` | `egcl` | Fedora x86-64, glibc | Native |
| `egcl-static` | `egcl-static` | Linux x86-64, static musl | Native |
| `egcl-target-s390x-linux` | `egcl-s390x-linux` | Fedora s390x | QEMU |
| `egcl-target-aarch64-linux` | `egcl-aarch64-linux` | Fedora AArch64 | QEMU |
| `egcl-target-ppc64le-linux` | `egcl-ppc64le-linux` | Fedora ppc64le (little-endian POWER) | QEMU |
| `egcl-target-s390x-linux-static` | `egcl-s390x-linux-static` | Linux s390x, static musl | QEMU |
| `egcl-target-aarch64-linux-static` | `egcl-aarch64-linux-static` | Linux AArch64, static musl | QEMU |
| `egcl-target-ppc64le-linux-static` | `egcl-ppc64le-linux-static` | Linux ppc64le, static musl | QEMU |
| `egcl-target-windows` | `egcl-windows` | Windows x86-64 | Wine |
| `egcl-target-android` | `egcl-android`, `egcl-android-new` | ARM64 CLI; ARM64 and x86-64 APKs, API 28+ | QEMU for CLI; device/emulator for APK |

The normal `egcl` command uses glibc and supports JVM integration. `egcl-static`
is a separate subpackage with no dependency on the main package or its JVM;
its runtime and saved executables have no dynamic loader or shared-library
dependencies. Both binaries can be installed together.
The three cross Linux `-static` packages likewise run without a target sysroot
and can be installed independently of `egcl`. Their launchers use QEMU, but the
executables they save run directly on the target architecture. Static runtimes
do not provide the glibc runtime's dynamic shared-library FFI or JVM integration.

Each command-line runtime has ASDF preloaded. Target tools accept the normal EGCL arguments;
they execute the target runtime on the Fedora host, where it can load an
application and dump an executable for its own architecture. They do not
translate an existing x86-64 heap image into another platform's image.

## Build

Install host prerequisites (Rust through rustup):

```sh
sudo dnf install gcc clang binutils rpm-build rpm cpio python3 curl unzip \
    qemu-user wine mingw64-gcc mingw64-binutils glibc make java-devel \
    mkdocs mkdocs-material
rustup toolchain install 1.94.1 --profile minimal
rustup target add --toolchain 1.94.1 x86_64-unknown-linux-gnu x86_64-unknown-linux-musl s390x-unknown-linux-gnu \
    aarch64-unknown-linux-gnu powerpc64le-unknown-linux-gnu x86_64-pc-windows-gnu aarch64-linux-android x86_64-linux-android
rustup target add --toolchain 1.94.1 aarch64-unknown-linux-musl powerpc64le-unknown-linux-musl
rustup component add --toolchain 1.94.1 rust-src
bash packaging/fedora/prepare-tools.sh
python3 packaging/fedora/build.py \
    --android-ndk target/fedora-rpm/tools/android-ndk-r27d
```

The preparation script downloads official Fedora cross-compiler, sysroot, and
`libgcc` RPMs into `target/fedora-rpm/tools` and extracts them privately. It also
downloads Google's NDK r27d and checks the published checksum. It does not install
foreign RPMs in the host RPM database, require root, or invoke containers.
Use `ANDROID_NDK_HOME` to supply an existing NDK and avoid that download; pass the
same directory to `build.py --android-ndk`.

Rust supplies prebuilt musl standard libraries for x86-64, AArch64 and POWER.
For s390x, the preparation script builds checksum-pinned musl 1.2.5 and LLVM
libunwind 21.1.8 using Fedora's cross GCC and Clang. The builder then uses
`-Z build-std=std,panic_unwind` with `RUSTC_BOOTSTRAP=1` scoped to the s390x musl
build on the pinned Rust toolchain. This avoids requiring an s390x build host
or changing the project's compiler version. The saved runtime includes these
libraries statically; installed users need none of the build tools.

The build uses ordinary `cargo`, native MinGW, Fedora cross-GCC and the NDK.
The native RPM also installs `install-egcl-forks` in `/usr/bin`. Run
`install-egcl-forks /path/to/project` to install the EGCL library ports, or add
`--dry-run` before the project path to preview the commands. This requires `git`
and an `ocicl` version with Git-source support on `PATH`; these tools are checked
when the installer runs. Installation updates the project's `ocicl.csv`.

The native RPM includes the `JAVA` and `EGCL-JVM` APIs as the ASDF system
`egcl-jvm`, and `/usr/lib64/egcl/libegcl_jvm.so` with its Java helper classes
embedded. `%build` compiles this bridge from source using a JDK (17+) and builds
the HTML manual with MkDocs. Installed users need only a Java runtime; starting
the JVM never invokes a compiler. Load it from any directory:

```lisp
(asdf:load-system :egcl-jvm)
(defparameter *jvm* (java:start-jvm))
(java:static "java.lang.Integer" "parseInt" "42")
(java:stop-jvm *jvm*)
```

The manual is RPM documentation at `/usr/share/doc/egcl/manual/index.html`.
It includes local assets and explicit HTML links for browsing without a server.
Java integration is for the native glibc binary, not the cross-target runtimes.

The Android app libraries are compiled by the spec's `%build`, from a source
archive with locked, vendored Cargo dependencies (`--offline`). The local builder
sets `egcl_rustup=1` because Rust is installed through rustup; direct rpmbuild
otherwise requires Fedora's `cargo` package. Pass the absolute NDK directory as
`--define 'android_ndk /path/to/android-ndk-r27d'`. Both Android Rust targets must
already be installed. The local `build.py` packaging path reuses prebuilt CLI
payloads; the release workflow below rebuilds every runtime from its shared SRPM.
It strips debug sections before dumping ASDF images. RPM stripping and debuginfo
extraction are disabled because modifying an executable after dumping can
remove its appended image. Cross ELF files are excluded from host dependency
and provides generation. The native binary retains automatic ELF dependencies.

Builds and image probes run through `scripts/egcl-limited.sh`; a working user
systemd session is required. `CARGO_BUILD_JOBS`, `EGCL_MEM_MAX`, and
`EGCL_TIMEOUT` control resource limits. The builder does not use PGO.

RPMs appear in `target/fedora-rpm/RPMS/x86_64/`. The builder checks each staged
runtime, packages it, extracts the RPMs, compares payload bytes, then repeats
architecture/ASDF/evaluation/application-dump/restart and GC stress checks
in an isolated working directory without application sources.
`--package-only` reuses the staged CLI images, rebuilds both Android app libraries
from current source in `%build`, and verifies the extracted RPMs. Use it only
when deliberately reusing that stage's CLI binaries.
For a focused build, `--stage-only --target s390x-linux-static` builds and
verifies that payload without producing an incomplete RPM release. Repeat
`--target` to select additional payloads.

## GitHub releases

The **Fedora releases** workflow builds and verifies all ten RPMs on Fedora
44, including the glibc `egcl` and musl `egcl-static` packages. It runs the same
packaging checks described above, with systemd memory limits inside its Fedora
container. Runs finish independently when newer commits are pushed.
A source job builds one SRPM containing EGCL source, vendored Rust dependencies,
and the musl and LLVM unwinder sources, then uploads the `fedora44-srpm` artifact.
Six parallel builder jobs download that exact SRPM and rebuild the native,
s390x, AArch64, POWER, Windows and Android package groups. Each Linux group
produces both glibc and musl RPMs. Each builder verifies its extracted RPMs and
records the source RPM's SHA-256 checksum. A final collector requires matching
source/toolchain provenance and a complete ten-package set before publication.
A failed matrix job does not cancel the other builds.

The same source build can be reproduced locally after installing the pinned Rust
toolchain, its `rust-src` component and the selected target's standard libraries:

```sh
GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/main \
GITHUB_RUN_ID=1 GITHUB_RUN_ATTEMPT=1 RELEASE_MODE=build \
    python3 packaging/fedora/release.py plan
python3 packaging/fedora/source-rpm.py create --plan target/release-plan.json
python3 packaging/fedora/source-rpm.py rebuild s390x target/fedora-rpm/SRPMS/egcl-*.src.rpm
```

The source job needs downloaded Cargo dependencies, including those of the Rust
standard library, for vendoring. Rebuilding prepares cross toolchains before
`rpmbuild`; compilation inside the spec uses vendored dependencies offline.
Use a separate output directory for each group when rebuilding locally.

For a test release, select **Actions → Fedora releases → Run workflow**, choose
the branch and leave `mode` set to `test`. The equivalent CLI command is:

```sh
gh workflow run release.yml --ref main -f mode=test
```

This publishes a prerelease tagged `test-v0.0.1-RUN_ID-ATTEMPT`. It does not
become GitHub's latest stable release. Its RPM release is
`0.test.RUN_ID.ATTEMPT.fc44`, which sorts below the stable release for the same
version. Install matching versions of the main and target subpackages together.

Choose `mode=build` to exercise the entire build and download the RPMs from the
workflow's `fedora44-rpms` artifact without creating a tag or GitHub release.

For a stable release, update the workspace version and changelog, commit them,
then push a matching version tag, for example `v0.0.1`. The workflow rejects a
tag that differs from the workspace version. Stable RPMs use the spec's release
number (currently `6.fc44`). An existing release is never overwritten; a failed
upload can leave a draft for inspection before retrying.

Published assets include all ten binary RPMs, the shared SRPM, `CHANGELOG.md`, build provenance,
release metadata, the `RPM-GPG-KEY-egcl` public key, and `SHA256SUMS`.
Publication runs only after package identity, payload, and runtime checks pass.
Write permission and the signing key are confined to the publish job, which runs
in a protected `release` environment that only `main` and `v*` tags may deploy
to, and which waits for maintainer approval.

Two runs for the same ref never publish concurrently. Test prereleases beyond
the three most recent are pruned automatically.

### Package signing

Every published RPM, the SRPM included, is GPG-signed in the publish job:

```
EGCL RPM Signing Key <green@moxielogic.com>
RSA 4096, key ID F2EAEAEE344F7576
fingerprint 6101 7475 407E 35EB 2608 BF2B F2EA EAEE 344F 7576
```

Signing rewrites each RPM header, so it happens in a defined order: the
`SHA256SUMS` produced by the collector is checked first, which verifies the
hand-off from the build jobs; the packages are then signed; each signature is
verified against `packaging/fedora/RPM-GPG-KEY-egcl` in a keyring holding no
other key; and only then is `SHA256SUMS` rewritten over the signed bytes. The
published manifest therefore describes exactly the files you download.

To verify what you downloaded:

```sh
sha256sum --check SHA256SUMS
sudo rpmkeys --import RPM-GPG-KEY-egcl
rpmkeys --checksig egcl-*.rpm
```

Each package must report `digests signatures OK`. Note that `digests OK`
*without* the word `signatures` means the package is **unsigned** — and
`rpmkeys --checksig` still exits 0 in that case, so read the output rather than
relying on the exit status. Verify the fingerprint above out of band before
importing; a key shipped beside the packages it signs only proves they came from
the same place.

## Install and use

Install matching releases of the native package and whichever target packages
you need. After building release 6, for example:

```sh
sudo dnf install target/fedora-rpm/RPMS/x86_64/egcl-0.0.1-6.fc44.x86_64.rpm \
    target/fedora-rpm/RPMS/x86_64/egcl-target-android-0.0.1-6.fc44.x86_64.rpm
```

For example, put this in `build.lisp` after your application's loading code:

```lisp
(defun main () (format t "Hello from EGCL!~%"))
(save-lisp-and-die "hello" :executable t :toplevel #'main)
```

Then run `egcl-s390x-linux --no-init --load build.lisp` to produce an s390x
executable. Use `egcl-aarch64-linux`, `egcl-windows`, or `egcl-android` for
the other targets. Give Windows outputs an `.exe` suffix. Relative filenames
and forward slashes work conveniently under Wine.

The output includes EGCL and the saved application. QEMU/Wine are needed only
on the build host, not on the target. Linux applications still require compatible
glibc and libgcc on the deployment system, except for the `-static` variants.
A Fedora 44 glibc build is not a promise of
compatibility with older RHEL, Ubuntu, or SUSE releases. Android uses static
bionic and does not need the builder's sysroot; it is a CLI executable, not an
APK, and cannot dynamically load Android shared libraries. Device execution
must follow Android's executable-file and application sandbox rules.

## Android application projects

The same Android RPM includes reusable `libegcl_android.so` libraries for both
`aarch64-linux-android` (`arm64-v8a`) and `x86_64-linux-android` (`x86_64`), plus
`egcl-android-new`. These libraries support Android's dynamic FFI and load the
application's Lisp from APK assets. App builds require the Android SDK, a JDK,
Python 3 and Make; no EGCL source checkout, Rust, NDK or containers are needed.

```sh
egcl-android-new hello --package org.example.hello --template egl \
    --host=aarch64-linux-android
cd hello
export ANDROID_HOME=/path/to/android-sdk
make doctor
make install
make run

# Build for an x86-64 emulator, or include both libraries in one APK:
make HOST=x86_64-linux-android
make install HOST=x86_64-linux-android SERIAL=emulator-5554
make HOSTS="aarch64-linux-android x86_64-linux-android"
```

Edit `assets/app.lisp` and rebuild. The `egl` template draws a color that changes
on touch; `--template=minimal` logs messages and input without drawing. Generated
projects include the runtime bindings, Makefile, editable manifest, runtime
version metadata, signing instructions and their own README. `make release`
uses an explicitly supplied keystore; `make clean` preserves the debug key.
Run `make install-tools` once to install SDK platform-tools, build-tools and the
platform selected by the manifest. It bootstraps checksum-verified Google
command-line tools if needed and prompts for SDK licenses. Install a JDK first
(`sudo dnf install java-21-openjdk-devel` on Fedora). Use `SDK=/path/to/sdk`
for a custom writable SDK directory, and `SDK_BUILD_TOOLS=35.0.0` to select the
build-tools version. Ordinary builds do not download tools.

SDK paths and device selection can be stored in ignored `local.mk`.

APKs are separated by host under `build/`. `install` rejects a device/ABI mismatch.
The app host retains one Lisp worker across surface recreation, waits for EGL
cleanup before releasing a window, and exposes pause state and touch events.
Saved-image APK payloads are not supported yet. The app libraries have checked
16 KiB ELF LOAD and RELRO alignment; APKs use extracted, compressed libraries.
Actual execution on a 16 KiB device still needs device validation.

The Windows launcher uses `$XDG_DATA_HOME/egcl/wine` (default
`~/.local/share/egcl/wine`) unless `WINEPREFIX` is explicitly supplied. It does
not use the desktop's default Wine prefix. Target payloads live under
`/usr/libexec/egcl`; `EGCL_CROSS_ROOT` overrides that directory for testing.

## Runtime libraries and validation limits

The Linux target packages contain private copies of Fedora's glibc runtime
and target libgcc, with license notices. They do not expose foreign ELF
capabilities to the host package manager. Rebuild these packages when their
bundled libraries receive updates. `/usr/share/doc/egcl/build.json` records the
source commit, Rust/NDK versions, input RPM filenames, and dumped runtime hashes.
`/usr/libexec/egcl/android/runtime.json` records the app runtime API/version,
NDK, Android ABI mapping and hashes of the two shared libraries.

The packaging checks establish emulated execution and image round trips, not
native Windows or Android device compatibility for every application. Existing
port limitations still apply; see [Linux and Android ports](cross-compilation.md)
and [Windows](windows.md).

Run the launcher regression tests with:

```sh
python3 packaging/fedora/test-launcher.py
```
