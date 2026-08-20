# §6.11 — Bliss FASL (`.bfasl`) Portable Compiled-Artifact Format

Bliss FASL files (`.bfasl`) are the unit of *ahead-of-time compiled, loadable
code*.  The outer `.bfasl` container is the file-level envelope; its canonical
portable code payload is a **Bliss Bytecode Unit** (`BBU`), the closest analogue
to a JVM classfile: a versioned, verifiable compiled unit that sits **below
whole-heap images** (§7) and **above source**.
ASDF `compile-op` writes one `.bfasl` per source file under the output-
translation root (`~/.cache/bliss/asdf/<implementation-version>/`, R6.47);
`load-op` loads it. The format is defined here so that ASDF compile/load
(bliss-lb6.5), tiered runtime installation (bliss-jtc.3), and deopt metadata
(bliss-jtc.11) harden around a stable artifact rather than ad-hoc compiled
objects.

**Portability principle.** A `.bfasl` is *architecture-neutral*: its code
sections are portable Bliss **bytecode** (the T0/§4.4.3 representation), never
raw native machine code as a required payload. Loading on any platform installs
the bytecode (executable at T0) and lets the tiered system recompile to local
T1/T2 (§4.4). A `.bfasl` MAY additionally carry a platform-tagged **cached T1**
section as an optimization; it is used only when the loader's platform/ABI tag
matches exactly and is otherwise ignored. This follows the CLISP model
(§0 overview): FASLs are bytecode; native code is never portable.

## 6.11.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R6.60 | A `.bfasl` file MUST begin with the 8-byte header: magic `b"BLISSFAS"` (bytes `42 4C 49 53 53 46 41 53`) is not used; the magic is the 6 bytes `b"BFASL\0"` followed by a `u16` little-endian `format_version`. | MUST |
| R6.61 | The header MUST record: `format_version` (u16), `flags` (u8), `platform` (u8: 0 = portable/arch-neutral, nonzero = a registered native-target id for cached-T1 sections), `target_abi` (u16, 0 when portable), a `content_hash` (u64) over the source + declared dependency identities for cache invalidation, and a `checksum` (u32) over all header-and-section bytes. | MUST |
| R6.62 | The body MUST be a sequence of length-prefixed **sections**, each `{ kind: u16, length: u32, bytes }`. Unknown section kinds MUST be skippable by length (forward compatibility). | MUST |
| R6.63 | The loader MUST reject a file whose magic does not match (`bad-magic`), whose `format_version` major component differs from the running runtime's (`version-mismatch`), whose length is truncated (`truncated`), or whose `checksum` does not verify (`bad-checksum`). Rejection MUST be a signalled, catchable error — never a crash or silent partial load. | MUST |
| R6.64 | Version compatibility policy: `format_version` is `major<<8 | minor`. A loader MUST load any file with the same `major` and a `minor` ≤ its own (older minor readable). A differing `major` MUST be rejected. Portable (`platform = 0`) code sections MUST always be loadable on every platform of a matching `major`. | MUST |
| R6.65 | A cached-T1 section (`platform ≠ 0`) MUST be used only when both `platform` and `target_abi` equal the loader's; otherwise it MUST be ignored and the portable bytecode used instead. | MUST |
| R6.66 | Symbol and package references in the constant pool MUST be stored **by name** (package-qualified), not by process-local index, so identity is reconstructed by interning on load (see §1.7). | MUST |
| R6.67 | Loading a `.bfasl` MUST install each defined function into its symbol's function cell as the shared runtime function object (§1.11, bliss-jtc.6.8), so loaded code participates in tiering (counters, promotion) identically to interpreted definitions. | MUST |
| R6.68 | A `.bfasl` MUST carry a **source/debug map** section: for each function, its name, source pathname, and top-level form position, so backtraces and the debugger (§6.4) resolve loaded code to source. This metadata MUST survive a load in a fresh process. | MUST |
| R6.69 | `compile-op` output MUST be reproducible: compiling identical source with identical compiler settings MUST produce byte-identical `.bfasl` (modulo the `content_hash`/timestamps, which are excluded from reproducibility by being derived only from inputs). | SHOULD |
| R6.70 | Cache invalidation: a build tool (ASDF) MUST treat a `.bfasl` as stale when its `content_hash` does not match a freshly computed hash of the current source + dependency identities, and recompile. | MUST |
| R6.71 | New portable `.bfasl` writers MUST emit a `BYTECODE_UNIT` section containing a Bliss Bytecode Unit (`BBU`) for the source file's macroexpanded executable content. Legacy/decomposed sections MAY be emitted in addition, but the `BBU` is authoritative when present. [S6] | MUST |
| R6.72 | A `BBU` MUST contain bytecode lowered from macroexpanded Lisp forms, not raw source forms as the executable representation. Source forms MAY appear only in debug or provenance records. [S6] | MUST |
| R6.73 | A `BBU` MUST have its own bytecode-format version independent of the outer `.bfasl` container version, so loader changes and bytecode instruction-set changes can evolve separately. [S6] | MUST |
| R6.74 | All function bodies, load-time thunks, top-level side-effecting forms, and macro definitions that must run at load time MUST be represented as bytecode functions referenced from an ordered load plan. [S6] | MUST |
| R6.75 | The `BBU` constant pool MUST be a single indexed pool shared by all functions in the unit. Pool entries MUST be canonicalized within the unit so equal strings, symbols, package names, numeric literals, pathnames, and structural constants have one stable index. [S6] | SHOULD |
| R6.76 | The verifier MUST validate a `BBU` before installing any contained function: magic, version, table bounds, constant-pool tags, function signatures, branch targets, stack heights, local-slot accesses, unwind ranges, load-plan references, and source-map references. [S6] | MUST |
| R6.77 | `BBU` indices MUST be file-local unsigned integers; no serialized field may contain process-local pointers, heap addresses, symbol ids, package ids, or native-code addresses. [S6] | MUST |
| R6.78 | Bytecode version 1.0 MUST define a complete opcode table with explicit operands and stack effects; a loader MUST reject any opcode not defined for the unit's accepted `bytecode_version`. [S6] | MUST |
| R6.79 | `EVAL-WHEN` semantics MUST be resolved by `compile-file` before final load-plan emission: `:compile-toplevel` forms run in the compilation environment, `:load-toplevel` forms produce ordered `BBU` load actions, and `:execute` controls ordinary evaluation/compilation contexts. [S6] | MUST |

