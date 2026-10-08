# Linux cross-compilation

EGCL's CLI can be built on x86-64 for these Linux targets:

| Architecture | Rust target | Executable |
|---|---|---|
| AArch64 | `aarch64-unknown-linux-gnu` | `target/aarch64-unknown-linux-gnu/release/egcl` |
| POWER little-endian | `powerpc64le-unknown-linux-gnu` | `target/powerpc64le-unknown-linux-gnu/release/egcl` |
| IBM Z big-endian | `s390x-unknown-linux-gnu` | `target/s390x-unknown-linux-gnu/release/egcl` |
| Android AArch64 | `aarch64-linux-android` | `target-android/aarch64-linux-android/debug/egcl` |

These are dynamically linked glibc executables, suitable for Fedora. All three
support native T1 compilation and T0-to-T1 OSR, and a T2 backend emitting
optimized guarded fixnum and single-float arithmetic, branches, loops, runtime
calls and multiple-value transfers. Native register and spill roots are
synchronized through GC-scanned activation slots at runtime calls and sampled
loop safepoints. Live T1-to-T2 OSR grows the activation in place and imports its
live locals. Deoptimization reconstructs shared tagged-value recipes from their
live inputs. Functions the emitter does not cover stay at the tier below.

The AArch64 and POWER T2 emitters cover a smaller opcode set than the x86-64
one, so more functions remain at T1 there. Foreign calls: AArch64 uses AAPCS64
and POWER uses ELFv2, both for scalars only; s390x still reaches foreign code
through the bootstrap dispatcher's fixed set of call shapes. Aggregate
arguments, foreign callbacks and fiber context switching are not yet ported to
any of the three, so this is not a claim of full architecture parity.

Two per-architecture notes worth knowing before working on these. Rust's inline
assembly is not stable for powerpc64, so anything needing a hand-written stub
there — the foreign-call trampoline and the SIGSEGV recovery epilogue — is
generated at runtime through the POWER assembler instead. And POWER's sticky
`XER[SO]` cannot carry a per-operation overflow guard, so fixnum arithmetic
computes overflow explicitly rather than branching on a flag.

Library loading uses the system dynamic loader. Saved images have distinct
architecture tags; do not move heap images between architectures.

## RISC-V (native build)

RISC-V is built natively on RV64GC hardware rather than through `cross`; no
cross-toolchain container image is wired up yet. On a Debian 13 riscv64 host
with rustup's `riscv64gc-unknown-linux-gnu` toolchain:

```sh
rustup toolchain install 1.94.1 --profile minimal
cargo build --locked --release -p egcl --bin egcl \
  --target riscv64gc-unknown-linux-gnu --features egcl-rt/c-ffi
scripts/egcl-limited.sh python3 scripts/portability-smoke.py riscv64 -- \
  target/riscv64gc-unknown-linux-gnu/release/egcl
```

The interpreter and bytecode tiers run, saved images carry a distinct
`RISCV64` architecture tag, `*features*` includes `:riscv` and `:riscv64`, and
the portability smoke test passes natively. The T1 baseline compiler emits
RV64I code with guarded fixnum add, subtract, increment, decrement, negation
and comparisons, live T0-to-T1 OSR, and overflow and uncommon-trap
deoptimization; `scripts/riscv64-jit-smoke.py` requires observable T1
promotion and compares native, OSR and GC-stressed output with the
interpreter. The T2 optimizing emitter covers the same opcode set as the
s390x one: guarded fixnum add, subtract, multiply, negate and comparisons,
tagged single-float add, subtract and multiply, `EQ`, unguarded `CAR`/`CDR`,
bitwise operations and constant shifts, runtime calls with GC-synchronized
roots, sampled back-edge polls, live T1-to-T2 OSR and precise deopt exits;
the smoke's T2 sections require actual tier-2 installation and compare guard
exits, calls, OSR and polls with T0 under GC stress. The fiber scheduler
switches stacks natively (ra, s0-s11, fs0-fs11 and fcsr), so cooperative
fibers behave as on the other Linux ports. Foreign calls use the LP64D
calling convention for scalars (integers, pointers, floats, doubles,
variadic calls after the C promotions); aggregate arguments and foreign
callbacks are not yet ported. Null-pointer and stack-guard faults resume at
the runtime's recovery handler as on s390x; a fault inside JIT-compiled code
itself is not yet recoverable. The remaining slices are tracked as children
of Bead `bliss-miro8`.

