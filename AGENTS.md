# Agent Instructions

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
  standard library.** `crates/torcl/src/cli.rs` (the tree-walking evaluator)
  should delegate to `crates/torcl-stdlib` for library behaviour — streams,
  sequences, format, conditions, pathnames, hash tables, etc. — rather than
  reimplementing it inline. Duplicated implementations drift apart and create
  incompatible data representations (e.g. the negative-fixnum stream hack in
  cli.rs vs. the heap-object streams in `torcl-stdlib::streams`). When a
  builtin needs library behaviour, wire it to the stdlib API; if the stdlib
  lacks it, add it there and call it from the interpreter.
- Before adding a builtin to cli.rs, check whether `torcl-stdlib` already
  implements it. Prefer extending stdlib over growing cli.rs.

## Common Lisp library ports

Keep TorCL compatibility changes in GitHub forks under `atgreen`, using
`#+torcl` / `#-torcl` or ASDF `:if-feature :torcl` as appropriate. Test scenarios
must import these through ocicl's `git+URL@SHA` support and commit the pinned
`ocicl.csv`; do not use edited download directories as the durable port source.
See [ports/README.md](ports/README.md) and `~/git/ocicl/README.md`.

**Do not submit upstream patches, PRs, or issues yet.** Fork commits and test
integration are authorized; upstream submissions require a new user instruction.

## GC safety (READ THIS before touching allocating code)

torcl has a **moving, precise** minor GC: any allocation (`Arena::alloc_cons`,
`alloc_typed`, `arena_str`, building a list/instance/error, `resolve_sym`
interning, `eval_form`, `apply_function`, macro expansion, …) can fire a minor
GC that **relocates nursery objects** and updates every root the GC can find.
Two invariants must hold, or you get intermittent segfaults / "already borrowed"
panics / (across the `extern "C"` c2i boundary) a process **abort**. These are
recurring, hard-to-spot bugs (bliss-6b2 / asdf-6b2 / h6z / 011) — hold the line.

1. **Root every Rust-local `TorclVal` that must survive an allocation.** A
   `TorclVal` held only in a Rust local, register, or `Vec` across an alloc that
   can GC is invisible to the collector: the object moves and your copy is a
   stale (or poisoned) pointer. Use the intrusive lock-free root macros
   (bliss-a03; docs/design/gc-rooting.md):

   ```rust
   torcl_rt::rooted!(v = eval_form(expr, env)?);   // OWNS v; read/write as *v
   torcl_rt::rooted_ref!(_g = &mut existing);      // roots EXISTING local/Vec/
                                                   // struct in place; keep
                                                   // using `existing` directly
   ```

   `rooted_ref!` also roots whole structs (`Env`, `Lowerer`, `Environment`)
   via their `TraceHostRoots` impls. The legacy `ShadowRootScope`/`StackRoot`/
   `HostRoot` primitives still exist in torcl-rt but new code should not add
   uses — they cost a global lock per operation and are queued for removal.
   Classic smell: `let v = eval_form(..)?; <more eval_form/alloc>; use(v)` with
   `v` unrooted, or pushing into a `Vec` and calling `eval_form` again before
   the `Vec` is rooted. CI runs `scripts/gc-root-lint.sh` (ratcheted baseline
   in tools/gc-root-lint/baseline.txt) to catch new instances of this smell.

2. **Never hold a `RefCell` borrow (or a raw `&mut`) to GC-scanned state across
   an allocation.** The registered root scanners re-enter those cells during a
   GC — `CLOS_STATE` (torcl-stdlib clos.rs), a macro's bytecode-function
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
TORCL_GC_STRESS=1 ./target/.../torcl --no-init --eval '(your form)'
# Fill freed nursery with 0xFA (non-canonical HEAP_OBJECT) so a stale deref
# segfaults immediately instead of silently reading moved data:
TORCL_GC_STRESS=1 TORCL_GC_POISON=1 ./target/.../torcl --no-init --eval '…'
```

An "already borrowed" panic, a segfault, or an abort under stress that passes
without it means you have one of these. A clean run under `TORCL_GC_STRESS=1`
is the cheapest evidence a change that allocates is GC-safe.

**Bisecting a stress crash to one allocation (bliss-1uzt).** When
`TORCL_GC_STRESS=1` crashes but you can't see *which* allocation orphaned the
value, two knobs turn "segfault somewhere" into a bracket. They count every
allocation on the thread (the index is stable across runs of a deterministic
program):

```bash
# Only stress AFTER allocation index N — startup runs fast, and a GC fires on
# every later allocation, so the corrupting one can't be missed. Binary-search N:
# it crashes while N < (bad index) and goes clean once N passes it.
TORCL_GC_STRESS=1 TORCL_GC_STRESS_SKIP=40000 TORCL_GC_POISON=1 ./…/torcl …
# Force ONE collection at exactly index N and print the Rust allocation
# backtrace there. Precise, but sensitive to run-to-run index drift, so use it
# to name the site once SKIP has bracketed the window (independent of
# TORCL_GC_STRESS):
TORCL_GC_STRESS_AT=40120 TORCL_GC_POISON=1 ./…/torcl …
```

Prefer `SKIP` to localize (robust: it stresses the whole suffix) and `AT` to
name the allocation site once the window is tight.

**A `SKIP` above the program's allocation count is silently a no-op** — it
disables stressing entirely and reports a *clean run that proves nothing*
(bliss-sqpi: an early bracketing of that bug concluded "the fault is in
startup" purely from this artifact; the real corrupting GC was ~70
allocations from the *end*). Before trusting any clean `SKIP` result, confirm
the probe actually fires — `TORCL_GC_STRESS_AT=N` prints
`[gc-stress] … forcing minor GC at allocation #N` when `N` is in range, so
bisecting `AT` on that line first tells you the total and the usable range.

