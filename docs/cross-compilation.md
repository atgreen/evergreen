# Linux cross-compilation

TorCL's CLI can be built on x86-64 for these Linux targets:

| Architecture | Rust target | Executable |
|---|---|---|
| AArch64 | `aarch64-unknown-linux-gnu` | `target/aarch64-unknown-linux-gnu/release/torcl` |
| POWER little-endian | `powerpc64le-unknown-linux-gnu` | `target/powerpc64le-unknown-linux-gnu/release/torcl` |
| IBM Z big-endian | `s390x-unknown-linux-gnu` | `target/s390x-unknown-linux-gnu/release/torcl` |
| Android AArch64 | `aarch64-linux-android` | `target-android/aarch64-linux-android/debug/torcl` |

These are dynamically linked glibc executables, suitable for Fedora. They run
the interpreter and T0 bytecode engine. Native T1/T2/OSR compilation, foreign
calls/callbacks and fiber context switching are not yet ported; this is not a
claim of full architecture parity. Library loading uses the system dynamic
loader. Saved images have distinct architecture tags; do not move heap images
between architectures.

## Setup and build

Install Rust through rustup, Podman (or Docker), and `cross`:

```sh
cargo install cross --version 0.2.5 --locked
rustup toolchain install 1.94.1 --profile minimal
rustup target add --toolchain 1.94.1 aarch64-unknown-linux-gnu powerpc64le-unknown-linux-gnu s390x-unknown-linux-gnu
scripts/cross-port.sh build all
```

`Cross.toml` selects the cross-toolchain container images. Compilation runs on
x86-64; no emulated compiler or full Fedora guest is needed. The script selects
`torcl-rt/c-ffi` for glibc library loading and explicitly overrides the workspace's
default x86-64 musl target. The default x86-64 build keeps its direct syscalls
and static ELF loader.

Use `aarch64`, `ppc64le`, or `s390x` instead of `all` to build one architecture.
Set `CROSS=/path/to/cross`, `CROSS_CONTAINER_ENGINE=docker`, `CARGO_BUILD_JOBS`,
or `CARGO_TARGET_DIR` as needed. The script defaults to Rust 1.94.1 to match the
tested baseline and prevent `cross` from updating the rolling stable toolchain.
Override `RUSTUP_TOOLCHAIN` to test another compiler. The initial run downloads
container images.

## QEMU validation

Install `qemu-aarch64`, `qemu-ppc64le`, and `qemu-s390x` (Fedora's `qemu-user`
package), and use a session with the user systemd bus available:

```sh
scripts/cross-port.sh test all
```

This builds each CLI, runs the runtime OS ABI tests through `cross`, extracts
matching runtime libraries to a temporary directory, and runs the CLI through
the host's QEMU. It requires Python 3.11 or newer. No binfmt registration, root
privileges, or KVM are required. Tests run with the project's memory/time caps;
containers also receive a memory cap. `TORCL_MEM_MAX` and `TORCL_TIMEOUT` retain
their usual meanings.

The CLI regression compares interpreter, bytecode, default tiering, and forced
T2 (which falls back to bytecode on these targets) output. A focused raw-runtime
program runs with and without GC stress/poison, comparing output byte-for-byte.
It uses `--no-bootstrap` to avoid stressing prelude loading under emulation;
every allocation in that run is stressed, with no allocation-skipping knob.
The tests cover arithmetic (including big integers and
floats), specialized arrays, loops, collections, CLOS, conditions, streams, and
a compiled-file and a heap-image round trip in fresh processes. These tests establish the initial
CLI port, not native performance or full ANSI conformance.

Stack guards, safepoints and JIT mappings use the runtime kernel page size.
QEMU user-mode validation on a 4 KiB host does not replace testing on a native
64 KiB-page POWER or AArch64 kernel.

## Android (AArch64)

Android runs the same AArch64 Linux kernel but a different libc, so it is a
separate target rather than a variant of `aarch64-unknown-linux-gnu`:
`target_os` is `"android"`, bionic spells the errno accessor `__errno` rather
than `__errno_location`, and a saved image carries its own OS tag
(`Os::Android`) so a bionic image cannot load in a glibc or musl TorCL.
`*features*` gets **both** `:LINUX` and `:ANDROID` — kernel facilities like
`/proc`, epoll and signals all hold, while `:ANDROID` is what code needs to
branch on the platform itself.

Note the image tag in `Cross.toml` is the edge tag, not the `0.2.5` the other
targets use: 0.2.5's NDK predates r23, so it ships no `libunwind` for the clang
runtime and the final link fails with `cannot find -lunwind`.

No local NDK is needed — the container has one. A separate `CARGO_TARGET_DIR`
keeps the Android artifacts from thrashing the host target directory:

```sh
export CROSS_CONTAINER_ENGINE=podman CARGO_TARGET_DIR="$PWD/target-android"
cross build --target aarch64-linux-android -p torcl --bin torcl --features torcl-rt/c-ffi
cross test  --target aarch64-linux-android -p torcl-rt --features torcl-rt/c-ffi
cross run   --target aarch64-linux-android -p torcl --bin torcl --features torcl-rt/c-ffi -- \
    --no-init --load scripts/portability-smoke.lisp
```

The container's `/android-runner` wraps QEMU with the NDK sysroot, so `cross run`
and `cross test` execute Android binaries directly; the host's own
`qemu-aarch64 -L …` route used for the glibc targets needs a device linker at
`/system/bin/linker64` and is not the easy path here.

### Dumping the installable executable

`make image` cannot cross-compile: `scripts/build-image.lisp` dumps the world by
**running the target binary**, so the executable has to be produced under
emulation (or on a device). That works:

```sh
CROSS_CONTAINER_OPTS="-e TORCL_IMAGE_OUT=/target/torcl-android-image" \
  cross run --target aarch64-linux-android -p torcl --bin torcl --features torcl-rt/c-ffi -- \
      --no-init --load scripts/build-image.lisp
```

Verify it is self-contained by running it with the source tree ABSENT — the
plain `cargo build` binary reads `lib/boot.lisp` from disk, so mounting the repo
would hide the difference:

```sh
podman run --rm -v "$PWD/target-android:/img:ro" -w /img \
  ghcr.io/cross-rs/aarch64-linux-android:main \
  /android-runner aarch64 /img/torcl-android-image \
  --eval '(cl:format t "~S~%" (asdf:asdf-version))'
```

PGO (`make image`, `scripts/build-pgo-image.sh`) is deliberately not wired up for
Android: training also runs the binary, and a profile gathered under emulation
describes emulated execution rather than the device.

### What has been observed, and what has not

Verified under QEMU: the whole `torcl-rt` suite, `portable_os` 5/5 (kernel page
size, mapped memory and errno, a returning signal handler, and epoll — which was
cfg'd out entirely before the `target_os` fix), `portability-smoke.lisp` end to
end, and a dumped image that starts with ASDF preloaded and no source tree.

NOT established: anything on real hardware (no device or emulator was attached),
16 KiB-page behaviour, the seccomp filter Android applies to app processes, and
running inside an app. That last one is not packaging: Android blocks executing
binaries from app-writable storage, so an in-app TorCL has to become a JNI
library, and bionic's limited static-TLS surplus for `dlopen`'d libraries bears
directly on the execution-context design (spec R4.72/R4.73). See bliss-w2vp.
