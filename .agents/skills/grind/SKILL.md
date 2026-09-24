---
name: grind
description: TorCL-local autonomous work loop. Survey the Beads queue, pick the highest-value next task (prioritizing correctness and progress toward the HotSpot-like tiered-JIT Common Lisp goal), reprioritize the queue accordingly, then execute it end-to-end with the project's validation + GC-safety discipline. Trigger when the user says "/grind", "grind", "pick the next thing and do it", "keep making progress", or wants an autonomous session that decides and works without hand-holding.
---

# grind — the TorCL progress loop

You are advancing **TorCL**: a Common Lisp implementation built like the Java
HotSpot VM — a tiered JIT (T0 bytecode → T1 → T2 native) with speculation,
deoptimization, and OSR, over a **moving, precise, generational GC**, converging
on real ANSI semantics and self-hosting (ASDF, real `uiop:getenv`, …).

`/grind` = **decide what to work on next, reprioritize the queue so that
decision is legible, then do the work to completion.** One good increment,
committed and closed, beats a half-finished heroic epic. Prefer correctness and
demonstrable progress toward the goal over breadth.

## 0. Orient (every run)

```bash
bd prime            # workflow + memories (if context is stale)
bd ready            # claimable work
bd stats            # open/blocked/in-progress shape
```

Read `spec/stages.json` → note `current_stage` (the gate you're building toward)
and the next stage's gate. Pull the relevant `spec/NN-*.md` chapter on demand for
the subsystem you touch — do **not** read all of spec/. If you will touch Rust
that allocates, re-read the **GC safety** section of `AGENTS.md` first (§ below).

The real binary is `target/x86_64-unknown-linux-musl/debug/torcl` (musl is the
default target; `target/debug/torcl` usually does **not** exist). Build with
`cargo build -p torcl --bin torcl`.

## 1. Choose the next task — selection rubric

Score candidates in this order and pick the highest that is **tractable now**:

1. **Correctness first.** A bug that produces *wrong results* (silent
   miscompiles, wrong values, GC corruption, aborts) outranks any feature or
   perf work. Wrong answers poison everything built on top.
2. **On the critical path to a gate.** Prefer work that moves the
   `current_stage` gate or the *next* stage's gate (today: S5 hotspot tiering
   correctness, and S6 real-world-selfhost — real ASDF load, `uiop:getenv`).
3. **Serves the HotSpot goal.** Tiered-compile coverage/correctness, deopt/OSR
   fidelity, real GC-managed heap-object layouts (retire side-table/leaked
   representations), inline caches, self-host/ASDF load. Value work that makes
   the *engine* real, not incidental surface.
4. **Tractable and verifiable.** A clear done-signal and a way to *observe* it
   end-to-end. Favor a well-scoped bug or a decomposable slice over an
   open-ended epic.

Deprioritize: cosmetic cleanups, speculative features, anything with no line to
the goal, and **tracking/rollup epics** — don't "work" a rollup; decompose it
into a concrete child and do that.

When the top candidate is a large epic, carve off the smallest child that
delivers real, verifiable value and do *that* this run.

## 2. Anti-rat-hole guardrails

- **Time-box the investigation.** If after a bounded dig the task reveals itself
  as an architecture epic (multi-subsystem, no clear done-signal), **stop**:
  write findings + a decomposition as Beads, pick a smaller adjacent win, and
  proceed. Don't sink the session into a bottomless path.
- **Chase a done-signal, not a rabbit.** Every task needs an observable "it
  works now" (a form that returns the right value, a test that flips, a gate
  that passes). If you can't state it, you're rat-holing.
- **Commit validated increments.** Don't stockpile a giant uncommitted change.
  Land each proven step; the next session/agent benefits.
- **A decision that's genuinely the maintainer's** (a semantics policy, a big
  irreversible direction) — surface it briefly and pick the safe default or ask,
  rather than guessing and building the wrong thing at length.

## 3. GC-safety mandate (non-negotiable on the Rust side)

TorCL has a **moving, precise minor GC**: any allocation can relocate nursery
objects. The two invariants (full detail in `AGENTS.md` "GC safety"):

