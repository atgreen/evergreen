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
| `crates/bliss-cli` | User-facing CLI, REPL, script loading, image loading, and evaluation driver |
| `spec/` | Technical specification and roadmap |
| `scripts/spec-coverage.py` | Requirement-to-test traceability report |

## Build

```sh
cargo build
```

Build the CLI binary:

```sh
cargo build -p bliss-cli
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
cargo test -p bliss-cli
```

## CLI Usage

Run the REPL:

```sh
cargo run -p bliss-cli
```

Evaluate an expression:

```sh
cargo run -p bliss-cli -- --eval "(+ 1 2)"
```

Load a file:

```sh
cargo run -p bliss-cli -- --load path/to/file.lisp
```

Run a script and pass arguments through to Lisp as `*COMMAND-LINE-ARGS*`:

```sh
cargo run -p bliss-cli -- path/to/script.lisp -- arg1 arg2
```

The CLI currently accepts:

```text
Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]

Options:
  --help               Print this help message and exit
  --version            Print version information and exit
  --eval, -e EXPR      Evaluate EXPR and exit
  --load FILE          Load FILE and exit
  --image FILE         Path to the boot image
  --no-image           Start without loading an image
  --bootstrap          Bootstrap from lib/boot.lisp
  --workers N          Number of worker threads
  --heap-size SIZE     Heap size (e.g. 512M, 1G)
  --sandbox            Enable sandbox mode
  --no-init            Skip loading the init file
```

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
