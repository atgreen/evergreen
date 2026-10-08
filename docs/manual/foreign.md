# Foreign function interface

`EGCL-FFI` provides foreign pointers, explicit foreign storage, shared libraries,
scalar calls, and callbacks on supported targets. These interfaces are callable
from Lisp; the old extension inventory's “not installed” label is obsolete.
They are EGCL interfaces, not drop-in substitutes for every CFFI or SB-ALIEN
operation.

## Runtime and ABI requirements

Dynamic library loading requires a dynamic EGCL build with `egcl-rt/c-ffi`.
The Fedora native RPM and Android APK runtime provide dynamic builds. The
static Android CLI and default static musl CLI do not dynamically load libraries.
Foreign call and callback support are architecture-specific; ordinary Lisp
execution on a target does not establish FFI parity.

The caller supplies the exact C signature. The runtime cannot infer a prototype
from a symbol address. A wrong return type, argument type, calling convention,
or lifetime can corrupt a process despite valid Lisp syntax.

## Foreign types

Common scalar designators include `:int`, `:uint`, `:char`, `:uchar`, `:short`,
`:ushort`, `:long`, `:ulong`, `:int64`, `:uint64`, `:float`, `:double`, and
`:pointer`. `:void` is used for a return with no value.

Do not assume C `long` or a pointer is the same size on every target. Query the
runtime's size and alignment, and use fixed-width types when the C interface
specifies a fixed width.

### Size and alignment { #foreign-types }

**Functions**

```lisp
(egcl-ffi:foreign-type-size type)       ; size in bytes
(egcl-ffi:foreign-type-alignment type)  ; alignment in bytes
```

These report the runtime's layout for a supported foreign type. Do not use a
Lisp array's upgraded element type as proof that its storage has the same layout.

## Pointers and storage

### Pointer operations { #pointers }

**Functions**

```lisp
(egcl-ffi:pointerp object)
(egcl-ffi:make-pointer address)
(egcl-ffi:pointer-address pointer)
(egcl-ffi:pointer-eq pointer-a pointer-b)
(egcl-ffi:null-pointer)
(egcl-ffi:null-pointer-p pointer)
(egcl-ffi:inc-pointer pointer byte-offset)
```

A foreign pointer is an opaque object, not an integer address or a pointer to a
moving Lisp value. `make-pointer` wraps an address; it does not allocate storage
or establish ownership. `inc-pointer` changes the address by bytes, not elements.

### Allocation and release { #foreign-alloc }

**Functions** `(egcl-ffi:foreign-alloc bytes)` → pointer;
`(egcl-ffi:foreign-free pointer)`

Foreign storage has an explicit lifetime. Release the owning allocation once,
after C has stopped retaining or using it. Do not free an interior alias.
Freeing tracked storage invalidates its tracked aliases; double release and
access through a freed tracked alias are errors. A raw address supplied from
outside the allocator does not carry the same ownership information.

### Reading and writing { #mem-ref }

**Functions**

```lisp
(egcl-ffi:mem-ref pointer type &optional (offset 0))
(egcl-ffi:mem-set value pointer type &optional (offset 0))
(setf (egcl-ffi:mem-ref pointer type offset) value)
```

Offsets are bytes. The allocation must be large enough for the offset plus the
size of the value. Tracked allocations receive bounds and lifetime checks; a
foreign pointer is not permission to access arbitrary memory safely.

```lisp
(let ((p (egcl-ffi:foreign-alloc 8)))
  (unwind-protect
      (progn
        (setf (egcl-ffi:mem-ref p :int32) 21)
        (setf (egcl-ffi:mem-ref p :int32 4) 2)
        (* (egcl-ffi:mem-ref p :int32)
           (egcl-ffi:mem-ref p :int32 4)))
    (egcl-ffi:foreign-free p)))
;; => 42
```

### Vector access { #vector-data }

**Macro** `(egcl-ffi:with-pointer-to-vector-data (pointer vector &optional type) body...)`

