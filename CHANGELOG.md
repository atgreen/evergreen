# Changelog

## Unreleased

### Added

- Optional GC root-scanner timings attribute stack, symbol, and individual
  external scanner costs, with address anchors for resolving scanner names
  ([#710](https://github.com/atgreen/evergreen/pull/710)).

- Optional minor-GC phase timings distinguish root scans, evacuation,
  relocation, and reclamation from stop-the-world coordination
  ([#707](https://github.com/atgreen/evergreen/pull/707)).

- `(UNSIGNED-BYTE 8)` arrays store one byte per element and report their
  specialized type, with native access, supported array and sequence operations,
  and typed BFASL, image, and readable-print round trips
  ([#701](https://github.com/atgreen/evergreen/pull/701)).

- An illustrated [guide to function calling conventions](docs/design/function-calling-conventions.html)
  for readers new to EGCL, covering all sixteen calls between T2, T1, T0,
  and the tree walker, with register, stack, and runtime metadata diagrams
  ([#395](https://github.com/atgreen/evergreen/pull/395)).

- `install-egcl-forks` includes the audited ocicl libraries through maintained
  EGCL forks. Adapters cover archive metadata, command-line handling, clocks,
  readtable iteration, CLOS JSON encoding, type introspection and macro expansion;
  libraries with portable implementations include focused compatibility checks.
  The library guide records tested revisions and remaining capability limits
  ([#638](https://github.com/atgreen/evergreen/pull/638)).

### Performance

- Linux minor GC can track writes to older heap ranges with asynchronous
  kernel write protection, preserving legacy and full-scan fallbacks while
  avoiding process-wide write protection of unrelated pages
  ([#713](https://github.com/atgreen/evergreen/pull/713)).

- Unchanged hash tables can skip repeated strong entry-root scans during minor
  GC, while preserving full major and inspection scans and weak-entry cleanup
  ([#711](https://github.com/atgreen/evergreen/pull/711)).

- T2 on x86-64 Linux omits redundant implicit polls within guarded eight-byte
  stores, retaining precise moving-GC roots and interrupt checks before and
  after each bounded operation
  ([#706](https://github.com/atgreen/evergreen/pull/706)).

- Fast native byte-array construction avoids copying constructor arguments,
  while preserving mutable type checks and public fallback
  ([#705](https://github.com/atgreen/evergreen/pull/705)).

- T2 combines eligible eight-byte integer serialization loops into one native
  word store on x86-64 Linux, preserving checked fallback, loop results, and
  precise GC and interrupt boundaries
  ([#704](https://github.com/atgreen/evergreen/pull/704)).

- T2 checks vector layout and bounds once before eligible counted store loops,
  preserving checked fallback, reference barriers, and moving-GC safety
  ([#699](https://github.com/atgreen/evergreen/pull/699)).

- Common `MAKE-ARRAY` calls can use a collecting native adapter that checks
  mutable unsigned-byte-8 type descriptors at runtime and allocates through the
  existing stdlib constructor, preserving general fallback and `NOTINLINE` calls
  ([#691](https://github.com/atgreen/evergreen/pull/691)).

- Simple array construction accepts proper rank-one dimension lists in its native
  fast path while preserving option handling and numeric initialization
  ([#686](https://github.com/atgreen/evergreen/pull/686)).

- T2 can compute eligible invariant loop-bound arithmetic once while preserving
  first-entry deoptimization, observable effects, and generic OSR paths
  ([#690](https://github.com/atgreen/evergreen/pull/690)).

- T2 can shift fixnums and canonical one-limb integers right without a generic
  arithmetic call, preserving fallback for wider results and other inputs
  ([#689](https://github.com/atgreen/evergreen/pull/689)).

- More T2 functions retain the ordinary native calling convention when they
  use guarded fixnum MIN/MAX or packed-double array reads
  ([#688](https://github.com/atgreen/evergreen/pull/688)).

- References to established constants can compile as literal values. T2 also
  folds fixnum constants in cached bytecode using metadata captured before
  background compilation, avoiding repeated global lookups
  ([#678](https://github.com/atgreen/evergreen/pull/678)).

- T2 can store fixnums directly into simple vectors when the overwritten value
  is also a fixnum, preserving the GC barrier for reference stores
  ([#673](https://github.com/atgreen/evergreen/pull/673)).

- T2 functions with proven unboxed integer loops can retain ordinary native
  calls while keeping arguments and results compatible with Lisp callers
  ([#674](https://github.com/atgreen/evergreen/pull/674)).

- T2 on x86-64 Linux can peel a narrowing integer loop into a generic first
  iteration and a guarded fixnum remainder. Constant byte extraction compiles
  directly, and eligible integer arithmetic stays unboxed between operations
  while retaining precise deoptimization and generic fallbacks
  ([#664](https://github.com/atgreen/evergreen/pull/664)).

- Native tier compilation on x86-64 Linux and s390x Linux now runs on
  background workers while Lisp execution continues. Compiler snapshots let
  moving GC proceed during optimization and machine-code emission; macro
  expansion and other compile-time Lisp effects stay in the requesting execution
  ([#662](https://github.com/atgreen/evergreen/pull/662)).

- `EQUALP` hash tables now hash bignum and ratio keys without temporary
  Rust heap allocations, preserving exact cross-type numeric hashes and the
  existing float fast paths
  ([#668](https://github.com/atgreen/evergreen/pull/668)).

- T2 now profiles integer shifts, bitwise operations and other numeric
  operations, and retains feedback when a cold speculation fails. Keccak
  extraction no longer repeatedly recompiles the same invalid fixnum guess
  ([#663](https://github.com/atgreen/evergreen/pull/663)).

- Local functions defined with `flet` or `labels` can now compile when they
  read or write enclosing lexical variables. Small helpers inline into their
  callers, including sibling `labels` functions, while shared mutation and
  lexical scope are preserved. Native captured-variable accesses also avoid
  redundant runtime work when no poll or signal needs servicing
  ([#658](https://github.com/atgreen/evergreen/pull/658)).

- On x86-64 Linux, T2 loops now read vector elements and boxed or packed
  doubles directly, and store computed doubles into packed arrays without
  allocating a box. Vector stores retain the moving-GC write barrier, and
  array accesses keep bounds checks and fallback for other layouts
  ([#657](https://github.com/atgreen/evergreen/pull/657)).

- On x86-64, T2 now emits native fixnum `min`, `max`, `mod`, `rem`,
  and single-value `floor` and `truncate` operations, avoiding runtime calls
  in integer loops while preserving signed results and error fallback
  ([#656](https://github.com/atgreen/evergreen/pull/656)).

- On x86-64 Linux, a long-running loop that computes with double-floats,
  such as `(dotimes (i n s) (setq s (+ s 1d0)))`, now leaves the interpreter
  part-way through its first call, as a fixnum loop already did. Before, it
  stayed interpreted for the whole call and the function still reported
  tier 0, even though the same body ran about 300 times faster once enough
  separate calls had compiled it. The loop keeps running while its optimized
  code compiles, then continues in that code with its doubles held unboxed.
  A first call of 10,000,000 iterations of that loop takes 0.2 s instead of
  15.7 s, and the issue's 4,000,000-iteration call after two warm-up calls
  takes 0.019 s instead of 6.3 s. A double loop that calls a function goes
  native too, but its calls are still slow in that first call: 4,000,000
  iterations take 2.9 s instead of 9.6 s, and 0.11 s from the next call on
  ([#649](https://github.com/atgreen/evergreen/pull/649), reported in
  [#642](https://github.com/atgreen/evergreen/issues/642)).
- On x86-64 Linux, a loop typed at the REPL, given to `--eval` or written at
  the top level of a loaded file now leaves the interpreter part-way through,
  as a loop in a once-called function already did: after 100,000 iterations
  the running loop continues in native code. So does a loop in a closure,
  such as `(funcall (make-adder) n)` on a `(lambda ...)` that loops. `time`
  is now compiled instead of interpreted, so `(time (dotimes ...))` and
  `(print (time ...))` measure compiled code; before, everything inside `time`
  was evaluated by the slow tree-walker. `loop for i below n` inside a `let`
  is compiled too. Without any warm-up, 10,000,000 iterations of a top-level
  fixnum sum loop take 0.24 s instead of 14.4 s; inside `time` they take the
  same 0.24 s, where they used to take about 22 minutes. A top-level loop
  that calls a function takes 5.5 s instead of 17.5 s, a consing loop 3.1 s
  instead of 18.9 s, and `(loop for i below n sum i)` 0.23 s instead of more
  than 5 minutes. A loop that increments a special variable still runs at
  about 1 us per iteration
  ([#637](https://github.com/atgreen/evergreen/pull/637)).
- On x86-64 Linux, compiled code reads and assigns a special variable
  directly in its value cell when no thread or fiber has it dynamically
  bound, instead of calling into the runtime for every access. Bound,
  unbound and constant variables still take the runtime path, so `let`,
  `progv`, per-thread bindings, unbound-variable errors and assignments to
  constants behave as before. 10,000,000 iterations of
  `(dotimes (i n) (incf *x* i))` take 0.6 s instead of 9.3 s, in a function
  or at top level, and a loop that only reads a special takes 0.36 s instead
  of 3.3 s. A loop that runs inside a `let` binding of a special variable
  now leaves the interpreter like any other hot loop: 10,000,000 iterations
  of `(let ((*x* 0)) (dotimes (i n) (incf *x* i)))` take 8.4 s instead of 22 s
  ([#639](https://github.com/atgreen/evergreen/pull/639)).
- On x86-64 Linux, a long-running loop whose arithmetic changes type part-way
  through, for example a fixnum sum that becomes a single-float or overflows
  into a bignum, returns to native code after the change instead of
  finishing that call in the interpreter. Once the loop has run another
  100,000 iterations, the code that made the wrong assumption is replaced
  with code compiled for the new types. One call of
  `(dotimes (i 8000000) (setq acc (+ acc (if (< i 4000000) 1 1.0f0))))` takes
  0.6 s instead of 7.4 s. A loop can switch code a limited number of times,
  so a value that keeps changing type ends up in generic native code
  rather than recompiling for every change. Loops that move to double-floats
  still finish in the interpreter
  ([#646](https://github.com/atgreen/evergreen/pull/646)).

### Fixed

- Strings opened by a character given double-quote syntax now end at that
  character, and characters given single-escape syntax quote the next character
  while reading a string
  ([#695](https://github.com/atgreen/evergreen/pull/695)).

- Class precedence lists follow Common Lisp's specified topological ordering
  where it differs from C3, preserving the standard method and slot precedence
  for nonmonotonic multiple-inheritance graphs
  ([#696](https://github.com/atgreen/evergreen/pull/696)).

- `NOTINLINE` declarations remain effective through native specialization,
  while preserving argument evaluation and lookup of redefined functions
  ([#703](https://github.com/atgreen/evergreen/pull/703)).

- `WRITE-TO-STRING` honors printer controls under every forced tier during
  bootstrap, avoiding an undefined-function error from a later macro definition
  ([#700](https://github.com/atgreen/evergreen/pull/700)).

- `defstruct`'s keyword constructor accepts slots named `t` or after another
  constant, and a slot named after a special variable no longer changes what
  the later slots' initforms see
  ([#693](https://github.com/atgreen/evergreen/pull/693)).

- General arithmetic `LOOP` clauses evaluate bound and step initial forms once
  in source order, before binding the iteration variable, including zero-trip
  loops and loops whose bodies change the original bound or step
  ([#687](https://github.com/atgreen/evergreen/pull/687)).

- The array accessors (`array-rank`, `array-dimensions`, `array-total-size`,
  `array-element-type`, `array-displacement`, `array-has-fill-pointer-p` and
  `adjustable-array-p`) signal a `type-error` for an argument that is not an
  array, instead of answering as if it were a vector, and
  `(upgraded-array-element-type nil)` returns `NIL`
  ([#671](https://github.com/atgreen/evergreen/pull/671)).

- Native x86-64 branches preserve comparison results across intervening boxing,
  fixing incorrect loop counts and results in some unboxed integer loops
  ([#680](https://github.com/atgreen/evergreen/pull/680)).

- Private function symbols no longer cause multi-gigabyte allocations in the
  interpreter's arithmetic-classification cache
  ([#662](https://github.com/atgreen/evergreen/pull/662)).

- A variable proclaimed special only by `declaim`, with no `defvar`, is special
  for the rest of a file compiled with `compile-file`, so a `let` of it binds
  dynamically instead of lexically
  ([#675](https://github.com/atgreen/evergreen/pull/675)).

- Fiber migration preserves execution-local state after native calls on s390x,
  preventing wrong `NIL` results and crashes caused by reading a previous
  carrier's fiber identity ([#661](https://github.com/atgreen/evergreen/pull/661)).

- `compile-file` now expands package-local `define-package` macros normally,
  so macros that add `COMMON-LISP` to their package's use list compile and
  load correctly ([#655](https://github.com/atgreen/evergreen/pull/655),
  reported in [#652](https://github.com/atgreen/evergreen/issues/652)).

- `egcl-thread:signal-semaphore` no longer gives one permit to two takers.
  An `egcl-thread:try-semaphore` in another thread or fiber could take a
  permit that `signal-semaphore` was about to hand to a waiting fiber; both
  then had it and the semaphore's count went negative
  ([#648](https://github.com/atgreen/evergreen/pull/648)).
- A fiber's timed wait now has exactly one outcome when its timeout expires
  just as it is woken. Before, `condition-wait` with `:timeout` could return
  true for a fiber whose timeout had already ended the wait, although the
  `condition-notify` that reached it did not count it as notified and went on
  to wake another waiter. A fiber's timed semaphore wait could likewise
  return success while the signalled permit also stayed available, so one
  signal admitted two waiters. A wake that arrives too late for one wait can
  also no longer end the same fiber's next wait, such as a following sleep,
  early ([#647](https://github.com/atgreen/evergreen/pull/647)).
- `subtypep` treats `short-float` as the same type as `single-float` and
  `long-float` as the same as `double-float`, as `typep` already did
  ([#682](https://github.com/atgreen/evergreen/pull/682)).

- A condition is no longer corrupted when a garbage collection runs while it
  is being made. If a slot's initform allocated enough to start a collection,
  `make-condition`, `error` and `signal` could return an object that was no
  longer a condition: `type-of` returned `t`, and `handler-case` for its own
  type did not catch it. `warn` and `error` with format arguments could
  likewise lose their format control string
  ([#645](https://github.com/atgreen/evergreen/pull/645)).
- An internal error inside compiled code that a native function called no
  longer aborts the process. It is reported as an ordinary Lisp error, as it
  already was for compiled code called from the interpreter. Making a closure
  also no longer fails outright when a garbage collection it starts cannot
  run, for example because another thread did not stop in time; the
  collection is retried at the next allocation
  ([#644](https://github.com/atgreen/evergreen/pull/644); reported in
  [#609](https://github.com/atgreen/evergreen/issues/609)).
- `defstruct` with `(:conc-name "")` interns each accessor in the current
  package, as for any other prefix, instead of using the slot's own symbol
  ([#684](https://github.com/atgreen/evergreen/pull/684)).

- `read-line` on a Gray stream, such as a flexi-streams stream, now reports
  end of file. It returns `eof-value` when `eof-error-p` is false and signals
  `end-of-file` otherwise. Before, it returned an empty string forever, so a
  loop reading lines until `nil` never ended. Building cl-unicode's tables
  from the Unicode data files hung this way ([#643](https://github.com/atgreen/evergreen/pull/643)).
- A closure that has become garbage is now collected even if its body
  contains `progv`, and so is everything it captured. Before, such a closure
  was kept for the rest of the run. bordeaux-threads wraps every thread
  function in one, so each `bt:make-thread` leaked the thread's function and
  its captures: about 54 KB per thread, or 20 KB per connection in a
  thread-per-connection server. `progv` is now compiled. Closures the
  interpreter builds are also freed once they are unreachable, during normal
  garbage collection. Before, they were freed only between files loaded with
  `load` ([#640](https://github.com/atgreen/evergreen/pull/640); reported in
  [#634](https://github.com/atgreen/evergreen/issues/634)).

- A `make-package` nickname that already names a package, a `defpackage`
  `:import-from` of a name its package lacks, and an `import` name conflict
  each signal a correctable `package-error`; `continue` leaves out the
  offending nickname, name or symbol
  ([#692](https://github.com/atgreen/evergreen/pull/692)).

## 0.0.11 - 2026-10-24

### Added

- `EGCL-EXT:MACROEXPAND-ALL` recursively expands macros and symbol macros
  with lexical environment support. Lambda-list defaults now expand in their
  proper binding scope, including optional/key/aux and supplied-p variables;
  outer symbol macros no longer replace bound parameter references
  ([#641](https://github.com/atgreen/evergreen/pull/641)).
- `egcl-ext:with-locked-hash-table` is implemented. It was documented but
  undefined. It runs its body holding a recursive lock of the table's own, so
  the bodies of `with-locked-hash-table` forms on one table exclude each
  other, as in SBCL ([#547](https://github.com/atgreen/evergreen/pull/547)).
- `EGCL-MOP` adds `method-function` (AMOP's function of `args` and
  `next-methods`, whose `call-next-method` runs those methods in turn),
  `generic-function-method-combination` and `find-method-combination` (a
  method-combination object for `standard` or a built-in short form, as
  `defgeneric`'s `:method-combination` chose it), `generic-function-method-class`
  and `generic-function-declarations`. Generic functions now print as
  `#<STANDARD-GENERIC-FUNCTION name>`, including `(setf name)` ones, instead of
  `#<FUNCTION>`. The [closer-mop fork](docs/library-forks.md) imports the new
  names
  ([#512](https://github.com/atgreen/evergreen/pull/512)).
- `EGCL-MOP` reads generic functions and methods:
  `generic-function-name`, `generic-function-lambda-list`,
  `generic-function-argument-precedence-order`, `method-specializers`,
  `method-generic-function`, `method-lambda-list`,
  `specializer-direct-methods`, `specializer-direct-generic-functions`,
  `compute-applicable-methods-using-classes` (AMOP's two values) and
  `eql-specializer-object`. A program can find every generic function with a
  method on a class and write a congruent method of its own on each; answers
  follow `remove-method` and redefinition. `method-generic-function` now also
  answers a `(setf name)` generic function instead of `nil`. The
  [closer-mop fork](docs/library-forks.md) imports these, so the `c2mop:` names
  work on EGCL
  ([#511](https://github.com/atgreen/evergreen/pull/511)).
- `egcl-ext:map-readtable-macro-characters` calls a function once for each
  macro character of a readtable, with its reader function, whether it is
  non-terminating, and, for a dispatching character, its sub-characters and
  their functions. Libraries such as named-readtables no longer need to probe
  all 1,114,112 character codes, which took about 9 seconds per
  `merge-readtables-into`; with this function a merge takes a few
  milliseconds. `get-macro-character` now returns a function for a
  dispatching macro character made with `make-dispatch-macro-character`
  instead of `nil`
  ([#489](https://github.com/atgreen/evergreen/pull/489)).
- `EGCL_T2_LOG` and the `compile` trace (`EGCL_LOG=compile=trace`) say why a
  function never left the interpreter on x86-64 Linux: a captured closure
  environment, an `&rest`/`&key`/`&aux` lambda list, a special or boxed
  parameter, or the first bytecode instruction the native compiler refuses.
  Such functions never reach T2, and used to be absent from `EGCL_T2_LOG` or
  logged only as "mapped native entry unavailable". A mapped T2 decline now
  names the refused instruction too
  ([#462](https://github.com/atgreen/evergreen/pull/462)).
- When the native compiler itself refuses a function, `EGCL_T2_LOG` and the
  `compile` trace name the failing step -- for example the mapped emitter's
  error or a control scope a transfer site cannot recover -- instead of the
  generic "native emission declined"
  ([#469](https://github.com/atgreen/evergreen/pull/469)).
- `EGCL_T2_LOG` now explains the remaining hot functions that stayed at T0
  without a word. A function or `DEFMETHOD` body the bytecode compiler cannot
  lower logs `NAME: not compiled to bytecode (REASON)`, naming the construct,
  such as a special variable used as a lambda-list parameter, an unsupported
  `SETF` place, or a method defined inside a lexical binding. A function whose
  closure needs its captured `BLOCK`/`TAGBODY` logs a T1 decline, and a
  function blacklisted after repeated guard deopts logs one summary line
  naming where T0 resumed and the disproved speculation. Deopt lines
  now use the same plain function name as the rest of the log
  ([#480](https://github.com/atgreen/evergreen/pull/480)).
- `EGCL-EXT:INFLATE` decompresses raw deflate, zlib and gzip data in the
  runtime, checking each format's trailer. EGCL's port of chipz uses it, so
  unpacking a package with `ocicl install` no longer runs chipz's inflate in
  interpreted code: extracting a 120 KB tar archive takes 3 s instead of 23 s
  ([#455](https://github.com/atgreen/evergreen/pull/455)).
- Shared libraries can travel inside a saved executable.
  `egcl-ffi:embed-foreign-library` reads one on the build host, and
  `load-foreign-library` then finds it by SONAME, path or alias on any host.
  The executable's appended data is now a zip archive, so `unzip -l app` lists
  the embedded libraries and ordinary zip tools can extract, replace, add or
  remove them, as the LGPL requires users to be able to. On glibc builds with
  the `c-ffi` backend a library is loaded from a sealed, CRC-checked in-memory
  file, never extracted to disk, and embedded `DT_NEEDED` dependencies are
  loaded the same way
  ([#445](https://github.com/atgreen/evergreen/pull/445)).
- `EGCL-EXT:CRYPTO-SHA512-BLOCK`, `CRYPTO-SHA512-WORDS`, `CRYPTO-KECCAK-F1600`
  and `CRYPTO-KECCAK-ABSORB`: native SHA-512/384 compression and Keccak
  permutation for Ironclad, whose 64-bit words are bignums in Lisp. With the
  EGCL fork of Ironclad, a repeated HTTPS request through pure-tls drops
  from about 11 s to 7 s
  ([#453](https://github.com/atgreen/evergreen/pull/453)).

### Performance

- A minor garbage collection no longer keeps dead lists alive because they
  were built by setting a cons's cdr, as `loop ... collect`, `(setf (cdr tail)
  ...)` and `nconc` do. Every such store was recorded as a root for the next
  minor collection, even when the cons itself was new and already garbage, so
  the collector copied most of the 64 MB nursery each time. A loop that
  collects 50-element lists 300,000 times spent 12.4 s in collection and now
  spends 0.06 s ([#631](https://github.com/atgreen/evergreen/pull/631)).
- On AArch64, a self-recursive function calls itself through a register entry
  instead of generic dispatch: `fib 27` at T2 drops from 334 ms to 34 ms, and a
  self-recursive sum from 938 ms to 44 ms
  ([#622](https://github.com/atgreen/evergreen/pull/622)). A hot
  tail-recursive function now promotes to T2 and runs at loop speed instead of
  16 times slower, and its declared integer ranges are enforced
  ([#592](https://github.com/atgreen/evergreen/pull/592)).
- The System Z, ppc64le and AArch64 ports compile more code natively: captured
  variables, closure factories, register entries for resolved callees, raw
  double-float operations and four-argument callees on System Z, and closure
  factories on ppc64le and AArch64
  ([#475](https://github.com/atgreen/evergreen/pull/475),
  [#477](https://github.com/atgreen/evergreen/pull/477),
  [#484](https://github.com/atgreen/evergreen/pull/484),
  [#485](https://github.com/atgreen/evergreen/pull/485),
  [#486](https://github.com/atgreen/evergreen/pull/486),
  [#505](https://github.com/atgreen/evergreen/pull/505),
  [#531](https://github.com/atgreen/evergreen/pull/531),
  [#612](https://github.com/atgreen/evergreen/pull/612),
  [#613](https://github.com/atgreen/evergreen/pull/613),
  [#614](https://github.com/atgreen/evergreen/pull/614)).
- On x86-64 Linux, a loop in a function that is called only once now leaves
  the interpreter part-way through: once it has gone round 100,000 times, the
  function is compiled and the running loop continues in native code from the
  same iteration. Before, such a loop ran interpreted to the end. Without any
  warm-up, 10,000,000 iterations of a fixnum sum loop take 0.21 s instead of
  14.5 s, of a loop that calls another function 4.7 s instead of 16.8 s, of a
  loop that reads a special variable 2.9 s instead of 14.6 s, and 3,000,000 of
  a consing loop 4.3 s instead of 9.4 s; three calls of a 2,000,000-iteration
  driver of a double-float step function take 6.1 s instead of 21.5 s. A
  loop inside the same function's `handler-case`, `unwind-protect`, `catch`
  or special binding, a loop that computes with double-floats, and a loop at
  top level still run interpreted ([#630](https://github.com/atgreen/evergreen/pull/630)).
- On x86-64 Linux, a function whose loop uses `push`, `decf` or `loop ...
  collect` now gets the fast register calling convention when it is optimized,
  as other loops already did, so calling it is cheaper: 100,000 calls of a
  50-iteration `push` loop take 0.34 s instead of 0.50 s
  ([#630](https://github.com/atgreen/evergreen/pull/630)).
- `loop ... collect` no longer calls `rplacd` and `last` for every element it
  collects: a collected element is a fresh one-element list, so it becomes the
  new tail directly. A compiled `(loop repeat 50 collect x)` drops from about
  287 µs to 80 µs ([#628](https://github.com/atgreen/evergreen/pull/628)).
- On x86-64 Linux, optimized code allocates conses and double-float boxes
  itself, bumping the thread's allocation buffer, instead of calling the
  runtime for each one. A warmed `(cons 1 2)` in a loop drops from about
  0.26 µs to 0.013 µs, `(list a b c d)` from 0.52 µs to 0.044 µs and
  `(list* a b c d)` from 6.2 µs to 0.063 µs; 50 `push`es take 2 µs instead of
  13 µs and `(make-list 50)` about 9 µs instead of 20 µs, and consing a double
  computed in the loop drops from 0.38 µs to 0.035 µs. Collections and
  `EGCL_GC_STRESS` behave as before; `EGCL_INLINE_ALLOC=0` turns inline
  allocation off ([#625](https://github.com/atgreen/evergreen/pull/625)).
- On x86-64 Linux, a function whose body loops (`dotimes`, `dolist`, `do`,
  `loop` or a `tagbody`) now calls other compiled functions with a direct
  machine call once it is optimized, instead of building a managed activation
  for each call, and its loop no longer calls into the runtime on every
  iteration. A warmed call to a two-argument function from such a loop drops
  from about 690 ns to 12 ns. Calls to functions with `&optional` or `&key`
  parameters, and `apply`, also get cheaper: `apply` of a function object from
  a loop drops from about 1.8 µs to 0.75 µs and `funcall` of a symbol from
  2.7 µs to 1.6 µs ([#621](https://github.com/atgreen/evergreen/pull/621)).
- On x86-64 Linux, optimized code now calls more kinds of functions with a
  direct machine call, as it calls fixed-arity functions since #621:
  - A function whose trailing parameters are `&optional` (at most three
    parameters): a warmed call from a loop drops from about 640 ns to 15 ns.
  - A function with `&key` parameters whose defaults are constants, binding at
    most three variables: about 680 ns to 30 ns.
  - A function that computes with raw double-floats, and a loop that
    accumulates them: a call to a function returning `(+ x 1d0)` drops from
    about 880 ns to 180 ns, most of which is boxing the argument and the
    result.

  ([#623](https://github.com/atgreen/evergreen/pull/623))
- On x86-64 Linux, optimized code also calls a function with declared
  parameter types, such as `(declare (fixnum x))`, with a direct machine call;
  the function's entry checks the declarations and signals the same
  `type-error` as before. A warmed call from a loop drops from about 700 ns to
  14 ns. `(funcall f ...)` from optimized code, where `f` is the current
  definition of a named function, now enters that function directly too:
  `(funcall #'f s 1)` in a loop drops from about 660 ns to 19 ns
  ([#624](https://github.com/atgreen/evergreen/pull/624)).
- On x86-64 Linux, optimized double-float code no longer falls back to
  generic arithmetic when it combines a declared fixnum with a double-float
  under `(safety 0)`, as in `(* i 1d-3)`; the fixnum is converted to the
  nearest double, as before. `+`, `-` and `*` of three or more double-floats
  now also compute on unboxed doubles. GH #594's `step-plain` driver, which
  does both, drops from about 2.4 µs to 0.14 µs per call
  ([#627](https://github.com/atgreen/evergreen/pull/627)).
- On x86-64 Linux, optimized code computes `sqrt`, unary `-` and `/` of
  double-floats natively on unboxed doubles instead of calling the builtin.
  The answers are unchanged: `sqrt` of a negative double still returns a
  complex, `(sqrt -0d0)` and `(- 0d0)` are still `-0.0d0`, and dividing by
  zero still signals `division-by-zero`. Over 10M iterations, a loop adding
  `(sqrt (* x x))` drops from about 5.1 s to 0.09 s, one adding `(/ x 3d0)`
  from 3.7 s to 0.085 s, and one negating its sum from 4.2 s to 0.09 s.
  GH #594's `step-sqrt` driver drops from about 0.49 µs to 0.16 µs per call
  (2.7 µs before the previous change)
  ([#627](https://github.com/atgreen/evergreen/pull/627)).
- More than 50 builtins called from compiled code no longer rebuild and
  re-evaluate their call form each time. They include `evenp`, `oddp`, `/`,
  `member` and `assoc` with `:test` or `:key`, `mapcar`, `map`, `format`,
  `type-of`, `boundp`, `string=`, `make-hash-table`, `find-class`, `random`,
  `concatenate`, `float`, `sqrt` and the other transcendental functions, and the
  array, pathname and file predicates and accessors. A `:test`, `:key` or mapped
  function that is a builtin, such as `#'equal` or `#'1+`, is called directly.
  On x86-64 Linux, `(oddp e)` in a compiled loop drops from about 14 µs to
  0.6 µs, so `(count-if #'oddp list)` costs about 2.9 µs per element instead
  of 28 µs. `type-of`, `boundp` and `string=` drop from 19–26 µs to about
  4.5 µs a call, `member` and `assoc` with `:test` from about 30 µs to 7 µs,
  and `mapcar` and `format nil` from about 32 µs to 14 µs
  ([#618](https://github.com/atgreen/evergreen/pull/618)).
- `(require :asdf)` takes about half as long: 6.4 s instead of 11.8 s on an
  x86-64 Linux release build, which shortens every start whose `~/.egclrc`
  loads ASDF. UIOP's `define-package` spent most of that time in symbol and
  package lookups. `find-symbol`, `intern`, `export`, `import`, `unintern`,
  `symbol-package`, `package-use-list` and `package-used-by-list` no longer
  scan every symbol of a package, or of every package, to find one symbol,
  and they no longer allocate a string for each lookup. Called from compiled
  code, these functions and `string`, `list`, `gethash` stores and
  `(setf car)` / `(setf cdr)` now cost about 1 µs instead of 3-27 µs.
  `equal` hash tables with string keys hash and compare the keys in place
  ([#610](https://github.com/atgreen/evergreen/pull/610)).
- On s390x, a loop whose entry carries a hoisted fixnum proof can be entered
  mid-loop at T2. Such loops previously installed optimized code with no
  on-stack-replacement entry to land on, so they could only be replaced into
  T1. Measured on a z16, one counted integer loop runs at about 0.022 µs per
  iteration entered at T2 against about 0.088 µs entered at T1; across the
  tier-differential corpus 24 more functions now publish at least one
  on-stack-replacement entry. A loop in a function that never reaches T2 at
  all is unaffected
  ([#611](https://github.com/atgreen/evergreen/pull/611)).
- `substitute`, `substitute-if` and `substitute-if-not` on a whole list
  without `:count` or `:from-end` make one pass over the list, as `remove`
  does since #607. Substituting in an 8-element list drops from about 207 µs to
  90 µs ([#608](https://github.com/atgreen/evergreen/pull/608)).
- `remove`, `remove-if`, `remove-if-not`, `delete` and `delete-if` on a whole
  list without `:count` or `:from-end` make one pass over the list, instead of
  copying it into a vector, collecting the matching positions and then
  searching those positions once per element. `(remove 1 nil)` drops from about
  88 µs to 10 µs, and removing from an 8-element list from about 190 µs to
  70 µs ([#607](https://github.com/atgreen/evergreen/pull/607)).
- `sort` and `stable-sort` called from compiled code no longer rebuild and
  re-evaluate their call form each time, and a builtin predicate or key such
  as `#'<` or `#'car` is called directly instead of through the general
  function-call path on every comparison. On x86-64 Linux, sorting 100,000
  elements drops from about 25 µs to 13 µs per element with a `lambda`
  predicate, from 16 µs to 6.3 µs with `#'<`, and from 28 µs to 2.3 µs for a
  vector sorted with `:key #'car`
  ([#606](https://github.com/atgreen/evergreen/pull/606)).
- A minor garbage collection costs time in proportion to the data that
  survives it, not to how much was allocated since the last one. It no longer
  walks every object in the 64 MB nursery to find pinned objects, and no longer
  clears every byte of the reclaimed space during the pause; the space is
  cleared as it is handed out again. On x86-64 Linux a loop that allocates
  heavily but keeps nothing alive pauses about 7 ms per collection instead of
  about 48 ms ([#605](https://github.com/atgreen/evergreen/pull/605)).
- `funcall` and `apply` of a function object whose optimized native code is
  already installed run that code directly, instead of working out again on
  every call how to call the object. On x86-64 Linux, a warmed loop of
  `(funcall f s 1)` drops from about 2.0 µs to 0.65 µs a call for a `defun`'d
  function, from 2.5 µs to 0.63 µs for a `lambda`, from 4.0 µs to 1.6 µs for a
  closure, and from 0.9 µs to 0.3 µs for `#'+`. Higher-order functions
  written in Lisp gain the same way: `reduce`, `find-if` and `mapcar` over
  100,000 elements run two to four times faster, and `sort` with a `lambda`
  predicate about twice as fast. Argument-count and keyword
  errors, multiple values, non-local exits and redefinition behave as before
  ([#604](https://github.com/atgreen/evergreen/pull/604)).
- `make-instance` of a class reuses what it worked out for the class's
  previous instances: which slots each initarg fills, the default initargs and
  initforms to run, and the `:after` methods on `initialize-instance` and
  `shared-initialize` that apply. Compiled code, `funcall` and `apply` call it
  directly instead of building and evaluating a form. On x86-64 Linux, in a hot
  function, `(make-instance 'point :x i :y i)` drops from about 40 µs to
  1 µs, and a subclass with a third slot from about 54 µs to 1.1 µs. Initarg
  checking, default initargs, initforms, user methods, class redefinition and
  `make-instances-obsolete` behave as before; adding or removing a method or
  redefining a class makes the next call work everything out again
  ([#596](https://github.com/atgreen/evergreen/pull/596)).
- Compiled calls to structure accessors and to class `:reader`, `:writer` and
  `:accessor` functions read or store the slot directly. A read such as
  `(point-x p)` drops from about 3.2 µs to 0.2 µs, and a store such as
  `(setf (point-x p) v)` from about 5 µs to 0.2 µs, on x86-64 Linux in a hot
  function. User methods on the accessor, redefinitions of the accessor or
  its class, and slot type checks behave as before. A `(setf place)` writer
  installed with `(setf fdefinition)` or `defun` is now also used when a
  compiled `setf` of that place calls it by name, as `#'(setf place)` already
  did ([#588](https://github.com/atgreen/evergreen/pull/588)).
- On x86-64 Linux, single-float code compiles natively. A function with a
  single-float literal, such as `(defun f1 (x) (+ x 1.0))`, used to stay in
  the interpreter; now single-float `+`, `-`, `*` and the comparisons `<`,
  `<=`, `>`, `>=` and `=` run as native code in the T1 and T2 tiers, with the
  interpreter's results. A single-float Fibonacci, `(ffib 27.0)`, drops from
  about 2.1 s to 0.02 s, and `(+ x 1.0)` called ten million times from about
  21 s to 5.5 s, the same as its fixnum version. A cold call site that was
  guessed to be fixnum now recompiles after its first wrong guess instead of
  after eight deoptimizations
  ([#590](https://github.com/atgreen/evergreen/pull/590)).
- On x86-64 Linux, double-float arithmetic compiles natively in the T2 tier.
  `+`, `-` and `*` of two numbers, at least one of them a double-float, and
  the comparisons `<`, `<=`, `>`, `>=` and `=` on double-floats now run on
  unboxed doubles. A double stays in a register from one operation to the
  next and across loop iterations, and is boxed only when it is returned,
  passed to a call or stored. Results are the interpreter's, including
  contagion with fixnums, signed zeros, infinities and comparisons with NaN.
  Ten million iterations of `(setq s (+ s 1d0))` drop from about 2.9 s to
  0.05 s, the same as the fixnum loop, and a double-float Fibonacci,
  `(ffib 27d0)`, from about 3.2 s to 0.15 s
  ([#602](https://github.com/atgreen/evergreen/pull/602)).
- A function containing `load-time-value` compiles in the running image, with
  each `load-time-value` form evaluated once when the function is compiled. It
  stayed in the interpreter and evaluated the forms on every call, so
  CL-UNICODE rebuilt a literal cl-ppcre scanner per call
  ([#566](https://github.com/atgreen/evergreen/pull/566)).
- On x86-64 Linux, a hot function that calls a small helper that can
  deoptimize, such as `(defun pick (x y) (car y))`, now reaches the
  optimizing T2 tier. Inlining the helper used to make T2 refuse the caller,
  which then stayed at T1. A loop that calls such a helper 2,000,000 times
  drops from about 6.6 s to 1.2 s
  ([#557](https://github.com/atgreen/evergreen/pull/557)).
- A function whose `restart-case` clause does a `go` to an enclosing `tagbody`
  tag or a `return-from` to an enclosing `block` now compiles; it ran in the
  interpreter ([#555](https://github.com/atgreen/evergreen/pull/555)).
- `MEMBER-IF`, `MEMBER-IF-NOT` and `READ-DELIMITED-LIST` leave the
  interpreter. All three used `DO`, whose expander is defined later in the
  same prelude file, so the bytecode compiler refused their bodies and no
  later call could lift them -- they ran tree-walked for the life of the
  image. Scanning a 2000-element list 300 times with `MEMBER-IF` drops from
  50.3 s to 0.9 s, and it is now faster than the equivalent `COUNT-IF`
  ([#523](https://github.com/atgreen/evergreen/pull/523)).
- Linux AArch64 compiles more code natively. Its optimising compiler refused
  the type predicates (`TYPEP`, `SYMBOLP`, `INTEGERP`, `STRINGP`,
  `HASH-TABLE-P`), `LENGTH` on a simple string, memory fences, and the
  unreachable path after a call that never returns; its baseline compiler
  refused closure creation, memory fences, one of the two conditional
  branches, and the result check a declared `FTYPE` ends in. A refused
  operation does not merely run slowly -- it costs the WHOLE function its
  native code, so a declared return type or a single closure kept a function
  interpreted. Over the tier-differential corpus, 75 functions now reach the
  optimising tier where 59 did. Numeric code also stops saving and reloading
  values the collector can never move around each runtime call ([#501](https://github.com/atgreen/evergreen/pull/501)).
- On x86-64 Linux, interpreted code calling a function that has been compiled
  to native code no longer pays for a system call on every call: the runtime
  asked the kernel for the thread's shadow-stack status each time it entered
  native code, and now asks once per thread. Such a call also skips work
  that cannot change its outcome -- the tier bookkeeping for a callee
  already at T2, a fresh root for the callee's identity, and reloading
  unchanged floating-point control registers. A two-argument fixnum `+`
  called from a `COMPILE-FILE`d loop that stays in the interpreter fell from
  about 2.5 us to 1.0 us per call net of the loop; the same call with the
  callee left in the interpreter costs about 0.8 us, so a compiled callee
  is still slightly slower from interpreted code. A call with one `&key`
  argument fell from about 6.3 us to 3.8 us, against about 1.5 us
  interpreted, and `FUNCALL` of a closure from about 9 us to 6 us
  ([#499](https://github.com/atgreen/evergreen/pull/499)).
- On x86-64 Linux, a call from interpreted code to a function compiled to
  the optimizing tier's direct native calling convention takes a shorter
  entry path, so such a call is now cheaper than calling the same function
  interpreted. A two-argument fixnum `+` called from a `COMPILE-FILE`d loop
  that stays in the interpreter fell from about 0.95 us to 0.7 us per call
  net of the loop, against about 0.8 us with the callee interpreted.
  Compiled code calling a builtin such as `ELT`, or a compiled `&key`
  function calling a function such as `CHAR`, no longer looks the callee up
  again or prepares an unusable call frame on every call. A call with one
  `&key` argument whose body calls `CHAR` fell from about 3.8 us to 2.4 us,
  which is still slower than the 1.5 us it costs interpreted
  ([#502](https://github.com/atgreen/evergreen/pull/502)).
- On x86-64 Linux, optimized (T2) code reads `CHAR`, `SCHAR` and `ELT` of a
  simple string inline instead of calling the function. Any other argument,
  such as a list, a string with a fill pointer, or an index out of range,
  still calls the function, with the same result or error as before. A call
  from a `COMPILE-FILE`d loop that stays in the interpreter to
  `(defun kw (s &key (start 0)) (char s start))` fell from about 2.6 us to
  0.9 us net of the loop, about half its 1.7 us cost with `KW` left
  interpreted. `CHAR` itself, which is defined with `ELT`, is cheaper to call
  from interpreted code once compiled than left interpreted
  ([#506](https://github.com/atgreen/evergreen/pull/506)).
- On x86-64 Linux, a closure that reads or sets variables captured from the
  function or `LET` that created it -- `(lambda (y) (+ x y k))`, a counter's
  `(lambda () (incf n))`, or a wrapper such as `(lambda (&rest a) (apply
  original a))` installed with `(setf (symbol-function 'f) ...)` -- now
  compiles to native code and can reach the optimizing tier, including by
  on-stack replacement of a running loop, instead of staying in the
  interpreter. All closures made by one `LAMBDA` share one compiled body. This
  applies when the creating code is itself compiled, as it is for files
  built with `COMPILE-FILE` or ASDF and for functions called often enough to
  compile.
  `EGCL-EXT:FUNCTION-TIER` now reports the tier such a closure runs at. A
  closure whose `&optional` or `&key` default reads a variable of its
  creator, such as `(lambda (x &key (off base)) ...)`, no longer signals
  `UNBOUND-VARIABLE` when the creator is compiled
  ([#494](https://github.com/atgreen/evergreen/pull/494)).
- A closure whose creator has not been compiled -- a function called only
  once, such as `(defun outer (x) (let ((k 3)) (lambda (y) (+ x y k))))`, or
  a toplevel `LET` that installs a wrapper with `(setf (symbol-function 'f)
  ...)` -- is now compiled itself once it has been called a few times, and
  can then reach native code and the optimizing tier. Such closures used to
  run in the interpreter however hot they became, and
  `EGCL-EXT:FUNCTION-TIER` returned `NIL` for them. A closure that calls a
  local function of its creator's `FLET` or `LABELS`, or that sees a local
  `SPECIAL` declaration, still runs in the interpreter
  ([#497](https://github.com/atgreen/evergreen/pull/497)).
- A minor collection's pause no longer grows with the old generation on
  Linux. To find references into the nursery the collector traced every
  older object twice per minor collection, about 3-5 ms per MB of live
  conses: with 150 MB of conses retained, each minor collection took
  0.4-0.8 s. It now asks the kernel which pages were written since the
  previous minor collection (soft-dirty tracking) and scans only those. The
  same minor collections take 44-68 ms with 0-250 MB retained, and 48-50 ms
  with 0-150 MB of fixnum vectors (previously up to 174 ms). Tracking costs
  a page fault on the first write to each page of the process after every
  collection, mostly nursery pages, so it starts only once the old
  generation reaches a quarter of the nursery's size (16 MB by default);
  `EGCL_GC_SOFT_DIRTY=1` uses it always and `EGCL_GC_SOFT_DIRTY=0` never.
  Kernels without soft-dirty support and other systems keep the full scan
  ([#491](https://github.com/atgreen/evergreen/pull/491)).
- A call to a function with `&optional`, `&rest`, `&key` or `&aux`
  parameters no longer re-parses its lambda list and binds it through the
  interpreter on every call. The lambda list is parsed once, and calls whose
  omitted parameters default to constants bind straight into the frame; a
  default that must be evaluated, `:allow-other-keys` in a call, or a
  malformed keyword list still takes the general binder. A one-`&key` call from
  bytecode into native code, which had become slower than the bytecode call it
  replaced, is now about half its previous cost and faster than the bytecode
  call; calls with several `&key` parameters are back to about the bytecode
  call's cost, and every call from bytecode into native code is cheaper. A
  function the baseline compiler declined, including every closure made from
  a declined `(lambda ...)`, is no longer re-checked for tier-up on each call,
  and `EGCL_LOG=compile=trace` logs that decline instead of a dispatch line per
  call
  ([#495](https://github.com/atgreen/evergreen/pull/495)).
- On x86-64 Linux, a function whose `LAMBDA` makes a `RETURN-FROM`, `RETURN`
  or `GO` to a block or tag that the `LAMBDA` itself establishes, such as
  `(mapcar (lambda (x) (dolist (y x) (when (p y) (call (lambda () (return y))))))
  list)`, is now compiled natively and can reach the optimizing tier. Only a
  closure that exits past the `LAMBDA` it belongs to still keeps its creator
  in the interpreter. A function also no longer establishes its implicit
  block when its only `RETURN-FROM` forms name other blocks
  ([#479](https://github.com/atgreen/evergreen/pull/479)).
- On ppc64le, functions that use `MULTIPLE-VALUE-LIST` or
  `MULTIPLE-VALUE-PROG1` now reach the optimizing tier instead of stopping one
  tier below it, and functions that create a closure capturing no local
  variables -- `(mapcar (lambda (x) (* x 2)) l)`, `(sort l (lambda ...))`,
  `(remove-if (lambda ...) l)` -- compile to native code and reach the
  optimizing tier instead of staying in the interpreter entirely. x86-64 Linux
  gained these through its mapped native entry; POWER has no mapped entry, so
  its emitters call the same runtime helpers directly. A function creating a
  closure that *does* capture a variable now compiles to baseline native code
  there as well, instead of staying in the interpreter; it still does not reach
  the optimizing tier, as on x86-64
  ([#478](https://github.com/atgreen/evergreen/pull/478)).
- A `BLOCK` or `TAGBODY` that contains `ASSERT`, `CHECK-TYPE`,
  `IGNORE-ERRORS`, `HANDLER-CASE` or `RESTART-CASE` is no longer registered
  by name unless a closure or restart clause created inside it exits to it
  by name. Registration cost three allocations each time the block was
  entered, and on x86-64 Linux the native compiler declined any function
  containing such a block ("PushBlock needs an executing identity"). Those
  functions can now compile to native code, although other limits still
  apply: a function stays in T0 bytecode when a closure exits to its block
  (for example a `HANDLER-BIND` handler that does `RETURN-FROM`), or when a
  `RETURN-FROM` or `GO` leaves a `HANDLER-CASE`, `IGNORE-ERRORS` or
  `UNWIND-PROTECT` body. A cold Babel load compiles six more ASDF and UIOP
  functions to native code, among them `MAKE-ACTION-STATUS`,
  `COMPUTE-ACTION-STATUS`, `TRUENAMIZE` and `FEATUREP`
  ([#481](https://github.com/atgreen/evergreen/pull/481)).
- Calling a natively compiled function from interpreted code is cheaper on
  x86-64 Linux. Each such call used to rebuild the activation state for every
  call site in the function, which was one of the largest costs of loading
  Babel; the state of a finished call is now reused. The T1 compiler also no
  longer rescans the whole function for each removed trivial phi, each block's
  predecessors or each loop poll. A cold `(asdf:load-system :babel)` drops
  from about 44 s to 32 s (CPU time from 37.9 s to 26.0 s), which also
  undoes the slowdown that compiling more functions natively had caused
  ([#483](https://github.com/atgreen/evergreen/pull/483)).
- `COUNT`, `COUNT-IF` and `COUNT-IF-NOT` compile to bytecode instead of being
  tree-walked on every call, and `FIND`, `FIND-IF`, `FIND-IF-NOT` and the
  `COUNT` family no longer build a closure for each call. Counting over a
  100-element sequence is about 45x faster (`COUNT-IF` about 12x) and `FIND`
  about 2x faster. None of these functions is refused native compilation for
  a closure environment any more, and with keyword functions now compiling
  natively they reach T1 and T2 when hot
  ([#465](https://github.com/atgreen/evergreen/pull/465)).
- Methods hot by call count compile to native code on x86-64 Linux. A
  method's compiled body is named `(METHOD ...)`, which the T1 compiler tried
  to read back as a symbol; it failed, so any method needing the optimizing
  frontend stayed interpreted and never reached T2. Loading Babel now
  compiles 23 more ASDF methods to T1, and a method called past the T2
  threshold is optimized like a `DEFUN`
  ([#469](https://github.com/atgreen/evergreen/pull/469)).
- A `(return-from NAME ...)` inside a `FLET` or `LABELS` function compiles to
  bytecode: the compiler now establishes the implicit block named after the
  local function, as the tree-walker does. Such a return used to send the
  whole enclosing function to the tree-walker. chipz's inflate state machine
  is one, and inflating a 2.7 KB gzip member drops from 50 s to 2.7 s
  ([#449](https://github.com/atgreen/evergreen/pull/449)).
- `REPLACE` between simple vectors or simple strings, `SEARCH` with the
  default `EQL` test and no key on them, and `FILL` of a simple vector or
  simple string run natively instead of element by element in Lisp. Parsing
  a 121-certificate CA bundle with pure-tls drops from 20 s to about 14 s,
  and zeroing a 1 KB octet buffer with `FILL` from 4.4 ms to 28 us
  ([#450](https://github.com/atgreen/evergreen/pull/450)).
- Writing a slot of a structure that has typed slots stores through the
  accessor fast path once the value satisfies the slot's type, instead of
  running the writer method's body: about 12 us per write instead of 61 us.
  A value of the wrong type still signals the correctable `TYPE-ERROR`
  ([#452](https://github.com/atgreen/evergreen/pull/452)).
- `MAKE-ARRAY` of a plain vector -- a fixnum length, at most
  `:ELEMENT-TYPE` and either `:INITIAL-ELEMENT` or `:INITIAL-CONTENTS`, and
  an element type that upgrades to T -- is built natively instead of through
  the full option parsing: `(make-array 16)` takes about 12 us instead of
  31 us, and a four-element vector from a list about 17 us instead of 88 us
  ([#454](https://github.com/atgreen/evergreen/pull/454)).
- On x86-64, functions that multiply, left-shift or compare a fixnum by a
  constant wider than 32 bits, such as `(* a 1000000000000)`, `(ash a 40)` or
  `(< a 1000000000000)`, now reach the optimizing tier instead of stopping
  one tier below it
  ([#456](https://github.com/atgreen/evergreen/pull/456)).
- Functions whose lambda list has `&optional` parameters, but no `&rest`,
  `&key` or `&aux`, now compile to native code on x86-64 Linux. They used to
  stay in the interpreter there, because the native entry required every
  argument; it now fills in omitted optional arguments itself, so these
  functions reach T1 and then T2 like fixed-arity ones
  ([#457](https://github.com/atgreen/evergreen/pull/457)).
- Functions whose lambda list has `&rest`, `&key` or `&aux` parameters now
  compile to native code on x86-64 Linux too, provided no closure captures
  those parameters. The native entry binds the lambda list with the same
  binder as the interpreter, so keyword checking, supplied-p variables and
  defaults behave as before. Prelude functions such as `STRING-UPCASE`,
  `POSITION-IF`, `REDUCE` and `PARSE-INTEGER` now reach T1 and then T2
  instead of staying in the interpreter
  ([#470](https://github.com/atgreen/evergreen/pull/470)).
- Functions that create closures over their own parameters or local
  variables now compile to native code on x86-64 Linux, for example
  `REMOVE`, `REMOVE-IF` and any function that builds a `LAMBDA` over a `LET`
  binding it later changes. Such functions used to stay in the interpreter.
  The native entry now builds the shared closure environment the same way the
  interpreter does, so closures still see and change the same bindings,
  including after on-stack replacement into T2 and after deoptimization.
  Closure bodies that read bindings from the function that created them still
  run interpreted
  ([#482](https://github.com/atgreen/evergreen/pull/482)).
- `FLOOR`, `CEILING`, `TRUNCATE`, `ROUND`, `MOD` and `REM` on bignums divide
  once instead of going through exact rational arithmetic, which reduced
  `a/b` with a gcd first. A `MOD` of a 4096-bit number by a 2048-bit one is
  about 45x faster, an RSA-2048 signature check through Ironclad about 65x,
  and a repeated HTTPS request through pure-tls drops from about 38 s to 11 s
  ([#443](https://github.com/atgreen/evergreen/pull/443)).
- `LOOP` clauses that step a numeric index (`for i below n`, `for i from a to
  b`, with `collect`, `append`, `nconc`, `sum`, `count` and the like) now reach
  the optimizing T2 tier. Guard elimination left their IR failing verification,
  so such loops stayed at T1
  ([#458](https://github.com/atgreen/evergreen/pull/458)).
- A function whose arithmetic first runs natively on a path it had not yet
  taken, for example double-float code compiled before it was profiled or a
  branch first reached with double floats, no longer drops back to the
  interpreter for good. Its failed fixnum guesses now recompile it from the
  types seen since, so it stays native and can reach T2
  ([#459](https://github.com/atgreen/evergreen/pull/459)).
- On x86-64 Linux, a hot function that names another with `#'f` -- such as
  `(apply #'+ l)`, `(mapcar #'1+ l)`, `(funcall #'f x)`, or a `SETF` of a
  structure or class accessor -- is now compiled by the optimizing T2 tier
  instead of stopping at T1. It still looks the function up on every call, so
  redefining it takes effect as before
  ([#460](https://github.com/atgreen/evergreen/pull/460)).
- Functions that use `MULTIPLE-VALUE-LIST` or `MULTIPLE-VALUE-PROG1` now
  compile to native code on x86-64 Linux and reach the optimizing tier. They
  used to stay in the bytecode interpreter
  ([#461](https://github.com/atgreen/evergreen/pull/461)).
- Functions that build lists with backquote now compile to native code on
  x86-64 Linux and can reach the optimizing tier; they used to stay in the
  bytecode interpreter. In a cold `(asdf:load-system :babel)` this moves
  functions such as `UIOP:NORMALIZE-PATHNAME-DIRECTORY-COMPONENT` and
  `ASDF:MAKE-OPERATION` out of the interpreter
  ([#464](https://github.com/atgreen/evergreen/pull/464)).
- Native code returns NIL for `CAR` or `CDR` of NIL instead of falling back
  to the interpreter. Each such fallback counted as a failed speculation, so
  functions that routinely reach the end of a list -- `CDDR`, `CADR`, or
  ASDF's `ABSOLUTE-PATHNAME-P` on a pathname without a directory -- were sent
  back to the interpreter for good after eight of them. They now keep their
  native code and stay eligible for further tier-up
  ([#472](https://github.com/atgreen/evergreen/pull/472)).

### Fixed

- A ppc64le background T2 worker no longer aborts with "no entry found for
  key" when it inlines a callee whose blocks are laid out out of order
  ([#503](https://github.com/atgreen/evergreen/pull/503)).
- An allocation that fails because the collector could not run now reports the
  collector's error, instead of "GC heap unavailable" or "memory allocation of
  24 bytes failed" ([#550](https://github.com/atgreen/evergreen/pull/550)).
- Compiling a `restart-case` keeps its already-compiled clauses alive while
  later clauses compile, so a collection in between can no longer leave one
  dangling ([#553](https://github.com/atgreen/evergreen/pull/553)).
- `(compile nil '(lambda ...))` keeps the lambda's `the` type checks
  ([#571](https://github.com/atgreen/evergreen/pull/571)).
- `(abs -0d0)` returns `0.0d0`, as `(abs -0.0)` already returned `0.0`; it
  returned `-0.0d0` ([#629](https://github.com/atgreen/evergreen/pull/629)).
- A function whose optimized code arrives while one of its loops is still
  running inside a call from another function no longer crashes with
  "ordinary entry is not an OSR destination". The running loop now jumps into
  the optimized code at the right place, as it already did for an outermost
  call ([#626](https://github.com/atgreen/evergreen/pull/626)).
- A garbage collection no longer fails because another thread is late to
  stop for it. A thread compiling a large function to native code checks for a
  pending collection only when the compile finishes, which under load could
  take more than the second the collector allowed; every collection requested
  meanwhile failed with "safepoint handshake failed", and could then abort the
  process. The collector now waits up to 60 seconds
  (`EGCL_SAFEPOINT_TIMEOUT_MS`), and prints a note if a wait exceeds 5
  seconds ([#617](https://github.com/atgreen/evergreen/pull/617), #609).
- Compiled code calls `vector`, `string`, `find-package` and `gensym`
  directly, and compiled `(setf (aref ...))`, `(setf (elt ...))` and
  `(setf (car|cdr ...))` store directly. Each used to be rebuilt as a form
  and re-evaluated on every call: `(vector ...)` from compiled code went from
  about 16.6 µs to 3.3 µs
  ([#603](https://github.com/atgreen/evergreen/pull/603)).
- `reduce` walks only its `:start`/`:end` range, in place. It copied the
  whole sequence into lists first, so it allocated in proportion to the
  sequence and ran about twice as slowly
  ([#601](https://github.com/atgreen/evergreen/pull/601)).
- A joined thread no longer keeps about 88 KB of memory on the default
  x86-64 musl build. The thread-caching allocator now gives an exiting
  thread's cached blocks back, and each thread's links to the functions it
  compiled are released once it has exited. The issue's joined churn now grows
  by about 1.5 KB per thread, down from 88 KB, so a thread-per-connection
  server no longer leaks most of that per connection
  ([#598](https://github.com/atgreen/evergreen/pull/598)).
- `make-instance` calls `allocate-instance` and `initialize-instance` as
  generic functions, and `initialize-instance`'s standard method calls
  `shared-initialize`, as CLHS 7.1 requires. User primary, `:before` and
  `:around` methods on `initialize-instance` and `shared-initialize`, and
  methods on `allocate-instance`, used to be skipped; only `:after` methods
  ran. A primary method that does not call the next method now leaves the
  slots uninitialized, as in other implementations. A `make-instance`,
  `allocate-instance` or other standard generic function with only `:around`,
  `:before` or `:after` methods of the user's no longer signals "no primary
  method found": the standard method is its primary method. A user
  `make-instance` method with `&key` parameters no longer makes every other
  initarg an invalid keyword argument. Classes with no such methods keep the
  fast path ([#600](https://github.com/atgreen/evergreen/pull/600)).
- Allocating an instance or a structure no longer discards every cached
  class precedence list, and a structure constructor no longer recomputes
  its class's precedence list on each call
  ([#593](https://github.com/atgreen/evergreen/pull/593)).
- A thread made with `egcl-thread:make-thread` that finishes without being
  joined no longer keeps its memory. Each kept its OS thread stack, Lisp
  stack and thread-local storage, about 90 KB of RSS, until a join that a
  thread-per-connection server never makes, so such a server ran out of
  memory. Its OS thread and stacks are now released when another thread starts
  or finishes. `join-thread` still returns its values once, and
  `thread-name` still answers until then
  ([#595](https://github.com/atgreen/evergreen/pull/595)).
- Iterating the symbols of a package that does not exist, with `loop` or
  `with-package-iterator`, signals `package-error` instead of iterating
  nothing ([#589](https://github.com/atgreen/evergreen/pull/589)).
- Allocation no longer takes the global heap lock on every object: each
  thread counts its allocations privately and publishes them when its
  allocation buffer is retired. A cons-heavy loop runs about 20% faster, and
  threads that allocate no longer serialise on that lock or on a cache line
  shared between the signal-activity word and per-thread recovery slots
  ([#587](https://github.com/atgreen/evergreen/pull/587)).
- `(make-pathname :directory '(:relative))` keeps its `(:relative)`
  directory instead of turning it into NIL, so as a `pathname-match-p`
  wildcard it no longer matches every directory
  ([#585](https://github.com/atgreen/evergreen/pull/585)).
- Several stream functions now follow the standard more closely:
  - `read-char-no-hang` on an exhausted string stream returns the eof value
    or signals `end-of-file`, instead of returning `nil`. It also rejects a
    fifth argument with a `program-error`.
  - The `end-of-file` that `read-char`, `read-char-no-hang` and `peek-char`
    signal now carries the stream that ran out, so `stream-error-stream`
    returns that stream.
  - An echo stream echoes each character only once. A character returned by
    `unread-char` is not echoed again when it is read, and `peek-char` does
    not echo the character it peeks.
  - `peek-char` with a peek type of `t` skips the characters the current
    readtable treats as whitespace.
  - A broadcast stream's `stream-external-format` and `file-string-length`
    come from its last component. With no components they are `:default`
    and 1.
  - A string output stream made with a `base-char` element type, including
    one from `with-output-to-string`, returns a `simple-base-string`.

  ([#586](https://github.com/atgreen/evergreen/pull/586))
- A file stream opened through a logical pathname reports that pathname:
  `pathname` of the stream returns it, and `logical-pathname` accepts the
  stream. `pathname` returned the translated physical pathname, and
  `logical-pathname` rejected every stream
  ([#584](https://github.com/atgreen/evergreen/pull/584)).
- A logical pathname keeps its version. `(parse-namestring
  "host:temp.dat.newest")` has version `:newest` and the namestring
  `"HOST:TEMP.DAT.NEWEST"`; the version was dropped
  ([#583](https://github.com/atgreen/evergreen/pull/583)).
- `type-of` a pathname returns `pathname`, and of a logical pathname
  `logical-pathname`. Both returned `t`
  ([#582](https://github.com/atgreen/evergreen/pull/582)).
- `egcl-ext:atomic-incf` and `egcl-ext:atomic-decf` on a structure or CLOS
  accessor place work in compiled functions and `compile`d lambdas. They
  signalled "not a structure-slot accessor"
  ([#580](https://github.com/atgreen/evergreen/pull/580)).
- A lambda parameter declared or proclaimed special is bound dynamically as
  soon as it is bound, so later `&optional`, `&key` and `&aux` init forms, and
  the functions they call, see its new value.
  `define-compiler-macro` accepts a `(setf name)` function name, and
  `compiler-macro-function` returns it. A compiler macro called on a
  `(funcall #'name ...)` form destructures the arguments after the function,
  as the standard requires
  ([#581](https://github.com/atgreen/evergreen/pull/581)).
- A `let` of a variable whose name has earmuffs, such as `*x*`, is lexical
  unless the variable was proclaimed special by `defvar`, `defparameter`,
  `declaim` or a `special` declaration, as the standard requires. Every such
  binding was dynamic, so a local `special` declaration leaked into nested
  bindings and the functions they called. The standard and built-in special
  variables are still special
  ([#579](https://github.com/atgreen/evergreen/pull/579)).
- A macro named `atomic-incf` or `atomic-decf` in any package runs its own
  expansion. Such calls ran `egcl-ext:atomic-incf`/`atomic-decf` instead,
  matched by name, which broke the Atomics library and AllegroServe
  ([#578](https://github.com/atgreen/evergreen/pull/578)).
- `(setf (symbol-function x) f)` signals a `type-error` when `x` is not a
  symbol, as the standard requires. It signalled a `simple-error`
  ([#575](https://github.com/atgreen/evergreen/pull/575)).
- `(require :asdf)` works on machines other than the one that built egcl. It
  loaded `lib/asdf.lisp` from the build's source tree; it now falls back to a
  copy of ASDF compiled into the binary. `EGCL_ASDF_FILE` names a different
  ASDF to load ([#573](https://github.com/atgreen/evergreen/pull/573)).
- Strings a program produces are garbage-collected. `format nil`,
  `get-output-stream-string`, `with-output-to-string`, `read-line` and
  `princ-to-string` used to keep every distinct result forever, so a
  long-running program that produced strings grew without bound, about 14 MB
  per 100,000 short strings. These functions now return fresh strings: two
  `read-line` results with the same text are no longer `eq`, and modifying one
  no longer changes the other. A finalizer on a string now runs once the
  string is unreachable, and a weak hash table now drops a dead key that had
  survived several collections
  ([#577](https://github.com/atgreen/evergreen/pull/577)).
- `typep` against a `(mod n)` type handles bignums: a bignum object in range
  answered NIL, and a bignum modulus crashed the runtime
  ([#572](https://github.com/atgreen/evergreen/pull/572)).
- Character case follows Unicode instead of covering ASCII only.
  `char-upcase`, `char-downcase`, `upper-case-p`, `lower-case-p`,
  `both-case-p`, the `string-upcase`, `string-downcase` and
  `string-capitalize` families, `char-equal` and `string-equal` and their
  ordering relatives, `equalp`, `equalp` hash tables and the `~(` format
  directive all agree, as in SBCL: `(char-upcase #\é)` is `#\É` and
  `(string-upcase "ö")` is `"Ö"`. A character has case only when its mapping
  is one-to-one, so `#\ß`, whose uppercase is two characters, is returned
  unchanged, and `~:@(` no longer turns `"straße"` into `"STRASSE"`
  ([#570](https://github.com/atgreen/evergreen/pull/570)).
- `apply` signals a `type-error` when its last argument is not a proper list;
  `(apply #'list 1 2)` returned `(1)`. `(/= #c(1 2))` returns T instead of
  signalling. `defpackage` signals a `package-error` for `:import-from` a
  package that does not exist, before making the package. `(setf subseq)`
  signals a `type-error` for a non-integer or out-of-range bound instead of
  clamping it ([#569](https://github.com/atgreen/evergreen/pull/569)).
- A loop that evaluates a capturing `flet` or `labels` no longer grows
  without bound. Each closure's code and captured variables were released
  only by a full garbage collection, which nothing triggered, so 600,000
  evaluations of `(let ((x i)) (flet ((f () x)) (f)))` held 584 MB. Memory now
  stays at about 40 MB without an explicit `(egcl-ext:gc :full t)`
  ([#568](https://github.com/atgreen/evergreen/pull/568)).
- `disassemble` of a closure object that has been compiled, such as a `lambda`
  or a `labels` function taken with `#'`, shows its compiled code. It reported
  "not a compiled EGCL function" although `egcl-ext:function-tier` gave its
  tier ([#567](https://github.com/atgreen/evergreen/pull/567)).
- `compile-file` no longer installs a file's `defgeneric` and `defmethod`
  forms while compiling it, as SBCL does not; they take effect when the fasl
  is loaded. A method compiled into the running image could run before the
  rest of its file had loaded: ASDF-FLV's `asdf:perform :around` method found
  its own `defvar` unbound, which stopped pure-tls's tests from loading
  ([#565](https://github.com/atgreen/evergreen/pull/565)).
- `egcl --load FILE` no longer leaves stale heap pointers in the top-level
  environment when a garbage collection runs while the load is starting.
  Under `EGCL_GC_VERIFY` with `EGCL_GC_STRESS`, every `--load` failed
  verification ([#564](https://github.com/atgreen/evergreen/pull/564)).
- `compile-file` compiles a top-level `(defgeneric (setf name) ...)` again.
  Since #538 it signalled a `program-error` with no message, so usocket, and
  everything built on it, no longer compiled
  ([#562](https://github.com/atgreen/evergreen/pull/562)).
- Sandbox mode refuses `egcl::%socket-listen` and `egcl::%socket-accept`, as
  it already refused `egcl::%socket-connect`. A sandboxed program could open a
  listening socket ([#560](https://github.com/atgreen/evergreen/pull/560)).
- `(compile nil '(lambda ...))` and `(compile 'name '(lambda ...))` compile
  the lambda expression to bytecode, expanding its macros during `compile`.
  The result expanded them at its first call, so a macro with a compile-time
  effect had not run when `compile` returned
  ([#559](https://github.com/atgreen/evergreen/pull/559)).
- `(compile 'name)` of a function defined with `defun` compiles it to bytecode
  now, keeping the same function object. It returned success and left the
  function interpreted until enough calls made it hot
  ([#558](https://github.com/atgreen/evergreen/pull/558)).
- `makunbound`, `egcl-ext:cas` and `egcl-ext:atomic-incf`/`atomic-decf` signal
  an error for a constant (`nil`, `t`, a keyword or a `defconstant` name).
  `(makunbound 'pi)` made `pi` unbound, `cas` could replace `pi`'s value, and
  `cas` or `atomic-incf` on `nil` or `t` panicked in the runtime
  ([#554](https://github.com/atgreen/evergreen/pull/554)).
- In a file compiled with `compile-file`, a `restart-case` clause that names
  an enclosing local variable only through a macro now reads and sets that
  local, as in SBCL. It used the global value instead
  ([#552](https://github.com/atgreen/evergreen/pull/552)).
- In compiled code, a `setq` or `setf` of a variable declared special by an
  enclosing `locally` or binding form's `declare` now sets the dynamic value,
  as in SBCL. When a lexical binding of the same name enclosed the
  declaration, the store went to that lexical binding instead; inside a
  closure or `restart-case` clause this happened in every tier. `set` now
  changes the symbol's value cell, never a lexical binding of the same name
  ([#551](https://github.com/atgreen/evergreen/pull/551)).
- A closure saved in an image keeps the `macrolet` definitions of the place it
  was written. After the image was restored, a closure that called such a
  local macro signalled an error
  ([#548](https://github.com/atgreen/evergreen/pull/548)).
- A user method on `make-instance` can `call-next-method` into the built-in
  behaviour, and `next-method-p` is true there. It signalled that
  CALL-NEXT-METHOD was undefined. `(make-instance 'name ...)` also now runs
  methods specialized on the class object, as `(make-instance (find-class
  'name) ...)` does. A method that is the only applicable one now sees its own
  `next-method-p`, not an enclosing method's
  ([#546](https://github.com/atgreen/evergreen/pull/546)).
- In compiled code, a LOOP's `it` now means the conditional's test value only
  as the bare operand of the first clause after `when`, `if` or `unless`, as
  in `when x collect it`. Before, every `it` in that branch got the test
  value. This included an `it` inside a compound form, which hid an enclosing
  variable named `it`, and an `it` in a later `and` or `else` clause. As a
  result, `(loop for x in '(1 nil 2) when x collect (cons it it))` returned
  `((1 . 1) (2 . 2))` instead of signalling an unbound-variable error, as SBCL
  and the interpreter do
  ([#545](https://github.com/atgreen/evergreen/pull/545)).
- A `labels` function called from inside a `macrolet` that defines a macro of
  the same name as one of its siblings now calls that sibling, as in SBCL. It
  signalled an undefined-function error, because the function looked up its
  siblings where it was called rather than where it was defined
  ([#544](https://github.com/atgreen/evergreen/pull/544)).
- Compiled code now leaves a `let` correctly on a non-local exit when a closure
  captures one of its variables. This covers a `throw` to a `catch`, a
  `return-from`, a `go`, an `unwind-protect` cleanup and a `handler-case`
  clause. Before, the code after the exit kept that `let`'s bindings. A
  variable the `let` shadowed read the inner value, and a closure created
  afterwards captured the inner binding. The interpreter was already correct
  ([#542](https://github.com/atgreen/evergreen/pull/542)).
- A closure or `flet`/`labels` function run by the source evaluator (`eval`,
  or a form `compile-file` keeps as source) expands local macros with the
  `macrolet`s where it was written, not the caller's. A `(loop-finish)` in a
  closure now ends the loop that lexically contains it: inside an inner loop
  it used to end that inner loop, or signal `GO: no such tag`
  ([#539](https://github.com/atgreen/evergreen/pull/539)).
- `typep` with an `or`, `and` or `complex` type specifier whose earlier part
  runs a `satisfies` predicate or `deftype` expander that allocates now tests
  the later parts correctly. A collection during the earlier test could leave
  the later parts read from moved memory, and the test answered wrongly
  ([#540](https://github.com/atgreen/evergreen/pull/540)).
- A `defun` that `compile-file` keeps as source, such as one whose body
  contains a `defclass`, `defmethod` or `defmacro`, has its macros expanded at
  compile time. Loading the fasl expanded them again, where
  `*compile-file-pathname*` is NIL, so LIFT's `deftestsuite` inside a function
  failed to load ([#538](https://github.com/atgreen/evergreen/pull/538)).
- `compile-file` processes the body of a top-level `macrolet` or `locally` as
  top-level forms, so an `(eval-when (:compile-toplevel) ...)` that a
  `macrolet`'s macros expand into runs during compilation, not only when the
  fasl is loaded. CFFI's `types.lisp` compiles again
  ([#537](https://github.com/atgreen/evergreen/pull/537)).
- A `restart-case` clause whose restart is named `:loop` no longer makes
  `compile-file` fail with `Unsupported LOOP action "NIL"`, and defining such
  a function no longer signals that error to the caller's handlers. Restart
  and `handler-case` clauses, and `handler-bind` types, are no longer walked
  as code when a function body is macroexpanded, so a clause type such as
  `(or error storage-condition)` in a `defun` also matches. `(macro-function
  :loop)` is NIL ([#536](https://github.com/atgreen/evergreen/pull/536)).
- `handler-case` and `handler-bind` clauses whose type is a compound type
  specifier, such as `(or error storage-condition)` or
  `(and error (not warning))`, now match a condition of that type, as `typep`
  does. They never matched, so the condition went on to outer handlers. A
  `*break-on-signals*` set to a compound type is also tested with `typep`. An
  outer `handler-case` that catches a condition an inner one declined now
  receives the condition; it received NIL
  ([#535](https://github.com/atgreen/evergreen/pull/535)).
- `~wF` with a width and no digit count no longer keeps the zeros left by
  rounding a value just off a short decimal: `(format nil "~8F"
  -112.400000000001d0)` prints `"  -112.4"`, not `"-112.400"`, as in SBCL
  ([#534](https://github.com/atgreen/evergreen/pull/534)).
- `print-unreadable-object` with `:identity t` prints the object's address, so
  different objects print differently. It printed `sxhash`, which is the same
  for every standard object, so all instances shared one identity
  ([#526](https://github.com/atgreen/evergreen/pull/526)).
- `make-instance` called through `#'make-instance` (with `apply` or `funcall`)
  inside an `initialize-instance :after` method of an instance that was itself
  made that way no longer signals "no applicable method for generic function
  MAKE-INSTANCE". flexi-streams makes its streams this way, so
  `(flex:make-flexi-stream s :external-format :utf-8)` failed
  ([#525](https://github.com/atgreen/evergreen/pull/525)).
- `(make-hash-table :synchronized t)` now makes a synchronized table. The
  option was ignored, so threads sharing such a table could corrupt it:
  `gethash` returned another key's value, and the table kept duplicate keys
  and lost writes. `egcl-ext:hash-table-synchronized-p`, documented but
  missing, now reports the option
  ([#524](https://github.com/atgreen/evergreen/pull/524)).
- `eval` of a `defun` or `defmethod` whose body `setf`s an accessor (or any
  `(setf name)` function) no longer signals a spurious `program-error` to the
  caller's `handler-bind` handlers. A handler that answers a genuine
  macro-expansion error during such an `eval` with `return-from`, `go`,
  `throw`, a `handler-case` clause or `invoke-restart` now leaves as it should;
  `eval` used to return normally after the handler ran. Test harnesses such as
  LIFT reported a correct run-time `defclass`/`define-object` test as failing
  ([#515](https://github.com/atgreen/evergreen/pull/515)).
- `compile-file` reports an error a macro's expander signals while the file is
  compiled, as SBCL and CCL do: it prints a `caught ERROR` note naming the
  definition and the form to `*error-output*`, returns `t` for both
  `warnings-p` and `failure-p`, and compiles the form into code that signals a
  `program-error` ("Execution of a form compiled with errors") when it is
  reached. EGCL used to print nothing, return `nil` for both, and expand the
  macro again at run time, so a function compiled this way behaved differently
  from the rest of its fasl. Macros used in `defmethod` bodies are still
  expanded when the method is loaded
  ([#517](https://github.com/atgreen/evergreen/pull/517)).
- A generic function created implicitly by `defmethod` now has `&key` with
  no keyword names in its lambda list, as CLHS 7.6.1 specifies, so a later
  method may accept different keywords. EGCL used to copy the first method's
  keyword names into the generic function and reject such a method as not
  congruent. Keywords declared by `defgeneric` are still required of every
  method, and a call still rejects a keyword no applicable method accepts
  ([#510](https://github.com/atgreen/evergreen/pull/510)).

- A function with more than 16 parameters no longer reads garbage for its
  17th and later parameters once it is compiled to native code on x86-64.
  Native entry loaded parameter *i* from `[rdi + 8*i]` with an 8-bit
  displacement, so the 17th parameter's offset of 128 wrapped to -128 and
  read the caller's stack: stale values, function objects or raw words.
  Loop entry (OSR) into a function with more than 16 locals read the same
  wrong slots. Since `&optional`, `&rest` and `&key` functions enter native
  code, keyword-heavy functions hit it; pure-tls's TLS handshake signalled
  `The value :CLIENT-PRIVATE-KEY is not of type FIXNUM`, so every HTTPS
  download in `ocicl install` failed. These loads now use a 32-bit
  displacement when they need one
  ([#509](https://github.com/atgreen/evergreen/pull/509)).

- Deep recursion no longer signals a spurious `STORAGE-CONDITION` on Linux
  AArch64. That target used a 512 KiB control stack, the fallback for
  platforms without native compilation, although its native frames are the
  largest of any supported target. Recursion overflowed somewhere between
  5,000 and 10,000 deep, where x86-64 and IBM Z manage 50,000; AArch64 now
  gets the same 4 MiB they do. `EGCL_STACK_SIZE` still overrides it, and the
  mapping stays lazy, so untouched pages cost no resident memory ([#501](https://github.com/atgreen/evergreen/pull/501)).
- `EGCL_T2_LOG` and the `compile` trace now say why the AArch64 compilers
  refused a function. Every refusal shared one opaque code, so the log named
  neither the operation nor the place that declined it; a decline now names
  the refused operation, or the exact emitter line for a shape peculiar to
  that target ([#501](https://github.com/atgreen/evergreen/pull/501)).
- Compiled code no longer returns stale multiple values from a form that
  yields one value. When one arm of an `IF`, `WHEN`, `UNLESS` or `AND` could
  return several values and the other could not, the single-valued arm
  returned its value together with the values of an earlier call:
  `(let ((r (floor 7 2))) (if neg (not r) r))` returned 3 and 1, and
  `(multiple-value-list (if neg (g n) 5))` could give `(3 1)` instead of
  `(5)`. `TAGBODY` likewise returned its last statement's values instead of
  `NIL`, and `THROW` of a single-valued form transferred stale values with it.
  The interpreter was already correct; compiled code was wrong at every tier.
  Such functions can now also use the optimizing tier's direct-call entry,
  which cuts the cost of calling them from native code by about 40%
  ([#500](https://github.com/atgreen/evergreen/pull/500)).
- `COMPILE-FILE` compiles a file whose top-level `MACROLET` expands to
  `(defgeneric (setf NAME) ...)`, as zpb-ttf's `bounding-box.lisp` does. It
  used to warn "could not pre-read this file (PROGRAM-ERROR)" and write a
  source-only artifact that re-read the text at load time. A `:METHOD` option
  in such a `DEFGENERIC` now also sees the `MACROLET`'s local macros
  ([#488](https://github.com/atgreen/evergreen/pull/488)).
- In an image restored with `--image` or a delivered executable, a function
  that came with the image reports the tier its native code runs at.
  `egcl-ext:function-tier` used to answer 0 for it by name and by object after
  its T1 or T2 code was installed and running, and its invocation count kept
  advancing after its T2 code was installed; functions defined after the
  restore were unaffected. The saved
  image no longer carries a second copy of each such function's body
  ([#490](https://github.com/atgreen/evergreen/pull/490)).
- A foreign library handle from `egcl-ffi:load-foreign-library` saved in an
  image is now usable after `--image` or in a saved executable. Restoring an
  image reopens its libraries in load order under the same handles, as SBCL
  does. A handle whose library cannot be reopened is dead: a symbol lookup
  through it names the library and the reason, closing it does nothing, and
  saving the image again keeps it so a later restore retries the library.
  Before, image save reset every handle to null, so lookups signalled "null
  library handle" and closing one signalled an error, which broke reload code
  that closes a library before loading it again. Images now use format version
  12; images saved by older releases must be rebuilt
  ([#492](https://github.com/atgreen/evergreen/pull/492)).
- `INTERN` into the `KEYWORD` package keeps a colon in its string as part of
  the symbol's name. `(intern ":DECK" :keyword)` used to name `DECK` and
  `(intern "A:B" :keyword)` named `B`; they now name `":DECK"` and `"A:B"`,
  print as `:|:DECK|` and `:|A:B|`, read back as the same symbols, and survive
  `COMPILE-FILE` and `LOAD`. `FIND-SYMBOL`, `SYMBOL-NAME` and the printer agree
  with them, and `:|:DECK|` reads as the keyword named `":DECK"`. In any
  package, an escaped colon right after a single package marker is part of the
  name: `GQ:|:X|`, which is how an exported symbol named `":X"` prints, reads
  back as that symbol instead of as `GQ::X`
  ([#493](https://github.com/atgreen/evergreen/pull/493)).
- `--heap-size` and `EGCL_HEAP_SIZE` size the heap. Both were parsed and then
  ignored, so the heap stayed at 512 MB unless `EGCL_HEAP_MB` was set.
  `--heap-size` takes precedence over `EGCL_HEAP_SIZE`, which takes precedence
  over `EGCL_HEAP_MB`. A heap smaller than 256 MB gets a nursery of a quarter of
  the heap rather than a fixed 64 MB, and a restored image that does not fit the
  requested heap grows it to three times the image's data instead of failing.
  A size of 0 is rejected, and a heap the system cannot reserve is reported as
  such at startup instead of panicking at the first allocation
  ([#498](https://github.com/atgreen/evergreen/pull/498)).
- When live data outgrows the heap, the program gets a `STORAGE-CONDITION`
  instead of a panic with "GC minor evacuation exhausted regions", in
  interpreted and compiled code alike. Nothing moves when the survivors cannot
  be placed, and a major collection runs before the condition is signalled, so a
  program that handles it and drops its data can continue. Compiled code that
  has a handler, `UNWIND-PROTECT` or dynamic binding of its own active at that
  moment can still report "invalid native transfer capture" instead
  ([#498](https://github.com/atgreen/evergreen/pull/498)).
- A `THROW` from the default form of a `&key` or `&aux` parameter to a
  `CATCH` in the interpreted caller reaches that catch. Once the caller had
  been called a few times, the throw escaped as an internal error such as
  `__THROW__:1758` instead
  ([#470](https://github.com/atgreen/evergreen/pull/470)).
- On x86-64, a fully optimized function that left-shifted a masked value, such
  as `(ash (logand a #xFFFFFFFFFF) 30)`, returned a wrapped fixnum instead of
  the bignum result. It now checks the shift for overflow
  ([#456](https://github.com/atgreen/evergreen/pull/456)).
- `MULTIPLE-VALUE-LIST` returns exactly one value once its function is
  compiled. Compiled code used to leave the collected values behind as stale
  secondary values, so `(multiple-value-bind (a b) (f))` around a function
  ending in `MULTIPLE-VALUE-LIST` saw `b` as the second collected value instead
  of `NIL` ([#461](https://github.com/atgreen/evergreen/pull/461)).
- A backquote form returns exactly one value in the bytecode interpreter.
  It used to pass on the values of its last evaluated operand, so
  `` (multiple-value-list `(,(values 1 2))) `` returned `(1 2)` instead of
  `((1))` ([#464](https://github.com/atgreen/evergreen/pull/464)).
- Compiled code calls another package's `LENGTH`, `FIRST`, `ELT` and the
  other functions the compiler calls directly, rather than the CL function
  of the same name. A generic function such as `(defgeneric my:length (x))`
  was bypassed by callers compiled with `COMPILE-FILE`, which signalled
  "The value 42 is not of type SEQUENCE"
  ([#446](https://github.com/atgreen/evergreen/pull/446)).
- A saved image keeps every special proclamation. A valueless `(defvar flag?)`
  without earmuffs, the shape of Parenscript's compiler flag, became lexical
  after a restore, so a `LET` of it no longer reached the functions reading it.
  The image format is now version 11; images saved by 0.0.10 must be saved
  again ([#447](https://github.com/atgreen/evergreen/pull/447)).
- A thread made by `egcl-thread:make-thread` gets the main thread's stack size
  (the soft `RLIMIT_STACK`, else 8 MiB), or `EGCL_STACK_SIZE` when that is set,
  instead of Rust's 2 MiB. Threads overflowed with `STORAGE-CONDITION` at about
  a quarter of the main thread's recursion depth; a larger `RUST_MIN_STACK` is
  still honoured ([#448](https://github.com/atgreen/evergreen/pull/448)).
- On x86-64 Linux, a function that creates a closure capturing no local
  variables, such as `(mapcar (lambda (x) (* x 2)) list)`, is now compiled
  natively and can reach the optimizing tier. Previously any `LAMBDA` kept the
  whole function in the interpreter. Closures that capture variables still
  run interpreted
  ([#463](https://github.com/atgreen/evergreen/pull/463)).

## 0.0.10 - 2026-10-19

### Added

- `install-egcl-forks` also installs the EGCL forks of Ironclad, pure-tls and
  chipz, which HTTPS through pure-tls and archive extraction with chipz need
  ([#429](https://github.com/atgreen/evergreen/pull/429)).
- `EGCL-MOP:FORWARD-REFERENCED-CLASS`, the metaclass of a class named as a
  superclass before it is defined. `CLASS-OF` answers it and such a class
  prints as `#<FORWARD-REFERENCED-CLASS FOO>`, as on SBCL; it is a `CLASS` but
  not a `STANDARD-CLASS`, since nothing has yet said what its instances look
  like, and it becomes an ordinary class -- the same object, so references
  already taken to it stay `EQ` -- when the definition arrives
  ([#417](https://github.com/atgreen/evergreen/pull/417)).
- `EGCL-MOP`, the metaobject-protocol package `docs/egcl-lisp-api.md`
  specified but the bootstrap never installed. It provides
  `CLASS-DIRECT-SUPERCLASSES`, `CLASS-DIRECT-SUBCLASSES`,
  `GENERIC-FUNCTION-METHODS`, `CLASS-FINALIZED-P`, `FINALIZE-INHERITANCE`,
  `INTERN-EQL-SPECIALIZER` and `VALIDATE-SUPERCLASS`, and is the home of
  `CLASS-SLOTS`, `CLASS-DIRECT-SLOTS` and the `SLOT-DEFINITION-*` readers.
  `COMMON-LISP-USER` uses it, so those names stay reachable unqualified where a
  person is typing ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#371](https://github.com/atgreen/evergreen/issues/371)).
- `DEFCLASS`'s `:METACLASS` takes effect: `CLASS-OF` answers the named
  metaclass instead of `STANDARD-CLASS`. The option was parsed and discarded,
  so a kernel that tells its own classes from plain ones with
  `(eql (class-of (find-class name)) (find-class 'its-metaclass))` -- Gendl's
  does -- saw every class as plain
  ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#371](https://github.com/atgreen/evergreen/issues/371)).

- Package-local nicknames (CDR 10). `DEFPACKAGE` accepted `:LOCAL-NICKNAMES`
  and dropped it, so a library's `(:local-nicknames (:a :alexandria))` left
  `a:define-constant` unreadable: "package not found: A". The option now
  takes effect, the reader, `FIND-PACKAGE`, `INTERN` and `FIND-SYMBOL`
  resolve a prefix through the current package's local nicknames before the
  global names, `EGCL-EXT` exports `ADD-PACKAGE-LOCAL-NICKNAME`,
  `REMOVE-PACKAGE-LOCAL-NICKNAME`, `PACKAGE-LOCAL-NICKNAMES` and
  `PACKAGE-LOCALLY-NICKNAMED-BY-LIST` as the API document already listed
  them, `:PACKAGE-LOCAL-NICKNAMES` is on `*FEATURES*`, and UIOP's
  `DEFINE-PACKAGE` finds the functions in `EGCL-EXT`
  ([#372](https://github.com/atgreen/evergreen/pull/372)).
- Socket endpoint queries: `(egcl::%socket-listener-local-name listener)`,
  `(egcl::%socket-local-name stream)` and `(egcl::%socket-peer-name stream)`
  report an endpoint as an octet vector and a port together, four elements for
  IPv4 and sixteen for IPv6. A library adapter could previously learn a
  listener's port but never any address, so usocket's `get-local-address`,
  `get-peer-address`, `get-peer-port` and `get-local-name` had no primitive
  behind them and AllegroServe could not log or print a socket
  ([#352](https://github.com/atgreen/evergreen/pull/352),
  [#366](https://github.com/atgreen/evergreen/pull/366);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- `egcl-ext:chdir` and `egcl-ext:delete-directory`, and through them
  `uiop:chdir`, `uiop:delete-empty-directory` and `uiop:delete-directory-tree`,
  which signalled "Not (currently) implemented on EGCL"
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- The manual now documents every `EGCL-EXT` function, including `SETENV`,
  `UNSETENV`, `GET-PRECISE-TIME`, `REAL-TIME-NANOSECONDS`, the package-local
  nickname functions, `HASH-TABLE-WEAKNESS`, the funcallable-instance functions,
  declaration introspection and the embedded-file functions. Each one has an
  anchored entry and a symbol-index row
  ([#409](https://github.com/atgreen/evergreen/pull/409)).

### Fixed

- A thread waiting in `SLEEP` no longer holds up a garbage collection. A
  collection needed by another thread used to wait on the sleeper and give up,
  so a server whose main thread parks in `(loop (sleep 60))` could abort a
  worker with "memory allocation of 16 bytes failed"
  ([#434](https://github.com/atgreen/evergreen/pull/434)).
- `LOAD` and `COMPILE-FILE` bind `*PACKAGE*` for the extent of the file, so
  a file that sets `*PACKAGE*` and then starts a thread stays in its own
  package. The file's package used to live in the global value every thread
  shares, and the loader could read its next form in another package, then
  report "undefined function" for a function the file had just defined
  ([#433](https://github.com/atgreen/evergreen/pull/433)).
- A `MAKE-PACKAGE` name clash or a `DELETE-PACKAGE` of a missing package
  signals a `SIMPLE-PACKAGE-ERROR` whose report says what failed, such as
  "a package named FOO already exists". Both are still correctable with
  `CONTINUE`; until now they signalled a bare `PACKAGE-ERROR` with no message
  ([#436](https://github.com/atgreen/evergreen/pull/436)).
- `SLEEP` inside a fiber suspends only that fiber, so other fibers on the
  same carrier keep running. It used to block the carrier thread until it
  woke. In a pinned fiber it follows `EGCL-FIBER:*PINNED-BLOCKING-ACTION*`,
  like the other blocking operations
  ([#437](https://github.com/atgreen/evergreen/pull/437)).
- A macro defined inside a `LET` keeps its captured binding across a saved
  image, as a `DEFUN` or `DEFMETHOD` in the same `LET` already did. Only a
  macro's name, lambda list and body reached the image, and every restored
  expander was rebound to the fresh global environment, so an expansion that
  mentioned a captured variable signalled `UNBOUND-VARIABLE`. anaphora defines
  `SYMBOLIC` and `ANAPHORIC` inside `(with-unique-names ...)`, so in any saved
  core every anaphoric macro failed with "the variable
  `ANAPHORA::S-INDICATOR` is unbound". A macro now records which captured frame
  it closed over, and restored definitions share one frame rather than each
  getting a copy, so a binding several of them see stays shared
  ([#431](https://github.com/atgreen/evergreen/pull/431);
  issue [#426](https://github.com/atgreen/evergreen/issues/426)).
- A failed socket call carries its operating system error number, so
  usocket reports a port in use as `ADDRESS-IN-USE-ERROR` and a refused
  connection as `CONNECTION-REFUSED-ERROR`. Both used to arrive as
  `UNKNOWN-ERROR` with errno 0, and a port probe that handled only
  `ADDRESS-IN-USE-ERROR` retried the same busy port forever. Socket failures
  signal the new `EGCL-EXT:SOCKET-OS-ERROR`, which is still a `FILE-ERROR`, and
  `EGCL-EXT:SOCKET-OS-ERROR-ERRNO` reads the number. The usocket fork also
  accepts listeners with the default `character` element type
  ([#432](https://github.com/atgreen/evergreen/pull/432);
  issue [#427](https://github.com/atgreen/evergreen/issues/427)).
- A generic function dispatches an interpreted lambda or closure on the
  `FUNCTION` class. `CLASS-OF`, `FUNCTIONP` and `TYPEP` all answered
  `FUNCTION`, but dispatch disagreed with them in both tiers, so
  `(brt (lambda (y) y))` chose a `LIST` method over a `FUNCTION` one and a
  `LIST`-only generic accepted a lambda outright. A compiled lambda and
  `#'car` were already correct
  ([#423](https://github.com/atgreen/evergreen/pull/423)).
- `SETQ` of a parameter whose name is special reaches the callees that read it.
  Such a parameter is bound in the frame and then made dynamic, and the
  assignment path tried the frame before the value cell, so it updated a stale
  frame copy: after `(defun f (&key ((:indent *ind*) *ind*)) (setq *ind* 0)
  (reader))`, `(f :indent t)` answered the old value instead of `0`
  ([#425](https://github.com/atgreen/evergreen/pull/425)).
- A class metaobject prints its own name: `#<STANDARD-CLASS UC>` and
  `#<STRUCTURE-CLASS ST>`, as on SBCL, CCL and ECL. Once a class became an
  object it printed through the generic instance path, so every standard class
  in the image printed as the identical `#<STANDARD-CLASS>` -- useless in a
  backtrace or a slot dump
  ([#402](https://github.com/atgreen/evergreen/pull/402)).
- A class restored from an image keeps the slot shadowing its
  `:allocation :class` declaration implies. The names a class declares class
  allocated were not written to the core, so a class finalized for the first
  time *after* a restore -- which is what subclassing a restored class does --
  rebuilt its layout without them, and an inherited instance slot of the same
  name wrongly entered that layout. CLST format version 4 records the field
  ([#393](https://github.com/atgreen/evergreen/pull/393)).
- `INTERRUPT-THREAD` interrupts a running thread. It posted a value nothing
  ever observed, read only after the entry function returned, so rather than
  interrupting a thread it overrode the return value of one already finishing.
  It now raises the pending signal taken at every tier's safepoints, and sends
  a directed `SIGUSR1` so a thread parked in a syscall wakes; a wait with no
  deadline no longer parks forever through the signal
  ([#373](https://github.com/atgreen/evergreen/pull/373)).
- `FILE-POSITION` works on a Gray stream. It signalled a bare `STREAM-ERROR`,
  so cl-tar could not extract any archive and ocicl could not install a
  package. The Gray protocol gains `STREAM-FILE-POSITION`, as in SBCL, which
  `FILE-POSITION` calls and `EGCL-GRAY-STREAMS` exports
  ([#428](https://github.com/atgreen/evergreen/pull/428)).
- A compiled method shows up as `(METHOD <generic-function>)`, with its
  qualifier, in `EGCL_SPROF` profiles, the tiering log and backtraces, instead
  of an anonymous `METHOD` shared by every method
  ([#421](https://github.com/atgreen/evergreen/pull/421)).
- A form whose value is only tested no longer leaks its other values into the
  result. `(if (values x 99) x (list x))` returned X and 99 in compiled code,
  and a compiled `UNLESS` could even return the test's value instead of `NIL`;
  `IF`, `WHEN`, `UNLESS`, `COND` and non-final `AND`/`OR` forms now pass on
  only the primary value, as in SBCL
  ([#419](https://github.com/atgreen/evergreen/pull/419)).
- A fill-pointer or adjustable string made with `:element-type 'base-char`
  is a `BASE-STRING`: `ARRAY-ELEMENT-TYPE` answers `BASE-CHAR`, it is stored
  8-bit, and `VECTOR-PUSH`, `VECTOR-PUSH-EXTEND` and `ADJUST-ARRAY` signal
  `TYPE-ERROR` for a wider character instead of storing it, as in SBCL
  ([#418](https://github.com/atgreen/evergreen/pull/418)).
- A class redefinition or `CHANGE-CLASS` no longer moves the instance it
  updates, so an object keeps its identity: two references to it stay `EQ`, an
  `EQ` hash table still finds it, and `SLOT-MAKUNBOUND` returns the object the
  caller passed in. Instances hold their slots indirectly, as in SBCL, and an
  update replaces that storage rather than the object. A metaclass that gains a
  slot now gives its existing classes that slot without disturbing them, which
  previously corrupted the class registry
  ([#414](https://github.com/atgreen/evergreen/pull/414)).
- `BASE-CHAR`, `STANDARD-CHAR` and `EXTENDED-CHAR` are proper subtypes of
  `CHARACTER` at more doors: `(coerce #\λ 'base-char)` and `(coerce #\a
  'extended-char)` signal `TYPE-ERROR` instead of returning the character,
  the interpreter's `(the base-char #\λ)` signals one, and an
  `EXTENDED-CHAR` array is a `CHARACTER` string. `COERCE` to a string
  signals `TYPE-ERROR` for an element that is not a character instead of
  dropping it, and `MAKE-ARRAY` rejects `:initial-contents` whose length is
  not the vector's instead of padding it
  ([#416](https://github.com/atgreen/evergreen/pull/416)).
- String literals are `CHARACTER` strings, as in SBCL, so a copy of one can
  hold any character: `(setf (char (copy-seq "abc") 0) #\λ)` works, and a
  literal is the same kind of string whether its file was compiled or not.
  `EGCL-EXT:READTABLE-BASE-CHAR-PREFERENCE` (`:SYMBOLS`, `:STRINGS`, `:BOTH`
  or `NIL`, as in SBCL) can make a readtable read compact 8-bit literals
  instead ([#411](https://github.com/atgreen/evergreen/pull/411)).
- Several threads may read one hash table at once, as in SBCL; only writes
  need exclusive access. Concurrent lookups in a table keyed by conses or
  other objects hashed by address used to crash once a garbage collection
  moved its keys ([#400](https://github.com/atgreen/evergreen/pull/400)).
- Threads running interpreted code no longer crash while another thread
  collects garbage. A `:synchronized t` hash table now takes its lock on every
  operation as documented; it never did, and the interpreter's shared
  macro-expansion cache is such a table
  ([#398](https://github.com/atgreen/evergreen/pull/398)).
- A class is an object rather than its own name symbol, so `(symbolp
  (find-class 'foo))` is false and `DEFCLASS` returns the class as CLHS
  requires. A class with a user `:METACLASS` is now an instance of it:
  `TYPEP` holds, a method specialized on the metaclass applies and is
  preferred over one on `STANDARD-CLASS`, and the class carries the
  metaclass's slots (issue
  [#384](https://github.com/atgreen/evergreen/issues/384),
  [#399](https://github.com/atgreen/evergreen/pull/399)).
- A slot added by redefining a class takes its `:INITFORM` on instances that
  already exist, as CLHS 4.3.6 requires: `(defclass c () ())`, an instance,
  then `(defclass c () ((s :initform 42)))` left `S` unbound where it now
  answers 42. `UPDATE-INSTANCE-FOR-REDEFINED-CLASS` is called for the update,
  so a method specialized on it participates and receives the added slots, the
  discarded slots and their values
  ([#412](https://github.com/atgreen/evergreen/pull/412),
  [#408](https://github.com/atgreen/evergreen/pull/408)).
- A class that is only a forward reference -- named as a superclass and not yet
  defined -- can no longer be instantiated, as in SBCL: `MAKE-INSTANCE`,
  `ALLOCATE-INSTANCE` and `CHANGE-CLASS` each used to return an object with no
  slots, built against a layout the eventual `DEFCLASS` then contradicts.
  `EGCL-MOP:CLASS-FINALIZED-P` also stops claiming T for such a class, or for a
  class that inherits from one, and `EGCL-MOP:FINALIZE-INHERITANCE` signals for
  them instead of silently doing nothing
  ([#406](https://github.com/atgreen/evergreen/pull/406)).
- A class named as a superclass **before** it is defined, and then defined with
  a `:METACLASS` that has slots, is an instance of that metaclass like any
  other. `CLASS-OF` answered the metaclass while `TYPEP`, `SLOT-EXISTS-P`,
  `SLOT-VALUE`, the metaclass's slot accessors and method dispatch all still
  saw a plain `STANDARD-CLASS`. A kernel that appends a late-defined mixin to
  every class -- Gendl's does -- hit this on exactly the classes that mattered
  ([#404](https://github.com/atgreen/evergreen/pull/404);
  issue [#384](https://github.com/atgreen/evergreen/issues/384)).
- An inherited slot is reachable when the superclass is defined after the
  subclass: `(defclass sub (super) ...)` followed by `(defclass super ()
  ((a)))` left `A` unusable on an instance of `SUB`. Redefining a class now
  updates its subclasses' instance layouts, and no longer discards its record
  of them ([#399](https://github.com/atgreen/evergreen/pull/399)).
- `TYPE-OF` answers the class itself when the class has no proper name, rather
  than a name that no longer finds it
  ([#399](https://github.com/atgreen/evergreen/pull/399)).
- A built-in class is an object rather than an integer. `(find-class
  'integer)` was the fixnum -14, so `(eql -14 (find-class 'integer))` and
  `(integerp (find-class 'standard-class))` were both true, `(class-name -14)`
  answered `INTEGER`, a method specialized on `BUILT-IN-CLASS` applied to the
  integer -14, and a class printed as `-40`
  ([#396](https://github.com/atgreen/evergreen/pull/396)).
- A readtable built from the standard syntax with `GET-MACRO-CHARACTER` and
  `SET-MACRO-CHARACTER`, as named-readtables' `(:merge :standard)` does, reads
  the forms inside a list with that readtable. It used the standard one, so a
  pythonic `"""…"""` string inside a form broke apart (40ants-doc). `LOAD` also
  binds `*READTABLE*`, so a loaded file's `IN-READTABLE` no longer leaks into
  the code that loaded it
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- A constant one library defines while it compiles no longer becomes the
  symbol another package reads for that name. ironclad's and flexi-streams'
  `+BUFFER-SIZE+` turned up in drakma and salza2, and salza2's `DEFPARAMETER`
  of it failed as an assignment to a constant
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- `COMPILE-FILE` no longer panics on data shaped like a `DEFCONSTANT` form,
  such as the `CASE` key `((defconstant) …)`. A literal string with a fill
  pointer, such as cl-interpol's `#?"…"` reads, compiles into the fasl
  instead of making the whole file source-only, and the read-time `#.` forms
  of a file that is still source-only see its source as
  `*COMPILE-FILE-PATHNAME*`
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- A saved image keeps every compiled function. One whose code referred to a
  constant the compiled-unit format could not hold, such as a package, a hash
  table, a host-built closure or a `HANDLER-BIND` handler, was left out with a
  warning and ran interpreted after a restart; an image of ocicl lost several
  hundred. Compiled files now hold package and hash-table constants too
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- `BASE-CHAR` is the element type of an 8-bit base string, consistently.
  `TYPEP` called every character a `BASE-CHAR` while `BASE-STRING` matched
  only 8-bit storage, `MAKE-STRING` ignored `:element-type`, and `MAP`,
  `COERCE` and `MAKE-SEQUENCE` built 32-bit strings for `BASE-STRING`, so
  `(make-array 5 :element-type 'base-char)` and `(map 'base-string ...)`
  answered strings `TYPEP` refused as base strings, and `TYPE-OF` called
  every string `SIMPLE-BASE-STRING`. `BASE-CHAR` is now the code points
  below 256 and `EXTENDED-CHAR` the rest, `UPGRADED-ARRAY-ELEMENT-TYPE` and
  `ARRAY-ELEMENT-TYPE` answer `BASE-CHAR` for 8-bit strings (a string
  displaced onto one included), the constructors honor the requested width
  (a wide character is a `TYPE-ERROR`), `COPY-SEQ`, `SUBSEQ`, `REVERSE` and the `REMOVE`,
  `SUBSTITUTE` and `REMOVE-DUPLICATES` families keep a string's width, and
  `TYPE-OF` answers `(SIMPLE-ARRAY CHARACTER (n))` for a 32-bit string (ansi
  sequences `MAP.SPECIALIZED-STRING.3`, `LENGTH.STRING.6`; the sequences
  chapter now passes completely and joins the gate)
  ([#403](https://github.com/atgreen/evergreen/pull/403)).
- Hash tables find every key their test calls equal. An `EQUALP` table missed
  `1d0` for `1`, `0.5` for `1/2`, an adjustable or fill-pointer vector or string
  for a simple one, a string for a vector of its characters, and equal
  structures and hash tables; `EQUAL` tables missed fill-pointer strings and lost
  an adjustable-vector key once an element changed. `EQL` (and `EQL`/`EQUAL`
  tables) no longer treat `-0d0` as `0d0`, nor `EQUAL` `-0.0` as `0.0`, and
  `SXHASH` no longer changes when the garbage collector moves an object
  ([#388](https://github.com/atgreen/evergreen/pull/388)).
- A generic function dispatched on a class object no longer answers whatever
  the first such call decided. With methods on `STANDARD-CLASS` and
  `STRUCTURE-CLASS`, whichever class was passed first fixed the answer for
  every class passed after it, and a method on `BUILT-IN-CLASS` traded
  decisions with one on `INTEGER` the same way
  ([#389](https://github.com/atgreen/evergreen/pull/389)).
- A saved image no longer lets newly compiled code redefine the local
  functions of code it saved. Loading the ocicl runtime in the `make image`
  egcl replaced one inside `PARSE-INTEGER`, after which `(parse-integer "1")`
  signalled "not an integer" and ASDF could not load babel
  ([#386](https://github.com/atgreen/evergreen/pull/386)).
- `RESTART` is consistently a standard class. Its class precedence list has
  always been `(RESTART STANDARD-OBJECT T)`, but `(class-of (find-class
  'restart))` answered `BUILT-IN-CLASS` and `(subtypep 'restart
  'standard-object)` answered false. All three agree now
  ([#387](https://github.com/atgreen/evergreen/pull/387)).
- `MAKE-PATHNAME` no longer builds a pathname with a wrong component when a
  garbage collection happens while it runs: with a `:directory` list or a
  namestring `:defaults`, a freshly made `:name` or `:type` string could be
  replaced by unrelated data, such as the directory's namestring
  ([#382](https://github.com/atgreen/evergreen/pull/382)).
- A library's own `DEFUN` of `CLASS-PRECEDENCE-LIST` no longer redefines the
  implementation's. `EGCL-MOP` owns and exports the name now, and the
  interpreter dispatches that spelling, so a bare read of it inside a package
  that does not inherit `EGCL-MOP` interns that package's own symbol instead of
  the implementation's. With `CLASS-SLOTS` in the previous release this
  completes [#370](https://github.com/atgreen/evergreen/issues/370)
  ([#383](https://github.com/atgreen/evergreen/pull/383)).
- `FORMAT`'s `~R` prints bignums in English: `(format nil "~R" (expt 10 20))`
  signalled a TYPE-ERROR saying its argument was not an integer. It also names
  every short-scale group through vigintillion, so 10^9 is "one billion", not
  "one thousand million"
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- `~D`, `~B`, `~O`, `~X` and `~R` keep a pad or comma character outside
  Latin-1: it was cut to its low byte, so `#\★` padded with a control character
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- `NTH-VALUE` and `DECODE-UNIVERSAL-TIME` signal a TYPE-ERROR for an argument
  that is not a non-negative integer instead of converting it through a float:
  a compiled `(nth-value -1 ...)` answered the first value, and
  `(decode-universal-time 1.5)` decoded a time
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- A library's own `DEFUN` of `CLASS-SLOTS` no longer redefines the
  implementation's. The non-ANSI MOP readers were reachable as a bare name from
  any package, so an internal helper of that name in a package that exports
  nothing hijacked the implementation's function, process-wide and silently.
  They live in `EGCL-MOP` now. `CLASS-PRECEDENCE-LIST` and
  `METHOD-SPECIALIZERS` are unchanged: they are name-dispatched builtins whose
  dispatch ignores the package, so moving the symbol does not stop the override
  ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#370](https://github.com/atgreen/evergreen/issues/370)).
- A package designator that names no package now signals `PACKAGE-ERROR`
  instead of creating the package. `INTERN`, `EXPORT`, `IMPORT`, `SHADOW` and
  `USE-PACKAGE` created it; `FIND-SYMBOL`, `UNUSE-PACKAGE` and
  `PACKAGE-USE-LIST` answered as though the lookup had simply found nothing. So
  a typo in a package name, or a library loaded before the package it names,
  left a live package of that name: `FIND-PACKAGE` then answered it, a later
  `DEFPACKAGE` of the real one merged into the mistake, and the fault surfaced
  far from where it was made. `DEFPACKAGE` still resolves a forward `:use`
  reference, which is what the create-on-miss path exists for
  ([#379](https://github.com/atgreen/evergreen/pull/379);
  issue [#375](https://github.com/atgreen/evergreen/issues/375)).
- `OPEN` and `WITH-OPEN-FILE` honor `:external-format :latin-1` (also
  `:iso-8859-1`) and `:ascii`, alone or as the first element of a list such
  as `(:latin-1 :eol-style :lf)`; `:default` and `:utf-8` remain UTF-8. The
  keyword was accepted and ignored, so a Latin-1 file was decoded as UTF-8
  and the first accented letter was a `STREAM-ERROR`: cl-pdf opens its font
  metrics and cl-typesetting its French hyphenation patterns that way, and
  cl-typesetting could not load. Reading decodes one octet per character,
  writing refuses a character outside the format's range, and
  `STREAM-EXTERNAL-FORMAT` answers the format a file stream was opened with
  instead of `:default`. A format the stream layer cannot decode (`:utf-16`,
  `:utf-32`) is refused at `OPEN` rather than read as UTF-8
  ([#374](https://github.com/atgreen/evergreen/pull/374)).
- `LISTEN` on a freshly opened octet file stream answers T while bytes remain
  and NIL at the end of the file. With nothing buffered yet it asked the
  handle as if it were a socket, and a regular file always answered NIL, so a
  `(unsigned-byte 8)` file stream had to be read once before `LISTEN` saw
  it ([#369](https://github.com/atgreen/evergreen/pull/369)). The file-stream
  case joins the socket test of
  [#360](https://github.com/atgreen/evergreen/pull/360).
- A thread waiting for input on a stream no longer locks every other thread out
  of it. Input parked while holding the stream's state mutex, so for as long as
  one thread waited for a peer, any other operation on that stream blocked --
  `CLOSE`, a write on the other half of a socket, even `INPUT-STREAM-P`, which
  performs no I/O. A server could not close a worker's idle keep-alive
  connection, and on a bidirectional socket it could not write a reply while
  another thread read. Readers are now serialized on a separate lock and wait
  with the state mutex released
  ([#353](https://github.com/atgreen/evergreen/pull/353);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- The same fix now covers composite streams. A `TWO-WAY`, `ECHO`,
  `CONCATENATED` or `SYNONYM` stream reached its component while still holding
  its own lock, so a read parked on the component locked every other thread out
  of the composite -- and `*TERMINAL-IO*` is bidirectional
  ([#354](https://github.com/atgreen/evergreen/pull/354);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- Character reads and process-pipe reads use the same non-parking path as byte
  reads, so a read that waits for input holds no stream mutex. Regular file I/O
  is deliberately excluded: a readiness poll always reports a regular
  descriptor ready, so polling cannot make a disk read cooperative, which is
  also where the JVM leaves file I/O for its virtual threads
  ([#365](https://github.com/atgreen/evergreen/pull/365);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- `READ-SEQUENCE` on a socket no longer deadlocks other threads using that
  stream. It was the last read that still waited while holding the stream's
  state mutex, and the reachable one: socket streams are `(unsigned-byte 8)`,
  so `READ-SEQUENCE` is how a library reads a socket. Any other thread touching
  the stream blocked -- `CLOSE`, a write on the other half, even
  `STREAM-ELEMENT-TYPE`, which performs no I/O -- and because the thread that
  would supply the bytes was usually the blocked one, the program deadlocked
  rather than merely stalling. `READ-SEQUENCE` now waits with the mutex
  released and resumes where it left off, so a request split across several
  arrivals still fills completely and a short count still means end of file
  ([#367](https://github.com/atgreen/evergreen/pull/367);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- `COMPILE-FILE` now gives the body of a load-time-only top-level `EVAL-WHEN`
  the top-level processing CLHS 3.2.3.1 requires. Without `:COMPILE-TOPLEVEL`
  but with `:LOAD-TOPLEVEL`, the body forms are still top-level forms, so a
  nested `(eval-when (:compile-toplevel) ...)`, a `DEFMACRO` or a `DEFPACKAGE`
  among them keeps its compile-time effect. The whole form was evaluated
  instead, which answered NIL for the outer form and never reached the inner
  one: cl-store's `DEFBACKEND` expands to exactly that nest, so every later use
  in the file compiled as a function call and the fasl died loading with
  "STREAM is unbound"
  ([#355](https://github.com/atgreen/evergreen/pull/355)).
- `GET-DISPATCH-MACRO-CHARACTER` now reports a callable function for each
  standard `#` sub-character (CLHS 2.4.8) rather than NIL for all but `#\`. A
  reader that wraps the standard syntax -- zacl rewrites Allegro's
  `(version>= ...)` features and hands the rest back to the standard `#+`
  reader -- died calling NIL
  ([#356](https://github.com/atgreen/evergreen/pull/356)).
- A control transfer out of a reader macro now passes through `COMPILE-FILE`'s
  pre-read. A `HANDLER-CASE` clause around `COMPILE-FILE` taking a condition a
  reader macro signalled, or an invoked restart, was reported as a read failure
  and silently downgraded the output to a source-only artifact, so the clause
  never ran
  ([#357](https://github.com/atgreen/evergreen/pull/357)).
- The reader now accepts the character names SBCL and Clozure CL share:
  `Ff`, `Formfeed`, `Lf`, `Cr`, `Bs`, `Ht`, `Sp`, `Del` and `Altmode`.
  `CHAR-NAME` still answers the standard names, so printing round-trips are
  unchanged ([#358](https://github.com/atgreen/evergreen/pull/358)).
- The reader now closes the list when a Lisp reader-macro function returns no
  values immediately before the closing parenthesis, as it already did for
  `#|...|#`. The list previously lost its terminator
  ([#359](https://github.com/atgreen/evergreen/pull/359)).
- `LISTEN` no longer consumes the pending byte of an octet stream. It was
  `READ-CHAR-NO-HANG` followed by `UNREAD-CHAR` for every stream, which on an
  octet socket parked the byte in the character unread slot where `READ-BYTE`
  never looks: `LISTEN` answered T and the `READ-BYTE` after it hung or lost an
  octet outright. usocket's `WAIT-FOR-INPUT` calls `LISTEN` on every stream
  socket before its backend runs, so AllegroServe hung on its first read after
  every wait ([#360](https://github.com/atgreen/evergreen/pull/360)).
- `FUNDAMENTAL-STREAM` now sits under `STREAM`, so a method on `STREAM` applies
  to a Gray stream and `(typep gray-stream 'stream)` is true
  ([#362](https://github.com/atgreen/evergreen/pull/362)).
- `DEFINE-SYMBOL-MACRO` definitions now live in one process-wide table, so a
  symbol macro defined on one thread is visible on every other. Each thread
  previously kept its own, and a definition made before a thread started was
  invisible inside it
  ([#363](https://github.com/atgreen/evergreen/pull/363)).

### Performance

- A function with `&optional` parameters binds them by position, filling in
  defaults in a short prologue, instead of through the general lambda-list
  binder on every call. Calls to such functions in pure-tls's certificate
  parsing and in `SEARCH` took about a third less time
  ([#392](https://github.com/atgreen/evergreen/pull/392)).
- A compiled function whose `IF` returns a call's result in one arm, such as a
  recursive `(if (< n 2) (leaf n) (+ (f (- n 1)) (f (- n 2))))`, now uses the
  direct native calling convention instead of going through Rust for every
  call: about 5 ns per call instead of about 700 ns
  ([#420](https://github.com/atgreen/evergreen/pull/420)).
- Generic-function calls do less repeated work. A `DEFCLASS` accessor reads
  or writes its slot directly once dispatch selects it (user methods on the
  accessor still take over), slot writes skip the class-slot search for
  ordinary slots, and per-generic and per-class facts are cached. From
  compiled callers an accessor read takes about 3 us instead of 11, a write 8
  instead of 23, and `READ-BYTE` on a Gray stream 40 instead of about 110; a
  repeated HTTPS request through drakma and pure-tls is about twice as fast
  ([#415](https://github.com/atgreen/evergreen/pull/415)).
- A method body that loops now reaches optimized native code (T2) like the
  same body in a `DEFUN`. It used to stop at baseline native code (T1) until
  its 4096th call: a 1000-iteration loop took about 4 ms per call as a method
  and 30 us as a function, and now takes about 50 us as a method. Methods
  restored from a saved image are covered too
  ([#413](https://github.com/atgreen/evergreen/pull/413)).
- AES, GCM's GHASH multiply and SHA-256 compression run in the runtime, as
  `EGCL-EXT:CRYPTO-*` block primitives that EGCL's Ironclad fork calls in place
  of its portable Lisp. With the fork, encrypting 64 KB with AES-GCM takes
  seconds instead of 193 s, hashing it with SHA-256 takes under 0.3 s instead
  of 18 s, and a repeated HTTPS request through pure-tls is about 10x faster.
  `EGCL-EXT:SHA256` and `EGCL-EXT:CRC32` are now documented
  ([#410](https://github.com/atgreen/evergreen/pull/410)).
- Calls made from compiled (native) code into other compiled functions are
  cheaper: a call no longer looks `FUNCALL`/`APPLY` up by name, rebuilds the
  callee's per-call buffers, rescans its bytecode or registers a global root
  for it, and finding the current execution's state is quicker. A recursive
  native caller spends about 27% less per call (about 38% when it establishes a
  `HANDLER-CASE`), and a cached `(asdf:load-system :babel)` is about 5% faster
  ([#390](https://github.com/atgreen/evergreen/pull/390)).
- Compiled code finds its execution-local state with fewer checks per access,
  so a native call that establishes a `HANDLER-CASE` costs about 7% less again
  ([#397](https://github.com/atgreen/evergreen/pull/397)).

## 0.0.9 - 2026-10-15

### ANSI conformance

The Dietz ansi-test suite now runs end to end. Nine chapters pass completely:
`cons` (1882 tests), `data-and-control-flow` (1428), `misc` (740),
`conditions` (673), `strings` (509), `characters` (259), `environment` (210),
`hash-tables` (158) and `files` (87).

- `SUBTYPEP` compares numeric bounds exactly. Bounds were held as `f64`, which
  cannot tell adjacent integers apart above 2^53, so
  `(subtypep '(integer 2^64 2^64) '(integer 2^64+1 2^64+1))` answered `T, T` --
  a definite and wrong answer -- where it must answer `NIL, T`. Every bound the
  range algebra accepts (integer, ratio, single or double float) is a rational,
  so bounds are now exact rationals and every comparison is exact
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- An error or non-local exit from a user print function propagates. A
  `*PRINT-PPRINT-DISPATCH*` entry or a `PRINT-OBJECT` method that signalled was
  read as "no applicable entry", so the object printed normally and the error
  vanished: `(format nil "~S" 1)` answered `"1"` instead of signalling, and a
  `THROW` out of an entry was swallowed. The error is now re-signalled at the
  boundary of the printing operation
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- A method object reports its own qualifiers once detached from its generic
  function. `METHOD-QUALIFIERS` recovered the name of a short-form combination
  by looking up the owning generic, so a method removed with `REMOVE-METHOD`
  reported `(STANDARD)` rather than its own `(+)`. Methods now carry their
  combination, including across `SAVE-LISP-AND-DIE` and image restore
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- `ADD-METHOD` keeps a method it displaces. Replacing a method with the same
  qualifiers and specializers dropped the old one, so a program still holding
  it got a `TYPE-ERROR` from `(add-method gf displaced)` and the wrong method
  answered the call. The displaced method is now detached and can be re-added
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- `MACRO-FUNCTION` of a standard macro the evaluator implements natively returns
  an expander that actually expands. It previously handed back the form
  unchanged, so the returned function satisfied the type but did nothing
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- Run on kernels built without user shadow-stack support. On such a kernel
  (WSL2's, and many cloud VM kernels) `arch_prctl(ARCH_SHSTK_STATUS)` fails with
  `EINVAL`, which was read as "hardening state unknown" and refused, so every
  program aborted with `ordinary callable boundary is unavailable`
  ([#314](https://github.com/atgreen/evergreen/pull/314)).
- Reader: honor single escapes inside `|...|`, resolve escaped `\T` and `|NIL|`
  to the constants, read `t` and `nil` in a `#+`/`#-` feature expression as the
  keywords `:T` and `:NIL`, honor the length argument of `#n*` and `#n(`, give
  `#S` slots their initforms, and read `#nA` sequences.
  `WITH-STANDARD-IO-SYNTAX` binds `*PACKAGE*` to the package rather than its
  name ([#315](https://github.com/atgreen/evergreen/pull/315)).
- Printer and `FORMAT`: `WRITE` binds every printer-control keyword;
  `*PRINT-READABLY*` overrides `*PRINT-LENGTH*`, `*PRINT-LEVEL*` and
  `*PRINT-GENSYM*`; printing circular structure without `*PRINT-CIRCLE*` stops
  at the stack budget instead of faulting. The `~^`, `~{`, `~/name/`, `~<`,
  `~F`, `~C`, `~D` and `~R` directives follow their parameters, `FORMATTER`
  functions return the arguments they did not consume, and pprint-dispatch
  tables work ([#317](https://github.com/atgreen/evergreen/pull/317),
  [#320](https://github.com/atgreen/evergreen/pull/320),
  [#324](https://github.com/atgreen/evergreen/pull/324)).
- Streams, files and pathnames: `READ-CHAR-NO-HANG` and `LISTEN` no longer
  block on a pipe, socket or terminal with nothing buffered, and both honor a
  character pushed back with `UNREAD-CHAR`. `*TERMINAL-IO*` is bidirectional.
  Binary streams support wide `(unsigned-byte N)` element types; a broadcast
  stream reports its last component's `FILE-POSITION` and `FILE-LENGTH` and
  passes on values its components accept; and a too-large or negative
  `FILE-POSITION` signals a `TYPE-ERROR` instead of crashing the process.
  Logical pathname translations accept the ANSI list of wildcard pairs,
  `DIRECTORY` merges against the default directory, and `READ-SEQUENCE` fills a
  list ([#316](https://github.com/atgreen/evergreen/pull/316),
  [#318](https://github.com/atgreen/evergreen/pull/318),
  [#321](https://github.com/atgreen/evergreen/pull/321)).
- Symbols, macros and structures: every standard macro and function the
  evaluator implements natively reports as `FBOUNDP`, the REPL history
  variables and `*BREAK-ON-SIGNALS*` are bound, `DEFMACRO` keeps its
  documentation string, `SYMBOL-MACROLET` shadows a global symbol macro,
  `TRACE` and `UNTRACE` work, and `:TYPE`d structures are supported
  ([#319](https://github.com/atgreen/evergreen/pull/319),
  [#322](https://github.com/atgreen/evergreen/pull/322),
  [#325](https://github.com/atgreen/evergreen/pull/325)).
- CLOS: method objects are first class, with `REMOVE-METHOD`, `ADD-METHOD`,
  `COMPUTE-APPLICABLE-METHODS`, and a `FIND-METHOD` that distinguishes one
  method combination from another. `DEFCLASS` and `DEFGENERIC` syntax and
  initarg validity are checked, `SLOT-MISSING` is called, class slots shadow
  inherited instance slots, effective slots and the built-in classes follow the
  CLHS precedence order, short-form method combinations run with `:around`
  methods, and `CHANGE-CLASS` is generic
  ([#316](https://github.com/atgreen/evergreen/pull/316),
  [#323](https://github.com/atgreen/evergreen/pull/323)).
- Sequences and types: `NREVERSE` reverses a vector in place, `MAPCAR` blames
  the dotted tail in its `TYPE-ERROR`, `TYPEP` knows `GENERIC-FUNCTION` and
  `STANDARD-GENERIC-FUNCTION`, bit operations accept any bit array,
  `ADJUST-ARRAY` copies, `RATIONALIZE` round-trips through the float it came
  from, and a copied `RANDOM-STATE` replays its sequence
  ([#315](https://github.com/atgreen/evergreen/pull/315),
  [#316](https://github.com/atgreen/evergreen/pull/316),
  [#317](https://github.com/atgreen/evergreen/pull/317),
  [#325](https://github.com/atgreen/evergreen/pull/325)).
- Character names round-trip for the whitespace characters. `PRIN1` printed
  `#\No-break_space` and `#\Ideographic_space` as a literal invisible space,
  so the output could not be read back and was indistinguishable from
  `#\Space`. A character with a name now prints by name whenever it is not
  simple graphic, matching SBCL
  ([#348](https://github.com/atgreen/evergreen/pull/348)).
- A `:TYPE`d structure enforces its declared slot types. The constructor and
  the setf writers generated for a `(:type list)` or `(:type vector)` structure
  accepted any value, so a slot declared `INTEGER` silently held a keyword.
  Both now signal a `TYPE-ERROR`, including when the structure is loaded from a
  compiled file with no source present
  ([#348](https://github.com/atgreen/evergreen/pull/348)).
- A generic call and `MAKE-INSTANCE` no longer abort the process while
  validating keyword arguments. Both read their methods' lambda lists while
  holding the evaluator's method table, and naming a symbol takes the symbol
  registry, which the runtime's lock-order guard refuses in that order. A
  restored image has a cold symbol-name cache, so its first `MAKE-INSTANCE`
  tripped it; `ASDF:LOAD-SYSTEM` tripped the generic-call site on every debug
  build ([#341](https://github.com/atgreen/evergreen/pull/341)).
- `FUNCTION-KEYWORDS`, `INITIALIZE-INSTANCE`, `NO-APPLICABLE-METHOD` and
  `NO-NEXT-METHOD` are generic functions with function objects, so code that
  walks the standard generic functions -- or adds a method to one -- works
  ([#340](https://github.com/atgreen/evergreen/pull/340)).
- `RENAME-FILE` accepts an open file stream as its filespec, not only a
  pathname or namestring
  ([#339](https://github.com/atgreen/evergreen/pull/339)).
- `PARSE-INTEGER` with `:JUNK-ALLOWED` returns the index of the delimiter that
  stopped it, rather than an index past the whitespace following it
  ([#343](https://github.com/atgreen/evergreen/pull/343)).
- The Gray stream generic function `STREAM-WRITE-STRING` takes `START` and
  `END` as optional arguments, so a two-argument call from portable code finds
  an applicable method
  ([#342](https://github.com/atgreen/evergreen/pull/342)).
- The standard reader macros `GET-MACRO-CHARACTER` reports are callable
  functions. It returned a value that satisfied the type but could not be
  funcalled, so a readtable built by copying the standard one broke
  ([#346](https://github.com/atgreen/evergreen/pull/346)).
- A nested `EVAL-WHEN` inside a compile-time body is evaluated under the
  execute rule, as CLHS 3.2.3.1 requires
  ([#344](https://github.com/atgreen/evergreen/pull/344)).
- A `THROW`, `GO` or `RETURN-FROM` out of a form `COMPILE-FILE` evaluates at
  compile time passes through unchanged instead of being captured as a value
  ([#345](https://github.com/atgreen/evergreen/pull/345)).
- Loading a fasl whose package exports `T` or `NIL` no longer panics. The
  loader looked up a symbol index for the two constants, which have none
  ([#347](https://github.com/atgreen/evergreen/pull/347)).

### Performance

- Memoize tree-walking macro expansions, so an interpreted loop body expands
  its macros once instead of once per iteration. `ASSERT` expands a `LOOP`,
  which made interpreted test loops far slower than necessary; a
  2000-iteration `ASSERT` loop now runs in 14 ms. Memoized expansions are
  dropped at each `EVAL` boundary, so a form mutated between two evaluations is
  expanded again rather than reusing the first expansion
  ([#321](https://github.com/atgreen/evergreen/pull/321),
  [#322](https://github.com/atgreen/evergreen/pull/322)).

### Correctness

- Root printed and formatted values across calls back into Lisp — the
  pretty-printer dispatch hook and the `~{` iteration body function — so a
  relocating garbage collection during a user print function or `FORMATTER`
  cannot leave the caller holding a stale object
  ([#320](https://github.com/atgreen/evergreen/pull/320),
  [#324](https://github.com/atgreen/evergreen/pull/324)).
- Check supported primitive `FTYPE` primary-result declarations even at safety
  zero, preserving multiple values and the executing definition's contract.
  Retain checks in source-free BFASL files, and enforce declared argument types
  on interpreted calls, mapped recursion, and optimized self-tail calls
  ([#331](https://github.com/atgreen/evergreen/pull/331)). The matched typed
  Fibonacci comparison measured a small slowdown from 30 ms to 31 ms median;
  see the [checked-result report](tests/benchmarks/results/2026-10-14-fib-checked-results.md).
- Raise the declared `TYPE-ERROR` when a nested recursive argument contract
  fails, instead of returning a value through a replacement definition
  ([#334](https://github.com/atgreen/evergreen/pull/334)).
- Resume a validated chain of self-inlined native frames through ordinary guard
  recovery, transferring the outer activation state exactly once and preserving
  the executing definition across redefinition and garbage collection
  ([#333](https://github.com/atgreen/evergreen/pull/333),
  [#334](https://github.com/atgreen/evergreen/pull/334)).

### Known limitations

- `--shake` retains every candidate function instead of pruning the
  unreachable ones, so a shaken executable is correct but no smaller than the
  image it came from. The conformance work in this release took the saved image
  from 3 source closures to 30, and the reader entry points quoted inside them
  reach the analyzer's "can invoke arbitrary code" rule, which then keeps
  everything. The shaker is being conservative rather than wrong -- nothing is
  removed that a program needs -- and `dynamic = preserve` behaves as before.
  Tracked in `bliss-mihqk`; the six `shake_cli` pruning tests are marked
  `#[ignore]` against it.

## 0.0.8 - 2026-10-14

### Native compilation and performance

- Compile eligible recursive self-calls directly through the x86-64 Lisp ABI,
  preserving `NOTINLINE`, lexical function bindings, and the executing definition
  across redefinition and garbage collection. Retain this behavior in source-free
  BFASL files ([#307](https://github.com/atgreen/evergreen/pull/307)).
- Reduce ordinary T2 call overhead with cheaper multiple-value handling,
  smaller frames, execution-local polling, and fewer redundant checks. Let
  proven noncollecting entry paths, including simple recursive base cases,
  return before constructing a native frame
  ([#308](https://github.com/atgreen/evergreen/pull/308),
  [#309](https://github.com/atgreen/evergreen/pull/309),
  [#310](https://github.com/atgreen/evergreen/pull/310),
  [#326](https://github.com/atgreen/evergreen/pull/326)).
- Preserve bounded integer parameter ranges in T2. Use proven numeric bounds
  to remove redundant recursive argument checks and add/subtract overflow
  checks, while retaining checks where safety cannot be proved. Optimize
  eligible pure return paths with bounded partial evaluation
  ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Eliminate redundant native frame-slot reloads while retaining the stores
  needed for garbage collection and recovery
  ([#326](https://github.com/atgreen/evergreen/pull/326), incorporating the reload
  optimization from [#311](https://github.com/atgreen/evergreen/pull/311)).

Benchmark details and caveats are recorded in the
[checked-range recursion report](tests/benchmarks/results/2026-10-14-fib-range-proofs.md).
Its measured release revision ran warmed, bounded-integer `fib(35)` in a median
30 ms on the benchmark host, versus 33 ms for an implementation-specifically
tuned SBCL version. This is a workload-specific comparison, not a general
performance claim or a matched v0.0.7-to-v0.0.8 comparison.

### Fiber scheduling and cancellation

- Schedule fibers cooperatively without automatic CPU time slices or per-fiber
  CPU budgets. Add cooperative cancellation with explicit checkpoints and
  interruptible public sleep, park, and join; retain opt-in sandbox CPU limits
  ([#326](https://github.com/atgreen/evergreen/pull/326), incorporating
  [#312](https://github.com/atgreen/evergreen/pull/312)).

### Correctness and compatibility

- Accept global `INLINE` and `NOTINLINE` declarations for SETF function names
  without confusing their policies with the corresponding getters, fixing
  ASDF loading ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Enforce declared argument bounds on direct recursive calls, including calls
  replaced by pure return shortcuts. Correct constant-left subtraction when
  the result reuses the right operand's register
  ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Retain global `FTYPE` declaration metadata during compilation and source-free
  BFASL loading ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Reject oversized typed allocations before their body lengths or aligned
  footprints overflow GC size metadata
  ([#302](https://github.com/atgreen/evergreen/pull/302)).
- Convert huge rational values to floating point without intermediate infinity,
  preserving subnormal values and correct rounding
  ([#304](https://github.com/atgreen/evergreen/pull/304)).
- Preserve large-object identities, payload references, and true header pinning
  when restoring and resaving heap images
  ([#305](https://github.com/atgreen/evergreen/pull/305)).

### Tooling

- Label native profiler maps with the actual T1, T2, or OSR tier
  ([#303](https://github.com/atgreen/evergreen/pull/303)).

### Scope limits

Retaining `FTYPE` metadata does not yet enable result-type enforcement or
optimization based on declared result types. General recursive inlining with
nested deoptimization and a private unboxed local-call ABI remain separate work.

## 0.0.7 - 2026-10-12

- Use the new x86-64 Linux Lisp register ABI for supported T1/T2 functions,
  including recursive and cross-function calls, branches, global operations,
  and stack arguments. Preserve multiple values, moving-GC roots, and function
  redefinition across native calls and interpreter adapters; avoid false stack
  overflow errors on worker threads and foreign callbacks
  ([#298](https://github.com/atgreen/evergreen/pull/298),
  [#299](https://github.com/atgreen/evergreen/pull/299),
  [#301](https://github.com/atgreen/evergreen/pull/301)).
- Add eligible native tail calls and resume failed optimization guards without
  repeating completed calls
  ([#286](https://github.com/atgreen/evergreen/pull/286),
  [#292](https://github.com/atgreen/evergreen/pull/292)).
- Support packed `DOUBLE-FLOAT` arrays, including adjustable and displaced
  arrays. Compile eligible array arithmetic to unboxed native operations and
  reduce the instructions needed for double addition and multiplication
  ([#278](https://github.com/atgreen/evergreen/pull/278),
  [#290](https://github.com/atgreen/evergreen/pull/290),
  [#295](https://github.com/atgreen/evergreen/pull/295)).
- Reduce idle T2 loop-poll overhead on Linux x86-64; poll every selected T2
  backedge on AArch64, POWER, RISC-V, and System Z
  ([#272](https://github.com/atgreen/evergreen/pull/272),
  [#279](https://github.com/atgreen/evergreen/pull/279)).
- Scan overlapping native GC root slots once while preserving distinct copies
  of live values ([#274](https://github.com/atgreen/evergreen/pull/274)).

The new ABI migration remains incomplete: protected control scopes, raw numeric
bodies, and some multiple-value consumers still use compatible execution paths.

## 0.0.6 - 2026-10-11

This release also includes the [0.0.5 changes](https://github.com/atgreen/evergreen/blob/v0.0.6/CHANGELOG.md#005---2026-10-10).

- Speed up T2 native recursion on Linux x86-64 with machine-stack self-calls,
  cheaper returns, fewer multiple-value resets, and correct fiber stack limits.
  A release-build `fib(32)` comparison improved from 1.04 s to 0.21–0.23 s
  ([#248](https://github.com/atgreen/evergreen/pull/248),
  [#259](https://github.com/atgreen/evergreen/pull/259),
  [#260](https://github.com/atgreen/evergreen/pull/260),
  [#262](https://github.com/atgreen/evergreen/pull/262)).
- Remove invocation counting from ordinary T2 execution.
  `FUNCTION-INVOKE-COUNT` now reports tiering heat; use `PROFILE` for exact counts
  ([#261](https://github.com/atgreen/evergreen/pull/261)).
- Correct `DEFTYPE` defaults, lexical and dynamic bindings, named returns, and
  documentation; preserve `SPECIAL` parameters in compiled closures. Evaluate
  optional `TYPEP`/`SUBTYPEP` arguments and enforce `TYPEP` errors through `FUNCALL`
  ([#255](https://github.com/atgreen/evergreen/pull/255),
  [#256](https://github.com/atgreen/evergreen/pull/256),
  [#257](https://github.com/atgreen/evergreen/pull/257)).
- Accept repeated `DEFPACKAGE` option names without losing conflict checks;
  fix `APROPOS` handling of `NIL`, `T`, and invalid argument counts
  ([#250](https://github.com/atgreen/evergreen/pull/250),
  [#253](https://github.com/atgreen/evergreen/pull/253)).
- Accept open and closed file streams as pathname designators, retaining their
  opening paths when defaults change
  ([#252](https://github.com/atgreen/evergreen/pull/252)).
- Preserve saved builtin function identity and printing across replacement and
  restoration; protect large rational numerators during moving GC
  ([#249](https://github.com/atgreen/evergreen/pull/249),
  [#251](https://github.com/atgreen/evergreen/pull/251),
  [#263](https://github.com/atgreen/evergreen/pull/263)).

## 0.0.5 - 2026-10-10

### Performance and diagnostics

- Improve native execution performance on Linux x86-64: restore efficient
  direct recursive calls in eligible T2 code, and reduce thread-lookup and
  idle-safepoint overhead, including redundant polls in empty jump blocks.
  Remove per-operation function-binding checks for standard CL primitives.
  Recursive calls now count toward automatic T2
  promotion; cold or wrong-arity interpreter fallbacks count only once.
  Keep loop counters attached to the running function during redefinition or
  unbinding, avoiding invalid function accesses
  ([#227](https://github.com/atgreen/evergreen/pull/227),
  [#228](https://github.com/atgreen/evergreen/pull/228),
  [#230](https://github.com/atgreen/evergreen/pull/230),
  [#231](https://github.com/atgreen/evergreen/pull/231),
  [#232](https://github.com/atgreen/evergreen/pull/232),
  [#233](https://github.com/atgreen/evergreen/pull/233),
  [#234](https://github.com/atgreen/evergreen/pull/234),
  [#235](https://github.com/atgreen/evergreen/pull/235),
  [#237](https://github.com/atgreen/evergreen/pull/237)).

- Correct T2 diagnostics to report the profile choices actually used and the
  code actually installed; distinguish code emission from publication and
  clarify baseline arithmetic optimization. Make native compilation refusals
  searchable by function name, explain unsupported instructions, and report
  the multiple-value resets remaining in emitted code
  ([#236](https://github.com/atgreen/evergreen/pull/236),
  [#247](https://github.com/atgreen/evergreen/pull/247)).

### Compatibility

- **Compatibility:** Standard `COMMON-LISP` functions now have fixed semantics,
  following the Common Lisp standard and SBCL policy. Redefining or unbinding
  them is unsupported; this withdraws the earlier builtin-rebinding extension.
  User-defined functions, including distinct symbols created with `SHADOW`,
  remain redefinable
  ([#237](https://github.com/atgreen/evergreen/pull/237)).

### Runtime and control-flow correctness

- Let eligible running native loops on Linux x86-64 switch to optimized T2
  code without restarting, preserving completed effects, moving GC roots,
  multiple values and inherited handlers. Mapped T1/T2 now supports local
  multiple-value bindings, including those generated by `SETF`, and eligible
  child loops can optimize while their native parent is suspended. Local
  `CATCH` retains its original tag registration through optimization and guard
  recovery. `UNWIND-PROTECT` loops can optimize in protected bodies or running
  cleanups, preserving saved values and pending transfers without repeating
  cleanup. Local `HANDLER-BIND`, `HANDLER-CASE` and `RESTART-CASE` scopes
  retain their original registrations through optimization and guard recovery.
  Special-variable bindings retain their original values across these tier
  changes and restore correctly before outer cleanups, including nested
  bindings of the same variable.
  Direct recursive activations and other unsupported local scopes remain
  outside this OSR route
  ([#218](https://github.com/atgreen/evergreen/pull/218),
  [#219](https://github.com/atgreen/evergreen/pull/219),
  [#220](https://github.com/atgreen/evergreen/pull/220),
  [#223](https://github.com/atgreen/evergreen/pull/223),
  [#224](https://github.com/atgreen/evergreen/pull/224),
  [#225](https://github.com/atgreen/evergreen/pull/225),
  [#226](https://github.com/atgreen/evergreen/pull/226)).

- Preserve required type guards when optimized loops are entered through OSR
  (including nested loop entries) and reject entries with missing live values,
  preventing invalid accesses
  caused by skipped definitions
  ([#215](https://github.com/atgreen/evergreen/pull/215),
  [#216](https://github.com/atgreen/evergreen/pull/216),
  [#217](https://github.com/atgreen/evergreen/pull/217)).

- Preserve original arguments after native deoptimization and retain the callee
  in keyword-binding error backtraces. Restore supported deep recursion on
  Linux x86-64 with a larger managed-stack default, preserving stack-overflow
  conditions and explicit overrides
  ([#245](https://github.com/atgreen/evergreen/pull/245),
  [#246](https://github.com/atgreen/evergreen/pull/246)).

- Fix character counts and indexing in runtime-created Unicode strings, and
  preserve fresh-string mutability for wider characters
  ([#244](https://github.com/atgreen/evergreen/pull/244)).

### Packages

- Add an x86-64 Fedora package for building standalone RISC-V Linux
  executables with the static RV64GC musl runtime and QEMU launcher
  ([#211](https://github.com/atgreen/evergreen/pull/211)).

## 0.0.4 - 2026-10-10

### Performance and native compilation

- Reduce native call overhead on Linux x86-64: eligible T1 and T2 functions
  use the new native ABI by default, removing exceptional-status checks after
  successful returns while preserving moving GC roots, multiple values,
  speculative arithmetic, recursion and guard recovery. Unsupported bodies
  use an interpreter adapter; existing native T1 code remains when T2 declines.
  **Temporary limitation:** current-frame OSR is disabled on Linux x86-64,
  so unsupported bodies and long loops in cold functions may run slower
  ([#204](https://github.com/atgreen/evergreen/pull/204),
  [#207](https://github.com/atgreen/evergreen/pull/207),
  [#210](https://github.com/atgreen/evergreen/pull/210)).
- Optimize named calls, builtin calls, `GETHASH`, nested `FUNCALL` callbacks,
  captured-variable access, simple-string access and missed hash-table lookups
  on supported compiled paths.
  Retain replacement checks, multiple values, GC safety and error recovery
  ([#142](https://github.com/atgreen/evergreen/pull/142),
  [#145](https://github.com/atgreen/evergreen/pull/145),
  [#149](https://github.com/atgreen/evergreen/pull/149),
  [#150](https://github.com/atgreen/evergreen/pull/150),
  [#151](https://github.com/atgreen/evergreen/pull/151),
  [#152](https://github.com/atgreen/evergreen/pull/152),
  [#153](https://github.com/atgreen/evergreen/pull/153),
  [#155](https://github.com/atgreen/evergreen/pull/155)).
- Extend compiler support for captured lexical reads and writes, shared
  compilation across closures, nested callbacks and retained-body leaf
  inlining, preserving lexical `RETURN-FROM` and captured state during
  deoptimization. Some of these paths still require the legacy compiler;
  Linux x86-64 uses interpreter fallback where mapped native support is absent
  ([#146](https://github.com/atgreen/evergreen/pull/146),
  [#147](https://github.com/atgreen/evergreen/pull/147),
  [#148](https://github.com/atgreen/evergreen/pull/148),
  [#175](https://github.com/atgreen/evergreen/pull/175)).
- Improve numeric guards and recovery on supported compiled paths, including
  nullable values and bignums. Remove redundant multiple-value resets and loop
  tag checks, retain only live deoptimization locals, and preserve pending
  recursive operands. Repair x86-64 OSR recovery metadata for future use;
  Linux x86-64 OSR remains disabled in this release. Decline unsupported
  checked RISC-V OSR entries safely ([#100](https://github.com/atgreen/evergreen/pull/100),
  [#103](https://github.com/atgreen/evergreen/pull/103),
  [#106](https://github.com/atgreen/evergreen/pull/106),
  [#131](https://github.com/atgreen/evergreen/pull/131),
  [#135](https://github.com/atgreen/evergreen/pull/135),
  [#144](https://github.com/atgreen/evergreen/pull/144)).

### Compatibility

- Saved bytecode now uses version 2.0 and core images use format 9. Recompile
  older FASLs and rebuild older images. Saved functions retain their definitions
  and lexical captures across redefinition, aliases, unbinding and restoration
  ([#174](https://github.com/atgreen/evergreen/pull/174),
  [#202](https://github.com/atgreen/evergreen/pull/202),
  [#204](https://github.com/atgreen/evergreen/pull/204)).

### Calling and control-flow correctness

- Keep each function's selected definition, captures and compiled code together
  across concurrent redefinition and tier promotion, including callable-instance
  replacements and saved source-backed functions. Preserve closure function and
  control scopes through installation and image restoration, and resolve symbol
  `FUNCALL`/`APPLY` targets in the global function namespace
  ([#148](https://github.com/atgreen/evergreen/pull/148),
  [#192](https://github.com/atgreen/evergreen/pull/192),
  [#193](https://github.com/atgreen/evergreen/pull/193),
  [#204](https://github.com/atgreen/evergreen/pull/204)).
- Honor replacement builtins, readers and memory fences in interpreted, compiled
  and saved code, including lexical `VALUES` and `BYTE`, `LDB`, `TYPEP`,
  `FIRST-CHAR` and returning `ERROR` replacements. Preserve replacement functions'
  multiple values and nonlocal exits
  ([#194](https://github.com/atgreen/evergreen/pull/194),
  [#195](https://github.com/atgreen/evergreen/pull/195),
  [#196](https://github.com/atgreen/evergreen/pull/196),
  [#197](https://github.com/atgreen/evergreen/pull/197),
  [#202](https://github.com/atgreen/evergreen/pull/202)).
- Prevent stale secondary values from leaking through optimized primitives,
  predicates, accessors and numeric operations. Preserve single-float T2
  comparisons and correct spilled memory-barrier results
  ([#132](https://github.com/atgreen/evergreen/pull/132),
  [#198](https://github.com/atgreen/evergreen/pull/198),
  [#199](https://github.com/atgreen/evergreen/pull/199)).
- Preserve the exact restart selected by `CERROR`, even when an outer restart
  is also named `CONTINUE`; restore special bindings before cleanup and
  destination clauses, including transfers that replace an earlier unwind
  ([#201](https://github.com/atgreen/evergreen/pull/201)).
- Fix `EQUALP` hash-table operations across integer and signed single-float zero,
  and signal `TYPE-ERROR` for improper property lists passed to `GETF`
  ([#89](https://github.com/atgreen/evergreen/pull/89),
  [#154](https://github.com/atgreen/evergreen/pull/154)).

### Native exceptional transfers

These transfers are enabled by default on x86-64 Linux. Full compiled-shape
coverage and the rollout on other platforms remain incomplete.

- Keep eligible recursive calls, calls between T2 definitions, `FUNCALL`, `APPLY`
  and protected functions in native segments. Preserve live linkage, callable
  identity, cleanup, restarts, multiple values and moving arguments across
  legacy calls, Rust reentry and fallback
  ([#157](https://github.com/atgreen/evergreen/pull/157),
  [#164](https://github.com/atgreen/evergreen/pull/164),
  [#165](https://github.com/atgreen/evergreen/pull/165),
  [#166](https://github.com/atgreen/evergreen/pull/166),
  [#171](https://github.com/atgreen/evergreen/pull/171),
  [#172](https://github.com/atgreen/evergreen/pull/172)).
- Compile fixnum arithmetic and recover speculative guards without replaying
  effects or losing callers. Reduce root bookkeeping and unnecessary fallback;
  support rooted heap literals and debugger arguments. Keep rejection caches
  and deoptimization feedback tied to the correct definition
  ([#158](https://github.com/atgreen/evergreen/pull/158),
  [#162](https://github.com/atgreen/evergreen/pull/162),
  [#168](https://github.com/atgreen/evergreen/pull/168),
  [#187](https://github.com/atgreen/evergreen/pull/187),
  [#189](https://github.com/atgreen/evergreen/pull/189),
  [#194](https://github.com/atgreen/evergreen/pull/194)).
- Preserve continuations and roots across fiber suspension and migration,
  validate destination-worker compatibility, and release retained execution
  state when it stops. Preserve all ordinary and interactive restart return
  values. `DISASSEMBLE` distinguishes cached transfer code and the actual ABI
  of installed code ([#156](https://github.com/atgreen/evergreen/pull/156),
  [#159](https://github.com/atgreen/evergreen/pull/159),
  [#160](https://github.com/atgreen/evergreen/pull/160),
  [#168](https://github.com/atgreen/evergreen/pull/168)).

### Applications and deployment

- Embed read-only build-host files into saved images with `egcl-ext:embed-file`
  and ASDF components from `egcl-embed-asdf`, so applications can read them
  after deployment without the original files
  ([#173](https://github.com/atgreen/evergreen/pull/173)).

- Rename the tree-shaking command to `--shake`, its manifest to
  `egcl-shake-manifest`, and its ASDF extension to `egcl-shake-asdf`
  ([#163](https://github.com/atgreen/evergreen/pull/163)).

- Describe shaken executables in ASDF system definitions with
  `egcl-shake-asdf`; `asdf:make` writes the specification, saves the core
  and runs the tree shaker to produce the executable
  ([#161](https://github.com/atgreen/evergreen/pull/161)).

### Platform support

- Restore RISC-V loop promotion into T2 by checking imported fixnum values
  when entering optimized loops, with recovery for failed checks
  ([#140](https://github.com/atgreen/evergreen/pull/140)).
- Add opt-in published native callable entries for Rust-to-Lisp calls on
  AArch64 Unix with `EGCL_NATIVE_TRANSFER=1`
  ([#206](https://github.com/atgreen/evergreen/pull/206)).
- Decode RISC-V native instructions in process for `DISASSEMBLE`, including
  compressed instructions, ABI register names and branch targets
  ([#136](https://github.com/atgreen/evergreen/pull/136)).
- Decode POWER native instructions in process for `DISASSEMBLE`, with
  branch targets, OSR entries, runtime helper names and tagged constants
  ([#139](https://github.com/atgreen/evergreen/pull/139)).
- **Windows x86-64:** add opt-in native callable entry boundaries with rooted
  arguments and Windows unwind metadata. Enable with `EGCL_NATIVE_TRANSFER=1`;
  unsupported process mitigation policies produce an explicit error. Compiled
  bodies retain their existing ABI behind the boundary
  ([#205](https://github.com/atgreen/evergreen/pull/205)).
- **s390x:** support ELF-ABI foreign calls and Lisp callbacks, including floating
  point, narrow integers, large argument lists and supported variadic calls
  ([#119](https://github.com/atgreen/evergreen/pull/119),
  [#133](https://github.com/atgreen/evergreen/pull/133),
  [#141](https://github.com/atgreen/evergreen/pull/141)).
- **s390x:** reduce T1/T2 direct-call, recursive-call and builtin-call overhead;
  improve fixnum guards, constants and register saving. Direct recursive
  deoptimization preserves arguments and backtraces; wrong-arity T2 self-calls
  signal `PROGRAM-ERROR` on every target
  ([#77](https://github.com/atgreen/evergreen/pull/77),
  [#80](https://github.com/atgreen/evergreen/pull/80),
  [#88](https://github.com/atgreen/evergreen/pull/88),
  [#94](https://github.com/atgreen/evergreen/pull/94),
  [#97](https://github.com/atgreen/evergreen/pull/97),
  [#107](https://github.com/atgreen/evergreen/pull/107),
  [#108](https://github.com/atgreen/evergreen/pull/108),
  [#111](https://github.com/atgreen/evergreen/pull/111),
  [#113](https://github.com/atgreen/evergreen/pull/113),
  [#115](https://github.com/atgreen/evergreen/pull/115),
  [#122](https://github.com/atgreen/evergreen/pull/122)).
- **s390x:** compile more predicates, simple-string operations, closure factories,
  fixnum multiplication and memory barriers, including saved bytecode. Improve
  compilation diagnostics and increase the default CL stack to 4 MiB
  (512 KiB elsewhere)
  ([#91](https://github.com/atgreen/evergreen/pull/91),
  [#99](https://github.com/atgreen/evergreen/pull/99),
  [#101](https://github.com/atgreen/evergreen/pull/101),
  [#102](https://github.com/atgreen/evergreen/pull/102),
  [#118](https://github.com/atgreen/evergreen/pull/118),
  [#126](https://github.com/atgreen/evergreen/pull/126),
  [#127](https://github.com/atgreen/evergreen/pull/127)).
- **ppc64le:** correct T2 single-float arithmetic; inline more baseline numeric
  operations and optimize helper calls. Enable T2 loops, more predicates and
  bit operations, timely loop polling, and live T1-to-T2 loop replacement
  ([#95](https://github.com/atgreen/evergreen/pull/95),
  [#96](https://github.com/atgreen/evergreen/pull/96),
  [#98](https://github.com/atgreen/evergreen/pull/98),
  [#104](https://github.com/atgreen/evergreen/pull/104),
  [#105](https://github.com/atgreen/evergreen/pull/105),
  [#110](https://github.com/atgreen/evergreen/pull/110),
  [#114](https://github.com/atgreen/evergreen/pull/114)).
- **riscv64:** add baseline and optimizing native compilers with live OSR and
  deoptimization, native fiber switching, scalar LP64D foreign calls including
  variadics, recoverable null/stack-guard faults, and an opt-in native segment
  boundary that preserves integer and floating-point registers
  ([#93](https://github.com/atgreen/evergreen/pull/93),
  [#112](https://github.com/atgreen/evergreen/pull/112),
  [#120](https://github.com/atgreen/evergreen/pull/120),
  [#121](https://github.com/atgreen/evergreen/pull/121),
  [#123](https://github.com/atgreen/evergreen/pull/123),
  [#128](https://github.com/atgreen/evergreen/pull/128)).

### Quicklisp and library compatibility

- Add an x86-64 Linux Quicklisp client port with pinned setup instructions,
  native networking and filesystem adapters, fresh installation and offline
  reload. Honor custom `*MACROEXPAND-HOOK*` functions, accept file streams in
  `PROBE-FILE`, and fix wildcard-directory traversal for distribution discovery
  ([#86](https://github.com/atgreen/evergreen/pull/86),
  [#87](https://github.com/atgreen/evergreen/pull/87),
  [#90](https://github.com/atgreen/evergreen/pull/90),
  [#92](https://github.com/atgreen/evergreen/pull/92)).

## 0.0.3 - 2026-10-07

### Common Lisp support

- `EGCL-GRAY-STREAMS` now exports `STREAM-READ-SEQUENCE` and
  `STREAM-WRITE-SEQUENCE`, and `READ-SEQUENCE` / `WRITE-SEQUENCE` on a Gray
  stream dispatch through them with one bulk call, falling back to the scalar
  generics by default. Portable libraries that specialize the
  trivial-gray-streams sequence methods are now reached from the standard
  entry points ([#81](https://github.com/atgreen/evergreen/pull/81)).

- Add `PPRINT-LOGICAL-BLOCK`, its lexical list-traversal helpers, conditional
  line breaks and indentation, and `PPRINT-FILL` / `PPRINT-LINEAR`. Nested
  blocks honor prefixes, suffixes, print limits, and circular-list labels,
  allowing Quicklisp's setup code to compile past its missing-macro failure
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

### Performance

- `MULTIPLE-VALUE-PROG1` now compiles to bytecode and baseline native code,
  allowing Ironclad's Keccak implementation to tier instead of remaining
  interpreted, including after compiled-file loading
  ([#56](https://github.com/atgreen/evergreen/pull/56)).

- Functions using `CAR`, `CDR`, bitwise operations, shifts, or power-of-two
  `MOD` now reach T2 native code on s390x instead of stopping at T1
  ([#62](https://github.com/atgreen/evergreen/pull/62)).

- `EQ`, `NULL`, and `NOT` now reach T2 native code on s390x
  ([#65](https://github.com/atgreen/evergreen/pull/65)).

- `SEARCH` now advances list candidates without restarting each traversal and
  searches vectors without copying the entire target, avoiding long stalls
  while libraries scan large text such as TLS certificate bundles
  ([#67](https://github.com/atgreen/evergreen/pull/67)).

- Constant-kind memory barriers now use dedicated bytecode and x86 native
  fence instructions, including after compiled-file loading
  ([#30](https://github.com/atgreen/evergreen/pull/30)).

### Platform support

- Restore Windows builds and stack-limit checks by selecting the Windows OS
  backend correctly ([#85](https://github.com/atgreen/evergreen/pull/85)).

- Evergreen now builds and runs natively on riscv64 Linux (RV64GC). The
  interpreter and bytecode tiers, compiled-file loading, and heap images
  work; saved images carry a distinct `RISCV64` tag, and `*features*`
  includes `:riscv` and `:riscv64`. Native compilation and the foreign-call
  ABI are not yet ported ([#82](https://github.com/atgreen/evergreen/pull/82)).

- Recoverable null-pointer and stack-guard faults on s390x now resume at the
  runtime recovery handler instead of terminating the process
  ([#64](https://github.com/atgreen/evergreen/pull/64)).

- `DISASSEMBLE` now decodes s390x native code in-process, with mnemonics,
  operands and branch targets in binutils syntax, instead of listing raw
  bytes ([#74](https://github.com/atgreen/evergreen/pull/74)).

- The EGCL CLI now builds and runs natively on Apple Silicon macOS from source
  ([#51](https://github.com/atgreen/evergreen/pull/51)).

### Correctness and reliability

- `OPEN :DIRECTION :PROBE` now returns a closed stream and honors
  `:IF-DOES-NOT-EXIST :CREATE`, allowing Quicklisp to enable installed
  distributions. Input streams also honor explicit file creation without
  truncating existing content ([#83](https://github.com/atgreen/evergreen/pull/83)).

- Unbound slot reads now call user-defined `SLOT-UNBOUND` methods through both
  `SLOT-VALUE` and generated accessors, enabling lazy slot initialization such
  as Quicklisp's distribution directories. The default method signals
  `UNBOUND-SLOT` with the offending slot and instance
  ([#78](https://github.com/atgreen/evergreen/pull/78)).

- `ENSURE-DIRECTORIES-EXIST` now creates the final directory component of a
  directory-only pathname, allowing fresh Quicklisp distribution installations
  to create their metadata directories
  ([#79](https://github.com/atgreen/evergreen/pull/79)).

- Floating-point `FORMAT` directives now accept ratios and bignums, allowing
  Quicklisp download progress to display sizes and rates. Large ratio
  components are scaled safely, out-of-range rational arguments signal an
  arithmetic error, and negative-zero signs are preserved
  ([#76](https://github.com/atgreen/evergreen/pull/76)).

- `DIRECTORY` now returns `NIL` for wildcard searches under missing
  directories, allowing fresh Quicklisp setup without a pre-created
  `local-init/` directory ([#75](https://github.com/atgreen/evergreen/pull/75)).

- `WRITE` now uses the dynamic `*PRINT-ESCAPE*` value when `:ESCAPE` is omitted
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

- `TYPE-OF` now preserves the defining package of structure, class, and
  condition names, fixing cross-package digest copying used by TLS key
  derivation ([#57](https://github.com/atgreen/evergreen/pull/57)).

- Extension macros, including `EGCL-EXT:CAS` and atomic arithmetic, now expand
  consistently in interpreted and compiled code. Local function shadowing and
  definition-time macro snapshots are preserved
  ([#58](https://github.com/atgreen/evergreen/pull/58)).

- The EGCL fork installer now includes the Atomics CAS adapter, supporting
  CAS-dependent initialization in libraries such as `cl-cancel`
  ([#59](https://github.com/atgreen/evergreen/pull/59)).

- Native TCP listeners can now be shared across threads and fibers, honor
  backlog and address-reuse options, and use GC-safe cooperative accept and
  readiness waits on Unix; closing a listener wakes pending waits
  ([#60](https://github.com/atgreen/evergreen/pull/60)).

- Native threads now share their creator's package registry, preserving library
  symbol lookup in workers and threads created by fibers. This fixes Ironclad
  reporting SHA256 as unsupported in TLS server threads
  ([#68](https://github.com/atgreen/evergreen/pull/68)).

- `ENCODE-UNIVERSAL-TIME` now rejects invalid fields and preserves exact
  fractional time-zone offsets and large years
  ([#69](https://github.com/atgreen/evergreen/pull/69)).

- Compiled `HANDLER-BIND` now preserves lexical handler functions and initializer
  behavior, including native execution and garbage collection
  ([#70](https://github.com/atgreen/evergreen/pull/70)).

- `CHANGE-CLASS` now honors initialization arguments and class-change hooks,
  preserving retained slot values and initializing newly added slots
  ([#71](https://github.com/atgreen/evergreen/pull/71)).

- Native socket byte-read timeouts now signal `EGCL-EXT:IO-TIMEOUT`, retaining
  the affected stream so callers can distinguish expiry from other I/O errors
  ([#72](https://github.com/atgreen/evergreen/pull/72)).

- The USOCKET fork now supports binary TCP listeners, cross-thread accept and
  close, listener port queries, explicit address-reuse options, and inherited
  or independently configured read timeouts with `USOCKET:TIMEOUT-ERROR` mapping
  ([#61](https://github.com/atgreen/evergreen/pull/61)).

- `EQ` comparisons between constant operands now compile at T2, including
  comparisons used as values or branch conditions
  ([#50](https://github.com/atgreen/evergreen/pull/50)).

- Macro-generated closures now retain their enclosing `TAGBODY` exits, while
  loops without captured exits can reach T2 compilation
  ([#49](https://github.com/atgreen/evergreen/pull/49)).

- Native code now preserves live immediate values across inserted loop and
  straight-line polls, preventing runtime calls from clobbering those values
  ([#40](https://github.com/atgreen/evergreen/pull/40)).

- Native-enabled startup no longer hangs while compiling declaration-scanning
  loops; call results now retain conservative type facts at control-flow merges
  ([#38](https://github.com/atgreen/evergreen/pull/38)).

- `SETQ` and variable `SETF` now reject constants defined after the assigning
  function was compiled, and saved core images retain constant declarations
  ([#44](https://github.com/atgreen/evergreen/pull/44)).

- Repeated `POSITION`, `POSITION-IF`, and `POSITION-IF-NOT` searches now avoid
  excessive memory growth from compiling a new predicate helper on every call
  ([#42](https://github.com/atgreen/evergreen/pull/42)).

- Bit-vector results from `COPY-SEQ`, `SUBSEQ`, and `REVERSE` now preserve
  zero bits when garbage collection recycles storage
  ([#41](https://github.com/atgreen/evergreen/pull/41)).

- Generated `DEFSTRUCT` setters now enforce slot types before mutation,
  including inherited constraints and list/vector-backed structures
  ([#37](https://github.com/atgreen/evergreen/pull/37)).

- `COMPILE-FILE` no longer runs ordinary `DEFVAR` initializers early, and
  explicit compile-time initialization follows source order while existing
  bindings remain unchanged
  ([#36](https://github.com/atgreen/evergreen/pull/36)).

- Compile-time traversal now preserves remaining definitions when garbage
  collection relocates the forms being processed
  ([#35](https://github.com/atgreen/evergreen/pull/35)).

- `COMPILE-FILE` now leaves ordinary `DEFPARAMETER` initialization until load
  time, preserving existing values and avoiding duplicate initializer effects
  while retaining compile-time special declarations
  ([#34](https://github.com/atgreen/evergreen/pull/34)).

- `DEFSTRUCT` constructors now enforce declared slot types, including defaults,
  inherited constraints, BOA constructors, and list/vector-backed structures
  ([#31](https://github.com/atgreen/evergreen/pull/31)).

- Circular reader labels now preserve identity in vectors, mixed object graphs,
  arrays, and structure literals, including source-free compiled files
  ([#33](https://github.com/atgreen/evergreen/pull/33)).

- Functions returned by `MACRO-FUNCTION` now retain their macro definition
  after redefinition, and copied macro expanders survive saved-image restoration
  ([#32](https://github.com/atgreen/evergreen/pull/32)).

- Compiled `TYPEP` now returns exactly one value at every execution tier,
  without leaking secondary values from an earlier form or its argument
  ([#29](https://github.com/atgreen/evergreen/pull/29)).

## 0.0.2 - 2026-10-05

### Documentation

- Document atomic-update semantics, supported places, memory ordering, and
  current Gray-stream availability
  ([#23](https://github.com/atgreen/evergreen/pull/23)).

### Common Lisp extensions

- `EGCL-EXT:CAS`, `ATOMIC-INCF`, and `ATOMIC-DECF` now provide lock-free
  updates for supported places
  ([#20](https://github.com/atgreen/evergreen/pull/20)).

### Correctness and reliability

- Drakma plain HTTP GET and POST now work through the native TCP backend.
  The fork installer selects USOCKET's EGCL branch, and Gray-stream predicates,
  EOF handling, binary sequence reads, and array element-type aliases behave
  correctly for its stream wrappers
  ([#22](https://github.com/atgreen/evergreen/pull/22)).

- Retained generic-function values now continue dispatching through their
  generic methods after the function name is replaced by an advice closure
  ([#19](https://github.com/atgreen/evergreen/pull/19)).

- `REMF`, `PSETF`, `SHIFTF`, `ROTATEF`, and `SETF` of `VALUES` now honor
  custom place storing forms and preserve multiple store values
  ([#18](https://github.com/atgreen/evergreen/pull/18)).

- Mutexes restored from saved images now retain their names, recursive behavior,
  and shared identity while restarting unlocked with fresh native state
  ([#17](https://github.com/atgreen/evergreen/pull/17)).

- `UNINTERN` now removes the requested symbol by identity without folding its
  name, checks shadowing conflicts before changing the package, and reports
  invalid symbols and missing packages
  ([#16](https://github.com/atgreen/evergreen/pull/16)).

- Symbol lookup now preserves literal colons in imported and keyword names,
  including lookup through exports, inherited symbols, and the reader
  ([#15](https://github.com/atgreen/evergreen/pull/15)).

- `SHADOWING-IMPORT` now accepts fresh and previously uninterned symbols,
  preserving their names and identities, and propagates failures instead of
  silently reporting success
  ([#14](https://github.com/atgreen/evergreen/pull/14)).

- Package export and import operations now preserve symbol-name case, and
  export conflict checks distinguish differently cased and shadowed names
  ([#13](https://github.com/atgreen/evergreen/pull/13)).

- Reader symbol lookup now preserves escaped names and readtable case modes
  even when an uppercase symbol with the same spelling already exists
  ([#12](https://github.com/atgreen/evergreen/pull/12)).

- Compiled direct recursion now preserves errors and nonlocal exits raised
  by its stack-limit fallback instead of continuing through native callers
  ([#11](https://github.com/atgreen/evergreen/pull/11)).

- Compiled self-calls that use runtime dispatch now preserve pending errors
  and nonlocal exits instead of continuing execution after the call
  ([#10](https://github.com/atgreen/evergreen/pull/10)).

- Image-saving options now remain valid when pathname or option evaluation
  triggers a moving garbage collection, preventing corrupted option traversal
  in `SAVE-LISP-AND-DIE` and `%SAVE-CORE`
  ([#8](https://github.com/atgreen/evergreen/pull/8)).

### Debugging and observability

- Interactive debugger backtraces now preserve the failing Lisp call chain and
  available original arguments after activations unwind. Historical frames are
  labeled explicitly, and frame evaluation and locals are reported unavailable
  instead of being applied to unrelated live frames
  ([#3](https://github.com/atgreen/evergreen/pull/3)).

## 0.0.1 - 2026-10-04

This release establishes the first versioned baseline of Evergreen Common Lisp
(EGCL). EGCL is an experimental Common Lisp implementation built from scratch
in Rust. It is intended to explore a practical Lisp runtime with adaptive
native compilation, a moving collector, lightweight concurrency, and
application delivery. It targets ANSI Common Lisp, but 0.0.1 does not claim
complete conformance, compatibility stability, or production readiness.

### Execution model

- The reader, macro expander, and compiler lower supported Lisp forms to T0
  bytecode. A tree-walking evaluator remains available for bootstrap and
  compatibility paths.
- Repeatedly called functions can advance from T0 to baseline native T1 and
  optimized native T2 code. Unsupported target or operation shapes remain at a
  lower tier.
- Hot loops can enter native code during the current invocation through
  on-stack replacement (OSR).
- T2 can specialize dynamically typed operations under guarded assumptions.
  Failed guards deoptimize to a less specialized execution point while
  preserving live values and effects already performed.
- Functions remain redefinable while the program runs. Installed native code,
  OSR code, and older active definitions retain separate metadata and
  lifetimes.

### Common Lisp and runtime

- The implemented language includes the reader, packages, macros, lexical and
  dynamic binding, closures, multiple values, non-local control transfer,
  CLOS, conditions and restarts, streams, sequences, hash tables, pathnames,
  printing, and `FORMAT`. Coverage remains incomplete and is tracked by the
  specification and conformance suites.
- ASDF is included in saved distribution images. `COMPILE-FILE` produces EGCL's
  portable BFASL format for supported code, and ASDF systems can compile and
  load through the normal Lisp interfaces.
- A precise, moving generational garbage collector traces interpreted,
  bytecode, native, foreign-call, thread, and fiber roots. Stress and poison
  modes are part of runtime validation.
- Native threads provide joins, mutexes, condition variables, thread-local
  bindings, safepoints, and GC coordination.
- Stackful fibers multiplex Lisp executions over native carrier threads and
  can suspend and migrate on supported targets. Established TCP stream reads,
  writes, and readiness waits can park an unpinned fiber; connect, accept, and
  DNS operations are not yet cooperative.

### Interoperability and delivery

- The foreign function interface loads shared libraries, calls C functions,
  supports callbacks, and manages foreign memory. ABI coverage differs by
  architecture; x86-64 has the broadest call-shape support.
- The native x86-64 glibc distribution includes `egcl-jvm`, with JVM startup,
  Java calls, named bindings, Lisp implementations of Java interfaces,
  reference scopes, and shutdown. Builds configured with Python support expose
  the Lisp-facing `PY` CPython API.
- A running Lisp environment can be saved as a core image or appended to an
  executable. Images record runtime compatibility requirements and are tied to
  their architecture and runtime capabilities.
- Delivery can remove unreachable Lisp objects, functions, macros, methods,
  compiler tiers, builtin implementations, disassembly support, and the tree
  walker. A specialized delivered runtime can retain T0 only, T0+T1, or all
  three tiers.
- The Android tooling creates Makefile-based NativeActivity projects for ARM64
  devices and x86-64 emulators, including minimal and EGL/OpenGL ES templates.

### Debugging and observability

- Interactive errors enter a debugger with restarts, backtrace display, frame
  selection, frame evaluation where metadata permits, and stepping commands.
  Batch errors exit without waiting for terminal input.
- Lisp backtraces cover tree-walked, T0, native T1/T2/OSR, fiber, and outbound
  foreign-call boundaries. Historical error snapshots retain frames before
  unwinding; original arguments are reported when they have a proven stable
  location.
- Installed native code carries in-memory ELF/DWARF metadata for generated
  function names, machine-code ranges, and recoverable fixed-arity argument
  homes. Supported Linux builds register generated code through GDB's JIT
  interface.
- The runtime exposes disassembly, tier observations, event profiling,
  allocation profiling, GC statistics, tracing, and breakpoint/watchpoint
  primitives.

### Platforms and packages

- Runtime targets include Linux x86-64, AArch64, ppc64le, and s390x; Windows
  x86-64; and Android ARM64, with Android x86-64 application libraries. Native
  compiler, FFI, and validation coverage varies by target.
- The Fedora 44 x86-64 package set contains the native glibc runtime, a static
  musl runtime, glibc and static image-dumping tools for AArch64, ppc64le, and
  s390x, a Windows image-dumping tool, and Android CLI/application tooling.
- The same source RPM can build native glibc and static packages on ppc64le.
  Those POWER-native RPMs are not published as 0.0.1 assets because the release
  service does not yet provide the native OpenJDK required by that builder.
  Cross runtimes are executed under QEMU or Wine during package validation;
  packaged payloads are extracted and checked again for architecture, ASDF,
  dump/restart, and GC-stress behavior.
- Saved applications and target tools are documented in the
  [manual](docs/manual/index.md). The current support matrix is in
  [Platform support](docs/manual/user/reference/platforms.md).

### Known boundaries

- EGCL remains under active development. ANSI coverage, diagnostics,
  performance, and behavior across tiers and targets are not yet uniform.
- Source lines, inline scopes, complete local-variable recovery, DWARF unwind
  rules, full native C-stack unwinding, and capture of a concurrently running
  fiber are not complete. Optimized, overwritten, variadic, and register-only
  arguments may be unavailable in a backtrace.
- Foreign aggregate arguments are not portable across every target, and some
  non-x86 native backends support fewer optimized operations than x86-64.
- Heap images are architecture-specific. Dynamic target executables require a
  compatible system ABI, while static runtimes cannot provide every dynamic
  integration feature.
- The project specification is staged. Version 0.0.1 establishes the S5
  HotSpot-engine baseline: promoted code, OSR, and deoptimization must preserve
  results. Later specification stages remain future work.
