# bordeaux-threads on Bliss

A working port of [bordeaux-threads](https://github.com/sionescu/bordeaux-threads)
(0.9.4) to Bliss. The classic v1 API (`bt:`) loads and runs: `make-thread` /
`join-thread` (offload + join), `make-lock` / `with-lock-held`, semaphores,
`threadp` / `current-thread` / `thread-name`.

Verified end to end:

```lisp
(asdf:load-system :bordeaux-threads)
(defun sq3 () (* 3 3)) (defun sq4 () (* 4 4)) (defun sq5 () (* 5 5))
(mapcar #'bt:join-thread
        (list (bt:make-thread #'sq3) (bt:make-thread #'sq4) (bt:make-thread #'sq5)))
;; => (9 16 25)
(let ((l (bt:make-lock))) (bt:with-lock-held (l) 42))       ; => 42
(bt:join-thread (bt:make-thread (lambda () (+ 40 2))))      ; => 42
```

## Execution model and limits

Bliss runs one interpreter thread at a time safely — the **worker-offload +
join** model: a spawned worker runs while the spawner blocks in `JOIN-THREAD`.
In that model the lock / condition-variable / semaphore primitives are correct
as **no-ops** (there is never a second interpreter thread mutating shared state
to exclude). True shared-memory parallelism (two interpreter threads allocating
concurrently) is still a work in progress in the Bliss runtime (see
`bliss-bw3t`), and a thread function that closes over **lexical** variables finds
them unbound on the worker (`make-thread` reifies a lambda, so a body that
references only globals and its own parameters runs — `bliss-nubv`).

## Contents

- `apiv1-impl-bliss.lisp` — the v1 backend (installed as
  `ocicl/bordeaux-threads-0.9.4/apiv1/impl-bliss.lisp`).
- `apiv2-impl-bliss.lisp` — the v2 backend (installed as
  `ocicl/bordeaux-threads-0.9.4/apiv2/impl-bliss.lisp`). Note: the v2 (`bt2:`)
  `make-thread` still hits a Bliss macroexpand issue; the v1 API is the working
  surface.

## Applying the port (the `ocicl/` tree is gitignored)

1. Copy `apiv1-impl-bliss.lisp` → `ocicl/bordeaux-threads-0.9.4/apiv1/impl-bliss.lisp`
   and `apiv2-impl-bliss.lisp` → `ocicl/bordeaux-threads-0.9.4/apiv2/impl-bliss.lisp`.
2. Patch `ocicl/bordeaux-threads-0.9.4/bordeaux-threads.asd`:
   - add `bliss` to the `(pushnew :thread-support *features*)` feature list;
   - add `(:file "impl-bliss" :if-feature :bliss)` to both the `api-v1` and
     `api-v2` module component lists (after the `impl-abcl` entry).
3. Patch `ocicl/bordeaux-threads-0.9.4/apiv2/atomics.lisp`: add `#+bliss`
   branches to `atomic-cas` / `atomic-decf` / `atomic-incf` (plain, single-
   threaded — a CAS is `(when (eql place old) (setf place new) t)`; incf/decf are
   ordinary), add `bliss` to each `#-(or …)` guard, and add `bliss` to the
   `atomic-integer` `cell` slot's `#+(or …)` group.
4. Patch `ocicl/trivial-garbage-.../trivial-garbage.lisp`
   (`weakness-keyword-opt`): add `bliss` to the fallbacks so weak hash-tables
   degrade to ordinary (strong) tables instead of signalling
   "Your Lisp does not support weak … hash-tables."

## Bliss runtime fixes this port depended on

Landed in the Bliss tree (not here):
- Compiled-macro `&whole` now binds the whole call form including the operator
  (was dropping the operator, breaking global-vars' `define-global-var*`).
- `*TYPE-DEFINITIONS*` / `*CONDITION-TYPES*` / `*CONDITION-DEFINITIONS*` seeded in
  the symbol value cell so compiled `deftype`/`define-condition` see them.
- `MAKE-THREAD` reifies a lambda so non-capturing closures run on a worker.
- `JOIN-THREAD` marks the joiner Blocked so a peer's GC can proceed.
