# Typed Fibonacci with checked recursive argument contracts

Measured runtime revision: `174ecdb4e2220d2fec6f3a13bb692b2fe861ff2b`.
Related work: PR #326, Beads `bliss-tw3g5.2` and `bliss-dpuvm`.

This remeasures the checked-range changes after the
[pure-recursion release baseline](2026-10-13-fib-pure-recursion.md).
The source programs and shared correctness/timing driver are unchanged from
that report. The working tree was clean for the build and measurements.

## Results

Three serial processes per implementation, alternating EGCL then SBCL, each
pinned to CPU 0. Each process warmed with 6000 calls to `fib(8)`, checked all
inputs 0 through 40 against an independent iterative oracle, and measured
seven calls to `fib(35)`. EGCL waited for and asserted installed T2 before
checking results. Every timed answer was 9227465. All six processes exited
successfully with empty stderr.

| Implementation | Median | Minimum | Maximum |
|---|---:|---:|---:|
| EGCL release, checked ranges | 31 ms | 30 ms | 31 ms |
| Tuned SBCL 2.6.8 | 33 ms | 32.999 ms | 34 ms |

Raw elapsed ticks, one row per process:

```text
EGCL (1000 ticks/second)
30 31 31 31 31 31 31
30 31 31 31 31 31 31
30 31 31 31 31 30 31

SBCL (1000000 ticks/second)
33000 33000 33000 33000 33000 33000 33000
32999 33000 33000 33000 33000 34000 33000
32999 33000 33000 33000 33000 33000 33001
```

The preceding EGCL release measurement was 30 ms median; this one is 31 ms.
These are separate measurement batches with millisecond-resolution EGCL
timing, not a controlled attribution of the entire difference to one check.
The current comparison remains specific to this workload, host, and source
tuning. SBCL uses an inline unsigned-64 local helper at safety 0; EGCL uses
the unchanged bounded-parameter recursive function and checked contracts.
This is not a claim of equivalent safety policy or general compiler parity.

## Native-code evidence

The installed EGCL artifact is 2420 bytes including cold paths, compared
with 2372 in the preceding report. Both recursive call sites retain the
small-input constant shortcuts and direct self-call fallback. Each now has
a lower-bound check before its shortcuts; the upper-bound check is omitted.
The following are actual instruction offsets, with absolute branch targets
renamed for readability:

```asm
; First recursive argument, n-1, after the runtime poll:
+00eb: mov rax,[rsp+50h]
+00f0: mov rdx,0
+00fa: cmp rax,rdx
+00fd: jl  first_cold_dispatch

; Second recursive argument, n-2, after the runtime poll:
+02dc: mov rax,[rsp+50h]
+02e1: mov rdx,0
+02eb: cmp rax,rdx
+02ee: jl  second_cold_dispatch

; Remaining actual recursive calls:
+01ca: call self_entry_at_0042
+03bb: call self_entry_at_0042
```

The machine code still uses tagged fixnums, ordinary Lisp calls, result-tag
guards, multiple-value handling, and overflow branches. The source branch
implies `n >= 2` on both recursive paths, so the remaining lower-bound
checks identify a range-proof optimization opportunity, not permission to
remove declaration enforcement without a compiler proof.

## Build and reproduction

The release build completed in 4m13s with one Cargo job, a 4G memory cap,
zero swap allowance, and a 900-second timeout. It reused the existing
`target/hotspot-build` cache; no new target directory was created.

```sh
env XDG_RUNTIME_DIR=/run/user/1000 \
  DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  CARGO_INCREMENTAL=0 \
  CARGO_TARGET_DIR=/home/green/git/evergreen/target/hotspot-build \
  EGCL_MEM_MAX=4G EGCL_TIMEOUT=900 \
  scripts/egcl-limited.sh cargo build --release -j 1 -p egcl --bin egcl
```

Runtime processes used `EGCL_MEM_MAX=1G EGCL_TIMEOUT=180`, the same systemd
user-bus environment, and `scripts/egcl-limited.sh taskset -c 0`:

```text
EGCL executable:
/home/green/git/evergreen/target/hotspot-build/x86_64-unknown-linux-musl/release/egcl
Arguments: --no-init --load /tmp/pure-recursion-matched-egcl.lisp

SBCL executable:
/home/linuxbrew/.linuxbrew/bin/sbcl
Arguments: --noinform --no-sysinit --no-userinit --script /tmp/pure-recursion-matched-sbcl.lisp
```

The EGCL driver loads the bounded source, warms, asserts tier 2, loads the
shared driver from the preceding report, then disassembles `fib`. The SBCL
driver loads the precompiled tuned FASL, warms, then loads that shared driver.
Keep NOTINLINE local to the timed caller as in the preceding report.
Raw stdout/stderr are saved locally as
`/tmp/checked-range-release-{egcl,sbcl}-{0,1,2}.{stdout,stderr}`;
EGCL stdout includes each installed disassembly.
An initial invocation mistakenly used EGCL's unsupported `--script` option
and exited during argument parsing; it produced no benchmark samples.

SHA-256 fingerprints:

```text
EGCL release executable
eee9b006c56e617cbc726b156c4df876eed8285a30352b8eaa0879947a622fb7
SBCL executable
e6ff14f816e7a236010eaec058c886751d93fbbfde26c8dbfed312c351a019b6
SBCL tuned FASL
fdf562a8091a2418810517af77da94b2093e1e651858452d97a779c2222cf4d1
```

## Remaining scope

This run verifies the release benchmark, not the full branch integration
gate. The checked-contract compiler/native regression suites were recorded
with the implementation commit; they were not rerun for this prose-only
report. FTYPE result propagation, nested-deoptimization self-inlining, and
a private unboxed local-call ABI remain incomplete. `bliss-dpuvm` remains
active regardless of this timing comparison.
