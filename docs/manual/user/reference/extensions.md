# Operating-system interface

These extensions expose process state and synchronous subprocess execution.
They are in `EGCL-EXT`. Their signatures differ from some similarly named
SBCL extensions; use this dictionary rather than substituting package names
in an SBCL call.

## Environment

### `egcl-ext:getenv` { #getenv }

**Function** `(egcl-ext:getenv name)` → string or `nil`

Returns the process environment variable named by the string `name`. Returns
`nil` when it is absent or cannot be represented by the host environment API.
The current evaluator does not enforce the specification's proposed `:env`
sandbox capability check on this operation.

```lisp
(or (egcl-ext:getenv "HOME") "no home directory")
```

## Current directory

### `egcl-ext:getcwd` { #getcwd }

**Function** `(egcl-ext:getcwd)` → namestring or `nil`

Returns the process working directory as a string with a trailing slash, or
`nil` if the host query fails. This is process state; it is not the same thing
as a dynamically bound Lisp `*default-pathname-defaults*`.

## Subprocesses

### `egcl-ext:run-program` { #run-program }

**Function** `(egcl-ext:run-program command)` → exit-code, stdout, stderr

Runs a command synchronously and captures standard output and standard error.
A list of strings names the executable and its arguments directly. On Unix,
a string command is interpreted by `/bin/sh -c`.

The returned code is `-1` when the host provides no ordinary exit code.
Captured bytes are decoded into strings with replacement for invalid UTF-8.
A failure to launch the command signals a file error. A nonzero child exit
status is a returned status, not by itself a Lisp launch error.

This entry point does not accept SBCL's `run-program` keyword interface. It
returns strings rather than a process object or live stream. Streaming and
asynchronous process management are not supplied by this function.

```lisp
(multiple-value-bind (code output errors)
    (egcl-ext:run-program '("printf" "%s" "hello"))
  (list code output errors))
;; => (0 "hello" "")
```

Use a list when no shell expansion is needed. A string intentionally permits
shell syntax and must not be assembled from untrusted fragments as though it
were an argument list. Sandbox mode denies the operation.

## Command-line arguments

### `*command-line-args*` { #command-line-args }

**Variable** list of strings

Contains the application arguments after the CLI's `--` separator. Without
arguments it is `nil`.

```sh
egcl report.lisp -- first second
```

In that script the variable is `("first" "second")`. It does not include the
executable name, script pathname, or interpreter options.

### `egcl-ext:raw-command-line-arguments` { #raw-command-line-arguments }

**Function** `(egcl-ext:raw-command-line-arguments)` → list of strings

Returns the host argument vector, including the executable as its first item.
This is a lower-level interface used by integration libraries. It is distinct
from the parsed application arguments above.

See [Starting and stopping](../../starting.md) for invocation order and
init-file behavior, and [Compilation](../../compiler.md) for the compiler
observation functions previously grouped with these operating-system calls.

## String and octet conversion

**Function** `(egcl-ext:string-to-octets string &key (external-format :utf-8) (start 0) end null-terminate)` → byte vector

**Function** `(egcl-ext:octets-to-string vector &key (external-format :utf-8) (start 0) end)` → string

These functions convert UTF-8 or ASCII text for binary files and foreign buffers.
Encoding bounds count characters; decoding bounds count bytes. `end` defaults to
sequence length. `:null-terminate t` appends one zero byte when encoding; decoding
does not treat zero as a terminator, so pass the position of the first zero as
`:end` when reading a C string. Inputs are not modified and results are fresh.

Supported format names are `:utf-8`, `:utf8`, `:ascii`, and `:us-ascii`;
`:default` means UTF-8. ASCII encoding rejects non-ASCII characters. Both
decoders replace malformed input with U+FFFD, rather than signalling as SBCL's
default decoder does. Other external formats are not supported.

## Atomic updates

**Macro** `(egcl-ext:cas place old new)` → previous value

