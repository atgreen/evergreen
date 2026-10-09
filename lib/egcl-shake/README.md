# egcl-shake-asdf

Describe a shaken executable in your application's own `.asd`, and build it
with `asdf:make`. This is the ASDF front end to EGCL's shaker, the one
the [shake guide](../../docs/manual/user/how-to/save-executable.md#shake-an-application-from-a-saved-image)
drives by hand with `egcl --image app.core --shake app.shake --output app`.

```lisp
(asdf:defsystem "my-app"
  :components ((:file "my-app")))

(asdf:defsystem "my-app/shake"
  :defsystem-depends-on ("egcl-shake-asdf")
  :class "egcl-shake-asdf:shaken-application"
  :build-operation "egcl-shake-asdf:shake-op"
  :depends-on ("my-app")
  :shake-entry "my-app::main"
  :shake-prune-packages ("MY-APP")
  :shake-dynamic :explicit)
```

```sh
egcl --eval '(require :asdf)' \
     --eval '(asdf:load-asd (truename "my-app.asd"))' \
     --eval '(asdf:make "my-app/shake")'
./build/my-app
```

`asdf:make` loads the application, writes `build/my-app.shake` from the
system's slots, saves `build/my-app.core` in a child process, shakes
it in another, and leaves the executable and its retention report
(`build/my-app.manifest`) beside them. Both steps are child processes because
saving a core terminates the process that saves it and the shaker restores a
core in a fresh process; they run the same `egcl` executable that is running
the build (override with `EGCL_RUNTIME` or `egcl-shake-asdf:*runtime-pathname*`).

The initargs mirror the specification keys one for one:

| Initarg | Specification key | Default |
| --- | --- | --- |
| `:shake-entry` (or ASDF's `:entry-point`) | `entry` | required |
| `:shake-prune-packages` | `prune-package`, one per element | none |
| `:shake-keep` | `keep`, one per element | none |
| `:shake-dynamic` `:preserve` / `:explicit` | `dynamic` | `:preserve` |
| `:shake-runtime` `:full` / `:specialized` | `runtime` | `:full` |
| `:shake-max-tier` `:t2` / `:t1` / `:t0` | `max-tier` | `:t2` |
| `:shake-runtime-keep` | `runtime-keep`, one per element | none |
| `:shake-runtime-source` | `--runtime-source` | the recorded checkout |
| `:shake-system` | the system the core loads | the primary system |
| `:shake-output` | `--output` | `build/NAME` |

Entries and keeps are symbols or `"package::name"` strings; a string is upcased
the way the reader would read it, so name a mixed-case function with a symbol.

The child that saves the core inherits `CL_SOURCE_REGISTRY`, and is handed
`asdf:*central-registry*` and the source-registry parameter this process
configured. It reads no init file, so systems found only through an init-file
hook must be named in one of those.

`crates/egcl/tests/shake_asdf_cli.rs` drives this route from a fixture
project. Installing `egcl` puts this extension in
`/usr/share/common-lisp/source/egcl-shake/`, which ASDF's default source
registry already searches.
