# §4.2 Macro Expansion

**Scope:** This section specifies EGCL's macro expansion subsystem —
the layer between the reader (§4.1) and IR construction (§4.3). It
covers `macroexpand-1`, `macroexpand`, compiler macros, symbol macros,
the environment protocol, `*macroexpand-hook*`, circular expansion
detection, code-walking for special forms, and the interaction between
macro expansion and the lexical environment.

Source: `crates/egcl-compiler/src/macroexpand.rs`

---

## 4.2.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R4.10 | `macroexpand-1` MUST perform exactly one step of macro expansion on a form, returning `(values expanded-form expanded-p)`. If the form is not a macro call, it MUST return the original form and `NIL`. | MUST |
| R4.11 | `macroexpand` MUST iterate `macroexpand-1` until the result is no longer a macro form. It MUST NOT recurse into sub-forms — that is the code-walker's responsibility. | MUST |
| R4.12 | `define-compiler-macro` MUST associate a compiler macro with a function name. The compiler MUST consult compiler macros before ordinary macros during compilation. If the compiler macro declines (returns the `&whole` form unchanged), the compiler MUST fall through to the ordinary macro, then to function call. | MUST |
| R4.13 | `define-symbol-macro` and `symbol-macrolet` MUST cause symbol macro expansion of symbols in variable position. A symbol macro MUST NOT be expanded when the symbol appears in a position that would be subject to `setq` without an intervening `macroexpand` — `setq` of a symbol macro MUST be converted to `setf` of its expansion. | MUST |
| R4.14 | The environment protocol MUST provide `variable-information`, `function-information`, `declaration-information`, `augment-environment`, `parse-macro`, and `enclose` (CLtL2 §8.5 / SBCL `sb-cltl2` compatible). These MUST return accurate information about bindings visible at macro-expansion time. `parse-macro` and `enclose` MUST be used for correct `macrolet` processing. | MUST |
| R4.15 | `*macroexpand-hook*` MUST be called by `macroexpand-1` to perform the actual expansion. Its default value MUST be `funcall`. User code MAY rebind it to intercept or instrument expansion. | MUST |
| R4.16 | The macro expander MUST detect circular expansion (a form expanding back to itself or to a previously-seen form in the same expansion chain) and signal an error of type `program-error`. | MUST |

---

## 4.2.2 Data Structures

### D4.06 Environment

The `Environment` structure represents the lexical environment at
macro-expansion time. It is threaded through the code-walker and passed
to macro functions as their second argument.

```rust
/// Expansion-time lexical environment.
/// This is NOT the runtime environment — it contains only the static
/// information available during compilation.
#[derive(Clone, Debug)]
pub struct Environment {
    /// Parent environment (lexical chain).
    parent: Option<Arc<Environment>>,

    /// Variable bindings visible in this scope.
    variables: HashMap<Symbol, VarInfo>,

    /// Function/macro bindings visible in this scope.
    functions: HashMap<FunctionName, FnInfo>,

    /// Active declarations (optimize, type, etc.).
    declarations: Vec<DeclInfo>,

    /// Block names in scope (for RETURN-FROM).
    blocks: HashSet<Symbol>,

    /// Tag names in scope (for GO).
    tags: HashSet<Symbol>,
}
```

#### Variable binding info

| Variant | Fields | Meaning |
|---|---|---|
| `Lexical` | `type_decl: Option<TypeSpec>`, `special_p: bool` | Variable from LET, LET*, LAMBDA, etc. |
| `Special` | `type_decl: Option<TypeSpec>` | Dynamic variable |
| `SymbolMacro` | `expansion: Form` | Symbol macro (SYMBOL-MACROLET / DEFINE-SYMBOL-MACRO) |
| `Constant` | `value: EgclVal` | Named constant (DEFCONSTANT) |

#### Function binding info

| Variant | Fields | Meaning |
|---|---|---|
| `Function` | `type_decl: Option<FunctionType>`, `inline_decl: InlinePolicy` | Ordinary function (DEFUN, FLET, LABELS) |
| `Macro` | `expander: MacroFunction` | Macro (DEFMACRO, MACROLET) |
| `SpecialOperator` | — | Special operator (IF, LET, PROGN, etc.) |

`InlinePolicy` is one of `Unspecified`, `Inline`, `NotInline`.

#### Declaration info

`DeclInfo` carries a `DeclKind` and a `DeclScope` (`Local` or `Global`).

