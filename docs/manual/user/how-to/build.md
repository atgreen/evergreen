# Build from source

Use this procedure to install EGCL on x86-64 Linux from an existing checkout.
You need Git, Make, a C compiler/linker, and Rust installed through rustup. The
repository's `rust-toolchain.toml` selects the required Rust toolchain and musl
target; use that pin rather than choosing a newer compiler independently.

## Build a development binary

From the repository root:

```sh
cargo build -p egcl
scripts/egcl-limited.sh target/x86_64-unknown-linux-musl/debug/egcl \
  --no-init --eval '(+ 1 2)'
```

The expression prints `3`. The repository defaults to the x86-64 musl target.
The memory-limit wrapper requires a working systemd user session; it is used for
local runtime checks in this project, not installed as part of EGCL.

## Build and install the standalone command

```sh
rustup component add llvm-tools-preview
EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 scripts/egcl-limited.sh make image
sudo make install
```

`make image` performs profile-guided builds, trains the runtime, and creates
`target/egcl` with ASDF preloaded. `make install` copies that executable into
`/usr/local/bin`. Run the build as your ordinary user.

To build without profile-guided optimization, use `make image-no-pgo` instead
of `make image`. See the repository's
[build Makefile](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/Makefile)
for installation prefix overrides.

## Enable dynamic foreign libraries

Choose the glibc target explicitly and enable the FFI feature:

```sh
cargo build --release -p egcl --target x86_64-unknown-linux-gnu \
  --features egcl-rt/c-ffi
```

Run `target/x86_64-unknown-linux-gnu/release/egcl`. The default static musl
binary cannot dynamically load shared libraries. The Fedora native RPM uses a
dynamic runtime; the Android APK runtime is also a separate dynamic build.

## Install locally built Fedora RPMs

The project provides a container-free RPM build procedure in
[the packaging guide](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/docs/fedora-rpm.md).
It produces local packages; these instructions do not assume a public package
repository. Install matching versions of `egcl` and each `egcl-target-*`
package you need. Keep the base and installed target packages together when
upgrading: target packages require the exact base version and release.
