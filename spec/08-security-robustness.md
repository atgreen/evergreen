# §8 Security & Robustness

**Scope:** This chapter specifies TorCL's defences against hostile or
malformed input, resource exhaustion, unsafe memory access, and
concurrency hazards. It covers the sandboxing model, safe FFI boundary,
resource limits, signal safety, reader hardening, integer overflow
semantics, thread-safety contracts, crash recovery, and the fuzzing
strategy that validates all of the above.

---

## 8.1  Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R8.01 | TorCL MUST provide a *restricted evaluation mode* (sandbox) that disables file I/O, network access, FFI calls, and OS process spawning. | MUST |
| R8.02 | Sandbox capability grants MUST be whitelist-based; the default set MUST be empty (deny-all). | MUST |
| R8.03 | All `unsafe` Rust blocks in the runtime MUST be confined to `crates/torcl-rt/src/ffi.rs`, `crates/torcl-rt/src/gc/*.rs`, and `crates/torcl-rt/src/signal.rs`; each block MUST be documented with a `// SAFETY:` comment. | MUST |
| R8.04 | FFI pointer arguments MUST be validated (non-null, alignment, bounds) before dereference. | MUST |
| R8.05 | No raw pointer value MUST ever be directly accessible to user CL code. | MUST |
| R8.06 | The runtime MUST enforce a configurable maximum heap size hard cap; allocation beyond the cap MUST signal a `STORAGE-CONDITION`. | MUST |
| R8.07 | The runtime MUST enforce a configurable stack-depth limit; exceeding it MUST signal a `TORCL-EXT:STACK-OVERFLOW-ERROR` (restartable). | MUST |
| R8.08 | Sandbox mode MUST support a per-evaluation CPU time limit; exceeding it MUST signal `TORCL-EXT:TIMEOUT-CONDITION`. | MUST |
| R8.09 | An allocation-rate throttle SHOULD be available in sandbox mode to slow runaway loops without killing them outright. | SHOULD |
| R8.10 | Signal handlers MUST be async-signal-safe; all non-trivial work MUST be deferred to the next safepoint. | MUST |
| R8.11 | The reader MUST be hardened against deeply nested structures (configurable depth limit, default 4096). | MUST |
| R8.12 | The reader MUST detect circular structure (`#n=` / `#n#`) and reject it when `*READ-CIRCULAR*` is NIL (the default). | MUST |
| R8.13 | Read-time evaluation (`#.`) MUST be disabled by default (`*READ-EVAL*` = NIL). | MUST |
| R8.14 | Fixnum arithmetic that overflows the 61-bit range MUST promote the result to bignum; silent wraparound MUST NOT occur. | MUST |
| R8.15 | Thread-safety contracts for every public runtime API MUST be documented and enforced (§8.7). | MUST |
| R8.16 | A deterministic lock-ordering scheme MUST be defined; acquiring locks out of order MUST be detectable in debug builds. | MUST |
| R8.17 | Image loading MUST verify a checksum before mapping; corrupted images MUST be rejected with a clear diagnostic. | MUST |
| R8.18 | The project MUST maintain continuous fuzzing targets for the reader, the compiler front-end, and the GC allocator. | MUST |
| R8.19 | Type-safe alien-value marshalling MUST be used at the FFI boundary; CL types MUST round-trip through C types without information loss unless the user explicitly requests truncation. | MUST |
| R8.20 | Sandbox mode MUST NOT be escapable via `EVAL`, `COMPILE`, `LOAD`, or any reflective facility. | MUST |
| R8.21 | The runtime SHOULD provide a capability to detect potential deadlocks (cycle detection in the lock-wait graph) in debug builds. | SHOULD |
| R8.22 | All security-critical configuration defaults MUST be safe-by-default (deny sandbox capabilities, `*READ-EVAL*` NIL, etc.). | MUST |

---

## 8.2  Sandboxing Model

### 8.2.1  Overview

