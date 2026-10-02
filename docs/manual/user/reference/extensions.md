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
