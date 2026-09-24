# Babel load performance: state, method, and open leads

Written as a handoff. Everything here is measured; where something is a guess it
says so. If you read one section, read **"How to measure in this codebase"** —
this system defeats reasoning-from-source with unusual consistency, and most of
the wasted effort in this investigation came from skipping it.

## PGO is the default image build (2026-09-24, bliss-jcr9)

At the user's request, `make image` now runs the guarded PGO pipeline described
below. `make pgo-image` is an alias; `make image-no-pgo` retains the ordinary
release-image recipe. `make install` still only copies the existing executable.
The image target always retrains for the current sources and toolchain, including
when `target/torcl` already exists. A missing or mismatched `llvm-profdata` is an
error, not a silent fallback. Ordinary Cargo builds remain unchanged.

This changes the build default, not the scope of the performance evidence below.
It does not fix the unported ocicl `trivial-features` dependency or the ASDF
unsupported-implementation error runaway (`bliss-xku1`).

Validation: 12 orchestration tests pass, including default routing, output
override, failure isolation, alias coalescing, and non-PGO/install separation.
A real capped `make image` completed both compiler passes (2m12s/2m46s), all
training phases, image save and restart (`PGO-IMAGE-OK`), then loaded cached
Babel with the correct Hello encoding. The isolated output is
`target/pgo-default-check/torcl`; profiles/logs are in `target/pgo/run.urm6ec/`.
Neither the installed executable nor the user's default image was replaced.
Shellcheck, shell syntax, formatting and diff checks pass. Spec coverage still
reports the tracked 14 uncovered/11 unstaged requirements; the separate full
PGO-harness validation (`bliss-lm5f`) remains in progress. Review was a solo
adversarial review, not an independent review.

## Initially opt-in PGO images and the actual CLI gap (2026-09-24, bliss-84km)

`make pgo-image` now provides the reproducible instrument/train/merge/use/image
pipeline. At that point ordinary `make image` and Cargo release builds were unchanged. It
requires llvm-profdata matching rustc's LLVM version, uses fresh private build
and profile directories, excludes preparation profiles, checks each training
run's results and raw profile, and rejects profile-use warnings. Image saving
and restart validation happen in a staging directory beside the destination;
only then is the old executable atomically replaced. Python orchestration
tests cover failure isolation and run in CI. See README for configuration.

Real musl and GNU runs both build, train, save, and restart ASDF images. The GNU
run also exercises a space-containing build directory. Musl compiler passes
took 2m55s and 2m44s; GNU passes took 1m25s and 1m59s, with concurrent validation
affecting those times. Both have only the tracked unused-mut warning, no
profile-mismatch warnings. Validation outputs are isolated at
`target/pgo-candidate/torcl` and `target/pgo-gnu-candidate/torcl`: neither the
existing default `target/torcl` nor `/usr/local/bin/torcl` was replaced.

After build/test jobs stopped, five alternating CPU-0-pinned fresh processes
per workload/build measured first cached loads using the **actual ASDF-preloaded
executables**, without reloading ASDF into those images:

| Saved-image load | ThinLTO cycles | PGO cycles | Reduction | SBCL cycles |
|---|---:|---:|---:|---:|
| Babel | 2.359 G | 2.012 G | **14.7%** | 0.707 G |
| CL-PPCRE | 1.010 G | 0.853 G | **15.5%** | 0.621 G |

Instructions fall 6.284 → 5.095 G for Babel and 2.778 → 2.170 G for CL-PPCRE.
Allocation is unchanged at 12,094,944 and 4,298,272 Lisp bytes respectively;
no measured load recompiles source or collects garbage. Three fresh SBCL
references per library were taken in the same batch. Saved-image wall medians
are Babel **0.550 → 0.473 s** (SBCL 0.166 s), and CL-PPCRE
**0.234 → 0.202 s** (SBCL 0.144 s). This batch's Babel ranges are tight:
0.548–0.557 s before, 0.471–0.480 s after. These are measured batch results,
not universal wall-clock promises. Process startup is excluded.

**The earlier ~2× Babel figure was for the raw runtime, not the saved-image
CLI.** The actual PGO saved-image load remains **~2.85× SBCL cycles for Babel**
and **~1.37× for CL-PPCRE**. The saved image does substantially more work than
the raw runtime even though startup is outside the counter window. In the
same validation session, raw runtime Babel uses 3.500 → 2.816 G instructions
and 1.498 → 1.307 G cycles (12.7% less); raw CL-PPCRE uses
1.801 → 1.390 G instructions and 0.707 → 0.608 G cycles (14.1% less).
The raw-runtime allocation is also lower: 10,185,616 and 3,701,024 bytes.
`bliss-c6td` tracks profiling and removing this image-specific overhead;
restored tiers, method state, and captures are leads, not established causes.
There is an initialization confound: `REQUIRE ASDF` (used to build the image)
explicitly loads `lib/asdf.lisp`, while the raw benchmark loads `lib/asdf.bfasl`.
A follow-up raw PGO control using REQUIRE/source still uses only 2.935 G
instructions, 1.388 G cycles, and 10,120,080 Lisp bytes in the Babel window.
That single sample suggests the source/FASL difference alone does not explain
most of the image gap; compare images built both ways and profile restoration
before assigning a cause. Its log/counters are
`/tmp/torcl-pgo-source-asdf-control.{log,stat}`.

Focused musl PGO checks pass: all 15,106 Babel reverse-table entries match
normal/stress-20,000/poison/verify/SBCL; the actual regex scanner survives full
GC; the saved image loads cached Babel; the complete training-fixture regression
passes on the profile-use compiler, including source compilation and missing
FASL rejection. A held-out runtime probe (4M-iteration sum, sorting, mutable
capture, multiple values) produces identical output on baseline, PGO, and SBCL.
Five alternating pairs of that source/runtime probe also reduce median
instructions 2.539 → 2.147 G and cycles 0.501 → 0.438 G. This window includes
reading/compiling its Lisp source and executing its checks; it is not a pure
steady-state loop benchmark or a blanket no-regression claim. GNU table results
also match normal/stress/poison/verify/SBCL, and its saved image loads Babel.

Normal workspace gates were rerun: default **2,569 pass / 12 fail / 7 ignored**;
release **2,565 pass / 12 fail / 7 ignored**. Existing STRINGP, sequence-fixture,
doctest, and parallel threading/safepoint failures remain. A newly observed
normal-profile scheduler yield-order failure is filed as `bliss-8diq`; the
exact default scheduler/threading binaries pass all 31 tests serially. The
exact release threading/safepoint binaries also pass all 31 tests serially. These
are normal builds, **not a full PGO-compiled Rust test-harness run**. Broader
PGO validation remains tracked in `bliss-lm5f`; the workspace is not all green
and the overall performance goal is not complete.

Artifacts: `target/pgo/run.huOVbE/`, `target/pgo gnu/run.UEKgAt/`,
`/tmp/torcl-pgo-keeper[-image]-{babel,ppcre}-{before,after,sbcl}-*.{log,stat}`,
`/tmp/torcl-pgo-keeper-{default,release}-tests.log`, and
`/tmp/torcl-pgo-keeper-{tables-normal,tables-stress,regex-gc,image-load}.log`.
An earlier real run stopped because its executing shell script was edited
mid-run; that failed validation attempt is retained at `target/pgo/run.NTaujw/`.
The successful runs used a frozen script. A subsequently added regression
checks that leftover staging diagnostics cannot turn a successfully published
image into an apparent build failure; cleanup now retains them with a note.

## Reproducible PGO training fixture (2026-09-24, bliss-08hq)

`scripts/pgo-workload.lisp` supplies dependency-free training phases for the
opt-in PGO build. It requires an absolute `TORCL_PGO_WORK` private
directory and `TORCL_PGO_PHASE` set to `prepare`, `load`, or `runtime`. Run each
phase in a fresh process from the checkout root, with init files disabled.
Preparation generates and compiles a 24-file ASDF system into its private
cache. Cached loading checks every unit and refuses any attempted compilation;
runtime training checks CLOS, hash tables, sequences, and conditions. No Babel,
CL-PPCRE, ocicl, user configuration, or downloaded data is needed.

For example, with a built release CLI:

```sh
pgo_work=$(mktemp -d /tmp/torcl-pgo-training.XXXXXX)
TORCL_PGO_WORK="$pgo_work" TORCL_PGO_PHASE=prepare \
  scripts/torcl-limited.sh target/x86_64-unknown-linux-musl/release/torcl \
  --no-init --load scripts/pgo-workload.lisp
# In separate fresh processes, use TORCL_PGO_PHASE=load and =runtime.
# An instrumented build must keep preparation's raw profile separate and
# merge only the intended load/runtime profiles, never all profiles blindly.
```

The real-process regression is `bash scripts/test-pgo-workload.sh BINARY`,
wrapped in `scripts/torcl-limited.sh` for TorCL, or the same command with a
third argument `sbcl` for the SBCL oracle. It checks missing preparation,
invalid phase/path, space-containing paths, clean and repeated preparation,
two fresh cached loads with unchanged source/cache metadata, runtime results,
and rejection of a deliberately removed FASL. Test artifacts are retained in
the printed temporary directory. ASDF configuration uses string path
designators; the equivalent pathname-object destination exposes a baseline
TorCL bug tracked as `bliss-hw5x`.

The fixture's profile has now been remeasured through the guarded build
(`bliss-84km`), as recorded above. The experimental gains below are historical
spike results, not a substitute for those maintained-workflow measurements.
Broader validation remains tracked in `bliss-lm5f`.

## PGO experiment: held-out initial loads (2026-09-24, bliss-j9de)

**Experimental, not shipped.** Rust instrumentation profile-guided optimization
removes another material fraction of initial cached-load work beyond the
ThinLTO baseline `a42cc6c`. No production build configuration, default image,
installed executable, or machine-generated profile was changed by this spike.

