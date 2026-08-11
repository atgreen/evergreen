# §5.8 Pathnames and Logical Pathnames

**Scope:** This section specifies the pathname abstraction in Bliss,
covering the `PATHNAME` and `LOGICAL-PATHNAME` class hierarchy,
component representation, parsing and reconstruction algorithms,
logical pathname translation, pathname merging, wildcard matching,
and file-system interaction. Bliss targets ANSI X3.226-1994 §19
with POSIX-oriented physical pathnames and a Bliss-specific `~`
expansion extension.

---

## 5.8.1 Requirements

| ID | Requirement |
|----|-------------|
| R5.181 | Bliss MUST implement the `PATHNAME` class as a sealed, immutable structure with slots: host, device, directory, name, type, version. |
| R5.182 | Bliss MUST implement `LOGICAL-PATHNAME` as a subclass of `PATHNAME` whose components are canonically uppercase strings. |
| R5.183 | The directory component MUST be represented as a list with a keyword head (`:ABSOLUTE` or `:RELATIVE`) followed by string or keyword elements (`:WILD`, `:WILD-INFERIORS`, `:UP`, `:BACK`). |
| R5.184 | `MAKE-PATHNAME` MUST accept keyword arguments `:HOST`, `:DEVICE`, `:DIRECTORY`, `:NAME`, `:TYPE`, `:VERSION`, `:DEFAULTS`, and `:CASE` per ANSI 19.4.2. |
| R5.185 | `PARSE-NAMESTRING` MUST accept a string and optional host, default-pathname, and return a pathname plus the index where parsing stopped. |
| R5.186 | Physical pathname parsing (algorithm A5.11) MUST handle POSIX path semantics: `/` separator, absolute vs relative detection, `.` and `..` canonicalisation. |
| R5.187 | Namestring reconstruction (algorithm A5.12) MUST produce a string that, when re-parsed, yields an `EQUAL` pathname. Round-trip: `(equal pn (parse-namestring (namestring pn)))` MUST hold for all well-formed physical pathnames. |
| R5.188 | Logical pathname parsing MUST accept the syntax `[host:]word{;word}*` with uppercase canonicalisation, per ANSI 19.3.1. |
| R5.189 | `LOGICAL-PATHNAME-TRANSLATIONS` MUST be `SETF`-able and store a list of `(from-wildcard to-wildcard)` pairs per logical host. |
| R5.190 | `TRANSLATE-LOGICAL-PATHNAME` (algorithm A5.13) MUST iterate translations, matching the logical pathname against each from-pattern via `PATHNAME-MATCH-P`, and substitute matched components into the to-pattern. |
| R5.191 | `MERGE-PATHNAMES` (algorithm A5.14) MUST implement the ANSI 19.2.3 defaulting protocol: fill `NIL` components from the default pathname, then from `*DEFAULT-PATHNAME-DEFAULTS*`. |
| R5.192 | `PATHNAME-MATCH-P` MUST support wildcards `:WILD`, `:WILD-INFERIORS` (in directories), and `*` within name/type strings. |
| R5.193 | `TRANSLATE-PATHNAME` MUST transfer wildcard-matched fragments from a source pathname to a target pattern per ANSI 19.2.2.5. |
| R5.194 | Bliss MUST expand leading `~` and `~user` in namestrings at parse time as a Bliss extension (§9). The expanded pathname stores the resolved absolute path, not the tilde form. |
| R5.195 | `ENOUGH-NAMESTRING` MUST return a string sufficient to reconstruct the pathname relative to a given default. |
| R5.196 | `PROBE-FILE` MUST return the truename if the file exists, or `NIL`. It MUST resolve symlinks. |
| R5.197 | `TRUENAME` MUST call `realpath(3)` on POSIX and signal `FILE-ERROR` if the file does not exist. |
| R5.198 | `DIRECTORY` MUST accept a wild pathname and return a list of truename pathnames matching the pattern. It MUST handle `:WILD-INFERIORS` via recursive directory traversal. |
| R5.199 | `ENSURE-DIRECTORIES-EXIST` MUST create all missing directories in the pathname's directory component (equivalent to `mkdir -p`). It MUST return the pathname and a boolean indicating whether any directory was created. |
| R5.200 | All pathname objects MUST be immutable once constructed. Concurrent reads from multiple threads MUST NOT require synchronisation. |
| R5.201 | `USER-HOMEDIR-PATHNAME` MUST return a pathname for the current user's home directory (from `$HOME` or `getpwuid`), with an optional host argument (ignored on POSIX). The returned pathname MUST have name and type components of NIL. |
| R5.202 | `WILD-PATHNAME-P` MUST return true if the pathname contains any wild components (`:WILD`, `:WILD-INFERIORS`, or `*` within name/type strings). When called with an optional field-key argument (`:HOST`, `:DEVICE`, `:DIRECTORY`, `:NAME`, `:TYPE`, `:VERSION`), it MUST return true only if that specific component is wild. |