## 6.11.2 File Structure

```text
Offset  Size   Field
  0       6    magic          = b"BFASL\0"
  6       2    format_version : u16  (major<<8 | minor), little-endian
  8       1    flags          : u8   (bit0: has cached-T1; bit1: signed; …)
  9       1    platform       : u8   (0 = portable; else native-target id)
 10       2    target_abi     : u16  (0 when portable)
 12       8    content_hash   : u64  (source + dependency identities)
 20       4    section_count  : u32
 24       …    sections[]
  …       4    checksum       : u32  (over bytes [0 .. checksum_offset))
```

Each **section** is:

```text
kind   : u16       // SectionKind
length : u32       // byte length of `bytes`
bytes  : [u8; length]
```

### Section kinds

| Kind | Name | Contents |
|------|------|----------|
| 1 | `FUNCTIONS` | one entry per compiled top-level function: name-ref, arity, bytecode instruction stream, `n_locals`/`max_stack`, handler/restart tables, param layout. |
| 2 | `CONSTANT_POOL` | the literal pool: immediates inline; strings/bit-vectors as bytes; symbols/packages **by name** (R6.66); conses/vectors structurally (recursive pool refs). |
| 3 | `SYMBOLS` | package-qualified names referenced by the constant pool and code. |
| 4 | `PACKAGES` | package names + nicknames the unit defines or references. |
| 5 | `SOURCE_MAP` | per-function `{ name, source_path, form_index, line }` (R6.68). |
| 6 | `DEBUG` | optional local-variable names, lambda-list, docstring. |
| 7 | `STACKMAPS` | per-safepoint GC stack maps + unwind tables (§2.4, bliss-jtc.4) for any cached-T1 code and for T0 safepoints. |
| 8 | `RELOCATIONS` | relocation records for cached-T1 code (entry patch sites, c2i/i2c thunks). |
| 9 | `DEPENDENCIES` | names + `content_hash`es of `.bfasl`s this unit requires (load order, staleness). |
| 10 | `CACHED_T1` | platform-tagged native code (R6.65); ignored on mismatch. |
| 11 | `TOPLEVEL_FORMS` | legacy source-form payload, accepted only when `BYTECODE_UNIT` is absent. New writers MUST NOT emit it; it is never a fallback for an invalid or incomplete BBU. |
| 12 | `BYTECODE_UNIT` | canonical classfile-like `BBU` payload: constant pool, bytecode functions, load plan, verification metadata, source/debug maps, and dependency records. |

