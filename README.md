# Bliss

Bliss is a from-scratch Common Lisp implementation written in Rust. The
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
| `crates/bliss-rt` | Runtime core: object model, values, GC, threads, FFI, sandboxing, images |
| `crates/bliss-compiler` | Bootstrap compiler pieces: reader, macro expansion, IR, optimisation, codegen, tiering, OSR, profiling |
| `crates/bliss-stdlib` | Standard-library support: packages, CLOS, conditions, streams, sequences, hash tables, FORMAT, pathnames, devtools |
| `crates/bliss` | User-facing CLI, REPL, script loading, image loading, and evaluation driver |
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
cargo build -p bliss
```

Run the test suite:

```sh
cargo test
```

Run tests for one crate:

```sh
cargo test -p bliss-rt
cargo test -p bliss-compiler
cargo test -p bliss-stdlib
cargo test -p bliss
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

The [Bliss-specific Lisp API manual](docs/bliss-lisp-api.md) documents the
currently callable extensions and the status of the complete planned Lisp API,
including fibers, native threads, synchronization, compiler introspection,
sandboxing, and developer tools.

Run the REPL:

```sh
cargo run -p bliss
```

Evaluate an expression:

```sh
cargo run -p bliss -- --eval "(+ 1 2)"
```

Load a file:

```sh
cargo run -p bliss -- --load path/to/file.lisp
```

Run a script and pass arguments through to Lisp as `*COMMAND-LINE-ARGS*`:

```sh
cargo run -p bliss -- path/to/script.lisp -- arg1 arg2
```

The CLI currently accepts:

```text
Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]

Bliss Common Lisp

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

When starting the REPL without `--no-init`, Bliss attempts to load the file
specified by `BLISS_INIT_FILE`; if that is unset, it falls back to `~/.blissrc`.

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