**Macro** `(egcl-ext:atomic-incf place &optional (delta 1))` → previous value

**Macro** `(egcl-ext:atomic-decf place &optional (delta 1))` → previous value

`CAS` compares the current value of `place` with `old` using `EQ`. On a
match it stores `new`; otherwise it leaves the place unchanged. It returns
the value it observed in either case, not a success flag. Compare that result
with the expected value using `EQ` to determine whether the exchange succeeded.

| Place | `CAS` | `ATOMIC-INCF` / `ATOMIC-DECF` |
| --- | --- | --- |
| Special variable, written as a bare symbol | Yes | Yes |
| Structure-slot accessor, such as `(counter-value counter)` | Yes | Yes |
| `(svref simple-vector index)` | Yes | Yes |
| `(car cons)` or `(cdr cons)` | Yes | No |
| `(symbol-value symbol)` or `(symbol-plist symbol)` | Yes | No |
| `(slot-value instance slot-name)` for an instance-allocated slot | Yes | No |

These are not general replacements for `SETF` or `INCF`: arbitrary SETF
places and ordinary lexical variables are not supported. Symbol value cells
refer to the current dynamic binding when one exists, otherwise to the global
binding. Updating a thread-local dynamic binding does not update another
thread's binding. Place subforms and value/delta forms are evaluated once,
with place subforms evaluated before the values.

`ATOMIC-INCF` adds `delta`; `ATOMIC-DECF` subtracts it. Both the stored value
and `delta` must be fixnums or a `TYPE-ERROR` is signalled. Overflow signals
`ARITHMETIC-ERROR` rather than wrapping or promoting to a bignum. A failed
type or overflow check does not store a replacement value. Unlike `INCF`
and `DECF`, these macros return the value **before** the update.

```lisp
(let ((cell (cons :empty nil)))
  (list (egcl-ext:cas (car cell) :empty :ready)
        (egcl-ext:cas (car cell) :empty :other)
        (car cell)))
;; => (:EMPTY :READY :READY)

(let ((counter (vector 10)))
  (list (egcl-ext:atomic-incf (svref counter 0) 3)
        (egcl-ext:atomic-decf (svref counter 0))
        (svref counter 0)))
;; => (10 13 12)
```

Shared-cell updates use lock-free compare-and-exchange operations: success
has acquire/release ordering and a failed comparison has acquire ordering.
Arithmetic updates retry if another updater wins the comparison. This is not
a transaction across several places and does not make an arbitrary object
thread-safe. All concurrent writers of the same cell must follow an atomic
protocol; do not mix these operations with unsynchronized `SETF`, `INCF`, or
`DECF`, or assume an ordinary read supplies the required synchronization.

## Memory ordering

**Function** `(egcl-ext:memory-barrier &optional (kind :full))` → `nil`

**Function** `(egcl-ext:load-barrier)` → `nil`

**Function** `(egcl-ext:store-barrier)` → `nil`

These operations order memory accesses on the calling native thread. `:read`
and `load-barrier` use an acquire fence; `:write` and `store-barrier` use a release
fence; `:full` uses a sequentially consistent fence. `:data-dependency` uses the
stronger acquire fence. Unknown kinds signal `program-error`.

For a shared-memory publication protocol, acquire after reading the publication
word and before consuming its payload; release after producing the payload and
before publishing its word. The publication word also needs an appropriate
atomic access protocol. A fence does not make an arbitrary memory access atomic
or wait for another thread to arrive.

On x86-64, calls with a literal kind compile to a dedicated bytecode operation
and native T1/T2 fences: `lfence` for `:read` and `:data-dependency`, `sfence`
for `:write`, and `mfence` for `:full`. `load-barrier`, `store-barrier`, and
`memory-barrier` with its default kind use this path too. Dynamic kinds retain
the checked runtime helper; other architectures retain the runtime path.
The interface promises memory ordering, not a particular instruction sequence
or call latency. Compiled files preserve the intrinsic without source fallback.