The bytecode instruction encoding is the serialization of the §4.4.3
`BytecodeFunction` instruction set; constant/symbol operands are pool indices.

## 6.11.3 Bliss Bytecode Unit (`BBU`)

A `BBU` is a self-contained, architecture-neutral bytecode object stored in a
`BYTECODE_UNIT` section.  The outer `.bfasl` says "this is a loadable compiled
artifact for this source/dependency identity"; the `BBU` says "these are the
portable instructions, constants, functions, and load-time actions to install".

All multi-byte integers in a `BBU` are little-endian.  Variable-length byte
sequences use `u32 length` followed by exactly that many bytes.  Text fields are
UTF-8 unless the field explicitly says otherwise.  Counts and indices are `u32`
unless a table below gives a narrower type.

### 6.11.3.1 Unit Header

```text
Offset  Size   Field
  0       4    magic              = b"BBU\0"
  4       2    bytecode_version   : u16  (major<<8 | minor)
  6       2    verifier_version   : u16
  8       4    unit_flags         : u32
 12       4    constant_count     : u32
 16       4    function_count     : u32
 20       4    load_action_count  : u32
 24       4    aux_table_count    : u32
 28       4    source_file_ref    : u32  (constant-pool index, or 0xffffffff)
 32       8    expanded_hash      : u64  (macroexpanded forms + compiler policy)
 40       ...  constant_pool[constant_count]
  ...     ...  functions[function_count]
  ...     ...  load_actions[load_action_count]
  ...     ...  aux_tables[aux_table_count]
```

`bytecode_version` tracks the opcode set and operand encoding.  A loader MUST
accept only bytecode with the same major version and a minor version no newer
than its own bytecode interpreter/compiler supports.  `verifier_version` tracks
abstract-interpretation rules for stack maps, type slots, and unwind metadata;
a loader MAY reject an older verifier version if the unit lacks metadata needed
by the current runtime's GC or deoptimizer.

`expanded_hash` is the reproducibility key for the macroexpanded input to the
bytecode compiler.  It is distinct from the outer `content_hash`: `content_hash`
answers "is this cache entry stale for this source and dependency graph?",
whereas `expanded_hash` answers "did macro expansion plus compiler policy
produce the same executable input?".

The four counted tables always appear in the order shown above.  This keeps a
`BBU` streaming-readable while still allowing unknown auxiliary tables to be
skipped by length.

### 6.11.3.2 Constant Pool

The constant pool is an indexed table.  Index `0xffffffff` is reserved as
`NO_INDEX`; all other indices refer to entries in `[0, constant_count)`.

Each entry is:

```text
tag     : u8
payload : tag-specific bytes
```

| Tag | Name | Payload |
|-----|------|---------|
| 0 | `Nil` | empty; canonical `NIL` |
| 1 | `T` | empty; canonical `T` |
| 2 | `Fixnum` | `i64 value`, range-checked against target fixnum width on load |
| 3 | `Bignum` | `u8 sign`, `u32 byte_len`, little-endian magnitude bytes |
| 4 | `Ratio` | `u32 numerator_ref`, `u32 denominator_ref` |
| 5 | `SingleFloat` | IEEE-754 `f32` bits |
| 6 | `DoubleFloat` | IEEE-754 `f64` bits |
| 7 | `Character` | Unicode scalar value as `u32` |
| 8 | `String` | `u32 byte_len`, UTF-8 bytes |
| 9 | `Bytes` | `u32 byte_len`, raw bytes for specialized arrays/bit-vectors |
| 10 | `Package` | `u32 name_ref`, `u32 nickname_count`, `u32 nickname_refs[]` |
| 11 | `Symbol` | `u32 package_ref`, `u32 name_ref`, `u8 symbol_kind` |
| 12 | `Keyword` | `u32 name_ref` |
| 13 | `Cons` | `u32 car_ref`, `u32 cdr_ref` |
| 14 | `Vector` | `u32 element_count`, `u32 element_refs[]` |
| 15 | `Pathname` | `u32 host_ref`, `u32 device_ref`, `u32 directory_ref`, `u32 name_ref`, `u32 type_ref`, `u32 version_ref` |
| 16 | `FunctionRef` | `u32 function_index` |
| 17 | `LayoutRef` | `u32 class_symbol_ref`, `u32 layout_hash_ref` |
| 18 | `LoadTimeCell` | `u32 producing_action_index` |

