# Matched forwarding-entry control

Use `ordinary_bridge.py --control forwarders` with retained release artifacts
from before default tail adoption. Both counted loops enter an environment-free
four-argument **forwarder** through the same outer runtime adapter:

```text
one: loop -> runtime entry -> ONE -> ordinary LEAF -> loop
two: loop -> runtime entry -> TWO -> ordinary ONE -> ordinary LEAF -> loop
```

`ONE` calls the identity leaf; `TWO` calls `ONE`. Each forwards `0`, `1`, `2`,
and the loop index unchanged. All three functions are `NOTINLINE`. Both loops
perform identical fixnum addition and return `n*(n-1)/2`; at one million
iterations the checksum is `499999500000`. The workload requests no Lisp heap
allocation in either loop on the intended 64-bit hosts. This is not an assertion
that runtime adapters perform zero internal allocation.

The direct-leaf versus forwarder workload remains an **absolute regression
control**. Restoring the leaf's managed entry changes its outer adapter choice;
subtracting that direct-leaf result therefore does not isolate an ordinary call.
The new pair keeps both outer targets as ordinary-only forwarding entries.

```sh
python3 tests/benchmarks/ordinary_bridge.py --control forwarders \
  --before-root /tmp/bridge-before --before-run RUN --before-artifact ARTIFACT \
  --after-root /tmp/bridge-fixed --after-run RUN --after-artifact ARTIFACT \
  --build-description 'retained matched CI release configuration and build logs' \
  --cpu 4 --iterations 1000000 --samples 9 --trials 5 \
  --output /tmp/chain-results
```

The shared runner verifies manifest hashes and release configuration, warms both
paths in each process, alternates binary and pair order, checks every checksum,
and retains named pre/post listings and T2 logs. This control reports `one_ns`,
`two_ns`, and `increment_ns` rather than relabeling them direct/forward. Its
parser requires ordinary ABI listings for `LEAF`, `ONE`, and `TWO` on the after
artifact. For both forwarders it also requires the ordinary-only offset-zero trap and
the initial ABI's fast
`call r11`, control reload, and depth-decrement sequence. A cold helper call or
a tail jump cannot alone satisfy that check. Default-tail artifacts must be
measured separately; the parser deliberately rejects their different path.

Review the retained listings before measuring: each forwarder's named ordinary
entry must load its target descriptor, validate revision/admission, marshal
RDI/RSI/RDX and overflow argument four with RCX count, and preserve the returned
tuple/CF. Check the loops' hot entry/body and OSR call sequences for matching
outer dispatch; differing cold deoptimization stub order is not a hot-path
change. The matching-artifact warmed-edge test proves zero cold handoffs for
an equivalent four-argument edge, not for every invocation in a timed process.
Neither code listings nor successful checksums alone prove all measured calls
used the fast path. Keep that limitation explicit.

The increment includes the additional forwarder's admission guards, argument
movement, depth/stack bookkeeping, and return. It is not machine `CALL` latency.
Only compare quiet, repeated, paired samples after path verification; retain
negative/noisy increments and report distributions. The initial correctness
smoke is not a performance result and does not amend the existing bridge report.

Run `python3 tests/benchmarks/test_ordinary_bridge.py` for both parser modes.
`ordinary-chain-workload.lisp` itself is portable Common Lisp; the timed driver
uses EGCL's nanosecond clock. SBCL may tail-call the chain despite `NOTINLINE`,
so its checksum smoke establishes portability, not an equivalent call sequence.

The [first quiet matched comparison](results/2026-10-12-ordinary-chain.md)
retains all absolute and paired distributions. It uses fixed pre-tail release
artifacts and keeps the small residual increment's noise explicit.
