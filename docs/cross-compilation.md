# Linux cross-compilation

TorCL's CLI can be built on x86-64 for these Linux targets:

| Architecture | Rust target | Executable |
|---|---|---|
| AArch64 | `aarch64-unknown-linux-gnu` | `target/aarch64-unknown-linux-gnu/release/torcl` |
| POWER little-endian | `powerpc64le-unknown-linux-gnu` | `target/powerpc64le-unknown-linux-gnu/release/torcl` |
| IBM Z big-endian | `s390x-unknown-linux-gnu` | `target/s390x-unknown-linux-gnu/release/torcl` |

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