TorCL provides a **restricted evaluation mode** that creates an isolated
execution context in which untrusted CL code can be evaluated with
controlled access to system resources. The model is inspired by Java's
`SecurityManager` (pre-deprecation) but uses a simpler, capability-based
design.

### 8.2.2  Capability System

A **capability set** (`CapabilitySet`) is an immutable bitfield attached
to each sandbox context. Capabilities are deny-by-default.

| Capability | Bit | Grants |
|------------|-----|--------|
| `CAP_FILE_READ` | 0 | Read access to files matching path whitelist |
| `CAP_FILE_WRITE` | 1 | Write access to files matching path whitelist |
| `CAP_NET_CONNECT` | 2 | Outbound TCP/UDP to whitelisted host:port pairs |
| `CAP_NET_LISTEN` | 3 | Bind and listen on whitelisted ports |
| `CAP_FFI` | 4 | Call foreign functions via `CFFI`-style interface |
| `CAP_PROCESS` | 5 | Spawn OS subprocesses |
| `CAP_ENV` | 6 | Read environment variables |
| `CAP_EVAL` | 7 | Use `COMPILE`, `LOAD`, and `EVAL` (always on outside sandbox) |
| `CAP_THREADS` | 8 | Spawn new threads |
| `CAP_IMAGE` | 9 | Save or load heap images |

D8.01 — `CapabilitySet`: a `u64` bitfield with `has(cap) -> bool` and
`grant(cap) -> Self`. Constant `EMPTY = 0` (deny-all).

### 8.2.3  Sandbox Context

D8.02 — `SandboxContext`: holds `capabilities: CapabilitySet`,
`path_whitelist: Vec<PathPattern>`, `net_whitelist: Vec<HostPortPattern>`,
`max_heap_bytes`, `max_cpu_ms`, `alloc_rate_limit: Option<BytesPerSec>`,
and `max_stack_depth`.

The context is stored in a thread-local `Option<Arc<SandboxContext>>`.
Every guarded operation calls `check_capability(cap)` which returns
`Ok(())` outside a sandbox or when the capability is granted, and
`Err(TorclError::capability_denied(cap))` otherwise.

**Thread propagation (R8.20):** When a new thread is spawned inside a
sandbox (via `CAP_THREADS`), the runtime MUST propagate the parent
thread's `SandboxContext` to the child thread by cloning the `Arc` into
the child's thread-local slot before any user code executes. A child
thread MUST NOT run with a wider capability set than its parent.
Spawning a thread without `CAP_THREADS` MUST signal
`TorclError::capability_denied(CAP_THREADS)`.

### 8.2.4  Sandbox Entry API

```lisp
;; CL-side entry point
(torcl-ext:with-sandbox (:capabilities '(:file-read :eval)
                         :path-whitelist '("/tmp/scratch/*")
                         :max-heap-mb 64
                         :max-cpu-seconds 5)
  (load "/tmp/scratch/user-code.lisp"))
```

### 8.2.5  Sandbox Escape Prevention (R8.20)

The following operations are intercepted inside a sandbox:

- `EVAL`, `COMPILE`, `COMPILE-FILE`, `LOAD` — gated by `CAP_EVAL`.
- `OPEN`, `WITH-OPEN-FILE`, `DELETE-FILE`, `RENAME-FILE` — gated by
  `CAP_FILE_READ` / `CAP_FILE_WRITE` and checked against path whitelist.
- `CFFI:FOREIGN-FUNCALL` and all FFI entry points — gated by `CAP_FFI`.
- `SB-EXT:RUN-PROGRAM` / `UIOP:RUN-PROGRAM` — gated by `CAP_PROCESS`.
- `SAVE-LISP-AND-DIE` / image operations — gated by `CAP_IMAGE`.
- `#.` read-time eval — always disabled inside sandbox regardless of
  `CAP_EVAL`.

---

## 8.3  Safe FFI

### 8.3.1  Unsafe Code Confinement (R8.03)

All `unsafe` blocks MUST be confined to the following modules:

- **`crates/torcl-rt/src/ffi.rs`** — C-ABI bridge, pointer validation,
  alien value marshalling.
- **`crates/torcl-rt/src/gc/*.rs`** — raw memory manipulation required
  by the garbage collector: bump-pointer TLAB allocation, object header
  access, `mmap`/`mprotect` for memory-mapped heap regions, and write
  barrier implementations.
- **`crates/torcl-rt/src/signal.rs`** — POSIX signal handler registration
  (`sigaction`), `mprotect` for stack guard pages, and
  async-signal-safe flag operations.

No other module may contain `unsafe` code. The `#![deny(unsafe_code)]`
attribute is set at crate level in `torcl-rt`, `torcl-compiler`, and
`torcl`, with per-module `#[allow(unsafe_code)]` only in the three
locations listed above.

### 8.3.2  Pointer Validation (R8.04)

Every raw pointer received from C is validated before use:

```rust
// A8.01 — FFI pointer validation
fn validate_ptr<T>(ptr: *const T, context: &str) -> Result<&T, TorclError> {
    if ptr.is_null() {
        return Err(TorclError::ffi_null_pointer(context));
    }
    if (ptr as usize) % std::mem::align_of::<T>() != 0 {
        return Err(TorclError::ffi_misaligned(context));
    }
    // SAFETY: pointer is non-null, aligned, and caller guarantees
    // that the pointee is valid for the lifetime of the returned ref.
    Ok(unsafe { &*ptr })
}
```

### 8.3.3  Type-Safe Alien Value Marshalling (R8.19)

| CL Type | C Type | Rust Bridge Type | Validation |
|----------|--------|-----------------|------------|
| `INTEGER` | Signed/unsigned integers, 8–64 bits | Checked integer magnitude → raw `u64` bits | Check the declared C range; box results outside the fixnum range as bignums |
| `SINGLE-FLOAT` | `float` | `f32` | NaN/Inf pass-through (documented) |
| `DOUBLE-FLOAT` | `double` | `f64` | NaN/Inf pass-through (documented) |
| `STRING` | `const char*` | `CStr` | UTF-8 validation, null-terminator check |
| `(UNSIGNED-BYTE 8) VECTOR` | `uint8_t*` + `size_t` | `&[u8]` | Length bounds check |
| `T` (boxed) | `torcl_val_t` | `TorclVal` | Tag validation |

Truncation (e.g., `BIGNUM` → `int32_t`) MUST be explicitly requested via
`:truncate t` in the FFI declaration and signals a warning when data is
actually lost.

### 8.3.4  No User-Accessible Raw Pointers (R8.05)

Foreign pointers are wrapped in a `TORCL-EXT:FOREIGN-POINTER` object that
is opaque to CL code. The internal address is never exposed via any
accessor. The object carries a type tag and an optional destructor
callback for correct resource management.

---

## 8.4  Resource Limits

### 8.4.1  Heap Size Hard Cap (R8.06)

| Parameter | Default | Env Var | CLI Flag |
|-----------|---------|---------|----------|
| Max heap | 4 GiB | `TORCL_MAX_HEAP` | `--max-heap` |
| Sandbox max heap | 256 MiB | (per-sandbox) | N/A |

When the allocator cannot satisfy a request within the cap:

1. Trigger a full GC cycle.
2. If still over cap, signal `STORAGE-CONDITION` with the restart
   `CONTINUE` (retry after user frees references) and `ABORT`.

### 8.4.2  Stack Depth Limit (R8.07)

The runtime maintains a call-depth counter incremented in every function
prologue and decremented in every epilogue. Default limit: 10 000 frames.

When the limit is reached, a `TORCL-EXT:STACK-OVERFLOW-ERROR` is
signalled. The condition is restartable via `CONTINUE` (which resets the
counter and re-enters, allowing a debugger to intervene).

The counter lives in a thread-local slot and is cheap (~1 ns per
increment via a thread-local atomic).

