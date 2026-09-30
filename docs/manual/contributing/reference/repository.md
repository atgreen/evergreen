# Repository map

| Path | Responsibility |
| --- | --- |
| `crates/egcl-rt` | Values, heap, GC, safepoints, threads, fibers, FFI, images, OS services |
| `crates/egcl-compiler` | Reader, macro expansion, IR, optimization, allocation, code emission, tier policy |
| `crates/egcl-stdlib` | Standard-library behavior: packages, sequences, streams, CLOS, conditions, FORMAT |
| `crates/egcl` | CLI, REPL, loading, evaluator, bytecode integration, native bridges |
| `crates/egcl-android` | Android NativeActivity runtime |
| `lib/` | Lisp prelude and bundled library sources |
| `packaging/android/` | Android runtime build and application templates |
| `packaging/fedora/` | Container-free RPM build, launchers, and validation |
| `spec/` | Staged technical specification |
| `docs/manual/` | Published-manual source |
| `docs/design/` | Implementation notes and design records |
| `scripts/` | Build, validation, and development tools |
| `.beads/` | Task-tracking integration |

## Specification entry points

- [Index](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/INDEX.md): chapter and requirement map.
- [Conventions](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/conventions.md): requirement levels and staging rules.
- [Stages](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/stages.json): current stage and gate definitions.

Only MUST requirements assigned at or below the current stage are gated.
The current stage is 5; later requirements remain part of the design roadmap.

## Library compatibility ports

Compatibility changes belong in maintained forks, using `#+egcl`, `#-egcl`,
or ASDF feature conditions. Tests import pinned fork revisions through ocicl's
`git+URL@SHA` support and commit the corresponding `ocicl.csv`. Edited download
caches are not durable port sources. See
[ports/README.md](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/ports/README.md).
Repository policy requires explicit authorization for upstream submissions.
