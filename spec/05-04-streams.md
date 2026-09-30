# §5.5  Streams

**Scope:** Stream class hierarchy, Gray streams protocol, built-in stream
implementations, external-format encoding/decoding, buffering strategies,
OS integration, and thread safety.

EGCL streams follow ANSI X3.226-1994 Chapter 21 and implement the
*Gray streams* extension (David N. Gray, 1989; de-facto standard across
SBCL, CCL, ECL, ABCL).  Gray streams are the user-facing extensibility
mechanism; all built-in stream types are built on top of the same protocol.

Source location: `crates/egcl-stdlib/src/streams.lisp` (CL layer) and
`crates/egcl-rt/src/io/` (Rust I/O primitives).

---

## 5.5.1  Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R5.111 | Implement the full ANSI stream class hierarchy (`stream`, `input-stream`, `output-stream`, `bidirectional-stream`, `file-stream`, `string-stream`, `broadcast-stream`, `concatenated-stream`, `two-way-stream`, `echo-stream`, `synonym-stream`). | MUST |
| R5.112 | Implement Gray streams base classes and the full generic-function protocol as specified in §5.5.2. | MUST |
| R5.113 | All standard CL stream functions (`read-char`, `write-char`, `read-byte`, `write-byte`, `read-sequence`, `write-sequence`, `listen`, `clear-input`, `clear-output`, `finish-output`, `force-output`, `peek-char`, `unread-char`, `read-line`, `write-string`, `terpri`, `fresh-line`, `file-position`, `file-length`, `close`, `open-stream-p`, `stream-element-type`, `interactive-stream-p`, `stream-external-format`) MUST dispatch through the Gray streams generic functions. | MUST |
| R5.114 | `file-stream` MUST use OS file-descriptor I/O with configurable buffering (§5.5.5). | MUST |
| R5.115 | Support external formats `:utf-8`, `:ascii`, `:latin-1`, `:utf-16`, `:utf-16le`, `:utf-16be`, `:utf-32`, `:utf-32le`, `:utf-32be`. Default external format MUST be `:utf-8`. | MUST |
| R5.116 | External-format error handling MUST support `:replacement` (U+FFFD) and `:error` (signal `encoding-error` / `decoding-error`) policies. | MUST |
| R5.117 | BOM (byte-order mark) detection on `:utf-16` and `:utf-32` streams MUST auto-select endianness; absence of BOM MUST default to big-endian per Unicode spec. | MUST |
| R5.118 | `string-stream` (`make-string-input-stream`, `make-string-output-stream`, `with-input-from-string`, `with-output-to-string`) MUST operate on in-memory character vectors with O(1) amortised `write-char`. | MUST |
| R5.119 | Composite streams (broadcast, concatenated, two-way, echo, synonym) MUST delegate to constituent streams exactly per ANSI semantics. | MUST |
| R5.120 | Every stream MUST embed a per-stream mutex; operations spanning multiple elements (e.g., `write-sequence`, `read-line`) MUST be atomic under that mutex. | MUST |
| R5.121 | `file-stream` MUST register a GC finalizer that closes the underlying fd if the stream becomes unreachable without explicit `close`. A warning condition SHOULD be signalled when the finalizer fires. | MUST / SHOULD |
| R5.122 | `close` MUST be idempotent; calling `close` on an already-closed stream MUST NOT signal an error. | MUST |
| R5.123 | After `close`, any I/O operation on the stream MUST signal a `stream-closed-error` (subtype of `stream-error`). | MUST |
| R5.124 | `file-position` and `file-length` MUST return `nil` for streams that do not support positioning (e.g., pipes, sockets). | MUST |
| R5.125 | Line-buffered streams MUST flush on `#\Newline` writes and on any read from a `two-way-stream`'s input side (terminal-style flushing). | MUST |
| R5.126 | Non-blocking I/O mode on `file-stream` SHOULD be available via an `:if-would-block` option (`:wait` default, `:return-nil`). | SHOULD |
| R5.127 | `read-sequence` and `write-sequence` on `file-stream` MUST bypass the per-character Gray dispatch and use bulk buffer copies for performance. | MUST |
| R5.128 | `stream-external-format` on character streams MUST return the canonical external-format designator (e.g., `:utf-8`). | MUST |
| R5.129 | The stream subsystem MUST be fully operational in the bootstrap image (Phase 1); Rust primitives provide fd-level I/O, CL code builds the class hierarchy on top. | MUST |
| R5.130 | `*standard-input*`, `*standard-output*`, `*error-output*`, `*trace-output*`, `*debug-io*`, `*query-io*`, `*terminal-io*` MUST be bound to appropriate streams at startup per ANSI §21.1. | MUST |

