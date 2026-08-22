# §12  Glossary & Cross-Reference Appendix

---

## 12.1  Glossary of Terms

| Term | Definition | Spec Reference |
|------|-----------|----------------|
| **BlissVal** | A 64-bit tagged word representing any Common Lisp value; low 3 bits encode the primary type tag. | §1.2 (D1.01) |
| **ObjectHeader** | An 8-byte header at the start of every heap-allocated object (except cons cells), containing type ID, GC bits, identity hash, and size. | §1.3 (D1.02) |
| **TLAB** | Thread-Local Allocation Buffer — a per-thread bump-pointer arena carved from nursery regions, enabling lock-free allocation. | §3.2.2 (D3.02) |
| **Nursery** | The young-generation heap pool; objects are initially allocated here via TLABs and collected by minor GC. | §3.2.2 |
| **Region** | A fixed-size (default 2 MB) unit of heap memory; the heap is divided into regions tagged by kind (Nursery, Survivor, OldGen, LargeObject, Free). | §3.2.1 (D3.01) |
| **Remembered set** | A data structure tracking cross-generation pointers (old→young) so minor GC need not scan old-gen; implemented as a card table. | §3.7 |
| **Card table** | A byte array covering old-gen (one byte per 512-byte card) that tracks dirty regions potentially containing old→young pointers. | §3.7.1 |
| **Write barrier** | Code inserted at every reference store to maintain GC invariants: the SATB barrier captures pre-mutation values during concurrent marking, and the card barrier marks dirty cards for the remembered set. | §3.6.2, §3.7.1 |
| **SATB (Snapshot-at-the-Beginning)** | A Yuasa-style write barrier strategy for concurrent marking: the pre-mutation value of any overwritten reference is logged to ensure the marking snapshot is complete. | §3.6.2 |
| **TAMS** | Top-at-Mark-Start — per-region pointer recorded at the start of a concurrent mark cycle; objects above TAMS are implicitly live. | §3.4 |
| **Mark bitmap** | A global bitmap (one bit per 16-byte granule) storing marking state (white/black/grey) for concurrent old-gen GC, kept in side tables rather than object headers. | §3.4 (R3.17) |
| **Minor GC** | A stop-the-world parallel scavenge that copies live nursery objects into survivor space or promotes to old-gen. | §3.5 (A3.01) |
| **Major GC** | Concurrent tri-colour marking of old-gen followed by stop-the-world evacuation of high-garbage regions. | §3.6 (A3.02), §3.8 |
| **Evacuation** | The compaction strategy for old-gen: live objects in selected regions are copied to fresh regions (G1-style mixed collection). | §3.8 |
| **Survivor space** | Heap regions that hold objects surviving minor GC but not yet promoted to old-gen; two logical semi-spaces (from/to). | §3.2.3 |
| **Safepoint** | A program point where a thread's state is fully walkable by the GC; inserted at function prologues, loop back-edges, and allocation slow-paths. | §2.5, §3.9 |
| **Polling page** | A single memory page used for safepoint synchronisation; `mprotect`-ing it to PROT_NONE triggers SIGSEGV-based trapping. | §2.5.1 |
| **Green thread** | A lightweight, user-space thread (fibre) multiplexed M:N onto OS worker threads; has its own CL stack and state machine. | §2.3 (D2.01) |
| **Worker thread** | A pinned OS thread from the worker pool; owns a TLAB, a work-stealing deque, and the Rust call stack. | §2.3.1 |
| **Block-based SSA** | The T2 intermediate representation: a CFG of basic blocks with typed block parameters (the SSA form of φ); instructions produce SSA values and terminators carry control-flow edges with block-argument lists. Cranelift / TurboFan-lite lineage. | §4.3 (D4.01, D4.02) |
| **Block parameter** | A typed SSA value defined at a block's head, receiving its value from each predecessor's terminator block-argument list; replaces φ-nodes and models function parameters and control-flow merges. | §4.3 (D4.01) |
| **MachNode** | A platform-specific machine instruction node produced by lowering the block-based SSA IR; input to register allocation and code emission. | §4.7 (D4.11) |
| **Inline cache (IC)** | A per-call-site cache that accelerates generic dispatch and type checks by remembering observed type→target mappings; transitions through Uninitialized → Monomorphic → Polymorphic → Megamorphic. | §4.8 (D4.03, D4.13, A4.11) |
| **OSR (On-Stack Replacement)** | Transferring execution from one compilation tier to another (typically T0/T1 → T2) at a loop back-edge without restarting the function. | §4.6 (A4.03) |
| **Deoptimisation** | The reverse of OSR: when a speculative guard fails in T2 code, the compiled frame is deconstructed and execution resumes in the interpreter (T0). | §4.6 (A4.04) |
| **Uncommon trap** | A guard inserted by the optimising compiler at speculative points; when the guard fails, it triggers deoptimisation and records the reason. | §4.6 (D4.10) |
| **Bliss bytecode** | Architecture-neutral instruction stream produced from macroexpanded forms; executed by T0, serialized in `.bfasl`, and used as the stable PC coordinate system for debug info, profiling, OSR, and deoptimisation. | §4.4 |
| **Tier (T0/T1/T2)** | Execution tiers: T0 = bytecode interpreter; T1 = baseline compiler (unoptimised native from bytecode); T2 = optimising compiler (block-based SSA, full pass pipeline). | §0 §1.1, §4.4 |
| **Type propagation** | A forward data-flow analysis pass in the T2 compiler that infers CL types for IR nodes, enabling specialisation and type-check elimination. | §4.5 (A4.02 step 2) |
| **Escape analysis** | An optimisation pass that identifies allocations that do not escape their defining scope, enabling stack-allocation or scalar replacement. | §4.5 (D4.09) |
| **Linear scan** | The register allocation algorithm used by Bliss: a single pass over SSA live ranges assigning physical registers and spilling to stack slots. | §4.7 (A4.10) |
| **CodeBuffer** | A growable byte buffer into which the code emitter writes machine instructions; copied into a GC-managed code region upon completion. | §4.7 (D4.12) |
| **Discriminating function** | A compiled native function that implements generic function dispatch; generated from the method table and inline caches. | §5.3 (R5.14) |
| **Gray streams** | An extensibility protocol for CL streams based on CLOS generic functions (trivial-gray-streams compatible). | §5.5 (R5.23) |
| **Robin Hood hashing** | The open-addressing hash table strategy used by Bliss, with backward-shift deletion and probe-distance balancing. | §5.7 (D5.01, D5.20) |
| **Condition system** | The CL error-handling mechanism based on conditions, handlers, and restarts; conditions are CLOS instances. | §5.4 |
| **Sandbox** | A restricted evaluation mode with capability-based access control, preventing untrusted code from accessing files, network, FFI, or OS processes. | §8.2 (D8.01, D8.02) |
| **Capability set** | An immutable bitfield attached to a sandbox context controlling which system resources are accessible (deny-by-default). | §8.2.2 (D8.01) |
| **`.bimg`** | The Bliss image file format: a memory-mapped snapshot of the heap, symbol table, package registry, and compiled code cache. | §7.2 (D7.01) |
| **Relocation table** | A delta-encoded list of heap offsets requiring pointer adjustment when an image is loaded at a different base address. | §7.2.10 (A7.01) |
| **C3 linearisation** | The algorithm for computing the Class Precedence List (CPL) in CLOS, ensuring a monotonic, consistent ordering of superclasses. | §5.3 (R5.11) |
| **SWANK / Slynk** | The IDE communication protocol (SLIME/SLY compatible) providing eval, completion, debugging, and inspection over a socket connection. Bliss does not implement it; it loads a standard upstream backend as a library and provides the socket/introspection primitives (§6.1.5). | §6.1.5, §6.3.7 (D6.07) |
| **Trampoline** | A small executable code stub used for FFI callbacks; allocated from a pool of executable pages and freed when the CL callback object is GC'd. | §2.7.5 |
| **Frame pointer chain** | The linked list of `prev_fp` pointers in CL stack frames enabling O(n) stack walks for debugging and GC root scanning. | §2.4.2 (D2.02) |
| **AlienType** | The enum describing foreign (C) types for FFI marshalling: Void, Int, Float, Double, Pointer, Struct, Union, FnPtr. | §2.7.2 (D2.03) |
| **WeakPointer** | A GC-managed object whose referent field is atomically cleared to NIL when the target becomes unreachable. | §3.10.2 (D3.03) |
| **Finalization** | Deferred invocation of cleanup functions on unreachable objects; finalizers run on a dedicated thread, never during GC. | §3.10.1 (R3.12) |
| **Triple-build** | The bootstrap verification protocol: Rust T1→CL₁→CL₂→CL₃; CL₂ = CL₃ proves the compiler is a fixed point. | §11.9 |

