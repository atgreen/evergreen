# Agent Instructions

## Git workflow

Since v0.0.1, all repository changes go through focused branches and pull
requests. Do not commit or push directly to `main`.

1. Start from current `github/main`. If the working tree contains unrelated
   changes, preserve them and use a worktree or switch branches only when doing
   so will not disturb them.
2. Claim or create the Bead, then create a descriptively named branch such as
   `fix/<bead>-<topic>`, `feat/<bead>-<topic>`, or `docs/<topic>`.
3. Keep the branch limited to one coherent change. Do not include unrelated
   scratch files, Beads interaction logs, build output, or another session's
   edits.
4. Commit validated increments on the branch with imperative subjects and the
   required `Co-Authored-By` trailer. Push the branch and open a PR whose title,
   description, validation, and Bead references describe the final patch.
5. Before merge, rebase onto current `main`, rerun the affected checks, inspect
   the final diff, and resolve every review conversation.
6. Use GitHub's **squash and merge** operation. Delete the merged branch, update
   local `main` with `git pull --ff-only github main`, then close and sync the
   Bead with the merge commit and validation evidence.

GitHub Actions may write its dedicated `gh-pages` publication branch. Release
tags and release assets follow the release procedure; they do not permit a
direct push to `main`. Until the comprehensive CI bead (`bliss-hn1cc`) is
complete, run and report the relevant checks without making known-unreliable
jobs required merge gates.

## Targeted validation

Keep the edit/test loop focused on the behavior being changed:

- Start with the exact regression and neighboring tests for the affected path.
  Use named test filters or a small integration target; confirm the intended
  tests actually ran. Broaden only when a failure, shared invariant, or wider
  change gives a concrete reason.
- Build each source revision once when practical, then reuse its binaries for
  focused checks. Combine compatible pending edits before an integration build.
  Record which source revision and uncommitted changes produced the binary;
  never validate new edits against an older executable.
- Target GC stress at allocating paths touched by the change. Keep the moving
  GC evidence requirements below: compare results, and prove relocation when
  claiming a rooting fix. Native tests must demonstrate the intended native
  path, not merely set a flag or pass through interpreter fallback.
- Run the required integration and quality gates on the final combined patch
  before landing. Do not repeat whole compiler, runtime, native, or BFASL suites
  after every local edit. After further edits or rebasing, rerun the affected
  checks; repeat broader checks only when their evidence has been invalidated.
- For prose-only documentation changes, inspect the diff and check formatting
  and links; do not rebuild the runtime or run unrelated test suites.
- Preserve logs and distinguish passed, failed, interrupted, and unrun checks.
  If a broad run is no longer useful, stop only this session's verified process
  or process group and replace it with the relevant checks. An interrupted run
  is not a pass. Keep all runtime runs within the memory limits below.

## Parallel work with sub-agents

When the current host/session permits delegation, use sub-agents for independent
implementation slices, focused audits, and review while the main agent advances
another part of the task. Avoid delegating work that immediately blocks on the
main agent's unfinished changes.

- Give each agent a bounded task, Bead, owned files or subsystem, expected
  result, and validation scope. Record ownership and dependencies in Beads.
- Give concurrent writers separate worktrees and branches. Preserve other
  sessions' edits. Read-only reviewers can inspect an existing worktree.
- Coordinate builds centrally. Check available memory, disk, and current load;
  more simultaneous builds can make the work slower. Never let separate
  worktrees build into the same Cargo target directory. Agents can audit,
  prepare patches, or run focused probes against an identified existing binary
  while a build is running.
- Integrate completed patches as they become available, review their interaction,
  and run the required combined validation once. Agent reports are evidence to
  inspect, not substitutes for reviewing the diff and verifying the result.
  Clean up an agent worktree only after its work is safely preserved and landed
  or explicitly abandoned.

## Coverage debt policy

The user approved an explicit known-debt ratchet on 2026-10-06. CI invokes
`scripts/spec-coverage.py --gate --debt-baseline spec/coverage-debt.json`.
The baseline records audited implementation or validation gaps, each with a
Bead and a concrete explanation. It does **not** make those requirements
covered or implemented, permit stage advancement, or excuse failing tests.
Reports must keep actual coverage and accepted debt separate.

New uncovered requirements fail the gate. Baseline entries must name current,
in-scope MUST requirements; malformed, duplicate, unknown, or already-covered
entries fail validation. When a requirement obtains legitimate test coverage,
remove its now-stale debt entry in the same PR. Do not close its implementation
Bead merely because the ratcheted gate passes.

