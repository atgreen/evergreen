# Static EGCL and native libraries

The default Linux target is `x86_64-unknown-linux-musl`. Runtime OS services use
direct syscalls; Rust's standard library and native dependencies determine the
remaining link inputs. A musl build can produce a static PIE executable.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release -p egcl --target x86_64-unknown-linux-musl
file target/x86_64-unknown-linux-musl/release/egcl
ldd target/x86_64-unknown-linux-musl/release/egcl
```

## Link a foreign static archive

Set `EGCL_STATIC_FOREIGN_MANIFEST` when building EGCL. The build links the
archives into the executable and registers the listed C symbols before loading
an image. Lisp uses the ordinary foreign-library and foreign-call interfaces.

For example, create `demo.c`:

```c
int demo_counter = 7;
int demo_add(int x, int y) { return x + y; }
```

Build a regular archive with a C compiler targeting the same architecture and
ABI as EGCL. This dependency-free example uses `cc`; libraries using libc need
a toolchain and runtime compatible with the selected Rust target.

```sh
cc -fPIC -c demo.c -o demo.o
ar crs libdemo.a demo.o
```

Create `demo.foreign` beside the archive:

```text
version = 1
library = demo
alias = libdemo.so
archive = libdemo.a
symbol = demo_add
symbol = demo_counter
```

Then build:

```sh
EGCL_STATIC_FOREIGN_MANIFEST="$PWD/demo.foreign" \
  cargo build --release -p egcl --target x86_64-unknown-linux-musl
```

In that executable:

```lisp
(defparameter *demo* (egcl-ffi:load-foreign-library "demo"))
(egcl-ffi:foreign-call
  (egcl-ffi:foreign-symbol-pointer "demo_add" *demo*)
  :int '(:int :int) '(20 22))                 ; => 42
(egcl-ffi:mem-ref
  (egcl-ffi:foreign-symbol-pointer "demo_counter" *demo*) :int) ; => 7
```

The manifest is UTF-8, with one `key = value` per line and optional blank lines
or whole-line `#` comments. Values can contain spaces; they are not quoted.
Start each library with `library`, followed by one or more `archive` and
`symbol` entries and optional `alias` entries. Archive paths are relative to the
manifest. Library names and aliases must be unique across the manifest. Only
listed exports are exposed through EGCL's static symbol table. Missing exports
fail at native link time. Thin archives are rejected.

Archives are linked whole, preserving native initialization and finalization
objects. Constructors run at process startup and destructors at process exit;
opening and closing a Lisp handle does not rerun them or unload linked code.
Closing invalidates that handle, and reopening creates a fresh handle. Static
providers take precedence over file and embedded-library lookup under their
registered names. Their listed symbols also participate in default symbol lookup
without an open handle. This does not add dynamic-loader exports for plugins.

The executable needs neither the manifest nor archive files at runtime. Saved
library handles reopen against its linked providers when an image is restored.
As with other foreign libraries, look up symbol pointers again after restore:
foreign pointers are cleared in saved images. Foreign globals are native process
state, not Lisp image state, and initialize anew in each process.

Archive bytes, names, aliases and export lists participate in the native runtime
identity. A matching-runtime source rebuild propagates the original manifest
configuration and rejects changed inputs. Such a rebuild needs the original
manifest and archives on the build host; an already built executable does not.
Changing linked code requires rebuilding the executable. ZIP replacement of
embedded shared libraries does not replace linked archives.

## Shared libraries

A static executable can contain a loader. The default x86-64 and AArch64 Unix
builds use a Rust ELF loader for compatible shared objects, with a limited host
symbol surface. This is distinct from native archive linking. Embedded shared
objects currently need the `c-ffi` backend. The system `dlopen` backend is enabled
with `egcl-rt/c-ffi`; for example, on glibc Linux:

```sh
cargo build --release -p egcl --target x86_64-unknown-linux-gnu \
  --features egcl-rt/c-ffi
```

Foreign call and callback ABI support is target-specific. Linking an archive
does not add support for otherwise unsupported signatures or architectures.
