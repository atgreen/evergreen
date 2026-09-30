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