---

## 12.2  Data-Structure Cross-Reference

| ID | Name | Chapter | Description |
|----|------|---------|-------------|
| D1.01 | BlissVal | §1.2 | 64-bit tagged pointer encoding all CL values |
| D1.02 | ObjectHeader | §1.3 | 8-byte header for heap objects (type ID, GC bits, hash, size) |
| D1.03 | Cons Cell | §1.5.1 | Headerless 16-byte pair (CAR + CDR) |
| D1.04 | Simple-Vector | §1.6.1 | Header + length + BlissVal[] data array |
| D1.05 | Simple Specialised Array | §1.6.2 | Header + length + rank + element-type tag + dimensions + packed data |
| D1.06 | String | §1.6.3 | UTF-8 encoded string (Simple-Base-String / Simple-Character-String) |
| D1.07 | Complex Array | §1.6.4 | Displaced/adjustable array with fill-pointer support |
| D1.08 | Symbol | §1.7 | 56-byte record: name, value, function, plist, package, flags, TLS index |
| D1.09 | Bignum | §1.8.1 | Arbitrary-precision integer (sign + limb array) |
| D1.10 | Ratio | §1.8.2 | Numerator/denominator pair in lowest terms |
| D1.11 | Complex | §1.8.3 | Complex number (realpart + imagpart) |
| D1.12 | Double-Float | §1.8.4 | Boxed IEEE 754 binary64 value |
| D1.13 | Hash-Table | §1.9 | Robin Hood open-addressing hash table with optional synchronisation |
| D1.14 | Structure Instance | §1.10.1 | Layout pointer + flat slot array |
| D1.15 | Standard-Object | §1.10.2 | Class pointer + indirected slot vector |
| D1.16 | Condition | §1.10.3 | Same layout as standard-object with distinct type ID |
| D1.17 | Interpreted Function | §1.11.1 | Lambda list + body + environment + name |
| D1.18 | Compiled Function | §1.11.2 | Entry point + code size + constants + tier marker |
| D1.19 | Closure | §1.11.3 | Function + flat captured-variable array |
| D1.20 | Package | §1.12 | Name + internal/external symbol tables + use-list + per-package lock |
| D1.21 | Stream | §1.13 | Direction + element-type + vtable + state + column |
| D1.22 | Pathname | §1.14 | Six CL pathname components (host, device, directory, name, type, version) |
| D1.23 | Readtable | §1.15 | Case mode + ASCII table + extended/macro/dispatch tables |
| D2.01 | GreenThread | §2.3.3 | Green thread descriptor (ID, state, stack, entry, TLS slots) |
| D2.02 | Frame Header | §2.4.2 | 40-byte CL stack frame: prev_fp, return_pc, function, code_info, flags |
| D2.03 | AlienType | §2.7.2 | FFI type descriptor enum (Void, Int, Float, Pointer, Struct, etc.) |
| D2.04 | BlissError | §2.10.1 | Rust-side runtime error enum (Oom, StackOverflow, FfiError, etc.) |
| D3.01 | RegionHeader | §3.2.1 | Per-region metadata: kind, generation age, live bytes, alloc pointers |
| D3.02 | TLAB | §3.2.2 | Thread-Local Allocation Buffer (cursor, limit, region index) |
| D3.03 | WeakPointer | §3.10.2 | Weak reference with atomically-clearable referent |
| D4.01 | IR Function/Block/Inst/Value | §4.3 | Block-based SSA: CFG of blocks with block parameters, instructions, and SSA values |
| D4.02 | IR Edge Types | §4.3 | Data (def-use / block-argument) and control (terminator successor) edges |
| D4.03 | InlineCacheEntry | §4.8 | Cached type→method mapping for generic dispatch |
| D4.04 | MethodCounters | §4.9 | Per-function invocation and back-edge counters for tier promotion |
| D4.05 | Readtable Layout | §4.1 | ASCII fast-path table + HashMap for extended Unicode chars |
| D4.06 | Environment | §4.2 | Lexical environment for macro expansion (bindings, declarations) |
| D4.07 | CompilationRequest | §4.4 | Queued request for background compilation (function, tier, profile) |
| D4.08 | Type Lattice | §4.5 | CL type lattice for type propagation pass — **ID collision**: §4.4 also assigns D4.08 to Stack Frame Layout; see §12.5 finding #8 |
| D4.08 | Stack Frame Layout | §4.4 | T1 stack frame layout diagram — **ID collision**: §4.5 also assigns D4.08 to Type Lattice; see §12.5 finding #8 |
| D4.09 | OSR Entry Map | §4.6 | Maps T1 locals to T2 SSA vars for on-stack replacement — **ID collision**: §4.5 also assigns D4.09 to Connection Graph; see §12.5 finding #9 |
| D4.09 | Connection Graph | §4.5 | Escape analysis connection graph — **ID collision**: §4.6 also assigns D4.09 to OSR Entry Map; see §12.5 finding #9 |
| D4.10 | Deopt Log | §4.6 | Per-function log of deoptimisation events with reasons |
| D4.11 | MachNode | §4.7 | Platform-specific machine instruction node |
| D4.12 | CodeBuffer | §4.7 | Growable byte buffer for machine code emission |
| D4.13 | InlineCacheSite | §4.8 | Metadata for a patchable IC location in compiled code |
| D4.14 | TypeProfileRing | §4.9 | Fixed-size ring buffer of observed types at call sites |
| D5.01 | BlissHashTable | §5.7 | Rust-side hash table struct (test, entries, count, lock) |
| D5.02 | PackageRegistry | §5.1 | Global name→Package HashMap under RwLock |
| D5.03 | Package (detailed) | §5.1 | Full package with local nicknames, conduit list, shadowing set |
| D5.04 | SymbolTable | §5.1 | Open-addressing hash table for package symbol lookup |
| D5.05 | Symbol Identity | §5.1 | Invariant: interned symbol has exactly one identity per package |
| D5.06 | Generic Function Metaobject | §5.3 | GF descriptor: name, methods, lambda-list, discriminator, cache |
| D5.07 | MethodTable | §5.3 | Sorted method list + effective method cache per GF |
| D5.08 | Dispatch Cache | §5.3 | Hash table mapping class tuples to effective methods |
| D5.09 | Instance Layout | §5.3 | CLOS instance memory layout (class ptr + slot vector) |
| D5.10 | HandlerCluster | §5.4 | Handler cluster for condition system — **ID collision**: §5.1 also assigns D5.10 to PrimitiveFn; see §12.5 finding #10 |
| D5.10 | PrimitiveFn | §5.1 | Bootstrap primitive function descriptor — **ID collision**: §5.4 also assigns D5.10 to HandlerCluster; see §12.5 finding #10 |
| D5.11 | RestartCluster | §5.4 | Cluster of restarts established by RESTART-BIND/RESTART-CASE |
| D5.12 | ThreadConditionState | §5.4 | Per-thread handler/restart stack and debugger state |
| D5.13 | Pre-allocated Storage Conditions | §5.4 | Pool of 4 pre-allocated STORAGE-CONDITION instances for OOM scenarios |
| D5.15 | BlissFileStream | §5.4 | File-backed stream with fd, buffer, external format |
| D5.16 | BlissStringStream | §5.4 | String-backed stream (input or output) |
| D5.17 | Broadcast Stream | §5.4 | Output stream fanning to multiple component streams |
| D5.18 | Concatenated Stream | §5.4 | Input stream reading sequentially from multiple sources |
| D5.19 | Two-Way Stream | §5.4 | Bidirectional stream delegating to input/output components |
| D5.20 | Echo Stream | §5.4 | Echo stream layout — **ID collision**: §5.5 also assigns D5.20 to Hash Table Layout; see §12.5 finding #11 |
| D5.20 | Hash Table Layout (detailed) | §5.5 | Robin Hood hash table with RwLock — **ID collision**: §5.4 also assigns D5.20 to Echo Stream; see §12.5 finding #11 |
| D5.21 | Synonym Stream | §5.4 | Synonym stream delegating to symbol-value — **ID collision**: §5.5 also assigns D5.21 to SEQ-DISPATCH-TABLE; see §12.5 finding #11 |
| D5.21 | SEQ-DISPATCH-TABLE | §5.5 | 2D dispatch table for sequence specialisation — **ID collision**: §5.4 also assigns D5.21 to Synonym Stream; see §12.5 finding #11 |
| D5.22 | StreamBuffer | §5.4 | Stream I/O buffer — **ID collision**: §5.5 also assigns D5.22 to TimsortMergeBuffer; see §12.5 finding #11 |
| D5.22 | TimsortMergeBuffer | §5.5 | Thread-local scratch buffer for Timsort — **ID collision**: §5.4 also assigns D5.22 to StreamBuffer; see §12.5 finding #11 |
| D5.23 | check_seq_bounds | §5.5 | Shared bounds-validation utility for :START/:END keywords |
| D5.25 | FormatOp | §5.6 | Compiled FORMAT directive node |
| D5.26 | JustifyOp | §5.6 | Justification (~<...~>) operation descriptor |
| D5.27 | XpStream | §5.6 | Pretty-printer stream with line-break / indent state |
| D5.28 | PprintDispatchTable | §5.6 | Priority-keyed dispatch table for pretty-printer |
| D5.29 | CircularityDetector | §5.6 | State machine for *PRINT-CIRCLE* detection |
| D5.30 | Pathname (CL) | §5.7 | CL pathname with six components |
| D5.31 | LogicalPathname | §5.7 | Logical pathname extending Pathname |
| D5.32 | LogicalHostEntry | §5.7 | Translation rules for a logical host |
| D5.33 | Directory Component | §5.7 | Representation of pathname directory components |
| D6.01 | repl-state | §6.2 | REPL session state (history, package, prompt, nesting level) |
| D6.02 | debug-frame | §6.2 | Debugger view of a stack frame (function, source loc, locals) |
| D6.03 | breakpoint | §6.2 | Breakpoint descriptor (kind, target, condition, hit count) |
| D6.04 | profiler-sample | §6.2 | Sampling profiler record (timestamp, thread, PC, backtrace) |
| D6.05 | profiler-report | §6.2 | Aggregated profiler report (kind, total samples, entries) |
| D6.06 | profiler-entry | §6.2 | Per-function profiler statistics (self/total time, alloc) |
| D6.07 | swank-connection | §6.2 | IDE protocol connection state (maintained by the loaded backend; informative) |
| D7.01 | Image Header | §7.2.3 | 128-byte .bimg file header (magic, version, platform, checksum) |
| D7.02 | Section Directory | §7.2.5 | Per-section entry in image file (type, offset, size) |
| D8.01 | CapabilitySet | §8.2.2 | u64 bitfield of sandbox capabilities (deny-by-default) |
| D8.02 | SandboxContext | §8.2.3 | Sandbox config: capabilities, whitelists, resource limits |
| D8.03 | AllocationThrottle | §8.4.3 | Sliding-window rate limiter for sandbox allocation |
| D9.01 | Thread | §9.2.8 | Thread object (name, state, OS handle, join waiters) — 56 bytes |
| D9.02 | Mutex | §9.2.8 | Non-recursive mutex (owner, state, futex) — 32 bytes |
| D9.03 | Waitqueue | §9.2.8 | Condition variable (queue head) — 24 bytes |
| D9.04 | Semaphore | §9.2.8 | Counting semaphore (count, wait queue) — 32 bytes |
| D9.05 | Semaphore-Notification | §9.2.8 | Notification status for semaphore wait — 16 bytes |
| D9.06 | Weak-Pointer (ext) | §9.4 | SBCL-compatible weak pointer — 24 bytes |
| D9.07 | Timer | §9.10.2 | Timer object (function, fire-time, repeat interval) — 64 bytes |
| D9.08 | CAS Expansion | §9.2.6 | Compare-and-swap macro expansion to compiler intrinsic |
| D9.09 | Recursive-Lock | §9.2.7 | Re-entrant mutex with recursion counter — 40 bytes |

