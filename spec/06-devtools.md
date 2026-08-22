# §6 Developer Tools

Bliss provides a full-featured interactive development environment
matching or exceeding the facilities available in mature Common Lisp
implementations (SBCL, CCL). Developer tools are first-class subsystems:
they integrate deeply with the runtime (§2), compiler (§4), and object
model (§1) rather than being bolted on after the fact. All tools
described here MUST be usable both from the built-in REPL and
programmatically from CL code or an IDE protocol connection.

---

## 6.1 Requirements

### 6.1.1 REPL

| ID | Requirement | Level |
|----|-------------|-------|
| R6.01 | Bliss MUST provide an interactive REPL conforming to the ANSI `read`-`eval`-`print` loop semantics. | MUST |
| R6.02 | The REPL MUST support multi-line input editing with bracket/paren matching and auto-indentation. | MUST |
| R6.03 | The REPL MUST maintain per-session persistent history, saved to `~/.bliss/repl-history`. | MUST |
| R6.04 | The REPL MUST provide TAB-completion for symbols (exported and accessible), package names, keywords, and filesystem paths in `load`/`require` forms. | MUST |
| R6.05 | The REPL SHOULD integrate with the inspector (§6.5) so that printed result objects can be interactively inspected. | SHOULD |
| R6.06 | The REPL MUST honour `*`, `**`, `***`, `+`, `++`, `+++`, `/`, `//`, `///` history variables per ANSI. | MUST |
| R6.07 | The REPL MUST catch all conditions and present restarts when an unhandled error occurs, rather than aborting. | MUST |
| R6.08 | The REPL SHOULD support user-configurable prompts via `bliss-ext:*repl-prompt-function*`. | SHOULD |
| R6.09 | The REPL MAY provide syntax highlighting of input (parenthesis rainbow, keyword colouring). | MAY |

### 6.1.2 Debugger

| ID | Requirement | Level |
|----|-------------|-------|
| R6.10 | Bliss MUST provide an interactive debugger entered on unhandled conditions. | MUST |
| R6.11 | The debugger MUST display the condition, available restarts, and a numbered backtrace of stack frames. | MUST |
| R6.12 | Stack frame inspection MUST show the function name, source location (file, line, column), and local variable bindings (when debug quality ≥ 2). | MUST |
| R6.13 | The debugger MUST support the `step`, `next`, `out`, and `continue` stepping commands. | MUST |
| R6.14 | Bliss MUST support breakpoints on function entry via `(bliss-debug:break-on-entry 'fn)`. | MUST |
| R6.15 | Bliss MUST support breakpoints on source location via `(bliss-debug:break-at file line)`. | MUST |
| R6.16 | Bliss MUST support conditional breakpoints: `(bliss-debug:break-on-entry 'fn :when expr)`. | MUST |
| R6.17 | Bliss SHOULD support watchpoints: `(bliss-debug:watch 'var :test #'predicate)` triggering the debugger when a watched binding changes and the predicate returns true. Special (dynamic) variable watchpoints MUST work at all debug levels. Lexical variable watchpoints require `(optimize (debug 3))` and compiler instrumentation (see A6.04a). | SHOULD |
| R6.18 | The debugger MUST allow evaluation of arbitrary forms in the lexical environment of the selected frame. | MUST |
| R6.19 | Debug information MUST be preserved according to the `debug` optimize quality: 0 = name only; 1 = name + source location; 2 = all locals; 3 = all locals + stepping. | MUST |
| R6.20 | When running under an IDE protocol connection, debugger events MUST be forwarded to the IDE rather than presented on the terminal. | MUST |

### 6.1.3 Profiler

| ID | Requirement | Level |
|----|-------------|-------|
| R6.21 | Bliss MUST provide a statistical (sampling) profiler that records the program counter at OS timer-signal intervals. | MUST |
| R6.22 | The sampling profiler MUST map sampled PCs back to CL function names and source locations via the code-location map (§4). | MUST |
| R6.23 | The sampling profiler MUST support configurable sample rate (default 1000 Hz, range 10–10000 Hz). | MUST |
| R6.24 | Bliss MUST provide a deterministic instrumentation profiler recording per-function call counts and cumulative/self time. | MUST |
| R6.25 | Bliss MUST provide an allocation profiler tracking per-type allocation counts, sizes, and allocation-site backtraces. | MUST |
| R6.26 | Profiler results MUST be reportable as flat tables (sorted by self-time or allocation count) and as call-graph trees. | MUST |
| R6.27 | The statistical profiler SHOULD be able to produce output compatible with `perf` (via JIT-dump, see §2) and FlameGraph tooling. | SHOULD |
| R6.28 | Profiler overhead: the sampling profiler MUST add < 5% overhead at default sample rate. The instrumentation profiler MAY add up to 3× slowdown. | MUST |

