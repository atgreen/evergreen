# Compilation

## Source, bytecode, and native code

EGCL lowers supported Lisp forms to bytecode and executes them in T0. The
runtime can compile hot functions to T1 baseline native code and then T2
optimized code. Forms that are not lowered can use the tree-walking evaluator.
Native backend coverage varies by architecture.

Compilation is therefore not a single event. Reading and evaluating a `defun`,
writing a compiled file, and optimizing a hot loop are distinct operations.
[Execution tiers](user/explanation/execution.md) describes the shared model;
the dictionary below specifies the user-visible operations.

## Function compilation

### `compile` { #compile }

**Function** `(compile name &optional definition)` → result, warnings-p, failure-p

With a definition, accepts a function or a lambda expression and produces a
callable function. A non-`nil` name installs the function in that symbol's
function cell and is returned as the primary value. With `nil` as the name,
the primary value is the function itself. Without a definition, uses the
existing function named by `name`.

The current implementation returns `nil` for the warning and failure values
on success. It does not force installation of a T2 version.

```lisp
(let ((f (compile nil '(lambda (x) (+ x 1)))))
  (funcall f 41))
;; => 42
```

## File compilation

### `compile-file` { #compile-file }

**Function** `(compile-file input-file &key output-file ...)`
→ output-truename, warnings-p, failure-p

Writes a compiled-file artifact. For source names ending in `.lisp` or `.lsp`,
the default output pathname replaces that suffix with `.fasl`. The contents use
EGCL's BFASL format; the filename suffix is not the format identifier.

Use `:output-file` to choose the destination. The current implementation also
accepts a positional output pathname as an extension, but portable code should
use the keyword. An odd or malformed keyword tail signals a program error.
Acceptance of additional compiler keywords does not establish full support for
their ANSI-specified effects.

Top-level `defvar` and `defparameter` forms proclaim their names special during
compilation, but ordinary initializers wait until the compiled file is loaded.
`defvar` still leaves an existing binding untouched. An explicit compile-time
`eval-when` evaluates its body in source order, including any initialization
it requests.

```lisp
(multiple-value-bind (output warnings-p failure-p)
    (compile-file "example.lisp" :output-file "example.fasl")
  (declare (ignore warnings-p))
  (unless failure-p (load output)))
```

### `compile-file-pathname` { #compile-file-pathname }

**Function** `(compile-file-pathname input-file &key output-file ...)` → pathname

Computes the compiled output pathname without compiling. Use it rather than
constructing a `.bfasl` name yourself. A saved core, even if renamed `.fasl`, is
not a compiled file and must not be passed to `load`.

ASDF uses compilation and loading to build systems. The
[ASDF guide](user/how-to/asdf.md) gives a minimal system with explicit source
registration. Compiled files should be rebuilt when changing incompatible
runtime versions; they are not a promise of indefinite binary compatibility.

## Declarations and optimization

Common Lisp declarations describe the program, but accepted declarations are
not a guarantee of a particular machine-code transformation. In particular,
declaring fixnum types is not a promise that every operation is unboxed or that
all runtime checks disappear.

T2 uses speculative guards and can reconstruct lower-tier state when a guard
fails. Deoptimization must preserve both completed side effects and the next
Lisp operation to execute. Raising compilation thresholds changes when work is
compiled, not the intended semantics of the source.

## Observing compilation

### `egcl-ext:function-tier` { #function-tier }

**Function** `(egcl-ext:function-tier function)` → integer or `nil`

Accepts a function designator. Returns `0`, `1`, or `2` for the installed tier,
or `nil` if the designator is not recognized as a tiered function. Polls for
completed background compilation before reading the tier.

### Invocation and loop counters { #counters }

**Functions**

```lisp
(egcl-ext:function-invoke-count function)
(egcl-ext:function-back-edge-count function)
(egcl-ext:function-osr-count function)
(egcl-ext:deopt-count)
```

The first two report invocation and back-edge counts, or `nil` for an
unrecognized designator. `function-osr-count` reports successful native OSR
entries on the current thread for a non-nil function-name symbol; it returns
`nil` for other designators. `deopt-count` is a process-wide count.
These are observations; use them for diagnostics, not application decisions.

A function's installed tier does not identify the tier of an active OSR loop.
For that purpose inspect OSR activity and execution behavior, rather than
concluding from a T0 label that no native code ran.

### `egcl-ext:bail-report` { #bail-report }

**Function** `(egcl-ext:bail-report)` → number of distinct reasons

Prints collected bytecode-lowering decline reasons and counts. Enable collection
by starting the process with `EGCL_BAIL_TRACE=1`. With collection disabled and
no recorded failures, there is nothing to report.

## Experimental compiler controls

Set these variables before starting the process. They are debugging controls,
not a portable Common Lisp interface, and many are read once.

| Variable | Use |
| --- | --- |
| `EGCL_BACKEND=tree-walker` | Compare against the tree-walking execution path |
| `EGCL_LAZY_COMPILE=0` | Request eager bytecode compilation rather than lazy compilation |
| `EGCL_T1_THRESHOLD` | Override the T1 invocation threshold |
| `EGCL_T2_THRESHOLD` | Compatibility override for the T2 invocation threshold |
| `EGCL_OSR_THRESHOLD` | Override named-loop OSR threshold; default 100,000 back edges |
| `EGCL_BAIL_TRACE=1` | Collect bytecode-lowering decline reasons |

A threshold does not expand the target backend's supported instruction set.
See [Platform support](user/reference/platforms.md) and
[Profiling and efficiency](profiling.md).

Implementation reference: [CLI compiler integration](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl/src/cli/bytecode.rs).