The final training set uses bundled ASDF, a synthetic 24-file system, and
CLOS/hash/sequence/condition checks. It has no third-party dependencies;
**neither Babel nor CL-PPCRE is in training**. Cache preparation runs separately
and its profile is excluded. Three fresh cached-system processes and three
runtime processes supply the training profile. Five alternating CPU-0-pinned
fresh-process measurements per held-out library give:

| Initial-load median | ThinLTO baseline | ThinLTO + PGO |
|---|---:|---:|
| Babel instructions | 3.497 G | **2.815 G (19.5% less)** |
| Babel cycles | 1.422 G | **1.242 G (12.6% less)** |
| CL-PPCRE instructions | 1.804 G | **1.390 G (22.9% less)** |
| CL-PPCRE cycles | 0.673 G | **0.579 G (13.9% less)** |

These windows count the first ASDF system load from populated FASL caches,
not process startup, training, or repeated loads. No measured load recompiles
source or collects garbage. Lisp allocation is unchanged: Babel 10,185,616
bytes, CL-PPCRE 3,701,024 bytes. Three fresh SBCL reference loads per library,
immediately preceding this batch, have median cycles 0.624 G and 0.558 G:
the candidate remains about **2.0× SBCL cycles for Babel**, versus **1.04× for
CL-PPCRE**. This is not wall-time parity; clock regimes still vary. An earlier
profile trained on Flexi Streams plus synthetic runtime work also reduced
held-out cycles by 11.8% and 12.8%, respectively.

Both candidate profiles produce all 15,106 Babel reverse-table entries
identically to SBCL, normally and with stress stride 20,000, poison, and heap
verification. The actual CL-PPCRE scanner/full-GC probe also passes with poison
and verification. Full workspace tests, GNU-target PGO, saved-image restart,
and broad non-load performance regression checks have **not** been run for
PGO; these focused checks do not establish production readiness.

Toolchain: rustc 1.94.1, LLVM 21.1.8, matching llvm-profdata 21.1.8. The
instrumented release build took 2m08s; profile-use rebuilds took about 2m36s.
The use builds enable `-pgo-warn-missing-function` and report no profile
mismatch warnings; the existing unused-mut warning remains (`bliss-d3hs`).
Builds use isolated `target/pgo-probe`, absolute profile paths, and the same
base compiler flags and target. Profiles must be regenerated for their build,
not committed or silently reused across unrelated sources/toolchains.

Training exposed two baseline correctness bugs, not PGO regressions:
`bliss-7zc7` (Flexi in-memory input constructor has no applicable VECTOR
method), and `bliss-t4qs` (ordinary top-level INCF executes during COMPILE-FILE:
TorCL counter 1 after compile / 2 after load, SBCL 0 / 1). Fresh cached synthetic
loads correctly execute each of the 24 units once. Keeper training must make
its phases explicit without concealing the compile-time side-effect defect.

Stabilization is tracked in dependent Beads tasks: `bliss-08hq` (deterministic
dependency-free training), `bliss-84km` (guarded opt-in `make pgo-image`), and
`bliss-lm5f` (full validation and fresh held-out measurements). Opt-in is the
proposed default pending user preference; the spike scripts are disposable,
not an existing supported build workflow. The overall initial-load goal remains
open.

Artifacts: `/tmp/torcl-pgo-portable-{babel,ppcre}-{before,after}-[12345].{log,stat}`,
`/tmp/torcl-pgo-{babel,ppcre}-sbcl-[123].{log,stat}`,
`/tmp/torcl-pgo-portable-tables-{normal,stress}.log`,
`/tmp/torcl-pgo-portable-regex-gc.log`, and
`/tmp/torcl-pgo-portable-{training-v2,build}.log`. Exclude the
`ppcre-sbcl-warm` files from performance comparisons: they include compilation.

## Cross-crate release optimization (2026-09-24, bliss-zjwh)

The previously deferred ThinLTO experiment reproduces against `f8cad53`, including
the subsequent FASL method/closure fixes. Release builds now use ThinLTO and one
codegen unit; development and ordinary test profiles are unchanged. This lets
LLVM optimize across the runtime, standard-library, compiler, and CLI boundaries
without changing Lisp semantics, tiering thresholds, or cached FASLs.

Five alternating CPU-0-pinned, fresh-process initial Babel loads with populated
FASL caches and the isolated TorCL dependency port give:

| Median | Before | ThinLTO + one codegen unit |
|---|---:|---:|
| Initial-load retired instructions | 3.856 G | **3.500 G (9.2% less)** |
| Initial-load CPU cycles | 1.568 G | **1.442 G (8.0% less)** |
| Initial-load wall time | 0.737 s | 0.680 s |
| Lisp bytes allocated | 10,185,616 | 10,185,616 |

All measured TorCL loads return the correct Hello encoding, perform zero
collections, and do not recompile source. SBCL's same-batch median is 0.291 s,
but clock variation is substantial: TorCL before spans 0.629–0.754 s, after
0.336–0.684 s, and SBCL 0.155–0.293 s. The raw median ratio is 2.34×; use the
repeated instruction/cycle reduction as the improvement evidence, not a claim
of a universally stable ratio or a closed gap. Whole-process startup was not
measured in this batch. The candidate release build took 82 seconds.

After validation, a second five-pair batch on the final Cargo-configured binary
confirms **3.856 → 3.494 G instructions (9.4% less)** and
**1.590 → 1.452 G cycles (8.7% less)**. SBCL's isolated-load medians are
1.219 G instructions and 0.636 G cycles: the remaining cycle gap is about
**2.3×**, not closed. This batch's wall medians reverse direction
(0.368 → 0.502 s) despite the consistent cycle reduction: individual TorCL
samples switch between roughly 2.1 and 4.3 GHz effective clock rates. Do not
claim a stable wall-time speedup from these batches. SBCL spans 0.159–0.296 s.

Full workspace validation ran in both profiles. All **796 CLI unit/integration
tests pass, zero fail, three ignored** in each. The release workspace reports
2,564 passes, 13 failures, seven ignored; the default workspace reports 2,570
passes, 11 failures, seven ignored. Seven failures are the existing STRINGP
metadata (`bliss-4ihx`), sequence fixtures (`bliss-0bdl`), and Lisp-as-Rust
doctests (`bliss-pqyy`). Additional parallel thread/safepoint and global
allocation-counter failures are tracked in `bliss-z11t` and `bliss-57hx`.
Non-LTO release controls reproduce them, including repeated safepoint and
allocation-counter failures. The exact ThinLTO binaries pass all 34 affected
tests serially. A default-profile parallel threading repeat also SIGSEGVs;
its precise cause remains open. **The workspace is not all green.**

The optimized CLI agrees with SBCL on all 15,106 Babel reverse-table entries,
normally and with stress-20,000/poison/verification; four focused method/closure
tests pass every-allocation stress. GNU release builds and produces identical
table entries, without the musl-only memcpy wrapper. Workspace check and format
pass; root lint has six baseline findings, zero new. Existing unused-mut,
strict-clippy and spec-coverage findings remain (`bliss-d3hs`, `bliss-5vr`,
`bliss-kjjd`). The full release test build took 16m35s; ordinary profiles are
unchanged. An unnecessary development build inside a release integration-test
helper is separately tracked as `bliss-97pz`.

The motivating current-state profile has 3,325 cycle samples and zero lost
samples. Main-thread self costs remain diffuse: bytecode run-loop 5.17%,
symbol-function lookup 4.69%, evaluator and symbol-index lookup 4.00% each.
Constant materialization is only 2.98% inclusive; BBU loading including its
Lisp execution is 20.66%. This is a runtime optimization, not a new FASL format.

Artifacts: `/tmp/torcl-initial-current.perf`,
`/tmp/torcl-initial-sprof.log`, `/tmp/torcl-initial-lto-bench.sh`,
`/tmp/torcl-initial-lto-{before,after,sbcl}-*.{log,stat}`, and
`/tmp/torcl-initial-lto-final-{before,after,sbcl}-*.{log,stat}`;
validation logs are `/tmp/torcl-initial-lto-{default-workspace,workspace}.log`,
`/tmp/torcl-initial-lto-final-tables-{normal,stress}.log`, and
`/tmp/torcl-initial-nolto-{controls,repeat-*}.log`.

## Keep FASL methods compiled and their captures GC-safe (2026-09-24)

`bliss-p7ha` fixes an asymmetry between source and compiled loading: the BBU
loader did not establish a top-level frame boundary. Methods loaded through an
ASDF call frame consequently captured that incidental environment and skipped
compilation. Compiled units now establish and restore their own boundary,
including on errors; methods genuinely nested in a binding form still capture it.

This exposed two independently reproduced GC defects. Method reification kept
unrooted copies of its lambda list and body across allocating macro expansion
(`bliss-ikho`). More importantly, dormant compiled closures' captured frames
were absent from root scanning (`bliss-6xts`): CL-PPCRE's `INNER-MATCHER` became a
stale pointer after collection. The bytecode scanner now visits these frames,
and method reification roots its inputs and newly compiled code before publishing
the function. An isolated real regex scanner previously crashed after full GC;
it now returns the expected Unicode-data fields with poison and verification.

On the final release, reading the same 1,000 characters through cached Flexi
Streams allocates **27,237,504 → 1,060,160 Lisp bytes (96.1% less)**. Both return
checksum 69,642. The native-stream control stays at 129,808 bytes. One paired run
took 1.740 → 0.029 seconds, but concurrent validation and clock variation make
allocation the stronger evidence. This is a stream-execution improvement, not a
Babel initial-load claim: five alternating cached Babel samples showed no
reliable wall-time gain and essentially unchanged allocation.

Validation: **796 CLI unit/integration tests pass, zero fail, three ignored**;
four focused method/closure tests also pass every-allocation stress with poison
and verification. A further full-GC liveness assertion passes after dropping the
test's independent capture root. All 15,106 Babel reverse-table entries match
between normal TorCL, stress-20,000/poison/verify TorCL, and SBCL. Workspace check,
format and diff checks pass; root lint has six baseline findings and zero new.
The existing unused-mut warning remains tracked as `bliss-d3hs`. Non-CLI test
suites were not rerun for these internal CLI changes.

