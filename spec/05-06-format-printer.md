# §5.9 FORMAT and Pretty-Printer

**Scope:** The `FORMAT` function, its directive language, the FORMAT compiler,
the XP-based pretty-printer, `*print-circle*` detection, and printer control
variables. Implements ANSI CL §22 (Printer) and §22.3 (FORMAT).

---

## 5.9.1 Requirements

| ID | Req | Level |
|----|-----|-------|
| R5.156 | `FORMAT` MUST accept destination of `nil`, `t`, a stream, or a string with fill-pointer. | MUST |
| R5.157 | All ANSI FORMAT directives (§22.3) MUST be supported with correct colon/at-sign modifier interactions. | MUST |
| R5.158 | `V` and `#` MUST be supported as directive parameters. | MUST |
| R5.159 | `~?` (recursive processing) MUST process a format string from the arg list; `~@?` uses the enclosing arg list. | MUST |
| R5.160 | `~{...~}` (iteration) MUST support all four colon/at variants with correct sublist/remaining-args semantics. | MUST |
| R5.161 | `~[...~]` (conditional) MUST support numeric, boolean (`~:[`), and true-test (`~@[`) variants. | MUST |
| R5.162 | `~<...~>` MUST implement both justification and logical-block modes. | MUST |
| R5.163 | `~/name/` MUST call the named function as `(funcall fn stream arg colon-p at-p &rest params)`. | MUST |
| R5.164 | Constant FORMAT strings SHOULD be compiled to closures at macro-expansion time. | SHOULD |
| R5.165 | FORMAT compiler output MUST be identical to interpreted output for all directives. | MUST |
| R5.166 | Pretty-printer MUST implement XP algorithm: logical blocks, conditional newlines (linear/fill/miser/mandatory), per-block indentation. | MUST |
| R5.167 | `*print-pprint-dispatch*` MUST be a type-specifier-keyed table with numeric priority; higher wins. | MUST |
| R5.168 | Default pprint-dispatch entries MUST exist for lists, `defun`, `let`, `cond`, `do`, `loop`, and standard special forms. | MUST |
| R5.169 | `*print-circle*` MUST detect shared/circular structure via `eq` hash table; label with `#n=`/`#n#`. | MUST |
| R5.170 | `*print-level*`/`*print-length*` MUST truncate with `#` and `...` respectively. | MUST |
| R5.171 | `*print-readably*` MUST signal `print-not-readable` if no re-readable form exists. | MUST |
| R5.172 | `*print-lines*` MUST limit output lines, emitting ` ..` for truncation. | MUST |
| R5.173 | `*print-miser-width*` MUST enable miser mode when available width ≤ threshold. | MUST |
| R5.174 | `*print-right-margin*` MUST control the right margin; `nil` → auto-detect terminal width. | MUST |
| R5.175 | Array printing MUST respect `*print-array*`, `*print-level*`, `*print-length*`, `*print-readably*`. | MUST |
| R5.176 | Hash-table printing: `#<HASH-TABLE :TEST eql :COUNT n>` by default. When `*print-readably*` is true, signal `print-not-readable` (per R5.171), since ANSI CL defines no standard readable syntax for hash tables. See §5.9.15 for a Bliss-extension reader macro that may be provided separately. | MUST |
| R5.177 | Structure printing MUST use print-function/print-object, falling back to `#S(...)`. | MUST |
| R5.178 | CLOS objects MUST dispatch through `print-object`. | MUST |
| R5.179 | Pretty-printer directives `~W`, `~I`, `~:T`, `~_` MUST work within FORMAT. | MUST |
| R5.180 | `FORMAT` MUST be thread-safe; concurrent `FORMAT nil` MUST NOT corrupt shared state. | MUST |

---

## 5.9.2 Data Structures

### D5.25 FormatOp — Compiled Format Directive Node