| Parameter | Default | Env Var |
|-----------|---------|---------|
| Max stack depth | 10 000 | `TORCL_STACK_DEPTH` |

### 8.4.3  Allocation-Rate Throttle (R8.09)

In sandbox mode, an optional allocation-rate throttle limits the number
of bytes allocated per unit time. When the rate is exceeded, the
allocating thread sleeps until the rate drops below the threshold.

D8.03 — `AllocationThrottle`: sliding-window rate limiter tracking
`window_ns` (default 100 ms), `max_bytes_per_window` (default 128 MiB),
`current_window_start`, and `bytes_this_window`. When the rate is
exceeded, the allocating thread sleeps until the window rolls over.
This prevents tight loops from exhausting memory before a time limit
fires.

### 8.4.4  CPU Time Limit (R8.08)

In sandbox mode, wall-clock and CPU-time limits are enforced:

1. Before entering the sandbox, a deadline is computed:
   `deadline = clock_gettime(CLOCK_THREAD_CPUTIME_ID) + limit`.
2. At each safepoint (§2, loop back-edges and function prologues), the
   current thread CPU time is checked against the deadline.
3. If expired, `TORCL-EXT:TIMEOUT-CONDITION` is signalled.

Safepoint checks are cheap (~3 ns) because they read a thread-local
flag set by a timerfd/kqueue callback in a monitor thread.

---

## 8.5  Signal Safety (R8.10)

### 8.5.1  Signal Handling Strategy

TorCL uses **deferred signal processing** at safepoints, following the
HotSpot model:

1. **Signal handler** (async-signal-safe): sets a per-thread flag in a
   `sig_atomic_t` variable. No heap allocation, no lock acquisition, no
   stdio.
2. **Safepoint poll** (synchronous): reads the flag and dispatches to the
   appropriate CL condition or runtime action.

### 8.5.2  Handled Signals

| Signal | Handler Action | Deferred Action |
|--------|---------------|-----------------|
| `SIGINT` | Set `pending_interrupt` flag | Signal `INTERRUPT-CONDITION` |
| `SIGSEGV` | Check if in GC guard page → set flag; else abort | Signal `MEMORY-FAULT-ERROR` |
| `SIGBUS` | Same as `SIGSEGV` | Signal `MEMORY-FAULT-ERROR` |
| `SIGFPE` | Set `pending_fpe` flag | Signal `ARITHMETIC-ERROR` |
| `SIGPIPE` | Set `pending_pipe` flag | Signal `STREAM-ERROR` on next I/O |
| `SIGUSR1` | Set `pending_safepoint` flag (fallback only — see note) | Thread enters safepoint |
| `SIGTERM` | Set `shutdown_requested` flag | Orderly shutdown sequence |

**Note on SIGUSR1:** The primary safepoint mechanism is polling-based —
safepoint checks are inserted at loop back-edges and function prologues
(§4.3). `SIGUSR1` is used **only** as a fallback to interrupt threads
that are blocked in long-running system calls (e.g., `read`, `poll`,
`futex_wait`) and therefore cannot reach a poll-based safepoint in a
timely manner. The `SIGUSR1` handler merely sets a flag; it does not
suspend the thread directly. This is consistent with §4.3's statement
that there is no signal-based suspension.

### 8.5.3  Guard Pages

Stack guard pages (one page at each end of the thread stack) are
protected via `mprotect`. A `SIGSEGV` on a guard page is converted to a
stack-overflow condition rather than a crash.

GC uses guard pages on TLAB boundaries to trigger slow-path allocation.

---

## 8.6  Robustness Against Malformed Input

### 8.6.1  Reader Hardening (R8.11, R8.12, R8.13)

