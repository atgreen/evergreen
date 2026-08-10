# §4.1 CL Reader

**Scope:** The Bliss reader converts a stream of characters into Lisp
objects. It implements the full CLHS §2 reader algorithm including
readtable dispatch, reader macros, number parsing, package-qualified
symbols, circular structure notation, and source-position tracking.
The bootstrap reader is written in Rust (`crates/bliss-compiler/src/reader.rs`);
a self-hosted CL reader MAY replace it in Phase 3 (§0, 4.5).

---

## 4.1.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R4.01 | Implement the full 10-step reader algorithm per CLHS §2.2. | MUST |
| R4.02 | Support all standard character syntax types: *whitespace*, *constituent*, *single-escape*, *multiple-escape*, *terminating macro char*, *non-terminating macro char*. | MUST |
| R4.03 | Provide all standard reader macros: `(`, `)`, `'`, `` ` ``, `,`, `"`, `;`, and the dispatch macro `#`. | MUST |
| R4.04 | Parse integers, ratios, floats (incl. exponent markers `S`, `F`, `D`, `L`, `E`), and `#C(...)` complex numbers. | MUST |
| R4.05 | Resolve package-qualified symbols: `pkg:sym`, `pkg::sym`, `#:sym` (uninterned), and `keyword:sym` / `:sym`. | MUST |
| R4.06 | Implement the full `#`-dispatch macro table per CLHS §2.4.8. | MUST |
| R4.07 | Support circular structure via `#n=` / `#n#` with correct back-patching. | MUST |
| R4.08 | Track source positions (file, line, column, byte offset) for every object read, attaching them as object metadata for use by the compiler (§4.2) and debugger (§6). | MUST |
| R4.09 | Signal CL-style `READER-ERROR` conditions (subtype of `STREAM-ERROR`) with position information on malformed input. | MUST |

---

## 4.1.2 Data Structures

### D4.05 Readtable Layout

A readtable maps each character (Unicode codepoint) to its syntax type
and, for macro characters, to a handler function.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SyntaxType {
    Constituent, Whitespace, TerminatingMacro,
    NonTerminatingMacro, SingleEscape, MultipleEscape, Invalid,
}

#[derive(Clone)]
pub struct CharEntry {
    pub syntax: SyntaxType,
    pub macro_fn: Option<BlissVal>,         // callable, or None
    pub dispatch_table: Option<Box<DispatchTable>>,  // dispatch macros only
}

/// Dispatch sub-table: sub-char → handler.  HashMap for Unicode support.
pub struct DispatchTable { pub entries: HashMap<char, BlissVal> }

pub struct Readtable {
    ascii: [CharEntry; 128],                // fast-path (0..128)
    extended: HashMap<char, CharEntry>,     // non-ASCII
    pub case: ReadtableCase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadtableCase { Upcase, Downcase, Preserve, Invert }
```

**Invariants:**

- Every codepoint not in `ascii` or `extended` defaults to
  `SyntaxType::Constituent` with no macro function.
- Exactly one readtable is "current" per dynamic scope, bound to
  `*READTABLE*`.
- `ReadtableCase::Upcase` is the default per CLHS §23.1.2.

**Standard ASCII Syntax Map (initial readtable):**

| Char(s) | Syntax Type | Macro? |
|---------|-------------|--------|
| Tab, Newline, Linefeed, Page, Return, Space | Whitespace | — |
| `(` | TerminatingMacro | `read-left-paren` |
| `)` | TerminatingMacro | `read-right-paren` |
| `'` | TerminatingMacro | `read-quote` |
| `;` | TerminatingMacro | `read-semicolon` |
| `"` | TerminatingMacro | `read-double-quote` |
| `` ` `` | TerminatingMacro | `read-backquote` |
| `,` | TerminatingMacro | `read-comma` |
| `#` | NonTerminatingMacro | dispatch macro |
| `\` | SingleEscape | — |
| `\|` | MultipleEscape | — |
| All other printable ASCII | Constituent | — |

### ReaderState

```rust
/// Mutable state threaded through a single `read` invocation.
pub struct ReaderState {
    pub stream: Box<dyn CharStream>,        // file, string, or REPL input
    pub readtable: *const Readtable,        // from *READTABLE*
    pub current_package: *const Package,    // from *PACKAGE*
    pub read_base: u32,                     // *READ-BASE* (default 10)
    pub read_suppress: bool,                // *READ-SUPPRESS*
    pub read_preserving_whitespace: bool,
    pub circle_table: HashMap<u64, CircleEntry>,  // #n=/#n# labels
    pub pos_tracker: PosTracker,
    pub recursive_p: bool,
}

#[derive(Clone)]
pub enum CircleEntry {
    Pending(PlaceholderRef),   // seen #n= but reading in progress
    Resolved(BlissVal),        // fully read
}

pub struct PosTracker {
    pub file: Option<PathBuf>,
    pub byte_offset: u64,
    pub line: u32,    // 1-based
    pub col: u32,     // 1-based, codepoints
}

#[derive(Clone, Copy)]
pub struct SourceSpan {
    pub file_id: u32,    pub start_line: u32,  pub start_col: u32,
    pub end_line: u32,   pub end_col: u32,
    pub byte_offset: u64, pub byte_len: u32,
}
```

**Object–source-span association:**  Source spans are stored in a
side-table (`HashMap<ObjectId, SourceSpan>`) rather than inline in
every object header. The reader populates this table; the compiler
queries it; the GC preserves entries for live objects (weak-key map).

---

## 4.1.3 Algorithms

### A4.05 Reader Algorithm (CLHS §2.2 — 10 Steps)

```text
PROCEDURE read(state: &mut ReaderState) → BlissVal:

