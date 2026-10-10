# Changelog

## Unreleased

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

### Native compilation and performance

- Use published native callable entries for Rust-to-Lisp calls on Linux x86-64.
  Eligible ordinary T1 and T2 functions now use mapped native calls without
  checking exceptional status after successful returns, preserving moving
  roots, multiple values, speculative arithmetic, recursion and guard recovery
  ([#204](https://github.com/atgreen/evergreen/pull/204),
  [#207](https://github.com/atgreen/evergreen/pull/207)).
- Speed up named calls, builtin calls, `GETHASH`, nested `FUNCALL` callbacks,
  captured-variable access, simple-string access and missed hash-table lookups.
  Retain replacement checks, multiple values, GC safety and error recovery
  ([#142](https://github.com/atgreen/evergreen/pull/142),
  [#145](https://github.com/atgreen/evergreen/pull/145),
  [#149](https://github.com/atgreen/evergreen/pull/149),
  [#150](https://github.com/atgreen/evergreen/pull/150),
  [#151](https://github.com/atgreen/evergreen/pull/151),
  [#152](https://github.com/atgreen/evergreen/pull/152),
  [#153](https://github.com/atgreen/evergreen/pull/153),
  [#155](https://github.com/atgreen/evergreen/pull/155)).
- Compile captured lexical reads and writes at x86-64 T2, share compilation
  across closures with independent captures, and compile supported nested
  callbacks on x86-64 and s390x. Restore retained-body leaf inlining while
  preserving lexical `RETURN-FROM` and captured state during deoptimization
  ([#146](https://github.com/atgreen/evergreen/pull/146),
  [#147](https://github.com/atgreen/evergreen/pull/147),
  [#148](https://github.com/atgreen/evergreen/pull/148),
  [#175](https://github.com/atgreen/evergreen/pull/175)).
- Keep nullable numeric paths native and recover to native code when operand
  types change, including bignums. Remove redundant multiple-value resets and
  repeated loop tag checks, and retain only live locals in deoptimization state.
  Preserve pending recursive operands and
  x86-64 OSR-entry recovery metadata; decline unsupported checked RISC-V OSR
  entries safely ([#100](https://github.com/atgreen/evergreen/pull/100),
  [#103](https://github.com/atgreen/evergreen/pull/103),
  [#106](https://github.com/atgreen/evergreen/pull/106),
  [#131](https://github.com/atgreen/evergreen/pull/131),
  [#135](https://github.com/atgreen/evergreen/pull/135),
  [#144](https://github.com/atgreen/evergreen/pull/144)).

### Opt-in native exceptional transfers

The full native ABI rollout remains incomplete. Additional transfer paths below
remain opt-in.

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
  values. `DISASSEMBLE` distinguishes cached transfer code from installed legacy
  code ([#156](https://github.com/atgreen/evergreen/pull/156),
  [#159](https://github.com/atgreen/evergreen/pull/159),
  [#160](https://github.com/atgreen/evergreen/pull/160),
  [#168](https://github.com/atgreen/evergreen/pull/168)).

### Platform support

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
