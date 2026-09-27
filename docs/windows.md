# Windows x86-64 CLI

TorCL can be cross-built on Linux as a Windows console executable:
`target/x86_64-pc-windows-gnu/release/torcl.exe`.

This is an initial interpreter/T0 bytecode port. Native T1/T2/OSR compilation,
Win64 foreign-call adapters/callbacks, fiber switching, structured-exception
recovery, and socket streams/readiness are not supported yet. Forcing T2 safely
falls back to bytecode. DLL loading and symbol lookup are available; global
Unix-style symbol lookup requires choosing a DLL explicitly on Windows.

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

Copy `torcl.exe` to Windows and use it from a terminal:

```powershell
.\torcl.exe --no-init --eval "(+ 40 2)"
.\torcl.exe --no-init --load program.lisp
```

The standard Lisp bootstrap is embedded in the executable. No separate Lisp
installation is required. The tested executable imports Windows system DLLs;
no MinGW runtime DLL needs to be copied alongside it.

`*FEATURES*` includes `:WINDOWS`, `:X86-64`, and `:LITTLE-ENDIAN`. Home-directory
and default `.torclrc` discovery use `USERPROFILE` (with `HOME` as a fallback).
Use forward slashes inside Lisp path strings, for example `C:/work/demo.lisp`.
Absolute drive paths and relative paths are supported. Drive-relative paths
(`C:demo.lisp`), UNC shares, and full ANSI device-component semantics remain
follow-up work.
Saved images carry a Windows platform tag and cannot be exchanged with Linux
images. Ctrl+C and Ctrl+Break request cooperative Lisp interruption.

## Validate under Wine

Install Wine and Python 3.11 or newer. From a Linux session with the user
systemd bus available:

```sh
scripts/windows-port.sh test
```

This builds the release executable and creates a temporary, isolated Wine
prefix. It runs Windows memory/protection, timing/thread, DLL-lifetime and stack
budget tests, then the CLI functional tests in interpreter, bytecode, default,
and forced-T2 fallback modes. Additional checks cover `USERPROFILE` without
`HOME`, catchable recursive stack exhaustion, moving-GC stress with poison,
and compiled-file and heap-image round trips in fresh processes.

The GC stress probe uses `--no-bootstrap` and stresses every allocation in the
probe; the broader functional tests load the normal prelude. Tests use
`scripts/torcl-limited.sh` and honor `TORCL_MEM_MAX` and `TORCL_TIMEOUT`.
Wine validation does not replace testing on a native Windows installation;
native Windows CI and the remaining OS/ABI work are tracked in Beads.
