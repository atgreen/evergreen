# Fully static `bliss-cli` (no mandatory libc)

Bliss's runtime-internal OS services use **direct Linux syscalls** rather than
libc (see `crates/bliss-rt/src/syscall.rs` and `context.rs`), so a default build
links no libc of our own and can be produced as a fully static executable.
bliss-bca.5.

## What works statically

| Capability | Status | How |
|------------|--------|-----|
| GC heap / JIT pages | ✅ | `mmap`/`mprotect`/`munmap` syscalls |
| Fibers (M:N scheduler) | ✅ | hand-written x86-64 context switch (`context.rs`), no glibc `ucontext` |
| Networking / async I/O | ✅ | `epoll`/`poll` syscalls; fiber readiness waits |
| Signals | ✅ | `rt_sigaction` (with an own `rt_sigreturn` restorer), `tgkill` safepoints |
| Process/thread services | ✅ | `gettid`/`tgkill`/`exit_group`/`prlimit64` syscalls |

## What is unavailable statically

- **C-ABI dynamic FFI** (`load-foreign-library` / dlopen). A dynamic loader
  cannot exist in a static binary; it is gated behind the off-by-default
  `c-ffi` Cargo feature. In a static build the loader functions return an
  "unavailable in this build" error. (In-process `ffi_call` / marshalling — which
  need no loader — remain available.)

## Building

```sh
# Fully static (musl). No system musl toolchain needed for pure-Rust std.
rustup target add x86_64-unknown-linux-musl
cargo build --release -p bliss-cli --target x86_64-unknown-linux-musl

# With dynamic C-ABI FFI (NOT static): enable the feature on a glibc target.
cargo build --release -p bliss-cli --features bliss-rt/c-ffi
```

## Verifying it is static

```sh
BIN=target/x86_64-unknown-linux-musl/release/bliss-cli
file "$BIN"     # => ELF ... static-pie linked
ldd  "$BIN"     # => statically linked
```

## Supported targets

- `x86_64-unknown-linux-musl` — primary, verified static.
- Other arches (e.g. `aarch64`) build, but the portable fiber context switch is
  currently x86-64 only; on other arches fibers fall back to the inline stub
  until arch-specific `context.rs` asm is added.