---

## 12.3  Algorithm Cross-Reference

| ID | Name | Chapter | Description |
|----|------|---------|-------------|
| A1.01 | Type-Tag Check Sequences | §1.17 | Inline type predicate logic for all BlissVal tags |
| A3.01 | Minor GC (Nursery Collection) | §3.5 | Parallel stop-the-world scavenge: root scan → copy → reclaim |
| A3.02 | Concurrent Mark | §3.6 | Tri-colour marking with SATB write barrier for old-gen |
| A4.01 | Macro Expansion | §4.2 | Iterative expansion: symbol-macro → compiler-macro → regular macro |
| A4.02 | Optimisation Pass Ordering | §4.5 | Fixed 13-step pipeline: IR → type prop → fold → inline → … → emit |
| A4.03 | OSR Entry | §4.6 | Transfer execution from T0/T1 into T2 code at loop back-edges |
| A4.04 | Deoptimisation (OSR Exit) | §4.6 | Deconstruct T2 frame → reconstruct interpreter frame → resume T0 |
| A4.05 | Reader Algorithm | §4.1 | CLHS §2.2 ten-step reader algorithm |
| A4.06 | Number Parsing | §4.1 | Deterministic grammar for integer, ratio, float, complex literals |
| A4.07 | IR Verification | §4.3 | Post-pass check of SSA dominance, type consistency, edge well-formedness |
| A4.08 | Tier Promotion Decision | §4.4 | Counter-threshold logic for T0→T1 and T1→T2 transitions |
| A4.09 | Inlining Decision | §4.5 | Budget- and profile-guided inlining heuristic |
| A4.10 | Linear Scan Register Allocation | §4.7 | Single-pass register allocation over SSA live ranges |
| A4.11 | IC State Machine | §4.8 | Uninitialized → Mono → Poly (≤4) → Megamorphic transitions |
| A4.12 | Tier Promotion Trigger | §4.9 | Counter overflow detection and compilation request submission |
| A5.01 | Symbol Table Lookup/Insert | §5.1 | Open-addressing hash lookup with Robin Hood probing |
| A5.02 | Single-Dispatch Vtable | §5.3 | Vtable-based fast path for single-argument generic dispatch |
| A5.03 | Multi-Package Lock Acquisition | §5.1 | Deterministic lock ordering for cross-package operations |
| A5.04 | Snapshot Iteration / Handler Search | §5.1 / §5.4 | Copy-on-read iteration; also condition handler search algorithm |
| A5.05 | Boot Load Algorithm / Handler-Bind | §5.1 / §5.4 | Bootstrap file loading; also HANDLER-BIND establishment |
| A5.06 | HANDLER-CASE Expansion | §5.4 | Macro expansion of HANDLER-CASE into unwind + handler code |
| A5.07 | RESTART-BIND/RESTART-CASE | §5.4 | Establishment of restart clusters on the dynamic stack |
| A5.08 | COMPUTE-RESTARTS / FIND-RESTART | §5.4 | Search active restart clusters for applicable restarts |
| A5.09 | INVOKE-RESTART | §5.4 | Transfer control to a restart's function |
| A5.10 | Signalling Protocols | §5.4 | SIGNAL / ERROR / WARN / CERROR dispatch logic |
| A5.11 | Debugger Entry Protocol | §5.4 | *DEBUGGER-HOOK* → default debugger entry sequence |
| A5.12 | Namestring Reconstruction | §5.7 | Convert pathname components to a platform namestring |
| A5.13 | Translate Logical Pathname | §5.7 | Apply logical-host translation rules to produce physical pathname |
| A5.14 | Merge Pathnames | §5.7 | Merge defaults into a pathname per ANSI semantics |
| A6.01 | REPL Loop | §6.3.1 | Read → eval → print with error recovery |
| A6.02 | Debugger Entry | §6.3.2 | Stack walk → display condition/restarts → debug REPL |
| A6.03 | Stack Walking | §6.3.3 | Frame-pointer traversal with code-location map lookup |
| A6.04 | Stepping Implementation | §6.3.4 | Breakpoint-trap patching for step/next/out with thread-check |
| A6.04a | Watchpoint Implementation | §6.3.4a | Guarded cells for special vars; compiler instrumentation for lexicals |
| A6.05 | Sampling Profiler | §6.3.5 | SIGPROF-based PC sampling into lock-free ring buffer |
| A6.06 | Allocation Profiler | §6.3.6 | TLAB callback recording allocation site, type, and size |
| A6.07 | IDE Protocol Dispatch | §6.3.7 | Message-type dispatch for :emacs-rex, :emacs-interrupt, etc. (implemented by the loaded backend; informative) |
| A7.01 | Pointer Relocation | §7.2.10 | O(n) delta-decoded relocation pass on image load |
| A7.02 | Save Atomicity | §7.2.12 | Write to temp file → fsync → atomic rename |
| A8.01 | FFI Pointer Validation | §8.3.2 | Null-check + alignment check before dereference |
| A8.02 | Reader Depth Check | §8.6.2 | Recursive-descent depth counter with configurable limit |
| A8.03 | Checked Fixnum Addition | §8.7.1 | Overflow-checked 61-bit addition with bignum promotion |
| A8.04 | Lock-Order Assertion | §8.8.4 | Thread-local lock-stack validates monotonic acquisition |
| A9.01 | Hierarchical Timing Wheel | §9.10.2 | 4-level timing wheel with min-heap overflow for timer scheduling |
| A11.01 | Compiler Pass Implementation Order | §11.4.1 | 19-step bootstrap ordering for self-hosted T2 compiler |

