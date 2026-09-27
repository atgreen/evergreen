# Linux cross-compilation

TorCL's CLI can be built on x86-64 for these Linux targets:

| Architecture | Rust target | Executable |
|---|---|---|
| AArch64 | `aarch64-unknown-linux-gnu` | `target/aarch64-unknown-linux-gnu/release/torcl` |
| POWER little-endian | `powerpc64le-unknown-linux-gnu` | `target/powerpc64le-unknown-linux-gnu/release/torcl` |
| IBM Z big-endian | `s390x-unknown-linux-gnu` | `target/s390x-unknown-linux-gnu/release/torcl` |
| Android AArch64 | `aarch64-linux-android` | `target-android/aarch64-linux-android/debug/torcl` |

These are dynamically linked glibc executables, suitable for Fedora. They run
the interpreter and T0 bytecode engine. s390x also supports native T1 compilation
and T0-to-T1 OSR, including guarded fixnum arithmetic and precise deoptimization.
The s390x T2 backend emits optimized guarded fixnum and single-float arithmetic,
branches, loops, runtime calls and multiple-value transfers. Native register and spill
roots are synchronized through GC-scanned activation slots at runtime calls
and sampled loop safepoints. Live T1-to-T2 OSR grows the activation in place
and imports its live locals. Deoptimization reconstructs shared tagged-value
recipes from their live inputs. Unsupported functions stay at T1. AArch64 and
POWER still use T0 for native-tier requests. Foreign calls/callbacks and fiber context
switching are not yet ported; this is not a claim of full architecture parity.
Library loading uses the system dynamic
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
T2 (with T1 fallback for unsupported s390x functions and bytecode fallback on the other targets)
output. A focused raw-runtime
program runs with and without GC stress/poison, comparing output byte-for-byte.
It uses `--no-bootstrap` to avoid stressing prelude loading under emulation;
every allocation in that run is stressed, with no allocation-skipping knob.
The tests cover arithmetic (including big integers and
floats), specialized arrays, loops, collections, CLOS, conditions, streams, and
a compiled-file and a heap-image round trip in fresh processes. These tests establish the initial
CLI port, not native performance or full ANSI conformance.

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

### Driving a GUI: TorCL as a NativeActivity

The CLI shape above cannot draw. Android hands a drawable surface only to code
running inside the app's own process, so a spawned `torcl` binary — however it is
packaged — can never obtain one. `crates/torcl-android` is the other shape: the
runtime linked INTO the activity as a shared library.

```sh
export CROSS_CONTAINER_ENGINE=podman CARGO_TARGET_DIR="$PWD/target-android"
cross rustc --release -p torcl-android --target x86_64-linux-android \
      --crate-type cdylib
```

Note `cargo rustc --crate-type cdylib` rather than declaring it in the manifest.
The crate says `crate-type = ["rlib"]` because the workspace's default target is
static musl, which cannot produce a cdylib at all — declaring one breaks
`cargo build --workspace` for everybody. Making the crate standalone instead puts
its path dependencies outside the container mount, which breaks the cross build.
Asking for the crate type on the command line avoids both.

The APK needs no Java and no dex (`android:hasCode="false"`); the activity IS the
Lisp runtime:

```xml
<activity android:name="android.app.NativeActivity" android:exported="true">
    <meta-data android:name="android.app.lib_name" android:value="torcl_android" />
</activity>
```

The Rust half is deliberately thin and contains no EGL: it exports
`ANativeActivity_onCreate`, captures the `ANativeWindow*` when the surface
arrives, and starts the interpreter on a thread with that address in the form.
Everything else — `eglGetDisplay`, `eglChooseConfig`,
`ANativeWindow_setBuffersGeometry`, `eglCreateWindowSurface`, `eglCreateContext`,
`eglMakeCurrent`, `glClearColor`, `eglSwapBuffers` — is Lisp calling through
`torcl-ffi:foreign-call` (see `crates/torcl-android/src/egl.lisp`).

Surface lifetime is the one piece Lisp cannot decide for itself. Rust keeps an
`AtomicI32` that `onNativeWindowDestroyed` clears and Lisp reads with `mem-ref`
each frame; drawing into a destroyed surface crashes, and Android destroys it on
rotate, backgrounding and exit.

Verified on the emulator: the activity loads, EGL initializes, and successive
screenshots show the clear colour advancing — red rising, green falling, blue
pinned at the hardcoded 0.35 (89/255) — with the runtime's own thread alive in
the app's process and no child process anywhere.

What this does NOT yet cover: input (native_app_glue polls via `ALooper`, so
callbacks — which are JIT-generated and x86-64-only — are not needed), loading a
dumped image rather than an embedded source string, and arm64, where there is no
native JIT.
