# Matched forwarding-entry comparison — 2026-10-12

The fixed runtime reduced the median one-forwarder workload from **4745.73 to
2828.44 ns/iteration**, and the two-forwarder workload from **7224.25 to
2845.58 ns/iteration**. Their median *paired* increment fell from **2475.76 to
15.76 ns/iteration**. The fixed increment is small compared with its spread:
**4 of 45 differences were negative**, with a range of **−8.68 to 50.73 ns**.
This supports much lower incremental forwarding overhead, not a precise
machine-`CALL` latency or a percentage speedup for the small residual increment.

Both outer loop targets are forwarding entries. The comparison therefore avoids
the direct-leaf versus forwarder's different outer adapter selection. The
[direct-leaf benchmark](2026-10-12-ordinary-bridge.md) remains a separate absolute
regression control. These artifacts **predate default tail adoption**: both
forwarders execute ordinary non-tail calls.

## Distributions

All numbers are ns/iteration. Each row contains 45 samples: five fresh-process
trials, each with nine paired samples of 100,000 iterations. P95 uses nearest
rank. `Increment` is calculated within each pair, not by subtracting aggregate
medians. No negative differences or outliers were discarded.

| Artifact / path | Minimum | Median | P95 | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Baseline, one forwarder | 4723.97 | 4745.73 | 4765.87 | 4770.07 |
| Baseline, two forwarders | 7169.63 | 7224.25 | 7253.43 | 7497.60 |
| Baseline, increment | 2440.65 | 2475.76 | 2490.23 | 2733.84 |
| Fixed, one forwarder | 2802.84 | 2828.44 | 2877.58 | 2884.05 |
| Fixed, two forwarders | 2822.53 | 2845.58 | 2884.93 | 2903.37 |
| Fixed, increment | −8.68 | 15.76 | 36.10 | 50.73 |

The absolute median changes are −40.40% for one forwarder and −60.61% for two.
Per-trial increment medians were baseline `[2452.25, 2479.42, 2486.19, 2456.58,
2477.45]` and fixed `[13.55, 15.60, 25.38, 23.44, 8.50]`. All baseline differences
were positive. The fixed spread, process-to-process variation, remaining outer
runtime work, and occasional negative pairs limit interpretation of its small
increment.

## Path and correctness evidence

The [workload](../ordinary-chain-workload.lisp) marks LEAF, ONE, and TWO
`NOTINLINE`; both loops use identical fixnum arithmetic and arguments. Every
sample returned checksum **4999950000**. The same-artifact warmed-edge integration
test ran exactly once and passed. It proves zero cold handoffs for an equivalent
four-argument edge after warming, **not a fallback count for each timed process**.

All ten artifact-trials retained unchanged named pre/post T2 listings, with no
named deoptimization/invalidation in their logs. In every fixed-artifact trial:

- LEAF, ONE, and TWO have named ordinary ABI entries. ONE and TWO have `UD2` at
  offset zero, preserving the same ordinary-only outer adapter choice.
- Both forwarders marshal three register arguments and overflow argument four,
  load `RCX=4`, execute the admitted `call r11` at `+034a`, and retire depth at
  `+0356` before flag-preserving stack teardown. Their later cold helper CALL
  is distinct. TWO adds an ordinary call to ONE before the common ONE→LEAF edge.
- The two loops have matching hot entry/body and OSR instruction structure
  through `+0562` (241 instructions), after normalizing immediate values and
  relocated addresses while retaining registers and instruction order. Cold
  deoptimization stub ordering differs after that point; it is not evidence
  that those stubs executed. Raw operands and listings are retained for review.

There is no per-timed-process fallback counter. The measured increment includes
admission guards, argument movement, stack/depth bookkeeping, and return; it
must not be labeled isolated instruction latency. The workload requests no Lisp
heap allocation in its fixnum loop, but internal runtime allocation was not
measured. SBCL checksum smoke passed separately; its possible tail optimization
makes it unsuitable as a non-tail path witness here.

## Reproduction and provenance

Artifact trials ran **2026-10-12 02:11:47–02:13:47 UTC**. All collaborating agents
held builds/tests during the quiet window, and a process check found none before
starting. CPU affinity was 4 on the Intel Core Ultra 7 255H host, governor
`powersave`; recorded one-minute load ranged approximately 1.19–1.68. Affinity
and cooperative quiet do not establish hardware isolation. Binary order
alternated across trials, and pair order alternated within each process.

Runner revision: `586e2f76`. Command configuration: `ordinary_bridge.py --control
forwarders --cpu 4 --iterations 100000 --samples 9 --trials 5`, with
`EGCL_FORCE_TIER=t2`. Both CI builds used Rust 1.94.1, x86-64 musl release,
empty features, opt-level 3, thin LTO, and one codegen unit.

| Artifact | Compiled CI merge revision | Run / artifact ID |
| --- | --- | --- |
| Baseline | `33a09236691db374b96916df453ed8bf38a85ea9` | `38190922517` / `11720748549` |
| Fixed | `14b673182d9ee29c2d16c0f4a7d897a77aa417cd` | `38193763195` / `11722218732` |

Retained CI archive SHA256:

```text
baseline 9b4c1907571e688215e4e0976c7654c50cac0b9fa88c8425cec1aac7740d3a55
fixed    7e252caf5c16b319e8525a67f8489590171d30b8d6057d7a2736c01ef1b42599
```

The [raw manifest](2026-10-12-ordinary-chain.json) retains all paired samples,
commands, binary/manifest SHA256 values, host details, and per-process loads.
The [evidence archive](2026-10-12-ordinary-chain-evidence.tar.gz) additionally
contains every stdout/stderr/T2 log, pre/post listing, generated workload,
warmed-edge test output, CI manifests/build logs, download provenance, and
per-file hashes. The original CI binary archives remain at the paths recorded
in `measurement-provenance.json`; they are not duplicated in the evidence tar.

Evidence archive SHA256:
`6fd202129cabc78480af5b88a3f851204199a1abef2c5f1e52590ee3441412fb`.