---

## 5.5.2  Gray Streams Protocol

### 5.5.2.1  Base Classes

All Gray stream classes live in the `EGCL-GRAY-STREAMS` package, re-exported
from `COMMON-LISP`.

| Class | Superclasses | Role |
|-------|-------------|------|
| `fundamental-stream` | `stream`, `standard-object` | Root of Gray hierarchy; adds CLOS-based dispatch.  Contains a `stream-id` slot (monotonically increasing `u64` assigned at creation) used for lock ordering (§5.5.7.3). |
| `fundamental-input-stream` | `fundamental-stream`, `input-stream` | Readable stream mixin. |
| `fundamental-output-stream` | `fundamental-stream`, `output-stream` | Writable stream mixin. |
| `fundamental-character-stream` | `fundamental-stream` | Element type is `character`. |
| `fundamental-binary-stream` | `fundamental-stream` | Element type is `(unsigned-byte 8)` or user-specified. |
| `fundamental-character-input-stream` | `fundamental-input-stream`, `fundamental-character-stream` | Character + input. |
| `fundamental-character-output-stream` | `fundamental-output-stream`, `fundamental-character-stream` | Character + output. |
| `fundamental-binary-input-stream` | `fundamental-input-stream`, `fundamental-binary-stream` | Binary + input. |
| `fundamental-binary-output-stream` | `fundamental-output-stream`, `fundamental-binary-stream` | Binary + output. |

### 5.5.2.2  Required Generic Functions — Input

| Generic Function | Signature | Contract |
|-----------------|-----------|----------|
| `stream-read-char` | `(stream) → character or :eof` | Read one character. Subclass MUST implement. |
| `stream-unread-char` | `(stream character) → nil` | Push back one character. At most one unread between reads. |
| `stream-read-char-no-hang` | `(stream) → character or nil or :eof` | Non-blocking variant; returns `nil` if no data available. |
| `stream-peek-char` | `(stream) → character or :eof` | Default: `read-char` + `unread-char`. |
| `stream-listen` | `(stream) → boolean` | `T` if a character is available without blocking. |
| `stream-read-line` | `(stream) → string, eof-p` | Default: loop on `stream-read-char` until `#\Newline` or `:eof`. |
| `stream-clear-input` | `(stream) → nil` | Discard pending input. |
| `stream-read-byte` | `(stream) → integer or :eof` | Read one byte from a binary stream. Subclass MUST implement. |
| `stream-read-sequence` | `(stream sequence start end) → index` | Bulk read. Default: loop on element reads. Optimised for built-in streams (R5.127). |

### 5.5.2.3  Required Generic Functions — Output

| Generic Function | Signature | Contract |
|-----------------|-----------|----------|
| `stream-write-char` | `(stream character) → character` | Write one character. Subclass MUST implement. |
| `stream-line-column` | `(stream) → integer or nil` | Current column number, or `nil` if unknown. |
| `stream-start-line-p` | `(stream) → boolean` | Default: `(eql (stream-line-column stream) 0)`. |
| `stream-write-string` | `(stream string &optional start end) → string` | Default: loop on `stream-write-char`. |
| `stream-terpri` | `(stream) → nil` | Default: `(stream-write-char stream #\Newline)`. |
| `stream-fresh-line` | `(stream) → nil` | Write newline unless at column 0. |
| `stream-finish-output` | `(stream) → nil` | Block until all output delivered to destination. |
| `stream-force-output` | `(stream) → nil` | Initiate delivery without blocking. |
| `stream-clear-output` | `(stream) → nil` | Discard buffered output. |
| `stream-advance-to-column` | `(stream column) → boolean` | Pad with spaces to reach `column`. |
| `stream-write-byte` | `(stream integer) → integer` | Write one byte to a binary stream. Subclass MUST implement. |
| `stream-write-sequence` | `(stream sequence start end) → sequence` | Bulk write. Default: loop on element writes. |