`Symbol.symbol_kind` is `0 = interned`, `1 = uninterned`, `2 = gensym`,
`3 = external`.  Interned and external symbols are reconstructed through their
package/name pair (R6.66).  Uninterned and gensym symbols are fresh per load,
but references to the same pool entry within a unit MUST resolve to the same
fresh symbol object.

Structural constants (`Cons`, `Vector`, `Pathname`) are materialized after
primitive constants.  Cycles are not allowed in the portable constant pool; a
compiler that needs circular literal structure MUST emit load-time bytecode to
construct it and store the result in a `LoadTimeCell`.

### 6.11.3.3 Function Table

Each function record serializes one §4.4.3 `BytecodeFunction`.

```text
name_ref        : u32  // Symbol, FunctionRef owner, or NO_INDEX for anonymous
lambda_list_ref : u32  // debug/provenance constant, or NO_INDEX
doc_ref         : u32  // documentation string, or NO_INDEX
flags           : u32
arity_min       : u16
arity_max       : u16  // 0xffff means &rest / open arity
n_locals        : u16
max_stack       : u16
code_len        : u32
code            : [u8; code_len]
literal_count   : u32
literal_refs    : u32[literal_count]
handler_count   : u32
handlers        : HandlerRecord[handler_count]
pc_info_count   : u32
pc_info         : PcInfoRecord[pc_info_count]
debug_ref       : u32  // DebugRecord table index or NO_INDEX
```

`literal_refs` maps compact per-function literal operands to unit constant-pool
indices.  Bytecode instructions SHOULD use the smallest operand width that can
address this per-function literal table; this keeps bytecode compact while the
unit still has one canonical constant pool.

Function flags:

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `named` | Installable in a symbol function cell |
| 1 | `macro_function` | Function is the expander for a load-time macro definition |
| 2 | `compiler_macro_function` | Function is a compiler-macro expander |
| 3 | `load_time_thunk` | Function is invoked by the load plan, not directly installed |
| 4 | `never_inline` | Preserve call boundary for declarations/debugging |
| 5 | `contains_eval` | Function may observe dynamic compilation environment |
| 6 | `requires_full_debug` | Debug metadata is required for correct restart/deopt behavior |

`HandlerRecord`:

```text
kind       : u8   // cleanup, block, catch, tagbody, handler-bind, restart
start_pc   : u32
end_pc     : u32
target_pc  : u32
data_ref   : u32  // condition type, block name, tag, restart name, or NO_INDEX
stack_depth: u16
```

`PcInfoRecord`:

```text
pc              : u32
source_span_ref : u32
stack_map_ref   : u32
flags           : u16  // safepoint, back-edge, deopt-point, type-check, call-site
```

### 6.11.3.4 Bytecode Instruction Stream

The `code` bytes in each function are Bliss T0 bytecode (§4.4.3), emitted after
macro expansion and special-form lowering.  The BBU bytecode encoding is:

```text
opcode  : u8
operands: opcode-specific immediate bytes
```

Operands are one of:

| Operand | Encoding | Meaning |
|---------|----------|---------|
| `u8/u16/u32` | little-endian fixed width | counts, local slots, arity |
| `s32` | little-endian signed | relative branch delta |
| `pc` | `u32` byte offset from start of this function's `code` |
| `lit` | unsigned index into this function's `literal_refs` |
| `fn` | unsigned index into this BBU's function table |
| `cp` | unsigned index into this BBU's constant pool |

Bytecode version 1.0 defines this portable opcode set.  Stack effects are shown
as `before -> after`; `v*` means `argc` argument values in left-to-right order
with the last argument nearest the top of stack.