---

## 12.4  Requirement Traceability Matrix

This table maps each primary goal (G1–G8 from §0.1) to the specific
requirements that satisfy it.

### G1 — ANSI Compliance (pass ansi-test)

| Requirement | Summary |
|-------------|---------|
| R1.01–R1.21 | Object model conforms to CL type system |
| R4.01–R4.09 | Reader implements CLHS §2.2 algorithm |
| R4.10–R4.16 | Macro expansion per CLHS §3.1.2.1.2 |
| R5.01–R5.02 | CL package exports exactly 978 ANSI symbols |
| R5.06–R5.09 | Bootstrap provides all Phase 1 primitives; boot sequence has no circular deps |
| R5.11–R5.17 | CLOS: class hierarchy, MAKE-INSTANCE, slot access, method combination, redefinition |
| R5.18–R5.22 | Full condition system hierarchy, HANDLER-BIND/CASE, restarts |
| R5.23–R5.26 | Streams: Gray streams, built-in types, external formats, thread safety |
| R5.27–R5.30 | Sequence functions with correct dispatch and sort algorithms |
| R5.31–R5.34 | Hash tables: Robin Hood, SXHASH contract, WITH-HASH-TABLE-ITERATOR |
| R5.35–R5.39 | Pathname parsing and logical pathnames per ANSI |
| R5.40–R5.43 | FORMAT: all ANSI directives; pretty-printer |
| R5.44–R5.45 | Error signalling with correct ANSI condition types |
| R10.01–R10.03 | ANSI test suite must pass; CI-gated |
| R10.13–R10.16 | Fuzz testing of reader, compiler pipeline, and FFI boundaries; triage within 48 hours |
| R10.23–R10.26 | Dedicated fuzz targets with persistent seed corpora and weekly minimisation |

