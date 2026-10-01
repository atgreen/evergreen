# Saved images

## `save-lisp-and-die`

```lisp
(save-lisp-and-die pathname &key executable toplevel)
```

Writes a core image of the live Lisp world and terminates the saving process.
`save-image-and-die` is an accepted alias.

!!! note "Which package the name lives in"

    These operators belong to `EGCL-EXT`, the way SBCL's belong to `SB-EXT`, and
    `COMMON-LISP-USER` sees them unqualified. The examples here are written for a
    script evaluated in `CL-USER`, where the bare name is what you want.

    Inside your own package they are not visible bare — `(defpackage :my-app
    (:use :cl))` inherits `COMMON-LISP` and nothing else, so a bare
    `save-lisp-and-die` there names a fresh, undefined symbol in `MY-APP`.
    Qualify it, or use the package:

    ```lisp
    (egcl-ext:save-lisp-and-die "app.core")            ; from any package
    (defpackage :my-app (:use :cl :egcl-ext))          ; or inherit it
    ```

    This is ordinary Common Lisp package behaviour (CLHS 11.1.2), not an EGCL
    quirk: a program is entitled to define its own `SAVE-IMAGE`, and one does —
    slynk's backend interface.

| Argument | Meaning |
| --- | --- |
| `pathname` | Output pathname designator |
| `:executable` | When true, append the core to a copy of the runtime executable |
| `:toplevel` | Function to call when the saved application starts |

Without `:toplevel`, an executable starts a REPL. Without `:executable`, the
output is a core file restored with `egcl --image FILE`.

The current implementation accepts some additional SBCL-style keywords,
including `:compression` and `:save-runtime-options`, without implementing
their effects. Do not rely on them to change the output format or startup.

## `egcl-ext:*init-hooks*`

A list of functions or function names called without arguments when a saved
image starts. Register process-specific reinitialization before saving:

```lisp
(defun reconnect-services ()
  ;; Reopen resources that cannot survive an image save.
  ...)
(pushnew 'reconnect-services egcl-ext:*init-hooks*)
```

Hooks run in list order, once per startup, after restoring the image and setting
up the current process's streams and arguments. They finish before the user
init file, `--eval`, `--load`, scripts, or the saved `:toplevel` function.
`--no-init` skips the user init file, not these hooks. A cold start without an
image does not run them. Startup uses a snapshot of the list, so changes made by
a hook affect future saves/restores; an unhandled hook error aborts startup.

Bundled UIOP registers its `call-image-restore-hook` dispatcher here. This
refreshes ASDF's user cache and UIOP's temporary directory, streams, and command
line for the current user instead of retaining the image builder's values.
Images built before this integration must be rebuilt to include the registration.

## Artifacts

| Artifact | Consumer | Contents |
| --- | --- | --- |
| Lisp source (`.lisp`) | `load` or the CLI | Readable forms |
| Compiled file (`.fasl`, BFASL contents) | `load` | Compiled-file data |
| Core image | `--image` | Live heap and registered runtime state |
| Saved executable | Operating system | Runtime binary plus embedded core |
| Android APK | Android package installer | Native runtime, manifest, and Lisp assets |

A core image is not a compiled source file. A different filename extension
does not convert between formats. Images require a compatible runtime and
target architecture; they are not an interchange format between machines of
different architectures or arbitrary EGCL builds.

The Android project generator currently packages Lisp source assets, not saved
core images. See [Images and applications](../explanation/images.md).

Format 5 records native runtime requirements in the Settings section. The CLI
validates source content identity, target triple, toolchain, Cargo features,
compiler flags, native capabilities, selected builtins, and available compiler
tiers before heap restoration.
See [native runtime specialization](../how-to/save-executable.md#specialize-the-native-runtime).