### 6.1.4 Disassembler

| ID | Requirement | Level |
|----|-------------|-------|
| R6.29 | `(disassemble 'fn)` MUST display the native machine code for compiled functions. By default, the highest-tier compiled code currently installed is shown. | MUST |
| R6.30 | Disassembly output MUST be annotated with source-location markers mapping instructions back to CL source forms. | MUST |
| R6.31 | Disassembly SHOULD show IR-level annotations (e.g., type inferences) when `(>= debug 2)`. | SHOULD |
| R6.32 | For interpreted (T0) functions, `disassemble` MUST print a notice that the function has not been compiled and offer to compile it. | MUST |
| R6.32a | `disassemble` MUST accept a `:tier` keyword argument — `(disassemble 'fn :tier :t1)` — to request a specific tier's code. When the function exists at multiple tiers simultaneously (e.g., T1 code being replaced by T2 via OSR, see §4), the output MUST indicate which tier is displayed and list the other available tiers. If the requested tier is not available, a `simple-error` is signalled. | MUST |

### 6.1.5 IDE Protocol

IDE integration (SLIME/SLY, and its wire protocol variously called SWANK or
Slynk) is **not** built into the Bliss runtime. The protocol is large, tracks
the editor's releases, and is already maintained upstream; reimplementing it in
the core would duplicate library behaviour and drift. Instead — exactly as on
SBCL — Bliss loads a standard upstream backend (`slynk` or `swank`) as an
ordinary Common Lisp library, and the runtime's job is to provide the primitives
that backend depends on. A vendored copy of the backend lives under `lib/slynk/`
with a thin Bliss adapter (`communication-style nil`, single-threaded).

| ID | Requirement | Level |
|----|-------------|-------|
| R6.33 | Bliss MUST support SLIME/SLY-based IDE integration by loading a standard upstream backend (`slynk` or `swank`) as an ordinary library, the same way that backend loads on SBCL. Bliss MUST NOT build the IDE wire protocol (message framing, `:emacs-rex` dispatch, the eval/inspect/debug RPCs) into the runtime. | MUST |
| R6.34 | To host such a backend, Bliss MUST provide the runtime primitives it depends on: TCP stream sockets (listen / accept / local-port / connect / close), `(unsigned-byte 8)` socket streams supporting framed octet I/O (`read-sequence`/`write-sequence`/`read-byte`/`write-byte` and an explicit `finish-output` that flushes to the descriptor), and a blocking single-threaded (`communication-style nil`) serve loop that can be driven entirely from Lisp. | MUST |
| R6.35 | The introspection operations a SLIME/SLY backend composes MUST be available to loaded Lisp code so that eval, completion, arglist hints, cross-reference, and the inspector work through it: `macroexpand-1`/`macroexpand-all`, function arglists (§9), `find-definitions` and `xref` (callers/callees), `apropos`, `describe`, `inspect`, `compile-string`/`compile-file`, and symbol completion. | MUST |
| R6.36 | Bliss SHOULD also support an LSP (Language Server Protocol) server for non-Emacs editors, likewise loaded as a library over the same introspection primitives. | SHOULD |
| R6.37 | Connection authentication and the SLIME security model are the loaded backend's responsibility; Bliss MUST provide the file and socket primitives it uses for a per-session secret, and MUST default socket binding to localhost (`127.0.0.1`). | MUST |
| R6.38 | When Bliss's threading model (§13) provides threads, the loaded backend MUST be able to enumerate them and drive thread-focused debugging through Bliss's thread-introspection primitives; a single-threaded (`communication-style nil`) backend is permitted when threads are unavailable. | MUST |

### 6.1.6 Trace, Describe, Inspect, Room, Time

| ID | Requirement | Level |
|----|-------------|-------|
| R6.39 | `trace` and `untrace` MUST work per ANSI semantics; `trace` MUST print function entry/exit with arguments and return values, indented by call depth. | MUST |
| R6.40 | `trace` MUST support `:break`, `:condition`, and `:report` options (SBCL-compatible extensions, listed in §9). | MUST |
| R6.41 | `describe` MUST produce a human-readable summary of any object, including type, slots (for CLOS instances), and documentation strings. | MUST |
| R6.42 | `inspect` MUST provide an interactive, navigable browser for compound objects (drill into slots, array elements, hash-table entries). | MUST |
| R6.43 | `room` MUST report heap statistics: total heap size, nursery/old-gen occupancy, number of regions, bytes allocated since last GC, GC pause histogram. | MUST |
| R6.44 | `time` MUST report wall-clock time, user/system CPU time, bytes consed, GC time, number of GC pauses, and page faults. | MUST |

### 6.1.7 ASDF Integration