### G2 — Competitive Peak Throughput (within 2× of SBCL)

| Requirement | Summary |
|-------------|---------|
| R4.27 | T2 applies full optimisation pipeline |
| R4.31–R4.37 | Type propagation, inlining, escape analysis, LICM, DCE, constant folding |
| R4.44 | Linear-scan register allocation |
| R4.48–R4.52 | Inline caches for generic dispatch |
| R5.13–R5.14 | Slot access and GF dispatch optimised by compiler |
| R5.29 | Compiler-generated specialised sequence code |
| R10.09–R10.12 | cl-bench tracking; benchmark suite on every release |
| R11.16–R11.17 | Performance targets tracked in CI per phase |

### G3 — Fast Startup (< 50 ms cold)

| Requirement | Summary |
|-------------|---------|
| R2.01 | Boot to REPL in < 50 ms |
| R7.02 | Image load via mmap in < 50 ms for 64 MB image |
| R7.03 | Pointer relocation when base address differs |
| R5.09 | Boot sequence < 200 ms from image cache |
| R10.12 | Startup time measured cold and warm |

### G4 — Tiered Compilation (interpreter → baseline → optimising)

| Requirement | Summary |
|-------------|---------|
| R4.23–R4.24 | T0 interprets any CL form; maintains invocation counters |
| R4.25–R4.26 | T1 compiles with < 1 ms latency; includes profiling stubs |
| R4.27–R4.30 | T2 applies full optimisation; runs on background thread |
| R4.38–R4.41 | OSR entry/exit; deoptimisation with blacklisting |
| R4.53–R4.58 | Profiling counters, type profiles, tier-promotion triggers |