  STEP 1 — Read one character X from stream.
    If EOF:
      If recursive_p → signal READER-ERROR (unexpected EOF).
      If eof_error_p → signal END-OF-FILE.
      Else → return eof_value.

  STEP 2 — Classify X by its syntax type in the current readtable.

  STEP 3 — If X is an INVALID character:
    Signal READER-ERROR "invalid character".

  STEP 4 — If X is WHITESPACE:
    Discard X, go to Step 1.

  STEP 5 — If X is a TERMINATING or NON-TERMINATING MACRO character:
    Call the associated reader macro function with (stream, X).
    [If read_suppress: the macro function still runs (to consume
     tokens syntactically) but its return value is discarded;
     return NIL.]
    If the macro function returns zero values → go to Step 1 (comment).
    If it returns one value → return that value.

  STEP 6 — If X is a SINGLE-ESCAPE character:
    Read next char Y.  If EOF → signal READER-ERROR.
    Begin token; add Y as constituent with trait "alphabetic" (escaped).
    Go to Step 8.

  STEP 7 — If X is a MULTIPLE-ESCAPE character:
    Begin token; enter |...| mode.
    Go to Step 9.

  STEP 8 — CONSTITUENT or escaped character starts a token.
    (This is even-multiple-escape mode.)
    Accumulate characters into a token buffer:
    LOOP:
      Read character Y.
      Y is EOF → go to Step 10.
      Y is CONSTITUENT or NON-TERMINATING MACRO →
        add Y to token, continue loop.
      Y is SINGLE-ESCAPE →
        read next char Z (EOF → error), add Z as escaped, continue.
      Y is MULTIPLE-ESCAPE → enter Step 9 (odd-multiple-escape mode).
      Y is TERMINATING MACRO → unread Y, go to Step 10.
      Y is WHITESPACE →
        if read_preserving_whitespace then unread Y.
        Go to Step 10.

  STEP 9 — ODD-MULTIPLE-ESCAPE mode (inside |...|).
    LOOP:
      Read character Y.
      Y is EOF → signal READER-ERROR "unterminated |".
      Y is CONSTITUENT, WHITESPACE, TERMINATING MACRO,
        NON-TERMINATING MACRO → add Y as escaped, continue.
      Y is SINGLE-ESCAPE →
        read next char Z (EOF → error), add Z as escaped, continue.
      Y is MULTIPLE-ESCAPE → return to Step 8 (even mode).

  STEP 10 — Token complete.  Interpret the accumulated token:
    a) Apply readtable case conversion to non-escaped characters.
    b) If read_suppress is true → return NIL (do not construct any
       object, do not intern symbols, do not signal package/symbol
       errors — see READ-SUPPRESS rules below).
    c) Attempt to parse as a number (→ Algorithm A4.06).
       If successful → return the numeric object.
    d) Otherwise, interpret as a symbol name.
       Resolve package qualification (§4.1.4).
       Intern or find the symbol in the appropriate package.
       Return the symbol.
