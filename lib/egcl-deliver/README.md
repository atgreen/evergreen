# egcl-deliver-asdf

Describe a delivered executable in your application's own `.asd`, and build it
with `asdf:make`. This is the ASDF front end to EGCL's delivery tool, the one
the [delivery guide](../../docs/manual/user/how-to/save-executable.md#deliver-an-application-from-a-saved-image)
drives by hand with `egcl --image app.core --deliver app.delivery --output app`.

```lisp
(asdf:defsystem "my-app"
  :components ((:file "my-app")))

(asdf:defsystem "my-app/deliver"
  :defsystem-depends-on ("egcl-deliver-asdf")
  :class "egcl-deliver-asdf:delivered-application"
  :build-operation "egcl-deliver-asdf:deliver-op"
  :depends-on ("my-app")
  :deliver-entry "my-app::main"
  :deliver-prune-packages ("MY-APP")
  :deliver-dynamic :explicit)
```

```sh
egcl --eval '(require :asdf)' \
     --eval '(asdf:load-asd (truename "my-app.asd"))' \
     --eval '(asdf:make "my-app/deliver")'
./build/my-app
```

`asdf:make` loads the application, writes `build/my-app.delivery` from the
system's slots, saves `build/my-app.core` in a child process, runs delivery on
it in another, and leaves the executable and its retention report
(`build/my-app.manifest`) beside them. Both steps are child processes because
saving a core terminates the process that saves it and delivery restores a
core in a fresh process; they run the same `egcl` executable that is running
the build (override with `EGCL_RUNTIME` or `egcl-deliver-asdf:*runtime-pathname*`).

The initargs mirror the specification keys one for one:

| Initarg | Specification key | Default |
| --- | --- | --- |
| `:deliver-entry` (or ASDF's `:entry-point`) | `entry` | required |
| `:deliver-prune-packages` | `prune-package`, one per element | none |
| `:deliver-keep` | `keep`, one per element | none |
| `:deliver-dynamic` `:preserve` / `:explicit` | `dynamic` | `:preserve` |
| `:deliver-runtime` `:full` / `:specialized` | `runtime` | `:full` |
| `:deliver-max-tier` `:t2` / `:t1` / `:t0` | `max-tier` | `:t2` |
| `:deliver-runtime-keep` | `runtime-keep`, one per element | none |
| `:deliver-runtime-source` | `--runtime-source` | the recorded checkout |
| `:deliver-system` | the system the core loads | the primary system |
| `:deliver-output` | `--output` | `build/NAME` |

Entries and keeps are symbols or `"package::name"` strings; a string is upcased
the way the reader would read it, so name a mixed-case function with a symbol.

The child that saves the core inherits `CL_SOURCE_REGISTRY`, and is handed
`asdf:*central-registry*` and the source-registry parameter this process
configured. It reads no init file, so systems found only through an init-file
hook must be named in one of those.

`crates/egcl/tests/delivery_asdf_cli.rs` drives this route from a fixture
project. Installing `egcl` puts this extension in
`/usr/share/common-lisp/source/egcl-deliver/`, which ASDF's default source
registry already searches.