### G5 — Modern GC (generational, concurrent, compacting)

| Requirement | Summary |
|-------------|---------|
| R3.01–R3.05 | Nursery/old-gen regions, TLAB, large-object regions |
| R3.06–R3.07 | Minor GC: STW parallel scavenge, < 2 ms target |
| R3.08–R3.10 | Concurrent old-gen marking; evacuation compaction |
| R3.11 | Remembered sets for cross-generation pointers |
| R3.12–R3.13 | Finalization; weak references |
| R3.14 | Safepoint-based (no async signal suspension) |
| R3.17 | Mark state in side-table bitmap, not object headers |
| R3.19 | Large objects never moved |
| R10.15 | FFI boundary fuzzing catches GC/runtime robustness issues |

### G6 — Native Threads (OS-thread-per-core + green scheduling)

| Requirement | Summary |
|-------------|---------|
| R2.03 | Configurable OS worker-thread pool |
| R2.04–R2.05 | M:N green threads with own CL stacks |
| R2.06–R2.07 | Walkable frames; safepoint polls at prologues/back-edges |
| R2.19 | Support ≥ 100,000 simultaneous green threads |
| R9.04 | SBCL-compatible threading API (SB-THREAD) |

### G7 — Self-Hosting (compiler and GC policy in Bliss CL)

