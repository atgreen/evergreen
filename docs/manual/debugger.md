# Conditions and debugging

## Conditions in application code

EGCL implements Common Lisp condition handling and restarts. Handle an expected
failure at the point where the application can recover; leave an unexpected
failure available for inspection.

```lisp
(handler-case
    (error "Cannot read the requested record")
  (error (condition)
    (format nil "Handled: ~A" condition)))
```

A handler transfers control according to its binding form. A restart represents
a recovery action established by the program. Listing a restart does not make
an arbitrary failed operation resumable: the action and its dynamic extent
must still exist.

```lisp
(restart-case
    (invoke-restart 'use-value 42)
  (use-value (value) value))
;; => 42
```

## Entering the debugger

An unhandled error reached from an interactive REPL is reported and enters the
debugger command loop. Piped input and batch runs do not wait indefinitely for
debugger input. An unhandled batch error instead produces an unsuccessful
process exit.

Interpreted primitive failures, such as an unbound variable or `(car 42)`,
signal while the failing activation is still present. `HANDLER-BIND` handlers
see the live dynamic bindings and restarts before `UNWIND-PROTECT` cleanup;
unhandled conditions retain an owned snapshot for the batch report. Native
unbound-variable and direct-builtin failures also retain their historical call
chain before native teardown. Their condition signalling still waits for a
GC-safe interpreter boundary: retaining the trace does not make the retired
native activation available for live inspection.

A Lisp failure inside a foreign callback retains its historical Lisp frames
when the enclosing foreign call reports `EGCL-FFI:FFI-ERROR`. C still receives
the defined zero result and returns normally before that new condition is
signalled. If C invokes more callbacks before returning, the first failure's
trace remains protected from GC. `EGCL-FFI:CALLBACK-ERROR` continues to return
the latest diagnostic text. This does not provide native C internal frames or
permit Lisp nonlocal exits to cross C.

The debugger displays frames and available restarts. Frame detail depends on
the execution path and retained metadata. Optimized native code, runtime bridges,
and foreign frames do not necessarily preserve every source local. Do not
assume SBCL's debugger feature set or commands apply unchanged.

## Command dictionary

Enter commands at the debugger prompt, without wrapping the command name in a
Lisp list.

| Command | Alias | Operation |
| --- | --- | --- |
| `help` | `?` | List commands |
| `backtrace [n]` | `bt` | Display frames, optionally limited to n |
| `frame n` | `f` | Select frame n and display available local bindings |
| `eval form` | `e` | Request evaluation in the selected frame |
| `restart n` | `r`, or bare n | Invoke the numbered restart |
| `abort` | `q`, `:abort`, `:a` | Return to top level |
| `continue` | `c` | Continue through the debugger's continuation path |
| `step` | `s` | Request stepping into the next form |
| `next` | `n` | Request stepping over a call |
| `out` | `finish` | Request stepping out of the selected execution level |

Frame evaluation and stepping rely on runtime metadata and available stepping
hooks; the presence of a command is not a guarantee of full source-level
inspection across every optimized frame. Prefer an explicit named restart for
application-defined recovery.

## Inspecting a function

`disassemble` reports information about the function's installed code. In a
tiered implementation that code may change after repeated calls. An active loop
can also enter native code through OSR without changing the installed function
tier. The [compiler observations](compiler.md#function-tier) distinguish those
cases.

For performance investigations, use [profiling](profiling.md) rather than
inferring execution cost from a disassembly or a single tier label.

## Lisp snapshot functions

`egcl-debug:list-backtrace` returns an innermost-first list of owned frame
plists. `:count` defaults to 20 and `:start` to zero; both accept nonnegative
fixnums. The keys are `:function` (a name string or NIL), `:arguments` (the
original argument list when recorded), `:arguments-available-p`, and `:origin`
(`:interpreted`, `:managed`, or `:entry`). Managed frames alone do not identify
which compiler tier is executing.

An empty argument list with `:arguments-available-p` true describes a zero-argument
call. NIL availability means the arguments are unknown. Argument objects remain
valid after the call returns or GC moves them, but are shared references: later
mutation is visible through a retained snapshot. These are historical snapshots,
not handles for evaluating expressions in a live frame.

T1/T2 frames can recover original fixed-arity arguments from their managed slots
when the compiler proves those slots are never overwritten. Variadic or boxed
parameters, overwritten bindings, and native register-only entries remain
unavailable. If any argument lacks a proven location, the whole argument list is
unavailable; it is never filled with current local values or guessed slots.

`egcl-debug:print-backtrace` accepts the same count/start options and `:stream`
(default `*debug-io*`), prints with bounded circular argument formatting, and
returns NIL. A NIL stream designates `*standard-output*`.

```lisp
(egcl-debug:list-backtrace :count 10)
(egcl-debug:print-backtrace :stream *error-output* :count 20)
```

Capture inside a handler before unwinding to retain the failing call chain.
These functions capture the current Lisp execution; they do not reconstruct an
already-unwound stack from a condition object.

## Native debug metadata

Installed T1, T2, and OSR code carries an in-memory ELF/DWARF image describing
its function name and exact machine-code range. The logical backtrace collector
reads that image for native function identities. On supported Linux native
targets, the same image is registered through GDB's JIT interface, allowing GDB
to discover generated functions and resolve pending function breakpoints.
Registration ends before executable memory is freed, even if a backtrace reader
still retains the immutable metadata. An older active definition keeps its own
image when the function is redefined.

Fixed-arity native argument homes are encoded as DWARF formal parameters with
frame-relative locations. The frame base is the entry value of the platform's
first C argument register: the managed slot pointer. This follows the standard
[DWARF entry-value expression](https://dwarfstd.org/issues/230808.1.html).
The Lisp adapter supplies that known managed activation and checks every slot
bound before copying values; original homes and copied snapshots are GC roots.

Source lines, inline frames, and DWARF unwind rules are not yet emitted. GDB
also needs caller entry-value context to evaluate these locations, so recognizing
a function or its argument recipes does not guarantee a complete mixed native
backtrace or printable arguments. Lisp backtraces retain runtime adapters for
interpreted frames, bytecode frames, and fibers; precise GC maps keep their
separate collector responsibility.

Names containing NUL use `\0` in DWARF; literal backslashes are doubled so those
names stay distinguishable. Metadata generation and registration happen at code
installation and retirement. DWARF parsing happens when a trace requests native
identity and arguments, with no additional operation on each ordinary native
call. This does add installation work and memory for the debug image; it is not a claim that
backtrace support as a whole has zero runtime cost.

## Reducing a failure

Record a reproducer before changing compiler controls. Establish whether it
fails in a clean batch invocation, with the tree-walking backend, and after
loading a saved image. Preserve the exact input and output for each run.

A wrong result under GC stress is as significant as a crash. Runtime contributors
should follow the [GC safety procedure](contributing/how-to/gc-safety.md);
application users should include the relevant environment settings in the bug
report instead of interpreting every abort as an application condition.

Implementation reference: [Debugger command implementation](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl-stdlib/src/devtools.rs).