### 5.5.2.4  Required Generic Functions — Query and Lifecycle

These standard CL functions MUST dispatch through Gray generic functions
on `fundamental-stream` subclasses (R5.113):

| Generic Function | Signature | Contract |
|-----------------|-----------|----------|
| `stream-element-type` | `(stream) → typespec` | Return the element type of the stream (e.g. `character`, `(unsigned-byte 8)`). Subclass MUST implement or inherit a correct default. |
| `open-stream-p` | `(stream) → boolean` | Return `T` if the stream is open. Default on `fundamental-stream`: check the `open` slot. |
| `close` | `(stream &key abort) → t` | Close the stream. If `:abort` is true, discard pending output without flushing. Default on `fundamental-stream`: set `open` to `nil`, deregister finalizer. MUST be idempotent (R5.122). |
| `interactive-stream-p` | `(stream) → boolean` | Return `T` if the stream is interactive (e.g. a terminal). Default on `fundamental-stream`: `nil`. `egcl-file-stream` overrides to check `isatty(3)`. |

---

## 5.5.3  Built-in Stream Implementations

### 5.5.3.1  `egcl-file-stream`

**Superclasses:** `fundamental-character-input-stream`,
`fundamental-character-output-stream` (for character mode); or
`fundamental-binary-input-stream` / `fundamental-binary-output-stream`
(binary mode).  Direction determined at open time.

**Data structure — D5.15 (`EgclFileStream`):**

```rust
struct EgclFileStream {
    fd: RawFd,                      // OS file descriptor
    direction: Direction,           // :input | :output | :io
    element_type: EgclVal,         // character or (unsigned-byte N)
    external_format: ExternalFormat,// §5.5.4
    buffer: StreamBuffer,           // §5.5.5
    position: u64,                  // logical byte position in file
    unread_char: Option<char>,      // one-slot push-back buffer
    column: u32,                    // current column tracking
    open: AtomicBool,               // guard for R5.123
    mutex: Mutex<()>,               // per-stream lock (R5.120)
    finalizer_registered: bool,     // GC finalizer flag (R5.121)
    nonblocking: bool,              // R5.126
}
```

**Behaviour:**
- Created via CL `open` / `with-open-file`.
- On character streams, bytes are decoded/encoded through the
  external-format codec (§5.5.4).
- `file-length` calls `fstat(2)`.
- Pipes/sockets/FIFOs: `file-position`/`file-length` return `nil` (R5.124).

**Buffer–position synchronisation:**

The `position` field tracks the *logical byte offset in the file* that
corresponds to the next byte the user would read or write, accounting for
buffered data.  It is maintained as follows:

1. **Read path:** After a `read(2)` syscall fills the buffer, `position`
   is set to `fd_offset_after_read - bytes_in_buffer + buf.pos`.  Each
   `stream-read-char` / `stream-read-byte` advances `buf.pos` and
   increments `position` by the number of raw bytes consumed (which may
   differ from 1 for multi-byte codecs).

2. **Write path:** Each `stream-write-char` / `stream-write-byte` appends
   encoded bytes to the buffer and increments `position` by the number of
   bytes written to the buffer.  On flush, `write(2)` sends `buf[0..fill]`
   to the fd; the fd offset then equals `position`.

3. **Bidirectional (`:io`) streams:** A direction-switch protocol prevents
   desynchronisation:
   - **Read → Write transition:** The buffer is invalidated (discarded).
     `lseek(fd, position, SEEK_SET)` is called to align the fd offset to
     the logical position before the first write.
   - **Write → Read transition:** The buffer is flushed via `write(2)`.
     The fd offset now equals `position`; the next `read(2)` refills the
     buffer from that point.
   The current direction is tracked in a `last_op: Option<Direction>` field
   (elided from D5.15 for brevity but stored alongside `buffer`).

4. **`file-position` (query):** Returns `position` directly — no syscall
   needed because `position` is always kept in sync.

