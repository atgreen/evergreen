# Windows x86-64 CLI

EGCL can be cross-built on Linux as a Windows console executable:
`target/x86_64-pc-windows-gnu/release/egcl.exe`.

The port supports the interpreter, T0 bytecode, native T1 and optimizing T2
compilation, and on-stack replacement (OSR) from T0 to T1 and T1 to T2.
Native code preserves moving-GC roots, resumes bytecode at failed speculation
guards, and supports direct native calls. Unsupported compilation shapes retain
their lower tier. Structured-exception recovery is not supported yet.
DLL loading and symbol lookup are available; global
Unix-style symbol lookup requires choosing a DLL explicitly on Windows.

Foreign calls and callbacks use generated Win64 adapters. They support
integers, pointers, single/double floats, mixed register and stack arguments,
and variadic calls with C argument promotions. Callbacks retain Lisp closures,
participate in moving GC, admit foreign threads, and contain Lisp errors and
nonlocal exits before returning through C. Callback signatures are scalar.
Buffered foreign calls support structs, packed structs, and unions passed and
returned by value, including variadic calls. Indirect aggregate arguments use
private aligned copies so C cannot overwrite the caller's original object.

Generated adapters use write-then-execute protection, probe large stack frames,
and retain registered Win64 unwind metadata for their lifetime. Native T1 and
T2 functions register unwind ranges for their normal, compiled-register (T2),
and OSR entries. T2 probes large spill frames and temporary deoptimization buffers.

The runtime scheduler uses Windows-owned fiber stacks for suspension, carrier
migration, and cooperative preemption. Mutex, condition-variable, semaphore,
and timer waits release their carrier. Each fiber retains its registered GC
root list while suspended and after migration. Windows descriptor-readiness
integration and catchable native stack exhaustion remain unfinished.

## Build on Linux

Install Rust via rustup and the x86-64 MinGW GCC toolchain, then run:

```sh
rustup toolchain install 1.94.1 --profile minimal
rustup target add --toolchain 1.94.1 x86_64-pc-windows-gnu
scripts/windows-port.sh build
```

The script explicitly selects the Windows target instead of the workspace's
usual Linux musl target. Override `RUSTUP_TOOLCHAIN`, `CARGO_BUILD_JOBS`, or
`CARGO_TARGET_DIR` if needed. No container or Windows SDK is required for this
GNU cross-build. MSVC builds have not been validated.

Copy `egcl.exe` to Windows and use it from a terminal:

```powershell
.\egcl.exe --no-init --eval "(+ 40 2)"
.\egcl.exe --no-init --load program.lisp
```

The standard Lisp bootstrap is embedded in the executable. No separate Lisp
installation is required. The tested executable imports Windows system DLLs;
no MinGW runtime DLL needs to be copied alongside it.

`*FEATURES*` includes `:WINDOWS`, `:X86-64`, and `:LITTLE-ENDIAN`. Home-directory
and default `.egclrc` discovery use `USERPROFILE` (with `HOME` as a fallback).
Use forward slashes inside Lisp path strings, for example `C:/work/demo.lisp`.
Physical pathnames support absolute drives (`C:/work/demo.lisp`), drive-relative
paths (`C:demo.lisp`), root-relative paths, UNC shares (`//server/share/file`),
and ordinary relative paths. Backslashes and verbatim drive/UNC prefixes are
accepted and namestrings use forward slashes. The drive letter is the device
component, normalized to uppercase. UNC pathnames use the server as host and
the share as device; their directory is absolute. `MAKE-PATHNAME` rejects a
relative UNC directory and supplies a root when its directory is nil.
`MERGE-PATHNAMES` inherits directories only within the same volume.

Physical pathname equality, hashing and wildcard matching ignore ASCII case
while preserving original namestring spelling and wildcard captures. Non-ASCII
case equivalence and Windows device namespaces such as `//./` are not supported.
UNC parsing and reconstruction are tested; live network-share access has not
been validated.
Saved images carry a Windows platform tag and cannot be exchanged with Linux
images. Ctrl+C and Ctrl+Break request cooperative Lisp interruption.

## Networking and subprocesses

Stream operations retain exclusive ownership when a fiber parks or migrates.
Fibers waiting for that ownership park without blocking their carrier, and
stream handles, composite components, and returned Lisp values remain rooted
through waits and unlocks. Native runtime mutex guards pin their owning fiber
until the guard is released.

