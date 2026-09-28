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
| `runtime = full` | Default: reuse the full delivery driver, without invoking Cargo |
| `runtime = specialized` | Build a matching release runtime with only the required optional native capabilities |
| `runtime-keep = disassembly` | Additional native capability root; supported names are `disassembly` and `dynamic-code` |

Use `dynamic = explicit` only when the root list describes your application.
For example, `(funcall (intern command "MY-APP"))` can name functions the analyzer
cannot infer from the saved data. Add a `keep` line for each permitted target,
or leave `dynamic = preserve`. Reachable arbitrary-code entry points such as
`EVAL` and `LOAD` retain all candidate functions and native capabilities even
with `dynamic = explicit`: the retention policy cannot override that dependency.
Plugins, foreign callbacks, and method redefinition also need explicit roots
when their targets cannot be inferred. This is a delivery policy, not a security
boundary.

The current pass retains all global data, symbol identities, packages, macros,
classes, methods, and functions outside the selected packages. It follows
references through saved data and source/bytecode, including nested functions
and captured environments. Conservatively retained registries can keep extra
functions alive. Runtime packages cannot be selected for pruning.

With the default `runtime = full`, delivery reduces the saved image and keeps
the full Rust runtime. Re-saving an executable replaces its embedded image
instead of stacking another copy of the previous core into the runtime prefix.

## Specialize the native runtime

Add this to the same delivery specification:

```text
runtime = specialized
```

Then run delivery with a matching source checkout:

```sh
torcl --image app.core --deliver app.delivery --output hello \
  --runtime-source /path/to/torcl
```

The source checkout defaults to the location recorded when the driver was built.
You need Cargo, the matching Rust toolchain, its target libraries, and the target
linker. The new runtime must execute on the delivery machine so its compatibility
contract can be checked; this command does not cross-deliver images.

Native capabilities are selected by the reachability graph. A retained
`DISASSEMBLE` reference keeps disassembly. A retained `EVAL`, `COMPILE`, `LOAD`,
`COMPILE-FILE`, `REQUIRE`, or reader operation supporting `#.` keeps
`dynamic-code`, which in turn keeps **every** native capability and candidate
Lisp function. The report names these roots. `dynamic = preserve` retains all
capabilities; explicit `runtime-keep` entries add roots, never remove required
ones.

When `dynamic-code` is unreachable, the specialized runtime omits those public
source-evaluation entry points and rejects `--eval`, `--load`, scripts, the
REPL, and legacy source images. It also skips init files. Validated saved cores
remain supported. Raw source-lambda invocation is an evaluation entry too:
saved raw lambda lists (including constant-pool and global data) are retained
conservatively, and applications that
construct them dynamically must retain `dynamic-code`.

The GC, T0 bytecode interpreter, and tiered compilers remain available.
Deoptimization resumes T0 bytecode; it does not require public `EVAL`.
`EvalHost` follows the references in its saved constant form rather than rooting
all capabilities. The **tree-walker itself is not removed by this initial
pass**: source closures, internal fallback paths, and shared builtin dispatch
still need a separate dependency split. This is not yet general removal of
every unused Rust builtin. Retained bootstrap functions and macros can contain
source lambdas and keep all native capabilities even when the application entry
does not call `EVAL`; the report identifies this conservative dependency.

Delivery generates a versioned native contract, builds through Cargo in
`target/delivery/` under the source checkout, checks the resulting executable,
and appends the reduced image. Compile-time selection removes references to the
instruction decoder; release LTO and linker garbage collection can then remove
its implementation. The cache separates source versions, targets, capabilities,
Cargo features, compiler flags, and toolchain identities. Existing output files
survive a failed build or compatibility check. A dry run only reports selection
and does not invoke Cargo.

`--runtime-info` prints a runtime's contract. Saved image format 5 includes the
same compatibility information and is checked before heap restoration. Source
content identity, target, Rust toolchain, Cargo features, and compiler flags
must match; the runtime's native capabilities must include the image's required
capabilities. The source fingerprint is a compatibility identifier, not a
cryptographic signature. Full runtimes can still read older images; specialized
runtimes require the new metadata. A reduced driver cannot be used for
`runtime = full` delivery.

The delivery report separates `native-bytes` and `image-bytes`. Compare native
sizes from the same release profile and stripping settings; a debug driver is
not a useful size baseline for a specialized release build. Strip an executable
**before** appending an image: stripping the delivered file may discard its core.
