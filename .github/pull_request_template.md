## What changed

<!-- Describe the concrete problem and the behavior before and after this change. -->

Tracking: <!-- GitHub issue and Bead, for example: Closes #123; bliss-abcde -->

## Validation

<!-- List the exact commands and results. Distinguish new failures from failures already present on the base revision. -->

```text
command
result
```

Affected execution paths and platforms:

- [ ] Tree walker
- [ ] T0 bytecode
- [ ] T1 baseline native code
- [ ] T2 optimized native code or OSR
- [ ] Foreign frames or callbacks
- [ ] Saved images or delivered executables
- [ ] Fibers, native threads, or safepoints
- [ ] Cross-target or platform-specific code

## Runtime safety

- [ ] This change cannot allocate or retain Lisp values across an allocation.
- [ ] I ran a normal and `EGCL_GC_STRESS=1 EGCL_GC_POISON=1` comparison and established that the relevant objects relocate.
- [ ] I explained below why a different GC-safety check is appropriate.

GC-safety evidence or reason it is not applicable:

<!-- Delete choices that do not apply; do not mark an inapplicable check merely to fill the template. -->

## Documentation and release impact

- [ ] User-facing documentation is updated, or behavior is unchanged.
- [ ] Relevant specification requirements and coverage markers are updated, or the specification is unchanged.
- [ ] `CHANGELOG.md` is updated, or this change does not alter the release baseline.
- [ ] Performance claims use a release build and include the workload, warmup, sample count, and comparison.
- [ ] AI-authored changes preserve the provenance required by `AGENTS.md`, or no AI assistant authored the change.