The baseline is shrink-only without a new explicit user instruction: do not
add entries to make a regression pass, change requirement staging to hide a
gap, or manufacture citations. The command without `--debt-baseline` remains
the strict completeness check. Run `python3 scripts/test_spec_coverage.py`
when changing this policy or its checker. Keep new requirement-to-test
references out of source comments; use reviewed, functional tracking data.

## Changelog policy

`CHANGELOG.md` records user-facing changes, not the project's development
process. Add entries for relevant runtime behavior, public APIs, fixes,
performance improvements, and user-facing documentation. Entries after
v0.0.1 must link to the actual pull request.

Do **not** add entries for GitHub Actions (GHA), CI, release automation,
agent instructions, task tracking, or other software-development lifecycle
(SDLC) tooling-only changes. Describe those changes in the PR and Bead instead.
For a mixed change, document only its user-facing effect in the changelog.
Changes to this policy do not themselves need a changelog entry.

## Session Startup

At the start of each session, orient yourself in the spec before doing work.
A `SessionStart` hook (`scripts/spec-orient.sh`) injects a banner with the
current build stage as a reminder — the read itself is still your job:

1. Read `spec/INDEX.md` (chapter/requirement map), `spec/conventions.md`
   (notation, staging rules), and `spec/stages.json` (note `current_stage` —
   only MUST requirements at or below it are gated).
2. Pull individual `spec/NN-*.md` chapters on demand for the subsystem you are
   touching. Do **not** read all ~29 spec files up front.

### Enabling the orientation hook (per-agent, one-time)

`scripts/spec-orient.sh` is committed, but the hook that runs it lives in
`.claude/settings.json`, which is **gitignored** — so each agent/checkout must
wire it up locally. Add a `SessionStart` command hook that runs the script.
For Claude Code, `.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "bd prime --hook-json" },
          { "type": "command", "command": "bash scripts/spec-orient.sh" }
        ]
      }
    ]
  }
}
```

(The `bd prime` entry is the existing Beads hook; keep it and add the
`spec-orient.sh` line beside it.) Other agent runners can invoke
`bash scripts/spec-orient.sh` from their equivalent session-start mechanism, or
just run it manually at the start of a session. The script's stdout is the
orientation banner and is safe to run anytime.

## The `/grind` skill

`.agents/skills/grind/SKILL.md` (checked in) is the project's autonomous work
loop: survey the Beads queue, pick the highest-value next task (correctness +
progress toward the HotSpot-like tiered-JIT goal, without rat-holing), reprioritize
the queue, and execute it end-to-end with the GC-safety and validation discipline
below. Codex and other agents that read `.agents/skills/` pick it up directly.

Claude Code discovers skills from `.claude/skills/`, which is **gitignored**, so
each checkout wires it locally once (like the orientation hook above):

```bash
ln -sfn ../../.agents/skills/grind .claude/skills/grind   # from the repo root
```

Then invoke it with `/grind`. Editing `.agents/skills/grind/SKILL.md` updates the
skill for everyone (the symlink points at the tracked file).

## Architecture Principles

- **The interpreter MUST NOT duplicate functionality that belongs in the
  standard library.** `crates/egcl/src/cli.rs` (the tree-walking evaluator)
  should delegate to `crates/egcl-stdlib` for library behaviour — streams,
  sequences, format, conditions, pathnames, hash tables, etc. — rather than
  reimplementing it inline. Duplicated implementations drift apart and create
  incompatible data representations (e.g. the negative-fixnum stream hack in
  cli.rs vs. the heap-object streams in `egcl-stdlib::streams`). When a
  builtin needs library behaviour, wire it to the stdlib API; if the stdlib
  lacks it, add it there and call it from the interpreter.
- Before adding a builtin to cli.rs, check whether `egcl-stdlib` already
  implements it. Prefer extending stdlib over growing cli.rs.

## Common Lisp library ports

Keep EGCL compatibility changes in GitHub forks under `atgreen`, using
`#+egcl` / `#-egcl` or ASDF `:if-feature :egcl` as appropriate. Test scenarios
must import these through ocicl's `git+URL@SHA` support and commit the pinned
`ocicl.csv`; do not use edited download directories as the durable port source.
See [docs/library-forks.md](docs/library-forks.md) and `~/git/ocicl/README.md`.
The forks are the only copy: there is no `ports/` directory of patch sources.