```

**`*READ-SUPPRESS*` semantics (CLHS §2.2):**

When `*READ-SUPPRESS*` is true (bound by `#+` / `#-` to skip forms):

1. **No object construction:** The reader MUST NOT construct objects.
   Top-level `read` returns NIL. Recursive reads for skipped forms
   also return NIL.
2. **No interning:** Symbols MUST NOT be interned. The reader still
   parses tokens to consume the correct number of s-expressions, but
   symbol names are discarded.
3. **Suppressed errors:** The following errors MUST NOT be signalled
   under `*READ-SUPPRESS*`:
   - Undefined package (`pkg:sym` where `pkg` does not exist).
   - Symbol not external (`pkg:sym` where `sym` is not exported).
   - Undefined `#n#` circle label reference.
   - Invalid `#` dispatch sub-characters (except structurally
     malformed input that prevents determining how much to skip).
   - Invalid radix digits, malformed numbers, etc.
4. **Syntactic parsing preserved:** The reader MUST still parse
   syntactically to correctly determine token boundaries, balancing
   of parentheses, strings, and `|...|` escapes. This is essential
   for `#+` / `#-` to skip exactly one form.
5. **Interaction with reader macros:** Standard reader macros (list,
   string, `#(...)`, etc.) MUST still recursively read sub-forms to
   consume them, but discard the results. Dispatch macros that take a
   numeric argument still read the argument.
6. **Interaction with `#=` / `##`:** `#n=` still reads the sub-form
   (to consume it) but MUST NOT store a label. `#n#` MUST NOT signal
   an error for undefined labels; it returns NIL.

**Case conversion rules (Step 10a):**

| `ReadtableCase` | Action on non-escaped chars |
|-----------------|----------------------------|
| `Upcase` | Convert to uppercase |
| `Downcase` | Convert to lowercase |
| `Preserve` | No change |
| `Invert` | If all unescaped chars are uppercase → downcase all; if all lowercase → upcase all; mixed → no change |

### A4.06 Number Parsing FSM

Number tokens are recognised by a finite-state machine that handles
integers, ratios, floats, and the potential-number rules of CLHS §2.3.1.

```text
INPUT: token chars[] with escaped[] flags, read_base (default 10)
OUTPUT: BlissVal (fixnum, bignum, ratio, float) or FAIL (not a number)

RULE 0 — No escaped character may appear in a number.
          If any char is escaped → FAIL immediately.

PHASE 1 — Sign prefix:
  If chars[0] ∈ {'+', '-'}, record sign, advance index.
  If remaining is empty → FAIL (bare sign is a symbol).

PHASE 2 — Classify token form by scanning for '/', '.', exponent markers.

  CASE A — INTEGER (decimal or radix):
    All remaining chars are digits valid in *READ-BASE*.
    Optional trailing '.' makes it explicitly decimal (base 10).
    If magnitude fits 61 bits → return Fixnum.
    Else → allocate Bignum.

  CASE B — RATIO (contains exactly one '/'):
    Split at '/'.  Both halves must be non-empty sequences of
    digits valid in *READ-BASE*.
    Parse numerator and denominator.
    Reduce to lowest terms (GCD).
    If denominator = 1 → return integer.
    If denominator = 0 → signal READER-ERROR "zero denominator".
    Else → return Ratio object.

  CASE C — FLOAT (contains '.', or exponent marker, or both):
    Grammar (extended BNF):
      float    ::= sign? integer? '.' fraction? exponent?
                 | sign? integer exponent
      integer  ::= digit+
      fraction ::= digit+
      exponent ::= marker sign? digit+
      marker   ::= [sS] | [fF] | [dD] | [lL] | [eE]
      digit    ::= [0-9]        (floats are always base 10)

    Exponent marker mapping:
      S, s → short-float  (Bliss: single-float / f32)
      F, f → single-float (f32)
      D, d → double-float (f64)
      L, l → long-float   (Bliss: double-float / f64)
      E, e → *READ-DEFAULT-FLOAT-FORMAT* (default: single-float)

    If no digits at all (e.g., just ".") → FAIL.
    Parse via Rust `f64::from_str` or equivalent after normalisation.
    Return Single-Float or Double-Float per marker.

  CASE D — POTENTIAL NUMBER (CLHS §2.3.1.1):
    A token is a potential number if and only if ALL of these hold:
      (a) It consists entirely of digits, sign characters (+/-),
          ratio markers (/), decimal points (.), extension characters
          (^, _), and number markers (letters that are not adjacent
          to other letters — used as exponent markers, etc.).
      (b) It contains at least one digit (a character whose digit-
          weight in *READ-BASE* is non-NIL, or a decimal digit if a
          decimal point is present).
      (c) It starts with a digit, sign, decimal point, or extension
          character.
      (d) It does not end with a sign.
      (e) It contains no package marker (colon).

    If the token satisfies potential-number syntax but does not
    match any concrete number syntax (integer, ratio, or float)
    above → Bliss signals a READER-ERROR with the message
    "token has potential number syntax but is not a valid number".
    (Rationale: silently treating these as symbols masks typos;
    signalling an error is the safest portable-compatible choice and
    matches the behaviour of SBCL and CCL.)
```

