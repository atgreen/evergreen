# Saved images

## `save-lisp-and-die`

```lisp
(save-lisp-and-die pathname &key executable toplevel)
```

Writes a core image of the live Lisp world and terminates the saving process.
`save-image-and-die` is an accepted alias.

| Argument | Meaning |
| --- | --- |
| `pathname` | Output pathname designator |
| `:executable` | When true, append the core to a copy of the runtime executable |
| `:toplevel` | Function to call when the saved application starts |

Without `:toplevel`, an executable starts a REPL. Without `:executable`, the
output is a core file restored with `torcl --image FILE`.

The current implementation accepts some additional SBCL-style keywords,
including `:compression` and `:save-runtime-options`, without implementing
their effects. Do not rely on them to change the output format or startup.

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
different architectures or arbitrary TorCL builds.

The Android project generator currently packages Lisp source assets, not saved
core images. See [Images and applications](../explanation/images.md).