TCP client and accepted connections are owned bidirectional octet streams.
The existing `egcl::%socket-connect`, `%socket-listen`, `%socket-accept`,
`%socket-read-timeout`, and `%socket-wait-for-input` primitives work on Windows.
Buffered input, readiness timeouts, EOF, `LISTEN`, and explicit `CLOSE` are
supported; closing releases the socket without waiting for GC. Socket position
and length queries return `NIL`. Windows sockets are not Unix descriptors, so
`%socket-fd` returns `NIL`; descriptor-based fiber waits remain unsupported.
Socket readiness uses [Winsock WSAPoll](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsapoll).

`egcl-ext:run-program` runs synchronously and returns three values: exit status,
stdout, and stderr. A list supplies an executable and its arguments directly;
a string supplies shell syntax to `COMSPEC` (normally `cmd.exe`) with
`/D /S /C`. Both output pipes are drained concurrently, and stdin receives EOF.
Captured bytes are decoded as UTF-8 with replacement for invalid sequences.
Waiting publishes native callers' GC roots and parks unpinned fibers, allowing
other fibers to run. Pinned callers follow the configured blocking policy;
the error policy rejects the operation before starting a child.
Interactive subprocess streams, asynchronous process management, and alternate
console code-page decoding are not provided by this API.

The standard-library backend can now own child stdin/stdout/stderr pipes as
buffered character or octet streams. Blocking pipe I/O runs on a helper for
managed fibers and admits GC for native callers. Pipe position and length
queries return `NIL`; closing stdin sends EOF, and closing an input pipe cancels
outstanding readiness waits. Windows pipe readiness uses
[PeekNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe).
The Rust `egcl_stdlib::process::launch_program` API returns an owned `Process`
with separately transferable stdin/stdout/stderr pipes. `try_wait` observes exit
without blocking; `wait` supports an optional timeout and retains exit status
for repeated or concurrent waiters. A timeout leaves the child running.
`terminate` requests immediate termination of the child, not its descendants;
call `wait` separately to observe completion. Callers must drain stdout and
stderr concurrently when either may fill, and close stdin when the child needs
EOF. Waiting never drains or closes a pipe implicitly.

Dropping the owner closes pipes still held by it without killing or waiting for
the child. A native reaper retains ownership until exit (or a permanent OS wait
error). This initial backend uses one reaper thread per live child and polls
status every two milliseconds; managed waiters suspend through the scheduler.
The Lisp process object and launch/wait/terminate interface remain to be wired
to this backend.

`CLOSE` reports output-flush failures and leaves the stream open. After correcting
the cause, retry `FINISH-OUTPUT` or `CLOSE`; successfully written bytes are not
sent again. Use `(close stream :abort t)` to discard pending output and release
the handle when retry is inappropriate.

## Validate under Wine

Install Wine and Python 3.11 or newer. From a Linux session with the user
systemd bus available:

```sh
scripts/windows-port.sh test
```

This builds the release executable and creates a temporary, isolated Wine
prefix. It runs Windows memory/protection, timing/thread, DLL-lifetime and stack
budget tests, then the CLI functional tests in interpreter, bytecode, default,
and forced-T2 modes. Native tests assert T1/T2 promotion and OSR entry,
compare arithmetic and deoptimization against interpretation, exercise direct
calls and error propagation under GC stress, and unwind normal/OSR frames and
temporary direct-call stack saves. T2 tests check large deoptimization buffers
and OS unwinding at every decoded instruction boundary across multiple frame
sizes, including stack probes. Runtime tests also execute small Win64 functions,
check read/execute protection and release, and exercise the OS unwinder at
generated prologue/epilogue boundaries. Additional checks cover `USERPROFILE` without
`HOME`, catchable recursive stack exhaustion, moving-GC stress with poison,
TCP connect/accept, buffering, timeouts, EOF, immediate close, split UTF-8
nonblocking reads, subprocess quoting and concurrent output pipes,
pathname components, construction, merging, wildcard searches, equality/hashing,
and compiled-file and heap-image pathname round trips in fresh processes.

MinGW-built C DLL fixtures check scalar and aggregate foreign calls, varargs, callbacks,
large frames and all three stack-allocation unwind encodings. Managed callback
tests cover moving GC, foreign-thread admission, nested entry and error
containment; CLI tests exercise allocating Lisp closures and checked aggregate
buffers through C DLL calls. Aggregate tests also check packed objects ending
at guard pages, hidden return pointers, small aggregate register returns, and
alignment and isolation of indirect copies.

The GC stress probe uses `--no-bootstrap` and stresses every allocation in the
probe; the broader functional tests load the normal prelude. Tests use
`scripts/egcl-limited.sh` and honor `EGCL_MEM_MAX` and `EGCL_TIMEOUT`.
Wine validation does not replace testing on a native Windows installation;
native Windows CI and the remaining OS/ABI work are tracked in Beads.
