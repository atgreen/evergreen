# Changelog

## Unreleased

### Common Lisp support

- Add `PPRINT-LOGICAL-BLOCK`, its lexical list-traversal helpers, conditional
  line breaks and indentation, and `PPRINT-FILL` / `PPRINT-LINEAR`. Nested
  blocks honor prefixes, suffixes, print limits, and circular-list labels,
  allowing Quicklisp's setup code to compile past its missing-macro failure
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

### Performance

- Constant-kind memory barriers now use dedicated bytecode and x86 native
  fence instructions, including after compiled-file loading
  ([#30](https://github.com/atgreen/evergreen/pull/30)).

### Platform support

- The EGCL CLI now builds and runs natively on Apple Silicon macOS from source
  ([#51](https://github.com/atgreen/evergreen/pull/51)).

### Correctness and reliability

- `WRITE` now uses the dynamic `*PRINT-ESCAPE*` value when `:ESCAPE` is omitted
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

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
