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

## Reducing a failure

Record a reproducer before changing compiler controls. Establish whether it
fails in a clean batch invocation, with the tree-walking backend, and after
loading a saved image. Preserve the exact input and output for each run.

A wrong result under GC stress is as significant as a crash. Runtime contributors
should follow the [GC safety procedure](contributing/how-to/gc-safety.md);
application users should include the relevant environment settings in the bug
report instead of interpreting every abort as an application condition.

Implementation reference: [Debugger command implementation](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl-stdlib/src/devtools.rs).
