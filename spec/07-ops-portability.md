# §7 Ops & Portability

This chapter specifies image persistence, deployment modes, platform
support tiers, configuration, build & release processes, versioning
policy, and logging/diagnostics infrastructure for the TorCL runtime.

---

## 7.1  Requirements

| ID | Requirement |
|----|-------------|
| R7.01 | The runtime MUST support saving the full heap state to a `.bimg` image file and restoring it to resume execution. |
| R7.02 | Image load MUST use `mmap(2)` for the heap section and complete in < 50 ms for a 64 MB image on tier-1 platforms. |
| R7.03 | The image loader MUST perform pointer relocation when the mapped base address differs from the original save address. |
| R7.04 | The image format MUST include a magic number, format version, platform tag, and checksum so that incompatible images are rejected with a clear error before any heap access. |
| R7.05 | TorCL MUST support a standalone executable deployment mode by prepending an image to the runtime binary. |
| R7.06 | TorCL MUST provide a shared-library (`libtorcl.so` / `libtorcl.dylib`) embedding mode with a stable C-ABI surface (see §8, R8.xx). |
| R7.07 | TorCL MUST provide a REPL/script mode that reads from stdin or a file path argument. |
| R7.08 | Linux x86-64 and macOS aarch64 MUST be tier-1 platforms: all tests pass, CI-gated, release binaries provided. |
| R7.09 | macOS x86-64 MUST be tier-2: CI runs, best-effort fixes, community-contributed binaries. |
| R7.10 | FreeBSD x86-64 MUST be tier-3: builds accepted, no CI, fixes accepted but not prioritised. |
| R7.11 | Configuration MUST be resolved in order: in-image defaults → environment variables → CLI flags, with later sources overriding earlier. |
| R7.12 | The build MUST be a Cargo workspace build reproducible on tier-1 platforms with a single `cargo build --release`. |
| R7.13 | Release artifacts MUST include source tarball, Linux x86-64 tarball, macOS aarch64 tarball, Homebrew formula, and Debian `.deb` package. |
| R7.14 | The C-ABI shared library MUST follow semantic versioning; breaking ABI changes bump the major version. |
| R7.15 | Image format changes MUST bump the image format version; the loader MUST reject images with a higher version than it supports. |
| R7.16 | The runtime MUST emit structured log output (JSON-lines) with configurable severity levels. |
| R7.17 | The runtime MUST emit a `perf-<pid>.map` file on Linux when JIT code is generated, enabling `perf report` symbol resolution. |
| R7.18 | GC and JIT subsystems MUST provide dedicated log channels that can be enabled independently. |
| R7.19 | The runtime SHOULD support loading an image from a read-only filesystem (e.g., container images) without requiring write access. |
| R7.20 | The image save operation MUST be atomic: either the complete image is written or the previous file is left untouched. |
| R7.21 | Startup mode selection MUST be deterministic: explicit CLI mode (`--eval`, `--load`, script path, `--bootstrap`, `--no-image`) overrides appended-image and default-image discovery. |
| R7.22 | The runtime MUST support a bootstrap-without-image operating profile that loads `lib/boot.lisp` from the source tree or installation prefix and exposes enough functionality to build and save a first image. |
| R7.23 | The runtime MUST support an immutable-image operating profile in which the mapped image remains read-only and all post-startup mutation occurs in freshly allocated writable regions. |
| R7.24 | Init-file discovery MUST be explicit and suppressible: in interactive startup with no explicit `--eval`, `--load`, or script path, the runtime MUST load the file named by `TORCL_INIT_FILE` if set, else `~/.torclrc` if present; `--no-init` MUST suppress both probes. |
| R7.25 | The standard bootstrap prelude MUST load before init-file processing and before executing `--eval`, `--load`, or a positional script unless `--no-bootstrap` is in effect; `--bootstrap` MAY remain as a compatibility alias for explicitly requesting this prelude-backed bootstrap profile. |
| R7.26 | `--no-bootstrap` MUST disable standard bootstrap-prelude loading and expose the raw evaluator/runtime state for the selected startup mode. |

---

## 7.2  Image Format (`.bimg`)

### 7.2.1  Overview

A `.bimg` file is a serialised snapshot of the TorCL runtime state.
It captures the heap, symbol table, package registry, and compiled
native code so that the runtime can resume from a saved continuation
without replaying bootstrap load sequences.  See §0 (section 5.4) for
the high-level design rationale.

### 7.2.2  File Layout