**Do not submit upstream patches, PRs, or issues yet.** Fork commits and test
integration are authorized; upstream submissions require a new user instruction.

## GC safety (READ THIS before touching allocating code)

egcl has a **moving, precise** minor GC: any allocation (`Arena::alloc_cons`,
`alloc_typed`, `arena_str`, building a list/instance/error, `resolve_sym`
interning, `eval_form`, `apply_function`, macro expansion, …) can fire a minor
GC that **relocates nursery objects** and updates every root the GC can find.
Two invariants must hold, or you get intermittent segfaults / "already borrowed"
panics / (across the `extern "C"` c2i boundary) a process **abort**. These are
recurring, hard-to-spot bugs (bliss-6b2 / asdf-6b2 / h6z / 011) — hold the line.

1. **Root every Rust-local `EgclVal` that must survive an allocation.** A
   `EgclVal` held only in a Rust local, register, or `Vec` across an alloc that
   can GC is invisible to the collector: the object moves and your copy is a
   stale (or poisoned) pointer. Use the intrusive lock-free root macros
   (bliss-a03; docs/design/gc-rooting.md):

   ```rust
   egcl_rt::rooted!(v = eval_form(expr, env)?);   // OWNS v; read/write as *v
   egcl_rt::rooted_ref!(_g = &mut existing);      // roots EXISTING local/Vec/
                                                   // struct in place; keep
                                                   // using `existing` directly
   ```

   `rooted_ref!` also roots whole structs (`Env`, `Lowerer`, `Environment`)
   via their `TraceHostRoots` impls. The legacy `ShadowRootScope`/`StackRoot`/
   `HostRoot` primitives still exist in egcl-rt but new code should not add
   uses — they cost a global lock per operation and are queued for removal.
   Classic smell: `let v = eval_form(..)?; <more eval_form/alloc>; use(v)` with
   `v` unrooted, or pushing into a `Vec` and calling `eval_form` again before
   the `Vec` is rooted. CI runs `scripts/gc-root-lint.sh` (ratcheted baseline
   in tools/gc-root-lint/baseline.txt) to catch new instances of this smell.

2. **Never hold a `RefCell` borrow (or a raw `&mut`) to GC-scanned state across
   an allocation.** The registered root scanners re-enter those cells during a
   GC — `CLOS_STATE` (egcl-stdlib clos.rs), a macro's bytecode-function
   `RefCell` (cli.rs `visit_macro_def_roots`), `EnvFrame`s. If a `borrow_mut`
   is held (e.g. `with_state_mut { … alloc … }`) when GC fires, the scan's
   `borrow_mut` double-borrows and panics — and reached via compiled code across
   the `extern "C"` c2i adapters it **aborts**. Drop the borrow before you
   allocate (collect what you need, release, then alloc), don't call
   allocating/evaluating functions inside a `with_state_mut`/`borrow_mut` closure.

**Prove it before you commit.** Run the affected path under the GC fuzzers —
they turn these latent, load-dependent bugs into deterministic failures:

```bash
# Fire a minor GC on (almost) every allocation — the deterministic reproducer
# for BOTH invariants above:
EGCL_GC_STRESS=1 ./target/.../egcl --no-init --eval '(your form)'
# Fill freed nursery with 0xFA (non-canonical HEAP_OBJECT) so a stale deref
# segfaults immediately instead of silently reading moved data:
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 ./target/.../egcl --no-init --eval '…'
```

An "already borrowed" panic, a segfault, or an abort under stress that passes
without it means you have one of these. A clean run under `EGCL_GC_STRESS=1`
is the cheapest evidence a change that allocates is GC-safe.

**Bisecting a stress crash to one allocation (bliss-1uzt).** When
`EGCL_GC_STRESS=1` crashes but you can't see *which* allocation orphaned the
value, two knobs turn "segfault somewhere" into a bracket. They count every
allocation on the thread (the index is stable across runs of a deterministic
program):

```bash
# Only stress AFTER allocation index N — startup runs fast, and a GC fires on
# every later allocation, so the corrupting one can't be missed. Binary-search N:
# it crashes while N < (bad index) and goes clean once N passes it.
EGCL_GC_STRESS=1 EGCL_GC_STRESS_SKIP=40000 EGCL_GC_POISON=1 ./…/egcl …
# Force ONE collection at exactly index N and print the Rust allocation
# backtrace there. Precise, but sensitive to run-to-run index drift, so use it
# to name the site once SKIP has bracketed the window (independent of
# EGCL_GC_STRESS):
EGCL_GC_STRESS_AT=40120 EGCL_GC_POISON=1 ./…/egcl …
```