| Requirement | Summary |
|-------------|---------|
| R11.03 | T2 compiler written in CL, compiled by T1 |
| R11.06–R11.07 | Each compiler pass independently testable; IR+opts before lowering |
| R11.09–R11.12 | Triple-build bootstrap verification (CL₂ = CL₃) |

### G8 — Embeddability (C-ABI shared library)

| Requirement | Summary |
|-------------|---------|
| R2.11–R2.14 | Platform C ABI; libffi for variadic/struct calls |
| R7.05–R7.06 | Standalone executable; libbliss.so with stable C-ABI |
| R7.14 | Shared library follows semantic versioning |
| R8.03–R8.05 | Unsafe code confined; no raw pointers to CL code |
| R8.19 | Type-safe alien value marshalling |
| R10.15 | FFI boundary functions fuzzed with random alien descriptors and data |

---

## 12.5  Cross-Chapter Consistency Notes

The following items were verified or identified during the cross-chapter
consistency audit. Items (1)–(5) were resolved in the current revision;
their resolutions are confirmed below. Any remaining findings follow.

### 12.5.1  Previously Resolved — Verification

1. **Image relocation model (§2.2 ↔ §7.2):** §2.2 now acknowledges
   that `.bimg` stores absolute pointers and that an O(n) relocation
   pass (A7.01) runs when the mapped base differs from `original_base`.
   **Status: Consistent.** Both sections reference the same algorithm
   and agree on the common-case skip when bases match.

2. **GC marking state location (§1.3 ↔ §3.4):** §1.3.1 explicitly
   states that bits 55:54 are reserved and that mark state lives in
   the side-table bitmap (§3.4). §3.4 defines the global mark bitmap
   and R3.17 mandates side-table marking. **Status: Consistent.**
   The ObjectHeader `gc_bits` field retains only structural flags
   (forwarded, pinned, remembered) written during STW phases.

3. **Large-object threshold (§1.3 ↔ §3.2.5):** §1.3 defines the
   threshold as `region_size / 2` (default 1 MB with 2 MB regions).
   §3.2.5 uses the same formula. §1.21 lists `BLISS_LARGE_OBJECT_THRESHOLD`
   as derived from region size. §3.13 shows `(derived)` with
   `region_size / 2`. **Status: Consistent.**

