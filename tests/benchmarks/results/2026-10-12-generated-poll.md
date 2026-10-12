# Mapped T2 generated poll comparison

On this host, the generated idle guard reduced median summation-loop time from
183.761 to 4.870 ns per iteration (37.735× ratio of medians). This is a whole-loop comparison, including
arithmetic and branches, with **two selected poll sites per iteration**. It is
not an isolated per-poll cost or a rendezvous-latency measurement.

| Revision | Samples | Min | p5 | Median | p95 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before `3e326969` | 45 | 181.951 | 182.073 | 183.761 | 186.234 | 186.535 |
| After `aec3aa68` | 45 | 4.812 | 4.833 | 4.870 | 4.971 | 5.271 |

All timings are ns/iteration. Five separate processes per binary each ran three
untimed warmups and nine timed calls of one million iterations. Process order
alternated before/after, after/before across trials. Every timed call returned
499999500000. No samples were discarded. Percentiles use linear interpolation;
the JSON retains every sample and each trial's summary.

Both binaries were built from the exact revisions with Rust/Cargo 1.94.1,
`cargo build --locked --release -p egcl --bin egcl`, target
`x86_64-unknown-linux-musl`, default thin LTO and one codegen unit. Builds used
the same physical worktree's private target, sequentially, with two Cargo jobs
and a 5 GiB cap. Each completed binary was copied and hashed before changing
the source checkout. No benchmark-specific compiler change was applied.

The run began 2026-10-12 00:20:59 UTC on an Intel Core Ultra 7 255H, pinned to
CPU 4, with its existing `powersave` governor unchanged. All collaborating
agents paused builds and tests; a host process check found no cargo, rustc or
QEMU process immediately before the run. Desktop/browser processes remained.
One-minute system load snapshots ranged from about 1.48 to 1.53 across the
16-CPU host, so this was a coordinated no-build desktop run, not an isolated
machine or fixed-frequency experiment. The measurements apply to this workload
and configuration.

Every trial logged the named workload's mapped T2 publication and disassembled
its current mapped T2 code before and after measurement. All ten trials have
identical pre/post listings, including instruction offsets and addresses, and
exactly one named mapped publication. The named workload's trace and stderr
contain no deoptimization, invalidation or recompilation messages. This is
trace/listing evidence, not a hardware count of executed guards. The saved first-trial
excerpts show the executed loop shape:

- [Before assembly](2026-10-12-generated-poll-before.asm): backward branch
  `+00aa` returns to `+003b`; unconditional poll veneer calls occur at `+004e`
  and `+0077`.
- [After assembly](2026-10-12-generated-poll-after.asm): backward branch
  `+026c` returns to `+0051`; request checks at `+0051` and `+0152` skip the
  veneer calls at `+0138` and `+0239` when idle. The cold calls remain present.

The production change also removes T1 heat accounting from T2 backedges; this
comparison measures the complete change rather than attributing the difference
solely to one machine instruction sequence. Neither the old 76 ns claim nor a
100 microsecond safepoint bound follows from these results.

[Evidence JSON](2026-10-12-generated-poll.json) contains all 90 samples, hashes,
exact revisions, build metadata, host details and commands. Full stdout,
stderr, T2 logs and the generated workload are retained locally in
`/tmp/bliss-vpbuk-quiet-results`; build logs are
`/tmp/bliss-vpbuk-before-build.log` and `/tmp/bliss-vpbuk-after-build.log`.
The preserved binaries and build/host metadata are in
`/tmp/bliss-vpbuk-binaries`. Use the [runner instructions](../generated-poll.md)
to reproduce the protocol; local temporary artifacts are not repository files.
