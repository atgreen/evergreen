# Changelog

## Unreleased

- `CLOSE :ABORT T`, and `WITH-OPEN-FILE` left by an error, delete the output
  file and leave any file it superseded untouched, instead of leaving an empty
  file in its place ([#1005](https://github.com/atgreen/evergreen/pull/1005)).

- `TYPEP` and type declaration checks find DEFTYPE definitions through a
  symbol index instead of scanning every definition ever made
  ([#1004](https://github.com/atgreen/evergreen/pull/1004)).

- Activation census diagnostics can report exact native-call admission reasons
  and installed argument metadata, keeping eligibility events separate from
  managed preparation counts
  ([#994](https://github.com/atgreen/evergreen/pull/994)).

- Inherited shared-slot writes and unbinds update the defining class cell when
  queried through a subclass ([#999](https://github.com/atgreen/evergreen/pull/999)).

- Empty NIL-element string comparisons honor active length for displaced and
  fill-pointer vectors ([#996](https://github.com/atgreen/evergreen/pull/996)).

## 0.0.12 - 2026-11-03

This release expands native compilation and improves closures, arrays, CLOS,
printing and library compatibility. The [full v0.0.12 notes](https://github.com/atgreen/evergreen/blob/main/docs/changelog/0.0.12.md)
retain every detailed entry and PR reference. [Published releases through v0.0.11](https://github.com/atgreen/evergreen/blob/main/docs/changelog/0.0.11-and-earlier.md)
are archived separately.

### Compatibility

- Rebuild older saved images for image format 20, and recompile bytecode from
  earlier major formats for bytecode major 3. Regenerate compiled files that
  collapsed distinct same-named `FLET`/`LABELS` functions
  ([#973](https://github.com/atgreen/evergreen/pull/973), [#828](https://github.com/atgreen/evergreen/pull/828), [#937](https://github.com/atgreen/evergreen/pull/937)).
- `DEFSTRUCT` accessors are ordinary functions: wrong object types signal
  `TYPE-ERROR`, `DEFMETHOD` no longer extends them, and included writers enforce
  their declared slot types ([#948](https://github.com/atgreen/evergreen/pull/948)).
- Portable FASLs reject unsupported captured function constants instead of
  saving process-local closure identities. Saved cores preserve supported
  function literals and captured state ([#848](https://github.com/atgreen/evergreen/pull/848)).

### Compiler and native execution

- Closures share compiled templates and tiering state while retaining distinct
  captures. Native code accesses heap lexical environments directly;
  noncapturing closures from one `LAMBDA` can reuse the same `EQ` function object
  ([#968](https://github.com/atgreen/evergreen/pull/968), [#971](https://github.com/atgreen/evergreen/pull/971), [#973](https://github.com/atgreen/evergreen/pull/973)).
- More lexical function creators, captured methods, destructuring bindings and
  multiple-value and handler forms compile instead of repeatedly entering the
  tree walker ([#981](https://github.com/atgreen/evergreen/pull/981), [#970](https://github.com/atgreen/evergreen/pull/970), [#967](https://github.com/atgreen/evergreen/pull/967)).
- Eligible x86-64 Linux native calls and function-object calls reuse ordinary
  bridges, storage and initialized contexts, reducing repeated managed
  activation setup while preserving redefinition and tier-promotion checks
  ([#989](https://github.com/atgreen/evergreen/pull/989), [#990](https://github.com/atgreen/evergreen/pull/990), [#992](https://github.com/atgreen/evergreen/pull/992)).
- The x86-64 native calling convention covers more `&REST`, `&OPTIONAL`, `&KEY`
  and multiple-value functions, including larger argument lists. Compiled
  `FUNCALL` targets with up to 15 arguments can stay native
  ([#980](https://github.com/atgreen/evergreen/pull/980)).
- Top-level and once-called loops can enter native code during their first call,
  including double-float loops. `TIME` now measures compiled execution too
  ([#637](https://github.com/atgreen/evergreen/pull/637), [#649](https://github.com/atgreen/evergreen/pull/649)).
- T2 lowers more numeric, bit, array, structure and builtin operations directly,
  sharing type, range and layout proofs while retaining runtime checks,
  precise GC roots and deoptimization
  ([#975](https://github.com/atgreen/evergreen/pull/975), [#948](https://github.com/atgreen/evergreen/pull/948), [#736](https://github.com/atgreen/evergreen/pull/736), [#657](https://github.com/atgreen/evergreen/pull/657)).
- Native compilation runs on background workers on x86-64 and s390x Linux.
  Small x86-64 native code objects share executable pages, reducing mapping
  pressure; `--heap-size` is honored again
  ([#662](https://github.com/atgreen/evergreen/pull/662), [#956](https://github.com/atgreen/evergreen/pull/956), [#953](https://github.com/atgreen/evergreen/pull/953)).
- AArch64, ppc64le and System Z gain more native vector, string, integer and
  floating-point operations. System Z fiber migration preserves execution-local
  state after native calls
  ([#760](https://github.com/atgreen/evergreen/pull/760), [#851](https://github.com/atgreen/evergreen/pull/851), [#853](https://github.com/atgreen/evergreen/pull/853), [#661](https://github.com/atgreen/evergreen/pull/661)).

### Common Lisp behavior

- Packed `(UNSIGNED-BYTE 8)` arrays use byte storage. Adjustable, displaced,
  fill-pointer and NIL-element arrays preserve storage, metadata and shared
  references through array operations, compiled files and saved images
  ([#701](https://github.com/atgreen/evergreen/pull/701), [#742](https://github.com/atgreen/evergreen/pull/742), [#865](https://github.com/atgreen/evergreen/pull/865)).
- `MAPCAN` and `MAPCON` join results destructively with `NCONC`; `NSUBST` and its
  variants modify existing tree cells while preserving retained aliases
  ([#965](https://github.com/atgreen/evergreen/pull/965), [#929](https://github.com/atgreen/evergreen/pull/929)).
- `FORMAT` and pretty printing share logical-block layout behavior and preserve
  circular references, print limits and callback state. Gray-stream `READ-LINE`
  correctly reports EOF instead of returning empty strings indefinitely
  ([#718](https://github.com/atgreen/evergreen/pull/718), [#888](https://github.com/atgreen/evergreen/pull/888), [#643](https://github.com/atgreen/evergreen/pull/643)).
- Recursive and custom readers preserve readtable syntax, shared labels and
  callback roots. `PACKAGE:NAME` requires an exported symbol and does not
  silently intern missing names
  ([#876](https://github.com/atgreen/evergreen/pull/876), [#837](https://github.com/atgreen/evergreen/pull/837), [#765](https://github.com/atgreen/evergreen/pull/765)).
- CLOS supports custom slot-definition classes and finalization hooks, standard
  generic documentation, and Common Lisp class precedence. Class renaming
  preserves class identity and existing registrations
  ([#826](https://github.com/atgreen/evergreen/pull/826), [#871](https://github.com/atgreen/evergreen/pull/871), [#696](https://github.com/atgreen/evergreen/pull/696), [#874](https://github.com/atgreen/evergreen/pull/874)).
- First-class lexical `CALL-NEXT-METHOD` and `NEXT-METHOD-P` retain their original
  continuations across redefinition. Generic functions named by uninterned
  symbols preserve their exact name identities
  ([#894](https://github.com/atgreen/evergreen/pull/894), [#905](https://github.com/atgreen/evergreen/pull/905)).
- Functions and compiled files preserve definition-site lexical functions,
  macros, special declarations and distinct uninterned binding names
  ([#820](https://github.com/atgreen/evergreen/pull/820), [#904](https://github.com/atgreen/evergreen/pull/904), [#859](https://github.com/atgreen/evergreen/pull/859), [#912](https://github.com/atgreen/evergreen/pull/912)).
- Condition construction and native callback errors preserve Lisp error handling
  across collection. Unreachable closures, including `PROGV` wrappers, and their
  captures can be reclaimed during normal GC
  ([#645](https://github.com/atgreen/evergreen/pull/645), [#644](https://github.com/atgreen/evergreen/pull/644), [#640](https://github.com/atgreen/evergreen/pull/640)).
- Type queries preserve package and class identity and distinguish more numeric
  and array layouts. Invalid `STRING` designators signal `TYPE-ERROR` with the
  original datum ([#838](https://github.com/atgreen/evergreen/pull/838), [#702](https://github.com/atgreen/evergreen/pull/702), [#981](https://github.com/atgreen/evergreen/pull/981)).
- `EQUAL`/`EQUALP` tables and `SXHASH` distinguish more list structure, and exact
  integer arithmetic avoids redundant rational normalization
  ([#974](https://github.com/atgreen/evergreen/pull/974), [#930](https://github.com/atgreen/evergreen/pull/930)).

### Interop, delivery and observability

- Static applications can expose native archives with `EGCL_STATIC_FOREIGN_MANIFEST`.
  `WITH-POINTER-TO-VECTOR-DATA` supports packed octet and double-float vectors
  through the existing copy-in/copy-out interface
  ([#887](https://github.com/atgreen/evergreen/pull/887), [#722](https://github.com/atgreen/evergreen/pull/722)).
- ARM64 foreign-call fixes cover stack alignment, natural-width stack arguments,
  narrow integers and variadic promotions. SDK/device and neighboring ARM64 FFI
  execution remain unvalidated
  ([#890](https://github.com/atgreen/evergreen/pull/890), [#900](https://github.com/atgreen/evergreen/pull/900)).
- `install-egcl-forks` includes audited ocicl libraries through maintained EGCL
  forks, with tested revisions and remaining capability limits documented
  ([#638](https://github.com/atgreen/evergreen/pull/638)).
- Statistical profiles include actual native Lisp frames and tiers. Opt-in GC
  timings and stack-overflow diagnostics expose runtime costs and overflow origins
  ([#931](https://github.com/atgreen/evergreen/pull/931), [#707](https://github.com/atgreen/evergreen/pull/707), [#944](https://github.com/atgreen/evergreen/pull/944)).

### Experimental features and limits

- Opt-in x86-64 Linux experiments include execution-owned T1 countdowns
  (`EGCL_EXECUTION_T1_HEAT=1`), call linkage (`EGCL_R13_CALL_LINKAGE=1`) and eager
  FASL T1 compilation (`EGCL_FASL_EAGER_T1=1`)
  ([#983](https://github.com/atgreen/evergreen/pull/983), [#982](https://github.com/atgreen/evergreen/pull/982), [#981](https://github.com/atgreen/evergreen/pull/981)).
- Keyword-default visibility, lazy native-frame publication and broader T1 loop
  entries remain experimental. Surrogate character codes are preserved in
  character/string storage, with stream, reader and formatting limitations
  ([#980](https://github.com/atgreen/evergreen/pull/980)).
- Initial iOS device/simulator support uses T0 without executable memory and
  includes a worker-host API, SwiftUI scaffold and Xcode project generator.
  Full application, generated-project, SDK and device execution remain unvalidated
  ([#899](https://github.com/atgreen/evergreen/pull/899), [#909](https://github.com/atgreen/evergreen/pull/909), [#910](https://github.com/atgreen/evergreen/pull/910)).
