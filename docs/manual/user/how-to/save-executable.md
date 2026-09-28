# Save an executable

Use a saved executable to ship a loaded Lisp application. Start with the TorCL
runtime for the machine that will run the application. For another target,
follow [Build for another platform](cross-build.md).

Create `build.lisp`:

```lisp
(defun main ()
  (format t "Hello from TorCL!~%"))

(save-lisp-and-die "hello" :executable t :toplevel #'main)
```

Run:

```sh
torcl --no-init --load build.lisp
./hello
```

Saving terminates the build process. Running `hello` invokes `main` and prints
`Hello from TorCL!`. Load your own application and its libraries before calling
`save-lisp-and-die`.

## Save a development session

To restart into a REPL with your definitions available, omit `:toplevel`:

```lisp
(defun square (x) (* x x))
(save-lisp-and-die "session" :executable t)
```

After the saving process exits, run `./session` and evaluate `(square 9)`.
The result is `81`.

## Save a separate core file

```lisp
(save-lisp-and-die "session.core")
```

Restart it with a compatible runtime:

```sh
torcl --image session.core
```

Use `--image` for a core file, not `load`. See the
[image reference](../reference/images.md) for format and compatibility limits.

## Deliver an application from a saved image

Delivery takes a saved core and a separate specification. It restores the core
in the delivery process, selects an entry point, removes unreachable named
functions in packages you explicitly select, and writes a standalone executable.
The input core is unchanged. Its old entry point and your init file are not run.

For example, save this application as `app.core`:

```lisp
(defpackage :my-app (:use :cl))
(in-package :my-app)
(defun greeting () "Hello from a delivered image!")
(defun unused-helper () "Development only")
(defun main () (format t "~A~%" (greeting)))
(save-lisp-and-die "app.core")
```

Create `app.delivery`:

```text
version = 1
entry = MY-APP::MAIN
prune-package = MY-APP
dynamic = explicit
```

Inspect the retention report, then deliver:

```sh
torcl --image app.core --deliver app.delivery --output hello --dry-run
torcl --image app.core --deliver app.delivery --output hello
./hello
```

The dry run writes no output files. Delivery writes `hello` and a versioned
text report, `hello.manifest`, listing retained and removed candidate functions,
retention reasons, platform, runtime policy, and file sizes. Existing executables
are replaced only after the new executable has been built. The executable and
report are separate files, not a transactional pair.

The specification is a UTF-8, line-oriented format, **not TOML or Lisp**. Write
unquoted `key = value` lines; blank lines and lines beginning with `#` are ignored.
Names are case-sensitive and normally uppercase. Unknown keys, duplicate
singleton keys, missing functions, and unknown packages are errors.

| Key | Meaning |
| --- | --- |
| `version = 1` | Required specification version |
| `entry = PACKAGE::FUNCTION` | Required existing entry function, called with no arguments |
| `prune-package = PACKAGE` | Package whose named functions may be removed; repeat for multiple packages |
| `keep = PACKAGE::FUNCTION` | Additional root, such as a dynamically selected callback; repeat as needed |
| `dynamic = preserve` | Default: retain all candidate functions to preserve unknown dynamic targets |
| `dynamic = explicit` | Opt in to pruning; declare every additional dynamic entry with `keep` |

Use `dynamic = explicit` only when the root list describes your application.
For example, `(funcall (intern command "MY-APP"))` can name functions the analyzer
cannot infer from the saved data. Add a `keep` line for each permitted target,
or leave `dynamic = preserve`. The same applies to code later introduced through
`load`, `eval`, plugins, foreign callbacks, or method redefinition. This policy
does not disable those operations or provide a security boundary.

The current pass retains all global data, symbol identities, packages, macros,
classes, methods, and functions outside the selected packages. It follows
references through saved data and source/bytecode, including nested functions
and captured environments. Conservatively retained registries can keep extra
functions alive. Runtime packages cannot be selected for pruning.

This is **image-only delivery**: it keeps the full runtime, including the
interpreter and tiered compilers. It does not rebuild Rust, remove native
builtins, or claim to minimize the whole heap. Use the same compatible target
runtime that loads the input core; delivery does not translate images between
architectures. Re-saving an executable replaces its embedded image instead of
stacking another copy of the previous core into the runtime prefix.
