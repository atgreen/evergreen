# Fedora RPMs and cross image-dumping tools

This experimental packaging path builds native x86-64 Fedora TorCL and four
optional target packages. Both building and using them are container-free.
It produces local binary RPMs; it is not yet a Fedora-reviewed source RPM.
The initial build baseline is Fedora 44 and Rust 1.94.1.

| Package | Command | Application target | Host runner |
| --- | --- | --- | --- |
| `torcl` | `torcl` | Fedora x86-64, glibc | Native |
| `torcl-target-s390x-linux` | `torcl-s390x-linux` | Fedora s390x | QEMU |
| `torcl-target-aarch64-linux` | `torcl-aarch64-linux` | Fedora AArch64 | QEMU |
| `torcl-target-windows` | `torcl-windows` | Windows x86-64 | Wine |
| `torcl-target-android` | `torcl-android` | Android AArch64, API 28+ | QEMU |

Each runtime has ASDF preloaded. Target tools accept the normal TorCL arguments;
they execute the target runtime on the Fedora host, where it can load an
application and dump an executable for its own architecture. They do not
translate an existing x86-64 heap image into another platform's image.

## Build

Install host prerequisites (Rust through rustup):

```sh
sudo dnf install gcc binutils rpm-build rpm cpio python3 curl unzip \
    qemu-user wine mingw64-gcc mingw64-binutils glibc
rustup toolchain install 1.94.1 --profile minimal
rustup target add --toolchain 1.94.1 x86_64-unknown-linux-gnu s390x-unknown-linux-gnu \
    aarch64-unknown-linux-gnu x86_64-pc-windows-gnu aarch64-linux-android
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
`--package-only` repackages an existing stage and still verifies the extracted
RPMs; use it only when deliberately reusing that stage's binaries.

## Install and use

Install the native package and whichever target packages you need, or all five:

```sh
sudo dnf install target/fedora-rpm/RPMS/x86_64/*.rpm
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

The packaging checks establish emulated execution and image round trips, not
native Windows or Android device compatibility for every application. Existing
port limitations still apply; see [Linux and Android ports](cross-compilation.md)
and [Windows](windows.md).

Run the launcher regression tests with:

```sh
python3 packaging/fedora/test-launcher.py
```
