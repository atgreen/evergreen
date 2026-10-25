# Compilation

## Source, bytecode, and native code

EGCL lowers supported Lisp forms to bytecode and executes them in T0. The
runtime can compile hot functions to T1 baseline native code and then T2
optimized code. Forms that are not lowered can use the tree-walking evaluator.
Native backend coverage varies by architecture.

`multiple-value-prog1` is supported in bytecode and baseline native code,
including compiled files. It preserves all values of its first form (including
zero values), evaluates the remaining forms for their effects, and then returns
the saved values. Functions using its multiple-value list operations currently
remain at T1 because T2 does not yet lower those operations.

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

Current compiled files use bytecode version 2.0. Files containing version-1
bytecode must be recompiled from source: those older compilers could erase a
call's identity when lowering bit operations, type predicates, accessors, or
memory fences. Loading them would bypass later function replacements. The
loader rejects these files before running their load forms and does not fall
back to any source stored inside the artifact. Recompile affected libraries
and their callers with `compile-file` or your ASDF build, then rebuild saved
images that contain them.

## Declarations and optimization

Common Lisp declarations describe the program, but accepted declarations are
not a guarantee of a particular machine-code transformation. In particular,
declaring fixnum types is not a promise that every operation is unboxed or that
all runtime checks disappear.

`defstruct` slot `:type` declarations are checked by generated constructors
and setters. A failed setter check signals `type-error` before changing the
slot. For ordinary structures, inherited setters use the actual object's
effective slot type, including a child's narrower `:include` declaration.
List/vector-backed setters check the type declared for that accessor; their
sequence representation does not identify a more specific structure type.
This does not add slot-type enforcement to `defclass`.

T2 uses speculative guards and can reconstruct lower-tier state when a guard
fails. Deoptimization must preserve both completed side effects and the next
Lisp operation to execute. Raising compilation thresholds changes when work is
compiled, not the intended semantics of the source.

### Recursive macroexpansion { #macroexpand-all }

**Function** `(egcl-ext:macroexpand-all form &optional environment)` → expanded form

Recursively expands macros and symbol macros in executable positions of `form`.
Unlike `macroexpand`, it descends into function arguments and bodies. It respects
local macro definitions and lexical variable/function bindings; quoted data,
parameter names, keyword names, and method specializers remain data. Compiler
macros are not expanded.

An omitted or `nil` environment means the global environment, including global
symbol macros. A non-`nil` environment must be a live lexical environment received
by a macro's `&environment` parameter. Its dynamic extent is unchanged by this
function. Invalid environment objects signal `program-error`, and conditions
signalled by expanders propagate to the caller.

```lisp
(defmacro plus-one (x) `(+ ,x 1))
(egcl-ext:macroexpand-all '(list '(plus-one 2) (plus-one 2)))
;; => (LIST '(PLUS-ONE 2) (+ 2 1))
```

`load-time-value` expressions are expanded in the global environment, without
evaluating them. Atomic update forms (`egcl-ext:atomic-incf` and `atomic-decf`)
require specialized place lowering and currently signal `program-error` when
encountered in an executable position. They are never silently returned as
fully expanded forms. Macro-defining forms (`defmacro` and
`define-compiler-macro`) also signal `program-error`: their destructuring lambda
lists do not yet have a complete recursive walker. Ordinary `macrolet` bindings
and calls to existing macros are supported. The exact shape of other expansions
is implementation specific and may change between releases.

### Declaration introspection { #declaration-introspection }

These functions let a macro read declarations in force. They are the
implementation layer beneath `egcl-cltl2:declaration-information`; portability
libraries normally use that instead.

Lexical environments retain only some declaration specifiers: `optimize`,
`inline`, `notinline`, and specifiers whose name has been proclaimed with
`(proclaim '(declaration name))`. Other specifiers are not visible to these
functions. An environment object is valid only during the macro expansion that
received it.

### `egcl-ext:declaration-specifier` { #declaration-specifier }

**Function** `(egcl-ext:declaration-specifier name environment)` → declaration specifier or `nil`

Returns the innermost complete declaration specifier, such as
`(my-declaration x 3)`, whose name is the symbol `name` in the lexical
`environment`. Returns `nil` when there is none or when `environment` is `nil`.
A non-symbol `name` signals `type-error`; an object that is not a live macro
expansion environment signals `program-error`.

```lisp
(proclaim '(declaration units))
(defmacro units-in-force (&environment env)
  `',(egcl-ext:declaration-specifier 'units env))
(locally (declare (units meters))
  (units-in-force))
;; => (UNITS METERS)
```

### `egcl-ext:declaration-specifiers` { #declaration-specifiers }

**Function** `(egcl-ext:declaration-specifiers name environment)` → list

Returns every retained specifier named `name` in `environment`, outermost
first. This is the cumulative counterpart of `declaration-specifier`, for
declarations such as `optimize` whose qualities combine across nested
declarations. Arguments and errors are as for `declaration-specifier`.

### `egcl-ext:proclaimed-declarations` { #proclaimed-declarations }

**Function** `(egcl-ext:proclaimed-declarations)` → list of symbols

Returns the names proclaimed as declarations, in no particular order.

### `egcl-ext:proclaimed-optimize` { #proclaimed-optimize }

**Function** `(egcl-ext:proclaimed-optimize)` → list

Returns the globally proclaimed optimization qualities as a fresh list of
`(quality value)` entries, in the order the qualities were first proclaimed.
A later proclamation of a quality replaces its value. A quality proclaimed
without a value is recorded as `3`. Only qualities that have been proclaimed
appear; the list does not include defaults for the others.

```lisp
(proclaim '(optimize (speed 2) safety))
(egcl-ext:proclaimed-optimize)
;; => ((SPEED 2) (SAFETY 3)) when nothing else has been proclaimed
```

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

`function-invoke-count` reports tiering heat, not a complete lifetime
invocation total. New dispatches stop counting while T2 code for the current
definition is installed, including calls that fall back because of native
depth limits. It
resumes after that code is removed or the function is redefined. For exact
counts of selected functions, use [explicit profiling](profiling.md#profile),
which invalidates compiled linkage and pins those functions to T0.

`function-back-edge-count` reports aggregate loop back-edge counts; its
behavior is unchanged by the invocation-counting policy. Both functions
return `nil` for an unrecognized designator.
`function-osr-count` reports successful native OSR
entries on the current thread for a non-nil function-name symbol; it returns
`nil` for other designators. `deopt-count` is a process-wide count.
These are observations; use them for diagnostics, not application decisions.

A function's installed tier does not identify the tier of an active OSR loop.
For that purpose inspect OSR activity and execution behavior, rather than
concluding from a T0 label that no native code ran.

### Generic-function receiver profile { #generic-receiver-profile }

**Functions**

```lisp
(egcl-ext:generic-receiver-profile-count name)
(egcl-ext:generic-receiver-profile-distinct-count name)
```

Report the receiver profile the tiering engine keeps for the generic function
named by the symbol `name`. Generic dispatch with at least one argument
records the class of the first argument. `generic-receiver-profile-count` returns the
number of recorded dispatches. `generic-receiver-profile-distinct-count`
returns the number of distinct receiver classes currently held in the profile;
the profile holds at most four, so the value saturates at 4 for a megamorphic
generic function. Both return `0` for a generic function with no recorded
dispatches, including one that does not exist.

The profile belongs to the current thread or fiber. Setting
`EGCL_PROFILING_DISABLED=1` before starting the process disables recording, and
both functions then return `0`. Like the other counters, these are diagnostics.

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