**Complex numbers** are handled by the `#C` dispatch macro (§4.1.6),
not by the number parser directly.

---

## 4.1.4 Package-Qualified Symbol Resolution

When Step 10c encounters a token with package markers, the reader
resolves the symbol as follows:

```text
INPUT: token string (after case conversion)
OUTPUT: symbol BlissVal

CASE 1 — No package marker:
  Intern symbol in *PACKAGE* (current package).

CASE 2 — Leading ':' (keyword):
  Intern in the KEYWORD package.
  Ensure the symbol's value cell is bound to itself.

CASE 3 — Single colon "pkg:sym":
  Find package named "pkg".  If not found → READER-ERROR.
  Look up "sym" among the EXTERNAL symbols of pkg.
  If not found → READER-ERROR "symbol SYM not external in PKG".
  Return the symbol.

CASE 4 — Double colon "pkg::sym":
  Find package named "pkg".  If not found → READER-ERROR.
  Intern "sym" in pkg (creates it if absent — full access).
  Return the symbol.

CASE 5 — "#:" prefix (uninterned):
  Create a fresh, uninterned symbol with the given name.
  It is not recorded anywhere; each occurrence creates a new symbol.
```

The package lookup (`find-package`) MUST be case-sensitive and MUST
consult the package nickname table as well as primary names.

---

## 4.1.5 Standard Reader Macros

Each standard reader macro is a Rust function with signature:
`fn(state: &mut ReaderState, char: char) → Option<BlissVal>`.
Returning `None` means "no value produced" (e.g., comments).

| Char | Macro | Behaviour |
|------|-------|-----------|
| `(` | `read_left_paren` | Read list elements until `)`. Handle dotted pair: exactly one object after `.`. Signal error on `. . `, `. )` at start, etc. |
| `)` | `read_right_paren` | Signal READER-ERROR if not inside a list read (stray `)`). Otherwise handled by `read_left_paren`'s loop. |
| `'` | `read_quote` | Read next object X, return `(QUOTE X)`. |
| `;` | `read_semicolon` | Skip to end of line, return no values. |
| `"` | `read_double_quote` | Accumulate chars until unescaped `"`. Handle `\` escapes: `\\` → `\`, `\"` → `"`. Return a string object. |
| `` ` `` | `read_backquote` | Read next form X, return backquote expansion template (see §4.1.5.1 below). |
| `,` | `read_comma` | Must appear inside backquote. Read next form. If next char is `@`, read form and wrap in `SYS:BQ-SPLICE`. If next char is `.`, read form and wrap in `SYS:BQ-NSPLICE` (destructive splice). Else wrap in `SYS:BQ-UNQUOTE`. Signal READER-ERROR if outside backquote. |

### 4.1.5.1 Backquote Expansion Algorithm

Backquote (quasiquote) expansion is implementation-dependent per CLHS
§2.4.6, but MUST produce forms that, when evaluated, yield the
structure described by the template. Bliss follows the algorithm
described in **Alan Bawden, "Quasiquotation in Lisp" (1999)**, which
correctly handles arbitrary nesting depths. Guy Steele's Appendix C
from CLtL2 is an acceptable alternative reference.

**Internal BQ-* forms:**