```text
Offset  Section              Size
──────  ───────────────────  ──────────────────────
0x0000  Header               128 bytes (fixed)
0x0080  Section Directory    N × 32 bytes
        ── page-aligned ──
        Heap Section         variable (page-multiple)
        Symbol Table         variable
        Package Registry     variable
        Compiled Code Cache  variable
        Relocation Table     variable
        Checksum Trailer     32 bytes
```

### 7.2.3  Header — D7.01

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0x00 | 8 | `magic` | `0x544F5243_4C494D47` (`"TORCLIMG"` in ASCII) |
| 0x08 | 4 | `format_version` | Monotonically increasing u32; see §7.8 |
| 0x0C | 4 | `flags` | Bit flags (see below) |
| 0x10 | 8 | `platform_tag` | Encoded arch + OS (see §7.2.4) |
| 0x18 | 8 | `original_base` | Virtual address at which the heap was mapped during save |
| 0x20 | 8 | `heap_size` | Byte length of the serialised heap section |
| 0x28 | 8 | `entry_continuation` | `TorclVal` of the continuation to resume on load |
| 0x30 | 4 | `section_count` | Number of entries in the section directory |
| 0x34 | 4 | `gc_generation` | GC generation counter at save time |
| 0x38 | 8 | `gc_metadata_offset` | Offset to GC remembered-set / mark bitmap snapshot |
| 0x40 | 8 | `save_timestamp` | Unix epoch seconds (informational, not security-critical) |
| 0x48 | 24 | `reserved` | Zero-filled; future use |
| 0x60 | 32 | `header_sha256` | SHA-256 of bytes 0x00..0x5F |

**Flag bits:**

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `COMPRESSED` | Heap section is zstd-compressed |
| 1 | `CODE_SIGNED` | Compiled code cache carries signatures |
| 2 | `READ_ONLY_SAFE` | Image was saved with no mutable state pending |
| 3–31 | Reserved | MUST be zero |

### 7.2.4  Platform Tag Encoding

```rust
/// Packed as: (arch: u32) << 32 | (os: u32)
pub fn platform_tag(arch: Arch, os: Os) -> u64 { ... }

enum Arch { X86_64 = 1, Aarch64 = 2 }
enum Os   { Linux = 1, MacOS = 2, FreeBSD = 3 }
```

The image loader MUST reject an image whose platform tag does not
match the running platform (R7.04).

### 7.2.5  Section Directory — D7.02

Each entry in the section directory:

| Offset | Size | Field |
|--------|------|-------|
| 0x00 | 4 | `section_type` — enum: Heap=1, Symbols=2, Packages=3, Code=4, Reloc=5, GcMeta=6, Settings=7 |
| 0x04 | 4 | `flags` — per-section flags (compression, alignment) |
| 0x08 | 8 | `file_offset` — byte offset from start of file |
| 0x10 | 8 | `size` — byte length in file (compressed size if applicable) |
| 0x18 | 8 | `uncompressed_size` — original size; equals `size` when uncompressed |

### 7.2.6  Heap Section

The heap section is a verbatim copy of the TorCL heap regions (§3)
serialised in region order.  Each region is page-aligned (4 KiB) in
the file to allow `mmap` with `MAP_FIXED` at the original base or
a relocated base.

When the `COMPRESSED` flag is set, the heap section is a single zstd
frame.  The loader decompresses into an anonymous mapping before
pointer relocation.

### 7.2.7  Symbol Table

A flat array of symbol entries:

```rust
struct ImageSymbol {
    name_offset: u32,   // offset into name string pool
    name_len: u16,
    package_id: u16,    // index into package registry
    value_cell: u64,    // relocated TorclVal
    function_cell: u64, // relocated TorclVal
    plist_cell: u64,    // relocated TorclVal
    flags: u32,         // exported, shadowing-imported, constant, etc.
}
```

A separate name-string pool follows the array; names are UTF-8, not
null-terminated.

### 7.2.8  Package Registry

```rust
struct ImagePackage {
    name_offset: u32,
    name_len: u16,
    nickname_count: u16,
    internal_symbols_start: u32, // index range in symbol table
    internal_symbols_count: u32,
    external_symbols_start: u32,
    external_symbols_count: u32,
}
```

Nicknames follow as a length-prefixed string list after the package
array.

### 7.2.9  Compiled Code Cache

Native code generated by T1/T2 compilers (§4) is saved as a sequence
of code blobs:

| Field | Size | Description |
|-------|------|-------------|
| `function_id` | 8 | Symbol table index of the owning function |
| `tier` | 1 | Compilation tier (1 or 2) |
| `code_size` | 4 | Machine-code byte length |
| `reloc_count` | 4 | Number of internal relocations |
| `code_bytes` | variable | Raw machine code |
| `reloc_entries` | variable | `(offset: u32, kind: u8, target: u64)` tuples |

On load, the code cache section is mapped as `PROT_READ | PROT_EXEC`
after relocation.

### 7.2.10  Relocation Table

The relocation table records every pointer within the serialised heap
that requires adjustment when the load base differs from
`original_base`.  Entries are delta-encoded offsets (sorted, u32
deltas) for compactness.

```text
reloc_entry ::= delta: u32   // byte offset from previous reloc site
```

When the gap between two consecutive relocatable pointers exceeds
2^32 − 2 bytes (≈ 4 GiB), a single u32 delta is insufficient.  The
escape encoding `delta = 0xFFFFFFFF` signals that the next 8 bytes
encode the actual delta as a little-endian u64:

```text
escape_entry ::= 0xFFFFFFFF: u32, extended_delta: u64
```

This supports arbitrarily large heaps while keeping the common-case
entry compact (4 bytes).  A delta value of `0xFFFFFFFF` MUST NOT be
used literally; all gaps of exactly 0xFFFFFFFF bytes MUST also use the
escape encoding.

**A7.01 — Pointer relocation algorithm:**

```text
1. offset_delta ← load_base − original_base
2. IF offset_delta = 0 THEN skip relocation
3. pos ← 0
4. FOR EACH entry IN relocation_table:
5.     delta ← read_u32(entry)
6.     IF delta = 0xFFFFFFFF THEN delta ← read_u64(entry + 4)   // escape
7.     pos ← pos + delta
8.     *(u64 *)(heap + pos) += offset_delta
9. Flush instruction cache for code regions
```

This runs in O(n) where n = number of relocatable pointers.  Typical
images have < 5 M entries; relocation completes in < 20 ms on tier-1
hardware.

### 7.2.11  Checksum Trailer

The final 32 bytes of the file contain a SHA-256 digest computed over
all preceding bytes.  The loader MUST verify this checksum before
mapping any heap data (R7.04).

### 7.2.12  Save Atomicity — A7.02

Image save writes to a temporary file (same directory, `.bimg.tmp`
suffix) and performs `fsync` + atomic `rename(2)` to the target path.
On failure, the temporary file is unlinked and the previous image is
preserved (R7.20).

---

## 7.3  Image Save / Load Operations

### 7.3.1  Save (`TORCL-EXT:SAVE-IMAGE`)

```lisp
(torcl-ext:save-image pathname &key (executable nil)
                                    (compression :none)  ; or :zstd
                                    (purify t))
;; Returns: pathname on success.
;; Signals TORCL-EXT:IMAGE-ERROR on failure.
```

`TORCL:SAVE-IMAGE` and `TORCL:IMAGE-ERROR` are deprecated compatibility
spellings for older migration code.

1. Trigger a full GC to compact the heap and clear the nursery.
2. If `purify` is true, promote all live objects to old-gen and
   discard nursery / survivor metadata.
3. Stop all threads except the saving thread (safepoint barrier).
4. Serialise sections in directory order.
5. Build relocation table by scanning all heap pointer fields.
6. Write header, sections, relocation table, and checksum (A7.02).
7. If `executable` is true, prepend the runtime binary (see §7.4.1).
8. Resume threads.

### 7.3.2  Load

Load is performed by the runtime startup code
(`crates/torcl-rt/src/startup.rs`).

1. Open image file, read and validate header (magic, version,
   platform tag, header SHA-256).
2. Verify trailing checksum (full-file SHA-256) over all bytes
   preceding the 32-byte trailer.  This MUST occur before any heap
   data is interpreted or relocated, so that corrupt or tampered
   images are rejected before processing (R7.04).
3. Read section directory.
4. `mmap` heap section with `MAP_PRIVATE`.
5. If load base ≠ `original_base`, execute relocation (A7.01).
6. Deserialise symbol table; reconstruct package registry.
7. Map compiled code cache as RX pages; apply code relocations.
8. Resume from `entry_continuation`.

Load from a read-only filesystem is supported because `MAP_PRIVATE`
creates copy-on-write pages; no write to the image file is needed
(R7.19).

---

## 7.4  Deployment Modes

### 7.4.1  Standalone Executable

A standalone executable is produced by concatenating:

```text
[ runtime ELF/Mach-O binary ] [ .bimg payload ] [ 8-byte offset ]
```