CL-UNICODE is **not working end to end**. Its original-data generator progresses
beyond the callable corruption but hits the 6 GiB cap after 410 seconds; its
last sampled input offset is 270,336 bytes. `bliss-het3` tracks unbounded compiled
closure/environment retention and repeated bytecode-body cloning. Do not remove
necessary roots to mask that cost, or present this partial progress as a completed
Unicode load. The TorCL dependency-rename port is separately available in
`ports/trivial-features-torcl/`.

Artifacts: `/tmp/torcl-unicode.UQCFUA/` (regex, stream, and full-generation
probes), `/tmp/torcl-babel-port.Fs7oj1/roots-tables-*.log`, and
`/tmp/torcl-closure-roots-cli-full.log`.

## Remove five more evaluated-argument form bridges (2026-09-24, bliss-f7qh)

A fresh census of **only the initial cached Babel load** counted 21,099
remaining calls through `apply_function`'s quoted-form reconstruction. The
largest included COERCE (2,654), APPEND (2,454), PATHNAME-DIRECTORY (2,131),
MAKE-INSTANCE (1,852), slot writes (1,849), next-method calls (1,264),
SUBTYPEP (1,086), and SUBSEQ (831). This is execution overhead, not FASL
decoding: already-evaluated arguments were being wrapped in temporary Lisp
forms and evaluated again.

Five paths now share evaluated-argument kernels with their source handlers:
APPEND, COERCE, SUBSEQ, `TORCL::SET-SLOT-VALUE`, and
`TORCL::%CALL-NEXT-METHOD`. APPEND's library behavior moved out of the
interpreter into `torcl-stdlib::sequences`; the other paths reuse existing
coercion, sequence, slot, and method machinery. Non-leaf operations were not
added to the native direct-builtin table. Cross-tier testing also exposed
and fixed three interpreter operator arms that ignored global function
redefinitions (`bliss-7bje`).

Five alternating CPU-0-pinned release samples against `9d0551c`, in fresh
processes with populated build-specific FASL caches and before test jobs:

| Median | Before | Shared value kernels |
|---|---:|---:|
| Initial Babel FASL load | 0.789 s | 0.719 s |
| SBCL initial load, same batch | 0.284 s | 0.284 s |
| Initial-load cycles (three isolated windows) | 1.691 G | **1.616 G (4.4% less)** |
| Initial-load instructions (same windows) | 4.111 G | **3.956 G (3.8% less)** |
| Lisp bytes allocated in the load | 12,610,480 | **10,568,240 (16.2% less)** |

Both versions perform zero minor/major collections in the measured load,
and no measured run recompiles. Clock variation matters: load ranges are
0.670–0.794 s before, 0.631–0.764 s after, and 0.239–0.286 s for SBCL.
The raw ratio of medians is 2.53×, but this is not as stable a wall-time
comparison as the preceding short-copy batch. Treat the remaining gap as
roughly 2.5–3× on this host; the repeated instruction/cycle reduction is
the firmer evidence for this change. Do not claim the gap is closed.

The five new dispatcher tests were observed failing before implementation.
Cross-tier coverage checks tail sharing, coercion to functions, slicing,
slot writes, original/replacement next-method arguments, multiple values,
errors, and lexical/global replacement. A fresh-process FASL regression
deletes its source before loading and agrees at normal and GC-stress strides
1/7/31 with poison and heap verification. A stdlib full-GC regression checks
APPEND's copied heads and shared tail. All **15,106** entries in Babel's two
encoding tables agree between normal TorCL, stress/poison/verify TorCL,
and SBCL. Review was adversarial self-review, not independent review.

Final gates: **733 CLI integration tests pass, zero fail, three ignored**;
45 CLI unit tests pass. The non-CLI workspace has 1,778 passes, five known
failures, four ignored: STRINGP metadata (`bliss-4ihx`) and four sequence
fixtures (`bliss-0bdl`). Two existing doctests fail (`bliss-pqyy`). Workspace
check passes with the tracked unused-mut warning (`bliss-d3hs`); root lint
has six baseline findings, zero new. Strict clippy stops at nine existing
runtime diagnostics (`bliss-5vr`); spec coverage retains 14 uncovered
stage-5 and 11 unstaged requirements (`bliss-kjjd`). Workspace format drift
remains (`bliss-2uj1`); changed code was formatted without sweeping unrelated
lines, and diff checks pass. The workspace is not all green.

Two alternative probes were rejected before this keeper: removing the
native builtin function-cell guard entirely saved only 0.15% instructions
and 0.62% cycles (`bliss-dw24`, closed); compiling keyword
DESTRUCTURING-BIND through an APPLY/lambda rewrite saved 1.0% instructions
and 0.49% cycles while adding 131,472 Lisp bytes (`bliss-jx2n`, reopened).
Neither speculative change remains. An alleged PATHNAME-DIRECTORY rooting
bug was refuted by allocation tracing and output-equal stress testing
(`bliss-85cy`). Separately tracked inspection findings: duplicate
MAKE-INSTANCE initarg-key evaluation (`bliss-zbcw`) and SUBSEQ's existing
NIL-end/noninteger-index handling (`bliss-wo86`); neither is fixed here.

Artifacts: `/tmp/babel-bridge-census.log`,
`/tmp/babel-bridge5-final-{before,after,sbcl}-*.{log,stat}`,
`/tmp/babel-bridge5-final-window-{before,after}-*.{log,stat}`,
and `/tmp/babel-bridge5-{normal,stress,sbcl}-tables.log`.

## Reduce short-copy latency in the musl CLI (2026-09-24, bliss-gv5v)

The next win is below the Lisp dispatch layer. The post-literal profile put
12.26% of self samples in musl `memcpy`; disassembly showed alignment and
REP MOVSQ setup even for small buffers. Its missing frame pointer can skip
the immediate caller in sampled stacks, so this was **not** evidence for a
speculative error-object or ownership rewrite.

The Linux x86-64 musl CLI now wraps `memcpy`: lengths 0–64 use bounded
scalar/SSE2 head/tail copies, and larger lengths tail-call the original libc
routine. All source chunks are loaded before any stores. SSE2 is baseline
on x86-64; there is no CPU probing, AVX/ERMS dependency, allocation, lock,
TLS, or runtime initialization. Cargo scopes the linker argument to this
binary and target; GNU builds and library consumers retain their libc.
An initial probe also changed bulk copies, but preserving libc above 64 bytes
retained the gain, so that larger change was discarded.

Five alternating CPU-0-pinned release samples against `2e97d50`, fresh
processes with populated build-specific FASL caches, before test jobs:

| Median | Before | Short-copy path |
|---|---:|---:|
| Initial Babel FASL load | 0.932 s | **0.801 s (14.1% less)** |
| SBCL initial load, same batch | 0.287 s | 0.287 s |
| TorCL / SBCL | 3.25× | **2.79×** |
| Initial-load cycles (three isolated windows) | 1.974 G | **1.692 G (14.3% less)** |
| Initial-load instructions (same windows) | 4.135 G | 4.109 G (0.6% less) |
| Whole-process wall time | 3.40 s | 3.01 s |
| Whole-process cycles | 7.248 G | 6.365 G |

This is chiefly a latency gain, not eliminated Lisp work. Both versions
allocate 12,610,480 Lisp bytes and perform zero minor/major collections in
the measured load. Headline load ranges are 0.931–0.947 s before,
0.797–0.807 s after, and 0.286–0.288 s for SBCL. Whole-process ranges are
3.26–3.42 s before and 2.68–3.03 s after. One candidate isolated-window
sample runs in 0.422 s instead of ~0.800 s while retaining similar cycles
and instructions; use the five-pair headline and cycle evidence, not that
outlier, for the claim. Performance evidence is from this host, not a
cross-machine guarantee. The gap remains substantial.

A second five-pair batch after the test-harness binding correction and all
test jobs confirms **0.929 → 0.796 s (14.3% less)** for TorCL. Isolated-load
cycles are **1.971 → 1.686 G (14.4% less)**, with instructions 4.128 → 4.113 G
and unchanged allocation/GC. Whole-process wall medians are 3.38 → 2.88 s.
However, this batch has pronounced frequency-regime shifts: SBCL spans
0.153–0.286 s (median 0.206), and TorCL has a 0.404 s candidate sample.
Its wall-time ratio is not comparable to the stable first batch; the ~2.8×
headline above refers specifically to that first batch, not universal parity
across clock regimes. The repeated cycle reduction corroborates the change.

The keeper was rebuilt in a fresh Cargo target directory. Five dedicated
tests check every short length and alignment pair, untouched surrounding
bytes, read-only sources and inaccessible guard pages, zero-length invalid
pointers, large libc fallbacks, concurrent calls, callee-saved registers and
the direction flag, and overlapping small ranges. The bytewise oracle uses
volatile accesses independently of memcpy. The initial three tests were
observed failing against an inert stub before implementation. Linked-binary
disassembly verifies the large-copy tail jump resolves to original `memcpy`,
not the wrapper. A GNU release build has no wrapper symbol and passes a
copy/reverse smoke test. Normal and stress-20,000/poison/verification Babel
output match fresh SBCL on all 15,106 reverse-table entries. Review is
adversarial self-review, not independent. It caught a binary-unit-test-only
linker recursion: a `cfg(test)` fallback to `memcpy` was itself wrapped.
The kernel now always names `__real_memcpy`; only the unwrapped integration
test supplies a libc trampoline. The binary test harness then exits cleanly,
and all five dedicated tests pass again.

