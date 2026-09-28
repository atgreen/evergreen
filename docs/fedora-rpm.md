# Fedora RPMs and cross image-dumping tools

This experimental packaging path builds native x86-64 Fedora TorCL and five
optional target packages. Both building and using them are container-free.
It produces local binary RPMs; it is not yet a Fedora-reviewed source RPM.
The initial build baseline is Fedora 44 and Rust 1.94.1.

| Package | Command | Application target | Host runner |
| --- | --- | --- | --- |
| `torcl` | `torcl` | Fedora x86-64, glibc | Native |
| `torcl-target-s390x-linux` | `torcl-s390x-linux` | Fedora s390x | QEMU |
| `torcl-target-aarch64-linux` | `torcl-aarch64-linux` | Fedora AArch64 | QEMU |
| `torcl-target-ppc64le-linux` | `torcl-ppc64le-linux` | Fedora ppc64le (little-endian POWER) | QEMU |
| `torcl-target-windows` | `torcl-windows` | Windows x86-64 | Wine |
| `torcl-target-android` | `torcl-android`, `torcl-android-new` | ARM64 CLI; ARM64 and x86-64 APKs, API 28+ | QEMU for CLI; device/emulator for APK |

Each command-line runtime has ASDF preloaded. Target tools accept the normal TorCL arguments;
they execute the target runtime on the Fedora host, where it can load an
application and dump an executable for its own architecture. They do not
translate an existing x86-64 heap image into another platform's image.

## Build

Install host prerequisites (Rust through rustup):

```sh
sudo dnf install gcc binutils rpm-build rpm cpio python3 curl unzip \
    qemu-user wine mingw64-gcc mingw64-binutils glibc make java-devel \
    python3-mkdocs python3-mkdocs-material
rustup toolchain install 1.94.1 --profile minimal
rustup target add --toolchain 1.94.1 x86_64-unknown-linux-gnu s390x-unknown-linux-gnu \
    aarch64-unknown-linux-gnu powerpc64le-unknown-linux-gnu x86_64-pc-windows-gnu aarch64-linux-android x86_64-linux-android
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

The build uses ordinary `cargo`, native MinGW, Fedora cross-GCC and the NDK.
The native RPM includes the `JAVA` and `TORCL-JVM` APIs as the ASDF system
`torcl-jvm`, and `/usr/lib64/torcl/libtorcl_jvm.so` with its Java helper classes
embedded. `%build` compiles this bridge from source using a JDK (17+) and builds
the HTML manual with MkDocs. Installed users need only a Java runtime; starting
the JVM never invokes a compiler. Load it from any directory:

```lisp
(asdf:load-system :torcl-jvm)
(defparameter *jvm* (java:start-jvm))
(java:static "java.lang.Integer" "parseInt" "42")
(java:stop-jvm *jvm*)
```

The manual is RPM documentation at `/usr/share/doc/torcl/manual/index.html`.
It includes local assets and explicit HTML links for browsing without a server.
Java integration is for the native glibc binary, not the cross-target runtimes.

The Android app libraries are compiled by the spec's `%build`, from a source
archive with locked, vendored Cargo dependencies (`--offline`). The local builder
sets `torcl_rustup=1` because Rust is installed through rustup; direct rpmbuild
otherwise requires Fedora's `cargo` package. Pass the absolute NDK directory as
`--define 'android_ndk /path/to/android-ndk-r27d'`. Both Android Rust targets must
already be installed. This does not yet convert the other prebuilt CLI payloads
into a Fedora-reviewed source RPM.
It strips debug sections before dumping ASDF images. RPM stripping and debuginfo
extraction are disabled because modifying an executable after dumping can
remove its appended image. Cross ELF files are excluded from host dependency
and provides generation. The native binary retains automatic ELF dependencies.

Builds and image probes run through `scripts/torcl-limited.sh`; a working user
systemd session is required. `CARGO_BUILD_JOBS`, `TORCL_MEM_MAX`, and
`TORCL_TIMEOUT` control resource limits. The builder does not use PGO.

RPMs appear in `target/fedora-rpm/RPMS/x86_64/`. The builder checks each staged
runtime, packages it, extracts the RPMs, compares payload bytes, then repeats
architecture/ASDF/evaluation/application-dump/restart and GC stress checks
in an isolated working directory without application sources.
`--package-only` reuses the staged CLI images, rebuilds both Android app libraries
from current source in `%build`, and verifies the extracted RPMs. Use it only
when deliberately reusing that stage's CLI binaries.

## Install and use

Install matching releases of the native package and whichever target packages
you need. After building release 5, for example:

```sh
sudo dnf install target/fedora-rpm/RPMS/x86_64/torcl-0.1.0-5.fc44.x86_64.rpm \
    target/fedora-rpm/RPMS/x86_64/torcl-target-android-0.1.0-5.fc44.x86_64.rpm
```

For example, put this in `build.lisp` after your application's loading code:

```lisp
(defun main () (format t "Hello from TorCL!~%"))
(save-lisp-and-die "hello" :executable t :toplevel #'main)
```

Then run `torcl-s390x-linux --no-init --load build.lisp` to produce an s390x
executable. Use `torcl-aarch64-linux`, `torcl-windows`, or `torcl-android` for
the other targets. Give Windows outputs an `.exe` suffix. Relative filenames
and forward slashes work conveniently under Wine.

The output includes TorCL and the saved application. QEMU/Wine are needed only
on the build host, not on the target. Linux applications still require compatible
glibc and libgcc on the deployment system: a Fedora 44 build is not a promise of
compatibility with older RHEL, Ubuntu, or SUSE releases. Android uses static
bionic and does not need the builder's sysroot; it is a CLI executable, not an
APK, and cannot dynamically load Android shared libraries. Device execution
must follow Android's executable-file and application sandbox rules.

## Android application projects

The same Android RPM includes reusable `libtorcl_android.so` libraries for both
`aarch64-linux-android` (`arm64-v8a`) and `x86_64-linux-android` (`x86_64`), plus
`torcl-android-new`. These libraries support Android's dynamic FFI and load the
application's Lisp from APK assets. App builds require the Android SDK, a JDK,
Python 3 and Make; no TorCL source checkout, Rust, NDK or containers are needed.

```sh
torcl-android-new hello --package org.example.hello --template egl \
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

The Windows launcher uses `$XDG_DATA_HOME/torcl/wine` (default
`~/.local/share/torcl/wine`) unless `WINEPREFIX` is explicitly supplied. It does
not use the desktop's default Wine prefix. Target payloads live under
`/usr/libexec/torcl`; `TORCL_CROSS_ROOT` overrides that directory for testing.

## Runtime libraries and validation limits

The Linux target packages contain private copies of Fedora's glibc runtime
and target libgcc, with license notices. They do not expose foreign ELF
capabilities to the host package manager. Rebuild these packages when their
bundled libraries receive updates. `/usr/share/doc/torcl/build.json` records the
source commit, Rust/NDK versions, input RPM filenames, and dumped runtime hashes.
`/usr/libexec/torcl/android/runtime.json` records the app runtime API/version,
NDK, Android ABI mapping and hashes of the two shared libraries.

The packaging checks establish emulated execution and image round trips, not
native Windows or Android device compatibility for every application. Existing
port limitations still apply; see [Linux and Android ports](cross-compilation.md)
and [Windows](windows.md).

Run the launcher regression tests with:

```sh
python3 packaging/fedora/test-launcher.py
```