```rust
enum FormatOp {
    Literal(String),
    Write { colon: bool, at: bool, params: SmallVec<[Param; 4]>, readably: bool },
    Radix { base: Param, colon: bool, at: bool, params: SmallVec<[Param; 4]> },
    Float { kind: FloatKind, colon: bool, at: bool, params: SmallVec<[Param; 5]> },
    Character { colon: bool, at: bool },
    Simple(SimpleKind),
    Tab { colon: bool, at: bool, colnum: Param, colinc: Param },
    GoTo { colon: bool, at: bool, count: Param },
    Recursive { at: bool },
    Plural { colon: bool, at: bool },
    CaseConversion { kind: CaseKind, body: Vec<FormatOp> },
    Conditional { kind: CondKind, clauses: Vec<Vec<FormatOp>>, default: Option<usize> },
    Iteration { colon: bool, at: bool, body: Vec<FormatOp>, max_iter: Param },
    Justify(JustifyOp),
    UpAndOut { colon: bool, params: SmallVec<[Param; 3]> },
    PprintNewline { kind: NewlineKind },
    PprintIndent { colon: bool, n: Param },
    PprintWrite { colon: bool, at: bool },
    UserDispatch { name: String, colon: bool, at: bool, params: SmallVec<[Param; 4]> },
}

enum Param { Literal(i64), CharLiteral(char), V, Hash, Default }
enum CondKind { Numeric, Boolean, TrueTest }
enum FloatKind { Fixed, Exponential, General, Dollars }
enum SimpleKind { Newline(Param), FreshLine(Param), Page(Param), Tilde(Param) }
enum NewlineKind { Linear, Fill, Miser, Mandatory }
enum CaseKind { Downcase, Capitalize, CapitalizeFirst, Upcase }
```

### D5.26 JustifyOp

```rust
struct JustifyOp {
    logical_block: bool,
    segments: Vec<Vec<FormatOp>>,
    mincol: Param, colinc: Param, minpad: Param, padchar: Param,
    prefix: Option<String>, suffix: Option<String>,
    per_line_prefix: bool,
}
```

### D5.27 XpStream — Pretty-Printer Stream

```rust
struct XpStream {
    target: Stream,
    buffer: String,
    queue: VecDeque<QueueEntry>,
    section_stack: Vec<SectionEntry>,
    column: usize,
    line_width: usize,
    line_count: usize,
    max_lines: Option<usize>,
    miser_width: Option<usize>,
}

enum QueueEntry {
    Text { start: usize, end: usize },
    Newline { kind: NewlineKind, section_end: usize },
    Indent { kind: IndentKind, amount: i32 },
    BlockStart { prefix: Option<String>, per_line: bool, section_end: usize },
    BlockEnd { suffix: Option<String> },
}

enum IndentKind { Block, Current }

struct SectionEntry {
    start: usize, indent: usize, miser_mode: bool,
    per_line_prefix: Option<String>,
}
```

### D5.28 PprintDispatchTable

```rust
struct PprintDispatchTable { entries: Vec<DispatchEntry> }
struct DispatchEntry {
    type_specifier: TypeSpecifier,
    priority: f64,
    function: LispFn,
}
```

### D5.29 CircularityDetector

```rust
struct CircularityDetector {
    visits: EqHashMap<BlissVal, u32>,      // pass 1: visit counts
    labels: EqHashMap<BlissVal, usize>,    // pass 2: shared → label id
    next_label: usize,
}
```

---

## 5.9.3 Algorithm A5.09 — FORMAT Directive Parsing

Recursive descent over the format string producing `Vec<FormatOp>`.

```text
PROCEDURE parse_format(s: &str) -> Vec<FormatOp>:
  ops = []; i = 0
  WHILE i < s.len():
    IF s[i] != '~':
      start = i
      WHILE i < s.len() AND s[i] != '~': i += 1
      ops.push(Literal(s[start..i]))
    ELSE:
      i += 1
      params = parse_params(s, &i)
      colon = false; at = false
      WHILE s[i] in {':', '@'}: set flag; i += 1
      ch = upper(s[i]); i += 1
      MATCH ch:
        'A','S' => Write   | 'D','B','O','X','R' => Radix
        'F','E','G','$' => Float | 'C' => Character
        '%','&','|','~' => Simple | 'T' => Tab | '*' => GoTo
        '?' => Recursive | '_' => PprintNewline | 'I' => PprintIndent
        'W' => PprintWrite | '^' => UpAndOut | 'P' => Plural
        '[' => parse_conditional(s, &i, ...)
        '{' => parse_iteration(s, &i, ...)
        '<' => parse_justify_or_block(s, &i, ...)
        '(' => parse_case(s, &i, ...)
        '/' => parse_user_dispatch(s, &i, ...)
  RETURN ops

PROCEDURE parse_params(s, i) -> SmallVec<Param>:
  // Parse comma-separated: integer | 'c (char) | V | # | (omitted → Default)
  // Stop when next char is not a param-start or comma.
```

**Invariant:** No backtracking. Each nesting construct (`[`, `{`, `<`, `(`)
has a unique closing char, so parsing is single-pass with recursion depth
bounded by nesting depth.

---

## 5.9.4 FORMAT Compiler

When `FORMAT` receives a constant string (detectable at macro-expansion time),
the compiler transforms it to a closure:

1. `parse_format(control)` → `Vec<FormatOp>`.
2. Each `FormatOp` compiles to direct function calls (no interpretation at runtime).
3. The `FORMATTER` macro and compiler-macro on `FORMAT` both call this path.

**Constant folding:** When all params are literals (no `V`/`#`), individual
directives are folded (e.g., `~10A` → fixed-width write of width 10).

**Cache:** Non-constant strings are compiled on first use and cached in a
thread-local LRU (capacity: 256, keyed by string `eq`).

---

## 5.9.5 ANSI Directive Reference

### Output

| Directive | Params | Notes |
|-----------|--------|-------|
| `~A` | mincol,colinc,minpad,padchar | Aesthetic. `~:A` prints `()` for nil. `~@A` left-pads. |
| `~S` | (same) | Standard (prin1). Modifiers as `~A`. |
| `~W` | — | Write. `~:W` → pretty. `~@W` → no level/length limit. |
| `~C` | — | Char. `~:C` spells name. `~@C` `#\` syntax. `~:@C` key name. |

### Radix

| Directive | Params | Notes |
|-----------|--------|-------|
| `~D`/`~B`/`~O`/`~X` | mincol,padchar,commachar,comma-interval | `:` inserts commas. `@` forces sign. |
| `~R` | radix,... | No params → English cardinal. `:` ordinal. `@` Roman. `:@` old Roman. |

### Float

| Directive | Params |
|-----------|--------|
| `~F` | w,d,k,overflowchar,padchar |
| `~E` | w,d,e,k,overflowchar,padchar,exponentchar |
| `~G` | w,d,e,k,overflowchar,padchar,exponentchar |
| `~$` | d,n,w,padchar — `~:$` sign before pad, `~@$` always sign |

### Layout and Control

| Directive | Description |
|-----------|-------------|
| `~%` / `~&` / `~\|` / `~~` | Newlines / fresh-line / page / tildes (all accept count param). |
| `~T` | Tab. `~@T` relative. `~:T`/`~:@T` logical-block tab. |
| `~*` | Skip args forward. `~:*` backward. `~@*` absolute goto. |
| `~?` | Recursive FORMAT. `~@?` uses remaining args. |
| `~P` | Plural "s" if ≠ 1. `~:P` backs up one arg. `~@P` "y"/"ies". `~:@P` backs up then "y"/"ies". |
| `~(..~)` | Case: `~(` downcase, `~:(` capitalize, `~@(` cap-first, `~:@(` upcase. |

### Conditional `~[...~]`

- **Numeric** `~[c0~;c1~;...~]`: Select by integer. `~:;` marks default.
- **Boolean** `~:[false~;true~]`: Two clauses, nil/non-nil.
- **True-test** `~@[clause~]`: Execute if non-nil; arg remains available.

### Iteration `~{...~}`

| Variant | Meaning |
|---------|---------|
| `~{body~}` | Arg is a list; each iter consumes elements for body's directives. |
| `~:{body~}` | Arg is list of sublists; one sublist per iteration. |
| `~@{body~}` | Remaining args are the iteration list. |
| `~:@{body~}` | Remaining args are sublists. |

`~n{` limits to `n` iterations. `~^` exits if no more args. Empty body → next arg as control string.

### Justification `~<...~>`

- **Text mode** (no `~:;` as first separator, and no colon modifier on `~<`): segments spaced within mincol. `~@<` right-justify, `~:@<` center. MUST be closed with `~>` (not `~:>`).
- **Logical-block mode**: triggered by `~:<` (colon modifier on the opening directive) OR by `~:;` appearing as the first segment separator. Provides prefix/suffix/body for the pretty-printer. MUST be closed with `~:>` (colon modifier on the closing directive). Using `~>` to close a logical block, or `~:>` to close a text justification, is an error (`format-error`).

### Up-and-Out `~^`

Exits iteration body or suppresses justification segments. `~n^` if n=0; `~n,m^` if n=m; `~n,m,k^` if n≤m≤k (the three parameters are tested for monotonic non-decreasing order per ANSI CL §22.3.9.2). `~:^` exits outer `~:{` iteration.

### User Dispatch `~/name/`

Function designator between slashes, called as `(funcall fn stream arg colon-p at-p &rest params)`.

---

## 5.9.6 Algorithm A5.10 — XP Pretty-Printer

Implements Waters (1992) XP algorithm. Output buffered in `XpStream` (D5.27),
flushed once line-break decisions are finalized.

```text
PROCEDURE pprint_logical_block(xp, prefix, suffix, per_line_p, body_fn):
  enqueue(xp, BlockStart{prefix, per_line_p, section_end: TBD})
  IF prefix: emit_text(xp, prefix)
  section_start = xp.queue.len() - 1
  push_section(xp, column, check_miser(xp))
  body_fn(xp)
  pop_section(xp)
  IF suffix: emit_text(xp, suffix)
  enqueue(xp, BlockEnd{suffix})
  backpatch section_end on BlockStart

PROCEDURE attempt_output(xp):
  WHILE xp.queue not empty:
    MATCH xp.queue.front():
      Text{start, end} → write to target, advance column
      Newline{kind, section_end}:
        fits = section_fits(xp, section_end)
        emit = (kind == Mandatory) OR
               (kind == Linear AND NOT fits) OR
               (kind == Miser AND miser_active AND NOT fits) OR
               (kind == Fill AND (NOT fits OR next_block_doesnt_fit))
        IF emit: output newline + per-line-prefix + indentation
          IF *print-lines* exceeded: output " ..", abort
      Indent{kind, amount} → update block indentation
      BlockStart{prefix, per_line, section_end}:
        push_section(xp, xp.column, check_miser(xp), per_line prefix)
        IF prefix: write prefix to target, advance column
      BlockEnd{suffix}:
        IF suffix: write suffix to target, advance column
        pop_section(xp)

FUNCTION section_fits(xp, section_end) -> bool:
  // Sum text widths in queue[0..section_end]; return true if ≤ line_width.
  // Mandatory newlines don't count against fit.
```

**Miser mode (R5.173):** Active when `line_width - block.indent ≤ *print-miser-width*`.

---

## 5.9.7 Pprint-Dispatch Table

### Dispatch Algorithm

```text
PROCEDURE pprint_dispatch(object, table) -> function:
  best = nil; best_priority = -∞
  FOR entry IN table.entries:
    IF typep(object, entry.type_specifier):
      IF entry.priority > best_priority:
        best = entry
      ELSE IF entry.priority == best_priority:
        // ANSI CL §22.2.1.4: behavior is unspecified when priorities are equal.
        // Bliss extension: prefer the more specific type via subtypep.
        // When neither type is a subtype of the other, the most recently
        // added entry wins (stable-order tiebreak).
        IF subtypep(entry.type, best.type) AND NOT subtypep(best.type, entry.type):
          best = entry
  RETURN best.function OR default_print_function
```

> **Note (Bliss extension):** The `subtypep` tiebreaker when priorities are
> equal is a Bliss design choice. ANSI CL §22.2.1.4 leaves this behavior
> unspecified (implementation-dependent). When neither type is a subtype of the
> other, Bliss uses insertion order as the final tiebreak.

### Default Entries (R5.168)

| Type Specifier | Pri | Style |
|----------------|-----|-------|
| `cons` | 0 | Function-call: `(fn arg1 arg2 ...)` |
| `(cons (member defun defmacro))` | 10 | Name + lambda-list, body indented 2 |
| `(cons (member let let* flet labels))` | 10 | Bindings + body indented 2 |
| `(cons (member cond))` | 10 | Each clause on own line |
| `(cons (member do do*))` | 10 | Var-bindings, end-test, body |
| `(cons (member loop))` | 10 | LOOP keywords top-level, clauses indented |
| `(cons (member if when unless))` | 10 | Test first, branches indented 2 |
| `(cons (member progn block tagbody))` | 10 | Each form on own line |
| `(cons (member setq setf psetq psetf))` | 10 | Pair-wise indentation |
| `vector` | -1 | `#(e0 e1 ...)` with fill newlines |
| `array` | -1 | Nested `#nA(...)` with level indentation |
| `hash-table` | -2 | `#<HASH-TABLE :TEST eql :COUNT n>` |

---

## 5.9.8 `*print-circle*` Detection (D5.29)

### Two-Pass Labelling Algorithm

```text
// Pass 1: Walk object graph, count visits per eq-identity.
PROCEDURE walk(det, object):
  IF immediate(object): RETURN
  IF det.visits[object] exists: det.visits[object] += 1; RETURN
  det.visits.insert(object, 1)
  recurse into sub-components:
    - cons: car, cdr
    - vector: each element
    - array: each element (row-major order)
    - structure: each slot value (via structure-class slot definitions)
    - CLOS object: each bound slot value (via class-slots / slot-value; R5.178)
    - hash-table: each key and value

// Pass 2: Assign labels to objects with visit count > 1.
FOR (obj, count) IN det.visits WHERE count > 1:
  det.labels[obj] = det.next_label++

// Printing: on first encounter of labelled obj, emit #n= then body.
// On subsequent encounters, emit #n#.
```

---

## 5.9.9 Printer Control Variable Interactions

| Variable | Default | `*print-readably*` forces | Effect |
|----------|---------|--------------------------|--------|
| `*print-escape*` | T | → T | Backslash / `#\` in output |
| `*print-readably*` | NIL | (master) | Re-readable output or signal error |
| `*print-pretty*` | NIL | — | Enable XP pretty-printer |
| `*print-circle*` | NIL | → T | Circularity detection (Bliss forces T under `*print-readably*` to guarantee re-readable output for circular structures) |
| `*print-level*` | NIL | → NIL | Depth truncation (`#`) |
| `*print-length*` | NIL | → NIL | Length truncation (`...`) |
| `*print-lines*` | NIL | — | Line count limit |
| `*print-miser-width*` | NIL | — | Miser threshold |
| `*print-right-margin*` | NIL | — | Right margin |
| `*print-base*` | 10 | → 10 | Integer radix |
| `*print-radix*` | NIL | → T | Radix prefix |
| `*print-case*` | :UPCASE | — | Symbol case |
| `*print-gensym*` | T | → T | `#:` for uninterned |
| `*print-array*` | T | → T | Array contents |
| `*print-pprint-dispatch*` | (std) | — | Dispatch table |

---

## 5.9.10 Level/Length Truncation (R5.170)

```text
PROCEDURE print_list(stream, list, level):
  IF *print-level* AND level > *print-level*: write '#'; RETURN
  write '('
  count = 0
  WHILE cons(list):
    IF *print-length* AND count >= *print-length*: write "..."; BREAK
    IF count > 0: write ' '
    print_object(stream, car(list), level + 1)
    list = cdr(list); count += 1
  IF list != nil: write " . "; print_object(stream, list, level + 1)
  write ')'
```

---

## 5.9.11 Error Handling

| Condition | Type | When |
|-----------|------|------|
| Unknown directive | `format-error` | Parsing |
| Mismatched close (`~]`,`~}`,`~>`,`~)`) | `format-error` | Parsing |
| Wrong param count | `format-error` | Parsing |
| Insufficient arguments | `format-error` | Execution |
| Not printable readably | `print-not-readable` | `*print-readably*` T, no readable syntax |

All `format-error` conditions MUST include the original format string and
approximate character position.

---

## 5.9.12 Concurrency (R5.180)

- `FORMAT nil` → thread-local string stream; no shared state.
- `FORMAT` to a shared stream synchronizes via stream lock (§5.5).
- `*print-pprint-dispatch*` is immutable once installed; mutation copies via `copy-pprint-dispatch`. Concurrent reads are lock-free.
- FORMAT string cache is thread-local (no locking).
- Circularity detector allocated per-call on the stack (no sharing).

---

## 5.9.13 Configuration

| Parameter | Default | Env Override | Description |
|-----------|---------|-------------|-------------|
| FORMAT cache capacity | 256 | `BLISS_FMT_CACHE_SIZE` | Thread-local LRU size |
| Default right margin | 80 | `BLISS_PRINT_RIGHT_MARGIN` | Fallback when terminal undetectable |
| Max circularity walk depth | 100000 | `BLISS_PRINT_CIRCLE_DEPTH` | Stack guard for pathological structures |

---

## 5.9.15 Bliss Extension: Hash-Table Readable Syntax (Future)

ANSI CL defines no standard readable syntax for hash tables. Bliss MAY provide
an optional reader macro (e.g., `#H((:test eql) (k1 v1) (k2 v2))`) as a
non-standard extension to enable readable hash-table output. Until such a macro
is defined and documented, `*print-readably*` with a hash-table argument MUST
signal `print-not-readable` per R5.171. If a readable hash-table syntax is
adopted, R5.176 will be updated accordingly and the reader macro will be
documented in the reader specification (§5.3).

---

## 5.9.16 Test Strategy

| Area | Method |
|------|--------|
| Directive parsing | Unit tests for every directive, modifier combo, parameter variant; malformed string errors. |
| FORMAT compiler | Property tests: compiled vs interpreted output must be identical for random format+args. |
| Pretty-printer | Golden-file tests at various margin widths for `defun`, `let`, `cond`, etc. |
| `*print-circle*` | Circular cons, shared sublists, circular vectors, self-referencing structs. Deterministic labels. |
| Level/length | Nested lists at various `*print-level*` and `*print-length*` settings. |
| Thread safety | Concurrent FORMAT-to-nil; concurrent pretty-printing to separate streams. |
| ANSI compliance | `ansi-test` §22 — all tests MUST pass. |
| Edge cases | Empty format string, `~{~}` with empty list, deeply nested conditionals, `~?` recursive, zero-length justification. |
