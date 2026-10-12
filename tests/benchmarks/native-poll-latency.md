# Generated T2 request observation harness

The ignored native fixture `ordinary_t2_generated_poll_request_observation_latency`
measures a process-signal-epoch notification reaching a published generated
poll on x86-64 Linux. It does **not** measure a complete stop-the-world rendezvous,
OS signal delivery, collection duration, or a portable worst-case latency bound.

Run the fixture and its two synchronization regressions through the memory
limiter, with the worktree's own Cargo target and a coordinated build slot:

```sh
CARGO_BUILD_JOBS=2 EGCL_MEM_MAX=5G EGCL_TIMEOUT=1200 \
  scripts/egcl-limited.sh cargo test -p egcl --lib poll_latency \
  -- --include-ignored --nocapture --test-threads=1
```

Preserve the full output, source revision and build profile. Reuse the resulting
test binary for repeated trials rather than rebuilding. A normal test build
contains test instrumentation and is not a release-runtime performance claim.
Record concurrent workload and host details when comparing trials; the test
does not create an isolated machine or fix CPU frequency.

The fixture installs mapped T2 code directly, verifies its code ownership, and
executes a finite native loop holding a freshly allocated cons in a native-only
home. An untimed forcing observer locates that home, removes itself, and waits
until the real poll has drained and refreshed its control state before releasing
the requester thread. The measured observer does not force idle guards cold.

The requester holds a mutex while taking its timestamp, incrementing the real
process-signal activity epoch, and publishing a unique sequence and expected
epoch. It retains the atomic increment's return value to identify its exact
arrival; a later load could include another producer's activity. This uses the
same atomic operation as the production signal announcer on its real shared
word. The generated guard therefore sees a real pending request. The published
observer requires that the expected epoch is present globally and still newer
than the target carrier's consumed epoch. It then timestamps the observation.
The sample includes the producer's short critical section, publication, observer
dispatch, mutex acquisition and native-root lookup costs.

Completion is acknowledged only after the real runtime poll drains activity and
refreshes the native control. The consumed epoch may be greater than the request
epoch because unrelated activity can arrive concurrently. Exactly one sequence
is outstanding at a time; the producer waits for its completion before spacing
and posting the next request. Separate tests reject an already-consumed epoch
and accept completion after later epoch activity.

The first measured observation also forces a moving collection after its arrival
timestamp. The fixture requires the native home to move and the generated return
to equal that moved pointer, then checks the cons payload. Collection completion
is outside the arrival metric but precedes the sequence's completion timestamp.

Output records:

- `POLL-LATENCY-PATH`: mapped T2 code identity, generated idle skips, Rust idle
  preflights and published polls.
- `POLL-LATENCY`: requested, observed, completed, missed, unposted and timeout
  counts, a `>100us` reporting bucket, relocation status and every elapsed sample.
- `POLL-LATENCY-SAMPLE`: sequence, requested/acknowledged epochs and monotonic
  timestamps relative to one shared origin.
- `POLL-LATENCY-SUMMARY`: range and nearest-rank p50/p95/p99 in nanoseconds.

All 16 requests must complete within the finite loop for the correctness fixture
to pass. No latency percentile or maximum is a pass/fail threshold. In particular,
zero samples in the `>100us` bucket does not establish the specification's bound.

The [initial validation evidence](results/2026-10-12-native-poll-latency.json)
retains source and binary hashes, raw observations and result counts: three
harness tests, 23 neighboring tests, three tests under GC stress and poisoning,
and five repeated three-test runs. Those instrumented test-profile runs were
not isolated from concurrent compilation and are correctness evidence for the
harness, not a release latency benchmark.