| ID | Requirement | Level |
|----|-------------|-------|
| R6.45 | Bliss MUST integrate ASDF 3.3+ as its system/build manager. | MUST |
| R6.46 | `(require :system-name)` MUST delegate to ASDF when the module is not a built-in. | MUST |
| R6.47 | Bliss MUST provide `bliss-ext:*asdf-output-translations*` defaulting to a per-implementation cache directory (`~/.cache/bliss/asdf/`). | MUST |
| R6.48 | ASDF `compile-op` and `load-op` MUST interact correctly with Bliss's tiered compilation: ASDF-compiled files are compiled at T1 minimum. | MUST |

---

## 6.2 Data Structures

### D6.01 — `repl-state`

```lisp
(defstruct repl-state
  (history       (make-array 0 :adjustable t :fill-pointer 0) :type vector)
  (history-index 0              :type fixnum)
  (package       *package*      :type package)
  (prompt-fn     #'default-prompt :type function)
  (input-buffer  ""             :type string)
  (level         0              :type fixnum))  ; debugger nesting depth
```

### D6.02 — `debug-frame`

A runtime representation of a single stack frame exposed to the debugger.

| Field | Type | Description |
|-------|------|-------------|
| `frame-pointer` | `(unsigned-byte 64)` | Raw frame pointer from the stack walker |
| `function` | `function` | The function object for this frame |
| `source-location` | `(or null source-loc)` | File, line, column (nil for foreign frames) |
| `locals` | `(or null simple-vector)` | Vector of `(name . value)` pairs when `debug ≥ 2` |
| `live-p` | `boolean` | Whether the frame is still on the stack |

### D6.03 — `breakpoint`

```lisp
(defstruct breakpoint
  (id          0      :type fixnum)
  (kind        :entry :type (member :entry :location :watchpoint))
  (target      nil)                 ; function-name | (file . line) | watch-target (see below)
  (condition   nil    :type (or null function))  ; predicate for conditional breakpoints
  (enabled-p   t      :type boolean)
  (hit-count   0      :type fixnum))

;; For watchpoints (kind = :watchpoint), target is a watch-target:
(defstruct watch-target
  (name        nil    :type symbol)             ; variable name
  (scope       :special :type (member :special :lexical))
  (thread      nil    :type (or null thread))   ; nil = all threads
  (frame       nil    :type (or null debug-frame)))  ; lexical scope anchor (when scope = :lexical)
```

### D6.04 — `profiler-sample`

```lisp
(defstruct profiler-sample
  (timestamp  0   :type (unsigned-byte 64))  ; nanoseconds since profiler start
  (thread-id  0   :type fixnum)
  (pc         0   :type (unsigned-byte 64))
  (backtrace  #() :type simple-vector))       ; vector of PCs (configurable depth)
```

### D6.05 — `profiler-report`

```lisp
(defstruct profiler-report
  (kind           :sampling :type (member :sampling :instrumented :allocation))
  (total-samples  0         :type fixnum)
  (elapsed-ns     0         :type (unsigned-byte 64))
  (entries        #()       :type simple-vector))  ; vector of profiler-entry
```

### D6.06 — `profiler-entry`

| Field | Type | Description |
|-------|------|-------------|
| `function` | `function-designator` | The profiled function |
| `self-samples` | `fixnum` | Samples where this function was top-of-stack |
| `total-samples` | `fixnum` | Samples where this function appeared anywhere |
| `call-count` | `(or null fixnum)` | Non-nil for instrumented profiler |
| `self-time-ns` | `(or null (unsigned-byte 64))` | Non-nil for instrumented profiler |
| `total-time-ns` | `(or null (unsigned-byte 64))` | Non-nil for instrumented profiler |
| `alloc-bytes` | `(or null fixnum)` | Non-nil for allocation profiler |
| `alloc-count` | `(or null fixnum)` | Non-nil for allocation profiler |

### D6.07 — `swank-connection` (informative)

This connection state is maintained by the **loaded** IDE backend
(`lib/slynk/`), not by the Bliss runtime — it is documented here only so the
primitives §6.1.5 requires can be traced to a concrete consumer. Bliss supplies
the `socket` stream (an `(unsigned-byte 8)` TCP stream, R6.34) and, when threads
are available (§13), the `thread`/`repl-thread` objects; the remaining fields are
purely the backend's bookkeeping.

| Field | Type | Description |
|-------|------|-------------|
| `socket` | `stream` | Bidirectional `(unsigned-byte 8)` socket stream (provided by Bliss) |
| `thread` | `thread` | Dedicated I/O thread for this connection (multi-threaded style only) |
| `repl-thread` | `thread` | REPL evaluation thread (multi-threaded style only) |
| `auth-token` | `string` | Session secret for authentication |
| `buffer-package` | `package` | Current package for this connection |
| `pending-returns` | `hash-table` | Continuation-id → callback |

