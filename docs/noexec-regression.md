# Linux no-executable-memory evidence

`crates/egcl/tests/noexec_memory_cli.rs` prepares a Linux regression for the
interpreter-only runtime profile. It supports little-endian x86-64 and AArch64.
The experimental `noexec.yml` workflow builds just that integration target once,
then runs the identified artifact inside the existing memory/time limiter. It
records source revision, input hashes, toolchain, kernel, Cargo artifact records,
executable hashes and output. It runs on pull requests and manual dispatch,
outside required CI aggregation, with `continue-on-error: true`.

Only the repository's existing `ubuntu-24.04` x86-64 runner is configured here.
There is no existing native AArch64 runner in the repository to reuse. The
AArch64 fixture source is included; execution on that architecture remains
unverified. No cross-emulator result substitutes for native kernel enforcement.

Each parent case launches a fresh copy of the already linked Rust harness. The
child installs seccomp **after its dynamic loader has run**, before calling
`egcl::run`, initializing Lisp, or creating its Lisp worker. Tier and stress
configuration is supplied by the parent process environment. Thread synchronization
is mandatory; a failure to install the filter fails the case. The filter checks
the syscall architecture and rejects mixed-ABI syscall numbers.

The filter kills the process for anonymous `mmap` with `PROT_EXEC`, and for
`mprotect` or `pkey_mprotect` requesting `PROT_EXEC`. It permits file-backed
executable mappings. This deliberately measures a narrower invariant than
“this process can never acquire executable code.” It is not a security sandbox,
does not test executable file loading, and does not establish Apple entitlement,
SDK, linker, simulator or physical-device compatibility.

The prepared assertions require:

- Each anonymous-map/protection-transition control to die from **SIGSYS**.
  An OOM kill, assertion failure, ordinary error, timeout or successful return
  cannot satisfy that requirement.
- Cold source bootstrap, loading a source file, hot integer loops, CLOS slot
  access, condition signaling/readers, READ/EVAL, COMPILE and specialized arrays
  to succeed under forced T0, normally and with post-init stress/poison. The hot
  function must actually remain at tier 0, and each child must print exactly one
  unquoted completion line.
- Where T1 is compiled into the build, an unfiltered control must actually reach
  tier 1 and return correct values. The same native-enabled program under the
  filter must die from SIGSYS. The denied mapping can arise during native-enabled
  bootstrap or later compilation; the fixture does not attribute it to a
  particular Lisp function. Builds configured without T1 omit this control and
  must not be reported as proving denial of an available native compiler.

The tests do not assert a moving-GC rooting repair. Stress compares semantic
results but supplies no physical-relocation proof. Their source and workflow
were prepared without running tests, builds, typechecks or kernel probes.
Successful future runs must be reported with their actual artifact and platform;
the existence of this fixture or a nonblocking workflow is not passing evidence.

The kernel's [seccomp filter documentation](https://docs.kernel.org/userspace-api/seccomp_filter.html)
describes architecture checks, inheritance, filter limitations and SIGSYS
termination. The filter uses the kernel UAPI and libc syscall constants rather
than a product-specific executable-memory test hook.