---

## 5.8.2 Data Structures

### D5.30 — Pathname

```rust
/// Tagged as Heap object (tag 010). Header class-id = CLASS_PATHNAME.
#[repr(C)]
pub struct Pathname {
    pub header: ObjectHeader,  // class-id, GC bits
    pub host:      BlissVal,   // NIL | string | logical-host-designator
    pub device:    BlissVal,   // NIL | string | :UNSPECIFIC
    pub directory: BlissVal,   // NIL | (:ABSOLUTE|:RELATIVE . components)
    pub name:      BlissVal,   // NIL | string | :WILD | :UNSPECIFIC
    pub type_:     BlissVal,   // NIL | string | :WILD | :UNSPECIFIC
    pub version:   BlissVal,   // NIL | :NEWEST | :WILD | :UNSPECIFIC | integer
    pub namestring_cache: AtomicPtr<BlissVal>, // lazily computed, immutable once set
}
```

**Invariants:**
- All slots are set at construction time and never mutated (R5.200).
- `namestring_cache` populated on first `NAMESTRING` call via CAS; stored string is immutable.
- `directory` list elements are interned keywords or immutable strings.

### D5.31 — LogicalPathname

```rust
/// Subclass of Pathname. Header class-id = CLASS_LOGICAL_PATHNAME.
#[repr(C)]
pub struct LogicalPathname {
    pub base: Pathname,  // all Pathname slots
    // No additional slots; differs in class-id and invariant:
    // all string components are uppercase ASCII.
}
```

**Invariants:**
- All string components contain only uppercase letters, digits, and hyphens.
- Host is a non-NIL string naming a defined logical host.

### D5.32 — LogicalHostEntry

```rust
pub struct LogicalHostEntry {
    pub name: Box<str>,                          // canonical uppercase
    pub translations: RwLock<Vec<Translation>>,   // SETF-able, per R5.189
}

pub struct Translation {
    pub from_pattern: GcRef<Pathname>,   // logical wild pathname
    pub to_pattern:   GcRef<Pathname>,   // physical wild pathname
}
```

**Storage:** A global `RwLock<HashMap<Box<str>, LogicalHostEntry>>`
keyed by uppercase host name. Accessed via `LOGICAL-PATHNAME-TRANSLATIONS`.

### D5.33 — Directory Component Representation

The directory slot is a proper list with one of two structures:

```lisp
;; Absolute:  (:ABSOLUTE "usr" "local" "lib")   → /usr/local/lib/
;; Relative:  (:RELATIVE "src" "utils")          → src/utils/
;; With wildcards:
;;   (:ABSOLUTE "home" :WILD "projects" :WILD-INFERIORS)
;;   → /home/*/projects/**/
```

| Element | Meaning |
|---------|---------|
| `:ABSOLUTE` | Path starts from root `/` |
| `:RELATIVE` | Path is relative to working directory |
| String | Literal directory name |
| `:WILD` | Matches exactly one directory level |
| `:WILD-INFERIORS` | Matches zero or more directory levels |
| `:UP` | Parent directory (canonical `..`) |
| `:BACK` | Syntactic parent (undoes previous component without file-system access) |