Uses temporary foreign storage, copies the vector in, executes the body, copies
values back, and frees the storage. The default type is `:unsigned-char`.
The pointer is valid only inside the body. This is a copying interface, not a
promise to pin the Lisp vector in place. Cleanup also runs on nonlocal exit.

## Libraries and calls

### Library lifetime { #libraries }

**Functions**

```lisp
(egcl-ffi:load-foreign-library path)                 ; library object
(egcl-ffi:foreign-library-p object)                 ; generalized boolean
(egcl-ffi:foreign-symbol-pointer name &optional library)
(egcl-ffi:close-foreign-library library)
```

Supply a pathname understood by the platform's dynamic loader. A library object
is distinct from a foreign pointer. Supplying the library to symbol lookup
makes the lookup scope explicit; omitting it uses the default lookup scope.
Close the library only when no code can call its symbols or use its data.
Failures are reported through the FFI error interface.

### `egcl-ffi:foreign-call` { #foreign-call }

**Function**

```lisp
(egcl-ffi:foreign-call pointer return-type argument-types arguments
                        &optional fixed-count)
```

Calls a foreign entry point. `argument-types` and `arguments` are corresponding
lists: arguments are not supplied as Lisp rest arguments. For a variadic C
function, supply the number of fixed arguments as the final parameter so the
runtime can apply the appropriate ABI rules and promotions.

For example, with a library exporting `int twice(int)`:

```lisp
(let ((library (egcl-ffi:load-foreign-library "./libexample.so")))
  (unwind-protect
      (egcl-ffi:foreign-call
        (egcl-ffi:foreign-symbol-pointer "twice" library)
        :int '(:int) '(21))
    (egcl-ffi:close-foreign-library library)))
;; => 42
```

Build that example library on Linux with:

```c
int twice(int value) { return value * 2; }
```

```sh
cc -shared -fPIC example.c -o libexample.so
```

The C compiler and library must target the same architecture as the EGCL
runtime doing the call. The sample filename and command are Linux-specific.

## Callbacks

### Callback lifetime { #callbacks }

**Functions**

```lisp
(egcl-ffi:make-callback function return-type argument-types)
(egcl-ffi:callback-pointer callback)
(egcl-ffi:foreign-callback-p object)
(egcl-ffi:free-callback callback)
(egcl-ffi:callback-error callback)
```

`make-callback` returns a callback object; use `callback-pointer` to obtain the
entry pointer passed to C. Keep the callback alive until all C references and
invocations have ended. Freeing it while C may still call it is invalid.

Callback errors cannot unwind arbitrarily through C. The bridge records the
failure and returns a zero result to C; the enclosing foreign call can then
signal an FFI error. `callback-error` consumes saved diagnostic text, including
errors recorded on a foreign thread. Callback support must be checked for the
target architecture separately from scalar foreign calls.

On a runtime with callback support (x86-64 and s390x), a callback can be exercised through
the same call interface before giving it to a C library:

```lisp
(let ((callback (egcl-ffi:make-callback (lambda (x) (+ x 1)) :int '(:int))))
  (unwind-protect
      (egcl-ffi:foreign-call (egcl-ffi:callback-pointer callback)
                              :int '(:int) '(41))
    (egcl-ffi:free-callback callback)))
;; => 42
```

## Conditions and sandboxing

`egcl-ffi:ffi-error` is the public FFI condition. Invalid pointer ownership,
unsupported signatures, loader failures, and callback bridge errors can reach
this interface. Sandbox mode denies foreign access; using `funcall` instead of
a direct call does not bypass that policy.

Implementation reference: [Public FFI wrappers](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/boot.lisp).

## Java integration

The [Java integration chapter](java.md) covers the in-process HotSpot JVM,
the primary `JAVA` API, descriptor-based `EGCL-JVM` calls, Java interfaces
implemented by Lisp callbacks, and reference ownership. Java calls use the
checked JVM bridge rather than application-supplied JNI prototypes.
