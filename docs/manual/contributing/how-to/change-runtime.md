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
`egcl-stdlib` and call it from the evaluator. Avoid another implementation of
the same library operation in `cli.rs`.

## Add a regression at the right boundary

Exercise the form directly when testing compiled behavior. Wrapping it in
`eval` can route both halves of a differential test through the tree walker
and leave the lowerer untested.

For allocating code, follow [GC safety](gc-safety.md) and compare normal output
with stress/poison output. Passing without a crash does not prove the values
are correct.

## Validate the change

Run the changed behavior and its direct callers first. GitHub Actions uses the
same tiered runner locally and on hosted machines:

```sh
EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 scripts/egcl-limited.sh \
  python3 scripts/ci-tests.py build --suite fast \
  --target x86_64-unknown-linux-musl --shards 1 \
  --manifest target/ci-tests/manifest.json
scripts/egcl-limited.sh python3 scripts/ci-tests.py run \
  --manifest target/ci-tests/manifest.json --shard 0 \
  --results target/ci-tests/results-0.json
bash scripts/ci-lint.sh
```

PRs and pushes to main run smoke, fast regression, and relevant additional
checks. Documentation-only changes run the CI contract checks and skip Rust
builds; unknown paths or an unavailable change range select runtime validation.
The stable `ci-required` check verifies the expected jobs, including intentional
skips. It does not turn a failed or canceled test into success. Required branch
protection should use this check only after its hosted reliability is established.

Full musl and GNU/FFI regression runs nightly, on a manual `full` dispatch,
for a merge-group event where supported, and before future release publication.
Each configuration builds once. Eight deterministic shards reuse that exact
artifact and split large harnesses by named tests, using recorded duration
estimates rather than balancing by test count alone. Results and timings are
uploaded even when tests fail. The archived binaries require the same absolute
checkout path as the build because existing tests embed executable paths.

GC stress and cross-platform matrices run nightly or when explicitly requested.
Manual GC runs select either `full` or `subset`, avoiding duplicate execution.
Superseded PR and main validation runs cancel; explicit release/manual runs are
preserved. Merge-group support is present, but GitHub does not currently offer
merge queues for this personal-account repository.

Targeted tests do not replace comprehensive coverage. Existing full-suite
failures remain failures and must be tracked in Beads, not hidden by ignores or
weakened assertions. Cross-target code still needs execution on the affected
platform; a host build cannot validate another platform's `cfg` branches.

Update user-facing documentation when behavior changes. Commit validated work,
close the Bead with the commit and validation evidence, and follow the active
session's push policy. An explicit instruction not to push takes precedence.
