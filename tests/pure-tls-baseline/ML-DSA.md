# Focused ML-DSA comparison: 2026-10-26

EGCL remains roughly 242 times slower than SBCL on the later measured
keygen/sign/verify roundtrip (16.445 s versus approximately 0.068 s).
Tiering heuristics contribute, but changing thresholds alone did not close the
gap: earlier promotion was slower, disabling T2 was slower, and already-T2
polynomial loops remained substantially slower than SBCL.

These are exploratory measurements of one deterministic case, not a statistical
performance claim or a full-suite comparison. Every condition completed with
successful verification, correct key/signature sizes, and the same signature
SHA256 `25d60898f6aa74d6c49426f219128c16b62df3fb8a07836a9c4849e210165e8b`.

## Scope and provenance

The case uses a 32-byte seed filled with `#x42`, a 32-byte signing randomizer
filled with `#x55`, and message `Hello, post-quantum world!`. Each timed round
performs key generation, signing and verification. This fixes signing's rejection
sequence; loading and output/hash checks are outside the operation timings.

Both implementations used pure-tls `1fce0aab061bbb82777895f5fd8dab3272337265`
and Ironclad `cc43462b19b42e2827559b443159c76900f2e8aa`, on the same KVM
host with affinity to CPUs 8–11. EGCL was the release runtime
`3cb5e8b236f3b9dfd64f85c84d92f4aa46b4b166`, SHA256
`e07a41f528603c051937eba874723707f39616ee09751dfa82bf90b60a9301cd`;
SBCL was `2.6.9-1.fc44`. Ironclad's implementation-specific fast paths differ:
SBCL advertises fast mod64 arithmetic/assembly; EGCL advertises its 64-bit
primitives/assembly. This compares the pinned library as each implementation
runs it, rather than forcing identical machine-level algorithms.

The full SBCL suite was blocked during dependency loading at `PURI:URI-IS-IP6`.
That does not invalidate this successfully loaded focused case; it does prevent
claiming a full-suite SBCL comparison.

[JSON](ml-dsa-comparison-2026-10-26.json) records every round and component batch,
source/log SHA256 values and external artifact locations.
[CSV](ml-dsa-comparison-2026-10-26.csv) exposes the same measurements.
The original benchmark sources and raw logs are session-external artifacts,
not bundled files. Portable benchmarks added alongside this report can differ
in path parameterization; their hashes must not be substituted for the measured
sources' hashes.

## Roundtrip observations

| Condition | Load s | Round 0 s | Round 1 s | Deopts before → after |
| --- | ---: | ---: | ---: | --- |
| SBCL cached | 1.583 | 0.132 | 0.067 | not applicable |
| EGCL default | 26.106 | 18.565 | 16.445 | 22 → 636 |
| EGCL earlier promotion | 29.654 | 27.340 | 22.469 | 113 → 1987 |
| EGCL T2 disabled | 25.458 | 33.677 | 34.166 | not recorded |
| EGCL Keccak output helper pinned T0 | 26.479 | 11.494 | 10.238 | 22 → 22 |

SBCL rounds 1–4 were 0.067000, 0.067999, 0.068000 and 0.067999 s.
Each EGCL condition has only one observation after its first round: there is no
EGCL warm median or uncertainty estimate here. Cached loading is a separate
measurement; these are not cold-load comparisons. Earlier promotion used
`EGCL_T0_T1_THRESHOLD=1`, `EGCL_T1_T2_INVOKE_THRESHOLD=10`, and
`EGCL_T1_T2_BACKEDGE_THRESHOLD=1000`; OSR remained at its default 100,000. The selected default/early runs suppress diagnostic logging;
separate exploratory/profile runs are not pooled into these timings.

The pin intervention calls
`(egcl-ext:profile 'ironclad::keccak-state-extract-output)`. PROFILE deliberately
keeps that helper at T0. NTT helpers still reach T2. Its improvement and unchanged
deopt count isolate a substantial cost around this helper's speculation cycle;
**pinning is a counterfactual diagnostic, not a proposed production solution**.
Repeated compilation/deoptimization is confirmed and tracked by `bliss-yuxa7`;
the exact guard responsible remains unknown. More aggressive thresholds are
therefore not a universal fix.