---

## 5.8.3 Algorithms

### A5.11 — Physical Pathname Parsing (POSIX)

**Input:** namestring `S` (a string), optional default-host.
**Output:** a `Pathname` instance.

```text
PARSE-PHYSICAL-POSIX(S):
  1. TILDE EXPANSION (Bliss extension, R5.194):
     a. If S starts with "~/" → replace "~" with value of
        environment variable HOME (or pw_dir from getpwuid).
     b. If S starts with "~user/" → replace "~user" with
        pw_dir from getpwnam("user").
     c. If lookup fails → signal FILE-ERROR.
     d. Set S to the expanded string.

  2. ABSOLUTE / RELATIVE DETECTION:
     a. If S starts with "/" → dir-head ← :ABSOLUTE, advance past
        leading "/" (collapse multiple leading "/" into one,
        except "//" which is implementation-defined — Bliss treats
        "//" identically to "/").
     b. Else → dir-head ← :RELATIVE.

  3. TOKENISE on "/":
     Split S into tokens by "/".  Discard empty tokens produced by
     trailing or consecutive "/" characters.

  4. SEPARATE NAME.TYPE from directory tokens:
     a. last-token ← pop last token from the list.
     b. If S ended with "/" → last-token is a directory component,
        push it back; name ← NIL, type ← NIL.
     c. Else → split last-token on the rightmost ".":
        - No "." → name ← last-token, type ← NIL.
        - "." is first char AND is the only "." in the token →
          name ← last-token (including dot), type ← NIL.
          (Hidden files: ".bashrc" → name=".bashrc")
        - "." is first char but there are additional dots →
          apply the rightmost-dot rule: name ← part before
          rightmost ".", type ← part after rightmost ".".
          (E.g., ".bashrc.bak" → name=".bashrc", type="bak")
        - Otherwise → name ← part before rightmost ".",
          type ← part after rightmost ".".
     d. If last-token is empty string → name ← NIL, type ← NIL.

  5. CANONICALISE directory tokens:
     For each token in remaining directory tokens:
       - "."  → skip (current directory, no-op).
       - ".." → append :UP to directory list.
       - "*"  → append :WILD.
       - "**" → append :WILD-INFERIORS.
       - Otherwise → append token as string.

  6. CONSTRUCT PATHNAME:
     host      ← NIL (physical POSIX has no host)
     device    ← :UNSPECIFIC
     directory ← (dir-head . canonicalised-tokens), or NIL if empty
                  and dir-head is :RELATIVE and no tokens exist
     name      ← from step 4 (NIL or string)
     type      ← from step 4 (NIL or string)
     version   ← :NEWEST
     Return MAKE-PATHNAME with these components.
```

**Edge cases:**
- Empty string `""` → all components NIL except version ← `:NEWEST`.
- Root `"/"` → directory `(:ABSOLUTE)`, name NIL, type NIL.
- Trailing slash `"/tmp/"` → directory `(:ABSOLUTE "tmp")`, name NIL.
- Dot files: `".gitignore"` → name `".gitignore"`, type NIL.
- Multiple extensions: `"foo.tar.gz"` → name `"foo.tar"`, type `"gz"`.

### A5.12 — Namestring Reconstruction

**Input:** a `Pathname` P.
**Output:** a POSIX namestring string.

```text
RECONSTRUCT-NAMESTRING(P):
  1. result ← empty string.

  2. DIRECTORY:
     a. If directory is NIL → skip.
     b. If (first directory) = :ABSOLUTE → append "/".
        If (first directory) = :RELATIVE → (nothing).
     c. For each element E in (rest directory):
        - E is a string   → append E, append "/".
        - E = :WILD        → append "*/".
        - E = :WILD-INFERIORS → append "**/".
        - E = :UP           → append "../".
        - E = :BACK         → error: :BACK MUST be resolved at
          construction time (see below) and MUST NOT appear in a
          pathname passed to NAMESTRING.

  3. NAME:
     a. If name is NIL → skip.
     b. If name = :WILD → append "*".
     c. Else → append name string.

  4. TYPE:
     a. If type is NIL or :UNSPECIFIC → skip.
     b. If type = :WILD → append ".*".
     c. Else → append ".", append type string.

  5. Return result.
```

