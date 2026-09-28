# Command line

This option listing is generated at site-build time from `help_text()` in
`crates/torcl/src/cli.rs`. It describes this checkout's command-line interface.

<!-- generated-cli-help -->

## Invocation modes

| Invocation | Behavior |
| --- | --- |
| `torcl` | Interactive REPL |
| `torcl --eval FORM` | Evaluate a form and exit |
| `torcl --load FILE` | Load a Lisp file and exit |
| `torcl FILE -- ARG…` | Run a script with application arguments |
| `torcl --image FILE` | Restore a core image |
| `torcl --image FILE --deliver SPEC --output APP` | Deliver an executable from a saved core and retention specification |

Delivery is a separate execution mode: it cannot be combined with `--eval`,
`--load`, a script, or application arguments. `--dry-run` prints its retention
report without producing files. See the
[delivery guide](../how-to/save-executable.md#deliver-an-application-from-a-saved-image)
for the specification format and dynamic-entry retention contract.

The prelude loads by default. `--no-bootstrap` selects the raw evaluator.
For the REPL, `TORCL_INIT_FILE` selects an initialization file; its default is
`~/.torclrc`. `--no-init` skips it. Script execution skips the REPL init file.

Application arguments after `--` are available as `*command-line-args*`.
`torcl-ext:raw-command-line-arguments` returns the host argument vector,
including the executable name.
