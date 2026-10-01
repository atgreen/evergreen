<p align="center">
  <img src="docs/assets/evergreen-banner2x.png" alt="Evergreen Common Lisp" width="1280">
</p>

> [!WARNING]
> **This is an experiment.**
>
> Evergreen is under active development. The parts that do work may not behave
> the way you expect, or the way the standard says they should. It may never work.
>
> Everything below describes what Evergreen is *trying* to be. Read it as a
> statement of intent, not as a description of something you can depend on.
> Evergreen can be both incredibly fast and embarrassingly slow.  Just know that 
> this is a work in progress.
>
> Evergreen exists to find out
> whether a real language implementation (a tiered JIT, a moving generational
> collector, a standard library) can be built through arms-length expert guidance
> of AI-driven development. 
> See [Authorship & Governance](#authorship--governance).

Common Lisp is an evergreen language: mature, enduring, and remarkably
resistant to obsolescence. **Evergreen Common Lisp (EGCL)** is a new
implementation built to carry it forward.

**Evergreen is Common Lisp with a HotSpot-inspired native runtime**,
built from scratch in Rust. It starts executing in bytecode, compiles
hot code to native instructions, and specializes dynamically typed
programs as they run.

- **Tiered compilation with on-stack replacement.** Execution progresses from
  bytecode through a baseline native compiler to an optimizing compiler. Hot
  loops can enter compiled code during the current invocation, without waiting
  for the function to return. See [how Lisp runs](docs/manual/user/explanation/execution.md).
- **Speculative optimization without required type declarations.** The compiler
  specializes supported operations under guarded assumptions about runtime
  values. When an assumption fails, precise deoptimization resumes less
  specialized execution while preserving program semantics. Live function
  redefinition remains part of the programming model.
- **Lightweight fibers with synchronous socket I/O.** On x86-64, many fibers
  share native carrier threads. Reads, writes, and readiness waits on established
  TCP streams park an unpinned fiber so other work can run, while Lisp code stays
  sequential. Connect, accept, and DNS are not yet cooperative. See
  [fibers and socket I/O](docs/manual/fibers.md#socket-io).
- **Multiple architectures and operating systems.** Targets include x86-64
  Linux, Windows, and Android; AArch64 Linux and Android; and Power (`ppc64le`)
  and IBM Z (`s390x`) Linux. Compiler and FFI coverage varies by target; see
  [cross-compilation and platform details](docs/cross-compilation.md) and
  [Windows support](docs/windows.md).
- **Native interoperability.** Call C libraries through the
  [foreign function interface](docs/manual/foreign.md), and access the JVM through
  `egcl-jvm`. The `PY` package provides Lisp-facing CPython object and calling
  APIs in builds with Python support enabled.
- **Standalone applications and saved images.** Save a running Lisp environment
  with its libraries preloaded, restore it later, or package it as a native
  executable. The default x86-64 Linux build is fully static. See
  [saved images and executables](docs/manual/user/reference/images.md).
- **Tree shaking of Lisp and Rust.** Deliver a saved image with unreachable
  functions, macros, methods, closures, and heap objects removed. Supported
  source functions can be compiled during delivery. Native specialization can
  omit unused builtin implementations, disassembly, and the tree walker when
  reachable code permits. Choose `max-tier = t1` to omit T2, or `max-tier = t0`
  to omit both native compilers. Explicit retention roots and `--dry-run` explain
  what stays and why. See [application delivery](docs/manual/user/how-to/save-executable.md#deliver-an-application-from-a-saved-image).
- **Native Android applications.** Generate and package APKs with Lisp lifecycle,
  touch-input, and EGL/OpenGL ES code using `egcl-android-new`. See
  [building Android applications](docs/manual/user/how-to/android.md).

EGCL combines a precise generational garbage collector with Common Lisp's
macros, CLOS, conditions, and restarts. It targets ANSI Common Lisp, supports
ASDF systems, and provides selected SBCL-compatible extensions. The project is
under active development; the manual describes current interfaces and
limitations, while `spec/` records the design and self-hosting roadmap.

## Manual

The [EGCL manual](docs/manual/index.md) covers running Lisp, ASDF systems,
saved executables, cross-target tools, Android applications, and runtime
contributions. It uses Material for MkDocs with a subject-oriented implementation
reference inspired by the SBCL manual, plus symbol and concept indexes. Preview
it locally:

```sh
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r requirements-docs.txt
make docs-serve
```

Use `make docs` for a strict build. Online publication is not enabled yet.
See [Writing documentation](docs/manual/meta/documentation-guidelines.md).

## Workspace

This is a Cargo workspace using Rust 2024 and requiring Rust 1.85 or newer.

| Path | Purpose |
| --- | --- |
| `crates/egcl-rt` | Runtime core: object model, values, GC, threads, FFI, sandboxing, images |
| `crates/egcl-compiler` | Bootstrap compiler pieces: reader, macro expansion, IR, optimisation, codegen, tiering, OSR, profiling |
| `crates/egcl-stdlib` | Standard-library support: packages, CLOS, conditions, streams, sequences, hash tables, FORMAT, pathnames, devtools |
| `crates/egcl` | User-facing CLI, REPL, script loading, image loading, and evaluation driver |
| `lib/` | Lisp-side prelude (`boot.lisp`) and bundled sources loaded at startup |
| `tests/` | Cross-cutting suites: ANSI conformance, differential, integration, property, and sanitizer configs |
| `fuzz/` | `cargo-fuzz` targets and corpora for the reader, compiler, evaluator, FORMAT, FFI, and image loader |
| `spec/` | Technical specification and roadmap |
| `scripts/spec-coverage.py` | Requirement-to-test traceability report |

## Build

```sh
cargo build
```

Build the CLI binary:

```sh
cargo build -p egcl
```

For native x86-64 Fedora RPMs with optional s390x Linux, AArch64 Linux,
Windows, and Android image-dumping tools, see
[container-free Fedora packaging](docs/fedora-rpm.md).

The Android RPM also provides `egcl-android-new` and shared runtimes for ARM64
phones and x86-64 emulators. Generate an EGL app with
`egcl-android-new hello --host=aarch64-linux-android --template egl`, then run
`make install` and `make run` in `hello`. Use `make HOST=x86_64-linux-android`
to build for an emulator. App builds use the installed runtime and Android SDK;
they do not require rebuilding EGCL.

For Linux AArch64, ppc64le, and s390x CLI cross-builds from x86-64, see
[cross-compilation and QEMU validation](docs/cross-compilation.md). All four
Fedora primary architectures — x86-64, AArch64, ppc64le and s390x — run the
native T1 baseline and T2 optimizing JITs, with on-stack replacement (OSR) for
running loops and precise deoptimization. Native runtime calls and loops support
the moving garbage collector; validation under QEMU includes GC stress and
poisoning, compiled-file loading, and saved-image round trips, and on AArch64
also a differential run on a physical device.

What the non-x86-64 ports do not yet have. Foreign callbacks and fiber context
switching are x86-64 only. s390x reaches foreign code through the bootstrap
dispatcher's fixed set of call shapes, while AArch64 and ppc64le support scalar
foreign calls through AAPCS64 and ELFv2 respectively; aggregate arguments are
unported everywhere but x86-64. The AArch64 and ppc64le T2 emitters cover a
smaller opcode set than the x86-64 one and leave the rest at T1.

For in-process Java calls and Java interfaces implemented by Lisp functions,
see [Java integration in the manual](docs/manual/java.md). The current
implementation uses a native x86-64 glibc build and a local JDK, with explicit
reference ownership and checks for JVM startup, signals, callbacks, and shutdown.
The primary `JAVA` API provides inferred calls, named bindings, Lisp callbacks
and `with-scope` cleanup; the explicit `EGCL-JVM` descriptor API remains available.

For the Windows x86-64 CLI, see [Windows cross-builds and Wine validation](docs/windows.md).
The Android AArch64 CLI is built and validated the same way — same kernel, a
different libc — and is
[documented alongside the Linux ports](docs/cross-compilation.md#android-aarch64),
including how the installable executable is dumped under emulation, since
`make image` runs the target binary and so cannot cross-compile.

Build the standalone `egcl` executable with ASDF preloaded, then install it:

```sh
make image              # trains PGO and produces target/egcl
sudo make install       # installs /usr/local/bin/egcl
```

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
[the load-performance handoff](docs/design/load-performance-handoff.md).

The implementation identifies itself as `EGCL` and provides the `:egcl`
feature. Configuration uses `~/.egclrc` and `EGCL_*` environment variables.
When migrating an existing installation, update initialization files and
library feature conditionals, and rebuild saved images and compiled caches.
The rename does not install compatibility aliases for the previous names.

Run the test suite:

```sh
cargo test
```

Run tests for one crate:

```sh
cargo test -p egcl-rt
cargo test -p egcl-compiler
cargo test -p egcl-stdlib
cargo test -p egcl
```

Fuzz targets live under `fuzz/` and run via `cargo-fuzz`:

```sh
cargo install cargo-fuzz
cargo fuzz list
cargo fuzz run fuzz_reader
```

Continuous integration (`.github/workflows/`) runs the workspace tests and
clippy on Linux and macOS, a nightly fuzzing job, and sanitizer builds.

## CLI Usage

The [EGCL-specific Lisp API manual](docs/egcl-lisp-api.md) documents the
currently callable extensions and the status of the complete planned Lisp API,
including fibers, native threads, synchronization, compiler introspection,
sandboxing, and developer tools.

Run the REPL:

```sh
cargo run -p egcl
```

Evaluate an expression:

```sh
cargo run -p egcl -- --eval "(+ 1 2)"
```

Load a file:

```sh
cargo run -p egcl -- --load path/to/file.lisp
```

Run a script and pass arguments through to Lisp as `*COMMAND-LINE-ARGS*`:

```sh
cargo run -p egcl -- path/to/script.lisp -- arg1 arg2
```

The CLI currently accepts:

```text
Usage: egcl [OPTIONS] [SCRIPT] [-- CL-ARGS...]

Evergreen Common Lisp

Options:
  --help               Print this help message and exit
  --version            Print version information and exit
  --eval, -e EXPR      Evaluate EXPR and exit
  --load FILE          Load FILE and exit
  --image FILE         Path to the boot image
  --no-image           Start without loading an image
  --bootstrap          Deprecated; the prelude now loads by default
  --no-bootstrap       Skip the bootstrap prelude (raw evaluator)
  --workers N          Number of worker threads
  --heap-size SIZE     Heap size (e.g. 512M, 1G)
  --tlab-size SIZE     Per-thread TLAB size
  --nursery-size SIZE  Nursery size
  --stack-size SIZE    CL stack size per green thread
  --gc-log FILE        Write GC logs to FILE
  --jit-dump           Emit jitdump metadata
  --log-level LEVEL    Set log level (error|warn|info|debug|trace)
  --sandbox            Enable sandbox mode
  --no-init            Skip loading the init file

Arguments after -- are passed through to CL as *command-line-args*.
```

The bootstrap prelude (`lib/boot.lisp`) now loads by default; `--bootstrap`
is retained only for compatibility, and `--no-bootstrap` starts the raw
evaluator without it.

When starting the REPL without `--no-init`, EGCL attempts to load the file
specified by `EGCL_INIT_FILE`; if that is unset, it falls back to `~/.egclrc`.

## Specification

Start with:

- `spec/INDEX.md` for the master chapter index
- `spec/00-overview.md` for goals, architecture, and design decisions
- `spec/conventions.md` for requirement notation

Source modules cite spec sections as `§N.M`. Tests are expected to cite
requirement IDs such as `R6.45` when they cover normative behavior.

Generate a human-readable traceability report:

```sh
python3 scripts/spec-coverage.py
```

Use the gate mode when uncovered `MUST` requirements should fail the check:

```sh
python3 scripts/spec-coverage.py --gate
```

## Development Notes

- The implementation is still in bootstrap form. Some spec goals describe the
  intended architecture rather than fully completed behavior.
- Keep runtime and compiler code aligned with the relevant `spec/` sections.
- Prefer adding focused tests under the crate that owns the behavior.
- If a test implements a normative spec requirement, include the requirement ID
  in the test source so `scripts/spec-coverage.py` can find it.

## Authorship & Governance

Anthony Green is the creator and maintainer of Evergreen Common Lisp,
however, this repository was created using an **AI-driven development
model**.

* **Authorship:** Core code, test suites, and documentation were almost entirely generated by AI assistants following high-level human prompts and specification.
* **Human Role:** Conceptual architecture, prompt direction, and repository orchestration (minimal manual code review or line-by-line verification).
* **Provenance & Models:** Model usage is logged per-change in the git commit history.

## License

Evergreen Common Lisp is licensed under the **GNU General Public License,
version 3 or later, with the Classpath Exception** — the same arrangement
OpenJDK uses for the Java class library:

```
GPL-3.0-or-later WITH Classpath-exception-2.0
```

**Your programs are yours.** Evergreen combines its runtime with your code to
run it, and `SAVE-LISP-AND-DIE` emits one executable containing both. The
Classpath Exception is what keeps the GPL from reaching that executable: you may
license and sell an application Evergreen compiles or delivers on whatever terms
you choose. Modifications to Evergreen *itself* stay under the GPL.

See [LICENSE](LICENSE) for the licence and
[LICENSE.classpath-exception](LICENSE.classpath-exception) for the exception,
which also names the one third-party component that keeps its own licence
(`lib/asdf.lisp`).

Version 3 rather than OpenJDK's version 2 because Evergreen links a register
allocator offered only under Apache-2.0, which is compatible with GPLv3 but not
with GPLv2.
