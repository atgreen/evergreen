# Changelog

## Unreleased

- Use native entries for eligible x86-64 T2 `FUNCALL` callbacks, including
  captured closures, and reuse adapters when callback bodies alternate.
  Preserve captured bindings and multiple values across GC and deoptimization.
  This reduces full Prechelt phone-encoding time by about 9% in the measured
  x86-64 workload ([#148](https://github.com/atgreen/evergreen/pull/148)).

- Preserve the original behavior of saved source-backed function objects after
  a new `DEFUN`. Saved FASL functions also remain callable when their function
  cell is aliased to another function
  ([#148](https://github.com/atgreen/evergreen/pull/148)).

- Compile captured lexical reads and writes at x86-64 T2, preserving shared
  bindings and captured state during deoptimization. Nested closures share
  bytecode templates and native compilations while retaining independent
  captures. Numeric phase changes recover even when sibling closures have
  updated the shared feedback, and deoptimization preserves the calling
  instance across code replacement. This reduces full Prechelt phone-encoding
  time by about 12% in the measured x86-64 workload
  ([#147](https://github.com/atgreen/evergreen/pull/147)).

- Compile supported nested callbacks in eagerly compiled x86-64 and s390x
  functions while preserving captured lexical state. This reduces full
  Prechelt phone-encoding time by about 9% in the measured x86-64 workload
  ([#146](https://github.com/atgreen/evergreen/pull/146)).

- Cache compiled GETHASH call targets while preserving both return values and
  function replacement. This reduces full Prechelt phone-encoding time by
  about 15% in the measured workload
  ([#145](https://github.com/atgreen/evergreen/pull/145)).

- Keep hot arithmetic functions native when their operands change from fixnums
  or single floats to bignums. Mixed integer loops now recover to generic T2
  code without repeatedly deoptimizing, reducing full Prechelt phone-encoding
  time by about 33% in the measured workload
  ([#144](https://github.com/atgreen/evergreen/pull/144)).

- Reduce x86-64 T2 named-call overhead and speed up runtime-state access for
  native threads and fibers, while preserving function redefinition and GC
  safety. Native callees recover from fixnum/single-float phase changes instead
  of being incorrectly left in the interpreter
  ([#142](https://github.com/atgreen/evergreen/pull/142)).

- Foreign callbacks now work on s390x: `egcl-ffi` can hand a Lisp function to
  C as a function pointer (a `qsort` comparator, an event handler), with
  arguments and results following the ELF ABI and errors contained at the C
  boundary exactly as on x86-64
  ([#141](https://github.com/atgreen/evergreen/pull/141)).

- Foreign calls on s390x now go through a generated System z ABI adapter:
  floating-point arguments and results, narrow integers, more than five
  integer or four floating-point arguments, and variadic tails all follow the
  ELF ABI. Previously `(sqrt 16d0)` through the FFI answered 16.0 and `pow`
  returned a denormal, with no error
  ([#133](https://github.com/atgreen/evergreen/pull/133)).

- T2 keeps nullable numeric paths native: testing an argument for NIL or
  another sentinel no longer triggers a premature numeric guard and repeated
  fallback to the interpreter. This also keeps the phone-encoding benchmark's
  filtering helper at T2
  ([#135](https://github.com/atgreen/evergreen/pull/135)).

- Memory barriers return NIL correctly from x86-64 T2 code when their results
  spill to the stack, preventing corrupted values and crashes while printing
  a returned list ([#132](https://github.com/atgreen/evergreen/pull/132)).

- T2 deoptimization now preserves pending operands after recursive calls and
  retains frame reconstruction metadata when a guard exists only at an x86-64
  OSR entry. RISC-V declines checked OSR entries it cannot validate, keeping
  those loops on their existing tier
  ([#131](https://github.com/atgreen/evergreen/pull/131)).

- On s390x, eligible self-recursive functions now call their T2 native entry
  directly, reducing recursive-call overhead while retaining stack-limit
  checks ([#77](https://github.com/atgreen/evergreen/pull/77)).

- T2 now propagates fixnum guards through tail-call loop parameters and
  recursive results, allowing functions such as `TAK` to use direct native
  self-calls on s390x ([#80](https://github.com/atgreen/evergreen/pull/80)).

- On s390x, native code now calls eligible leaf builtins through cached
  builtin slots instead of resolving each call by name, while retaining
  redefinition checks ([#88](https://github.com/atgreen/evergreen/pull/88)).

- GETF now signals TYPE-ERROR for improper property lists with a non-NIL
  atomic tail instead of returning the default value
  ([#89](https://github.com/atgreen/evergreen/pull/89)).

- On s390x, T2 now compiles additional type predicates and names unsupported
  opcodes in compilation diagnostics
  ([#91](https://github.com/atgreen/evergreen/pull/91)).

- riscv64 now has a T1 baseline native compiler with guarded fixnum
  arithmetic, comparisons, live on-stack replacement, and deoptimization, so
  hot functions run as RV64 machine code instead of bytecode.
  ([#93](https://github.com/atgreen/evergreen/pull/93)).

- On s390x, T1 native code now calls other native functions directly, pushing
  the callee frame inline instead of dispatching through the generic adapter.
  With T2 disabled on a z17: takl 196 to 149 ms, deriv 234 to 201, div2 460
  to 290 ([#94](https://github.com/atgreen/evergreen/pull/94)).

- Single-float arithmetic on ppc64le now returns correct values once a
  function reaches the optimizing native tier; `(+ 1.5 2.25)` previously
  returned `2.2578125` there while the interpreter and baseline tier were
  right ([#95](https://github.com/atgreen/evergreen/pull/95)).

- Fixnum `+`, `-`, `1+`, `1-`, unary `-` and the numeric comparisons
  now compile inline with type and overflow guards in ppc64le baseline native
  code instead of calling the runtime for every operation, taking a counted
  loop from about 4,800 to 1,500 instructions per iteration at T1
  ([#96](https://github.com/atgreen/evergreen/pull/96)).

- On s390x, T2 native code calls other native functions directly as well,
  with the callees resolved when the compile is queued. With T2 on, on a z17:
  takl 119 to 106 ms, deriv 217 to 189, div2 375 to 330
  ([#97](https://github.com/atgreen/evergreen/pull/97)).

- Functions containing loops now reach T2 native code on ppc64le instead of
  stopping at T1: the back-edge safepoint poll had no GC root set, which made
  the optimizing tier decline every loop
  ([#98](https://github.com/atgreen/evergreen/pull/98)).

- On s390x, T2 now compiles uncommon traps and the simple-string fast paths
  (`STRINGP`, `LENGTH`, ASCII `CHAR`), so shapes such as UIOP's `FIRST-CHAR`
  and `REDUCE :FROM-END` reach T2 as on x86-64; the T2 log names the source
  line of every s390x structural decline
  ([#99](https://github.com/atgreen/evergreen/pull/99)).

- T2 no longer emits a runtime call for every `SETQ` in a loop. Multiple-value
  resets that cannot be observed before the next reset are removed, so a
  call-free loop of assignments runs as straight-line native code; a ten-SETQ
  loop went from 1.35 s to 0.02 s for two million iterations
  ([#100](https://github.com/atgreen/evergreen/pull/100)).

- s390x T2 compilation diagnostics now report the actual register entry offset
  used by direct recursive calls
  ([#101](https://github.com/atgreen/evergreen/pull/101)).

- The default CL stack is 4 MiB on s390x (512 KiB elsewhere), so recursion
  that computes on x86-64 computes there too: `(deep 50000)` no longer signals
  `STORAGE-CONDITION` ([#102](https://github.com/atgreen/evergreen/pull/102)).

- T2 deopt frame states name only the locals the interpreter can still read.
  A `LET*` chain inside a loop no longer keeps every binding alive to the end
  of the function, so its arithmetic stays in registers and the loop
  back-edge carries only the loop variables
  ([#103](https://github.com/atgreen/evergreen/pull/103)).

- ppc64le baseline native code no longer brackets every runtime helper call
  with the SIGSEGV-recovery toggle, which cannot resume on that target yet,
  and resets multiple-values state with a bare leaf call; a counted loop
  drops from about 1,500 to 1,000 instructions per iteration at T1
  ([#104](https://github.com/atgreen/evergreen/pull/104)).

- Functions using `CAR`, `CDR`, `EQ`, `NULL`, `NOT`, the logical bit
  operations or `ASH` by a constant now reach T2 native code on ppc64le
  instead of stopping at T1
  ([#105](https://github.com/atgreen/evergreen/pull/105)).

- T2 checks a loop-carried fixnum once on loop entry (and once when a loop
  is entered through OSR) instead of re-testing its tag on every iteration
  ([#106](https://github.com/atgreen/evergreen/pull/106)).

- On s390x, a T2 deoptimization now resumes the interpreter in place, so a
  guard failing deep inside a directly recursive call returns the right value:
  `(pow2 70)` answers 2^70 instead of signalling TYPE-ERROR. A self-call with
  the wrong number of arguments now signals PROGRAM-ERROR at T2 on every
  target instead of binding whatever the argument registers held
  ([#107](https://github.com/atgreen/evergreen/pull/107)).

- On s390x, native code now calls T2 functions that may deoptimize directly
  instead of through the generic adapter, since such a callee resumes the
  interpreter in place and returns a finished value: div2 327 to 239 ms on a
  z17 ([#108](https://github.com/atgreen/evergreen/pull/108)).

- The sampled back-edge poll in ppc64le baseline native loops now fires at
  the configured threshold instead of after about four billion iterations, so
  a hot loop in a warm function requests T2 compilation and responds to
  signals and GC safepoints ([#110](https://github.com/atgreen/evergreen/pull/110)).

- On s390x, a self-recursive T2 function keeps its direct self-call entry
  when its body also calls other functions, as long as no heap value is live
  across a call; such functions no longer pay the generic adapter on every
  recursive call ([#111](https://github.com/atgreen/evergreen/pull/111)).

- riscv64 now has a T2 optimizing compiler with the s390x opcode coverage:
  guarded fixnum and single-float arithmetic, `EQ`, `CAR`/`CDR`, bitwise
  operations and constant shifts, GC-safe runtime calls, polled loops, live
  T1-to-T2 OSR and precise deoptimization
  ([#112](https://github.com/atgreen/evergreen/pull/112)).

- s390x T2 code uses shorter native instruction sequences for small constants
  and fixnum tag checks
  ([#113](https://github.com/atgreen/evergreen/pull/113)).

- A hot loop in a warm function on ppc64le now hands off from baseline
  native code into its compiled T2 loop mid-flight instead of finishing at
  the baseline tier; measured at 78 instead of 978 instructions per
  iteration ([#114](https://github.com/atgreen/evergreen/pull/114)).

- s390x T2 functions now save and restore only the callee-saved registers they
  use, reducing native call overhead
  ([#115](https://github.com/atgreen/evergreen/pull/115)).

- On s390x, `EGCL-EXT:MEMORY-BARRIER`, `LOAD-BARRIER` and `STORE-BARRIER` now
  compile to a native serialization instruction at every tier instead of a
  generic call, and functions using them persist to bfasl as on x86-64
  ([#118](https://github.com/atgreen/evergreen/pull/118)).

- s390x foreign calls now accept variadic calls supported by the existing
  fixed-arity integer dispatcher
  ([#119](https://github.com/atgreen/evergreen/pull/119)).

- Fibers on riscv64 now switch stacks natively instead of running on the
  scheduler's no-context-switch fallback, so cooperative scheduling, parking,
  and carrier migration behave as on the other Linux ports
  ([#120](https://github.com/atgreen/evergreen/pull/120)).

- Foreign calls on riscv64 now use the LP64D calling convention for every
  scalar signature, including floats, doubles, mixed argument lists and
  variadic functions, instead of the bootstrap dispatcher's fixed integer
  shapes ([#121](https://github.com/atgreen/evergreen/pull/121)).

- An inline s390x deoptimization now resumes on its owned Lisp frame when
  available, preserving original arguments without duplicating the function in
  backtraces ([#122](https://github.com/atgreen/evergreen/pull/122)).

- Recoverable null-pointer and stack-guard faults on riscv64 now resume at
  the runtime recovery handler instead of terminating the process
  ([#123](https://github.com/atgreen/evergreen/pull/123)).

- On s390x, functions that create closures (any LAMBDA in the body) now
  compile at T1 instead of staying interpreted, including loops that create a
  closure per iteration and factories whose parameters are captured
  ([#126](https://github.com/atgreen/evergreen/pull/126)).

- On s390x, T1 code now multiplies fixnums inline with a full-width overflow
  check instead of calling the numeric runtime for every `*`, deoptimizing to
  the interpreter for a bignum product or a non-fixnum operand
  ([#127](https://github.com/atgreen/evergreen/pull/127)).

- RISC-V now supports the opt-in native segment boundary, preserving callee-
  saved integer and floating-point registers across native returns and
  transfer exits ([#128](https://github.com/atgreen/evergreen/pull/128)).

- Add an EGCL Quicklisp client port for x86-64 Linux, with native TCP and
  filesystem adapters, pinned setup instructions, and verified fresh
  distribution installation, dependent-system loading, and offline reload
  ([#92](https://github.com/atgreen/evergreen/pull/92)).

- Initialize `*MACROEXPAND-HOOK*` and honor custom hooks during macroexpansion
  and compilation, including symbol macros. This enables Quicklisp's
  compilation-progress wrapper when loading downloaded systems
  ([#90](https://github.com/atgreen/evergreen/pull/90)).

- `PROBE-FILE` now accepts file streams, including closed streams, allowing
  Quicklisp to finish writing its local-project index
  ([#87](https://github.com/atgreen/evergreen/pull/87)).

- Fix `DIRECTORY` traversal of wildcard directory components, allowing
  Quicklisp to discover installed distributions. Single `*` components match
  exactly one level; `**` still searches recursively without duplicate scans
  ([#86](https://github.com/atgreen/evergreen/pull/86)).

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