**Invariant (R5.187):** For any well-formed pathname P constructed from
a POSIX namestring, `(equal P (parse-namestring (namestring P)))`.

**`:BACK` resolution:** Because POSIX namestrings have no distinct
representation for `:BACK` (as opposed to `:UP`), `:BACK` MUST be
resolved at `MAKE-PATHNAME` construction time. When `:BACK` appears
in a directory list passed to `MAKE-PATHNAME`, it is resolved
syntactically by removing the immediately preceding directory
component. If no preceding component exists (i.e., `:BACK` appears
immediately after `:ABSOLUTE` or `:RELATIVE`), `MAKE-PATHNAME` signals
a `FILE-ERROR`. After construction, no pathname's directory slot will
ever contain `:BACK`. This guarantees the round-trip invariant R5.187
holds for all constructed pathnames.

### A5.13 — Translate Logical Pathname

**Input:** logical pathname `LP`.
**Output:** a physical pathname.

```text
TRANSLATE-LOGICAL-PATHNAME(LP):
  1. host ← (pathname-host LP).
  2. translations ← (logical-pathname-translations host).
     If host is not a defined logical host → signal TYPE-ERROR.

  3. For each (from-pattern, to-pattern) in translations:
     a. If (PATHNAME-MATCH-P LP from-pattern):
        result ← (TRANSLATE-PATHNAME LP from-pattern to-pattern)
        ;; If result is still a logical pathname, recurse:
        If result is a LOGICAL-PATHNAME
          → return TRANSLATE-LOGICAL-PATHNAME(result)
        Else
          → return result.

  4. If no translation matched → signal FILE-ERROR
     "No translation for logical pathname ~A" LP.
```

**PATHNAME-MATCH-P** semantics for logical pathnames:
- `:WILD` matches any single component (string or keyword).
- `:WILD-INFERIORS` matches zero or more directory components.
- String comparison is case-insensitive (all components are uppercase).
- `*` within a name/type string matches any substring.

**TRANSLATE-PATHNAME** wildcard transfer per component:
- `:WILD` in to-pattern → substitute matched component from source.
- `*` in to-pattern string → substitute substring matched by `*` in from-pattern.
- Literal in to-pattern → use as-is.
- `:WILD-INFERIORS`: matched directory subsequence spliced into to-pattern at corresponding position.

### A5.14 — Merge Pathnames

**Input:** pathname P, default-pathname D, default-version V.
**Output:** merged pathname.

```text
MERGE-PATHNAMES(P, D, V):
  ;; D defaults to *DEFAULT-PATHNAME-DEFAULTS*
  ;; V defaults to :NEWEST

  1. For each component C in (host, device, directory, name, type):
     merged-C ←
       If (pathname-C P) is non-NIL → (pathname-C P)
       Else → (pathname-C D)

  2. DIRECTORY MERGE (ANSI 19.2.3 special rule):
     If P has a relative directory and D has a directory:
       merged-directory ← append D's directory components
                          before P's relative components.
       i.e., (append (pathname-directory D)
                     (rest (pathname-directory P)))
     Else:
       Use the result from step 1.

  3. VERSION:
     If (pathname-name P) is non-NIL → merged-version ← (or (pathname-version P) V)
     Else → merged-version ← (or (pathname-version P) (pathname-version D) V)

  4. Return MAKE-PATHNAME with merged components.
```

---

## 5.8.4 Logical Pathname Syntax and Parsing

Logical pathnames use a host-based syntax distinct from physical paths:

```
logical-namestring ::= [host ":"] [";"]  { word ";" }* [name] ["." type ["." version]]
word               ::= { letter | digit | "-" }+
```

