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

## Build a native macOS arm64 binary

On Apple Silicon, enter the repository's Nix shell and select the Darwin
target explicitly. The checkout defaults to Linux/musl.

```sh
nix-shell --run 'cargo build --locked --release -p egcl --bin egcl --target aarch64-apple-darwin'
target/aarch64-apple-darwin/release/egcl --no-init --eval '(+ 1 2)'
```

The executable is a native Mach-O arm64 file and the expression prints `3`.
The Nix-built executable can link to libraries in `/nix/store` (including
`libiconv`); build it on the Mac where you intend to run it, or keep those
runtime libraries available when copying it.
For a faster development build, omit `--release` and run the binary under
`target/aarch64-apple-darwin/debug/`. The standalone image and install targets
below are Linux build procedures; on macOS, run the Cargo-built CLI directly.

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
[build Makefile](https://github.com/atgreen/evergreen/blob/main/Makefile)
for installation prefix overrides.

## Profile-guided build details

`make image` uses profile-guided optimization (PGO) by default, with dependency-free
synthetic training to optimize the Rust runtime. It requires `llvm-profdata` matching the LLVM
version printed by `rustc -vV` (the Rust `llvm-tools-preview` component is
preferred; alternatively set `LLVM_PROFDATA` to a matching executable):

```sh
rustup component add llvm-tools-preview
EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 scripts/egcl-limited.sh make image
```

This performs two release builds plus training, then saves and restarts the
ASDF image before atomically replacing `target/egcl`. It does not install it.
`make pgo-image` remains an alias. For an ordinary release image without training
or `llvm-profdata`, use `make image-no-pgo`; ordinary Cargo builds are unchanged.
PGO failures are reported, never silently replaced with a non-PGO build. Build logs, private
training caches, and profiles are retained under a fresh `target/pgo/run.*`
directory; preparation profiles are excluded from optimization training.
Profiles are local build artifacts, not distributable inputs to unrelated
source revisions or toolchains. Allow several minutes and extra build storage.

Overrides: `EGCL_PGO_TARGET` (default `x86_64-unknown-linux-musl`, must be
runnable on the build host), `EGCL_PGO_ROOT` (artifact directory),
`EGCL_IMAGE_OUT` (output executable), and `CARGO_BUILD_JOBS` (build parallelism).
Both compiler passes preserve the same `CARGO_ENCODED_RUSTFLAGS` or `RUSTFLAGS`.
Failures leave the previous output executable intact and retain logs; failed
image stages may also leave a `.egcl-pgo.*` directory beside the output.
Run `make test-pgo-build` for the orchestration tests (Python 3, no Rust build).
Performance evidence and outstanding validation are in
[the load-performance handoff](https://github.com/atgreen/evergreen/blob/main/docs/design/load-performance-handoff.md).

## Enable dynamic foreign libraries

Choose the glibc target explicitly and enable the FFI feature:

```sh
cargo build --release -p egcl --target x86_64-unknown-linux-gnu \
  --features egcl-rt/c-ffi
```

Run `target/x86_64-unknown-linux-gnu/release/egcl`. This build uses the system
dynamic loader for foreign libraries. The default static musl binary instead
uses a built-in ELF loader with limited library compatibility; it does not
provide the glibc loader. The Fedora native RPM uses a dynamic runtime; the
Android APK runtime is also a separate dynamic build.

## Install locally built Fedora RPMs

The project provides a container-free RPM build procedure in
[the packaging guide](https://github.com/atgreen/evergreen/blob/main/docs/fedora-rpm.md).
For published Fedora 44 x86-64 packages, use the
[release repository instructions](https://github.com/atgreen/evergreen#install-on-fedora-44-x86-64).
Install matching versions of `egcl` and each `egcl-target-*` package you need.
Keep the base and installed target packages together when upgrading: target
packages require the exact base version and release.