---

## 6.3 Algorithms & Control Flow

### 6.3.1 REPL Loop — A6.01

```text
PROCEDURE repl-loop(state):
  LOOP:
    display-prompt(state)
    input ← read-with-editing(state)       // multi-line, bracket-aware
    IF input = :eof THEN RETURN
    push-history(state, input)
    form ← HANDLER-CASE read-from-string(input)
                READER-ERROR(e) → report(e); CONTINUE LOOP
    values ← HANDLER-BIND eval(form)
               ERROR(e) → invoke-debugger(e, state); CONTINUE LOOP
    update-history-vars(values)             // *, **, ***, /, //, ///
    print-values(values)
    CONTINUE LOOP
```

### 6.3.2 Debugger Entry — A6.02

```text
PROCEDURE invoke-debugger(condition, repl-state):
  frames ← walk-stack(current-thread)       // see §6.3.3
  restarts ← compute-restarts(condition)
  level ← repl-state.level + 1
  display-condition(condition)
  display-restarts(restarts)
  display-backtrace(frames, :count 10)      // initial 10 frames
  LOOP (debug-repl at level):
    command ← read-debug-command()
    CASE command OF
      :backtrace n    → display-backtrace(frames, :count n)
      :frame n        → select-frame(frames[n]); show-locals
      :eval form      → eval-in-frame(form, selected-frame)
      :step           → install-single-step-trap; invoke-restart 'continue
      :next           → install-next-trap(selected-frame); invoke-restart 'continue
      :out            → install-finish-trap(selected-frame); invoke-restart 'continue
      :continue       → invoke-restart(find-restart 'continue)
      :restart n      → invoke-restart(restarts[n])
      :abort          → invoke-restart(find-restart 'abort)
```

### 6.3.3 Stack Walking — A6.03

The stack walker traverses native frames using frame pointers (which
MUST be preserved when `debug ≥ 1`; see R6.19). For each frame:

1. Read the return address from the frame.
2. Look up the return address in the **code-location map** (a sorted
   array of `(pc-offset, source-loc, locals-descriptor)` tuples emitted
   by the compiler per compiled function; see §4).
3. If the address falls within a known compiled function's code range,
   construct a `debug-frame` with source location and (if available)
   decode locals from the stack/registers using the locals-descriptor.
4. If the address is in foreign (C/Rust) code, emit a foreign-frame
   marker with the symbol name from the dynamic linker (`dladdr`).
5. Continue to the caller's frame pointer until the stack bottom
   sentinel is reached.

### 6.3.4 Stepping Implementation — A6.04

Stepping is implemented by patching the next instruction(s) with
breakpoint traps (`int3` on x86-64, `brk` on AArch64):

- **Step:** patch the next CL-level source location in the current
  function and all call targets (resolved at the call site).
- **Next:** patch only the next source location in the current function
  (skip calls).
- **Out:** patch the return address of the current frame.

**Thread-locality of trap patches.** Because code pages are shared
across threads, patching a code byte with a trap instruction is
inherently process-wide — all threads executing through that address
will hit the trap. Bliss achieves thread-local stepping semantics via
a **thread-check in the trap handler**:

1. When a stepping command is issued, the stepping thread's ID is
   recorded in the thread-local `*stepping-thread*` variable, and the
   trap address(es) are registered in a global **stepping-trap table**
   mapping `pc → (thread-id, original-byte, step-kind)`.
2. When any thread hits an `int3`/`brk` trap, the trap handler
   consults the stepping-trap table:
   - If `current-thread-id = entry.thread-id`, the trap is consumed:
     the original instruction byte is restored, the debugger is
     re-entered on the stepping thread, and new traps are installed
     for the next step.
   - If `current-thread-id ≠ entry.thread-id`, the trap is
     **transparent**: the handler single-steps past the patched
     instruction (using the processor's single-step flag on x86-64,
     or a temporary instruction restore + re-patch on AArch64) and
     resumes the non-stepping thread without entering the debugger.
3. The stepping-trap table is protected by a spinlock (held only
   during the brief lookup/update in the trap handler). At most one
   thread may be stepping at a time per code address; if a second
   thread requests stepping through an already-patched address, its
   stepping request is queued until the first thread's trap fires.

### 6.3.4a Watchpoint Implementation — A6.04a

Watchpoints monitor variable bindings for changes and trigger the
debugger when a predicate is satisfied. The mechanism differs by
variable scope:

- **Special (dynamic) variables:** The runtime intercepts writes to
  watched special variable bindings by replacing the symbol's value
  cell with a **guarded cell** — a wrapper that invokes the watchpoint
  check on each `setq`/`set`/`setf symbol-value`. The guard is
  installed by `bliss-debug:watch` and removed by
  `bliss-debug:unwatch`. No compiler support is needed; all writes to
  special variables go through the value-cell indirection already.