**Parsing rules:**
1. If string contains `:` preceded by a valid logical host → extract host, parse remainder.
2. Leading `;` → relative directory; otherwise absolute.
3. Split on `;` for directory components (uppercase).
4. Final segment split on `.` → name, type, optional version.
5. `*` → `:WILD`; `**` as directory → `:WILD-INFERIORS`.
6. Version: `NEWEST` → `:NEWEST`; `*` → `:WILD`; numeric → integer; absent → `NIL`.
7. All string components uppercased at parse time (R5.182).

**Example:**

```lisp
(setf (logical-pathname-translations "SYS")
      '(("SYS:SRC;**;*.*.*" #P"/opt/bliss/src/**/*.*")))
(translate-logical-pathname "SYS:SRC;COMPILER;IR.LISP")
;; → #P"/opt/bliss/src/compiler/ir.lisp"
```

---

## 5.8.5 Function Contracts

### MAKE-PATHNAME

```lisp
(make-pathname &key host device directory name type version
                    defaults case)
  → pathname
```

- `:CASE` — `:COMMON` (default) or `:LOCAL`. `:COMMON` stores uppercase as-is, interpreted per local convention on output. `:LOCAL` stores verbatim.
- If `:HOST` designates a known logical host, returns a `LOGICAL-PATHNAME`.
- Validates directory list head is `:ABSOLUTE` or `:RELATIVE`; signals `TYPE-ERROR` otherwise.

### PARSE-NAMESTRING

```lisp
(parse-namestring thing &optional host (defaults *default-pathname-defaults*)
                  &key (start 0) end junk-allowed)
  → pathname, position
```

- If `thing` is already a pathname → return it, position = start.
- If `thing` is a stream associated with a file → return
  `(pathname thing)`, position = start. The stream must be a file
  stream; for non-file streams, signals `TYPE-ERROR`.
- If `host` is a known logical host → parse as logical pathname.
- Otherwise → parse via A5.11 (POSIX physical).
- `junk-allowed` true: return values up to point of failure.
- `junk-allowed` false (default): signal `PARSE-ERROR` on unconsumed input.

### ENOUGH-NAMESTRING

```lisp
(enough-namestring pathname &optional (defaults *default-pathname-defaults*))
  → string
```

- Returns the shortest namestring that, when merged with `defaults`, reproduces `pathname` via `MERGE-PATHNAMES` (R5.195).
- Omits components equal to the corresponding default. For directory: computes relative path from default's directory if both are absolute.

### PATHNAME-MATCH-P

```lisp
(pathname-match-p pathname wildcard) → boolean
```

- Compares each component of `pathname` against `wildcard`.
- `:WILD` matches any single component value, including NIL, for
  name/type/version components (per ANSI 19.2.2.3 — `:WILD` matches
  any value). This NIL-matching is intentional ANSI alignment.
- `:WILD` in the directory component matches any single directory
  element (string, `:UP`, etc.) but does NOT match an empty directory
  list — an empty directory list is matched only by NIL or another
  empty directory list.
- `:WILD-INFERIORS` in directory matches zero or more directory elements.
- `*` within a string component matches any substring (glob semantics).
- NIL in wildcard component matches only NIL in pathname.
- Host comparison: if the wildcard's host is non-NIL and differs from
  the pathname's host, the match fails. If the wildcard's host is NIL,
  the host component is not constrained (matches any host).

### TRANSLATE-PATHNAME

```lisp
(translate-pathname source from-wildcard to-wildcard &key) → pathname
```

- `source` MUST match `from-wildcard` via `PATHNAME-MATCH-P`.
  If not → signal `error`.
- Transfers wildcard-matched segments from source into `to-wildcard`.
- See A5.13 for wildcard transfer details.

### USER-HOMEDIR-PATHNAME

```lisp
(user-homedir-pathname &optional host) → pathname
```

- Returns a pathname representing the current user's home directory (R5.201).
- On POSIX: reads `$HOME`; if unset, falls back to `pw_dir` from
  `getpwuid(getuid())`.
