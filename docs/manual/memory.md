# Memory and garbage collection

EGCL manages Lisp objects with a precise collector whose minor collections can
move objects. Ordinary Lisp references participate in the collector's root
protocol. Foreign addresses and Rust locals need separate lifetime handling.

## Lisp and foreign memory

The Lisp heap holds objects such as conses, strings, and instances. The host
runtime also allocates Rust data structures outside that heap. A Lisp consing
measurement does not account for every byte allocated by the process.

Foreign storage allocated through `egcl-ffi:foreign-alloc` is explicitly owned.
Losing the last Lisp wrapper is not a request to free storage that C may still
retain. Use `foreign-free` after the foreign lifetime ends. See
[Foreign pointers and storage](foreign.md#foreign-alloc).

## Heap size

The Lisp heap is reserved once, at startup, and does not grow while the
program runs. Its size comes from the first of these that is set:

1. `--heap-size SIZE` on the command line, for example `--heap-size 2G`.
2. The `EGCL_HEAP_SIZE` environment variable, with the same syntax
   (`512m`, `2g`, or a byte count).
3. The older `EGCL_HEAP_MB` environment variable, a whole number of MiB.
4. Otherwise, 512 MiB.

Sizes round up to whole 1 MiB regions, with a minimum of 16 MiB; a size of 0
is an error. The nursery is a quarter of the heap, up to 64 MiB.
`--nursery-size`, `EGCL_NURSERY_SIZE` and `--tlab-size` are accepted but do not
yet change it.

The reservation is committed lazily, so memory the program never uses is not
backed by RAM. The whole size must still be reservable: under Linux's default
overcommit policy a heap larger than RAM plus swap is refused, and egcl stops at
startup with "cannot reserve N bytes for the heap".

A saved world restored with `--image`, or embedded in an executable, takes the
heap size of the process that restores it, not the one that saved it. A heap
too small for the restored data grows to three times the size of that data.

When live data outgrows the heap, egcl first collects the whole heap. If the
data still does not fit, the program receives a `storage-condition` at its
next safepoint, shortly after the allocation that needed the space. The
collector moves nothing in that case, and a program that handles the condition
and drops the data can continue. Restart with a larger `--heap-size` when the
data is meant to be live.

Compiled code receives the condition too. One case can still report "invalid
native transfer capture" instead: compiled code whose own `handler-case`,
`handler-bind`, `restart-case`, `catch`, `unwind-protect` or special binding is
active when the heap runs out.

## Explicit collection

### `egcl-ext:gc` { #gc }

**Function** `(egcl-ext:gc &key full)` → `nil`

With a true `:full` value, requests a full collection. Otherwise requests a
minor collection. The Lisp entry point also processes deferred Lisp finalizers.
Unknown keywords and unpaired keyword arguments are errors.

```lisp
(egcl-ext:gc :full t)
```

Explicit collection can help make a diagnostic experiment repeatable. Calling
it frequently is not a general performance optimization.

## Finalization

### `egcl-ext:finalize` { #finalize }

**Function** `(egcl-ext:finalize object function &key dont-save)` → object

Registers a deferred finalizer for the object. `function` must be a function.
The callback is called with no arguments. Errors in a deferred callback are
reported as warnings rather than allowed to unwind the collection caller.
The callback must not retain the very object whose unreachability it is intended
to observe. Finalization is not a prompt resource-release mechanism; use
`unwind-protect` for resources with a known lexical lifetime.

The current evaluator accepts `:dont-save`, but does not implement distinct
behavior for that option. Do not infer SBCL's complete finalization/image
contract from the accepted spelling.

### `egcl-ext:cancel-finalization` { #cancel-finalization }

**Function** `(egcl-ext:cancel-finalization object)` → `nil`

Cancels the deferred finalizers registered for the object.

## Weak hash tables

`make-hash-table` accepts a `:weakness` argument naming which part of an entry
the table holds weakly: `:key`, `:value`, or `:key-and-value`. `nil` makes an
ordinary table. Any other value, including Trivial-Garbage's `:key-or-value`,
signals `program-error` rather than silently creating a strong table.

### `egcl-ext:hash-table-weakness` { #hash-table-weakness }

**Function** `(egcl-ext:hash-table-weakness hash-table)` → `:key`, `:value`, `:key-and-value`, or `nil`

Returns the weakness the table was created with, or `nil` for an ordinary
table. A non-hash-table argument signals `type-error`. Trivial-Garbage's
`hash-table-weakness` uses this function.

```lisp
(egcl-ext:hash-table-weakness (make-hash-table :weakness :key))  ; => :KEY
```

## Measurement and debugging

`room` describes Lisp heap usage; engine profiling provides collection counts
and pause totals. Host allocations require different instrumentation. See
[Profiling and efficiency](profiling.md).

For runtime contributors, a collector-visible root is required for every Lisp
value that must survive an allocating call. Do not hold a borrow of GC-scanned
state across an allocation. The complete procedure and stress settings are in
[GC safety](contributing/how-to/gc-safety.md).

Implementation reference: [GC and finalizer entry points](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl/src/cli.rs).
