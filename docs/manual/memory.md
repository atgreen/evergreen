# Memory and garbage collection

TorCL manages Lisp objects with a precise collector whose minor collections can
move objects. Ordinary Lisp references participate in the collector's root
protocol. Foreign addresses and Rust locals need separate lifetime handling.

## Lisp and foreign memory

The Lisp heap holds objects such as conses, strings, and instances. The host
runtime also allocates Rust data structures outside that heap. A Lisp consing
measurement does not account for every byte allocated by the process.

Foreign storage allocated through `torcl-ffi:foreign-alloc` is explicitly owned.
Losing the last Lisp wrapper is not a request to free storage that C may still
retain. Use `foreign-free` after the foreign lifetime ends. See
[Foreign pointers and storage](foreign.md#foreign-alloc).

## Explicit collection

### `torcl-ext:gc` { #gc }

**Function** `(torcl-ext:gc &key full)` → `nil`

With a true `:full` value, requests a full collection. Otherwise requests a
minor collection. The Lisp entry point also processes deferred Lisp finalizers.
Unknown keywords and unpaired keyword arguments are errors.

```lisp
(torcl-ext:gc :full t)
```

Explicit collection can help make a diagnostic experiment repeatable. Calling
it frequently is not a general performance optimization.

## Finalization

### `torcl-ext:finalize` { #finalize }

**Function** `(torcl-ext:finalize object function &key dont-save)` → object

Registers a deferred finalizer for the object. `function` must be a function.
The callback is called with no arguments. Errors in a deferred callback are
reported as warnings rather than allowed to unwind the collection caller.
The callback must not retain the very object whose unreachability it is intended
to observe. Finalization is not a prompt resource-release mechanism; use
`unwind-protect` for resources with a known lexical lifetime.

The current evaluator accepts `:dont-save`, but does not implement distinct
behavior for that option. Do not infer SBCL's complete finalization/image
contract from the accepted spelling.

### `torcl-ext:cancel-finalization` { #cancel-finalization }

**Function** `(torcl-ext:cancel-finalization object)` → `nil`

Cancels the deferred finalizers registered for the object.

## Measurement and debugging

`room` describes Lisp heap usage; engine profiling provides collection counts
and pause totals. Host allocations require different instrumentation. See
[Profiling and efficiency](profiling.md).

For runtime contributors, a collector-visible root is required for every Lisp
value that must survive an allocating call. Do not hold a borrow of GC-scanned
state across an allocation. The complete procedure and stress settings are in
[GC safety](contributing/how-to/gc-safety.md).

Implementation reference: [GC and finalizer entry points](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/torcl/src/cli.rs).
