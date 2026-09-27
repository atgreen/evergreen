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