Final correctness gates: **729 CLI integration tests pass, zero fail, three
ignored**, including all 358 acceptance, 52 FASL, and 12 differential tests.
The previously flaky T2-observation case passes in this run. CLI units pass
40/40. The non-CLI workspace has 1,777 passes, five known failures, four
ignored: STRINGP metadata (`bliss-4ihx`) and four sequence fixtures
(`bliss-0bdl`). Two existing Lisp-as-Rust doctests fail (`bliss-pqyy`).
Workspace check passes with the tracked unused-mut warning (`bliss-d3hs`).
Root lint has six baseline findings, zero new. Strict clippy stops at nine
existing runtime diagnostics (`bliss-5vr`); spec coverage still has 14
uncovered stage-5 requirements and 11 unstaged requirements (`bliss-kjjd`).
Existing workspace format drift remains (`bliss-2uj1`); the new files pass
targeted rustfmt and the changed lines pass diff checks. The whole workspace
is not all green.

Artifacts: `/tmp/babel-copy-first-batch/` (first benchmark),
`/tmp/babel-copy-final-*` (repeat), `/tmp/babel-copy-*.log`, and
`/tmp/babel-copy-final-bench.sh`. The refreshed frame-pointer profile
(`bliss-9h2o`) is `/tmp/babel-post-copy.perf`: 3,545 samples, zero lost, no
source compilation and no GC. Main-thread self samples: run_loop 5.04%,
symbol_function 4.66%, find_index 3.34%, eval_list 3.15%, short-copy wrapper
1.77%, original memcpy 1.10%. Inclusive samples: source read/eval 11.82%,
extended LOOP 9.94%, variadic binding 5.17%, make-instance 4.53%, constant
materialization 2.56%, ClassDef cloning 1.02%. Tests were active, so use this
for attribution only. Inclusive costs overlap, not all are removable. The
next bounded probe is `bliss-dw24`, the native function-cell epoch fast path,
with a full mutation audit and redefinition guards intact.

## Keep numeric literals in compiled artifacts (2026-09-24, bliss-ujsc)

BBU version 1.10 adds the reserved portable bignum and double-float constant
encodings. Doubles preserve their IEEE bits; integers use signed little-endian
magnitude bytes, including partial final limbs. Ratio constants can now contain
bignum components. The parser rejects invalid signs, noncanonical magnitudes,
truncation, and use of the new tags under an older bytecode version. Existing
artifacts remain readable; an old reader rejects the new version cleanly.

This avoids **whole-file source fallback** for Alexandria's `types.lisp` and
`numbers.lisp`. It removes source reading/expansion during the initial load,
not merely a few decoder instructions. The throwaway probe was removed before
keeper implementation; both pool units and the source-free artifact regression
were observed failing before the implementation.

Five alternating CPU-0-pinned release samples against `b0e0fdc`, fresh
processes with populated build-specific FASL caches, before test jobs:

