# bordeaux-threads on EGCL

A working port of [bordeaux-threads](https://github.com/sionescu/bordeaux-threads)
(0.9.4) to EGCL. **Both the classic v1 API (`bt:`) and the v2 API (`bt2:`)**
load and run: `make-thread` / `join-thread` (offload + join), `make-lock` /
`with-lock-held`, recursive locks, semaphores, `threadp` / `current-thread` /
`thread-name`.

Verified end to end (v1):

```lisp
(asdf:load-system :bordeaux-threads)
(defun sq3 () (* 3 3)) (defun sq4 () (* 4 4)) (defun sq5 () (* 5 5))
(mapcar #'bt:join-thread
        (list (bt:make-thread #'sq3) (bt:make-thread #'sq4) (bt:make-thread #'sq5)))
;; => (9 16 25)
(let ((l (bt:make-lock))) (bt:with-lock-held (l) 42))       ; => 42
(bt:join-thread (bt:make-thread (lambda () (+ 40 2))))      ; => 42
```

And v2 (`bt2:`):

```lisp
(bt2:join-thread (bt2:make-thread (lambda () (* 5 5))))     ; => 25
(let ((l (bt2:make-lock))) (bt2:with-lock-held (l) 42))     ; => 42
(let ((rl (bt2:make-recursive-lock)))
  (bt2:with-recursive-lock-held (rl) (bt2:with-recursive-lock-held (rl) :ok))) ; => :OK
(bt2:threadp (bt2:current-thread))                          ; => T
(let ((s (bt2:make-semaphore :count 1))) (bt2:wait-on-semaphore s :timeout 0)) ; => T
```

## Execution model and limits

EGCL runs one interpreter thread at a time safely — the **worker-offload +
join** model: a spawned worker runs while the spawner blocks in `JOIN-THREAD`.
In that model the lock / condition-variable / semaphore primitives are correct
as **no-ops** (there is never a second interpreter thread mutating shared state
to exclude). True shared-memory parallelism (two interpreter threads allocating
concurrently) is still a work in progress in the EGCL runtime (see
`bliss-bw3t`), and a thread function that closes over **lexical** variables finds
them unbound on the worker (`make-thread` reifies a lambda, so a body that
references only globals and its own parameters runs — `bliss-nubv`).

## Contents

- `apiv1-impl-egcl.lisp` — the v1 backend (installed as
  `ocicl/bordeaux-threads-0.9.4/apiv1/impl-egcl.lisp`).
- `apiv2-impl-egcl.lisp` — the v2 backend (installed as
  `ocicl/bordeaux-threads-0.9.4/apiv2/impl-egcl.lisp`). It provides only the
  primitive SPI (threads, locks, condition variables); the portable
  `%semaphore` struct in `api-semaphores.lisp` builds semaphores on top of it.

## Applying the port (the `ocicl/` tree is gitignored)

1. Copy `apiv1-impl-egcl.lisp` → `ocicl/bordeaux-threads-0.9.4/apiv1/impl-egcl.lisp`
   and `apiv2-impl-egcl.lisp` → `ocicl/bordeaux-threads-0.9.4/apiv2/impl-egcl.lisp`.
2. Patch `ocicl/bordeaux-threads-0.9.4/bordeaux-threads.asd`:
   - add `egcl` to the `(pushnew :thread-support *features*)` feature list;
   - add `(:file "impl-egcl" :if-feature :egcl)` to both the `api-v1` and
     `api-v2` module component lists (after the `impl-abcl` entry).
3. Patch `ocicl/bordeaux-threads-0.9.4/apiv2/atomics.lisp`: add `#+egcl`
   branches to `atomic-cas` / `atomic-decf` / `atomic-incf` (plain, single-
   threaded — a CAS is `(when (eql place old) (setf place new) t)`; incf/decf are
   ordinary), add `egcl` to each `#-(or …)` guard, and add `egcl` to the
   `atomic-integer` `cell` slot's `#+(or …)` group.
4. Patch `ocicl/trivial-garbage-.../trivial-garbage.lisp`
   (`weakness-keyword-opt`): add `egcl` to the fallbacks so weak hash-tables
   degrade to ordinary (strong) tables instead of signalling
   "Your Lisp does not support weak … hash-tables."
5. Patch `ocicl/bordeaux-threads-0.9.4/apiv2/api-threads.lisp` for the v2 API:
   add a `#+egcl` branch to `establish-dynamic-env` that returns FUNCTION
   directly (EGCL cannot carry a capturing wrapper's lexicals to the worker, nor
   provide the real lock-synchronised handshake the wrapper relies on), and a
   `#+egcl` branch to `join-thread` that returns `(%join-thread native-thread)`
   (the worker's value; %return-values slots are unused on this path).

## EGCL runtime fixes this port depended on

Landed in the EGCL tree (not here):
- Compiled-macro `&whole` now binds the whole call form including the operator
  (was dropping the operator, breaking global-vars' `define-global-var*`).
- Compiled-macro `&environment` now reaches `macroexpand-1` (was NIL), so v2's
  `with-lock-held` works.
- `*TYPE-DEFINITIONS*` / `*CONDITION-TYPES*` / `*CONDITION-DEFINITIONS*` seeded in
  the symbol value cell so compiled `deftype`/`define-condition` see them.
- `(defun (setf place) …)` also installs the mangled `%SETF-WRITER-place`
  function, so compiled `(setf (place …) v)` call sites resolve it.
- `PROGV` implemented.
- `(defvar name)` with no value leaves NAME unbound (interpreted and compiled).
- A CLOS class type is not shadowed by a same-bare-name `deftype` in another
  package.
- `COPY-PPRINT-DISPATCH`, `MAKE-RANDOM-STATE`, `RANDOM-STATE-P`,
  `COPY-READTABLE`, `*RANDOM-STATE*` stubs.
- `MAKE-THREAD` reifies a lambda so non-capturing closures run on a worker.
- `JOIN-THREAD` marks the joiner Blocked so a peer's GC can proceed.