| Defence | Mechanism | Default |
|---------|-----------|---------|
| Nesting depth | Counter incremented on `(`, `#(`, `#S(`, etc.; error at limit | 4 096 levels |
| Token length | Maximum token length before error | 1 MiB |
| Circular structures | `#n=` / `#n#` tracking; rejected when `*READ-CIRCULAR*` is NIL | NIL |
| Read-time eval | `#.` dispatches only when `*READ-EVAL*` is T | NIL |
| Numeric overflow | Reader detects integers exceeding `most-positive-fixnum` and produces bignum | Always |
| Ratio simplification | `0` denominator signals `READER-ERROR` before division | Always |
| String escapes | Invalid escape sequences signal `READER-ERROR` | Always |

### 8.6.2  Recursive Descent Limits

The reader is implemented as a recursive-descent parser. To prevent
stack overflow in the Rust call stack, the reader tracks its current
depth and returns `Err(TorclError::NestingTooDeep)` before exceeding
the limit.

```rust
// A8.02 — Reader depth check
fn read_list(&mut self) -> Result<TorclVal, TorclError> {
    self.depth += 1;
    if self.depth > self.max_depth {
        return Err(TorclError::nesting_too_deep(self.depth));
    }
    let result = self.read_list_inner();
    self.depth -= 1;
    result
}
```

---

## 8.7  Integer Overflow Handling (R8.14)

### 8.7.1  Fixnum Arithmetic

All fixnum arithmetic operations check for overflow before returning:

```rust
// A8.03 — Checked fixnum addition
fn fixnum_add(a: i64, b: i64) -> TorclVal {
    match a.checked_add(b) {
        Some(sum) if is_fixnum_range(sum) => TorclVal::fixnum(sum),
        _ => promote_to_bignum(a, b, Op::Add),
    }
}
```

The `is_fixnum_range` check ensures the result fits in 61 bits
(−2^60 .. 2^60 − 1). Overflow in any direction promotes to bignum.

### 8.7.2  Promotion Semantics

- `FIXNUM + FIXNUM` → `FIXNUM` or `BIGNUM`
- `FIXNUM * FIXNUM` → `FIXNUM` or `BIGNUM`
- `BIGNUM op BIGNUM` → `BIGNUM` (may demote back to fixnum if result
  fits)
- No operation silently wraps. This applies to all arithmetic
  operators: `+`, `-`, `*`, `TRUNCATE`, `MOD`, `REM`, `ASH`, `LOGAND`,
  `LOGIOR`, `LOGXOR`.

---

## 8.8  Thread Safety Contracts (R8.15, R8.16)

### 8.8.1  Safety Classification

Every public runtime API is classified into one of three categories:

| Category | Symbol | Meaning |
|----------|--------|---------|
| Thread-safe | `[TS]` | May be called from any thread at any time |
| Requires lock | `[RL]` | Caller must hold the specified lock |
| Thread-local | `[TL]` | May only be called from the owning thread |

### 8.8.2  Key API Contracts

| API / Operation | Category | Notes |
|-----------------|----------|-------|
| `SYMBOL-VALUE` read | `[TS]` | Atomic load of value cell |
| `SETF SYMBOL-VALUE` (global) | `[TS]` | Atomic store; CAS for `DEFGLOBAL` |
| `INTERN` / `FIND-SYMBOL` | `[TS]` | Package rwlock acquired internally |
| `MAKE-PACKAGE` | `[TS]` | Global package registry lock |
| `GETHASH` / `(SETF GETHASH)` on synchronized table | `[TS]` | Per-bucket striped locks |
| `GETHASH` / `(SETF GETHASH)` on unsynchronized table | `[RL]` | Caller must hold table lock |
| Nursery allocation (`cons`, `make-array`, etc.) | `[TL]` | Per-thread TLAB, no lock needed |
| Old-gen allocation | `[TS]` | Region lock acquired internally |
| `FUNCALL` / `APPLY` | `[TS]` | Stateless dispatch |
| Condition signalling | `[TL]` | Handler bindings are thread-local |
| `COMPILE` | `[TS]` | Compiler is reentrant; code installation uses CAS |
| Stream I/O on shared stream | `[RL]` | Caller must synchronize (per ANSI spec) |

### 8.8.3  Lock Ordering (R8.16)