```sh
scripts/egcl-limited.sh python3 scripts/riscv64-jit-smoke.py -- \
  target/riscv64gc-unknown-linux-gnu/release/egcl
```

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
`egcl-rt/c-ffi` for glibc library loading and explicitly overrides the workspace's
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
containers also receive a memory cap. `EGCL_MEM_MAX` and `EGCL_TIMEOUT` retain
their usual meanings.

The CLI regression compares interpreter, bytecode, default tiering, and forced
T2 (with T1 fallback for unsupported s390x functions and bytecode fallback on the other targets)
output. A focused raw-runtime
program runs with and without GC stress/poison, comparing output byte-for-byte.
It uses `--no-bootstrap` to avoid stressing prelude loading under emulation;
every allocation in that run is stressed, with no allocation-skipping knob.
The tests cover arithmetic (including big integers and
floats), specialized arrays, loops, collections, CLOS, conditions, streams, and
a compiled-file and a heap-image round trip in fresh processes. These tests establish the initial
CLI port, not native performance or full ANSI conformance.

The ppc64le runtime also has an ELFv2 native-segment ABI probe. It is run
explicitly with:

```sh
cross test --target powerpc64le-unknown-linux-gnu -p egcl-rt \
  --test native_segment_ppc64le -- --nocapture
```

That probe validates the machine boundary. With `EGCL_NATIVE_TRANSFER=1`, the
CLI additionally admits allocation-free, scope-free, deopt-free PPC64LE bodies
through the segment entry; calls, speculative guards, protected scopes, loop
polls, and fault-recovery cases still fall back to the checked ABI until their
separate gates pass.

For s390x, `scripts/s390x-jit-smoke.py` additionally requires observable native
T1 promotion and live OSR entry. It compares native and bytecode results for
loops, calls with more than five arguments, allocations, multiple values,
errors, and overflow deoptimization. Every-allocation GC stress with poisoning
must produce identical output. The OSR cases also cover uncommon traps with
active condition handlers. The runtime and CLI unit suites contain s390x
instruction-encoding, ABI execution, native frame and deoptimization tests.
T2 checks require actual tier-2 installation for arithmetic and branches, then
compare overflow/type guard exits against T0 under GC stress. Compiler tests
also execute optimized code with register spills and check precise guard
reconstruction. Optimized-loop checks require live T1-to-T2 OSR, exercise
moving GC in the grown activation, and verify a late overflow resumes without
replaying earlier effects. A call-free T2 loop must respond to SIGTERM before
the runtime's hard shutdown deadline.
The native call checks include wide argument lists, twelve-value returns,
spilled heap roots across allocations, function redefinition, and error/nonlocal
exits that must stop before subsequent side effects.
Validation currently uses QEMU; native IBM Z hardware performance is unmeasured.

s390x perf jitdump files identify their code as `EM_S390` and encode fields in
big-endian native byte order. `DISASSEMBLE` and the tier viewer show labeled raw
bytes with native offsets; System Z mnemonic decoding is not yet available.
The native smoke test checks the architecture identifier and compares the
listing's complete byte stream with the perf code-load record.

Stack guards, safepoints and JIT mappings use the runtime kernel page size.
QEMU user-mode validation on a 4 KiB host does not replace testing on a native
64 KiB-page POWER or AArch64 kernel.

## Android (AArch64)