## Already-native polynomial work

Each component measurement has three batches of 1000 operations after warmup.
NTT means one forward transform plus one inverse transform per iteration.

| Component | EGCL median s | SBCL median s | Ratio |
| --- | ---: | ---: | ---: |
| 1000 NTT/inverse pairs | 2.469 | 0.114 | 21.7× |
| 1000 polynomial additions | 0.088 | 0.008 | 11.0× |

Batch checksums match exactly between implementations. EGCL's component callers,
NTT, inverse NTT and addition helpers all report T2; deopts stay at 22 throughout.
Separate flat native samples identify executing T2 NTT and inverse-NTT code,
providing execution evidence beyond installed-tier queries. Thus this gap cannot
be explained solely by failure to promote. Arrays report element type `T` on
EGCL and `(UNSIGNED-BYTE 32)` on SBCL. Representation, generated loop code,
checked arithmetic, runtime transitions and calling conventions are candidates
for investigation; these measurements do not isolate each candidate's cost.

## What the profiles establish

A scoped flat `cycles:u` profile attributes approximately 24% of sampled cycles
to T2 inference across the mutator and compiler threads. Its event stream shows
37 T1 and 36 T2 compilations of the Keccak output helper. That diagnostic run
consed approximately 24 MB and recorded no GC during the selected operation:
GC did not explain its compilation-heavy behavior. Percentages across threads
are aggregate sampled CPU cycles, not fractions of wall time.

Callgraph sampling was denied, so flat attribution cannot reconstruct callers
or allocate all runtime costs to Lisp functions. The built-in statistical
profiler is also unsuitable for native tier shares: its current collection
hooks cover treewalk entry and T0 named calls, with no T1/T2 sampling hooks.
Do not interpret missing native samples as interpreter execution.

A separate profile after pinning still observed six compilations and one
approximately 162 ms GC. It is not a perfectly steady-state measurement.
`DECODE-POLY-Z` remained at T1: T2's mapped emitter rejected bytecode 4113,
`FixnumShl` with a variable shift count; the current emitter handles only a
constant count on this path. Installed tiers, actual native execution,
compilation events and GC pauses answer different questions and are retained
as separate evidence.

## Diagnostic array-candidate counterexample

A separate same-profile release binary built from `3cb5e8b2` plus
[the diagnostic patch](diagnostics/skip-raw-array-candidate.patch), SHA256
`81d300116a11b0194bee6f7ebfd11a0c0d3408eb8febbd97d647d8d29d88f414`,
was run once with its switch OFF and once ON. Setting
`EGCL_PROBE_SKIP_RAW_ARRAY_CANDIDATE=1` skips generic-array specialization only
in the raw candidate; the tagged-array pass remains enabled. This is not a
production change or default configuration.

| Same binary | Round 0 s | Round 1 s | Deopt increase |
| --- | ---: | ---: | ---: |
| Switch OFF | 18.960 | 16.438 | 610 |
| Switch ON | 16.569 | 14.423 | 680 |

Signature hashes still match. The later round improved about 12% in this single
process pair, while deoptimizations increased: the guard cycle remains unresolved.
An ON-only component observation took 2.535 s per 1000 NTT/inverse pairs,
versus 2.469 s in the older baseline; mapped code shrank from 21,528 to 18,690
bytes. Without a paired same-binary component control, these observations show
neither an NTT speedup nor a measured regression. Smaller code alone is not
sufficient evidence of faster execution.

## Priorities suggested by this case

1. Repair stable guard feedback and recompilation backoff for the confirmed
   Keccak cycle; identify the failing guard before choosing a production fix.
2. Extend mapped lowering for variable shifts and hot packing helpers such as
   `DECODE-POLY-Z`, then measure their effect on this unchanged case.
3. Improve already-T2 loops, array representation, generated arithmetic and
   runtime/call ABI costs using isolated components and native profiles.

No experimental runtime patch is proposed for landing by this report.
The complete failed EGCL baseline spent 80.6% of test time in OpenSSL tests and
9.8% in ML-DSA. Improving this crypto case alone cannot establish whole-suite
parity; profile OpenSSL/parser work separately. The focused analysis is tracked
by `bliss-aug7d`, alongside the full baseline's existing correctness follow-ups.
