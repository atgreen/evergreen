# Ordinary bridge after restoring the managed leaf entry

The explicit managed-entry capability substantially reduced the direct-loop
regression in the [initial comparison](2026-10-12-ordinary-bridge.md). In this
new matched run, the direct median remained **1.94% above baseline**, while
the forwarding median was **40.20% below baseline**. This is not evidence of
zero direct-call regression; `bliss-zsz0x` remains open pending integration.

All numbers are nanoseconds per complete loop iteration, including runtime
dispatch. Each row contains 45 samples of 100,000 iterations. The paired
increment is the within-pair `forward − direct`, not subtraction of medians.

| Runtime | Measurement | Minimum | Median | p95 | Maximum |
|---|---|---:|---:|---:|---:|
| Baseline | Direct loop | 2243.60 | 2255.56 | 2293.82 | 2298.52 |
| Baseline | Forward loop | 4728.14 | 4753.88 | 4829.92 | 4947.92 |
| Baseline | Paired increment | 2467.31 | 2497.82 | 2535.87 | 2687.46 |
| Fixed | Direct loop | 2283.85 | 2299.37 | 2341.53 | 2344.45 |
| Fixed | Forward loop | 2818.22 | 2842.74 | 2873.59 | 2876.15 |
| Fixed | Paired increment | 511.75 | 536.68 | 563.83 | 574.94 |

The direct leaf now uses its managed adapter; the forwarder uses the ordinary
root adapter. Although the loop dispatch instructions match, the selected
adapters differ. **536.68 ns is not an isolated native-call cost.** The [one-forwarder versus two-forwarder comparison](2026-10-12-ordinary-chain.md)
uses matching outer adapters; it does not replace these direct-regression controls.

Nearest-rank percentiles and observed ranges describe this run, not confidence
intervals. Process-trial medians show the variation that pooled values hide:

| Trial | Baseline direct | Fixed direct | Baseline forward | Fixed forward |
|---|---:|---:|---:|---:|
| 0 | 2255.94 | 2339.85 | 4745.83 | 2861.33 |
| 1 | 2292.58 | 2286.27 | 4822.08 | 2828.31 |
| 2 | 2246.77 | 2294.22 | 4731.90 | 2832.67 |
| 3 | 2255.67 | 2299.37 | 4746.93 | 2825.71 |
| 4 | 2254.84 | 2311.22 | 4768.83 | 2847.01 |

## Method and immutable provenance

The unchanged [runner](../ordinary_bridge.py) alternated binary order over five
trials and direct/forward order over nine pairs per process. All 180 timed calls
returned **4,999,950,000**; no sample was discarded. All five workload file
hashes match the initial comparison. Warmup, code listings, checksum checks and
printing remained outside timed calls.

The window ran **02:03:05–02:04:32 UTC on 2026-10-12**, pinned to CPU 4 of the
same Intel Core Ultra 7 255H host, Linux 7.2.7-200.fc44, existing `powersave`
governor. Cooperating agents paused builds/tests; a host process check found no
compiler or competing test before starting. The desktop remained active, so
this was a coordinated quiet window rather than an isolated machine.

Both artifacts were built on Ubuntu 24.04 with Rust **1.94.1**
(`e408947bfd200af42db322daf0fadfe7e26d3bd1`), musl x86-64, empty features,
release optimization level 3, thin LTO and one codegen unit. No local release
build was used. Archive manifests and actual executable hashes were verified.

| Artifact | Baseline | Fixed |
|---|---|---|
| Workflow | [38190922517](https://github.com/atgreen/evergreen/actions/runs/38190922517) | [38193763195](https://github.com/atgreen/evergreen/actions/runs/38193763195) |
| Artifact ID | 11720748549 | 11722218732 |
| Actual CI merge revision | `33a09236691db374b96916df453ed8bf38a85ea9` | `14b673182d9ee29c2d16c0f4a7d897a77aa417cd` |
| PR head | `5c636edb638f1b45424a34eb893f6378d82b871f` | `4b62981f426fc04d692b642d888724e6622d058e` |
| CLI SHA256 | `0713716cc07ec4f1b7b7323d57c9e34ac4ff8572e987fd55889c4c9d1b152fc7` | `70c019d7ebc44bccb703345cb3703f5e1b7232175716650662e1f9b9d1a1cb03` |

Both workflows passed required CI. The earlier test-only corrected artifact
from workflow 38192004707 was also preserved: its CLI and test binaries other
than `native_callable_image` were byte-identical to the initial measured
artifact. This new comparison uses the later production leaf-entry fix.

## Executed paths and limits

Before sampling, the exact fixed artifact's release test executable passed
`ordinary_mixed_entries_preserve_legacy_leaf_and_register_bridge_paths`: the
installed leaf advertises both entries, the ordinary-only bridge rejects
legacy entry, the warmed legacy leaf uses one nested entry, and a fourth heap
argument survives actual collection with normalized MV state. The same
artifact passed `ordinary_register_abi_installs_and_warms_a_four_argument_call`,
which proves zero cold handoffs for an equivalent warmed ordinary bridge.
These fixture counters do not measure fallbacks in each timed process.

Every timed process retained identical pre/post listings for all four named
functions and had no named deoptimization, recompilation or invalidation.
Normalized hot listings also match across all five trials per binary. The
fixed leaf/bridge and loop prefixes match the initial-after code, confirming
that this change restored runtime entry selection without changing those hot
machine instructions.

The [fixed listing](2026-10-12-ordinary-bridge-fixed-after.asm) retains complete
leaf/bridge code and loop prefixes. Ordinary entries remain leaf `+0043` and
bridge `+0040`; guarded direct `CALL r11` remains `+034a`, cold call `+03ef`,
and outer loop dispatch `+03af`. The bridge marshals three register arguments,
one overflow argument, count/closure and R13 context; its return cleanup
preserves CF and the meaningful result registers. Later loop deopt/OSR stubs
are explicitly omitted from the excerpt.

[JSON evidence](2026-10-12-ordinary-bridge-fixed.json) retains all samples,
host/load data, commands, workload and raw-output hashes, manifests, archive
provenance and acceptance-test output. Local complete raw output is under
`/tmp/bliss-t8a48-fixed-quiet`; archive/build evidence is under
`/tmp/bliss-t8a48-fixed-ci`. Keep those archives when extending the comparison
because CI artifact retention is short.