| `DeclKind` variant | Data | Meaning |
|---|---|---|
| `Optimize` | `OptimizeQualities` | Optimization settings |
| `Type` | `(Symbol, TypeSpec)` | Type declaration for a variable |
| `Ftype` | `(FunctionName, FunctionType)` | Type declaration for a function |
| `Dynamic` | `Symbol` | SPECIAL declaration |
| `Ignore` / `Ignorable` | `Symbol` | Ignore declarations |
| `Declaration` | `Symbol` | User-defined declaration name |
| `Custom` | `(Symbol, Form)` | Implementation-specific |

#### Compiler macro table

Global `CompilerMacroTable` is a `RwLock<HashMap<FunctionName, MacroFunction>>`.

`MacroFunction` is `Arc<dyn Fn(Form, &Environment) -> Form + Send + Sync>`.
It receives the whole call form and the environment, returning either
a replacement form or the original `&whole` form to decline.

---

## 4.2.3 Algorithm A4.01 — Macro Expansion

### Phase 1: `macroexpand-1(form, env)` — Single-step expansion

```text
FUNCTION macroexpand-1(form, env) → (expanded_form, expanded_p):
  1. If form is a SYMBOL:
     a. Look up form in env.variables.
     b. If found as VarInfo::SymbolMacro { expansion }:
        i.  Call *macroexpand-hook* with:
              hook(macro_fn, form, env)
            where macro_fn = |_form, _env| expansion.clone()
        ii. Return (result, T).
     c. Else: return (form, NIL).

  2. If form is a CONS with CAR being a SYMBOL (operator):
     a. Let operator = CAR(form).
     b. Look up operator in env.functions.
     c. If found as FnInfo::Macro { expander }:
        i.   Call *macroexpand-hook* with:
               hook(expander, form, env)
        ii.  Return (result, T).
     d. If found as FnInfo::SpecialOperator or FnInfo::Function:
        Return (form, NIL).
     e. If NOT found in env, look up in global macro table:
        i.  If global macro exists:
              Call *macroexpand-hook*(expander, form, env).
              Return (result, T).
        ii. Else: return (form, NIL).

  3. Otherwise (form is a non-symbol atom, or a cons with non-symbol
     CAR): return (form, NIL).
```

### Phase 2: `macroexpand(form, env)` — Iterative full expansion

```text
FUNCTION macroexpand(form, env) → (final_form, ever_expanded_p):
  1. Let expansion_history = empty set (for circular detection).
  2. Let ever_expanded = NIL.
  3. Loop:
     a. Add a fingerprint of form to expansion_history.
     b. Let (new_form, expanded_p) = macroexpand-1(form, env).
     c. If expanded_p is NIL: return (form, ever_expanded).
     d. Set form = new_form, ever_expanded = T.
     e. CIRCULAR EXPANSION CHECK (§4.2.5):
        Compute fingerprint of new_form.
        If fingerprint ∈ expansion_history:
          Signal PROGRAM-ERROR with diagnostic:
            "Circular macro expansion detected: ~S" new_form
        End if.
     f. If iteration count > *macroexpand-limit* (default 65536):
          Signal PROGRAM-ERROR:
            "Macro expansion limit exceeded for form: ~S"
        End if.
  4. End loop.
```

**R4.10 compliance:** Step 2.b delegates to `macroexpand-1` for exactly
one expansion step.

**R4.11 compliance:** The loop never recurses into sub-forms.

**R4.16 compliance:** Step 2.e detects circularity via fingerprinting.

### Phase 3: Full code-walk (`expand-form`)

Top-level entry point for the compiler's macro expansion phase.

```text
FUNCTION expand-form(form, env) → expanded_form:
  1. Let (form, _) = macroexpand(form, env).
  2. If form is a self-evaluating atom: return form.
  3. If form is a SYMBOL: return form.  ;; symbol macros already handled
  4. If form is a CONS:
     a. Let operator = CAR(form).
     b. If operator is a special operator:
          Dispatch to special-form handler table (§4.2.7).
     c. If operator is a LAMBDA expression:
          Expand as ((lambda ...) args...).
     d. Otherwise (function call):
          i.  COMPILER MACRO CHECK (§4.2.4):
              If compiler macro exists for operator
              AND notinline is NOT declared for operator in env
              AND the compiler macro does not decline (returns a form
                  not EQ to the &whole argument):
                Return expand-form(cm_result, env).
          ii. Expand each argument via expand-form.
          iii. Return (operator . expanded-args).
  5. Error: malformed form.
```