- **Lexical variables:** Watching lexical variable mutations requires
  **compiler support** and is only available when the function is
  compiled with `(optimize (debug 3))`. At debug level 3, the
  compiler emits write-barrier instrumentation around every `setq` of
  a watched lexical variable: after each write, a check calls
  `bliss-debug::%check-watchpoint` with the new value. The watched set
  is consulted via a thread-local table indexed by the
  `(function, variable-index)` pair from the debug info.

```text
PROCEDURE check-watchpoint(watch, old-value, new-value):
  IF watch.enabled-p AND
     (watch.thread = NIL OR watch.thread = current-thread) AND
     (watch.condition = NIL OR funcall(watch.condition, old-value, new-value))
  THEN
    increment watch.hit-count
    invoke-debugger(make-condition 'watchpoint-hit
                     :variable watch.name
                     :old-value old-value
                     :new-value new-value)
```

If a lexical watchpoint is requested for a function compiled below
`debug` 3, `bliss-debug:watch` signals a `simple-warning` explaining
that the function must be recompiled with `(debug 3)` for lexical
watchpoints and offers a restart to recompile.

### 6.3.5 Sampling Profiler — A6.05

```text
PROCEDURE start-sampling-profiler(rate-hz):
  sample-buffer ← allocate-ring-buffer(MAX-SAMPLES)
  configure-timer-signal(SIGPROF, interval = 1/rate-hz)
  register-signal-handler(SIGPROF, profiler-signal-handler)

SIGNAL-HANDLER profiler-signal-handler(context):
  pc ← context.instruction-pointer
  thread-id ← current-thread-id()
  backtrace ← unwind-fast(context, MAX-DEPTH)   // frame-pointer chain only
  sample-buffer.push(make-profiler-sample(pc, thread-id, backtrace))

PROCEDURE stop-sampling-profiler() → profiler-report:
  disable-timer-signal(SIGPROF)
  RETURN resolve-samples(sample-buffer)          // map PCs → function names
```

Signal-handler safety: the signal handler MUST NOT allocate GC-managed
memory. The ring buffer uses pre-allocated slots written with atomic
operations. The PC-to-function resolution happens outside the signal
handler when the profiler is stopped.

### 6.3.6 Allocation Profiler — A6.06

The allocation profiler intercepts the TLAB allocation fast path (§3) by
inserting an allocation-site callback:

1. When enabled, each allocation records `(type-tag, size, allocation-site-pc)`.
2. Samples are stored in a per-thread append-only log (no locks needed).
3. On stop, per-thread logs are merged and grouped by allocation site
   and type tag.
4. Overhead: one indirect function call per allocation (~5-15 ns). The
   profiler MAY sample (e.g., every Nth allocation) to reduce overhead
   for allocation-heavy workloads.

### 6.3.7 IDE Protocol Dispatch — A6.07 (informative)

The dispatch loop below is implemented by the **loaded** backend (`lib/slynk/`),
not by the Bliss runtime; it is shown to make the primitive requirements of
§6.1.5 concrete. Bliss's contribution is the framed `(unsigned-byte 8)` socket
I/O (R6.34) that `read-message`/`write-message` build on, the reader/evaluator,
and the introspection operations `eval-for-emacs` calls (R6.35).

```text
PROCEDURE serve(connection):                 // backend code, running on Bliss
  LOOP:
    message ← read-message(connection.socket) // framed 6-hex-digit length + s-expr
    CASE message.type OF
      :emacs-rex  → result ← eval-for-emacs(message.form, message.package)
                    send-to-emacs(connection, (:return (:ok result) message.id))
      :emacs-interrupt → interrupt(connection.repl-thread)
      :emacs-channel-send → dispatch-channel(message)
```

Under `communication-style nil` (the single-threaded style Bliss uses today)
`serve` runs the whole loop on the calling thread and evaluates each `:emacs-rex`
inline. When Bliss's threading model (§13) provides threads, the same backend can
run the multi-threaded style, evaluating on a per-connection REPL thread with the
`pending-returns` table under a lock.

---

## 6.4 Trace / Untrace Facility

`trace` uses an **encapsulation** mechanism rather than raw
`fdefinition` replacement, so that generic function identity and
dispatch are preserved. The encapsulation layer wraps the function's
invocation without replacing the function object in the symbol's
function cell.

#### 6.4.1 Regular Functions

For ordinary (non-generic) functions, encapsulation stores the
original function and installs a wrapper that calls through:

