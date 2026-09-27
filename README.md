# TorCL

TorCL is a from-scratch Common Lisp implementation written in Rust. The
current repository contains the bootstrap runtime, compiler pipeline,
standard-library support, command-line driver, tests, and a detailed technical
specification for the longer-term self-hosting system.

The project targets ANSI Common Lisp with selected SBCL-compatible extensions
where they are widely used and do not conflict with ANSI semantics. The
long-term design is documented in `spec/`, including the object model, runtime,
garbage collector, compiler tiers, standard library, developer tools, security
model, and self-hosting roadmap.

## Workspace

This is a Cargo workspace using Rust 2024 and requiring Rust 1.85 or newer.

| Path | Purpose |
| --- | --- |
| `crates/torcl-rt` | Runtime core: object model, values, GC, threads, FFI, sandboxing, images |
| `crates/torcl-compiler` | Bootstrap compiler pieces: reader, macro expansion, IR, optimisation, codegen, tiering, OSR, profiling |
| `crates/torcl-stdlib` | Standard-library support: packages, CLOS, conditions, streams, sequences, hash tables, FORMAT, pathnames, devtools |
| `crates/torcl` | User-facing CLI, REPL, script loading, image loading, and evaluation driver |
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
cargo build -p torcl
```

For Linux AArch64, ppc64le, and s390x CLI cross-builds from x86-64, see
[cross-compilation and QEMU validation](docs/cross-compilation.md). These initial
ports run the interpreter and bytecode engine; native JIT support is separate.
For the Windows x86-64 CLI, see [Windows cross-builds and Wine validation](docs/windows.md).

Build the standalone `torcl` executable with ASDF preloaded, then install it:

```sh
make image              # trains PGO and produces target/torcl
sudo make install       # installs /usr/local/bin/torcl
```

`make image` uses profile-guided optimization (PGO) by default, with dependency-free
synthetic training to optimize the Rust runtime. It requires `llvm-profdata` matching the LLVM
version printed by `rustc -vV` (the Rust `llvm-tools-preview` component is
preferred; alternatively set `LLVM_PROFDATA` to a matching executable):

```sh
rustup component add llvm-tools-preview
TORCL_MEM_MAX=8G TORCL_TIMEOUT=1200 scripts/torcl-limited.sh make image
```

This performs two release builds plus training, then saves and restarts the
ASDF image before atomically replacing `target/torcl`. It does not install it.
`make pgo-image` remains an alias. For an ordinary release image without training
or `llvm-profdata`, use `make image-no-pgo`; ordinary Cargo builds are unchanged.
PGO failures are reported, never silently replaced with a non-PGO build. Build logs, private
training caches, and profiles are retained under a fresh `target/pgo/run.*`
directory; preparation profiles are excluded from optimization training.
Profiles are local build artifacts, not distributable inputs to unrelated
source revisions or toolchains. Allow several minutes and extra build storage.

Overrides: `TORCL_PGO_TARGET` (default `x86_64-unknown-linux-musl`, must be
runnable on the build host), `TORCL_PGO_ROOT` (artifact directory),
`TORCL_IMAGE_OUT` (output executable), and `CARGO_BUILD_JOBS` (build parallelism).
Both compiler passes preserve the same `CARGO_ENCODED_RUSTFLAGS` or `RUSTFLAGS`.
Failures leave the previous output executable intact and retain logs; failed
image stages may also leave a `.torcl-pgo.*` directory beside the output.
Run `make test-pgo-build` for the orchestration tests (Python 3, no Rust build).
Performance evidence and outstanding validation are in
[the load-performance handoff](docs/design/load-performance-handoff.md).

The implementation identifies itself as `TorCL` and provides the `:torcl`
feature. Configuration uses `~/.torclrc` and `TORCL_*` environment variables.
When migrating an existing installation, update initialization files and
library feature conditionals, and rebuild saved images and compiled caches.
The rename does not install compatibility aliases for the previous names.

Run the test suite:

```sh
cargo test
```

Run tests for one crate:

```sh
cargo test -p torcl-rt
cargo test -p torcl-compiler
cargo test -p torcl-stdlib
cargo test -p torcl
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

The [TorCL-specific Lisp API manual](docs/torcl-lisp-api.md) documents the
currently callable extensions and the status of the complete planned Lisp API,
including fibers, native threads, synchronization, compiler introspection,
sandboxing, and developer tools.

Run the REPL:

```sh
cargo run -p torcl
```

Evaluate an expression:

```sh
cargo run -p torcl -- --eval "(+ 1 2)"
```

Load a file:

```sh
cargo run -p torcl -- --load path/to/file.lisp
```

Run a script and pass arguments through to Lisp as `*COMMAND-LINE-ARGS*`:

```sh
cargo run -p torcl -- path/to/script.lisp -- arg1 arg2
```

The CLI currently accepts:

```text
Usage: torcl [OPTIONS] [SCRIPT] [-- CL-ARGS...]

TorCL Common Lisp

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

When starting the REPL without `--no-init`, TorCL attempts to load the file
specified by `TORCL_INIT_FILE`; if that is unset, it falls back to `~/.torclrc`.

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

## License

The workspace is licensed under `MIT OR Apache-2.0`.
