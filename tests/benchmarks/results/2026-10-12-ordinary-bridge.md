# Ordinary bridge release comparison, 2026-10-12

The new ordinary path improves the forwarding workload, but regresses the
matched direct-call workload. Median direct-loop cost rose **23.9%**, while
median forwarding-loop cost fell **41.0%**. The direct regression is tracked
by **bliss-zsz0x**; the bridge result does not erase it.

All values below are **nanoseconds per loop iteration**, including the common
outer runtime dispatch. Each row contains 45 samples, each of 100,000 iterations.
The increment is `forward − direct` within each measured pair, not subtraction
of separately aggregated medians.

| Runtime | Measurement | Minimum | Median | p95 | Maximum |
|---|---|---:|---:|---:|---:|
| Before | Direct loop | 2245.72 | 2252.77 | 2291.65 | 2294.04 |
| Before | Forward loop | 4726.20 | 4753.06 | 4811.38 | 4813.56 |
| Before | Paired increment | 2474.82 | 2505.77 | 2524.02 | 2525.45 |
| After | Direct loop | 2766.60 | 2791.60 | 2804.39 | 2808.53 |
| After | Forward loop | 2790.80 | 2805.51 | 2817.52 | 2831.27 |
| After | Paired increment | -11.70 | 16.31 | 29.39 | 30.08 |

Percentiles use nearest rank. The after increment is small relative to its
sample spread, including negative pairs: **16.31 ns is not a precise isolated
CALL latency**. Across five process trials, direct medians span 2248.18–2290.07
ns before and 2777.09–2797.26 ns after; forwarding medians span 4734.20–4806.27
and 2797.85–2810.89 ns respectively. These distributions describe this run,
not statistical confidence intervals or a universal performance bound.

## Method and host

The [runner](../ordinary_bridge.py) used the same checked four-argument workload
for both immutable CI release binaries. Five trials alternated binary order;
nine pairs per process alternated direct/forward order. All 180 timed calls
returned **4,999,950,000**. No samples were discarded. Warmup, listing capture,
result checking and output occurred outside the timed call.

Measurement ran from 01:30:13 to 01:31:42 UTC on an Intel Core Ultra 7 255H,
Linux 7.2.7-200.fc44 x86-64, pinned to CPU 4 with the existing `powersave`
governor. All cooperating agents held builds and tests; a process check found
no compiler before starting. The desktop remained active: this was a
coordinated quiet window, not an isolated machine. Raw host details, load
samples, commands, workload hashes and every paired duration are in the
[JSON evidence](2026-10-12-ordinary-bridge.json).

Both CI builds used Ubuntu 24.04, Rust **1.94.1**
(`e408947bfd200af42db322daf0fadfe7e26d3bd1`), musl x86-64, empty features,
`--profile release`, default optimization level 3, thin LTO and one codegen
unit. Cargo manifests/lockfile, toolchain pin, Cargo configuration, workflow,
artifact builder and memory limiter were unchanged between these revisions.
No local release build was used.

| Artifact | Before | After |
|---|---|---|
| Workflow run | [38190922517](https://github.com/atgreen/evergreen/actions/runs/38190922517) | [38191501416](https://github.com/atgreen/evergreen/actions/runs/38191501416) |
| Artifact ID | 11720748549 | 11720754431 |
| Actual CI merge revision | `33a09236691db374b96916df453ed8bf38a85ea9` | `051fed7b02018150844d6a078f9f3c75aa5bb9e7` |
| CLI SHA256 | `0713716cc07ec4f1b7b7323d57c9e34ac4ff8572e987fd55889c4c9d1b152fc7` | `1622240a06b8953e7c3e51d0821b2aef8adc95d3f4ba1feb3950b37c4a77beb9` |

The original after workflow built successfully and passed lint. Its only
regression failure was a test demanding the old mapped-ABI disassembly label
for a function now using the ordinary ABI; save/restore and multiple-value
assertions passed. Test-only commit `ef6cdce0` corrected that expectation. The
corrected standalone test passed against the **original measured CLI**, and
[final CI 38192004707](https://github.com/atgreen/evergreen/actions/runs/38192004707)
passed all required checks. Those are separate validation facts; the measured
binary is still the original `051fed7b` artifact.

## Native path evidence

Every process published each of the four named functions exactly once and
retained identical before/after listings. No named deoptimization,
recompilation or invalidation appeared in its T2 log. Representative
[before](2026-10-12-ordinary-bridge-before.asm) and
[after](2026-10-12-ordinary-bridge-after.asm) listings retain the full leaf and
bridge plus loop hot prefixes; later deopt/OSR stubs are explicitly omitted.

The after identity leaf has an ordinary entry at `+0043`; the bridge enters at
`+0040`. The bridge checks execution/R13, pending requests, carrier, deadline,
depth, descriptor identity/revision and stack admission. It sets RDI/RSI/RDX,
argument four at `[rsp]`, RCX count and RAX closure, then performs the ordinary
indirect `CALL r11` at `+034a`. Its DEC/LEA/POP/RET teardown preserves the result
tuple and CF. The cold call remains at `+03ef`.

Both loops retain outer runtime dispatch at `+03af`. Their hot instruction
sequences through `+0562` match across binaries after address normalization,
apart from the intended callee symbol. Cold deopt stub ordering differs.
Normalized leaf/bridge and hot-loop instructions also match across all five
trials within each binary.

Before timing, the exact after artifact's release test executable passed
`ordinary_register_abi_installs_and_warms_a_four_argument_call`, which checks
zero cold handoffs after warming an equivalent four-argument bridge. This is
execution evidence for the integration; **it does not count fallbacks inside
each timed process**. The report therefore measures complete workloads and
does not attribute their entire difference to one machine instruction.

Archives and build logs were preserved locally under
`/tmp/bliss-t8a48-{baseline,after}-ci`; complete raw stdout, stderr and T2 logs
are under `/tmp/bliss-t8a48-quiet`. Their hashes and archive provenance accompany
the durable JSON evidence. CI artifact retention is short; keep local copies
when reproducing or extending the comparison.
