<p align="center">
  <img src="docs/assets/evergreen-banner2x.png" alt="Evergreen Common Lisp" width="1280">
</p>

**Evergreen Common Lisp (EGCL)** is an experimental Common Lisp implementation
written in Rust, with a HotSpot-inspired tiered JIT, a moving generational
garbage collector, and a tree shaker for standalone applications.

> [!WARNING]
> **This is an experiment.** Evergreen is for people interested in trying and
> developing a new Lisp implementation. It does not promise complete ANSI
> conformance, compatibility stability, or production readiness. Programs may
> fail or behave incorrectly, and performance varies widely by workload.
>
> The project also explores whether a language implementation can be built
> through expert guidance of AI-driven development, with minimal manual code
> review. It may never meet its goals. See [Authorship & Governance](#authorship--governance).

[Read the manual](https://atgreen.github.io/evergreen/) ·
[Version history](CHANGELOG.md) ·
[Report a bug](https://github.com/atgreen/evergreen/issues)

## What you can explore

- **Adaptive native compilation.** Supported code runs in bytecode and can
  advance to baseline and optimizing native compilers. Hot loops can enter
  native code during the current call through on-stack replacement. Cold
  functions and unsupported forms can use the tree-walking evaluator.
- **Optimization without required type declarations.** The compiler can
  specialize supported operations using observed values and guarded assumptions.
  Failed guards deoptimize to less specialized execution. Functions remain
  redefinable. See [how Lisp runs](docs/manual/user/explanation/execution.md).
- **Lightweight concurrency.** Fibers share native carrier threads. Reads,
  writes, and readiness waits on established TCP streams can park an unpinned
  fiber while other work runs. Connect, accept, and DNS still block the carrier.
  See [fibers and socket I/O](docs/manual/fibers.md#socket-io).
- **Application shaking.** Save a Lisp environment with libraries preloaded,
  or shake it into an executable. The shaker removes unreachable Lisp code
  and objects; specialized runtime builds can also omit unused builtins and
  compiler tiers. See [saved executables and shaking](docs/manual/user/how-to/save-executable.md).
- **Interoperability and Android.** Call C libraries through the
  [FFI](docs/manual/foreign.md), use [Java through `egcl-jvm`](docs/manual/java.md),
  or enable the Lisp-facing `PY` CPython API in a Python-enabled build.
  [Android tooling](docs/manual/user/how-to/android.md) generates NativeActivity
  applications with Lisp lifecycle and EGL/OpenGL ES code. These capabilities
  require the appropriate runtime build and platform.

EGCL implements macros, CLOS, conditions and restarts, and supports ASDF
systems and selected SBCL-compatible extensions. Existing libraries may need
compatibility changes; the [library fork guide](docs/library-forks.md) records
maintained ports and their validation scenarios. The manual describes current
interfaces and limits; `spec/` also contains requirements for future stages.

## Try it

### Install on Fedora 44 x86-64

The published release repository currently supplies **Fedora 44 x86-64**
packages. Install the runtime with ASDF preloaded:

```sh
sudo dnf config-manager addrepo --from-repofile=https://atgreen.github.io/evergreen/repo/egcl.repo
sudo dnf install egcl
```

On first use, dnf asks to import the signing key. Check its fingerprint:

```text
6101 7475 407E 35EB 2608 BF2B F2EA EAEE 344F 7576
```

Confirm the fingerprint through a channel independent of the package server.
Both package and repository-metadata signature checks are enabled. Packages
are hosted on GitHub Releases; repository metadata is hosted on the manual site.
Use the release repository above: the testing repository definition may exist
without published test-build metadata.

Other runtime targets do not imply native Fedora packages for those hosts.
See [platform support](#platform-support) and the
[source build guide](docs/manual/user/how-to/build.md).

### Run your first Lisp expression

```sh
egcl --no-init --eval '(+ 1 2)'
```

This prints `3` and exits. Start an interactive REPL with `egcl`, or run a
Lisp file with `egcl --load path/to/file.lisp`. Enter `(quit)` to leave the REPL.
Continue with [your first program](docs/manual/user/tutorials/first-program.md)
for a function, a script, and command-line arguments, then
[load an ASDF system](docs/manual/user/how-to/asdf.md).

Use `egcl --help` or the [CLI reference](docs/manual/user/reference/cli.md)
for options. The REPL reads `~/.egclrc` (or `EGCL_INIT_FILE`); `--no-init`
skips it. Batch evaluation and file loading skip the init file automatically.

### Build from source on x86-64 Linux

You need Git, a C compiler/linker, and Rust installed through rustup. The
repository pins Rust **1.94.1** and the `x86_64-unknown-linux-musl` target;
let rustup use that pin. The Cargo manifests declare Rust 1.85 as the minimum,
but that is not a guarantee that any newer compiler builds this checkout.

```sh
git clone https://github.com/atgreen/evergreen.git
cd evergreen
cargo build -p egcl
cargo run -p egcl -- --no-init --eval '(+ 1 2)'
```

This development binary uses static musl and a built-in ELF loader with limited
library compatibility. The Fedora `egcl` package uses glibc and the system
dynamic loader. For an installed standalone command, a dynamic FFI build, or profile-guided
optimization, follow the [build guide](docs/manual/user/how-to/build.md).

On Apple Silicon macOS, select the native target explicitly (the checkout's
default target is Linux):

```sh
nix-shell --run 'cargo build --locked -p egcl --bin egcl --target aarch64-apple-darwin'
target/aarch64-apple-darwin/debug/egcl --no-init --eval '(+ 1 2)'
```

This produces a Mach-O arm64 command. See the [build guide](docs/manual/user/how-to/build.md)
for a release build and validation notes.

## Platform support

Runtime targets include Linux x86-64, AArch64, ppc64le, s390x, and riscv64;
macOS arm64; Windows x86-64; and Android ARM64 and x86-64 application
runtimes. Native compiler coverage and foreign-call support vary by target.
Linux x86-64, AArch64, ppc64le, s390x, and riscv64 have T1 and T2 backends,
OSR, and deoptimization for supported code shapes; unsupported shapes remain
at a lower tier. Linux riscv64 is built natively rather than cross-compiled.
The macOS arm64 CLI is built from
source and has a narrower validation baseline than Linux.

The Fedora x86-64 repository also offers `egcl-static` and `egcl-target-*`
packages. Target tools run foreign-architecture runtimes through QEMU or Wine
to create executables for those targets. They are installed on the x86-64
host, not on the target machine. See the
[platform matrix](docs/manual/user/reference/platforms.md),
[cross-build guide](docs/cross-compilation.md), and
[Windows guide](docs/windows.md) for capabilities and validation boundaries.

## Documentation

The [published manual](https://atgreen.github.io/evergreen/) has a version
selector: `latest` follows the newest release and `dev` follows `main`.

| Start here | Purpose |
| --- | --- |
| [First program](docs/manual/user/tutorials/first-program.md) | Expressions, the REPL, scripts, and arguments |
| [ASDF systems](docs/manual/user/how-to/asdf.md) | Load a local Lisp project |
| [Saved executables](docs/manual/user/how-to/save-executable.md) | Save and shake an application |
| [Implementation reference](docs/manual/index.md) | Runtime behavior and EGCL-specific interfaces |
| [Contributing](docs/manual/contributing/index.md) | Repository structure, validation, and runtime development |
| [Specification](spec/INDEX.md) | Requirements, design, and staged roadmap |

## Development

This is a Rust 2024 Cargo workspace. The runtime, compiler, standard library,
and CLI live under `crates/`; Lisp sources are in `lib/`. See the
[repository map](docs/manual/contributing/reference/repository.md) for details.

Run workspace tests with `cargo test --workspace`. CI configures Linux
workspace tests and Clippy, targeted native Windows checks, scheduled fuzzing,
and sanitizer jobs. A configured job does not establish that all tests pass;
check the [current workflow results](https://github.com/atgreen/evergreen/actions).
Contributor runtime checks use [memory limits and GC stress](docs/manual/contributing/how-to/gc-safety.md).

Fuzzing requires a nightly toolchain:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
cargo +nightly fuzz run fuzz_reader
```

To build or preview the manual, follow
[Writing documentation](docs/manual/meta/documentation-guidelines.md).
When reporting a bug, include `egcl --version`, your OS and architecture,
how you installed or built EGCL, and a small reproducing Lisp program with
expected and actual results.

## Authorship & Governance

Anthony Green is the creator and maintainer of Evergreen Common Lisp.
Core code, test suites, and documentation were almost entirely generated by
AI assistants following high-level human prompts and specifications. Human
work focuses on architecture, direction, and repository orchestration, with
minimal manual code review or line-by-line verification. Agent usage is
recorded in commit history.

The project uses regression tests, differential checks, and GC stress testing
to find defects; those checks do not establish complete language conformance
or correctness. See the [contributor guide](docs/manual/contributing/index.md)
for the validation workflow and the [changelog](CHANGELOG.md) for release limits.

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
