# Change the runtime

Use this workflow for a change in an existing checkout. Read `AGENTS.md` and the
relevant specification chapter before editing.

## Establish the scope

```sh
bd ready
bd prime
```

Claim an existing task or create one before implementation. Record discovered
follow-ups in Beads rather than leaving them only in comments or working notes.
Read `spec/INDEX.md`, `spec/conventions.md`, and `spec/stages.json`; the current
stage is 5. A requirement in a later stage is a design target, not evidence of
current functionality.

Find both the tree-walking and compiled paths for the behavior you are changing.
If an operation belongs in the standard library, implement or extend it in
`torcl-stdlib` and call it from the evaluator. Avoid another implementation of
the same library operation in `cli.rs`.

## Add a regression at the right boundary

Exercise the form directly when testing compiled behavior. Wrapping it in
`eval` can route both halves of a differential test through the tree walker
and leave the lowerer untested.

For allocating code, follow [GC safety](gc-safety.md) and compare normal output
with stress/poison output. Passing without a crash does not prove the values
are correct.

## Validate the change

Run the affected tests and their direct callers. For a broad runtime change,
run the wider workspace gates with the project's memory cap:

```sh
TORCL_MEM_MAX=8G TORCL_TIMEOUT=1200 scripts/torcl-limited.sh cargo test --workspace
cargo check --workspace
cargo fmt --all -- --check
bash scripts/gc-root-lint.sh
python3 scripts/spec-coverage.py --gate
```

The normal lint gate is `cargo clippy --workspace --all-targets -- -D warnings`.
If an existing failure blocks a gate, record the precise failure instead of
calling the tree green. Cross-target changes need checks on the affected target;
a host build cannot validate target-specific `cfg` branches.

Update user-facing documentation when behavior changes. Commit validated work,
close the Bead with the commit and validation evidence, and follow the active
session's push policy. An explicit instruction not to push takes precedence.
