# bliss-7x7o — the ansi sequences chapter exhausts memory

## Correction

An earlier version of this file blamed **SEARCH-STRING.8**. That was wrong, and
the mistake is worth recording because it is easy to repeat: rt.lsp's
`do-entries` prints a test's name *after* it finishes, and only when it passed

    (format s "~@[~<~%~:; ~:@(~S~)~>~]" success?)

so the last name in the log is the last test that PASSED. The culprit is the
test *after* it: **SEARCH-STRING.9**.

## Reproducer

Smallest form, no harness, no ansi-test load order — the same expression is fine
compiled and runs away under `eval`:

```lisp
(load "<ansi-test>/auxiliary/search-aux.lsp")

;; fine — returns NIL in a few seconds
(flet ((%f (x) (case x ((#\0 a) 'c) ((#\1 b) 'd) (t nil))))
  (let ((target *searched-string*))
    (loop for pat in *pattern-sublists*
          for pos = (search pat target :start2 20 :key #'%f)
          unless (search-check pat target pos :start2 20 :key #'%f)
          collect pat)))

;; same form under EVAL — what rt.lsp does — grows without bound, OOM at 3G in ~100s
(eval '(flet ((%f (x) ...)) ...))          ; identical body
```

## What is established

- The cost is in `SEARCH` **with a `:key`**, under the interpreter. 40 calls:
  peak RSS 0.89G with `:key`, 0.23G without.
- It is not a Lisp-heap leak. `(room)` after a dozen such searches reports a
  nursery of ~3MB and an old generation of ~5MB while process RSS is over 1G,
  and `total consed` rises only ~2.25MB per search. The growth is host-side, and
  the GC never calls `madvise`/`MADV_DONTNEED`, so RSS is a high-water mark.
- Not a tiering bug: `BLISS_FORCE_TIER=T0` still OOMs. (Env vars do survive
  `scripts/bliss-limited.sh`'s `systemd-run` scope — verified with a deliberately
  bogus value, which printed the "unrecognized" warning through the wrapper.)
- Not closure creation: 40 `eval`s of the FLET that never call the closure stay
  flat at 0.23G. Nor plain closure calls: 200k funcalls of an `eval`-created
  closure from a compiled global defun cost nothing measurable.
- Under a HEAD binary the same site instead signalled
  `type error: Cons(0x...) is not of type FUNCTION`, and that identical error
  aborts the ansi CONS chapter at NSET-EXCLUSIVE.KEYWORDS.9.

## Where the time goes

`perf` on the reproducer is dominated by symbol-NAME machinery, not by anything
in SEARCH: `StrSearcher::new` (6.3%, from `symbol_bare_name`'s
`trim_start_matches`), `HashMap::insert` (6.1%), `symbols::find_index` (4.0%),
`strncmp`, sip hashing, malloc/free, plus
`bliss_compiler::macroexpand::Environment::visit_gc_roots` (3.4%).

That is the interpreter resolving functions by NAME STRING on every call.
Tracked separately as the real "direct builtin calls" work item.

## Fixed so far

`%match-at` (boot.lisp) called `(nth i list)` for every pattern element,
re-traversing from the head each time — O(plen * n^2) cdr steps per SEARCH. It
now walks with one `nthcdr` plus `cdr`. Roughly halves the time on the
reproducer; does not fix the memory growth.

## Next

Attach a native heap profiler (valgrind/massif is present; the binary is
musl-static, so expect it to be slow) to the `eval` reproducer above and find
what the `:key` path retains per call. The 27MB-per-call figure is host memory,
so it is Rust-side bookkeeping, not conses.