The final 8 bytes store the byte offset from the end of the file to
the start of the `.bimg` payload as a little-endian u64.  On startup,
the runtime checks whether it carries an appended image by reading its
own executable file:

```rust
fn find_appended_image(exe_path: &Path) -> Option<MappedImage> {
    let file = File::open(exe_path)?;
    let len = file.metadata()?.len();
    let trailer: [u8; 8] = read_at(&file, len - 8)?;
    let img_offset = u64::from_le_bytes(trailer);
    if img_offset == 0 || img_offset >= len - 8 {
        return None;
    }
    // Validate .bimg magic number at the computed offset before mmap.
    let magic: [u8; 8] = read_at(&file, img_offset)?;
    if u64::from_le_bytes(magic) != 0x544F5243_4C494D47 {
        return None; // Not a valid .bimg payload
    }
    mmap_image(&file, img_offset)
}
```

This enables single-file distribution of TorCL applications (R7.05).

### 7.4.2  Shared Library (Embedding)

`libtorcl` exposes a C-ABI interface (§8):

```c
torcl_ctx *torcl_init(const char *image_path, const torcl_opts *opts);
torcl_val  torcl_eval(torcl_ctx *ctx, const char *form);
void       torcl_destroy(torcl_ctx *ctx);
```

Multiple independent contexts MAY coexist in a single process.  Each
context owns its own heap, GC threads, and package registry (R7.06).

### 7.4.3  REPL / Script Mode

The `torcl` CLI binary operates in three sub-modes:

| Invocation | Behaviour |
|------------|-----------|
| `torcl` | Interactive REPL (see §6) |
| `torcl script.lisp` | Load and execute `script.lisp`, then exit |
| `torcl -e '(+ 1 2)'` | Evaluate expression, print result, exit |

The CLI uses the default image located via `TORCL_IMAGE_PATH` or the
compiled-in fallback path (R7.07).

---

## 7.5  Platform Matrix

### 7.5.1  Tier Definitions

| Tier | CI | Test Gate | Release Binaries | Bug Fix Policy |
|------|-----|-----------|-------------------|----------------|
| Tier-1 | Every PR & nightly | All tests MUST pass to merge | Official binaries each release | Blockers fixed before release |
| Tier-2 | Nightly only | Failures tracked, not blocking | Community or best-effort binaries | Fixes accepted, not prioritised |
| Tier-3 | None (manual) | Build-only verification | Source-only | Patches welcome |

### 7.5.2  Platform Assignments

| Platform | Architecture | Tier | Notes |
|----------|-------------|------|-------|
| Linux | x86-64 | 1 | Primary development target |
| macOS | aarch64 (Apple Silicon) | 1 | Full support including JIT W^X via `pthread_jit_write_protect_np` |
| macOS | x86-64 | 2 | Rosetta 2 testing accepted; native CI nightly |
| FreeBSD | x86-64 | 3 | Community-maintained; POSIX layer mostly shared with Linux |

### 7.5.3  Platform-Specific Concerns

**macOS aarch64:**
- JIT code emission requires alternating between writable and
  executable permissions using `pthread_jit_write_protect_np` (W^X
  enforcement); see §4.
- `mmap` `MAP_JIT` flag required for JIT pages.
- Code signing: ad-hoc signing (`codesign -s -`) for the standalone
  executable.

**FreeBSD:**
- `epoll` replaced by `kqueue` in the I/O subsystem (§2).
- `/proc/self/exe` replaced by `sysctl KERN_PROC_PATHNAME`.
- `perf-map` not applicable; DTrace probes provided instead.

---

## 7.6  Configuration System

### 7.6.1  Resolution Order

```text
in-image defaults  →  environment variables  →  CLI flags
(lowest priority)                              (highest priority)
```

(R7.11)

