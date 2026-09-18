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

## Resolved

Root cause: `MACROEXPAND_ENVIRONMENTS` (cli.rs) only ever GREW. Every macro
expansion binding an `&environment` parameter minted a fresh id and stored a
`MacroexpandEnv`; nothing ever removed one. Two costs compounded — the entries
were never freed, and `scan_evaluator_global_roots` walks every entry on every
collection, so each expansion made all later GCs slower.

Fixed in 17ed9d5 by giving them the dynamic extent CLHS 3.1.1.4 already
specifies: a scope guard in `expand_macro` drops the ids minted during that
expansion when it returns. On the reproducer above, 40 calls:

    peak RSS   1.33G -> 0.26G      (5x less)
    time      11720 -> 6605 ms     (44% faster)

Two smaller fixes found on the way: `%match-at` walked the list with `nth` per
pattern element (35e702b), and `symbol_bare_name` rebuilt a String on every
dispatch (3175c24, a separate 17.5% instruction win — measured but NOT the cause
of this bug).

## What cost the most time, so the next person skips it

`(room)` reported a ~3MB nursery and ~5MB old generation the entire time, so
every instinct to look at the Lisp heap or at GC tuning was wasted — enlarging
the nursery from 64MB to 512MB moved peak RSS by nothing at all. The growth was
host-side the whole time.

`valgrind --tool=massif --pages-as-heap=yes` is what located it, by showing the
growth under `__libc_malloc_impl` rather than under `gc::init_heap`. Plain
massif shows nothing useful here, because the GC mmaps its heap instead of
mallocing it. Add `--detailed-freq=1` or every snapshot comes back
`heap_tree=empty`.

Eliminated first, all by measurement rather than argument: tiering
(`BLISS_FORCE_TIER=T0` still OOMed), closure creation, plain closure calls,
per-GC forwarding-map churn, and symbol-name String churn.

Two measurement traps worth repeating:

- Wall-clock on this machine varies about 2x run to run and hid a real 17.5%
  improvement completely. `perf stat -e instructions` settled it in one run.
- Full `BLISS_GC_STRESS=1` from startup exceeds a 10-minute timeout in the debug
  build without being a hang: this program makes 30k-60k allocations, so that is
  30k-60k collections. Use `BLISS_GC_STRESS_SKIP` to stress only the tail, and
  confirm with `BLISS_GC_STRESS_AT=N` that the probe actually fires — a SKIP or
  AT above the allocation count is a silent no-op that reports a clean run
  proving nothing.