The reader produces an intermediate representation using the following
internal symbols in the `SYS` package. These are NOT part of the public
API and MUST NOT appear in fully expanded code after the backquote
expander runs.

| Form | Meaning |
|------|---------|
| `(SYS:BQ-QUOTE x)` | A self-evaluating or quoted datum — equivalent to `'x`. |
| `(SYS:BQ-UNQUOTE x)` | An unquoted form — evaluates `x` at runtime. Produced by `,x`. |
| `(SYS:BQ-SPLICE x)` | A splicing unquote — `x` must evaluate to a list whose elements are spliced in. Produced by `,@x`. |
| `(SYS:BQ-NSPLICE x)` | A destructive splicing unquote — like `BQ-SPLICE` but may use `NCONC`. Produced by `,.x`. |
| `(SYS:BQ-LIST x1 ... xn)` | Constructs a list from evaluated sub-forms. |
| `(SYS:BQ-LIST* x1 ... xn tail)` | Constructs a dotted list — like `LIST*`. |
| `(SYS:BQ-APPEND x1 ... xn)` | Appends evaluated list-valued sub-forms. |
| `(SYS:BQ-NCONC x1 ... xn)` | Destructive version of `BQ-APPEND`. |
| `(SYS:BQ-VECTOR contents)` | Backquoted vector `#(...)` — expands `contents` then coerces to simple-vector. |

**Expansion phases:**