Prefer `SKIP` to localize (robust: it stresses the whole suffix) and `AT` to
name the allocation site once the window is tight.

**Booting the image under stress costs minutes before your form runs.** The
bootstrap allocates ~100k objects, and `EGCL_GC_STRESS=1` forces a collection on
each (1.5–2 ms apiece, on every target — bliss-v2b6o). When the bootstrap is not
what you are probing, set `EGCL_GC_STRESS_AFTER_INIT=1` as well: the CLI then
starts the forced collections at the first `--eval`/`--load`/`--script` form.
A numeric `SKIP` cannot express this, because the bootstrap's allocation count
moves with every image change.

**A `SKIP` above the program's allocation count is silently a no-op** — it
disables stressing entirely and reports a *clean run that proves nothing*
(bliss-sqpi: an early bracketing of that bug concluded "the fault is in
startup" purely from this artifact; the real corrupting GC was ~70
allocations from the *end*). Before trusting any clean `SKIP` result, confirm
the probe actually fires — `EGCL_GC_STRESS_AT=N` prints
`[gc-stress] … forcing minor GC at allocation #N` when `N` is in range, so
bisecting `AT` on that line first tells you the total and the usable range.

**A clean `EGCL_GC_POISON=1` run does not mean "no GC bug".** Poison only
catches a *stale pointer being dereferenced*. A value orphaned before it is
stored — e.g. a sub-list left in a Rust temporary while a sibling argument
allocates — makes the program compute a quietly wrong answer with no
segfault at all. Diff the program's *output* against a non-stress run;
don't wait for a crash.

**A CLEAN STRESS RUN CAN MEAN "NOTHING MOVED", NOT "NOTHING IS WRONG" — measured,
bliss-c0diw / bliss-ahnzt.** Stress plus poison catches a stale pointer to an object
the collector MOVED. If the object never moves, there is nothing to catch, and a
probe over it cannot fail however many collections fire.

That is not hypothetical. The rooting was deliberately REMOVED from the
LAMBDA-application site whose bug was once deterministic (bliss-98mu), and probes
still gave identical correct answers — at strides 1, 5, 25, 100 and 500, with poison
on, across ~50,000 forced collections, with the un-rooted arm confirmed to run. The
reason, measured last: **those objects were never relocated at all.**

**So before concluding anything from a clean stress run, check that the value you care
about actually moves.** Six lines, and it answers the only question that matters:

```rust
let unrooted = value;                 // a plain copy the collector cannot see
egcl_rt::rooted_ref!(_r = &mut value);
… the allocating call under test …
assert_eq!(value, unrooted, "it moved");   // or eprintln! the two addresses
```

If the rooted copy differs afterwards, the object moved and the probe is live — an
unrooted copy would now be stale. If it does not differ, the probe proves nothing yet
and needs a shape where relocation really happens (bliss-98mu needed a real library
compile, not a hand-written form).

Two supporting facts, both measured, so they need not be re-derived:

- `EGCL_GC_REGION_LOG=1` reports each minor collection's retained/evacuated nursery
  split. A region containing anything pinned is retained IN PLACE — promoted without
  being relocated or poisoned (bliss-jtc.18) — so nothing in it moves. Worth checking,
  though it was NOT the explanation above: that probe logged `retained 0 evacuated 1`
  throughout.
- **A loaded `defun`'s body is the wrong place to probe.** It has survived several
  collections and been promoted out of the nursery, so those conses no longer move.
  Building the form at runtime (`read-from-string` + `eval`) puts it in the nursery,
  but as above that alone is still not sufficient.

`EGCL_GC_STRESS` also parses its value as an integer and silently DISABLES itself on
anything unparseable, so `EGCL_GC_STRESS=true` is a no-op. Use `=1`.

## The debug egcl used to be ~9x slower after `cargo build` (FIXED)

**Fixed in bliss-em8x — kept here because the symptom is memorable and you may
meet it in an old branch, an old bead, or a stale snapshot.**