- The returned pathname has `:ABSOLUTE` directory, name = NIL, type = NIL,
  version = `:NEWEST`. E.g., `#P"/home/user/"`.
- `host` argument is accepted for ANSI compatibility but ignored on POSIX
  (returns the local home directory regardless).
- If the home directory cannot be determined, signals `FILE-ERROR`.

### WILD-PATHNAME-P

```lisp
(wild-pathname-p pathname &optional field-key) → boolean
```

- If `field-key` is NIL (default): returns true if any component of
  `pathname` is wild (R5.202).
- If `field-key` is one of `:HOST`, `:DEVICE`, `:DIRECTORY`, `:NAME`,
  `:TYPE`, `:VERSION`: returns true only if that component is wild.
- A component is wild if it is the keyword `:WILD`, or (for directory)
  contains `:WILD` or `:WILD-INFERIORS`, or (for name/type strings)
  contains the character `*`.
- `:HOST` and `:DEVICE` are never wild for POSIX pathnames.
  For logical pathnames, `:HOST` is never wild.

---

## 5.8.6 File-System Interaction

### PROBE-FILE

```lisp
(probe-file pathspec) → truename | NIL
```

- Converts `pathspec` to a pathname, merges with defaults.
- If the file exists: calls `realpath(3)`, returns the truename
  pathname. For symlinks, returns the resolved target.
- If the file does not exist: returns `NIL`.
- Does NOT signal an error for non-existent files.

### TRUENAME

```lisp
(truename pathspec) → pathname
```

- Like `PROBE-FILE` but signals `FILE-ERROR` if the file does not
  exist (R5.197).
- Implementation: `realpath(3)` on POSIX.

### DIRECTORY

```lisp
(directory pathspec &key) → list-of-pathnames
```

- Accepts a wild pathname (R5.198).
- Returns truename pathnames for all matching files/directories.
- `:WILD-INFERIORS` triggers recursive traversal (`nftw(3)` or
  manual `opendir`/`readdir` recursion).
- Result order is unspecified by ANSI; Bliss sorts lexicographically
  by namestring for deterministic output.
- Symlinks: follows symlinks (returns truenames of targets).

### ENSURE-DIRECTORIES-EXIST

```lisp
(ensure-directories-exist pathspec &key verbose) → pathname, created-p
```

- Creates all directories in the pathname's directory component
  that do not yet exist (equivalent to `mkdir -p`, R5.199).
- Returns two values: the original pathname and a boolean `T` if
  any directory was actually created.
- If `:VERBOSE` is true, prints each directory created to
  `*STANDARD-OUTPUT*`.
- Signals `FILE-ERROR` on permission errors or invalid paths.

---

## 5.8.7 Tilde Expansion (Bliss Extension)

Per R5.194, Bliss expands `~` at parse time (A5.11 step 1):

| Input | Expansion |
|-------|-----------|
| `~/foo/bar.lisp` | `/home/user/foo/bar.lisp` (from `$HOME`) |
| `~root/.bashrc` | `/root/.bashrc` (from `getpwnam`) |
| `~nonexistent/x` | signals `FILE-ERROR` |

**Rationale:** Parse-time expansion ensures pathnames are fully resolved, simplifying comparison and merging. The tilde form never appears in stored components. This extension MUST be documented in `spec/09-extensions.md` (SBCL also expands `~`).

---

## 5.8.8 Error Handling

| Condition | When |
|-----------|------|
| `TYPE-ERROR` | Invalid component type passed to `MAKE-PATHNAME` (e.g., directory list without keyword head). |
| `TYPE-ERROR` | Undefined logical host in `TRANSLATE-LOGICAL-PATHNAME`. |
| `PARSE-ERROR` | `PARSE-NAMESTRING` with `junk-allowed` = NIL and unconsumed input. |
| `FILE-ERROR` | `TRUENAME` on non-existent file. |
| `FILE-ERROR` | Tilde expansion with unknown user. |
| `FILE-ERROR` | `ENSURE-DIRECTORIES-EXIST` with permission failure. |
| `FILE-ERROR` | No matching translation in `TRANSLATE-LOGICAL-PATHNAME`. |