1. **Read phase** (in the reader): `` `form `` is read as
   `(SYS:BQ-QUOTE form)` for atoms, or recursively walks list/vector
   structure to produce a tree of BQ-* forms. Nested backquotes
   increment a depth counter; commas decrement it. A comma at depth 0
   is an error. A comma at depth > 1 produces a *nested* BQ-UNQUOTE
   that is expanded only when the outer backquote is expanded.

2. **Simplification phase** (called after read, before the form is
   returned): The BQ-* tree is simplified:
   - `(BQ-APPEND (BQ-LIST a b) (BQ-LIST c d))` → `(BQ-LIST a b c d)`
   - `(BQ-LIST* a b ... (BQ-LIST c d))` → `(BQ-LIST a b ... c d)`
   - Constant sub-trees are folded into `BQ-QUOTE`.
   - Splice/nsplice are validated to appear only in list context; a
     splice in dotted-tail or atom context signals READER-ERROR.

3. **Code generation** (at macro-expansion time or compile time):
   The simplified BQ-* tree is lowered to standard CL forms:
   - `BQ-QUOTE` → `QUOTE`
   - `BQ-UNQUOTE` → the form itself
   - `BQ-LIST` → `LIST`
   - `BQ-LIST*` → `LIST*`
   - `BQ-APPEND` → `APPEND`
   - `BQ-NCONC` → `NCONC`

**Nested backquote invariant:** At nesting depth *d*, only commas
at depth *d* are expanded; inner commas remain as literal BQ-*
forms in the output structure. This is the key correctness property
for nested backquotes: `` `(a `(b ,,x)) `` must expand such that
the inner `,x` is evaluated when the *outer* backquote's result is
itself evaluated as a backquote template.

---

## 4.1.6 `#`-Dispatch Macro Table

The `#` character is a non-terminating macro character whose handler
reads a numeric argument (optional) and a sub-character, then
dispatches to the appropriate handler.

```text
PROCEDURE read_dispatch(state, '#'):
  1. Read digits accumulating into n: Option<u64>.
  2. Read sub-character C (case-insensitive for letter sub-chars).
  3. Look up C in the dispatch table of '#'.
     If not found → signal READER-ERROR "unknown # sub-char".
  4. Call handler(state, C, n).
```

**Standard dispatch sub-characters:**

| Sub-char | Name | Semantics |
|----------|------|-----------|
| `\` | char literal | `#\Space`, `#\Newline`, `#\x` — parse char name or single char. |
| `'` | function | `#'foo` → `(FUNCTION FOO)`. |
| `(` | simple-vector | `#(1 2 3)` → `#(1 2 3)`. Optional `n` prefix for length. |
| `*` | bit-vector | `#*101` → bit-vector. |
| `:` | uninterned symbol | `#:foo` — see §4.1.4 Case 5. |
| `.` | read-eval | `#.(+ 1 2)` → evaluate at read time. Controlled by `*READ-EVAL*`; if NIL, signal READER-ERROR. |
| `B`/`b` | binary integer | `#b1010` → 10. |
| `O`/`o` | octal integer | `#o17` → 15. |
| `X`/`x` | hex integer | `#xFF` → 255. |
| `R`/`r` | radix-n integer | `#3r102` → 11 (base 3). `n` is required. |
| `C`/`c` | complex number | `#C(1.0 2.0)` → complex. Read a two-element list. |
| `A`/`a` | array | `#nA(...)` — multi-dimensional array, `n` = rank. |
| `S`/`s` | structure | `#S(type slot val ...)` — construct structure. |
| `P`/`p` | pathname | `#P"path"` → pathname object. |
| `+` | feature-present | `#+feature form` — read form only if feature is in `*FEATURES*`. |
| `-` | feature-absent | `#-feature form` — read form only if feature is NOT in `*FEATURES*`. |
| `\|` | block comment | `#\| ... \|#` — nestable block comment. |
| `=` | circle-define | `#n=obj` — define label `n` for circular structure. |
| `#` | circle-ref | `#n#` — reference label `n` defined by `#n=`. |
| `<` | unreadable | `#<...>` — signal READER-ERROR (unreadable object). |

**Feature expressions** (`#+` / `#-`) MUST support compound feature
tests: `(and f1 f2)`, `(or f1 f2)`, `(not f)`, with arbitrary nesting.

---

## 4.1.7 Circular Structure (`#n=` / `#n#`)

Circular and shared structure is handled via a label table in
`ReaderState.circle_table`.

```text
PROCEDURE read_circle_define(state, '=', n):
  1. n MUST be provided; signal error if None.
  2. If n already in circle_table → signal READER-ERROR "duplicate label".
  3. Create a placeholder (a unique Cons cell with UNBOUND in car/cdr).
  4. Store CircleEntry::Pending(placeholder) in circle_table[n].
  5. Read the next object OBJ recursively.
  6. Replace circle_table[n] with CircleEntry::Resolved(OBJ).
  7. Walk OBJ and replace all occurrences of placeholder with OBJ
     (back-patching phase — see below).
  8. Return OBJ.

PROCEDURE read_circle_ref(state, '#', n):
  1. n MUST be provided; signal error if None.
  2. Look up n in circle_table.
     Not found → signal READER-ERROR "undefined label #n#".
  3. If Pending(placeholder) → return placeholder.
     (Will be replaced during back-patching of the enclosing #n=.)
  4. If Resolved(obj) → return obj.

BACK-PATCHING:
  Walk the object graph of OBJ (conses, vectors, structure slots):
  - If a slot == placeholder, replace with OBJ.
  - Guard against infinite loops by tracking visited objects
    (identity-based HashSet).
  - Back-patching MUST handle: cons car/cdr, simple-vector elements,
    structure slot values.
  - Back-patching MUST NOT descend into strings, numbers, or symbols.
```

The `circle_table` is scoped to a single top-level `read` call.  It is
cleared when `recursive_p` is false at entry.

---

## 4.1.8 Readtable API

During bootstrap these are Rust functions; after self-hosting, CL wrappers delegate to internals.

| Function | Signature | Key semantics |
|----------|-----------|---------------|
| `COPY-READTABLE` | `&optional from to` | Per CLHS: `from` is a readtable designator — if omitted, defaults to the current value of `*READTABLE*`; if NIL, designates the *standard* readtable; if a readtable object, uses that readtable. `to`: if NIL or omitted → return a fresh copy; if a readtable → modify it destructively to be a copy of `from` and return it. Deep copy: modifying the result MUST NOT affect the original. Note: T is NOT a valid readtable designator and MUST signal a `TYPE-ERROR`. |
| `MAKE-DISPATCH-MACRO-CHARACTER` | `char &optional non-term-p rt` | Set `char` as macro char with empty dispatch table. Non-term-p controls syntax type. Returns T. |
| `SET-MACRO-CHARACTER` | `char fn &optional non-term-p rt` | Install `fn` as reader macro for `char`, set syntax type. Returns T. |
| `GET-MACRO-CHARACTER` | `char &optional rt` | Returns two values: macro function (or NIL) and non-terminating-p. |
| `SET-DISPATCH-MACRO-CHARACTER` | `disp sub fn &optional rt` | `disp` MUST be dispatch macro char. `sub` MUST NOT be a digit. Stores `fn` in dispatch table. |
| `GET-DISPATCH-MACRO-CHARACTER` | `disp sub &optional rt` | Returns dispatch function for `sub` under `disp`, or NIL. |
| `READTABLE-CASE` | `rt` | Returns case mode. `(SETF READTABLE-CASE)` accepts `:UPCASE`, `:DOWNCASE`, `:PRESERVE`, `:INVERT`. |

---

## 4.1.9 Error Handling

All reader errors signal the condition type `READER-ERROR`, which is a
subtype of `STREAM-ERROR` per CLHS §23.

```rust
/// Reader error with source position.
pub struct ReaderErrorData {
    pub message: String,
    pub span: SourceSpan,
    pub stream: BlissVal,  // the stream, for STREAM-ERROR-STREAM
}
```

**Error categories and triggers:**

| Category | Trigger | Restart? |
|----------|---------|----------|
| Unexpected EOF | EOF inside string, list, `\|...\|`, `#n=` | No |
| Invalid character | `SyntaxType::Invalid` encountered | No |
| Unknown dispatch char | Unregistered `#` sub-char | No |
| Package not found | `nonexistent-pkg:sym` | `USE-VALUE` |
| Symbol not external | `pkg:internal-sym` | `USE-VALUE` |
| Duplicate circle label | `#1=... #1=...` | No |
| Undefined circle label | `#99#` with no prior `#99=` | No |
| Illegal dot context | `.` at start of list, multiple dots | No |
| Unreadable object | `#<...>` | No |
| Read-eval disabled | `#.form` when `*READ-EVAL*` is NIL | No |
| Zero denominator | `1/0` in ratio literal | No |
| Malformed number/char | Partial float `1.2e`, `#\BadName` | No |

`SourceSpan` MUST be attached to every `READER-ERROR` for diagnostics.

---

## 4.1.10 Concurrency

- `*READTABLE*` is a per-thread dynamic binding; concurrent reads on
  different streams do not contend.
- The standard readtable is immutable after init and MAY be shared
  without locking.
- `INTERN`/`FIND-SYMBOL` acquire the target package's RW lock (§5).
  Lock ordering: readtable-free → package-lock.
- Source-span side-table is per-compilation-unit (not global).

---

## 4.1.11 Configuration

| Variable | Default | Effect |
|----------|---------|--------|
| `*READTABLE*` | Standard readtable (upcase) | Current readtable |
| `*READ-BASE*` | 10 | Radix for integer/ratio tokens |
| `*READ-DEFAULT-FLOAT-FORMAT*` | `SINGLE-FLOAT` | Float type for `E` exponent marker |
| `*READ-EVAL*` | T | Allow `#.` read-time eval |
| `*READ-SUPPRESS*` | NIL | Suppress object construction (for `#+`/`#-`) |
| `*PACKAGE*` | `COMMON-LISP-USER` | Current package for interning |
| `*FEATURES*` | `(:BLISS :ANSI-CL :IEEE-FLOATING-POINT :64-BIT ...)` | Feature list for `#+`/`#-` |
| `BLISS:*READER-SOURCE-TRACKING*` | T | Enable source-span collection (disable for batch loads where debug info is unwanted) |

---

## 4.1.12 Test Strategy

| # | Test | Method |
|---|------|--------|
| T4.01 | Round-trip `(read (write-to-string obj))` for all printable types | Property-based |
| T4.02 | All 10 reader steps exercised | Unit tests per step |
| T4.03 | Number parser: integers, ratios, floats, edge cases | Table-driven |
| T4.04 | Package qualification: all 5 cases + errors | Unit tests |
| T4.05 | Circular structure: nested, forward-ref, dup-label error | Unit tests |
| T4.06 | All `#`-dispatch sub-characters | One test per sub-char |
| T4.07 | Custom reader macros via `SET-MACRO-CHARACTER` | Unit + integration |
| T4.08 | Source position accuracy vs known input | Directed tests |
| T4.09 | Every error category signals correct condition with position | Negative tests |
| T4.10 | `ansi-test` reader chapter | Integration |
| T4.11 | Concurrent reads from independent streams | Stress test |
| T4.12 | All four readtable case modes | Parameterised |