`Cargo.toml` set `[profile.test] opt-level = 3` (all crates) but
`[profile.dev.package.egcl] opt-level = 2` (**that package only**), so
`cargo build` left `egcl-rt`, `egcl-stdlib` and `egcl-compiler` — the GC, the
sequence library, the lowerer, i.e. most of the hot code — at opt-level 0.
**Both commands write the same path**, `target/<target>/debug/egcl`, so
whichever ran last silently decided the binary's speed. Measured on one commit,
identical source, `(remove-duplicates <3000 short strings> :test #'equalp)`:

```
                                     BEFORE                        AFTER
`cargo build --workspace`   100,817,432 bytes  322 ms    110,726,816 bytes  40 ms
`cargo test --no-run`       112,981,184 bytes   40 ms    112,344,248 bytes  40 ms
```

The dev profile now gives those three crates `opt-level = 2` as well, so both
subcommands produce a binary of the same speed and the order no longer matters.
The cost is that a from-scratch `cargo build --workspace` takes ~75s instead of
a few seconds; incremental builds are close to what they were.

What still holds:

- **Never compare timings of two debug binaries** unless you know the same cargo
  command produced both. The profiles are equal *now*, but snapshotting a binary
  still does not capture how it was built, and this is exactly the assumption
  that produced one confidently-reported, entirely fictitious "regression".
- Use the **release** binary for real perf numbers.
- Do **not** use binary SIZE to judge speed any more. It was a usable proxy
  while the gap existed (~112MB fast, ~100MB slow); the sizes are now close
  (110.7MB vs 112.3MB) and mean nothing about opt-level.

## Benchmarking a tiered loop: the iteration count AND the shape both matter

Two independent gates decide when a loop goes native, and a benchmark that
ignores either one measures warmup rather than steady state:

- `t1_threshold()` — **10 invocations** for T0 → T1
  (`EGCL_T0_T1_THRESHOLD`). A function called *once* does not clear it,
  however long its loop runs.
- `osr_threshold()` — **100,000 back-edges** for OSR inside a running
  activation (`EGCL_OSR_THRESHOLD`). The anonymous path next to it uses 200.

So the same empty `DOTIMES` costs wildly different amounts depending on where
it sits. Measured 2026-10-03 on `target/x86_64-unknown-linux-musl/release/egcl`,
min of 5 runs, with the 205 ms process startup subtracted:

```
  iterations   toplevel (dotimes (i N))   inside a function called once
      50,000        0.229 us/iter                1.110 us/iter
     100,000        0.102                        1.031
     500,000        0.082                        0.268
   1,000,000        0.085                        0.172
   4,000,000        0.078                        0.102
  10,000,000        0.077                        0.086
```

Both shapes converge on roughly **0.08 us/iter** — this engine's steady state
for an empty counted loop, consistent with the ~76 ns/iter back-edge GC poll
measured in bliss-qt702, which is what currently sets that floor.

A toplevel loop is there by 500k. The once-called shape is slower for longer
because **OSR is the only thing that rescues it**: one invocation never clears
the 10-call T0 → T1 gate, so the loop runs interpreted until the back-edge
count reaches `osr_threshold()`, and only the iterations after that run
native. That is why its average falls as N grows rather than dropping at a
point — at 10M, 100k interpreted iterations at ~1.03 plus 9.9M native ones at
~0.077 averages the 0.086 measured above.

Confirmed by removing OSR rather than inferring it, same 10M once-called loop:

```
  default                              1.07 s   0.087 us/iter
  EGCL_OSR_THRESHOLD=4000000000       10.40 s   1.019 us/iter   (OSR never fires)
  EGCL_OSR_THRESHOLD=1000              0.97 s   0.076 us/iter
  EGCL_T0_T1_THRESHOLD=1               0.95 s   0.074 us/iter
```

With OSR off, the loop costs ~1.02 us/iter however long it runs: that is the
un-promoted cost. Lowering the invocation gate instead is worth only ~1.13x
here, so for this shape the back-edge threshold is the lever, not the
invocation one.

Use **1M+ iterations** for a toplevel loop, more for a cold function, and say
which shape and which count a number came from. Around 100k the numbers are
also **bimodal** — note that 100k above took less wall time in total than 50k
did — so a single 100k sample reads as noise or as a regression and is
neither.

**Do not expect the ~150x this section used to claim.** That came with a
0.012 us/iter steady-state figure which nothing reproduces; the real spread
between a cold 50k loop and steady state is about 3x for a toplevel loop and
about 13x for a once-called one. If you see 0.012 us/iter quoted anywhere, it
is stale.

