# iOS runtime foundation

The initial iOS profile targets `aarch64-apple-ios` and
`aarch64-apple-ios-sim`. This is a platform foundation, **not a working iOS
application distribution or a verified device port**. Its source fixtures have
not yet been compiled or executed on either target.

The profile keeps the tree walker, bytecode interpreter, source bootstrap,
`READ`, `EVAL`, `LOAD`, and compilation to bytecode. Its runtime contract has a
T0 ceiling. `EGCL_FORCE_TIER=t1` or `t2` cannot enable native tiers;
`EGCL_FORCE_TIER=interp` still selects the tree walker. This ceiling is enforced
by the target build, rather than relying on an application setting an
environment variable before bootstrap.

Generated executable buffers are refused before mapping memory. The Darwin
mapping wrappers also reject executable protection in `mmap` and `mprotect` on
iOS; ordinary writable, read-only and inaccessible mappings remain available
for runtime storage and guards. These restrictions apply to EGCL's runtime
mapping APIs. They do not sandbox arbitrary native host code.

Both targets use the existing Darwin OS wrappers, signal-information layout
and ARM64 SDK context setter. Their Lisp features include `:UNIX`, `:DARWIN`
and `:IOS`. Device and simulator images receive separate platform identities,
which also distinguish them from macOS and Linux. Adding these identities does
not establish portable native-image delivery: bundle Lisp source and use the
existing embedded bootstrap for the first application runtime.

Foreign library lookup automatically uses the system-loader backend on iOS,
including builds without the optional `c-ffi` feature. Registered statically
linked native-library providers remain available through the existing library
and symbol APIs. The ELF loader is excluded. Scalar foreign calls now have a
source implementation of Apple's ARM64 convention: naturally aligned packed
fixed stack arguments, caller extension of narrow integer registers, and
promoted anonymous arguments in eight-byte stack slots. Calls use the existing
statically compiled trampoline and do not allocate executable adapters. This
implementation and its fixtures still require SDK/device validation. Aggregates
and Lisp callbacks remain outside the initial calls-only port's scope.

macOS retains its existing tiering, executable-memory and loader configuration.
macOS scalar calls share the Apple placement rules; aggregates and other
unsupported types are still rejected.

## Remaining work

The first application runtime still needs a thin host-linked library and
application scaffold: keep the host's main thread, start Lisp on an owned
worker using `egcl::run`, locate bundled source/assets, provide writable
application paths, and manage startup/shutdown. Subprocess creation needs an
explicit rejection policy in the shared process library. The scalar ABI source
fixtures need actual execution on Apple ARM64, including
narrow fixed stack arguments, signed/unsigned caller extension, mixed floating
point spills and real variadic calls. These validation prerequisites are not
satisfied by this foundation.

Build and link both targets using the appropriate Xcode SDK before claiming
support. Future validation must cover simulator and device startup, source
loading and bytecode compilation, the enforced tier ceiling even with native
tiering requested, denied executable mappings, storage/guard mappings, signal
recovery and GC stress with poison. Successful Linux tests or cross-target
Rust typechecking alone would not establish these properties on iOS.

Rust documents the targets and SDK requirements in its
[iOS platform notes](https://doc.rust-lang.org/rustc/platform-support/apple-ios.html).
Apple documents the distinct calling convention in
[Writing ARM64 code for Apple platforms](https://developer.apple.com/documentation/xcode/writing-arm64-code-for-apple-platforms).