5. **`file-position` (set):** Flushes dirty buffer data via `write(2)`,
   invalidates the read buffer, calls `lseek(fd, new_pos, SEEK_SET)`, and
   sets `position = new_pos`.

### 5.5.3.2  `egcl-string-stream`

**Data structure — D5.16 (`EgclStringStream`):**

```rust
struct EgclStringStream {
    string: EgclVal,         // underlying string (simple or adjustable)
    index: usize,             // current read/write position
    limit: usize,             // end index for input streams
    direction: Direction,     // :input or :output
    column: u32,
    open: AtomicBool,
    mutex: Mutex<()>,
}
```

- **Input:** Reads from a borrowed string slice.  `stream-read-char`
  returns characters from `index` to `limit`.
- **Output:** Writes to a fill-pointer string.  `get-output-stream-string`
  extracts the accumulated string and resets position.
- O(1) amortised `write-char` via fill-pointer growth (doubling strategy),
  satisfying R5.118.

### 5.5.3.3  Composite Streams

| Stream Type | Struct / D-number | Semantics |
|-------------|-------------------|-----------|
| `broadcast-stream` | D5.17 | Writes to all component streams; `stream-write-char` fans out. Reads not supported. |
| `concatenated-stream` | D5.18 | Reads from first component; on EOF, pops and reads from next. Writes not supported. |
| `two-way-stream` | D5.19 | Delegates reads to input-stream, writes to output-stream. `force-output` on write side before reads (R5.125). |
| `echo-stream` | D5.20 | Like `two-way-stream`, but characters read from input are echoed to output. `unread-char` suppresses echo of re-read character. |
| `synonym-stream` | D5.21 | Holds a symbol; every operation `symbol-value`s the symbol and delegates. |

All composite streams store their component(s) and delegate via the Gray
protocol.

**Composite stream locking policy (R5.120):**

Composite stream operations acquire **both** the composite's own mutex
**and** the mutex of each component stream they delegate to, following the
lock ordering defined in §5.5.7.3 (composite first, then components in
`stream-id` order).  Specifically:

- `broadcast-stream` `stream-write-char`: acquires the broadcast-stream
  lock, then acquires each component stream's lock in `stream-id` order
  before writing.  Component locks are released in reverse order after
  the write completes.
- `two-way-stream` / `echo-stream`: acquires the composite lock, then the
  relevant component's lock (input-side or output-side) for the operation.
- `synonym-stream`: acquires its own lock, resolves the symbol value, then
  acquires the target stream's lock.

This two-level locking ensures that (a) the composite operation is atomic
from the caller's perspective, and (b) individual component streams
remain safe when accessed both directly and through composites
concurrently.

---

## 5.5.4  External Formats and Encoding

### 5.5.4.1  Codec Trait

```lisp
(defgeneric codec-encode (codec character buffer)
  (:documentation "Encode CHARACTER into BUFFER (octet vector).
   Return the number of octets written."))

(defgeneric codec-decode (codec buffer start end)
  (:documentation "Decode one character from BUFFER[START..END].
   Return (VALUES character octets-consumed) or signal DECODING-ERROR."))

(defgeneric codec-name (codec)
  (:documentation "Return the canonical keyword name, e.g. :UTF-8."))

(defgeneric codec-replacement-character (codec)
  (:documentation "Return the replacement character (default U+FFFD)."))
```

### 5.5.4.2  Built-in Codecs

| Keyword | Codec Class | Bytes/Char | Notes |
|---------|------------|------------|-------|
| `:ascii` | `ascii-codec` | 1 | Signals on codepoints > 127. |
| `:latin-1` / `:iso-8859-1` | `latin-1-codec` | 1 | Identity map for 0–255. |
| `:utf-8` | `utf-8-codec` | 1–4 | R5.115 default. Validates continuation bytes. |
| `:utf-16` | `utf-16-codec` | 2–4 | BOM-detecting (R5.117). |
| `:utf-16be` | `utf-16be-codec` | 2–4 | Explicit big-endian. |
| `:utf-16le` | `utf-16le-codec` | 2–4 | Explicit little-endian. |
| `:utf-32` | `utf-32-codec` | 4 | BOM-detecting. |
| `:utf-32be` | `utf-32be-codec` | 4 | Explicit big-endian. |
| `:utf-32le` | `utf-32le-codec` | 4 | Explicit little-endian. |