**`DISASSEMBLE` does not tell you whether a loop promoted.** It reports the
*function's* installed tier, so a loop-hot function still prints `T0` long
after OSR has compiled and entered its loop natively. To check promotion,
scale the iteration count and compare us/iter, or use
`EGCL_T2_LOG=stderr` with a low T2 threshold to see per-function promotion and
OSR coverage directly. `DISASSEMBLE` is accurate for invocation-hot functions.

## Always cap egcl memory: `scripts/egcl-limited.sh`

Runaway egcl runs (e.g. ASDF recursion-to-OOM bugs like bliss-hlsa) have
driven this machine deep into swap and set off the global OOM killer. Wrap
**every** ad-hoc `egcl` invocation — and memory-hungry `cargo test` runs —
in `scripts/egcl-limited.sh`:

```bash
scripts/egcl-limited.sh target/x86_64-unknown-linux-musl/debug/egcl \
    --no-init --eval '(form)'
EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 scripts/egcl-limited.sh cargo test ...
```

It runs the command in a `systemd-run --user` scope with `MemoryMax`
(default 4G) and `MemorySwapMax=0`, plus a wall-clock `timeout` (default 600s).
Reading the outcome — the cap **cannot** masquerade as a GC bug:

- exit **137** (SIGKILL) + the `[egcl-limited]` note = cgroup OOM kill —
  memory cap hit, NOT corruption. Raise `EGCL_MEM_MAX` if legitimate.
- exit **124** = wall-clock timeout (hang/runaway loop).
- exit **139** (SIGSEGV) / **134** (SIGABRT) / "already borrowed" = a real
  GC/rooting bug. The cap never causes these: the kernel OOM-kills the scope
  outright, so egcl never sees a failed malloc.

## Running rr (reverse debugger) on this machine

This box is an Intel **hybrid** CPU (P-cores 0–5, E-cores 6–13) whose model is
newer than rr 5.9.0 knows about, so `rr record` fails two ways out of the box:
runtime CPU detection FATALs (`Intel CPU type 0x… unknown`), and if you force a
generic microarch the PMU counter self-check FATALs (`Got 0 branch events`)
because the P-core and E-core PMUs differ and the counter check flakes across
cores. There is **no passwordless sudo**, so you cannot relax
`perf_event_paranoid`/`nmi_watchdog` — the fix is purely about pinning + forcing
the right microarch.

**Recipe:** pin to a single P-core (`taskset -c 0`) and force the Meteorlake
microarch on **both** record and replay:

```bash
# Record (pin to core 0, force microarch so detection + counter-check pass)
taskset -c 0 rr record --microarch='Intel Meteorlake' ./target/release/egcl

# Replay in batch/autopilot (runs to program exit, no debugger).
# With no trace path, rr replays the most recently recorded trace.
taskset -c 0 rr replay -A 'Intel Meteorlake' -a

# Replay interactively (drops you into the rr/gdb prompt for reverse-continue etc.)
taskset -c 0 rr replay -A 'Intel Meteorlake'
```

Notes / caveats:

- Pinning to **one** core is required; the hybrid counter check flakes if the
  process migrates between a P-core and an E-core. Core 0 (a P-core) is known
  good. `--microarch`/`-A` is the same flag.
- `Intel Arrowlake` is also accepted by this rr build if Meteorlake ever
  misbehaves.
- Set `_RR_TRACE_DIR=<dir>` to control where traces land (otherwise
  `~/.local/share/rr/`).
- Verified end-to-end 2026-08-21: recorded `egcl` and replayed it to exit
  0 with no FATAL. Originally captured in bead memory `asdf-load-rr-findings`
  (`bd memories rr`), where it was used to reproduce the asdf-load corruption
  under `rr record`.

## Bead Issue Tracking

This project uses bd (beads) for issue tracking. See [bd prime] for
full workflow context.

### Quick Reference
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:1105d646 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.

## Git & Sync Policy

Commit frequently. When a piece of work is validated (tests/gates pass),
commit it with an imperative message and the agent Co-Authored-By trailer —
do not stockpile validated work uncommitted awaiting approval. Close the
bead with the commit hash and run `bd sync` so the next session benefits.
A current, explicit "do not commit" or "do not push" instruction still wins.

## Session Completion

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Commit and sync** - Commit validated work, `bd sync`; if a sync or push
   is blocked (e.g. auth), report the exact command and error
5. **Hand off** - Summarize changes, validation, issue status, and any blocked step
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