Android runs the same AArch64 Linux kernel but a different libc, so it is a
separate target rather than a variant of `aarch64-unknown-linux-gnu`:
`target_os` is `"android"`, bionic spells the errno accessor `__errno` rather
than `__errno_location`, and a saved image carries its own OS tag
(`Os::Android`) so a bionic image cannot load in a glibc or musl EGCL.
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
cross build --target aarch64-linux-android -p egcl --bin egcl --features egcl-rt/c-ffi
cross test  --target aarch64-linux-android -p egcl-rt --features egcl-rt/c-ffi
cross run   --target aarch64-linux-android -p egcl --bin egcl --features egcl-rt/c-ffi -- \
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
CROSS_CONTAINER_OPTS="-e EGCL_IMAGE_OUT=/target/egcl-android-image" \
  cross run --target aarch64-linux-android -p egcl --bin egcl --features egcl-rt/c-ffi -- \
      --no-init --load scripts/build-image.lisp
```

Verify it is self-contained by running it with the source tree ABSENT. The
prelude is not the reason to check: `lib/boot.lisp` is embedded at compile time
(`EMBEDDED_BOOT_LISP`), so the plain binary never needs it on disk. ASDF is the
reason — `bundled_asdf_path()` resolves `lib/asdf.lisp` through
`env!("CARGO_MANIFEST_DIR")`, a *build-time absolute path*, so a plain
cross-compiled binary can only `(require :asdf)` on a machine where that build
directory still exists (bliss-bp4q). The dumped image has ASDF inside it and does
not care:

```sh
podman run --rm -v "$PWD/target-android:/img:ro" -w /img \
  ghcr.io/cross-rs/aarch64-linux-android:main \
  /android-runner aarch64 /img/egcl-android-image \
  --eval '(cl:format t "~S~%" (asdf:asdf-version))'
```

PGO (`make image`, `scripts/build-pgo-image.sh`) is deliberately not wired up for
Android: training also runs the binary, and a profile gathered under emulation
describes emulated execution rather than the device.

### What has been observed, and what has not

Verified under QEMU: the whole `egcl-rt` suite, `portable_os` 5/5 (kernel page
size, mapped memory and errno, a returning signal handler, and epoll — which was
cfg'd out entirely before the `target_os` fix), `portability-smoke.lisp` end to
end, and a dumped image that starts with ASDF preloaded and no source tree.

Those CLI checks do not establish 16 KiB-page behaviour or every Android app
sandbox interaction. In-process ARM64 application execution is now verified on
a physical Pixel; see the NativeActivity workflow below.

### Driving a GUI: EGCL as a NativeActivity

`crates/egcl-android` embeds EGCL in an Android NativeActivity shared library.
The reusable host loads `android.lisp` and `app.lisp` from the APK's indexed
assets into one Lisp worker. EGL and GLES calls stay in Lisp, using the dynamic
FFI. Both ARM64 and x86-64 libraries are packaged by the Fedora RPM.

```sh
# After installing egcl-target-android and Android SDK/JDK tools:
egcl-android-new hello --host=aarch64-linux-android --template egl
cd hello
make install
make run
# Build for an x86-64 emulator:
make HOST=x86_64-linux-android
```

See [Fedora packaging](fedora-rpm.md#android-application-projects) for prerequisites,
SDK paths, universal APKs and signing. To build the libraries from source with
a local NDK, without containers:

```sh
python3 packaging/android/build-runtime.py --ndk /path/to/android-ndk-r27d \
    --stage target/android-stage
```

The builder explicitly requests `cargo rustc --crate-type cdylib` for each Android
target. The manifest remains `rlib` so the default static-musl workspace build
continues to work. The resulting APK needs no Java source, dex or Gradle.

The Activity host exposes window lifetime, pause state and primary-pointer touch
events. Lisp's `android-main` checks `android:running-p`, cleans up its EGL context
and returns when the surface is going away. Android's destruction callback waits
for that release before freeing the native window; the same interpreter handles
the next surface. `packaging/android/templates/egl.lisp` owns EGL setup/cleanup,
and `egl-app.lisp` demonstrates rendering and touch input.

Validation includes ARM64 rendering and touch on a Pixel, repeated surface
recreation and Activity relaunch, both ELF architectures and 16 KiB segment
alignment, and ARM64/x86-64/universal APK packaging. The installed emulator
37.1.11 currently crashes before boot on the Fedora host (bliss-d97ij), so the new
x86-64 runtime has not been execution-tested there. Actual 16 KiB-page device
execution and saved-image APK payloads remain unverified/unsupported respectively.