| Opcode | Mnemonic | Operands | Stack effect | Semantics |
|--------|----------|----------|--------------|-----------|
| `0x00` | `NOP` | none | `- -> -` | No operation; allowed only at verified opcode boundaries. |
| `0x01` | `CONST` | `lit` | `- -> value` | Push literal from this function's `literal_refs`. |
| `0x02` | `LOAD_LOCAL` | `u16 slot` | `- -> value` | Push lexical local slot. |
| `0x03` | `STORE_LOCAL` | `u16 slot` | `value -> value` | Store lexical local and leave the stored value. |
| `0x04` | `LOAD_CELL` | `u16 slot` | `- -> value` | Push value from a captured lexical cell. |
| `0x05` | `STORE_CELL` | `u16 slot` | `value -> value` | Store captured lexical cell and leave the stored value. |
| `0x06` | `MAKE_CELL` | `u16 slot` | `value -> value` | Box a local for closure capture. |
| `0x07` | `LOAD_SPECIAL` | `cp symbol` | `- -> value` | Read dynamic/special binding, then symbol value cell. |
| `0x08` | `STORE_SPECIAL` | `cp symbol` | `value -> value` | Write current dynamic binding or symbol value cell. |
| `0x09` | `LOAD_FUNCTION` | `cp symbol-or-fn` | `- -> fn` | Push function object from function cell or nested function ref. |
| `0x0a` | `STORE_FUNCTION` | `cp symbol` | `fn -> fn` | Install function object in a symbol function cell. |
| `0x0b` | `MAKE_CLOSURE` | `fn function_index`, `u16 capture_count`, `u16 slots[]` | `- -> fn` | Create closure over captured cells/locals. |
| `0x0c` | `CALL` | `u16 argc` | `fn v* -> values` | Call function value using normal CL call protocol. |
| `0x0d` | `CALL_NAMED` | `cp symbol`, `u16 argc` | `v* -> values` | Resolve symbol function cell and call it. |
| `0x0e` | `TAIL_CALL` | `u16 argc` | `fn v* -> values` | Tail-call function value, replacing current frame. |
| `0x0f` | `TAIL_CALL_NAMED` | `cp symbol`, `u16 argc` | `v* -> values` | Tail-call named function, replacing current frame. |
| `0x10` | `RETURN` | `u16 nvalues` | `values -> caller` | Return current primary plus secondary values. |
| `0x11` | `POP` | none | `value -> -` | Discard top value and clear secondary values. |
| `0x12` | `DUP` | none | `value -> value value` | Duplicate top value. |
| `0x13` | `VALUES` | `u16 nvalues` | `v* -> primary` | Set multiple-value buffer from stack values; push primary or `NIL`. |
| `0x14` | `CLEAR_VALUES` | none | `primary -> primary` | Clear secondary values in single-value contexts. |
| `0x15` | `TAKE_VALUES` | `u16 dst_slot`, `u16 count` | `primary -> -` | Store primary/secondary values into locals, `NIL`-padding as needed. |
| `0x16` | `VALUES_TO_LIST` | none | `primary -> list` | Materialize current multiple values as a list. |
| `0x17` | `LIST_TO_VALUES` | none | `list -> primary` | Set multiple-value buffer from list elements. |
| `0x18` | `BR` | `pc target` | `- -> -` | Unconditional branch. |
| `0x19` | `BR_IF_FALSE` | `pc target` | `value -> -` | Branch if value is `NIL`. |
| `0x1a` | `BR_IF_TRUE` | `pc target` | `value -> -` | Branch if value is not `NIL`. |
| `0x1b` | `TYPE_CHECK` | `cp type_spec` | `value -> value` | Enforce `THE`/declared type, signalling a catchable type error. |
| `0x1c` | `BIND_SPECIAL` | `cp symbol` | `value -> -` | Push dynamic binding for symbol for the current dynamic extent. |
| `0x1d` | `UNBIND_SPECIAL` | `u16 count` | `- -> -` | Pop `count` dynamic bindings. |
| `0x1e` | `PUSH_BLOCK` | `u32 block_id`, `cp name`, `pc resume`, `u16 sp` | `- -> -` | Establish `BLOCK` exit metadata. |
| `0x1f` | `RETURN_FROM` | `u32 block_id` | `value -> transfer` | Transfer to matching block, running intervening cleanups. |
| `0x20` | `PUSH_CATCH` | `pc resume`, `u16 sp` | `tag -> -` | Establish `CATCH` for dynamic tag. |
| `0x21` | `THROW` | none | `tag value -> transfer` | Transfer to matching catch, running intervening cleanups. |
| `0x22` | `PUSH_TAGBODY` | `u32 tagbody_id`, `u16 sp` | `- -> -` | Establish `TAGBODY` target set. |
| `0x23` | `GO` | `u32 tagbody_id`, `pc target` | `- -> transfer` | Transfer to tagbody target, running intervening cleanups. |
| `0x24` | `PUSH_UNWIND` | `pc cleanup`, `u16 sp` | `- -> -` | Establish `UNWIND-PROTECT` cleanup. |
| `0x25` | `ENTER_CLEANUP_NORMAL` | `pc cleanup`, `pc resume` | `value -> -` | Run cleanup after normal protected-form completion. |
| `0x26` | `CLEANUP_RETURN` | none | `- -> value/transfer` | Finish cleanup and resume saved normal or non-local continuation. |
| `0x27` | `POP_HANDLER` | none | `- -> -` | Pop most recent block/catch/tagbody/unwind handler. |
| `0x28` | `PUSH_HANDLER_CASE` | `u32 table`, `u16 sp` | `- -> -` | Establish `HANDLER-CASE` clauses from auxiliary handler table. |
| `0x29` | `POP_HANDLER_CASE` | none | `- -> -` | Disestablish current `HANDLER-CASE`. |
| `0x2a` | `PUSH_HANDLER_BIND` | `u32 table` | `- -> -` | Establish `HANDLER-BIND` dynamic handlers. |
| `0x2b` | `POP_HANDLER_BIND` | none | `- -> -` | Disestablish current `HANDLER-BIND`. |
| `0x2c` | `PUSH_RESTART_CASE` | `u32 table`, `pc resume`, `u16 sp` | `- -> -` | Establish restart clauses. |
| `0x2d` | `POP_RESTART_CASE` | none | `- -> -` | Disestablish current restarts. |
| `0x2e` | `INVOKE_RESTART` | `u16 argc` | `restart v* -> values` | Invoke restart object/function through condition system. |
| `0x2f` | `LOAD_TIME_VALUE` | `cp load_time_cell` | `- -> value` | Push value computed by load plan for a `LOAD-TIME-VALUE`. |
| `0x30` | `ALLOC_CONS` | none | `car cdr -> cons` | Allocate cons. |
| `0x31` | `ALLOC_VECTOR` | `u16 count` | `v* -> vector` | Allocate simple vector from stack values. |
| `0x32` | `SET_CAR` | none | `cons value -> value` | Mutate cons car. |
| `0x33` | `SET_CDR` | none | `cons value -> value` | Mutate cons cdr. |
| `0x34` | `GET_SLOT` | `cp slot_name` | `object -> value` | Generic instance slot read. |
| `0x35` | `SET_SLOT` | `cp slot_name` | `object value -> value` | Generic instance slot write. |
| `0x36` | `PUSH_ENV` | none | `- -> -` | Enter heap lexical environment frame for captured bindings. |
| `0x37` | `POP_ENV` | none | `- -> -` | Leave heap lexical environment frame. |
| `0x38` | `SAFEPOINT` | none | `- -> -` | Poll GC, interrupts, and scheduler; publish roots by stack map. |
| `0x39` | `BACK_EDGE` | `pc loop_header` | `- -> -` | Count loop edge, poll safepoint, and trigger OSR if eligible. |
| `0x3a` | `DEBUG_TRAP` | `u32 reason` | `- -> -` | Debugger/profiler trap; no semantic effect when disabled. |
| `0x3b` | `UNREACHABLE` | none | `- -> signal` | Verified unreachable path; signals if executed. |