All file-system operations that signal `FILE-ERROR` MUST include
the offending pathname in the condition's `:PATHNAME` slot
(per ANSI 19.4.25).

---

## 5.8.9 Concurrency

- Pathname objects are immutable once constructed (R5.200). No
  synchronisation is required for concurrent reads.
- The `namestring_cache` field in D5.30 uses `AtomicPtr` with
  release-store / acquire-load semantics. Multiple threads may
  race to populate it; all will compute the same string, and
  compare-and-swap ensures exactly one wins. Losers discard their
  computed string (it becomes GC garbage).
- `*DEFAULT-PATHNAME-DEFAULTS*` is a per-thread special variable.
  Each thread has its own binding; no cross-thread synchronisation
  needed.
- `LOGICAL-PATHNAME-TRANSLATIONS` storage (D5.32) is protected by
  a `RwLock`. Reading translations acquires a read lock; `SETF`
  acquires a write lock. Writer starvation is acceptable since
  translation tables are set once at startup.

---

## 5.8.10 Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `*DEFAULT-PATHNAME-DEFAULTS*` | Current directory at startup | Per-thread default pathname for merging. |
| Logical host `"SYS"` | Bliss installation root | Pre-defined logical host for system sources. |
| Env `BLISS_HOME` | `/usr/local/lib/bliss` | Fallback for `SYS:` translation root. |

---

## 5.8.11 Test Strategy

1. **Unit tests (A5.11):** absolute, relative, root, trailing slash, dot files, multiple extensions, consecutive slashes, `//`, empty string, tilde expansion with mocked `$HOME`/`getpwnam`.
2. **Round-trip (R5.187):** random pathnames verify `(equal pn (parse-namestring (namestring pn)))`.
3. **Logical pathnames:** parsing, uppercase canonicalisation, translation with wildcards, recursive translation.
4. **Merge-pathnames:** ANSI 19.2.3 examples, relative directory merging, version defaulting.
5. **Wildcard matching:** `PATHNAME-MATCH-P` with `:WILD`, `:WILD-INFERIORS`, embedded `*`, NIL components.
6. **File-system integration** (`tests/integration/`): `PROBE-FILE`, `TRUENAME`, `DIRECTORY`, `ENSURE-DIRECTORIES-EXIST`, symlink resolution.
7. **Concurrency:** multiple threads calling `NAMESTRING` on shared pathname to exercise CAS path.
8. **ANSI test suite:** `ansi-test` pathnames section MUST pass.

---

## 5.8.12 Module Map

| Source file | Contents |
|-------------|----------|
| `crates/bliss-rt/src/pathname.rs` | D5.30 `Pathname` struct, D5.31 `LogicalPathname`, construction, component accessors, namestring cache. |
| `crates/bliss-rt/src/pathname_parse.rs` | A5.11 POSIX parsing, tilde expansion, `PARSE-NAMESTRING` entry point. |
| `crates/bliss-rt/src/pathname_print.rs` | A5.12 namestring reconstruction, `ENOUGH-NAMESTRING`. |
| `crates/bliss-rt/src/pathname_logical.rs` | Logical pathname parsing, D5.32 translation table, A5.13 `TRANSLATE-LOGICAL-PATHNAME`. |
| `crates/bliss-rt/src/pathname_merge.rs` | A5.14 `MERGE-PATHNAMES`, `PATHNAME-MATCH-P`, `TRANSLATE-PATHNAME`. |
| `crates/bliss-rt/src/pathname_fs.rs` | `PROBE-FILE`, `TRUENAME`, `DIRECTORY`, `ENSURE-DIRECTORIES-EXIST` — thin wrappers around POSIX `libc` calls. |
| `crates/bliss-stdlib/src/pathnames.lisp` | CL-level convenience functions, `WITH-OPEN-FILE` pathname integration, user-facing `WILD-PATHNAME-P`, `PATHNAME-HOST` etc. accessor wrappers. |
