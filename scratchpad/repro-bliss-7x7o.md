# bliss-7x7o reproducer — SEARCH-STRING.8 consumes unbounded memory

Minimal repro (release binary, ~200s to hit a 4G cap):

1. Copy ansi-test somewhere private (the gate deletes fasls, so don't share a tree):

       cp -r ~/git/ansi-test /tmp/at && find /tmp/at -name '*.fasl' -delete

2. Replace `/tmp/at/sequences/load.lsp` with just:

       (compile-and-load* "search-aux.lsp")
       (in-package #:cl-test)
       (let ((*default-pathname-defaults*
              (make-pathname :directory (pathname-directory *load-pathname*))))
         (load "search-string.lsp") nil)

3. Run the chapter harness:

       ANSI_TEST_DIR=/tmp/at BLISS_MEM_MAX=4G BLISS_TIMEOUT=500 \
         scripts/ansi-gate.sh sequences

   => SEARCH-STRING.1-7 pass, then SEARCH-STRING.8 grows without bound and the
   cgroup kills it (exit 137).

## What is established

- It is SEARCH-STRING.8 specifically. RSS is flat at ~0.4G for the entire
  sequences chapter (2600+ tests) and then climbs ~1.1G/30s from the moment .8
  starts, to 13.4G, until the cap kills it.
- It is NOT a tiering bug: `BLISS_FORCE_TIER=T0` still OOMs.
- It is NOT test 8 on its own: a load.lsp with ONLY `deftest search-string.8`
  passes (1 passed, 0 failed). The preceding tests in the file matter.
- It is NOT the computation on its own: a harness-free script that loads
  search-aux.lsp and calls the .2/.3/.4/.7/.8 bodies as plain defuns, in that
  order and with .8 both first and last, completes fine.
- So the trigger needs the rt.lsp harness — `(eval (form entry))` per test with
  a compile-and-load'ed search-aux — plus the earlier tests having run.
- Under a HEAD-built binary the same site instead signalled
  `type error: Cons(0x...) is not of type FUNCTION`, and that same error also
  aborts the ansi CONS chapter at NSET-EXCLUSIVE.KEYWORDS.9. Two symptoms, one
  site: a value that should be a function read back as a cons, or unbounded
  consing. That pairing is what a rooting bug looks like.

## Next steps

- Bisect which of SEARCH-STRING.2-7 is required. NOTE: search-string.1 is
  inside a `#| ... |#` block comment, so a naive by-`deftest` splitter produces
  an unbalanced file — split on the comment too, or just delete tests from a
  copy of the real file.
- Then run that pair under `BLISS_GC_STRESS=1 BLISS_GC_POISON=1`, and bisect
  with `BLISS_GC_STRESS_SKIP` / `BLISS_GC_STRESS_AT` per AGENTS.md. Remember a
  SKIP above the program's allocation count is a silent no-op: confirm the
  `forcing minor GC at allocation #N` line actually fires.
- Use the RELEASE binary for the bisect. In the debug build, compiling
  search-aux.lsp alone takes ~6 minutes and will eat the whole timeout.