```lisp
;; Conceptual implementation for ordinary functions
(defun install-trace (fname &key break condition report)
  (let* ((original (fdefinition fname))
         (wrapper  (lambda (&rest args)
                     (let ((*trace-depth* (1+ *trace-depth*)))
                       (when (or (null condition)
                                 (apply condition args))
                         (format *trace-output* "~V@T~D: (~S ~{~S~^ ~})~%"
                                 (* 2 (1- *trace-depth*)) (1- *trace-depth*)
                                 fname args)
                         (when break (break "Trace break on ~S" fname)))
                       (let ((values (multiple-value-list
                                      (apply original args))))
                         (format *trace-output* "~V@T~D: ~S returned ~{~S~^ ~}~%"
                                 (* 2 (1- *trace-depth*)) (1- *trace-depth*)
                                 fname values)
                         (values-list values))))))
    (bliss-debug:encapsulate fname wrapper :type :trace)
    (record-trace fname original wrapper)))
```

`bliss-debug:encapsulate` records the encapsulation in a global table
keyed by `(fname, type)` and swaps the fdefinition. `untrace` calls
`bliss-debug:unencapsulate` to restore the original.

#### 6.4.2 Generic Functions

For generic functions, directly replacing the `fdefinition` would
destroy the GF dispatch function and its method table. Instead,
tracing a generic function MUST use one of two strategies selected by
the user:

- **GF-entry tracing** (default): The GF's `:around` method
  combination is augmented with a tracing around-method that logs
  entry/exit without replacing the GF object. This preserves GF
  identity, method dispatch, and MOP protocols.
- **Per-method tracing**: `(trace fname :methods t)` individually
  traces specific methods. Each method's function is encapsulated
  independently, so the user sees entry/exit for each applicable
  method rather than the top-level GF call.

#### 6.4.3 Setf Functions and Compiler Macros

- **Setf functions**: `(trace (setf foo))` MUST work by encapsulating
  the `fdefinition` of the setf function name `(setf foo)`, following
  the same ordinary-function encapsulation path.
- **Compiler macros**: Tracing a function that has an associated
  compiler macro MUST temporarily inhibit the compiler macro for
  traced calls so the trace wrapper is actually invoked at runtime.
  The compiler macro is restored on `untrace`.

#### 6.4.4 Thread Safety

Tracing MUST be thread-safe: the encapsulation/unencapsulation
operations use `CAS` on the `fdefinition` cell to prevent lost updates
when multiple threads trace/untrace concurrently. The global trace
registry is protected by a reader-writer lock.

---

## 6.5 Inspect / Describe

### `describe`

`describe` dispatches on the object type using a generic function:

```lisp
(defgeneric describe-object (object stream))
```

Methods are defined for all standard types. For CLOS instances, all
slots are printed with their names, types, and current values (or
`#<unbound>` if unbound). Documentation strings attached via
`(documentation obj t)` are included.

### `inspect`

The interactive inspector presents a numbered list of "parts" for the
current object. The user navigates by typing a part number to drill
in, or `:pop` to return to the parent. State is maintained in a stack:

```lisp
(defstruct inspector-state
  (stack   '() :type list)        ; stack of (object . parts) for :pop
  (current nil)                    ; object being inspected
  (parts   #() :type simple-vector)) ; vector of (label . sub-object)
```

Inspector parts are computed by a generic function:

```lisp
(defgeneric inspect-object-parts (object)
  (:documentation "Return a vector of (label . sub-object) pairs."))
```

For IDE protocol connections, the inspector parts are sent as a
structured S-expression and rendered by the IDE (SLIME inspector
buffer / SLY stickers).

---

## 6.6 `room` — Heap Statistics

`room` queries the GC subsystem (§3) and formats a report:

```text
BLISS Heap Usage:
  Nursery:     1.3 MB used /  2.0 MB capacity  (65%)
  Old Gen:    42.7 MB used / 64.0 MB capacity  (67%)  [32 regions]
  Large Obj:   8.0 MB across 4 regions
  Total:      52.0 MB heap, 78.0 MB committed
  GC stats:   12 minor (avg 1.2 ms), 1 major (18.3 ms), 0 concurrent
  Allocation rate: 24.5 MB/s (since last GC)
```

`room` accepts an optional argument: `t` (full detail including
per-type breakdowns), `nil` (one-line summary), or no argument
(default medium report as shown above). This matches ANSI semantics.

---

## 6.7 `time` Macro

The `time` macro MUST use `unwind-protect` so that timing information
is reported even when `form` performs a non-local exit (e.g., `throw`,
`return-from`, `go` to an outer tagbody). On non-local exit the report
is prefixed with `"(aborted)"` to indicate the form did not complete
normally.

