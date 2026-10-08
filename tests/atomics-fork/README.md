# Atomics on EGCL

This scenario fetches the `atgreen/atomics` EGCL CAS adapter and its dependencies
using immutable ocicl pins. Downloaded library sources are never patched.

```sh
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
  bash scripts/test-atomics-fork.sh
```

Requires ocicl with Git-source support and its installed runtime; override
`OCICL_BIN` and `OCICL_RUNTIME` when necessary. Every process uses the memory and
time limits in `scripts/egcl-limited.sh`. Logs and caches stay in the printed
temporary directory under `target/`.

The fork's checks cover successful/failed CAS on supported places, Boolean return
values, evaluation order, dynamic bindings, symbol macros, and a four-thread CAS
counter. The driver also loads `cl-cancel` and calls its CAS-dependent lazy
atomic-state initialization. It does not exercise the timer worker or deadlines.
Both cold and cached ASDF loads must pass; the cached run must not compile source.
The checks run under interpreter, T0, T1, and T2 execution settings. Those settings
do not by themselves prove that every library function promoted to a native tier.

The adapter supports CAS on cons cells, simple vectors, symbol values/plists,
structure slots, CLOS slots, and special variables. Atomics' `ATOMIC-INCF` and
`ATOMIC-DECF`, custom CAS places, and raw memory references are not supported.
Portable retry helpers are not certified by these checks. The runtime must
include the extension macro lookup correction tracked as `bliss-o2deg`.

Set `EGCL_PORT_STRESS=1` for an additional full-GC-stress, poison, and heap-verifier
run. It can be expensive; increase `EGCL_TIMEOUT` as needed. A timeout is a failed
check, not a passing stress result.
