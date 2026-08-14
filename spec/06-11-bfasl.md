# §6.11 — Bliss FASL (`.bfasl`) Portable Compiled-Artifact Format

Bliss FASL files (`.bfasl`) are the unit of *ahead-of-time compiled, loadable
code* — the closest analogue to a JVM classfile: a versioned, verifiable
compiled unit that sits **below whole-heap images** (§7) and **above source**.
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
| 11 | `TOPLEVEL_FORMS` | serialized non-`defun` top-level forms to evaluate at load time, in order (e.g. `defvar`, `defmacro`, side-effecting forms). |

The bytecode instruction encoding is the serialization of the §4.4.3
`BytecodeFunction` instruction set; constant/symbol operands are pool indices.

## 6.11.3 Loading Semantics

1. Read and verify the header (R6.63/R6.64): magic, version major match, checksum.
2. Read the `SYMBOLS`/`PACKAGES` sections and intern them (R6.66), producing the
   local index→object mapping for this load.
3. Materialize the `CONSTANT_POOL` (immediates, strings, structural objects) on
   the GC heap.
4. For each `FUNCTIONS` entry, reconstruct the `BytecodeFunction`, register it,
   and install the function object into the named symbol's function cell (R6.67);
   attach its `SOURCE_MAP`/`DEBUG` entry.
5. Evaluate `TOPLEVEL_FORMS` in order.
6. If a matching `CACHED_T1` section is present (R6.65), install its native code
   as the function's T1 entry (still revocable by deopt); otherwise the function
   starts at T0 and promotes normally.

Loading is **not** transactional across functions by default: functions installed
before an error remain installed (matching CL `load`), but the loader MUST signal
on the first structural error rather than continue past corruption.

## 6.11.4 Interaction With Other Subsystems

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

## 6.11.5 Verification & Security

- The loader MUST bounds-check every section length against the file size (no
  read past EOF) and MUST validate the checksum before materializing objects.
- The `flags.signed` bit + an optional trailing signature section are reserved
  for a future signed-FASL policy; unsigned loading is the default.
- A `.bfasl` from an untrusted source is code; loading it executes
  `TOPLEVEL_FORMS`. Trust is the caller's responsibility, exactly as for `load`
  of source.