```lisp
(defmacro time (form)
  `(let* ((gc-count-before   (bliss-gc:gc-count))
          (gc-time-before    (bliss-gc:total-gc-time-ns))
          (bytes-before      (bliss-gc:bytes-allocated))
          (faults-before     (bliss-sys:page-faults))
          (start-real        (bliss-sys:monotonic-ns))
          (start-user        (bliss-sys:cpu-user-ns))
          (start-sys         (bliss-sys:cpu-system-ns))
          (completed-p       nil))
     (unwind-protect
         (multiple-value-prog1 ,form
           (setq completed-p t))
       (let ((elapsed-real (- (bliss-sys:monotonic-ns) start-real))
             (elapsed-user (- (bliss-sys:cpu-user-ns) start-user))
             (elapsed-sys  (- (bliss-sys:cpu-system-ns) start-sys))
             (bytes-consed (- (bliss-gc:bytes-allocated) bytes-before))
             (gc-pauses    (- (bliss-gc:gc-count) gc-count-before))
             (gc-time      (- (bliss-gc:total-gc-time-ns) gc-time-before))
             (page-faults  (- (bliss-sys:page-faults) faults-before)))
         (format *trace-output*
                 "~&~:[(aborted) ~;~]Evaluation took:~%  ~,3F seconds of real time~%  ~
                  ~,3F seconds of user run time~%  ~
                  ~,3F seconds of system run time~%  ~
                  ~:D bytes consed~%  ~
                  ~D GC pauses totalling ~,3F seconds~%  ~
                  ~D page faults~%"
                 completed-p
                 (/ elapsed-real 1e9) (/ elapsed-user 1e9) (/ elapsed-sys 1e9)
                 bytes-consed gc-pauses (/ gc-time 1e9) page-faults)))))
```

---

## 6.8 ASDF Integration

Bliss integrates ASDF 3.3+ as its system/build manager. The integration
consists of:

1. **Boot loading:** ASDF source is bundled in `lib/asdf.lisp` and
   loaded during the boot sequence (§2) after the condition system and
   streams are available.
2. **Output translations:** Default output directory is
   `~/.cache/bliss/asdf/<implementation-version>/` so FASL files do not
   collide with other CL implementations.
3. **FASL format:** Bliss FASL files (`.bfasl`) are architecture-neutral
   compiled-unit files.  They contain portable Bliss bytecode plus constant
   pools, symbol/package references, source maps, stack maps, unwind tables,
   verification metadata, and optional cached T1 code for the producing
   platform.  When loaded on another architecture, the bytecode is interpreted
   at T0 or recompiled to local T1 code.  Hot functions may be promoted to T2
   by the tiered compilation system (§4).
4. **`require` hook:** `(require :name)` first checks built-in modules,
   then delegates to `asdf:load-system`.
5. **Compilation policy:** Files compiled via ASDF `compile-op` are
   compiled at a minimum of T1 (baseline compiler). The `speed` and
   `debug` optimize qualities in `declaim`/`declare` are respected.

---

## 6.9 Error Handling

| Failure Mode | Response |
|--------------|----------|
| REPL reader encounters invalid syntax | Signal `reader-error`; REPL catches it, prints message, prompts for new input. |
| Debugger stack walk encounters corrupt frame | Skip frame, emit `[corrupt frame at 0x...]` marker, continue walking. |
| Profiler signal handler re-enters itself | Drop the sample (detected via per-thread re-entry flag). |
| SWANK socket disconnects mid-eval | Evaluation thread receives `connection-closed` condition; evaluation is aborted, resources cleaned up. |
| Breakpoint in foreign code | Bliss does NOT set breakpoints in foreign frames; attempting to do so signals `simple-error`. |
| ASDF system not found | `asdf:missing-component` condition is signalled with a restart to install via Quicklisp if available. |

---

## 6.10 Concurrency

| Resource | Protection |
|----------|------------|
| `*traced-functions*` registry | Per-entry CAS on fdefinition swap; global trace list uses a reader-writer lock. |
| SWANK `pending-returns` table | Mutex per connection. |
| Profiler sample buffer | Lock-free ring buffer (atomic head/tail indices). Signal handler writes; reporting thread reads after stop. |
| Breakpoint table | Global reader-writer lock. Breakpoint install/remove acquires write lock. Breakpoint lookup on trap acquires read lock. |
| Inspector state | Thread-local; no sharing. Each SWANK connection has its own inspector stack. |
| Allocation profiler per-thread log | Thread-local append-only buffer. Merge at stop time acquires no locks (threads are paused at safepoint). |

---

## 6.11 Configuration

| Knob | Default | Env Variable | Description |
|------|---------|--------------|-------------|
| REPL history size | 10 000 entries | `BLISS_REPL_HISTORY_SIZE` | Max lines saved to history file |
| REPL history file | `~/.bliss/repl-history` | `BLISS_HISTFILE` | Path to persistent history |
| Profiler sample rate | 1000 Hz | `BLISS_PROF_RATE` | Samples per second (10–10000) |
| Profiler max depth | 64 frames | `BLISS_PROF_DEPTH` | Maximum backtrace depth per sample |
| Profiler sample buffer | 1 M samples | `BLISS_PROF_BUFSIZE` | Ring buffer capacity |
| IDE backend listen port | 4005 | — | Default port the loaded backend (`lib/slynk/`) listens on; chosen by the editor/backend, not a runtime knob |
| IDE backend interface | `127.0.0.1` | — | Bind address; Bliss's socket primitives default to localhost-only for security (R6.37) |
| ASDF output dir | `~/.cache/bliss/asdf/` | `BLISS_ASDF_CACHE` | Output translation root |
| Debug default quality | 1 | `BLISS_DEBUG` | Default `(optimize (debug N))` |

---

## 6.12 Module Map

Developer tools span several crates and CL source files:

```
crates/
  bliss/
    src/
      main.rs           # CLI entry point, argument parsing
      repl.rs            # §6.1 — REPL loop, line editing, history
      completion.rs      # §6.1 — TAB completion engine
  bliss-rt/
    src/
      debug/
        mod.rs           # Debug subsystem initialisation
        frames.rs        # §6.3.3 — stack walker, debug-frame construction
        breakpoints.rs   # §6.3.4 — breakpoint table, trap patching
        stepping.rs      # §6.3.4 — step/next/out implementation
      profiler/
        mod.rs           # Profiler subsystem initialisation
        sampling.rs      # §6.3.5 — signal-based sampling profiler
        instrument.rs    # R6.24 — deterministic instrumentation hooks
        alloc.rs         # §6.3.6 — allocation profiler hooks
        report.rs        # Report generation (flat + call-graph)
        flamegraph.rs    # R6.27 — FlameGraph/perf-compatible output
