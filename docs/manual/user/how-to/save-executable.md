# Save an executable

Use a saved executable to ship a loaded Lisp application. Start with the EGCL
runtime for the machine that will run the application. For another target,
follow [Build for another platform](cross-build.md).

Create `build.lisp`:

```lisp
(defun main ()
  (format t "Hello from EGCL!~%"))

(save-lisp-and-die "hello" :executable t :toplevel #'main)
```

Run:

```sh
egcl --no-init --load build.lisp
./hello
```

Saving terminates the build process. Running `hello` invokes `main` and prints
`Hello from EGCL!`. Load your own application and its libraries before calling
`save-lisp-and-die`.

## Save a development session

To restart into a REPL with your definitions available, omit `:toplevel`:

```lisp
(defun square (x) (* x x))
(save-lisp-and-die "session" :executable t)
```

After the saving process exits, run `./session` and evaluate `(square 9)`.
The result is `81`.

## Carry files in the image

A library such as local-time reads host files (`/usr/share/zoneinfo/...`)
through Lisp streams, and an image cross-built or copied to another host lands
where those files do not exist. `egcl-ext:embed-file` reads a file on the build
host, now, and carries its bytes in the image under the path the application
will open:

```lisp
(egcl-ext:embed-file "/usr/share/zoneinfo/UTC")                     ; same path on both hosts
(egcl-ext:embed-file "/etc/my-app/config.sexp" "deploy/config.sexp") ; a different source
(egcl-ext:embedded-file-p "/usr/share/zoneinfo/UTC")               ; => T
(egcl-ext:embedded-files)                                           ; => the paths
```

`open` for input, `probe-file`, `truename`, `file-length`, `file-write-date`,
`directory` and `load` find embedded files; an embedded file shadows a real
one at the same path, so behaviour does not depend on the host; and they are
read-only, so opening one for output, renaming or deleting it signals
`file-error`. Only the Lisp image sees them: the operating system, foreign code
called through the FFI and child processes do not. Saved images, shaken
executables and cores carry them; contents cost one byte per octet.

An application can list the files it needs in its own `.asd` with the
`egcl-embed-asdf` extension, as `(:embedded-file ...)` and `(:embedded-tree
... :only ...)` components that embed when the system loads; see
`lib/egcl-embed/README.md`.

## Save a separate core file

```lisp
(save-lisp-and-die "session.core")
```

Restart it with a compatible runtime:

```sh
egcl --image session.core
```

Use `--image` for a core file, not `load`. See the
[image reference](../reference/images.md) for format and compatibility limits.

## Shake an application from a saved image

The shaker takes a saved core and a separate specification. It restores the core
in its own process, selects an entry point, removes unreachable named
functions in packages you explicitly select, and writes a standalone executable.
The input core is unchanged. Its old entry point and your init file are not run.

For example, save this application as `app.core`:

```lisp
(defpackage :my-app (:use :cl))
(in-package :my-app)
(defun greeting () "Hello from a shaken image!")
(defun unused-helper () "Development only")
(defun main () (format t "~A~%" (greeting)))
(save-lisp-and-die "app.core")
```

Create `app.shake`:

```text
version = 1
entry = MY-APP::MAIN
prune-package = MY-APP
dynamic = explicit
```

Inspect the retention report, then shake:

```sh
egcl --image app.core --shake app.shake --output hello --dry-run
egcl --image app.core --shake app.shake --output hello
./hello
```

The dry run writes no output files. Shaking writes `hello` and a versioned
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
| `prune-package = *` | Make all named definitions eligible, including bootstrap helpers without a package |
| `keep = PACKAGE::FUNCTION` | Additional root, such as a dynamically selected callback; repeat as needed |
| `dynamic = preserve` | Default: retain all candidate functions to preserve unknown dynamic targets |
| `dynamic = explicit` | Opt in to pruning; declare every additional dynamic entry with `keep` |
| `runtime = full` | Default: reuse the full runtime that runs the shake, without invoking Cargo |
| `runtime = specialized` | Build a matching release runtime with only the required optional native capabilities |
| `max-tier = t2` | Default: include both native compilers; `t1` omits T2 and `t0` omits both, requiring `runtime = specialized` |
| `runtime-keep = disassembly` | Additional native capability root; supported names are `disassembly`, `dynamic-code`, and `tree-walker` |