### 5.5.4.3  Error Recovery

Controlled by the `:error-handling` option to `open` (and stored in
`external-format`):

| Policy | Behaviour |
|--------|-----------|
| `:replacement` (default) | Replace malformed input with U+FFFD; replace unencodable output with `?` (ASCII) or codec-specific substitute. |
| `:error` | Signal `decoding-error` (subtype of `stream-error`) on malformed input, `encoding-error` on unencodable output.  Restarts: `use-replacement` (continue with U+FFFD), `skip-character`, `use-value` (supply replacement interactively). |

### 5.5.4.4  BOM Handling (R5.117)

- **Reading `:utf-16` / `:utf-32`:** First 2/4 bytes examined.  If BOM
  detected, endianness is set accordingly and BOM consumed.  If absent,
  big-endian is assumed.
- **Writing `:utf-16` / `:utf-32`:** No BOM emitted by default.  Pass
  `:bom t` in the external-format spec to emit BOM at stream start.
- Explicit endian keywords (`:utf-16le`, etc.) MUST NOT consume or emit BOM.

---

## 5.5.5  Buffering Strategies

**Data structure — D5.22 (`StreamBuffer`):**

```rust
struct StreamBuffer {
    buf: Box<[u8]>,       // heap-allocated buffer
    capacity: usize,      // buf.len()
    pos: usize,           // current read position in buffer
    fill: usize,          // valid bytes in buffer (read) or write cursor (write)
    mode: BufferMode,     // Unbuffered | LineBuffered | FullyBuffered
    dirty: bool,          // write-back needed before refill
}

enum BufferMode {
    Unbuffered,           // every write → syscall
    LineBuffered,         // flush on #\Newline or input request
    FullyBuffered,        // flush when buffer full or explicit flush
}
```

| Mode | Default Buffer Size | When Used |
|------|-------------------|-----------|
| `Unbuffered` | 0 (no buffer; 1-byte temp only) | `:buffering :none` or `*error-output*` |
| `LineBuffered` | 4 KiB | Interactive / terminal streams |
| `FullyBuffered` | 8 KiB | Regular file streams |

**Selection rules:**
1. Explicit `:buffering` argument to `open` overrides everything.
2. If fd `isatty(3)` → `LineBuffered`.
3. Otherwise → `FullyBuffered`.

**Flush triggers:**
- `FullyBuffered`: buffer full, `force-output`, `finish-output`, `close`.
- `LineBuffered`: above + `#\Newline` written + read from paired input
  on a `two-way-stream` (R5.125).
- `Unbuffered`: after every write call.

---

## 5.5.6  OS Integration

### 5.5.6.1  File Descriptors

All file-stream I/O goes through Rust wrappers around POSIX
`read(2)`/`write(2)`/`lseek(2)`/`close(2)`:

```
crates/egcl-rt/src/io/
├── fd.rs          // RawFd wrapper, non-blocking mode toggle
├── buffer.rs      // StreamBuffer implementation
├── codec.rs       // Rust-side bootstrap UTF-8 codec (see below)
└── stdio.rs       // Setup of *standard-input*, etc. from fds 0/1/2
```

**Rust bootstrap codec vs. CL codec protocol (R5.129):**

During Phase 1 bootstrap, the CLOS-based codec generic functions
(§5.5.4.1) are not yet available because the class hierarchy has not been
built.  The Rust-side `codec.rs` provides a hard-coded UTF-8
encoder/decoder that is called directly by the bootstrap stream
primitives (e.g., reading `*.lisp` source files to build the image).

Once the CL stream class hierarchy is initialised (end of Phase 1):

1. The bootstrap Rust codec is **replaced**: `egcl-file-stream` methods
   switch to dispatching through the CL `codec-encode` / `codec-decode`
   generic functions.  The Rust entry points are no longer called for
   normal stream operations.
