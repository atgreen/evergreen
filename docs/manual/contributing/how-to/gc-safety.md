# Keep values safe across GC

EGCL's minor collector is precise and moving. Apply these rules to every path
that allocates, evaluates Lisp, interns a symbol, constructs an error, or calls
another function that may do so.

## Root values before the next allocation

Use an owning root for a new local:

```rust
egcl_rt::rooted!(value = eval_form(expr, env)?);
// Later allocations may update *value through the registered root.
```

To root an existing local or a supported aggregate in place:

```rust
egcl_rt::rooted_ref!(_roots = &mut values);
```

Keep the guard alive for as long as the value must survive allocations.
A copied `EgclVal` in a Rust local, register, or ordinary `Vec` is not by itself
visible to the collector. Avoid new uses of the legacy globally locked root
primitives; the intrusive root macros are the current approach.

## Release borrows before allocating

Do not hold a `RefCell` borrow, or an exclusive raw reference, to GC-scanned
state across an allocation. Collect the information you need, end the borrow,
and only then allocate. A root scanner may re-enter that state during collection.
A double borrow reached through an `extern "C"` bridge can abort the process.

## Run a stress probe

After building the affected binary, run a small reproducer normally and with
stress plus poisoning:

```sh
scripts/egcl-limited.sh target/x86_64-unknown-linux-musl/debug/egcl \
  --no-init --load reproduce.lisp
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 \
  scripts/egcl-limited.sh target/x86_64-unknown-linux-musl/debug/egcl \
  --no-init --load reproduce.lisp
```

Compare the output as well as the exit status. A value can be lost before it is
stored and produce a quietly wrong result instead of a poisoned-pointer crash.

On Linux, once the old generation reaches a quarter of the nursery's size,
minor collections can scan only the old-generation pages the kernel reports as
written since the previous collection. Auto mode prefers asynchronous write
protection of older heap ranges when the kernel supports it, with legacy
process-wide soft-dirty tracking as fallback. Unsupported or uncertain tracking
retains a full old-generation scan.

A small reproducer stays below that size. Set `EGCL_GC_SOFT_DIRTY=1` to force
legacy soft-dirty tracking even for a small heap; `EGCL_GC_SOFT_DIRTY=0` forces
full scanning. Presence of `EGCL_DISABLE_ASYNC_OLD_TRACKING` keeps Auto mode on
the legacy backend. `EGCL_GC_ASYNC_WP_LOG=1` reports successful async arming and
snapshots or fallback; use those markers to prove which backend ran. Keep this
diagnostic logging disabled for performance measurements.

A change that adds a new kind of off-heap storage reachable from a heap object
must be traced on every minor collection, not only when its holder's page is
written: see `traces_off_heap` in `crates/egcl-rt/src/gc.rs`. To tell a
scanning fault from a rooting fault, rerun with `EGCL_GC_SOFT_DIRTY=0`, which
scans the whole old generation, and with `EGCL_GC_VERIFY=1`, which checks after
every minor collection that no older object still refers into the nursery.
`EGCL_GC_REGION_LOG=1` prints each minor collection's scan mode, the regions it
walked and the young references it found.

## Read the failure correctly

| Result | Meaning |
| --- | --- |
| Exit 137 and the wrapper's memory-limit note | Cgroup memory cap hit |
| Exit 124 | Wall-clock timeout |
| Exit 139, exit 134, or an “already borrowed” panic | Investigate corruption or borrow/rooting discipline |

`EGCL_GC_STRESS_SKIP=N` can narrow a reproducer to a suffix of allocations.
First confirm that N is within the program's allocation count with
`EGCL_GC_STRESS_AT=N`: an out-of-range skip disables stressing and proves
nothing. See the full debugging instructions in
[AGENTS.md](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/AGENTS.md).