The verifier computes stack height and stack-map liveness from opcode stack
effects.  Every forward and backward branch target MUST point at an opcode
boundary.  Every backward branch MUST target or pass through a `BACK_EDGE` or
equivalent safepoint-marked PC so the tiering and interruption rules in §4.4.3.5
remain enforceable after loading.

This opcode set is sufficient for ANSI-level semantics because macros and
special forms are handled before or during lowering, not by adding one opcode
per Lisp operator.  `CALL`/`CALL_NAMED` are the escape path for ordinary
functions, generic functions, CLOS dispatch, arithmetic, sequence operations,
conditions, pathnames, streams, and implementation-provided builtins.  The
dedicated opcodes cover only the semantic machinery that must be visible to the
verifier, GC, unwinder, tiering engine, and deoptimizer: lexical slots, dynamic
bindings, closures, multiple values, non-local control flow, cleanup/restart
state, load-time cells, branches, and safepoints.

### 6.11.3.5 Load Plan

A `BBU` does not execute arbitrary source text at load time.  It executes an
ordered load plan whose actions reference bytecode functions and constants.

```text
action_kind : u8
flags       : u8
arg0        : u32
arg1        : u32
arg2        : u32
```

| Kind | Name | Arguments |
|------|------|-----------|
| 1 | `EnsurePackage` | `arg0 = Package constant`, `arg1 = Vector of use-list package-name Strings`, `arg2 = Vector of exported bare-name Strings` |
| 2 | `InternSymbol` | `arg0 = Package constant`, `arg1 = bare-name String` |
| 3 | `InstallFunction` | `arg0 = function index`, `arg1 = Symbol constant` |
| 4 | `InstallMacro` | `arg0 = function index`, `arg1 = Symbol constant` |
| 5 | `InstallCompilerMacro` | `arg0 = function index`, `arg1 = Symbol constant` |
| 6 | `BindSpecial` | `arg0 = Symbol constant`, `arg1 = value constant/function` |
| 7 | `EvalThunk` | `arg0 = load-time thunk function index` |
| 8 | `SetLoadTimeCell` | `arg0 = LoadTimeCell constant`, `arg1 = thunk function index` |
| 9 | `ProvideFeature` | `arg0 = Symbol/String constant` |
| 10 | `RequireDependency` | `arg0 = DependencyRecord index` |