4. **Image checksums (§7.2 ↔ §8.9.1):** §7.2.3 specifies SHA-256
   for the header checksum (32 bytes at offset 0x60). §7.2.11 specifies
   SHA-256 for the trailing whole-file checksum. §8.9.1 lists SHA-256
   for heap and code region checksums. **Status: Consistent.** All
   sections use SHA-256 uniformly.

5. **Nursery/TLAB environment variables (§2.8.1 ↔ §3.13):** §2.8.1
   lists `BLISS_TLAB_SIZE` (default 2m) and `BLISS_NURSERY_SIZE`
   (default 64m) as separate variables. §3.13 lists matching
   `--tlab-size` and `--nursery-size` CLI flags with the same defaults.
   **Status: Consistent.** The split correctly separates per-thread
   TLAB sizing from total nursery pool sizing.

### 12.5.2  Remaining Findings

6. **Heap size default: §2.8.1 vs §3.13 vs §7.6.2.**
   - §2.8.1 lists `BLISS_HEAP_SIZE` default as `512m`.
   - §3.13 lists `--heap-size` default as `256 MB`.
   - §7.6.2 lists `BLISS_HEAP_SIZE` default as `256m`.
   - **Recommendation:** Unify to a single value. §3.13 and §7.6.2
     agree on 256 MB; §2.8.1 should be updated to match.

7. **`BLISS_NURSERY_SIZE` semantics: §7.6.2 vs §2.8.1/§3.13.**
   - §7.6.2 describes `BLISS_NURSERY_SIZE` as "Per-thread nursery
     (TLAB) size" with default `2m`, conflating it with TLAB size.
   - §2.8.1 and §3.13 correctly distinguish nursery pool (64m) from
     TLAB (2m) as separate variables.
   - **Recommendation:** §7.6.2 should list both `BLISS_NURSERY_SIZE`
     (default 64m, total nursery pool) and `BLISS_TLAB_SIZE` (default
     2m, per-thread buffer) to match §2.8.1 and §3.13.

8. **D4.08 dual assignment.**
   - §4.5 defines D4.08 as the "CL Type Lattice" data structure.
   - §4.4 defines D4.08 as the "Stack Frame Layout" diagram.
   - Both are distinct data structures sharing the same ID.
   - **Recommendation:** Renumber the type lattice to D4.08a or assign
     a new ID (e.g., D4.15) to eliminate ambiguity.

9. **D4.09 dual assignment.**
   - §4.6 defines D4.09 as the "OSR Entry Map".
   - §4.5 defines D4.09 as the "Connection Graph" for escape analysis.
   - **Recommendation:** Renumber one; e.g., escape analysis connection
     graph becomes D4.16.

10. **D5.10 dual assignment.**
    - §5.1 (packages-bootstrap) defines D5.10 as `PrimitiveFn`.
    - §5.4 (conditions) defines D5.10 as `HandlerCluster`.
    - **Recommendation:** Renumber the bootstrap PrimitiveFn to D5.14
      (currently unassigned).

11. **D5.20/D5.21/D5.22 dual assignments (streams ↔ sequences).**
    - §5.4 (streams) defines D5.20 as Echo Stream, D5.21 as Synonym
      Stream, and D5.22 as StreamBuffer.
    - §5.5 (sequences-hashtables) defines D5.20 as Hash Table Layout,
      D5.21 as SEQ-DISPATCH-TABLE, and D5.22 as TimsortMergeBuffer.
    - **Recommendation:** Renumber the sequences-hashtables IDs to
      D5.34–D5.36 (or higher) to avoid collision.

12. **Missing D5.14 and D5.24.**
    - No data structure is assigned D5.14 or D5.24 in any chapter.
    - These gaps may be intentional but should be noted for future
      assignment when resolving the dual-assignment issues above.

13. **A5.04 and A5.05 dual assignments.**
    - §5.1 defines A5.04 as "Snapshot Iteration" and A5.05 as the
      "Boot Load Algorithm".
    - §5.4 defines A5.04 as "Handler Search" and A5.05 as
      "HANDLER-BIND Establishment".
    - **Recommendation:** Renumber the §5.4 algorithms to A5.15–A5.16
      (or higher) to avoid collision with §5.1.

14. **`*READ-EVAL*` default: §4.1 reader vs §8.6.1.**
    - §8.6.1 and §8.11 specify `*READ-EVAL*` default as NIL (R8.13).
    - ANSI CL specifies `*READ-EVAL*` default as T.
    - This is an intentional security-hardening divergence from ANSI
      (documented in §8), but it should be explicitly listed in §9
      (extensions) as a deliberate deviation with rationale, since it
      changes a standard variable's default.
    - **Recommendation:** Add an entry to §9 noting this safe-default
      override.
