# Runtime extensions

This page covers selected interfaces used by the manual. It is not an inventory
of every extension or a replacement for the ANSI Common Lisp reference.

## Environment and process

| Interface | Result |
| --- | --- |
| `(torcl-ext:getenv name)` | Environment string, or `nil` |
| `(torcl-ext:getcwd)` | Working-directory namestring with a trailing slash, or `nil` |
| `(torcl-ext:raw-command-line-arguments)` | Host argument vector as strings |
| `*command-line-args*` | Script arguments following `--` |
| `(torcl-ext:run-program command)` | Three values: exit code, stdout string, stderr string |

For `run-program`, a list of strings names the program and its arguments
directly. On Unix, a string command is interpreted by `/bin/sh -c`.
The operation is synchronous and captures output. A process without a host
exit code reports `-1`; host launch failures signal a file error. Sandbox mode
denies the operation.

```lisp
(multiple-value-bind (code output errors)
    (torcl-ext:run-program '("printf" "%s" "hello"))
  (list code output errors))
;; => (0 "hello" "")
```

## Compiler observations

| Interface | Result |
| --- | --- |
| `(torcl-ext:function-tier function)` | `0`, `1`, or `2`; `nil` if unrecognized |
| `(torcl-ext:function-invoke-count function)` | Invocation count, or `nil` |
| `(torcl-ext:function-back-edge-count function)` | Back-edge count, or `nil` |
| `(torcl-ext:deopt-count)` | Process-wide native deoptimization count |
| `(torcl-ext:bail-report)` | Prints collected lowering failures; returns number of distinct reasons |

`function-tier` polls completed background compilation before reading the
installed function tier. It does not report the execution tier of a loop
entered through OSR. Application correctness must not depend on promotion.
Start TorCL with `TORCL_BAIL_TRACE=1` to collect reasons for `bail-report`.

## Scope and longer-term interfaces

The repository's older
[extension inventory](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/docs/torcl-lisp-api.md)
includes planned packages and status labels recorded at an earlier development
stage. Treat those labels as historical; check the implementation before relying
on an interface listed only there. The
[technical specification](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/INDEX.md)
defines design contracts, including work not yet implemented.