Top-level forms are compiled into `load_time_thunk` functions and scheduled by
`EvalThunk` or more specific install actions.  For example, a source `defun`
usually becomes a bytecode function plus `InstallFunction`; a `defmacro` becomes
a bytecode expander plus `InstallMacro`; a side-effecting top-level `(pushnew
:x *features*)` becomes an `EvalThunk`.  `EVAL-WHEN` controls whether the
compiler runs a form during compilation, records a load action, both, or neither.

`EnsurePackage` is idempotent. It creates the package when absent, otherwise
adds the declared nicknames and use-list edges, then makes each exported name
external. An exported name already inherited through the use-list retains that
symbol's identity. `InternSymbol` makes a declared `:intern`/`:shadow` name
present after its package has been ensured. All referenced constants and action
arguments are verified before either action mutates the package registry.

`EVAL-WHEN` is resolved while building the load plan:

| Situation | `:compile-toplevel` | `:load-toplevel` | `:execute` |
|-----------|---------------------|------------------|------------|
| Top-level `compile-file` | evaluate immediately in the compilation environment | emit ordered load actions/thunks into the `BBU` | no direct effect unless also in compile/load set |
| Non-top-level compilation | no compile-time side effect | no load-plan action | compile the body as ordinary runtime bytecode |
| `load` of source or REPL/eval | not applicable | not applicable | evaluate the body normally |

Thus a top-level macro definition that must affect later forms in the same file
is executed during compilation when `:compile-toplevel` applies, and is also
serialized as an `InstallMacro` load action when `:load-toplevel` applies.  A
`LOAD-TIME-VALUE` form is compiled by adding a load-time thunk plus
`SetLoadTimeCell`; executable code then uses `LOAD_TIME_VALUE` to read that
cell.  The loader never re-runs macro expansion as part of executing a `BBU`.

The load plan executes in file order and is the unit's observable load-time
semantics.  Loader implementations MAY pre-materialize constants and verify all
functions before the first action, but they MUST NOT reorder actions with
observable effects.

### 6.11.3.6 Auxiliary Tables

Auxiliary tables may be embedded in the `BBU` or mirrored in outer `.bfasl`
sections for tools that want cheap access without parsing bytecode.

Each auxiliary table is framed as:

```text
table_kind : u16
flags      : u16
length     : u32
bytes      : [u8; length]
```

Unknown `table_kind` values MUST be skipped by `length`.

| Table | Required | Contents |
|-------|----------|----------|
| `SourceSpans` | MUST | source pathname ref, top-level form ordinal, byte offsets, line/column range |
| `DebugRecords` | SHOULD | local names, declaration summaries, docstrings, lambda-list, macroexpansion provenance |
| `StackMaps` | MUST | root bitmap/value-kind map for safepoint PCs and deopt PCs |
| `Dependencies` | MUST | required `.bfasl` identities, provided features, package dependencies, content hashes |
| `Policy` | SHOULD | compiler optimization/safety/debug/speed policy that influenced bytecode |
| `FunctionMetadata` | MUST for bytecode version 1.3+ | per-function `has_env`/`variadic` flags and parameter name, `VarLoc`, and primitive declared-type entries |