Use `dynamic = explicit` only when the root list describes your application.
For example, `(funcall (intern command "MY-APP"))` can name functions the analyzer
cannot infer from the saved data. Add a `keep` line for each permitted target,
or leave `dynamic = preserve`. Reachable arbitrary-code entry points such as
`EVAL` and `LOAD` retain all candidate functions and native capabilities even
with `dynamic = explicit`: the retention policy cannot override that dependency.
Plugins, foreign callbacks, and method redefinition also need explicit roots
when their targets cannot be inferred. This is a retention policy, not a security
boundary.

### Describe the shake in a system definition

An application can carry its shake policy in its own `.asd` and be
shaken by `asdf:make`, with no spec file to write and no core to save by
hand. Name `egcl-shake-asdf` in `:defsystem-depends-on` and give a secondary
system the shake class and build operation; the initargs mirror the
specification keys above one for one:

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
slots, saves `build/my-app.core` in a child process, shakes it in another,
and leaves the executable and `build/my-app.manifest` beside them. The
remaining initargs are `:shake-keep`, `:shake-runtime` (`:full` or
`:specialized`), `:shake-max-tier`, `:shake-runtime-keep`,
`:shake-runtime-source`, `:shake-system`, and `:shake-output`; ASDF's
own `:entry-point` stands in for `:shake-entry`. The extension is installed
with `egcl` under `/usr/share/common-lisp/source/`, where ASDF already looks;
from a source checkout, add `lib/egcl-shake/` to `CL_SOURCE_REGISTRY`. See
`lib/egcl-shake/README.md` for the child processes' registry.

The current pass retains all global data, symbol identities, packages,
classes, and functions outside the selected packages. It follows
references through saved data and source/bytecode, including nested functions
and captured environments. Conservatively retained registries can keep extra
functions alive. Runtime packages such as `COMMON-LISP` and `EGCL-INTERNAL`
can be selected explicitly; `KEYWORD` cannot. Installed printing, instance
initialization, and Gray stream protocols remain reachable through the
runtime's implicit calls, even when application code does not name them.

For the broadest candidate set, use `prune-package = *`. The default
`dynamic = preserve` still retains possible dynamic targets; combine the wildcard
with `dynamic = explicit` after declaring any additional entry points with
`keep`. A bootstrap helper that has no home package can be retained by its exact
reported registry name, such as `keep = %GCD2`. Definitions with a home package
still require `PACKAGE::NAME` in `keep`.

Generic functions in selected packages are candidates too. Retaining a generic
retains its methods, and a reachable saved method handle retains its owning
generic and method set. Class accessor names also retain their functions.
Unreachable generic functions and their owned method records are removed from
both evaluator and CLOS registries before saving. A generic can be an `entry`
or an explicit `keep` root.

For methods with saved bytecode, the shaker discards the redundant source body
and follows the compiled callable. Macros used only to compile such a method
do not stay alive through its old source. Uncompiled methods retain their
source and its dependencies.

Source and bytecode macros in selected packages also follow reachable names.
Saved macro expanders retain the definitions they invoke, and `keep` may name
a macro needed through a dynamically constructed name. An unreachable macro's
body does not retain its callees or native capabilities. Macros cannot serve
as the executable's `entry`. Compiler registrations and frozen captures with a
known macro owner follow that macro's reachability. Unreachable registrations
are released before saving. SETF expanders, compiler macros, and registrations
without a known owner remain conservative roots.

User-defined SETF writers in selected packages follow the reachability of their
accessor names and saved writer functions. Shaking removes an unreachable
writer's private function cell and source record. Use `keep = PACKAGE::ACCESSOR`
to retain a dynamically invoked writer, even if the accessor has no getter.
New BFASLs preserve the accessor symbol so the shaker can identify its owning
package. Older artifacts with missing or ambiguous ownership remain
conservatively retained.

