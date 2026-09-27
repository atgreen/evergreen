# Operating-system interface

These extensions expose process state and synchronous subprocess execution.
They are in `TORCL-EXT`. Their signatures differ from some similarly named
SBCL extensions; use this dictionary rather than substituting package names
in an SBCL call.

## Environment

### `torcl-ext:getenv` { #getenv }

**Function** `(torcl-ext:getenv name)` → string or `nil`

Returns the process environment variable named by the string `name`. Returns
`nil` when it is absent or cannot be represented by the host environment API.
The current evaluator does not enforce the specification's proposed `:env`
sandbox capability check on this operation.

```lisp
(or (torcl-ext:getenv "HOME") "no home directory")
```

## Current directory

### `torcl-ext:getcwd` { #getcwd }

**Function** `(torcl-ext:getcwd)` → namestring or `nil`

Returns the process working directory as a string with a trailing slash, or
`nil` if the host query fails. This is process state; it is not the same thing
as a dynamically bound Lisp `*default-pathname-defaults*`.

## Subprocesses

### `torcl-ext:run-program` { #run-program }

**Function** `(torcl-ext:run-program command)` → exit-code, stdout, stderr

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
    (torcl-ext:run-program '("printf" "%s" "hello"))
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
torcl report.lisp -- first second
```

In that script the variable is `("first" "second")`. It does not include the
executable name, script pathname, or interpreter options.

### `torcl-ext:raw-command-line-arguments` { #raw-command-line-arguments }

**Function** `(torcl-ext:raw-command-line-arguments)` → list of strings

Returns the host argument vector, including the executable as its first item.
This is a lower-level interface used by integration libraries. It is distinct
from the parsed application arguments above.

See [Starting and stopping](../../starting.md) for invocation order and
init-file behavior, and [Compilation](../../compiler.md) for the compiler
observation functions previously grouped with these operating-system calls.