Debug records may include macroexpansion provenance as source spans or compact
forms for tooling, but the executable semantics remain the bytecode and load
plan.  A debugger that asks for macroexpanded source should reconstruct it from
debug metadata when present, not require it for loading.

`FunctionMetadata` is framed with `table_kind = 6`. Its payload begins with a
`u32 function_count`, followed in function-table order by `u8 has_env`, `u8
variadic`, `u16 parameter_count`, then entries `{u32 name_ref, u8 location_kind,
u16 slot, u8 declared_type}`. Location kind 0 names a checked local slot; kind
1 denotes a boxed binding and uses slot `0xffff`. Declared types 0, 1, and 2
mean `Any`, `Fixnum`, and `SingleFloat`. A variadic function's core
`lambda_list_ref` is executable binder metadata and MUST be present; it is not
a retained function body or source fallback.

## 6.11.4 Loading Semantics

1. Read and verify the header (R6.63/R6.64): magic, version major match, checksum.
2. If a `BYTECODE_UNIT` section is present, parse and verify its `BBU` header,
   constant pool, function table, bytecode streams, auxiliary tables, and load
   plan before installing anything from the unit (R6.71-R6.77).
3. Materialize the `BBU` constant pool (immediates, strings, structural objects)
   on the GC heap; intern package and symbol references by name (R6.66).
4. Reconstruct every `BytecodeFunction` from the `BBU` function table and attach
   its source/debug/stack-map metadata.
5. Execute the `BBU` load plan in order, installing functions into symbol
   function cells and running load-time thunks (R6.67/R6.74).
6. If no `BYTECODE_UNIT` section is present, a compatibility loader MAY read the
   decomposed `SYMBOLS`/`PACKAGES`/`CONSTANT_POOL`/`FUNCTIONS` sections or the
   legacy source-form `TOPLEVEL_FORMS` section using the same invariants. A BBU
   that is present but invalid or incomplete MUST be rejected; the loader MUST
   NOT execute `TOPLEVEL_FORMS` as a fallback.
7. If a matching `CACHED_T1` section is present (R6.65), install its native code
   as the function's T1 entry (still revocable by deopt); otherwise the function
   starts at T0 and promotes normally.

Loading is **not** transactional across functions by default: functions installed
before an error remain installed (matching CL `load`), but the loader MUST signal
on the first structural error rather than continue past corruption.

## 6.11.5 Interaction With Other Subsystems

- **Images (§7).** An image is a whole-heap snapshot; a `.bfasl` is a single
  compiled unit. Saving an image after loading `.bfasl`s captures the installed
  function objects. `.bfasl` loading MUST NOT require an image and MUST be usable
  from a bare runtime.
- **ASDF (R6.47/R6.48, bliss-lb6.5).** `compile-op` writes `.bfasl` to the
  output-translation root; `load-op` loads it; a system's `.bfasl`s carry
  `DEPENDENCIES` for correct load order and staleness. Files are compiled at T1
  minimum where the baseline compiler supports the forms, else portable bytecode.
- **Tiering (§4.4, bliss-jtc.3/jtc.10).** Loaded functions are ordinary shared
  function objects; their FnMeta counters drive T1/T2 promotion. A `CACHED_T1`
  section is a warm-start optimization, not a correctness requirement.
- **Deopt (§4.6, bliss-jtc.11).** `STACKMAPS`/`DEBUG` carry the metadata deopt
  needs to reconstruct interpreter frames from any installed compiled entry.
- **Cache invalidation (R6.70).** The `content_hash` binds a `.bfasl` to its
  source + dependency identities; a mismatch means recompile.

## 6.11.6 Verification & Security

- The loader MUST bounds-check every section length against the file size (no
  read past EOF) and MUST validate the checksum before materializing objects.
- The `BBU` verifier MUST run before any load-plan action mutates packages,
  symbols, global variables, function cells, macro cells, or load-time cells.
- The `flags.signed` bit + an optional trailing signature section are reserved
  for a future signed-FASL policy; unsigned loading is the default.
- A `.bfasl` from an untrusted source is code; loading it executes
  bytecode from the `BBU` load plan. Trust is the caller's responsibility,
  exactly as for `load` of source.
