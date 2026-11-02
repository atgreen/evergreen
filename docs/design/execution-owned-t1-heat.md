# Execution-owned T1 heat countdowns

`EGCL_EXECUTION_T1_HEAT=1` opts x86-64 Linux into execution-owned T1
sampling countdowns. The flag is ignored on other platforms; they retain their
existing countdown implementation. The default retains per-artifact countdowns.
Canonical function invocation/back-edge counters and tier thresholds are
unchanged. This is a prerequisite for code sharing; TransferCode and template
caches still have their existing execution owners.

Each physical native artifact receives a never-reused heat identity. Managed
publication installs its countdown in the execution context before exposing
code. The native prologue resolves that identity once and caches a stable raw
pointer in an unscanned frame slot, beside a raw fallback countdown initialized
to one. Missing bindings select that fallback, so hot backedges always perform
only the pointer load, subtract and conditional branch. Cold reload/request
paths compare the pointer with the fallback address to select one versus batch.
Mapped entries resolve through NativePollControl.execution; ordinary entries
use R13. OSR entries initialize
the same cache. Countdown cells survive redefinition, nested calls, directory
growth and suspended activations until execution retirement; reentry never
resets a partially consumed batch. Companion artifacts use distinct identities.

A missing context, view or cell preserves the published cold poll, reporting
one observed edge rather than an unearned batch. Each entry initializes both
raw words before any poll. Frame sizing, precise map bounds and ordinary stack
admission include the additional 16 bytes; neither word is a tagged root.
Native fiber suspension switches RSP without moving the stack (runtime
context.rs), preserving the fallback address until that frame retires. Normal
sampled polls retain the existing batch request and reload policy. No lookup allocates Lisp objects,
collects, or acquires a runtime lock in generated code.

Validation must establish independent countdowns using identical emitted code,
accumulation across short activations, old/new artifact isolation, exact missing
binding credits, actual moving roots, real fiber migration, and T1 to T2/OSR
semantic parity. A carrier-id mutation alone is not evidence of migration.
The draft remains opt-in pending central validation. Migration tests must
demonstrate real carrier changes with this mode enabled.

Performance gate: release binaries, startup-excluded CPU plus perf instructions,
minimum five samples in ABBA order, proved mapped T1 loops with heat batches 1
and 128, plus repeated short activations. Compare off/on with equal results and
tier evidence; accept at most +2% CPU and +0.5% instructions per measured cohort.
Report misses as misses. Do not infer speedup from code size or debug timings.