Private compiled closures and source closure handles are traced from reachable
objects and code, including their captured environments. An unreachable
closure does not retain its callees or native capabilities just because it
remains in a runtime registry. Shaking removes its private code and captures
before saving; `private-code-removed` reports removed compiled entries and
`source-closures-removed` reports removed source closure entries.
Escaped closures and shared captured environments remain live when reachable.

Pruning removes function bindings and their owned registry records, then saves
the remaining reachable objects into a new image. The serializer omits dead
objects even from pinned regions, rather than leaving holes for their bytes.
Saved object identities let the loader rebuild pointers with an old-to-new
address map. Symbol identities and other retained data can therefore survive
after a function's implementation is removed. The original input file is never
modified.

With the default `runtime = full`, the shaker reduces the saved image and keeps
the full Rust runtime. Re-saving an executable replaces its embedded image
instead of stacking another copy of the previous core into the runtime prefix.

## Specialize the native runtime

Add this to the same shake specification:

```text
runtime = specialized
```

Then shake with a matching source checkout:

```sh
egcl --image app.core --shake app.shake --output hello \
  --runtime-source /path/to/egcl
```

The source checkout defaults to the location recorded when the driver was built.
You need Cargo, the matching Rust toolchain, its target libraries, and the target
linker. The new runtime must execute on the machine running the shake so its compatibility
contract can be checked; this command does not shake images for another platform.

Native capabilities are selected by the reachability graph. A retained
`DISASSEMBLE` reference keeps disassembly. A retained `EVAL`, `COMPILE`, `LOAD`,
`COMPILE-FILE`, `REQUIRE`, or reader operation supporting `#.` keeps
`dynamic-code`, which in turn keeps **every** native capability and candidate
Lisp function. The report names these roots. `dynamic = preserve` retains all
capabilities; explicit `runtime-keep` entries add roots, never remove required
ones.

When `dynamic-code` is unreachable, the specialized runtime omits those public
source-evaluation entry points and rejects `--eval`, `--load`, scripts, and the
REPL. It also skips init files. Validated saved cores
remain supported. Saved raw lambda lists, including constant-pool and global
data, retain `tree-walker` and the definitions referenced by their bodies.
A fixed lambda does not by itself retain every function or native capability;
an `EVAL` inside its body does. Applications that construct arbitrary lambda
bodies dynamically must retain `dynamic-code`.

The GC and T0 bytecode interpreter remain available. A shake includes both
native compilers by default. With `runtime = specialized`, choose the highest
included tier using `max-tier = t2` (default), `max-tier = t1` (omit the T2
optimizing compiler), or `max-tier = t0` (omit both native compilers).
The omitted compiler entry points are removed at build time so native linking
can remove their implementation. T0-only applications continue executing saved
bytecode. Native promotion and OSR cannot exceed the selected tier;
`EGCL_FORCE_TIER` requests above it are clamped to the available tier.

For example, a specification that selects the whole saved world and omits both
native compilers is:

```text
version = 1
entry = MY-APP::MAIN
prune-package = *
dynamic = explicit
runtime = specialized
max-tier = t0
```

Add `keep` entries for dynamically selected callbacks. Tier selection is
independent of source evaluation: reachable `EVAL` still retains its evaluator
and builtin dependencies even with `max-tier = t0`.

Deoptimization resumes T0 bytecode; it does not require public `EVAL`.
`EvalHost` follows the references in its saved constant form rather than rooting
all capabilities. Many library builtins share an evaluated-argument dispatcher
between source and compiled calls, avoiding the construction and evaluation of
temporary Lisp call forms. The separate `tree-walker` capability is omitted
when retained functions have restorable bytecode and their reachable operations
do not require source evaluation. Native linking then removes the tree-walking
operator dispatcher and source-to-bytecode compiler while retaining T0 bytecode
and any selected native compilation tiers.

