# Typed Fibonacci after range-proof code generation

Measured runtime revision: `7cc7875658dc8ee841089c9b11b5263994f8590f`.
Related work: [PR #326](https://github.com/atgreen/evergreen/pull/326),
Beads `bliss-pafx4`, `bliss-xwpnl`, `bliss-n6fjk`, and `bliss-dpuvm`.

This remeasures the unchanged source programs and shared timing driver from
the [pure-recursion report](2026-10-13-fib-pure-recursion.md), following the
[checked-range report](2026-10-13-fib-checked-ranges.md). The working tree
was clean for the release build and all measurements.

## Results

Three processes per implementation ran serially, alternating EGCL then SBCL,
all pinned to CPU 0. Each warmed with 6000 calls to `fib(8)`, checked every
input from 0 through 40 against an independent iterative oracle, then timed
seven calls to `fib(35)`. EGCL waited for and asserted installed T2. All six
processes exited successfully, all stderr files were empty, and all 42 timed
answers were 9227465.

| Implementation | Median | Minimum | Maximum |
|---|---:|---:|---:|
| EGCL release, range proofs | 30 ms | 29 ms | 31 ms |
| Tuned SBCL 2.6.8 | 33 ms | 32.999 ms | 34 ms |

Raw elapsed ticks, one row per process:

```text
EGCL (1000 ticks/second)
30 31 30 30 30 30 30
30 31 31 30 31 31 31
29 30 30 30 30 30 30

SBCL (1000000 ticks/second)
34000 33000 33000 33000 33000 33001 33000
33999 33000 33000 33000 33000 33000 33000
32999 33000 33000 34000 33000 33000 33000
```

The earlier checked-range batch measured EGCL at 31 ms median. These are
separate batches with millisecond-resolution EGCL timing, not a controlled
attribution of a one-millisecond gain to this patch. The same-batch SBCL
comparison remains specific to this host and workload. SBCL uses its tuned
inline unsigned-64 local helper at safety 0; EGCL uses the bounded-parameter
recursive function with checked ingress. This is not equivalent source
tuning, equivalent safety policy, or general compiler parity.

## Native-code evidence

The installed EGCL code is 2360 bytes including cold paths, down from 2420
in the checked-range report. The two recursive argument lower-bound checks
are absent, as are the two subtraction overflow branches. Both direct native
self-calls and the recursive-result sum's overflow check remain.

Actual instruction excerpts follow; the absolute call/branch targets have
been renamed for readability:

```asm
; n-1: checked n in [0,40] proves this tagged subtraction cannot overflow.
+00bb: sub r10,8
+00c2: mov [rsp+10h],r10       ; no JO between subtraction and store

; First actual recursive call, after poll, shortcuts, and stack admission.
+01ac: call self_entry_at_0042

; n-2: the same checked parameter range supplies the overflow proof.
+0277: sub r10,10h

; Second actual recursive call.
+037f: call self_entry_at_0042

; Return types remain unproven: retain the sum's overflow exit.
+045f: add r11,r10
+0462: jo overflow_deoptimization
```

The disassembly contains exactly one `jo`. It still uses tagged fixnums,
result-tag guards, ordinary Lisp calls, runtime polls, and stack admission.
FTYPE metadata is now retained, but is not consumed as a trusted result fact.
The bounded-Fibonacci native regression independently checks both self-calls
and the single remaining overflow branch. No private unboxed ABI or general
nested-deoptimization self-inlining was enabled by these changes.

## Build and reproduction

The release build completed in 4m13s with one Cargo job, a 3G memory cap,
zero swap allowance, and a 900-second timeout. It reused the existing cache:

```sh
env XDG_RUNTIME_DIR=/run/user/1000 \
  DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  CARGO_INCREMENTAL=0 \
  CARGO_TARGET_DIR=/home/green/git/evergreen/target/hotspot-build \
  EGCL_MEM_MAX=3G EGCL_TIMEOUT=900 \
  scripts/egcl-limited.sh cargo build --release -j1 -p egcl --bin egcl
```

The unchanged driver sources and setup are documented in the linked
pure-recursion report. Keep NOTINLINE local to the timed caller, not global
before lazy compilation. The runner `/tmp/range-proof-release-measure.sh`
unsets `EGCL_BOOT_FILE`, uses `EGCL_MEM_MAX=1G EGCL_TIMEOUT=180`, and invokes
each process through `scripts/egcl-limited.sh taskset -c 0`:

```text
EGCL executable:
/home/green/git/evergreen/target/hotspot-build/x86_64-unknown-linux-musl/release/egcl
Arguments: --no-init --load /tmp/pure-recursion-matched-egcl.lisp

SBCL executable:
/home/linuxbrew/.linuxbrew/bin/sbcl
Arguments: --noinform --no-sysinit --no-userinit --script /tmp/pure-recursion-matched-sbcl.lisp
```

Raw stdout/stderr files are `/tmp/range-proof-release-{egcl,sbcl}-{0,1,2}.{stdout,stderr}`;
each EGCL stdout includes its full native disassembly. The build log is
`/tmp/range-proof-release-build.log`.

SHA-256 fingerprints:

```text
EGCL release executable
5ca9006c0e8972f423defe6bdbdf4eeb5bfbab124f46f90c94631ea087da3ccb
SBCL executable
e6ff14f816e7a236010eaec058c886751d93fbbfde26c8dbfed312c351a019b6
SBCL tuned FASL
fdf562a8091a2418810517af77da94b2093e1e651858452d97a779c2222cf4d1
```

## Scope limits

This run validates the release benchmark, not the full branch landing gate.
Implementation commit `7cc78756` records 590 compiler-package tests and 48
focused native runtime tests; those were not rerun for this prose-only report.
Full FTYPE result propagation, nested-deoptimization self-inlining, and the
private unboxed local-call ABI remain unfinished. `bliss-dpuvm` stays active.