1. **Root every Rust-local `TorclVal` that must survive an allocation** — use
   `torcl_rt::rooted!` / `rooted_ref!`. A `TorclVal` held only in a Rust local /
   `Vec` across an allocating call is invisible to the collector; it moves and
   your copy goes stale → intermittent segfault / abort across the c2i boundary.
2. **Never hold a `RefCell` borrow (or raw `&mut`) to GC-scanned state across an
   allocation** — drop the borrow, then allocate.

A cheap, safe pattern: collect what you need into Rust-native data with **no
TorCL allocation in the loop**, then allocate (and let `build_*` root its input).
State *why* a change is GC-safe in the commit.

**Prove it before committing** on any allocating path you touched:

```bash
TORCL_GC_STRESS=1 TORCL_GC_POISON=1 \
  target/x86_64-unknown-linux-musl/debug/torcl --no-init --eval '(your form)'
bash scripts/gc-root-lint.sh    # must report "0 not in baseline"
```

A clean `TORCL_GC_STRESS=1` run is the cheapest evidence a change is GC-safe; an
abort/segfault/"already borrowed" under stress that passes without it means you
broke an invariant.

Also: the interpreter (`crates/torcl/src/cli.rs`) **must not duplicate stdlib**
behavior — wire builtins to `crates/torcl-stdlib`; extend stdlib rather than
growing cli.rs (`AGENTS.md` Architecture Principles).

## 4. Reprioritize the queue

Make the decision legible by aligning priorities with the rubric — *before* you
start coding:

- Raise correctness bugs and critical-path/goal work that are currently
  underranked; lower speculative or off-goal items. `bd update <id> --priority N`.
- Leave a one-line `bd comment` on anything you re-rank, saying why (e.g.
  "raise: wrong-result bug on the S6 ASDF path" / "lower: cosmetic, off critical
  path"). Keep churn minimal — reprioritize to *reflect* the plan, not to
  reshuffle the whole board.
- File newly discovered work as Beads immediately (never a `// TODO` or a mental
  note): `bd create "…" -t bug|task -p N`. Model blockers with `bd dep add`.

## 5. Execute end-to-end

```bash
bd update <id> --claim
```

1. Implement the smallest correct change. Match surrounding code idiom.
2. Build the real binary; **drive the affected flow** and observe the result
   (the /verify discipline) — not just tests. Wrong-result bugs demand you *see*
   the right value now.
3. Run the relevant suites (`cargo test -p torcl --test <suite>`, plus
   `-p torcl-stdlib` / `-p torcl-compiler` when touched). Run the lib tests
   single-threaded (`--lib -- --test-threads=1`) — the deliberate-SIGSEGV
   recovery tests (`jtc4_stack_map_tests`) and some `torcl-rt`
   `spec_threading_concurrency` tests flake under parallel signal/safepoint
   contention (pre-existing race, `bliss-lb6.20`); they pass in isolation. Don't
   attribute a flake to your change without re-running the suite single-threaded.
4. GC fuzz + lint any allocating path you touched (§3).
5. **Warning-clean build (non-negotiable, per the hackinator `finishing`
   skill).** Read the build output, not just the exit code: `cargo build
   --workspace` **and** `--release` must emit **zero** warnings before you
   commit. Fix each, or gate it explicitly (`#[cfg(...)]` / `#[allow(...)]`
   **with a comment saying why**) — never ignore. "Pre-existing" is not an
   exemption for a file you're touching; if it's genuinely out of scope, file a
   bead so it stays visible. "Only in the profile I don't build" is not an
   exemption — check both. A build that scrolls warnings is not green.
6. Commit with an imperative subject citing the bead id and a GC-safety note
   when relevant; end the message with:
   `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
7. `bd close <id>` with the commit hash and what was verified; `bd sync`.
   `git push` only when the user asks.

## 6. Loop or hand off

Report: what you picked and **why** (the rubric line it satisfied), the change,
how you verified it (including the GC evidence), what you re-ranked, and any new
Beads filed. Then pick the next task (repeat) until told to stop or the queue has
no tractable correctness/goal work left — at which point say so plainly rather
than inventing busywork.