---

## 4.2.4 Compiler Macros

**R4.12 compliance.**

### Consultation order

Compiler macros apply only to forms that have already been fully
macroexpanded and resolved to function calls — they do NOT compete
with ordinary macros for the same form. The `expand-form` code-walker
(§4.2.3 Phase 3) first calls `macroexpand` to exhaust all ordinary
macro expansion, then classifies the result. Only forms that survive
as function calls (step 4.d) reach the compiler macro check.

Within step 4.d, the order for a function call form
`(f arg1 arg2 ...)` is:

1. **Compiler macro check:** Look up `f` in the `CompilerMacroTable`.
   Skip if `notinline` is declared for `f` in the current environment.
   If found and not skipped, invoke the compiler macro function with
   `(form, env)`.
   - If it returns a form `eq` to the `&whole` argument → **declined**.
     Proceed to step 2.
   - Otherwise → use the returned form; re-enter `expand-form`
     (which will re-run `macroexpand` on the result, so ordinary
     macros in the expansion are handled correctly).
2. **Function call:** Expand arguments and generate a normal call.

### `define-compiler-macro`

`(define-compiler-macro name lambda-list &body body)` — MUST support
`&whole`. The compiler macro MUST be pure (no side effects) and MUST
decline by returning the `&whole` form when it cannot optimize.

### `compiler-macro-function`

`(compiler-macro-function name &optional env) → function-or-nil` —
`setf`-able to install or remove compiler macros.

### Compiler macro vs. `notinline`

If `notinline` is declared for a function, the compiler MUST NOT
consult its compiler macro.

---

## 4.2.5 Circular Expansion Detection

**R4.16 compliance.**

**Fingerprinting:** A structural hash of the form (atoms by identity,
conses by recursive CAR/CDR hash, depth-limited to 8 levels). The
expansion history is a `HashSet<u64>` local to each `macroexpand` call.
False positives yield a catchable `program-error`, never silent
miscompilation.

**Hard iteration limit:** `*macroexpand-limit*` (default 65 536)
bounds iterations per `macroexpand` call, catching non-repeating
divergent expansions (e.g., a macro appending a fresh `gensym`).

---

## 4.2.6 Symbol Macros

**R4.13 compliance.**

### `define-symbol-macro`

```lisp
(define-symbol-macro symbol expansion)
```

- Installs a global symbol macro. The symbol MUST NOT name a
  special variable or a constant at the time of definition
  (signal `program-error` if so).
- The expansion is stored in the global environment as
  `VarInfo::SymbolMacro`.

### `symbol-macrolet`

```lisp
(symbol-macrolet ((symbol1 expansion1) ...) &body body)
```

- Creates a local lexical scope where each symbol is a symbol macro.
- Declarations within the body that apply to the symbols (e.g.,
  `ignore`, `type`) are permitted per ANSI.
- A `special` declaration for a symbol-macrolet name signals
  `program-error`.

### Interaction with `setq`

When the code-walker encounters `(setq sym val)` and `sym` is a
symbol macro expanding to `expansion`:
1. Replace with `(setf expansion (expand-form val env))`.
2. Re-enter `expand-form` on the `setf` form.

This ensures `setq` of a symbol macro transparently becomes a
`setf` of the underlying place, per ANSI 3.1.2.1.1.

### Interaction with binding forms

When a binding form (`let`, `let*`, `lambda`, etc.) introduces a
variable binding, it MUST shadow any symbol macro of the same name.
The code-walker creates a new `Environment` frame with a
`VarInfo::Lexical` or `VarInfo::Special` entry that takes precedence.

---

## 4.2.7 Special-Form Expansion Table

The code-walker dispatches on the operator of a fully-macroexpanded
form. Each entry describes how sub-forms are recursively expanded.