### 7.6.2  Environment Variables

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `TORCL_HEAP_SIZE` | Size (e.g. `512m`, `2g`) | `256m` | Maximum heap size |
| `TORCL_GC_THREADS` | Integer | CPU count / 2 | Number of concurrent GC worker threads |
| `TORCL_IMAGE_PATH` | Path | `$PREFIX/lib/torcl/torcl.bimg` | Path to the default boot image |
| `TORCL_LOG_LEVEL` | `error\|warn\|info\|debug\|trace` | `warn` | Global log severity threshold |
| `TORCL_GC_LOG` | `0` or `1` | `0` | Enable GC-specific structured log channel |
| `TORCL_JIT_LOG` | `0` or `1` | `0` | Enable JIT-specific structured log channel |
| `TORCL_NURSERY_SIZE` | Size | `2m` | Per-thread nursery (TLAB) size |
| `TORCL_TIER1_THRESHOLD` | Integer | `10` | Call count triggering T0→T1 promotion |
| `TORCL_TIER2_THRESHOLD` | Integer | `5000` | Call/back-edge count triggering T1→T2 promotion |
| `TORCL_PERF_MAP` | `0` or `1` | `1` (Linux) | Emit `/tmp/perf-<pid>.map` for JIT symbols |
| `TORCL_CODE_CACHE_SIZE` | Size | `64m` | Maximum compiled-code cache size |
| `TORCL_INIT_FILE` | Path | `~/.torclrc` fallback | Interactive init file override; only consulted when no explicit `--eval`, `--load`, or script path is selected |

### 7.6.3  CLI Flags

| Flag | Equivalent Env Var | Example |
|------|--------------------|---------|
| `--heap-size` | `TORCL_HEAP_SIZE` | `--heap-size 1g` |
| `--gc-threads` | `TORCL_GC_THREADS` | `--gc-threads 4` |
| `--image` | `TORCL_IMAGE_PATH` | `--image app.bimg` |
| `--log-level` | `TORCL_LOG_LEVEL` | `--log-level debug` |
| `--gc-log` | `TORCL_GC_LOG` | `--gc-log` |
| `--jit-log` | `TORCL_JIT_LOG` | `--jit-log` |
| `--eval`, `-e` | — | `-e '(print 42)'` |
| `--no-image` | — | Start with an empty heap (no CL environment loaded) |
| `--bootstrap` | — | Load `lib/boot.lisp` bootstrap sequence without a pre-existing image (see §0 section 3); provides enough CL to run `SAVE-IMAGE` |
| `--no-bootstrap` | — | Skip the standard bootstrap prelude and run with the raw evaluator/runtime state |
| `--no-init` | — | Suppress `TORCL_INIT_FILE` and `~/.torclrc` init-file loading |
| `--version` | — | Print version and exit |

### 7.6.4  Init-File Discovery and Suppression

Init-file discovery is part of configuration resolution for interactive
use and is governed by R7.24:

1. If `--no-init` is present, no init file is probed or loaded.
2. Otherwise, if `TORCL_INIT_FILE` is set, its path is used.
3. Otherwise, the runtime probes `~/.torclrc`.
4. Init-file discovery only occurs when startup remains in interactive
   mode; any explicit `--eval`, `--load`, or positional script path
   suppresses it.

If a selected init file cannot be read or evaluated, startup MUST fail
with a diagnostic naming the path and the phase (`read` or `eval`) that
failed.

### 7.6.5  Bootstrap Prelude Defaults

The standard bootstrap prelude is a startup-layer default rather than a
user init file:

- By default it loads for every CLI execution mode before init-file
  processing and before any user `--eval`, `--load`, or positional
  script payload (R7.25).
- `--no-bootstrap` suppresses this load completely and does not fall
  back to an ambient default (R7.26).
- `--bootstrap` remains valid as an explicit compatibility spelling for
  requesting startup in the prelude-backed bootstrap profile when no
  saved image is used.

### 7.6.6  In-Image Defaults

At save time, the current configuration values are serialised into a
dedicated Settings section (`section_type = Settings = 7` in the
section directory, D7.02).  The Settings section contains a
length-prefixed array of key–value pairs (both UTF-8 strings) encoding
the configuration keys listed in §7.6.2.  On load, the runtime reads
this section before environment variable and CLI resolution, so these
values act as the lowest-priority defaults, allowing application-
specific tuning to persist across restarts.

> **Note:** The header's `reserved` field (offset 0x48, 24 bytes) MUST
> remain zero-filled per D7.01 and MUST NOT be used for settings
> storage.

---

## 7.7  Build & Release Process

### 7.7.1  Cargo Workspace

The project is a Cargo workspace (see §0, section 3):

```toml
[workspace]
members = [
    "crates/torcl-rt",
    "crates/torcl-compiler",
    "crates/torcl",
]
```

A full build is:

```bash
cargo build --release          # all crates
cargo test  --release          # Rust unit + integration tests
./scripts/run-ansi-tests.sh    # CL ANSI test suite
```

### 7.7.2  CI Matrix

