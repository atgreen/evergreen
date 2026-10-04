# Contributing to Evergreen Common Lisp

Thank you for helping with Evergreen Common Lisp (EGCL). EGCL is an experimental
Common Lisp implementation with a tiered JIT and a precise, moving garbage
collector. Small changes can cross the interpreter, compiler, runtime, and
standard-library boundaries, so evidence matters as much as the patch.

## Before starting

- Search the [issues](https://github.com/atgreen/evergreen/issues) and existing
  Beads work to avoid duplicating an investigation.
- Use the private process in [SECURITY.md](SECURITY.md) for suspected
  vulnerabilities.
- For a substantial behavior or design change, open an issue before investing
  heavily in an implementation.
- Read `spec/INDEX.md`, `spec/conventions.md`, and `spec/stages.json`, then the
  specification chapter for the subsystem you will change. Only `MUST`
  requirements assigned to the current stage or an earlier stage are release
  gates.

EGCL uses [Beads](https://github.com/gastownhall/beads) for durable project
tracking. In a configured checkout, run `bd prime` and `bd ready`, then claim or
create a Bead before changing code. Keep its status and evidence current and
sync it when the work is complete. A first-time contributor who does not yet
have Beads configured can start with a GitHub issue; a maintainer will associate
the work with a Bead before it lands.

## Build the project

The workspace uses the Rust toolchain pinned by `rust-toolchain.toml`.

```sh
cargo build -p egcl
```

The repository defaults to a static x86-64 Linux musl target. Platform-specific
instructions are in the [manual](https://atgreen.github.io/evergreen/) and in
`docs/cross-compilation.md`, `docs/windows.md`, and `docs/fedora-rpm.md`.

Read the [runtime contributor notes](docs/manual/contributing/index.md) before
changing the runtime. In particular:

- Library behavior belongs in `egcl-stdlib`; the tree-walking evaluator should
  call it rather than grow a second implementation.
- Test behavior through every affected execution path. A form wrapped in
  `EVAL` exercises the tree walker and does not prove that the bytecode lowerer
  handled the form.
- Any allocating change must follow the
  [GC-safety procedure](docs/manual/contributing/how-to/gc-safety.md). Root Lisp
  values across allocations, release borrows of GC-scanned state before
  allocating, and compare normal output with stress-and-poison output.
- Run EGCL and memory-intensive tests through `scripts/egcl-limited.sh`. Exit
  137 means the memory cap killed the process; exit 124 means timeout; neither
  is a passing result.

## Validate a change

Start with the smallest test that exercises the behavior, then run the directly
affected crate or integration suite. For a broad runtime change, the usual local
gates are:

```sh
EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 \
  scripts/egcl-limited.sh cargo test --workspace
cargo check --workspace
bash scripts/ci-lint.sh
python3 scripts/spec-coverage.py --gate
```

Use `cargo fmt --all -- --check` to inspect Rust formatting. The repository has
known pre-existing formatting drift, so do not include unrelated formatting
changes merely to make that command pass.

For code that can allocate, add a bounded reproducer and run it both normally
and with:

```sh
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 \
  scripts/egcl-limited.sh path/to/egcl --no-init --load reproduce.lisp
```

A clean stress run is useful evidence only when the values under test actually
relocate. The GC-safety guide explains how to establish that the probe is live.
Performance claims need a release build, a controlled comparison, repeated
measurements, and a clear distinction between warmup and steady state.

Do not silence failures, weaken assertions, reduce acceptance thresholds, or
remove a required gate to make a change appear green. If an existing failure
blocks a broad gate, record the exact command and output and compare it with the
same command on the base revision.

## Submit a pull request

Keep commits focused and use imperative commit subjects. In the pull request:

- link the GitHub issue and Bead;
- describe the behavior before and after the change;
- list the exact validation commands and results;
- identify the execution tiers and platforms exercised;
- include GC relocation evidence when the change allocates; and
- update the manual, specification, and changelog when the public behavior or
  release baseline changes.

If an AI assistant authored part of the change, preserve the agent provenance
required by `AGENTS.md`, including its `Co-Authored-By` trailer.

Contributions are distributed under
`GPL-3.0-or-later WITH Classpath-exception-2.0`, the license of this repository.