Locks MUST be acquired in strictly increasing level order. Acquiring a
lower-level lock while holding a higher-level lock is a bug, detected
by `debug_assert!` in debug builds.

| Level | Lock | Protects |
|-------|------|----------|
| 0 | Thread-local (no lock) | TLAB, handler bindings, dynamic env |
| 1 | Per-stream lock | Stream state and buffers |
| 2 | Hash-table/table-stripe lock | Hash-table entries |
| 3 | Global package registry | Package creation/deletion |
| 4 | Per-package rwlock | Package interning tables |
| 5 | Interned-string table | Canonical string identities |
| 6 | Compiler/code-cache lock | Code installation and debugger metadata |
| 7 | Profiling-data lock | Tier and developer-tool profiles |
| 8 | GC world/root lock | Heap coordination and relocatable side roots |
| 9 | Execution/scheduler registry | Threads, fibers, timers and I/O waits |
| 10 | Per-execution object | Native threads, fibers and connections |
| 11 | Image save mutex | Heap serialisation |

The authoritative table, same-level sub-order rules, and protocol-lock
exception are in §13.3.1. The checked wrappers are
`torcl_rt::lock_order::{OrderedMutex, OrderedRwLock}`.

### 8.8.4  Debug-Build Deadlock Detection (R8.21)

In debug builds, a thread-local lock-stack records every lock currently
held (by level). Attempting to acquire a lock at level ≤ the top of
the stack triggers:

```rust
// A8.04 — Lock-order assertion
fn assert_lock_order(requested_level: u8) {
    if cfg!(debug_assertions) {
        let stack = LOCK_STACK.with(|s| s.borrow().last().copied());
        if let Some(held) = stack {
            debug_assert!(
                requested_level > held,
                "Lock order violation: holding level {held}, requesting {requested_level}"
            );
        }
    }
}
```

Additionally, a background watchdog thread periodically scans the
lock-wait graph for cycles and logs a diagnostic if one is found.

---

## 8.9  Crash Recovery (R8.17)

### 8.9.1  Image Integrity

Every `.bimg` image file includes:

| Field | Size | Purpose |
|-------|------|---------|
| Magic number | 8 bytes | `TORCLIMG` ASCII |
| Version | 4 bytes | Image format version |
| Platform tag | 8 bytes | Architecture + OS hash |
| Heap checksum | 32 bytes | SHA-256 hash of the heap region |
| Code checksum | 32 bytes | SHA-256 hash of the compiled-code region |
| Total size | 8 bytes | Expected file size |

On load:

1. Verify magic number and version.
2. Verify file size matches `total_size`.
3. Compute SHA-256 of heap and code regions and compare against stored
   checksums (consistent with the image header checksum at §7.2.3).
4. Reject with `TORCL-EXT:CORRUPT-IMAGE-ERROR` on mismatch.

### 8.9.2  Graceful Handling of Corrupted Images

If image verification fails, TorCL:

1. Prints a clear diagnostic to stderr (file path, expected vs actual
   hash, which region failed).
2. Exits with a non-zero status code (exit code 78, `EX_CONFIG` per
   sysexits.h).
3. Does NOT attempt partial loading or recovery — a corrupt image is
   treated as unrecoverable.

### 8.9.3  Runtime Crash Handling

On unrecoverable faults (double-fault, GC invariant violation): write a
crash log to `~/.torcl/crash-<pid>-<timestamp>.log` (backtrace, register
state, GC phase, last 16 safepoints), best-effort flush open streams
(1 s timeout), then `_exit(134)`.

---

## 8.10  Fuzzing Strategy (R8.18)

### 8.10.1  Fuzz Targets