2. **Correctness guarantee:** The bootstrap test suite (§5.5.10) includes
   a round-trip comparison test that encodes and decodes a corpus of
   Unicode strings through both the Rust `codec.rs` path and the CL
   `utf-8-codec` path, asserting byte-identical output.  This test runs
   as part of image build validation.
3. After switchover, `codec.rs` remains compiled into the runtime but is
   only reachable via an internal `%bootstrap-decode-utf8` FFI function,
   retained for emergency / fallback use (e.g., decoding error messages
   if the CL codec signals during condition handling).

### 5.5.6.2  Non-blocking I/O (R5.126)

```lisp
(open path :direction :input :if-would-block :return-nil)
```

- Sets `O_NONBLOCK` on the fd.
- `stream-read-char-no-hang` / `stream-listen` use `poll(2)` with zero
  timeout.
- When a read/write would block and policy is `:return-nil`, the Gray
  method returns `nil` instead of blocking.
- Default policy `:wait` retries with blocking reads (no `O_NONBLOCK`).

### 5.5.6.3  GC Finalizer Auto-Close (R5.121)

- When a `egcl-file-stream` is allocated, a weak reference + closure is
  registered with the GC finalizer queue (§3).
- If the stream becomes unreachable without `close`:
  1. Finalizer calls `close(fd)`.
  2. Signals `style-warning` condition:
     `"Stream ~S was GC'd without being explicitly closed."`.
- Finalizer runs on a dedicated finalizer thread, not inside the GC pause.
- `close` deregisters the finalizer to avoid double-close.

---

## 5.5.7  Thread Safety and Concurrency

### 5.5.7.1  Per-Stream Mutex (R5.120)

Every stream object contains a `Mutex`.  All public stream operations
acquire this mutex:

```lisp
;; Pseudocode — actual implementation via :around method on fundamental-stream
(defmethod stream-write-string :around ((stream fundamental-stream) string &optional start end)
  (with-stream-lock (stream)
    (call-next-method)))
```

### 5.5.7.2  Bulk Operation Atomicity

`read-sequence`, `write-sequence`, `read-line`, and `write-string` hold
the stream lock for their entire duration.  This guarantees that
interleaved writes from multiple threads do not produce garbled output.

**Bulk bypass and codec interaction (R5.127):**

R5.127 requires that `read-sequence` / `write-sequence` on `file-stream`
bypass the per-character Gray generic-function dispatch (i.e., they do NOT
call `stream-read-char` / `stream-write-char` in a loop).  The bypass
behaviour depends on the stream's element type:

- **Binary streams** (`element-type` is an integer subtype): Bulk
  `read(2)` / `write(2)` directly between the user-supplied sequence and
  the stream buffer.  No codec is involved.  This is the fastest path.

- **Character streams**: The bulk path still performs codec
  encoding/decoding, but does so on buffer-sized chunks rather than
  character-by-character.  Specifically, `write-sequence` encodes the
  entire sub-sequence into the stream buffer via repeated `codec-encode`
  calls on spans of characters (not one GF call per character), flushing
  full buffers to the fd as needed.  `read-sequence` symmetrically fills
  the buffer via `read(2)` and decodes spans via `codec-decode`.  The
  codec generic functions are still called, but the per-character
  `stream-write-char` / `stream-read-char` generic functions and their
  `:around` methods (including redundant per-char lock acquisition) are
  bypassed.

This means "bypass" in R5.127 refers to bypassing the per-element Gray
stream GF dispatch loop, NOT bypassing codec processing.

### 5.5.7.3  Lock Ordering

When composite streams delegate to component streams, they acquire locks
in a fixed order to prevent deadlock:

1. **Composite stream lock first.**
2. **Component stream(s) in allocation order** (oldest-first, determined
   by a monotonic stream-id assigned at creation).

`synonym-stream` resolves the symbol value **inside** its own lock,
then acquires the target stream's lock — this prevents TOCTOU races if
the symbol is rebound concurrently.

### 5.5.7.4  Standard Stream Bindings

`*standard-input*`, `*standard-output*`, etc. are per-thread dynamic
bindings.  Each thread inherits the creating thread's bindings at spawn
time (§2).  Rebinding in one thread does not affect others.

