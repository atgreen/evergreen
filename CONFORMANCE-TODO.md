# Conformance TODO — missing/broken Rust primitives

> **Status:** #1 (`mod`), #2 (`member :key`), #3 (`destructuring-bind`
> `&optional`/`&rest`), #4 (integer `floor`/`ceiling`/`round`/`truncate`
> remainder), and #5 (`string<`/`string>` mismatch index) are **FIXED** in the
> interpreter. #6 (`setf` on `elt`/`aref`), #7 (`vector`/`make-array`), and #8
> (`rational`/`rationalize` via `integer-decode-float`) remain — they need new
> Rust array/float-decode primitives, tracked with the stdlib long-tail work.

These ANSI-conformance gaps cannot be fixed in `lib/boot.lisp` alone: they are
either builtins whose behavior is wrong (and Lisp `defun`/`defmacro` does **not**
override an interpreter builtin), or capabilities that need a new Rust
primitive. Each entry gives the observed behavior and the correct ANSI result.

## Builtins that are wrong and cannot be shadowed from Lisp

1. **`mod` is wrong for negative arguments.**
   `(mod -7 3)` => `-1`; ANSI requires `2` (result takes the sign of the
   divisor). `rem` is also `-1` there, which *is* correct for `rem`. So `mod`
   currently just aliases `rem`. EGCL's Lisp code works around this by using
   `(floor a b)`'s second value instead of `mod`.

2. **`member` ignores/breaks `:key`.**
   `(member 2 '((2)) :key #'car)` => `NIL`; ANSI requires `((2))`. The set
   operations in boot.lisp (`union`/`intersection`/`set-difference`/`adjoin`/…)
   avoid the builtin `member`-with-`:key` and do their own keyed comparison as a
   result.

3. **`destructuring-bind` binds `&rest`/`&optional` incorrectly.**
   - `(destructuring-bind (a &rest r) '(1 2 3) r)` => `3`; ANSI requires
     `(2 3)` — `&rest` gets the *last element* instead of the tail.
   - `(destructuring-bind (a &optional b) '(1) b)` signals
     "destructuring mismatch: expected list for pattern (&OPTIONAL B), got NIL"
     instead of binding `b` to `NIL`.
   This affects any macro relying on `&optional`/`&rest` destructuring in a
   `destructuring-bind` pattern.

4. **Integer `floor`/`ceiling`/`round`/`truncate` return a float remainder.**
   `(ceiling 7 2)` => `4 -1.0` and `(truncate 7 2)` => `3 1.0`; ANSI requires an
   *integer* second value (`-1`, `1`) when both arguments are integers. The
   quotient (primary value) is correct.

5. **`string<` / `string>` / `string=` return `T`/`NIL` instead of the mismatch
   index.** `(string< "abc" "abd")` => `T`; ANSI requires the index of the first
   differing character (`2`) when the predicate holds, else `NIL`. (boot.lisp
   supplies the missing `string<=`/`string>=`/`string/=` and the case-insensitive
   `string-lessp` family with the correct index-or-`NIL` return, but cannot fix
   the three builtins.)

## Missing primitives needed for further Lisp-level work

6. **`setf` on `(elt seq i)` and `(aref array i)` is unsupported.**
   "SETF: unsupported place (ELT ...)" / "(AREF ...)". Without an element
   store, the destructive sequence/array operators cannot mutate in place.
   boot.lisp therefore implements `fill`, `replace`, `nsubstitute*`,
   `nstring-*`, `delete*` as non-destructive rebuilds that return a fresh
   sequence — correct when the caller uses the return value, but they do not
   mutate the argument object as a strictly-conforming destructive op would.

7. **No `vector` / `make-array` constructor builtin.**
   `(vector 1 2 3)` and `(make-array 3 :initial-element 0)` are undefined, and
   `aref` has no settable place (see #6). General array support (adjustable
   arrays, fill pointers, multi-dimensional arrays, `array-*` accessors) needs
   Rust-level array objects. `coerce`-to-`vector` works for reading, so the
   Lisp sequence layer treats simple vectors as read-only.

8. **`rational` / `rationalize` of a float are not implementable.**
   Exact float→rational conversion needs `integer-decode-float` (and
   `float-radix`/`float-digits`), which are not provided. `floatp`, `integerp`,
   `rationalp`, `realp`, `characterp`, `functionp` are now defined in boot.lisp,
   but `rational`/`rationalize` on floats are omitted pending the decode
   primitive.