| Median | Before | Numeric literal encoding |
|---|---:|---:|
| Initial Babel FASL load | 0.969 s | **0.925 s (4.5% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 3.39× | **3.23×** |
| Initial-load instructions (three isolated windows) | 4.354 G | **4.138 G (5.0% less)** |
| Lisp bytes allocated during load | 13,465,040 | **12,610,480 (6.3% less)** |
| Whole-process instructions | 17.522 G | **17.313 G (1.2% less)** |

Both versions perform **zero minor and zero major collections** during all
measured loads. Initial-load wall ranges are 0.832–0.975 s before and
0.806–0.932 s after; SBCL spans 0.285–0.288 s. Whole-process wall medians
are **3.29 → 3.38 s**, with ranges 3.25–3.44 and 2.84–3.40 s. Whole-process
wall time is noisy and slightly worse in this batch: do not infer a startup
speedup from the load-window result. The remaining initial-load gap is still
substantial.

Exact-bit pool tests include signed zero, subnormals, infinities, a NaN payload,
multi-limb positive/negative bignums, and pool canonicalization. The exact-value
unit also passes every-allocation stress with poison and heap verification.
The source-deleted artifact test rejects legacy source and EvalSource, and
loads nested numeric literals and a big-denominator ratio at stress strides
1, 7, and 31 with forced T1, poison, and verification. Normal and stress-20,000
Babel output match fresh SBCL on all 15,106 reverse-table entries. A saved
`b0e0fdc` binary produces an old numeric artifact accepted by the new reader
and cleanly rejects a new artifact with `unsupported bytecode version 0x010a`.
Review is adversarial self-review, not independent.

Final gates: CLI units pass 40/40; CLI integration reports **723 passed, one
known T2-observability failure, three ignored** (`bliss-ugmu`). All 358
acceptance, 52 FASL, and 12 bytecode differential tests pass. The non-CLI
workspace suite reports 1,777 passed, five known failures, four ignored:
STRINGP compiler metadata (`bliss-4ihx`) and four sequence fixtures
(`bliss-0bdl`). The two existing Lisp-as-Rust doctests still fail (`bliss-pqyy`).
Workspace check passes with the tracked unused-mut warning (`bliss-d3hs`).
Root lint reports six baseline findings, zero new. Strict clippy stops at the
nine existing runtime diagnostics (`bliss-5vr`); spec coverage and existing
format drift remain (`bliss-kjjd`, `bliss-2uj1`), with no new formatting
findings in the changed code. These are not all-green workspace results.

Artifacts are `/tmp/babel-number-final-*`, `/tmp/babel-number-*.log`, and
`/tmp/babel-number-final-bench.sh`. The preceding T0 dispatch-before-metadata
probe saved only 0.05% of load instructions and was removed (`bliss-pwbb`).
The distinct native function-cell epoch idea remains unmeasured (`bliss-dw24`).
The refreshed isolated-load profile (`bliss-xqz0`) is
`/tmp/babel-post-literals.perf`: 3,968 samples, zero lost, no source compilation
and no GC. Self samples: memcpy 12.26%, run_loop 5.22%, eval_list 3.55%,
find_index 3.42%, symbol_function 3.38%. Inclusive samples: bind_variadic 7.69%,
eval_make_instance 5.84%, ClassDef cloning 2.04%, source reading/evaluation
12.24%, extended LOOP 10.52%, constant materialization 2.29%. These overlap;
Lisp execution beneath an interpreter frame is not all removable overhead.
Tests were active during sampling, so this profile supplies attribution, not
wall-time evidence. The next copying investigation is `bliss-gv5v`; a
speculative error-boxing rewrite is not justified by this profile alone.

## Remove value/sequence/comparison form bridges (2026-09-24, bliss-ixm0)

The next measured batch removes quoted-form reconstruction for VALUES,
VALUES-LIST, REVERSE, ENDP, and non-binary ordered comparisons. Interpreted and
evaluated-argument calls share comparison and multiple-value kernels; REVERSE
still delegates to the existing stdlib implementation. VALUES-LIST traverses
its proper-list argument once instead of validating, traversing again, and
cloning the result vector. Leaf calls also enter the direct builtin table.
Function-cell invalidation and arity checks remain in place.

Five alternating CPU-0-pinned release runs against `328e44a`, fresh processes
with populated build-specific FASL caches, before test jobs:

| Median | Before | Shared evaluated-argument kernels |
|---|---:|---:|
| Initial Babel FASL load | 1.015 s | **0.969 s (4.5% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 3.55× | **3.39×** |
| Initial-load instructions (three isolated windows) | 4.612 G | **4.350 G (5.7% less)** |
| Lisp bytes allocated during load | 17,090,960 | **13,465,040 (21.2% less)** |
| Whole-process instructions | 17.850 G | **17.528 G (1.8% less)** |

Both versions perform **zero minor and zero major collections** during every
measured load: unlike the preceding numeric-predicate change, this comparison
does not move a collection across the load timer. Whole-process wall medians
are 3.48 → 3.42 s; ranges are 3.21–3.48 and 3.08–3.44 s. Initial-load ranges
are 1.012–1.018 and 0.631–0.971 s; SBCL spans 0.151–0.287 s. Instruction counts
corroborate the direction despite the occasional much faster wall sample.
The remaining gap is substantial, not parity.

Watched red-to-green units require evaluated-argument dispatch and no temporary
Lisp allocation for value returns, empty reversal, and ENDP. Cross-tier cases
check zero/one/many values, stale secondary values, lists/vectors/strings/bits,
exact rational/bignum comparisons, arity/type errors, lexical shadowing, and
post-warmup replacement. The replacement test exposed existing operator-arm
inconsistencies for REVERSE/ENDP/VALUES-LIST, now fixed (`bliss-3ok2`). A
source-free FASL regression exercises runtime-created heap strings through
native calls at stress strides 1, 7, and 31 with poison and heap verification.
Normal and stress-20,000 Babel loads match fresh SBCL output on all 15,106
reverse-table entries. Review is adversarial self-review, not independent.

The Dietz comparison/REVERSE/ENDP/VALUES/VALUES-LIST run passes 204 of 205
tests; the saved `328e44a` binary gives the same result. Its lone failure,
unary `/=` rejecting a complex argument, is tracked as `bliss-cbjz` and is
outside the ordered-comparison change. CLI units pass 38/38; CLI integration
reports 722 passed, one known asynchronous T2-observation failure
(`bliss-ugmu`), and three ignored. All 358 acceptance, 51 FASL, and 12 bytecode
differential tests pass. The two existing Lisp-as-Rust doctests still fail
(`bliss-pqyy`). The non-CLI
workspace run reports 1,772 passed, ten failed, four ignored: five known
compiler/sequence failures (`bliss-4ihx`, `bliss-0bdl`) and five already-tracked
parallel safepoint-test failures (`bliss-cfo6`). An immediate serial rerun of
that concurrency target passes 21/21. Workspace check passes with the tracked
unused-mut warning (`bliss-d3hs`); root lint has six baseline findings and zero
new ones. Strict clippy stops at the nine existing runtime findings
(`bliss-5vr`). Existing spec-coverage and formatting failures remain
(`bliss-kjjd`, `bliss-2uj1`), with no new formatting findings in this change.
Gate logs are `/tmp/babel-value-*.log`; these results are not an all-green
workspace claim.

The fresh profile that selected this work is `/tmp/babel-post-numpred.perf`
(4,116 samples, zero lost, no source compilation). A single-lock named-function
lookup probe saved only 0.34% of load instructions and was removed (`bliss-27nz`).
The value-bridge throwaway probe was also removed before keeper tests; its
5.45% instruction reduction justified this implementation. Final artifacts are
`/tmp/babel-value-final-*` and `/tmp/babel-value-final-bench.sh`.

## Stop rebuilding forms for numeric predicates (2026-09-24, bliss-qcqx)

A temporary census of the evaluated-argument-to-source-form bridge counted
28,218 ZEROP calls and 1,920 PLUSP calls during the initial FASL load alone.
Those calls synthesized quoted Lisp forms only to evaluate them again.
ZEROP, PLUSP, and MINUSP now use shared, non-Lisp-allocating stdlib kernels
directly from interpreted and compiled calls. The census instrumentation is
removed. Function rebinding, lexical shadowing, arity checks, type errors, and
single-value semantics remain covered across interpreter, T0, and T1 paths.

Five alternating CPU-0-pinned release runs against `d16a84a`, fresh processes
with populated build-specific FASL caches, before running test jobs:

| Median | Before | Direct numeric predicates |
|---|---:|---:|
| Initial Babel FASL load | 1.222 s | **1.015 s (16.9% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 4.27× | **3.55×** |
| Initial-load instructions (three isolated windows) | 5.444 G | **4.621 G (15.1% less)** |
| Lisp bytes allocated during load | 21,175,568 | **17,090,960 (19.3% less)** |
| Whole-process instructions | 18.687 G | **17.861 G (4.4% less)** |

**GC timing contributes substantially to this result.** Baseline performs one
minor collection during load; the candidate performs none. Removing temporary
allocation lets the load finish just before the nursery fills (66,467,616 of
67,108,864 bytes occupied in a separate ROOM diagnostic). Subsequent allocation
can trigger the collection. No heap-size or collection-threshold knob changed,
but this is not a claim that the entire gain is intrinsic dispatch speed or
that collection work disappeared permanently.

An equal-collection control allocates a 65,536-element list before enabling
the initial-load instruction counter. Both versions then collect exactly once
during load: three-pair medians are **5.437 G → 5.049 G instructions (7.1% less)**.
This diagnostic ran alongside tests, so its wall times are not used. It shows
direct work reduction separately from crossing the GC threshold in the actual
workload. Whole-process wall medians in the uncontended headline batch are
3.70 → 3.49 s, with broad ranges 2.23–3.71 and 3.13–3.49 s. Initial-load ranges
are 0.845–1.225 and 1.014–1.016 s. The remaining gap is still substantial.

The shared kernels also fix exact-rational sign checks (`bliss-85sk`): converting
1/(ASH 1 2000) to float previously made ZEROP true and PLUSP false. The new
implementation inspects exact integer/ratio signs, handles complex zero, and
keeps PLUSP/MINUSP real-only. A watched rebinding regression exposed operator
calls ignoring global replacements; those now agree with FUNCALL/APPLY
(`bliss-jy9s`). Zero-allocation bridge and exact-rational regressions were
observed failing before implementation. Review is adversarial self-review,
not independent.

All 36 Dietz ANSI tests for these three predicates pass. A source-free FASL
test rejects legacy source/EvalSource, deletes the source, and exercises native
calls on runtime-created heap numbers under stress strides 1, 7, and 31 with
poison and verification. Normal and stress-20,000/poison/verify Babel output
matches fresh SBCL output on all 15,106 reverse-table entries.

Full gates report 1,777 non-CLI workspace tests passed, five known failures,
four ignored; CLI units pass 36/36 and CLI integration passes 720 with three
ignored. This includes all 358 acceptance, 50 FASL, and 12 bytecode differential
tests. The previously intermittent T2-observation test passes this batch;
that is not a claim to have fixed `bliss-ugmu`. The five non-CLI failures remain
STRINGP metadata (`bliss-4ihx`) and four sequence fixtures (`bliss-0bdl`).
Two existing Lisp-as-Rust doctests fail (`bliss-pqyy`). Workspace check passes
with the tracked unused-mut warning (`bliss-d3hs`). Root lint reports six
baseline findings, zero new. Strict clippy stops at nine existing runtime
findings (`bliss-5vr`); a separate stdlib run finds no new warnings. Existing
spec-coverage and formatting failures remain (`bliss-kjjd`, `bliss-2uj1`),
with none of the formatting diagnostics targeting this change's new code.
These results do not represent a fully green workspace.

Artifacts: `/tmp/babel-numpred-final-*` and
`/tmp/babel-numpred-final-bench.sh` for the headline comparison;
`/tmp/babel-numpred-balanced-*` and
`/tmp/babel-numpred-gc-balanced-window.lisp` for the equal-collection control;
`/tmp/babel-numpred-*.log` for correctness gates.
The next fresh initial-load profile is tracked in `bliss-wf7f`; remaining
bridge-call counts are leads, not proof of which cost dominates now.

## Reuse class-precedence query results (2026-09-24, bliss-uqle)

The post-ASH initial-load profile put 3.48% under class-precedence queries.
Method applicability repeatedly rebuilt the same C3 inheritance lists. Those
query results are now reused, with conservative invalidation on **every mutable
CLOS-state access and every GC**. Bootstrap registration-order inference can
change an otherwise untouched class's ancestors, so invalidating only the
explicitly redefined class would be incorrect. The derived cache holds only
registered classes, does not cache errors, and is omitted from core images.
It is discarded before root relocation, not retained as a second owning root.

Five alternating CPU-0-pinned release runs against `7a3c7d2`, fresh processes
with populated build-specific FASL caches, before running test jobs:

| Median | Before | Reused precedence lists |
|---|---:|---:|
| Initial Babel FASL load | 1.249 s | **1.224 s (2.0% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 4.37× | **4.28×** |
| Initial-load instructions (three separate isolated windows) | 5.583 G | **5.447 G (2.4% less)** |
| Whole-process instructions | 18.825 G | 18.680 G |

Every load still allocates 21,175,568 Lisp bytes and performs one minor, zero
major collections; median GC time is essentially unchanged, 0.146688 versus
0.146774 seconds. This removes Rust-side hierarchy reconstruction, not Lisp
allocation or a collection from the timer. Wall ranges are 0.983–1.253 s before
and 1.222–1.229 s after. Whole-process wall medians are 3.50 versus 3.70 s,
with ranges 3.26–3.74 and 3.33–3.71 s: this batch does **not** establish a
whole-process startup speedup.

The deterministic regression first failed at 1,000 C3 calculations for 1,000
identical queries; it now requires exactly one and checks independent result
vectors. Other unit cases cover ancestor redefinition, cycle introduction and
repair, unknown-class registration, inferred-superclass changes, GC invalidation,
and core restoration. Normal and GC-stress-20,000/poison/verify loads match a
fresh SBCL run on all 15,106 Babel reverse-table entries. Measurement artifacts
are `/tmp/babel-cpl-final-*`; the driver is `/tmp/babel-cpl-final-bench.sh`.

The six CPL units pass, as does a Lisp redefinition/dispatch regression at
stress strides 0, 1, 7, and 31 with poison and verification. That focused test
skips bootstrap: its original bootstrap-enabled version exposed `bliss-vzph`,
independently reduced to the saved **baseline** running only `--eval 1` at
stress stride 31 (an evacuated-nursery reference in a function object).
This is a tracked pre-existing GC failure, not a passing bootstrap stress gate.
Review was adversarial self-review, not independent.

The full non-CLI workspace run reports 1,774 passed, five known failures,
four ignored; serial CLI units pass 34/34. CLI integration reports 715 passed,
two failed, three ignored: the original bootstrap-enabled regression above,
now passing in its isolated form, and the existing T2-observation failure
(`bliss-ugmu`). All 357 pre-existing acceptance, 49 FASL, and 12 bytecode
differential cases pass. Two existing Lisp-as-Rust doctests still fail
(`bliss-pqyy`). Workspace check passes with the tracked unused-mut warning;
root lint has six baseline findings, zero new. Strict clippy stops at the nine
existing runtime findings (`bliss-5vr`); a separate stdlib run reports no new
findings. Spec coverage and formatting retain their existing failures
(`bliss-kjjd`, `bliss-2uj1`); new code is formatted. Detailed gate logs are
`/tmp/babel-cpl-*.log`. These results are not an all-green workspace claim.

Two other probes were removed: keying generic dispatch only by required
arguments saved 0.1% of instructions (`bliss-2t7v`); allowing EVAL through the
ordinary call lowerer saved 0.9% (`bliss-jx2n`). EVAL's existing null-lexical
environment violation is tracked separately in `bliss-rrcn`. Neither probe is
part of this change. The gap remains substantial; these measurements do not
claim parity with SBCL.

## Shift integers instead of exponentiating (2026-09-24, bliss-7jt1)

A fresh load-only profile after `d4427b8` put minor GC at 9.8% inclusive,
but also exposed **8.0% under ASH**. The bootstrap implementation calculated
powers of two and multiplied or divided, paying for general rational arithmetic,
GCD, interpreter calls, and temporary values merely to shift integers.
`torcl-stdlib::numbers::ash` now shifts fixnums directly and bignums limb-wise.
Both tree-walked and compiled builtin calls use that kernel; the bootstrap
workaround is gone. Right shifts preserve negative rounding and huge-count
saturation. Both arguments remain type-checked, including zero cases.

Five alternating CPU-0-pinned release runs against `d4427b8`, each a fresh
process with its build-specific FASL cache populated, before any test jobs:

| Median | Before | Direct integer shifts |
|---|---:|---:|
| Initial Babel FASL load | 1.362 s | **1.250 s (8.2% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 4.8× | **4.4×** |
| Initial-load instructions (three separate isolated windows) | 6.250 G | **5.583 G (10.7% less)** |
| Whole-process instructions | 19.502 G | 18.827 G |
| Minor-GC pause during load | 0.146229 s | 0.146936 s |

Every measured load still performs one minor and zero major collections.
The change removes roughly 5.6 MB of temporary Lisp allocation per load;
it does not defer collection or move work outside the timer. Load wall samples
span 0.922–1.366 s before and 1.109–1.255 s after. Whole-process medians are
3.83 s before and 3.71 s after, with wide ranges of 2.79–3.86 s and 3.11–3.74 s.
The isolated instruction counts support the improvement despite timing noise.
This remains a substantial gap to SBCL, not parity.

The regression first failed with `undefined function: ASH` before bootstrap
(R5.06). Tests now check 247 operand/count combinations through ordinary calls,
FUNCALL, and APPLY in interpreter, T0, and T1 modes, including function
rebinding. A direct-kernel test checks 150,000 small shifts without allocating
Lisp heap objects, fixnum boundaries, and oversized counts. A source-free FASL
test rejects legacy source and EvalSource actions, then exercises allocating
native calls under GC stress strides 1, 7, and 31 with poisoning and verification.
Normal and stress-20,000/poison/verify Babel runs match a fresh SBCL run on all
15,106 reverse-table entries. Review is adversarial self-review, not independent.
All 12 Dietz ANSI ASH tests also pass, including randomized fixnum/bignum shifts,
huge negative counts, type errors, and argument evaluation order.

The full non-CLI workspace gate reports 1,767 passed, six failed, four ignored.
The CLI gate reports 715 passed, three failed, three ignored, plus an aborted
parallel unit-test process. Its serial rerun passes all 34 units. All 357
acceptance, 49 FASL, and 12 bytecode differential tests pass. Known failures
remain in STRINGP metadata (`bliss-4ihx`), four sequence fixtures (`bliss-0bdl`),
two Lisp-containing doctests (`bliss-pqyy`), asynchronous T2 observation
(`bliss-ugmu`, also fails alone), and the parallel runtime lifecycle
(`bliss-lb6.20`). The additional reader property-test failure is an unchanged
generator/oracle mismatch: `Symbol("NIL")` renders as `NIL` and reads as `Nil`
(`bliss-stz9`). Workspace check passes with the tracked unused-mut warning;
root lint has six baseline findings and zero new ones. Strict clippy still
stops at nine baseline runtime errors; a separate non-strict stdlib run finds
nothing in the new module. Existing spec-coverage and rustfmt failures remain
(`bliss-kjjd`, `bliss-2uj1`); none of the formatting diagnostics target the new
module or tests. These gates are not represented as fully green.

The valid profile is `/tmp/babel-current-single.perf`; an earlier recording
around three child processes lost symbol mappings and was discarded as evidence.
Measurement artifacts are `/tmp/babel-ash-final-*`, with the benchmark driver
`/tmp/babel-ash-final-bench.sh`. Remaining copy/C3 and external-root leads are
tracked in `bliss-uqle` and `bliss-399z`; neither is a proven next optimization.

## Walk dead nursery objects once (2026-09-24, bliss-j0wa)

Minor GC separately walked all nursery objects to repair TLAB gaps, find pinned
regions, and build the exact-object-start index. Its evacuation pass then walked
them again to skip the dead objects. Preparation now combines the first three
walks; evacuation enumerates the live bitmap directly, in the original address
order. This removes work, without enlarging the nursery or deferring collection
past the load timer. Pinned-region retention, persistent forwarding, large
filler footprints, and exact-start validation remain intact.

Five alternating CPU-0-pinned release runs against `b31a3d0`, using fresh
processes and populated build-specific FASL caches:

| Median | Before | Single preparation walk + live iteration |
|---|---:|---:|
| Initial Babel FASL load | 1.412 s | **1.361 s (3.6% less)** |
| SBCL initial load, same batch | 0.287 s | 0.287 s |
| TorCL / SBCL | 4.9× | **4.7×** |
| Minor-GC pause during load | 0.195733 s | **0.146118 s (25.3% less)** |
| Initial-load instructions (three separate isolated windows) | 6.426 G | **6.256 G (2.6% less)** |
| Whole-process instructions | 19.689 G | 19.496 G |

Every measured TorCL load performs one minor and zero major collections.
Wall samples remain noisy: 1.073–1.415 s before, 1.036–1.362 s after.
Whole-process medians, including ASDF startup, were 3.51 s before and 3.85 s
after despite fewer instructions; this batch does **not** establish a startup
speedup. The isolated instruction check ran concurrently with tests, so its wall
times are not used. The five-run wall batch finished before workspace tests.

The deterministic regression first failed at **8,101 header reads for 2,000
mostly-dead objects**. It now reads **2,099** headers, passes a bound of fewer
than 2,256 reads, and checks the surviving cons's relocation and contents.
Other new tests cover bitmap
word/region boundaries and reset, zero-filled TLAB gaps, pinned forwarding
stubs, and large-header filler spans. All 625 runtime tests pass, including
61 unit tests.
Normal and stress/poison/verify runs match SBCL on all **15,106** Babel
reverse-table entries. An every-allocation stress/poison/verify run also
preserves source-free native-cons output `CONS-STRESS (100 99 0 1)`.
Review is adversarial self-review, not independent.

The complete workspace gate records **2,512 passed, seven failed, seven
ignored**: non-CLI 1,767/5/4 and CLI 745/2/3. All 357 acceptance, 48 FASL,
and 12 bytecode differential tests pass. The failures are the tracked compiler
STRINGP metadata test (`bliss-4ihx`), four sequence fixtures (`bliss-0bdl`), and
two Lisp-containing doctests (`bliss-pqyy`). The previously flaky T2 observation
test passes in this run. Workspace check passes with the existing unused-mut
warning (`bliss-d3hs`); root lint reports six baseline findings and zero new
ones. Clippy still reports nine existing runtime diagnostics (`bliss-5vr`),
spec coverage has 14 uncovered stage-5 requirements (`bliss-kjjd`), and existing
rustfmt drift remains (`bliss-2uj1`); none of its diagnostics target new code.

Several tempting alternatives were measured and discarded in this iteration:
static multiple-value classifier shortcuts and function-cell checks each saved
less than 1% (`bliss-amyr`); keyword-only direct-slot binding saved 0.33%
(`bliss-8cko`); allowing cold restart instructions to deopt promoted
`MAKE-ACTION-STATUS` to T1 but saved only 0.7% (`bliss-yun1`). Blanket invocation
uncommon traps broke bootstrap with an unbound `TESTFN` (`bliss-r03w`). Removing
the runtime destructuring-lowering restriction still left the observed ASDF
functions uncompiled (`bliss-jx2n`). All those probes were removed. Lisp-leaf
sample percentages were not removable dispatch costs.

Refresh the now-stale load-only profile next (`bliss-ixj5`). The remaining
external-root scanning lead is `bliss-399z`. The previous phase
probe's 33 ms marking and 33 ms relocation are attribution leads, not a proven
optimization or a reason to skip root scans.

## Reuse minor-GC evacuation destinations (2026-09-24, bliss-b03z)

A throwaway action-timing probe ruled out a large remaining binary-decoder win.
Alexandria's source-only `types` and `numbers` FASLs cost about 44 ms combined;
ASDF's source definitions cost about 114 ms. Babel's major initializers already
execute compiled load thunks. The 426 ms `enc-jpn` load includes the 267 ms minor
GC, rather than representing 426 ms of Lisp execution. Missing numeric literal
formats are tracked in `bliss-ujsc`, not treated as the primary remaining gap.

The GC probe found **346,384 live copies and 317,068 failed initial copy
attempts**. After the first survivor region filled, the collector kept trying
that same full region for every object, then searched all regions for another
destination. The copy phase alone took 101 ms. The collector now retains its
current survivor and old-generation destinations until they fill. Object-size
checks, pinned-host exclusion, root relocation, and collection timing remain
unchanged. All throwaway profiling instrumentation was removed.

Five alternating CPU-0-pinned release runs, each a fresh process loading an
already populated build-specific FASL cache, against `ff97468`:

| Median | Before | Destination reuse |
|---|---:|---:|
| Initial Babel FASL load | 1.476 s | **1.410 s (4.5% less)** |
| SBCL initial load, same batch | 0.287 s | 0.287 s |
| TorCL / SBCL | 5.1× | **4.9×** |
| Minor-GC pause during load | 0.258 s | **0.196 s (23.8% less)** |
| Initial-load retired instructions (three isolated windows) | 7.090 G | **6.422 G (9.4% less)** |
| Whole-process retired instructions | 20.349 G | 19.687 G |

Both versions collect exactly once (minor, not major) inside the timer. Load
samples span 1.255–1.489 s before and 1.267–1.421 s after. Whole-process wall
time, including ASDF startup, was especially noisy: medians 3.62 s before and
3.88 s after, despite fewer instructions. Do not infer a startup speedup from
this batch. The initial-load improvement is incremental; parity is not close.
The instruction-window repeats vary by less than 0.1% per binary. CLI tests
were still active during that count-only check, so its wall timings are not
used; the five-run wall-time batch above ran before workspace validation.

The deterministic regression first failed at 2,001 searches for 2,000 conses.
It now bounds searches by the number of destination regions, checks every
list element, and covers both survivor copying and immediate promotion.
The full runtime suite passes all 621 tests. Normal execution and
`TORCL_GC_STRESS=20000 TORCL_GC_POISON=1 TORCL_GC_VERIFY=1` agree with SBCL on
all 15,106 Babel reverse-table entries. An every-allocation stress/poison/verify
probe also preserves source-free native-cons results. Root lint has no new
findings. Review is adversarial self-review, not independent.

The complete workspace gate records **2,507 passed, eight failed, seven
ignored**, split into serial non-CLI tests and process-isolated CLI tests with
eight workers. All 357 acceptance and 48 FASL tests pass. The eight failures
are the tracked baseline compiler STRINGP metadata test (`bliss-4ihx`), four
sequence fixtures (`bliss-0bdl`), two Lisp-containing doctests (`bliss-pqyy`),
and asynchronous tier-observability assertion (`bliss-ugmu`). Workspace check
passes with the existing unused-mut warning (`bliss-d3hs`). Clippy still reports
nine existing runtime diagnostics (`bliss-5vr`); spec coverage has 14 uncovered
stage-5 requirements (`bliss-kjjd`). Widespread existing rustfmt drift is tracked
separately in `bliss-2uj1`.

The pre-change phase probe also measured roughly 60 ms indexing nursery
objects, 33 ms marking external roots, and 33 ms relocating external roots.
These are further profiling leads, not proven optimizations. The analogous
per-object major-GC destination search is filed separately as `bliss-2f9l`;
major GC is not on this initial-load path.

## Remove repeated runtime work from first FASL load (2026-09-23)

The next profile led to three general changes (`bliss-djhf`, `bliss-vbar`,
`bliss-7pyh`):

- Emit native `AllocCons` instructions, so portable quasiquote bytecode no
  longer disqualifies an entire function/loop from T1 or OSR.
- Remove T1's obsolete whole-function purity gate. Guards already resume at
  the exact failing bytecode instruction, so arithmetic after an impure call
  can speculate without replaying that call. Tests check actual deoptimization
  and exact side-effect counts through both invocation promotion and OSR.
- Borrow class metadata during read-only slot-owner traversal, instead of
  cloning every visited class, its slots, and its default initargs. The cycle
  set also borrows class names. There is no new cache or invalidation policy;
  instance-slot shadowing and the existing missing-graph fallback are preserved.

Three repeated isolated-load measurements against `89bab35`: median **7.769 G →
7.088 G instructions**, **8.8% less**. Native cons plus the purity-gate removal
alone measured 7.593 G; borrowing class metadata accounts for most of this
iteration's improvement. These are fresh processes loading existing FASLs;
source-compilation runs are excluded, and one minor GC remains inside the timer.

Five alternating CPU-0-pinned release runs, after all test processes finished:

| Metric (median) | `89bab35` | This change |
|---|---:|---:|
| Initial Babel FASL load | 1.628 s | **1.475 s (9.4% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 5.7× | **5.2×** |
| Whole-process wall time, including ASDF startup | 4.07 s | 3.96 s |

Baseline load samples ranged 1.383–1.632 s, candidate 1.474–1.490 s, SBCL
0.285–0.287 s. Both TorCL versions collected once during each timed load;
candidate GC time was about 0.260 s versus 0.255 s baseline, so the gain did
not come from deferring collection. This is incremental progress, not parity.

Rejected experiments matter for the next session:

- Lowering named OSR thresholds to 200: 7.732 G instructions versus 7.767 G
  default. Anonymous threshold 10: 7.757 G; anonymous OSR disabled: 7.759 G.
  Earlier promotion is not a large remaining lever in this workload.
- Enabling OSR uncommon traps: 7.655 G, also small.
- Native `MakeClosure` added only about 1% beyond native cons. That prototype
  was removed before landing: baked body pointers need an ownership audit
  across self-redefinition and direct native calls (`bliss-sg8w`, `bliss-ku9w`).
  A passing Lisp redefinition probe did not establish ownership safety.

A fresh frame-pointer profile (1,542 load-only samples) puts class-slot lookup
at 1.4% inclusive, down from roughly 6%. Remaining sampled costs include minor
GC 17%, source-reading/evaluation 10.7%, extended LOOP evaluation 6.8%,
multiple-value classification 5.8%, and variadic binding 4.7%. These overlap;
do not sum them. Constant materialization is only about 1.6%. Next leads are
remaining source-evaluated initializers (`bliss-mbzt`) and redundant
multiple-value classification (`bliss-amyr`), not a binary-decoder rewrite.

The real Babel load under `TORCL_GC_STRESS=20000 TORCL_GC_POISON=1
TORCL_GC_VERIFY=1` matches normal execution and SBCL for all 15,106 entries in
the two reverse tables. The focused final suites pass: 48 FASL tests, five OSR,
14 T1-deoptimization tests (one ignored), and five T1-native tests. Root lint
has zero new findings. Review was adversarial self-review, not independent.

The final workspace test run recorded **2,506 passed, eight failed, seven
ignored**. All 357 CLI acceptance tests pass. Failures remain the same tracked
baseline issues listed in the preceding iteration: `bliss-4ihx`, `bliss-0bdl`,
`bliss-pqyy`, and `bliss-ugmu`. Workspace check passes with the existing warning
(`bliss-d3hs`); Clippy still stops at nine runtime diagnostics (`bliss-5vr`),
and spec coverage at 14 uncited stage-5 requirements (`bliss-kjjd`).

## Compile encoding-table initializers (2026-09-23, bliss-bd8a)

The next initial-load profile isolated `asdf:load-system` with a `perf` control
FIFO, excluding ASDF startup. About 25% of samples were source evaluation inside
the FASL loader. Action timing identified two expensive reverse-table builders:
`+UNICODE-TO-JIS-X-0208+` and `+UNICODE-TO-KSC-5601+`. Their `LOOP` forms fell back
to `EvalSource` because the bytecode compiler lacked `ACROSS` and `OF-TYPE`
iteration clauses. Binary decoding was not the bottleneck.

Five alternating CPU-0-pinned release runs, each in a fresh process with its
build-specific FASL cache already populated, against saved baseline `66c6d4f`:

| Metric (median) | Before | Compiled initializers |
|---|---:|---:|
| Initial Babel FASL load | 1.964 s | 1.631 s (**17.0% less**) |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| TorCL / SBCL | 6.9× | **5.7×** |
| Initial-load retired instructions (three isolated windows) | 9.569 G | 7.761 G (**18.9% less**) |
| Initial-load allocation | 32.56 MB | 26.83 MB (**17.6% less**) |
| Whole-process retired instructions | 22.975 G | 21.177 G |
| Whole-process wall time, including ASDF startup | 4.26 s | 4.14 s |

No measured run compiled source. Both versions still perform **one minor GC**
during the timed load; collection was not deferred or moved outside the timer.
Wall time remains noisy even when pinned: baseline load samples ranged from
1.775–1.968 s, candidate 1.185–1.645 s, SBCL 0.186–0.288 s. Compare within this
batch, not against the earlier entry's absolute times. The three isolated
instruction measurements reproduce within 0.2% for each binary.

The compiler now lowers those clauses into portable bytecode. `ACROSS` evaluates
its vector and active length once, reads elements as the loop advances, and
supports strings, bit vectors, fill pointers, and multiple iteration drivers.
`OF-TYPE` is accepted as an unspecialized declaration. This is general compiler
support, not a Babel-specific cache or a change to when load-time work occurs.

GC stress exposed three correctness bugs, fixed alongside the optimization:

- Recursive `LOOP IT` substitution must root both the source tail and rebuilt
  head. Losing either could silently force source fallback during compilation.
- The reader must zero packed bit-vector payloads before OR-ing in one bits.
  Poisoned/reused nursery bytes otherwise changed `#*101` into `#*111`.
- Closure construction must root its cloned bytecode body **before** allocating
  the installed lambda list. Otherwise moved literal constants become stale
  before registry installation. A stride-7 regression catches this; collection
  on every allocation happened to promote the literal early and miss it.

Validation includes source-free FASL action assertions, deleting source before
load, every-allocation GC/poison testing, and a real Babel load with
`TORCL_GC_STRESS=20000 TORCL_GC_POISON=1 TORCL_GC_VERIFY=1`. All 15,106 entries in
the two reverse tables match SBCL, and stressed output matches normal output.

The workspace test run (non-CLI serial, CLI four threads) recorded **2,502 passed,
eight failed, seven ignored**. The failures are separately tracked baseline
issues: compiler STRINGP metadata (`bliss-4ihx`), four invalid sequence fixtures
(`bliss-0bdl`), two unlabelled Lisp rustdoc examples (`bliss-pqyy`), and a
40-call asynchronous tier-promotion assertion (`bliss-ugmu`). The latter also
fails on the saved baseline in all five control runs, with correct Lisp values.
All 47 FASL tests pass. Root lint has zero new findings; workspace check passes
with the existing unused-mut warning (`bliss-d3hs`). Clippy remains blocked by
existing runtime lints (`bliss-5vr`), and spec coverage by 14 uncited stage-5
requirements (`bliss-kjjd`).

The next larger hypothesis is selective native execution of one-shot FASL
initialization loops (`bliss-y625`): ordinary hotness thresholds may not pay off
before a load-time loop finishes. Profile the new load before changing policy,
and include compilation cost in the first-load timer. Class-metadata cloning
(`bliss-7pyh`) accounted for about 5% of the pre-change initial-load profile;
it is not an explanation of the entire remaining gap.

## Initial FASL load update (2026-09-23, bliss-w337)

The active target is now the **first** `asdf:load-system` in a fresh process,
with already-compiled FASLs, not no-op reloads. Five alternating CPU-0-pinned
release runs against saved baseline `94f7570`, with SBCL in the same batch:

| Metric (median) | Before | Nursery bitmaps |
|---|---:|---:|
| Initial Babel FASL load | 2.428 s | 1.686 s (**30.6% less**) |
| SBCL initial load | 0.247 s | 0.247 s |
| TorCL / SBCL | 9.8× | **6.8×** |
| Minor GC during initial load | 0.925 s | 0.234 s |
| Whole process, including ASDF startup | 4.67 s | 4.08 s |
| Whole-process retired instructions | 24.585 G | 22.976 G |
| Peak process RSS | 393 MiB | 168 MiB |

Both versions perform one minor GC during the timed load; neither changes the
nursery size or moves collection outside the timer. Every run returns
`BABEL-CHECK #(72 101 108 108 111)`, with no source compilation in the measured
runs. Whole-process instructions are **not** isolated load-phase instructions.

The structural change replaces the minor collector's per-object address
`HashMap` and live-object `HashSet` with two compact bitmaps. A region-offset
table packs only nursery regions: 64 MiB of nursery needs 1 MiB of bitmap
storage. One bitmap records exact object starts, the other liveness; interior
addresses and non-nursery references are still rejected. Marking reads layout
from intact object headers before evacuation rather than duplicating metadata
for every nursery object. Persistent forwarding is resolved before marking.
This removes hash-table construction and random lookups, not necessary GC work.

Reproduce the load phase from the repository root with this shared script:

```lisp
#+sbcl (require :asdf)
#+torcl (load "lib/asdf.bfasl")
(asdf:initialize-source-registry
 `(:source-registry (:tree ,(truename "ocicl/")) :inherit-configuration))
(time (asdf:load-system :babel))
(format t "~&BABEL-CHECK ~S~%"
        (babel:string-to-octets "Hello" :encoding :utf-8))
```

Use `scripts/torcl-limited.sh taskset -c 0` for each process. TorCL uses
`--no-init --load`; SBCL uses
`--noinform --no-sysinit --no-userinit --non-interactive --load`. Populate each
binary's build-hash-specific FASL cache first, then alternate saved baseline and
candidate binaries. `/usr/bin/time` and `perf stat -e cpu_core/instructions/u`
around the process measure the whole-process columns.

The remaining gap is real. The historical leads below remain context, not a
fresh attribution of the remaining initial-load time.

## 1. Earlier baseline (before the initial-load follow-up)

Benchmark: `(asdf:load-system :babel)` — one cold load plus ten no-op re-loads,
from fasls, `--no-init`.

```
start of investigation   ~36.8 s
now                      ~14.4 s
```

Against SBCL 2.6.8 on the same machine, alternating runs, both warm from fasls:

| | torcl | SBCL | ratio |
|---|---|---|---|
| cold load from fasl | 3.15 s | 0.31 s | ~10x |
| no-op re-load (each) | 0.89 s | 0.0046 s | **~193x** |

Cold loading is no longer the outlier. **The gap lives in the no-op re-load**,
which is pure ASDF traversal with nothing to install.

Method tier split over a load went 16.5% → 96.9% of method invocations compiled.
That seam is essentially closed; do not expect more from it.

## 2. How to measure in this codebase

**Guessing from source has failed roughly seven times in a row here.** Every
significant find came from a tool. The traps are written up in
`measuring-performance.md` §6a–§6e; the essentials:

- **perf cannot unwind the release binary.** Build with
  `-C force-frame-pointers=yes` and use `--call-graph fp`. `--call-graph dwarf`
  does not work here. `--comms` takes the BINARY name, so a renamed copy matches
  nothing — silently, reporting zero rows rather than erroring.
- **Pin to a P-core.** This is a hybrid CPU; an unpinned run lands on E-cores,
  reports `cpu_atom/cycles`, and collects far fewer samples. `taskset -c 0`.
- **perf only gets you to the inlined caller.** For the real site, break in gdb.
  You need real DWARF, and `RUSTFLAGS="-C debuginfo=2"` does NOT produce it —
  use `CARGO_PROFILE_RELEASE_DEBUG=2`. Then `break <mangled symbol>`,
  `ignore 1 <N>` to skip startup, `continue`, `bt`.
- **Wall clock on this machine cannot resolve anything under ~5%.** Load average
  swings results by 15%+. Use `taskset -c 0 perf stat -e cpu_core/instructions/u`
  — retired instructions are near-deterministic here and reproduce to five
  digits. Confirm direction with wall clock only when the box is quiet.
- **Discard the first run of a freshly built binary.** It is a cold page-cache
  artifact and looks like a large regression or win. Also note each binary has
  its own fasl cache keyed by build hash, so a first run may recompile babel.
- **A few gdb samples are not a distribution.** This one bit me: two samples both
  hit macro-environment cloning, I called it "what memcpy is doing", fixed it,
  and memcpy did not move at all (12.3% → 12.87%). Tally at least a dozen.
- **Benchmark the workload you are trying to improve.** A per-call microbenchmark
  and a no-op ASDF re-load exercise almost disjoint code here. Three large
  per-call wins moved the re-load by nothing (§4).

## 3. The current re-load profile

Frame-pointer build, `taskset -c 0`, 40 no-op re-loads, mutator self time:

```
12.87%  memcpy                      <- largest single entry, SOURCE UNKNOWN
 5.97%  cli::eval_list              <- still TREE-WALKING during the traversal
 2.57%  cli::symbol_bare_slice
 2.31%  core::hash::sip::Hasher
 2.29%  __rustc::__rust_dealloc
 2.29%  memcmp
 2.25%  symbols::find_index
 2.21%  __rustc::__rust_alloc
 2.11%  gc major_gc closure
 1.62%  hashbrown HashMap::insert
 1.60%  __libc_malloc_impl
 1.60%  cli::accessor_slot_name
```

`run_loop` is NOT in this list. That is the single most important fact about the
re-load: it is not dominated by compiled call dispatch, which is why per-call
work does not move it.

## 4. The pattern to internalise before optimising

Four fixes, each a large win on a call benchmark and worth ~nothing on the load:

| change | its own benchmark | babel |
|---|---|---|
| `8faef9b` block registration | per-call −45.7% | −0.07% |
| `04aae0e` operator classifiers | per-call −12.1% | −0.8% |
| `6a283ac` closure registry hash | closures −11.8% | ~0 |
| `bliss-ccso` call-next-method tiering | 13.2x on a real body | 0 |

Each was a genuine defect worth fixing. None of them was a *load* fix. Before
starting anything, profile the re-load and confirm your target appears in it.

## 5. Open leads, in profile order

1. **memcpy, 12.87%.** The largest entry and still unattributed. Ruled out:
   `Env::child_with_parent` (clones ten collections per scope but is 0.03% of the
   re-load) and the global macro `Environment` clone (fixed; memcpy unchanged).
   Sample properly — a dozen-plus gdb backtraces, tallying frames #6–#9 — before
   forming a theory.
2. **`eval_list` at 5.97% SELF.** Something substantial in the ASDF traversal is
   still tree-walked rather than compiled. Find out what and why it never
   promotes. The tier-split probe recipe is in `method-tiering.md`.
3. **The symbol/name cluster, ~11% combined**: `symbol_bare_slice` 2.6 +
   `find_index` 2.3 + memcmp 2.3 + SipHash 2.3 + `HashMap::insert` 1.6. Tracked
   as bliss-qerr. A NEGATIVE result is recorded there: borrowing instead of
   cloning `package_use_list` changed nothing, because the cost is the RwLock
   acquisition and hash lookup, not the copy.
4. **Closure construction, ~5 µs — about 14 function calls** (bliss-gajx). A
   closure is not a GC object; it is parked in a process-wide
   `HashMap<u64, Closure>` that the collector must scan, and ~33% of closure
   construction time is GC. Pruning sooner is *worse* (measured: 7.1 → 11.8 →
   34.7 µs/call as the prune floor drops), so that knob is already right. The
   structural fix is making closures first-class GC heap objects. Relevant to
   ASDF, whose hot methods build a closure per call via `do-asdf-cache`.

## 6. Things already tried that did not work

Recorded so they are not repeated:

- **Caching T2 compiler output** (bliss-eoma). Prize is now ~1.6% of a load: T2
  costs 1.07 G instructions to compile and returns 3.16 G, i.e. it is a net 3%
  WIN. It began this investigation as a 42% penalty; the change was aaeb60a.
  See `code-cache.md` §8d — that section's claim has been wrong three times in
  three different directions, so re-measure before believing any of it.
- **Integer control tokens** (bliss-taqn, closed). Ceiling ≤7% on
  catch/handler-case-heavy code and ~0 elsewhere, against a blast radius that
  includes a new `TorclError` variant, because tokens ride through errors as
  strings with prefix parsing.
- **Replacing the allocator wholesale** (bliss-05as). mimalloc will not build for
  musl here; dlmalloc builds and is 8% SLOWER. What worked was adding the one
  thing musl lacks — a per-thread free-list cache — which is now default-on for
  musl and was worth 28.7%.
- **Lowering `TORCL_CLOSURE_PRUNE_FLOOR`** — strictly worse, see §5.4.

## 7. Known-flaky / pre-existing failures

Do not spend time on these; both reproduce without any local change:

- `cli::bytecode::jtc4_stack_map_tests::run_native_rewrites_stack_guard_sigsegv_to_stack_overflow`
  — load-sensitive, ~2.5% in the full gate, 0/10 standalone. Tracked as
  bliss-zzty, which also fixed three real cross-thread signal-delivery races
  found while chasing it. An early 0/15 baseline made this look like a
  regression; it was sampling noise. Use n>=40 for flaky baselines here.
- `torcl-compiler --test t2_integration stringp_reaches_string_typecheck_through_inline_metadata`
  — fails at HEAD with everything stashed.

## 8. Useful knobs

```
TORCL_DISABLE_T2=1            no T2
TORCL_T2_DISCARD=1            compile at T2, install nothing  (separates compile
                              cost from emitted-code cost)
TORCL_T2_NO_QUEUE=1           snapshot for T2, never queue    (isolates the
                              mutator-side snapshot)
TORCL_BAIL_TRACE=1            record why the lowerer bailed; read with
                              (torcl-ext:bail-report)
TORCL_DIRECT_BUILTIN_STATS=1  direct-builtin hit/fallback counts
TORCL_GC_STRESS=1 TORCL_GC_POISON=1   mandatory for anything that allocates
```

The bail reporter now reports the reason from the attempt that actually decides,
and the innermost cause rather than the outermost wrapper — it used to report
neither, which hid `bliss-ptv4` behind a wrong answer for a while.