---

## 5.5.8  Error Handling

### 5.5.8.1  Condition Types

| Condition | Parent | When |
|-----------|--------|------|
| `stream-error` | `error` | Any stream-related error (ANSI). |
| `stream-closed-error` | `stream-error` | I/O on a closed stream (R5.123). |
| `end-of-file` | `stream-error` | Read past end of stream (ANSI). |
| `encoding-error` | `stream-error` | Unencodable character in output (R5.116). |
| `decoding-error` | `stream-error` | Malformed byte sequence in input (R5.116). |
| `file-stream-gc-warning` | `style-warning` | Finalizer closed an unreleased fd (R5.121). |

### 5.5.8.2  Restarts

All encoding/decoding errors establish these restarts:

| Restart | Effect |
|---------|--------|
| `use-replacement` | Continue with U+FFFD (decode) or `?` (encode). |
| `skip-character` | Drop the malformed byte(s) / unencodable char. |
| `use-value` | Caller supplies a replacement character. |

---

## 5.5.9  Configuration

| Tunable | Default | Env Var | Effect |
|---------|---------|---------|--------|
| Default external format | `:utf-8` | `EGCL_EXTERNAL_FORMAT` | Applied to all `open` calls without explicit `:external-format`. |
| File-stream buffer size | 8192 | `EGCL_STREAM_BUFFER_SIZE` | Bytes. Must be ≥ 512. |
| Line-buffer size | 4096 | `EGCL_LINE_BUFFER_SIZE` | Bytes. |
| GC finalizer warnings | enabled | `EGCL_WARN_UNCLOSED_STREAMS=0` | Set to `0` to suppress `file-stream-gc-warning`. |

---

## 5.5.10  Test Strategy

| Category | Approach |
|----------|----------|
| ANSI compliance | Paul Dietz `ansi-test` stream chapters (§21). |
| Gray streams extensibility | Define a custom counting-stream subclass; verify dispatch through all generic functions. |
| Encoding round-trip | For each codec: encode random Unicode → decode → compare. Property-based testing with edge cases (surrogates, overlong UTF-8, BOM). |
| Buffering correctness | Write patterns that cross buffer boundaries; verify byte-exact output. |
| Thread safety | Concurrent writes from N threads to a shared file-stream; verify no garbled interleaving and no crashes. |
| Composite streams | Verify broadcast fans out, concatenated chains, echo echoes, synonym follows rebinding. |
| Finalizer | Create streams without closing; trigger GC; verify fd is closed and warning signalled. |
| Non-blocking | Open a FIFO in non-blocking mode; verify `:return-nil` when empty. |
| Edge cases | Zero-length reads/writes, `unread-char` at BOF, `file-position` on pipe returns `nil`, double `close` is no-op. |

---

## 5.5.11  Module Map

```
crates/egcl-stdlib/src/
├── streams/
│   ├── package.lisp          # EGCL-GRAY-STREAMS package definition
│   ├── gray-classes.lisp     # fundamental-* class definitions (§5.5.2.1)
│   ├── gray-protocol.lisp    # generic function definitions (§5.5.2.2–3)
│   ├── file-stream.lisp      # egcl-file-stream (§5.5.3.1)
│   ├── string-stream.lisp    # egcl-string-stream (§5.5.3.2)
│   ├── broadcast.lisp        # broadcast-stream
│   ├── concatenated.lisp     # concatenated-stream
│   ├── two-way.lisp          # two-way-stream + echo-stream
│   ├── synonym.lisp          # synonym-stream
│   ├── codec/
│   │   ├── protocol.lisp     # codec generic functions (§5.5.4.1)
│   │   ├── ascii.lisp
│   │   ├── latin-1.lisp
│   │   ├── utf-8.lisp
│   │   ├── utf-16.lisp
│   │   └── utf-32.lisp
│   └── conditions.lisp       # stream conditions & restarts (§5.5.8)

crates/egcl-rt/src/io/
├── fd.rs                     # RawFd wrapper
├── buffer.rs                 # StreamBuffer (D5.22)
├── codec.rs                  # bootstrap UTF-8 fast path
└── stdio.rs                  # standard stream fd setup
```