lib/
  debugger.lisp          # §6.3.2 — CL-side debugger REPL, restart UI
  inspector.lisp         # §6.5 — describe-object, inspect-object-parts
  trace.lisp             # §6.4 — trace/untrace implementation
  profiler.lisp          # CL API: with-profiling, report-profile
  disassemble.lisp       # §6.1.4 — disassemble, source annotation
  slynk/                 # §6.1.5 — VENDORED upstream SLIME/SLY backend, loaded
                         #   as a library (NOT a built-in server). Bliss only
                         #   adds the adapter + prelude below.
    slynk.lisp           # upstream: message loop, RPCs, inspector, SLDB
    slynk-rpc.lisp       # upstream: wire protocol (framed s-expressions)
    slynk-completion.lisp# upstream: completion backend
    backend/bliss.lisp   # Bliss adapter: sockets, streams, getpid, compile hooks
    bliss-prelude.lisp   # Bliss shims the upstream code expects
    bliss-slynk-patch.lisp # single-threaded serve-requests + auth disabling
  asdf.lisp              # Bundled ASDF source
  asdf-integration.lisp  # §6.8 — require hook, output translations
```

---

## 6.13 Test Strategy

| Area | Method | Acceptance |
|------|--------|------------|
| REPL | Automated expect-style tests driving the CLI binary with PTY | Multi-line input, history recall, completion, error recovery all pass |
| Debugger | Integration tests that trigger conditions and script debugger commands via programmatic input | Backtrace matches expected frames; locals are correct; stepping visits expected source locations |
| Breakpoints | Unit tests setting breakpoints, running target functions, verifying debugger entry and hit counts | Conditional breakpoints fire only when predicate returns true |
| Sampling profiler | Run a known CPU-bound loop, verify the top function in the report matches | Self-time of target function > 80% of total; overhead < 5% |
| Allocation profiler | Allocate known quantities of known types, verify report matches | Reported counts and sizes within 1% of actual |
| Disassembler | Compile a known function, verify disassembly contains expected instruction patterns | Source annotations point to correct line numbers |
| IDE protocol | Load the vendored `lib/slynk/` backend into `bliss`, have it listen, connect a SLIME/SLY client (e.g. `icl`), and exercise the handshake | Client verifies the connection, injects its runtime, and round-trips eval + completion; replies are delivered (framed octets reach the socket) |
| Trace | Trace a function, call it, verify `*trace-output*` contains expected entry/exit lines | Nested call depth indentation is correct |
| `room` | Allocate known objects, call `room t`, parse output, verify reported sizes | Nursery/old-gen sizes within 10% of expected |
| `time` | Time a known-duration form (busy loop), verify wall-clock and bytes-consed | Wall-clock within 20% of expected; bytes-consed accurate |
| ASDF | Load an ASDF system, verify FASL output location and successful round-trip | System loads; FASLs appear in configured cache directory |