| Special Form | Expansion Rule |
|---|---|
| `block` | Expand body forms. Register block name in env. |
| `catch` | Expand tag form and body forms. |
| `eval-when` | Filter situations; expand body forms if applicable at compile time. |
| `flet` | Expand function bodies in new env; expand body in augmented env (fns NOT visible in own bodies). |
| `function` | If `(function (lambda ...))`, expand lambda body. If `(function name)`, no expansion. |
| `go` | No sub-form expansion (tag is not evaluated). |
| `if` | Expand test, then, and else forms. |
| `labels` | Expand function bodies in augmented env (fns ARE visible in own bodies); expand body. |
| `let` | Process declarations. Expand init-forms in outer env. Expand body in augmented env with new bindings. |
| `let*` | Like `let`, but each init-form is expanded in an env augmented by prior bindings. |
| `load-time-value` | Expand the value form. |
| `locally` | Process declarations. Expand body in augmented env. |
| `macrolet` | Install macro definitions in new env. Expand body in augmented env. Do NOT emit macrolet in output. |
| `multiple-value-call` | Expand function form and values forms. |
| `multiple-value-prog1` | Expand all sub-forms. |
| `progn` | Expand each body form sequentially. |
| `progv` | Expand symbols-form, values-form, and body. |
| `quote` | NO expansion (quoted data is opaque). |
| `return-from` | Expand result form. Validate block name in env. |
| `setq` | For each pair: check symbol-macro (→ convert to setf); else expand value form. |
| `symbol-macrolet` | Install symbol-macro bindings in new env. Expand body. Do NOT emit symbol-macrolet in output. |
| `tagbody` | Register tags in env. Expand non-tag body forms. |
| `the` | Expand the value form. Retain type specifier. |
| `throw` | Expand tag form and result form. |
| `unwind-protect` | Expand protected form and cleanup forms. |

### `macrolet` processing

1. Parse local macro definitions.
2. Compile each expander in an env that includes enclosing macros but
   NOT the current `macrolet`'s own macros (per ANSI 3.2.3.1).
3. Create a new `Environment` frame with `FnInfo::Macro` entries.
4. Expand body in the augmented environment.
5. `macrolet` MUST NOT appear in IR-bound output; replace with
   `(locally ...)` or splice into enclosing `progn`.

### `symbol-macrolet` processing

1. Validate no `special` declaration conflicts.
2. Create `VarInfo::SymbolMacro` entries in a new `Environment`.
3. Expand body; strip `symbol-macrolet` wrapper from output.

---

## 4.2.8 The `*macroexpand-hook*` Protocol

**R4.15 compliance.**

```lisp
;; Default binding
(defvar *macroexpand-hook* #'funcall)
```

`macroexpand-1` invokes the hook as:

```lisp
(funcall *macroexpand-hook* macro-function form env)
```

Where:
- `macro-function` is the expansion function (a `MacroFunction`).
- `form` is the macro call form.
- `env` is the expansion-time environment.

As a special (dynamically-scoped) variable, `*macroexpand-hook*` is
per-thread. Concurrent compilations do not interfere. Use cases:
tracing (log each expansion), sandboxing (reject disallowed macros),
and memoization (cache expansions keyed on form identity).

---

## 4.2.9 Environment Protocol (CLtL2 §8.5)

**R4.14 compliance.**

EGCL exposes the following functions in the `EGCL-CLTL2` package
(also aliased into `SB-CLTL2` for SBCL compatibility):

### `variable-information`

```lisp
(variable-information symbol &optional env)
  → kind, local-p, declarations-alist
```

| `kind` value | Meaning |
|---|---|
| `:lexical` | Lexical variable binding |
| `:special` | Special (dynamic) variable |
| `:symbol-macro` | Symbol macro |
| `:constant` | Named constant |
| `nil` | No binding found |

`local-p` is `T` if the binding is local (not global).

`declarations-alist` contains `(key . value)` pairs:
- `(type . type-spec)` — declared type
- `(ignore . t)` — declared `ignore`
- `(dynamic-extent . t)` — declared `dynamic-extent`

### `function-information`

```lisp
(function-information function-name &optional env)
  → kind, local-p, declarations-alist
```

| `kind` value | Meaning |
|---|---|
| `:function` | Ordinary function |
| `:macro` | Macro |
| `:special-form` | Special operator |
| `nil` | No binding found |

### `declaration-information`

```lisp
(declaration-information decl-name &optional env)
  → value
```

Returns information about the named declaration. Standard queries:

| `decl-name` | Return value |
|---|---|
| `optimize` | List of `(quality value)` — e.g., `((speed 3) (safety 1) (debug 0))` |
| `declaration` | List of declaration names valid in this environment |

### `augment-environment`

```lisp
(augment-environment env &key variable symbol-macro function
                              macro declare) → new-env
```

Creates a new environment augmented with given bindings and declarations.
Keywords: `:variable` (list of symbols), `:symbol-macro` (alist),
`:function` (list of names), `:macro` (alist of name→function),
`:declare` (declaration specifiers).