**A clean `TORCL_GC_POISON=1` run does not mean "no GC bug".** Poison only
catches a *stale pointer being dereferenced*. A value orphaned before it is
stored — e.g. a sub-list left in a Rust temporary while a sibling argument
allocates — makes the program compute a quietly wrong answer with no
segfault at all. Diff the program's *output* against a non-stress run;
don't wait for a crash.

**A SMALL PROBE UNDER STRESS PROVES LESS THAN IT LOOKS — measured, bliss-c0diw.**
The rooting was deliberately REMOVED from the LAMBDA-application site whose bug
was once deterministic (bliss-98mu), and probes then gave identical CORRECT
answers with and without it, under `TORCL_GC_STRESS=1 TORCL_GC_POISON=1`, both
for a small inline lambda and for eight levels of nested quasiquoted lambda
applications. Collections definitely fired — `TORCL_GC_STRESS_AT=100` and `=5000`
both reported forcing one. So the detector missed a known-real violation.

Two things to know from that:

- **A rooting bug is only visible if its object actually MOVED.** `minor_gc`
  retains a nursery region *in place*, promoting it to old-gen without relocating
  or poisoning it, when anything in it is pinned (gc.rs "Retained pinned regions
  were promoted to old-gen in place and must NOT be zeroed", bliss-jtc.18). An
  unrooted pointer into such a region stays valid, so whether a probe detects the
  bug depends on which region its forms landed in — luck, not coverage.
- **A loaded `defun`'s body is the wrong place to probe.** It has survived several
  collections and been promoted out of the nursery, so those conses no longer
  move at all. Build the form at runtime (`read-from-string` + `eval`) if you
  need it in the nursery — though as above, that still is not sufficient.

So treat a clean stress run as *failing to find* a bug, not as evidence there is
none. Where it matters, add the A/B: remove the root, confirm the probe FAILS,
put it back. A probe that cannot fail is not testing anything — and no "proved it
under GC stress" claim is worth more than a probe that was checked this way.

## The debug torcl used to be ~9x slower after `cargo build` (FIXED)

**Fixed in bliss-em8x — kept here because the symptom is memorable and you may
meet it in an old branch, an old bead, or a stale snapshot.**

`Cargo.toml` set `[profile.test] opt-level = 3` (all crates) but
`[profile.dev.package.torcl] opt-level = 2` (**that package only**), so
`cargo build` left `torcl-rt`, `torcl-stdlib` and `torcl-compiler` — the GC, the
sequence library, the lowerer, i.e. most of the hot code — at opt-level 0.
**Both commands write the same path**, `target/<target>/debug/torcl`, so
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

## Benchmarking a tiered loop: run 1M+ iterations, not 100k

`osr_threshold()` (crates/torcl/src/cli/bytecode.rs) defaults to **100,000
back-edges**, so a 100k-iteration loop sits *exactly on* the OSR promotion
point: roughly half its iterations run at T0 and half natively. Such a
benchmark measures warmup, not steady state, and is **bimodal** — an unchanged
binary will hand you wildly different numbers run to run, which reads as noise
or as a regression and is neither. Scaling one empty `DOTIMES` loop:

```
     50k   2.080 us/iter        100k   0.960 us/iter   <- on the threshold
    500k   0.014 us/iter          4M   0.012 us/iter   <- ~150x, fully promoted
```

Use **1M+ iterations** for anything meant to measure tiered steady state, and
say which regime a number came from. (`TORCL_OSR_THRESHOLD` overrides it.)

**`DISASSEMBLE` does not tell you whether a loop promoted.** It reports the
*function's* installed tier, so a loop-hot function still prints `T0` long
after OSR has compiled and entered its loop natively — a 2M-iteration call
reports T0 while running ~150x faster than T0. To check promotion, scale the
iteration count and compare us/iter; use `DISASSEMBLE` for invocation-hot
functions, where it is accurate.

## Always cap torcl memory: `scripts/torcl-limited.sh`

Runaway torcl runs (e.g. ASDF recursion-to-OOM bugs like bliss-hlsa) have
driven this machine deep into swap and set off the global OOM killer. Wrap
**every** ad-hoc `torcl` invocation — and memory-hungry `cargo test` runs —
in `scripts/torcl-limited.sh`:

```bash
scripts/torcl-limited.sh target/x86_64-unknown-linux-musl/debug/torcl \
    --no-init --eval '(form)'
TORCL_MEM_MAX=8G TORCL_TIMEOUT=1200 scripts/torcl-limited.sh cargo test ...
```

It runs the command in a `systemd-run --user` scope with `MemoryMax`
(default 4G) and `MemorySwapMax=0`, plus a wall-clock `timeout` (default 600s).
Reading the outcome — the cap **cannot** masquerade as a GC bug:

- exit **137** (SIGKILL) + the `[torcl-limited]` note = cgroup OOM kill —
  memory cap hit, NOT corruption. Raise `TORCL_MEM_MAX` if legitimate.
- exit **124** = wall-clock timeout (hang/runaway loop).
- exit **139** (SIGSEGV) / **134** (SIGABRT) / "already borrowed" = a real
  GC/rooting bug. The cap never causes these: the kernel OOM-kills the scope
  outright, so torcl never sees a failed malloc.

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
taskset -c 0 rr record --microarch='Intel Meteorlake' ./target/release/torcl

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
- Verified end-to-end 2026-08-21: recorded `torcl` and replayed it to exit
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