| Target | Input | Subsystem | Goal |
|--------|-------|-----------|------|
| `fuzz_reader` | Byte stream | Reader (§4) | Crash, hang, OOM in reader |
| `fuzz_reader_roundtrip` | Byte stream | Reader + Printer | `(equal (read (princ x)) x)` for printable objects |
| `fuzz_compiler` | S-expression AST (generated) | Compiler (§4) | ICE, miscompilation, panic |
| `fuzz_gc_stress` | Allocation pattern (size, count, lifetime) | GC (§3) | Heap corruption, dangling pointers |
| `fuzz_ffi_marshal` | Type + random value bytes | FFI (§2) | Crash in marshalling, type confusion |
| `fuzz_image_load` | Byte-mutated `.bimg` file | Image loader (§7) | Crash on corrupt image (should reject cleanly) |

### 8.10.2  Fuzzing Infrastructure

- **Engine:** `cargo-fuzz` (libFuzzer-based) for Rust targets.
- **Corpus:** seed corpus derived from `ansi-test` suite forms, SBCL
  test suite, and Quicklisp top-50 system source.
- **CI integration:** OSS-Fuzz enrollment for continuous fuzzing with
  coverage-guided mutation.
- **Sanitizers:** All fuzz targets compiled with AddressSanitizer (ASan),
  UndefinedBehaviorSanitizer (UBSan), and ThreadSanitizer (TSan for
  multi-threaded targets).

### 8.10.3  Crash Triage

Fuzz-discovered crashes are triaged as:

| Severity | Criteria | SLA |
|----------|----------|-----|
| P0 — Critical | Memory safety violation, exploitable | Fix within 24 h, no release until fixed |
| P1 — High | Panic/abort in safe code, data corruption | Fix within 1 week |
| P2 — Medium | Hang, excessive memory use | Fix within 1 month |
| P3 — Low | Cosmetic (bad error message, wrong source location) | Best-effort |

---

## 8.11  Configuration Summary

| Parameter | Default | Env Var | CLI Flag | Sandbox Override |
|-----------|---------|---------|----------|-----------------|
| Max heap size | 4 GiB | `TORCL_MAX_HEAP` | `--max-heap` | Per-sandbox |
| Stack depth limit | 10 000 | `TORCL_STACK_DEPTH` | `--stack-depth` | Per-sandbox |
| Reader nesting depth | 4 096 | `TORCL_READ_DEPTH` | — | Same |
| Reader max token length | 1 MiB | `TORCL_READ_TOKEN_MAX` | — | Same |
| `*READ-EVAL*` | NIL | — | `--read-eval` | Always NIL |
| `*READ-CIRCULAR*` | NIL | — | — | Same |
| CPU time limit (sandbox) | none | — | — | Per-sandbox |
| Alloc rate limit (sandbox) | none | — | — | Per-sandbox |
| Crash log directory | `~/.torcl/` | `TORCL_CRASH_DIR` | — | Same |

---

## 8.12  Test Strategy

### 8.12.1  Unit Tests

- Capability check logic: grant/deny for each capability bit.
- FFI pointer validation: null, misaligned, valid — expect correct
  `Result` variant.
- Fixnum overflow: boundary values at ±2^60, verify promotion.
- Lock-order assertions: intentionally violate order, confirm
  debug-build panic.
- Reader depth limit: nest to limit+1, expect `READER-ERROR`.
- Image checksum: corrupt one byte, expect rejection.

### 8.12.2  Integration Tests

- Sandbox escape attempts: try every known escape vector (`EVAL`,
  `COMPILE`, `LOAD`, FFI, `#.`, `RUN-PROGRAM`) inside sandbox with
  no capabilities; verify all are denied.
- Resource exhaustion: allocate in a loop inside sandbox with 16 MiB
  cap; verify `STORAGE-CONDITION` is signalled.
- Timeout: infinite loop in sandbox with 1-second CPU limit; verify
  `TIMEOUT-CONDITION` within 2 seconds wall-clock.
- Thread-safety: run `INTERN` from 8 threads concurrently; verify no
  crash and correct symbol identity.

### 8.12.3  Continuous Fuzzing

See §8.10. Fuzz targets run continuously via OSS-Fuzz with nightly
corpus sync and regression test generation from discovered crashes.