| Job | Platform | Trigger | Gate? |
|-----|----------|---------|-------|
| `build-linux-x86_64` | Linux x86-64 | Every PR, every push to `main` | Yes |
| `build-macos-arm64` | macOS aarch64 | Every PR, every push to `main` | Yes |
| `build-macos-x86_64` | macOS x86-64 | Nightly | No |
| `test-linux-x86_64` | Linux x86-64 | Every PR | Yes |
| `test-macos-arm64` | macOS aarch64 | Every PR | Yes |
| `ansi-test-linux` | Linux x86-64 | Nightly + release | Yes (release) |
| `lint` | Linux x86-64 | Every PR | Yes |
| `miri` | Linux x86-64 | Weekly | No |

CI uses GitHub Actions.  The `miri` job runs the GC and object model
under Miri for undefined-behaviour detection (weekly due to runtime
cost).

### 7.7.3  Release Artifacts

| Artifact | Contents | Produced By |
|----------|----------|-------------|
| `torcl-<ver>-src.tar.gz` | Full source tree | `git archive` |
| `torcl-<ver>-linux-x86_64.tar.gz` | `torcl` binary + default image + man page | CI |
| `torcl-<ver>-macos-arm64.tar.gz` | `torcl` binary + default image + man page | CI |
| `torcl-<ver>-macos-x86_64.tar.gz` | `torcl` binary + default image (best-effort) | CI |
| `torcl_<ver>_amd64.deb` | Debian package (binary + image + man page) | `cargo-deb` |
| `torcl.rb` | Homebrew formula (source build) | Release script |

### 7.7.4  Debian Package Layout

```text
/usr/bin/torcl
/usr/lib/torcl/torcl.bimg
/usr/lib/x86_64-linux-gnu/libtorcl.so.<major>
/usr/share/man/man1/torcl.1.gz
/usr/share/doc/torcl/copyright
```

### 7.7.5  Homebrew Formula

```ruby
class TorCL < Formula
  desc "Common Lisp implementation inspired by HotSpot"
  homepage "https://github.com/user/torcl"
  url "https://github.com/user/torcl/archive/refs/tags/v#{version}.tar.gz"
  depends_on "rust" => :build
  def install
    system "cargo", "build", "--release"
    bin.install "target/release/torcl"
    lib.install "target/release/libtorcl.dylib"
    # Build default image via the bootstrap sequence (see §0 section 3,
    # lib/boot.lisp).  --bootstrap loads the minimal runtime from
    # lib/boot.lisp without requiring a pre-existing image, providing
    # enough of the CL environment to execute SAVE-IMAGE.
    system bin/"torcl", "--bootstrap", "--eval",
           "(torcl-ext:save-image \"#{lib}/torcl/torcl.bimg\")"
  end
end
```

---

## 7.8  Versioning & Compatibility Policy

### 7.8.1  Semantic Versioning

