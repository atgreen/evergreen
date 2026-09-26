# TorCL support for fset

Fork: <https://github.com/atgreen/fset>, commit
`60a28fe91abfddfc8c2a3f6971933a0c2c5b20aa` (on `master`, one commit on top of
upstream `1af9d20`). Install it through an ocicl `git+URL@SHA` pin as described
in [../README.md](../README.md); do not edit the downloaded ocicl copy.

## Why the port is needed

`Code/port.lisp` defines `make-lock`, `with-lock`, `read-memory-barrier` and
`write-memory-barrier` under a **closed** list of per-implementation reader
conditionals — allegro, lispworks, cmu, sbcl (both `sb-thread` and not), clasp,
scl, openmcl, genera, clisp, ecl, abcl — with no `#-(or …)` fallback. TorCL
matches none of them, so all four were undefined and loading `Code/tuples.lisp`
died at `(make-lock "Tuple Key Lock")`. fset arrives via mgl-pax → dref, so this
blocked anything depending on it (`bliss-q00o`, blocking `bliss-tak0`).

## Two things that are not the obvious mapping

- **`with-lock` does not use `TORCL-THREAD:WITH-MUTEX`.** That macro always
  blocks and accepts no `:wait-p`, but `with-lock`'s contract is that
  `:wait? nil` returns *without evaluating the body* when the lock is held. The
  port acquires with `GRAB-MUTEX`, which returns `NIL` rather than signalling
  when a non-blocking acquire fails — verified under real thread contention —
  and releases under `UNWIND-PROTECT` so a non-local exit cannot leak the lock.
- **The memory barriers are a lock round trip, not `nil`.** fset uses them
  around lock-free reads of its transient structures and TorCL has a moving
  collector, so stubbing them out would be a correctness risk rather than a
  simplification. This follows the Allegro/LispWorks/Clasp precedent already in
  that file.

`make-lock` coerces its argument with `string`, as the SBCL branch does:
`MAKE-MUTEX` takes the name as a keyword, and fset passes a string from
`tuples.lisp` and a symbol from `define-atomic-series`.

Also adds TorCL's `make-char`, matching the SBCL/Clasp/ECL definition, which
fset's own test suite uses to generate random characters.

## Verification

The change is purely additive and entirely inside `#+torcl`, so no other
implementation's behaviour changes — confirmed by loading fset and a dependent
test system on **SBCL** with the fork in the registry. On TorCL it clears
`FSET::MAKE-LOCK` and the load proceeds past fset.
