# Starting and stopping

## Invocation modes

With no batch operation, `egcl` starts a read–eval–print loop. At the prompt,
enter forms and read their results. End of input or the standalone form `(quit)`
or `(exit)` leaves the REPL successfully.

```sh
egcl --no-init
```

The REPL recognizes those exit forms directly. Do not assume this provides an
ordinary callable `quit` function for arbitrary application code.

For batch evaluation, use `--eval` (or `-e`). Repeated occurrences run in order
in the same environment; an error stops the sequence.

```sh
egcl --eval '(defparameter *answer* 40)' --eval '(+ *answer* 2)'
```

`--load FILE` loads a file and exits. A positional pathname runs a script.
Arguments following `--` become the script's `*command-line-args*`.

```sh
egcl report.lisp -- input.csv output.csv
```

Prefer one batch mode per invocation. The current driver selects evaluation
forms before `--load`, and `--load` before a positional script; it does not
execute every supplied mode in command-line order.

## Prelude and initialization files

The Lisp prelude provides macros and functions needed by the bootstrap. It is
loaded automatically for ordinary starts that have not restored a core.
`--bootstrap` is retained as a compatibility option; `--no-bootstrap` requests
the raw evaluator and is intended for bootstrap work.

The interactive REPL then looks for `EGCL_INIT_FILE`, or `~/.egclrc` if that
environment variable is unset. An absent init file is normal. An evaluation
error in the init file is printed to standard error and startup continues.

| Startup | Reads the user's init file? |
| --- | --- |
| Ordinary interactive REPL | Yes |
| REPL with `--no-init` | No |
| `--eval`, `--load`, or positional script | No |
| Embedded saved executable with an application toplevel | No |
| Embedded saved REPL without an application toplevel | Yes, unless `--no-init` |

Do not put a batch application's required dependency setup only in `~/.egclrc`.
Load it explicitly in the script. This is particularly relevant to ASDF source
registries and package-manager search hooks.

## Return values and errors

Interactive evaluation prints the resulting value and returns to the prompt.
An error is reported and may enter the [debugger](debugger.md) on an interactive
terminal. Noninteractive input does not enter a blocking debugger loop.

Batch evaluation and loading report an unhandled error to standard error and
exit unsuccessfully. A normally completed script exits successfully. Lisp
conditions handled by the application need not become process failures.

## Restoring and saving a world

Use `egcl --image FILE` to start from a core file. To save the current world,
call `save-lisp-and-die`; saving terminates that process. A saved executable's
`:toplevel` designates the application entry function.

The [image dictionary](user/reference/images.md) gives the accepted options and
compatibility limits. [Save an executable](user/how-to/save-executable.md)
provides a complete build recipe, including a REPL image and separate core.

## Command-line reference

The complete public help listing is [Command line](user/reference/cli.md).
Options present only in implementation diagnostics should not be treated as
stable application configuration simply because the parser accepts them.

Implementation reference: [CLI startup and REPL](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl/src/cli.rs).