The project follows [SemVer 2.0](https://semver.org/):

- **Major** — breaking changes to the C-ABI, image format, or CL
  package exports.
- **Minor** — new features, non-breaking additions.
- **Patch** — bug fixes only.

### 7.8.2  C-ABI Stability

The shared library soname encodes the major version:
`libtorcl.so.1`, `libtorcl.so.2`, etc.  Functions in the public C
header are annotated `TORCL_API` and MUST NOT change signature within
a major version (R7.14).

### 7.8.3  Image Format Version

The `format_version` field in the header (D7.01) is independent of the
release version:

The current format is **3**, which appends an `MCOD` block to the host registry:
a u32 record count followed by pairs of u64 method-id bits and saved callable
bits. Method ids are immediate meta-handles; callable pointers are relocated
through the heap map and rooted before startup allocations. Compiled bodies
are restored separately through the existing bytecode registry.

Format-2 images added the opaque mutex heap type (`0x19`). Its process-local
native pointer is cleared on save; see §13.9 for restart semantics. Format-1
and format-2 images remain readable; absent compiled-method associations keep
the prior interpreted-dispatch behavior.

| Compat Rule | Behaviour |
|-------------|-----------|
| `loader_version == image_version` | Load normally |
| `loader_version > image_version` | Load with backward-compat shim (if supported) |
| `loader_version < image_version` | Reject with `IMAGE-VERSION-MISMATCH` error (R7.15) |

The project SHOULD maintain backward compatibility for at least 2
prior image format versions.

---

## 7.9  Logging & Diagnostics

### 7.9.1  Structured Logging

All runtime log output is JSON-lines to stderr (R7.16):

```json
{"ts":"2025-07-14T12:00:00.123Z","level":"info","mod":"gc","msg":"major GC completed","pause_us":1420,"freed_mb":38}
```

Fields:

| Field | Type | Description |
|-------|------|-------------|
| `ts` | ISO-8601 | Timestamp |
| `level` | string | `error`, `warn`, `info`, `debug`, `trace` |
| `mod` | string | Module: `gc`, `jit`, `rt`, `reader`, `image`, … |
| `msg` | string | Human-readable message |
| `…` | any | Module-specific structured fields |

### 7.9.2  GC Log Channel

Enabled by `TORCL_GC_LOG=1` or `--gc-log` (R7.18).  Events:

| Event | Key Fields |
|-------|------------|
| `minor-gc-start` | `nursery_used`, `thread_id` |
| `minor-gc-end` | `copied_bytes`, `pause_us` |
| `major-gc-start` | `heap_used`, `trigger` (`threshold` / `explicit`) |
| `major-gc-mark-end` | `marked_objects`, `elapsed_us` |
| `major-gc-evacuate-end` | `moved_regions`, `freed_regions`, `pause_us` |
| `major-gc-end` | `heap_used_after`, `total_pause_us` |

### 7.9.3  JIT Log Channel

Enabled by `TORCL_JIT_LOG=1` or `--jit-log` (R7.18).  Events:

| Event | Key Fields |
|-------|------------|
| `compile-start` | `function`, `tier`, `byte_size` (source IR) |
| `compile-end` | `function`, `tier`, `code_bytes`, `elapsed_us` |
| `osr-entry` | `function`, `bci`, `from_tier`, `to_tier` |
| `deoptimize` | `function`, `reason`, `bci` |
| `code-cache-full` | `evicted_count`, `freed_bytes` |

### 7.9.4  Perf-Map for Linux `perf`

When `TORCL_PERF_MAP=1` (default on Linux), the JIT compiler writes
entries to `/tmp/perf-<pid>.map` in the standard format (R7.17):

```text
<hex_start> <hex_size> <symbol_name>
```

This allows `perf record -p <pid>` / `perf report` to resolve JIT
frame symbols.

### 7.9.5  DTrace Probes (macOS / FreeBSD)

On platforms without `perf`, the runtime registers USDT probes via
the `probe!` macro (Rust `usdt` crate):

| Probe | Arguments |
|-------|-----------|
| `torcl:gc:minor-start` | `thread_id` |
| `torcl:gc:minor-end` | `copied_bytes`, `pause_ns` |
| `torcl:gc:major-start` | `heap_used` |
| `torcl:gc:major-end` | `freed_bytes`, `pause_ns` |
| `torcl:jit:compile` | `function_name`, `tier`, `code_size` |
| `torcl:jit:deopt` | `function_name`, `reason` |

---

## 7.10  Error Handling

| Failure Mode | Detection | Recovery |
|-------------|-----------|----------|
| Corrupt image header | Magic / checksum mismatch | Reject with `IMAGE-CORRUPT` condition; fall back to `--no-image` if interactive |
| Platform mismatch | Platform tag comparison | Reject with `IMAGE-PLATFORM-MISMATCH` condition |
| Version too new | `format_version` comparison | Reject with `IMAGE-VERSION-MISMATCH` condition |
| `mmap` failure | OS error return | Signal `STORAGE-CONDITION` with retry restart |
| Relocation overflow | Delta > heap size | Signal `IMAGE-CORRUPT` |
| Save I/O error | `write` / `fsync` error | Unlink temp file, signal `FILE-ERROR` |
| Code-cache mapping failure | `mprotect` RX error | Discard code cache, fall back to interpreter (degraded) |

---

## 7.11  Test Strategy

| Area | Method | Frequency |
|------|--------|-----------|
| Image round-trip | Save image, load in new process, verify heap integrity | Per-PR |
| Relocation | Save at address A, force load at address B, run test suite | Per-PR |
| Standalone executable | Build standalone, execute, check output | Per-PR |
| Shared library embedding | C test harness calls `torcl_init` / `torcl_eval` / `torcl_destroy` | Per-PR |
| Configuration precedence | Test env-var → CLI override → in-image default ordering | Per-PR |
| Cross-platform image rejection | Attempt to load a Linux image on macOS mock; expect error | Per-PR |
| Perf-map emission | JIT-compile a function, verify map file contains entry | Nightly |
| Structured log parsing | Capture log output, parse as JSON, validate schema | Per-PR |
| Debian package | `dpkg -i`, verify paths, run `torcl --version` | Release |

---

## 7.12  Concurrency

Image save acquires a global safepoint barrier (§2, §3): all mutator
threads reach a safepoint and suspend before serialisation begins.
Image load occurs before any user threads are started, so there are
no concurrency concerns during load.

The GC and JIT log channels use lock-free MPSC ring buffers
(`crossbeam-channel`) to avoid blocking mutator threads.  A dedicated
log-writer thread drains the ring and writes to stderr.

---

## 7.13  Module Map

| Source File | Responsibility | Spec Refs |
|-------------|---------------|-----------|
| `crates/torcl-rt/src/startup.rs` | Image load, appended-image detection, CLI parsing | R7.02–R7.07 |
| `crates/torcl-rt/src/image.rs` | Image serialisation / deserialisation, relocation | R7.01, R7.03, R7.04, R7.20 |
| `crates/torcl-rt/src/image/header.rs` | Header and section directory types (D7.01, D7.02) | R7.04, R7.15 |
| `crates/torcl-rt/src/image/reloc.rs` | Relocation table encoder / decoder (A7.01) | R7.03 |
| `crates/torcl-rt/src/config.rs` | Configuration resolution (env → CLI → image) | R7.11 |
| `crates/torcl-rt/src/log.rs` | Structured logger, GC/JIT channels | R7.16–R7.18 |
| `crates/torcl-rt/src/perf_map.rs` | Linux perf-map writer | R7.17 |
| `crates/torcl-rt/src/dtrace.rs` | USDT probe definitions | §7.9.5 |
| `crates/torcl/src/main.rs` | CLI flag parsing, REPL/script dispatch | R7.07 |

---

## 7.14  Runtime Operating Profiles

This section defines the startup profiles exposed by the CLI and by
embedding callers. It narrows the intended behaviour of the current
bootstrap implementation and gives later phases a stable compatibility
contract.

### 7.14.1  Startup Resolution Order

The runtime chooses exactly one startup profile in the following order
(R7.21):

1. `--eval EXPR` — start enough runtime state to evaluate `EXPR`,
   print the resulting values, and exit.
2. `--load FILE` — load `FILE`, evaluate top-level forms, and exit.
3. `SCRIPT` positional path — execute the script, bind remaining
   arguments after `--` to `*COMMAND-LINE-ARGS*`, and exit.
4. `--bootstrap` — skip image discovery and load the bootstrap file set
   rooted at `lib/boot.lisp`.
5. `--no-image` — start with an empty heap and no boot image.
6. Appended image on the executable — if present and valid, load it.
7. Default image discovery via `--image` or `TORCL_IMAGE_PATH`.
8. Fallback interactive REPL with no image if all image discovery
   mechanisms fail and stdin is a terminal.

After the runtime selects a startup profile, it applies prelude and init
loading in this order:

1. Load the standard bootstrap prelude unless `--no-bootstrap` is set
   (R7.25, R7.26).
2. If the selected profile is interactive REPL and `--no-init` is not
   set, perform init-file discovery per §7.6.3 (R7.24).
3. Execute the explicit `--eval`, `--load`, or script payload if one was
   selected.

If multiple explicit execution modes are requested, argument parsing
MUST fail before runtime initialisation with a diagnostic naming the
conflicting flags.

### 7.14.2  Bootstrap-Without-Image Profile

The bootstrap profile (R7.22) exists to support early development,
packaging, and recovery when no valid `.bimg` is available.

Bootstrap mode MUST:

- Initialise the object model, allocator, reader, evaluator, package
  registry, and enough stream support to read `lib/boot.lisp`.
- Search for `lib/boot.lisp` relative to the current working directory,
  then relative to the installed prefix, and fail with `FILE-ERROR` if
  neither location is valid.
- Permit `(torcl-ext:save-image ...)` once the bootstrap sequence has
  completed successfully.
- Continue to load the standard bootstrap prelude unless
  `--no-bootstrap` is set; `--no-bootstrap` selects the raw
  no-prelude variant even when combined with `--load` or `--eval`.

Bootstrap mode MUST NOT assume that any previously saved heap image,
compiled code cache, or package registry exists.

### 7.14.3  Immutable-Image Profile

In the immutable-image profile (R7.23), the saved image is treated as a
read-only baseline:

- The mapped image pages remain read-only after relocation.
- Mutable roots such as symbol value cells, package hash tables, thread
  state, and newly compiled code are copied or re-homed into writable
  runtime regions before mutation.
- If the process terminates without an explicit image save, mutations in
  writable regions are discarded without touching the original image.

This profile is the required deployment mode for container images,
Nix-style stores, and shared read-only installations.