### `parse-macro`

```lisp
(parse-macro name lambda-list body &optional env) → macro-function
```

Takes a macro name (a symbol), a macro lambda-list, a body (list of
forms), and an optional environment. Returns a macro expander function
(a `MacroFunction`) suitable for use with `macrolet` or
`augment-environment`. The returned function accepts two arguments
`(form env)` and destructures `form` according to `lambda-list`.

This is used internally by `macrolet` processing (§4.2.7) to compile
local macro definitions in the correct lexical environment.

### `enclose`

```lisp
(enclose lambda-expression &optional env) → function
```

Takes a lambda expression and an optional environment and returns a
function object that is the result of closing the lambda expression
over the given environment. This is needed for `macrolet` processing
(§4.2.7) where macro expander functions must capture the enclosing
lexical environment — specifically, the environment that includes
the enclosing macros but not the current `macrolet`'s own definitions
(per ANSI 3.2.3.1). `parse-macro` produces the lambda expression;
`enclose` closes it in the correct environment.

---

## 4.2.10 Interaction with Lexical Environment

### Environment threading

The code-walker constructs a chain of `Environment` frames as it
descends into nested binding forms. Each frame holds only the
bindings introduced by that form; lookup walks the parent chain
(Global → defun → let → macrolet → symbol-macrolet, etc.).

### Shadowing rules

1. A local variable binding shadows a symbol macro of the same name.
2. A local function/macro binding shadows a global function/macro.
3. `macrolet` bindings shadow `defmacro` bindings of the same name.
4. `symbol-macrolet` bindings shadow `define-symbol-macro` bindings.
5. `flet`/`labels` bindings shadow compiler macros for the same name
   in the scope of the body.

### Free declaration processing

Free-variable declarations (e.g., `(declare (special x))` in `locally`
where `x` is unbound) MUST affect the environment without creating a
new binding — recorded as `VarInfo::Special` overlays.

---

## 4.2.11 Error Handling

| Condition | Type | When |
|---|---|---|
| Circular macro expansion detected | `program-error` | Fingerprint collision in `macroexpand` history |
| Macro expansion limit exceeded | `program-error` | Iterations > `*macroexpand-limit*` |
| `symbol-macrolet` of special variable | `program-error` | At `symbol-macrolet` processing time |
| `define-symbol-macro` of special/constant | `program-error` | At `define-symbol-macro` evaluation time |
| Macro function signals an error | (propagated) | During `*macroexpand-hook*` invocation |
| Invalid `macrolet` lambda list | `program-error` | At `macrolet` processing time |
| Undeclared free variable (style) | `style-warning` | When `(safety >= 1)` and an unbound symbol is found |

All conditions MUST include source location (file, line, column) when
available from the reader's source-location table (§4.1).

---

## 4.2.12 Concurrency

- Global macro, compiler macro, and symbol macro tables are each
  protected by independent read-write locks (read for expansion,
  write for definition/removal).
- Per-thread `*macroexpand-hook*` bindings avoid contention.
- `Environment` objects are thread-local (Rust heap, not Lisp heap),
  dropped when top-level form expansion completes.

---

## 4.2.13 Configuration

| Parameter | Default | Description |
|---|---|---|
| `*macroexpand-limit*` | 65 536 | Max `macroexpand-1` iterations per `macroexpand` call |
| `*macroexpand-hook*` | `#'funcall` | Hook function called for each expansion step |
| `*compiler-macro-expand-p*` | `T` | When `NIL`, skip all compiler macro consultation (debug aid) |

---

## 4.2.14 Test Strategy

| Test Category | Coverage Target |
|---|---|
| ANSI compliance (`ansi-test` macroexpand/symbol-macro/compiler-macro sections) | 100% of applicable tests |
| Circular expansion (mutually-recursive and self-expanding macros) | R4.16 |
| `*macroexpand-hook*` (custom hook, verify call count and args) | R4.15 |
| Environment protocol (`variable-information` etc. across nested scopes) | R4.14 |
| Compiler macro consultation order and `notinline` suppression | R4.12 |
| Symbol macro + `setq` → `setf` conversion | R4.13 |
| Shadowing rules (all five cases in §4.2.10) | Structural |
| Concurrency (multi-threaded compilation under ThreadSanitizer) | Safety |
| Global definitions expanded on native workers, including worker-triggered moving GC | Safety |
| Performance (expand 10 000 nested macros in < 200 ms) | Throughput |