A specialized shake attempts to compile reachable named source functions that
have no captured lexical environment. It discards their source only after
confirming that the bytecode can be saved and restored independently, then
recomputes reachability. This also runs during `--dry-run`: compilation may
invoke macro expanders, but the shaker does not call the application entry point.
The input image is unchanged.

Unsupported source functions and closures, source handler forms, variadic
argument binders, uncompiled generic methods, and builtin paths that still
require the evaluator retain `tree-walker`. The report identifies these
dependencies. The initial audited builtin set covers arithmetic, basic list operations, multiple values,
function application, and `WRITE-LINE`; other builtin paths conservatively
retain the walker. BFASL input already provides compiled function bodies;
supported cold source definitions can now be compiled during the shake too.
Simply omitting `EVAL` is insufficient if other reachable operations still
require source evaluation.

Bootstrap definitions outside the selected packages are retained. Selecting
their packages allows unreachable library functions and macros to be removed
from an ordinary bootstrapped image. Conservative registry dependencies can
still retain all native capabilities even when its application
entry point comes from BFASL. The source-free native-removal tests use images
built with `--no-bootstrap`.

When the walker is unnecessary, the shaker also selects native builtin dispatch
arms from the reachable symbols. Unselected arms are removed before Rust code
generation, allowing LTO and linker garbage collection to discard their helpers
and library implementations. Aliases sharing an arm retain that implementation
together. Builtin slot numbers remain stable across runtimes. Images requiring
the walker currently retain the full builtin dispatcher until its implicit
calls are represented in the dependency graph. Reachable dynamic-code
operations always retain every builtin.

The shaker generates a versioned native contract, builds through Cargo in
`target/shake/` under the source checkout, checks the resulting executable,
and appends the reduced image. Compile-time selection removes references to the
instruction decoder; release LTO and linker garbage collection can then remove
its implementation. The cache separates source versions, targets, capabilities,
builtin selections, maximum tiers, Cargo features, compiler flags, and toolchain
identities. Existing output files survive a failed build or compatibility check.
A dry run reports selection without invoking Cargo or writing an executable;
source preparation and macro expansion can still run as described above.

`--runtime-info` prints a runtime's contract. Saved image format 5 includes the
same compatibility information and is checked before heap restoration. Source
content identity, target, Rust toolchain, Cargo features, and compiler flags
must match; the runtime's native capabilities must include the image's required
capabilities, builtin set, and compiler tiers. Contract schema 3 records builtin
names as UTF-8 hex strings; `builtins=*` denotes the complete set. The source
fingerprint is a compatibility identifier, not a cryptographic signature.
Full runtimes can still read older images; specialized runtimes require the new
metadata. A reduced driver cannot be used for
`runtime = full` shake.

The shake report separates `native-bytes` and `image-bytes`. Compare native
sizes from the same release profile and stripping settings; a debug driver is
not a useful size baseline for a specialized release build. Strip an executable
**before** appending an image: stripping the shaken file may discard its core.

### Measured size example

At commit `c25f245f`, the source-free native shake regression on x86-64 Linux
musl produced the following sizes. All three builds used Rust 1.94.1, the same
release profile (Thin LTO), the same application, and no additional stripping.
The application exercises arithmetic, loops, output, and condition handlers;
only `+`, `<`, `=`, `FUNCALL`, and `WRITE-LINE` remained in its builtin selection.

| Highest included tier | Native runtime bytes | Image bytes | Complete executable bytes |
| --- | ---: | ---: | ---: |
| T2 (default) | 6,302,160 | 246,167 | 6,548,343 |
| T1 | 5,128,024 | 246,167 | 5,374,207 |
| T0 | 4,947,864 | 246,167 | 5,194,047 |

The executable totals include the 16-byte image trailer. These are a small
fixture's measurements, not a promised size for other applications. The tests
check omitted compiler and builtin symbols and execute with automatic tiering,
forced-tier requests, and low promotion/OSR thresholds. Ordinary bootstrap
images can retain more code through the conservative dependencies described
above; all global data and symbol/package identities remain roots.
