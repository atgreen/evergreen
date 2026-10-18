# Operating-system interface

These extensions expose process state, synchronous subprocess execution,
clocks, text encoding, package-local nicknames, funcallable instances,
digests and cipher blocks, and atomic operations. They are in `EGCL-EXT`. Their signatures differ from some
similarly named SBCL extensions; use this dictionary rather than substituting
package names in an SBCL call.

## Environment

### `egcl-ext:getenv` { #getenv }

**Function** `(egcl-ext:getenv name)` → string or `nil`

Returns the process environment variable named by the string `name`. Returns
`nil` when it is absent or cannot be represented by the host environment API.
The current evaluator does not enforce the specification's proposed `:env`
sandbox capability check on this operation.

```lisp
(or (egcl-ext:getenv "HOME") "no home directory")
```

### `egcl-ext:setenv` { #setenv }

**Function** `(egcl-ext:setenv name value &optional (overwrite t))` → `value`

Sets the process environment variable named by the string `name` to the
string `value`. With `overwrite` false, an existing variable is left unchanged.
The function returns `value` in either case, so it does not report whether
the variable was changed. This is the shape of POSIX `setenv`.

The change is made to the real process environment, so child processes started
afterwards and foreign code see it. EGCL's own environment reads and writes are
serialized with each other; foreign code that calls the C library's `getenv`
concurrently with a change is not synchronized with it.

A `program-error` is signalled when `name` is empty or contains `=` or a NUL
character, or when `value` contains a NUL character.

```lisp
(egcl-ext:setenv "APP_MODE" "test")
(egcl-ext:getenv "APP_MODE")  ; => "test"
```

### `egcl-ext:unsetenv` { #unsetenv }

**Function** `(egcl-ext:unsetenv name)` → `nil`

Removes the process environment variable named by the string `name`. Removing
a variable that is not set is not an error. Invalid names signal
`program-error`, as for [`setenv`](#setenv).

## Current directory

### `egcl-ext:getcwd` { #getcwd }

**Function** `(egcl-ext:getcwd)` → namestring or `nil`

Returns the process working directory as a string with a trailing slash, or
`nil` if the host query fails. This is process state; it is not the same thing
as a dynamically bound Lisp `*default-pathname-defaults*`.

## Subprocesses

### `egcl-ext:run-program` { #run-program }

**Function** `(egcl-ext:run-program command)` → exit-code, stdout, stderr

Runs a command synchronously and captures standard output and standard error.
A list of strings names the executable and its arguments directly. On Unix,
a string command is interpreted by `/bin/sh -c`.

The returned code is `-1` when the host provides no ordinary exit code.
Captured bytes are decoded into strings with replacement for invalid UTF-8.
A failure to launch the command signals a file error. A nonzero child exit
status is a returned status, not by itself a Lisp launch error.

This entry point does not accept SBCL's `run-program` keyword interface. It
returns strings rather than a process object or live stream. Streaming and
asynchronous process management are not supplied by this function.

```lisp
(multiple-value-bind (code output errors)
    (egcl-ext:run-program '("printf" "%s" "hello"))
  (list code output errors))
;; => (0 "hello" "")
```

Use a list when no shell expansion is needed. A string intentionally permits
shell syntax and must not be assembled from untrusted fragments as though it
were an argument list. Sandbox mode denies the operation.

## Command-line arguments

### `*command-line-args*` { #command-line-args }

**Variable** list of strings

Contains the application arguments after the CLI's `--` separator. Without
arguments it is `nil`.

```sh
egcl report.lisp -- first second
```

In that script the variable is `("first" "second")`. It does not include the
executable name, script pathname, or interpreter options.

### `egcl-ext:raw-command-line-arguments` { #raw-command-line-arguments }

**Function** `(egcl-ext:raw-command-line-arguments)` → list of strings

Returns the host argument vector, including the executable as its first item.
This is a lower-level interface used by integration libraries. It is distinct
from the parsed application arguments above.

See [Starting and stopping](../../starting.md) for invocation order and
init-file behavior, and [Compilation](../../compiler.md) for the compiler
observation functions previously grouped with these operating-system calls.

## Time

### `egcl-ext:get-precise-time` { #get-precise-time }

**Function** `(egcl-ext:get-precise-time)` → seconds, nanoseconds

Returns the current wall-clock time as a universal time in whole seconds and
the nanoseconds within that second. Both values come from one clock reading, so
they cannot straddle a second boundary as two separate calls could. This clock
follows changes to the system time. Passing any argument signals
`program-error`.

```lisp
(multiple-value-bind (seconds nanoseconds) (egcl-ext:get-precise-time)
  (+ seconds (/ nanoseconds 1000000000)))
```

### `egcl-ext:real-time-nanoseconds` { #real-time-nanoseconds }

**Function** `(egcl-ext:real-time-nanoseconds)` → integer

Returns a monotonic time in nanoseconds since an origin fixed for the process,
the same origin as `get-internal-real-time`. Use differences between two
readings to measure elapsed time; the value is not related to calendar time.
It is unaffected by changes to the system clock.

## String and octet conversion

### Text conversion { #octet-conversion }

**Function** `(egcl-ext:string-to-octets string &key (external-format :utf-8) (start 0) end null-terminate)` → byte vector

**Function** `(egcl-ext:octets-to-string vector &key (external-format :utf-8) (start 0) end)` → string

These functions convert UTF-8 or ASCII text for binary files and foreign buffers.
Encoding bounds count characters; decoding bounds count bytes. `end` defaults to
sequence length. `:null-terminate t` appends one zero byte when encoding; decoding
does not treat zero as a terminator, so pass the position of the first zero as
`:end` when reading a C string. Inputs are not modified and results are fresh.

Supported format names are `:utf-8`, `:utf8`, `:ascii`, and `:us-ascii`;
`:default` means UTF-8. ASCII encoding rejects non-ASCII characters. Both
decoders replace malformed input with U+FFFD, rather than signalling as SBCL's
default decoder does. Other external formats are not supported.

## Package-local nicknames { #local-nicknames }

A package-local nickname makes a name designate another package only while a
given package is current. The reader, `find-package`, `intern`, and
`find-symbol` consult the current package's local nicknames before global
package names. `defpackage` accepts a `:local-nicknames` option with entries of
the form `(nickname package)`, and `*features*` contains
`:package-local-nicknames`. The functions below are the programmatic interface;
they follow CDR 10, as SBCL's do.

### `egcl-ext:add-package-local-nickname` { #add-package-local-nickname }

**Function** `(egcl-ext:add-package-local-nickname local-nickname actual-package &optional (package *package*))` → package

Makes the string designator `local-nickname` designate `actual-package` when
`package` is current, and returns `package`. Adding the same nickname for the
same package again is not an error. A `package-error` is signalled when the
nickname already designates a different package in `package`, when either
package designator names no package, or when the nickname is `CL`,
`COMMON-LISP`, or `KEYWORD`.

```lisp
(defpackage :my-app (:use :cl))
(egcl-ext:add-package-local-nickname "THR" :egcl-thread :my-app)
```

### `egcl-ext:remove-package-local-nickname` { #remove-package-local-nickname }

**Function** `(egcl-ext:remove-package-local-nickname local-nickname &optional (package *package*))` → `t` or `nil`

Removes `local-nickname` from `package`. Returns `t` if it was a local
nickname there and `nil` otherwise.

### `egcl-ext:package-local-nicknames` { #package-local-nicknames }

**Function** `(egcl-ext:package-local-nicknames &optional (package *package*))` → alist

Returns the local nicknames of `package` as a fresh alist of
`("NICKNAME" . package)` entries sorted by nickname.

### `egcl-ext:package-locally-nicknamed-by-list` { #package-locally-nicknamed-by-list }

**Function** `(egcl-ext:package-locally-nicknamed-by-list package)` → list of packages

Returns the packages that have a local nickname for `package`.

## Funcallable instances

A class whose `:metaclass` is named `funcallable-standard-class` creates
funcallable instances. The metaclass is recognized by its symbol name in any
package, so `c2mop:funcallable-standard-class` and the bare name are
equivalent. Such an instance is a function as soon as it exists, whether or not
a function has been installed. These two functions are the AMOP interface used
by portability layers such as closer-mop.

### `egcl-ext:set-funcallable-instance-function` { #set-funcallable-instance-function }

**Function** `(egcl-ext:set-funcallable-instance-function instance function)` → function

Installs `function` as the function called when `instance` is called, and
returns `function`. A `type-error` is signalled when `instance` is not a
funcallable instance.

### `egcl-ext:funcallable-instance-function` { #funcallable-instance-function }

**Function** `(egcl-ext:funcallable-instance-function instance)` → function or `nil`

Returns the function installed in `instance`. Returns `nil` when no function
has been installed or when `instance` is not a funcallable instance.

## Digests and cipher blocks

These functions run in the runtime rather than in Lisp. Every octet argument
holds integers in 0–255. An element outside that range signals `type-error`
instead of being truncated. `sha256` and `crc32` accept any sequence. The
block primitives take simple vectors, which is what `make-array` returns for
an `(unsigned-byte 8)` or `(unsigned-byte 32)` element type.

### `egcl-ext:sha256` { #sha256 }

**Function** `(egcl-ext:sha256 octets)` → 32-octet vector

Returns the SHA-256 digest of the sequence `octets` as a fresh 32-element
vector.

### `egcl-ext:crc32` { #crc32 }

**Function** `(egcl-ext:crc32 octets)` → integer

Returns the CRC-32 of `octets`, using the polynomial of ZIP, gzip and PNG.

### Block primitives { #crypto-block-primitives }

**Function** `(egcl-ext:crypto-aes-encrypt-block round-keys n-rounds in in-start out out-start)` → `nil`

**Function** `(egcl-ext:crypto-aes-decrypt-block round-keys n-rounds in in-start out out-start)` → `nil`

**Function** `(egcl-ext:crypto-ghash-mul accumulator hash-key)` → `nil`

**Function** `(egcl-ext:crypto-sha256-block registers octets start)` → `nil`

**Function** `(egcl-ext:crypto-sha256-words registers words)` → `nil`

These are the inner functions of AES, GCM and SHA-256. EGCL's port of
Ironclad calls them in place of its portable Lisp versions, the way Ironclad
uses assembly routines on SBCL. Each one updates its destination in place,
using Ironclad's data layout. Applications should normally call Ironclad
rather than these functions.

- The AES functions transform the 16 octets of `in` from `in-start` and store
  the result in `out` from `out-start`. `round-keys` is a vector of 60
  `(unsigned-byte 32)` words, holding the expanded key in FIPS-197 word order,
  and `n-rounds` is 10, 12 or 14. For decryption, the round keys are in
  reverse round order, with InvMixColumns applied to every round but the first
  and last.
- `crypto-ghash-mul` replaces the 16-octet `accumulator` with its product
  with `hash-key` in GCM's GF(2^128). Both are big-endian, as in SP 800-38D.
- `crypto-sha256-block` compresses the 64 octets of `octets` from `start` into
  `registers`, a vector of eight `(unsigned-byte 32)` words.
  `crypto-sha256-words` compresses a message block held as the first sixteen
  big-endian words of `words`.

A destination or source that is too short signals `type-error`, and a wrong
argument count signals `program-error`.

## Atomic updates

### Atomic place updates { #atomic-place-updates }

**Macro** `(egcl-ext:cas place old new)` → previous value

**Macro** `(egcl-ext:atomic-incf place &optional (delta 1))` → previous value

**Macro** `(egcl-ext:atomic-decf place &optional (delta 1))` → previous value

`CAS` compares the current value of `place` with `old` using `EQ`. On a
match it stores `new`; otherwise it leaves the place unchanged. It returns
the value it observed in either case, not a success flag. Compare that result
with the expected value using `EQ` to determine whether the exchange succeeded.

| Place | `CAS` | `ATOMIC-INCF` / `ATOMIC-DECF` |
| --- | --- | --- |
| Special variable, written as a bare symbol | Yes | Yes |
| Structure-slot accessor, such as `(counter-value counter)` | Yes | Yes |
| `(svref simple-vector index)` | Yes | Yes |
| `(car cons)` or `(cdr cons)` | Yes | No |
| `(symbol-value symbol)` or `(symbol-plist symbol)` | Yes | No |
| `(slot-value instance slot-name)` for an instance-allocated slot | Yes | No |

These are not general replacements for `SETF` or `INCF`: arbitrary SETF
places and ordinary lexical variables are not supported. Symbol value cells
refer to the current dynamic binding when one exists, otherwise to the global
binding. Updating a thread-local dynamic binding does not update another
thread's binding. Place subforms and value/delta forms are evaluated once,
with place subforms evaluated before the values.

`ATOMIC-INCF` adds `delta`; `ATOMIC-DECF` subtracts it. Both the stored value
and `delta` must be fixnums or a `TYPE-ERROR` is signalled. Overflow signals
`ARITHMETIC-ERROR` rather than wrapping or promoting to a bignum. A failed
type or overflow check does not store a replacement value. Unlike `INCF`
and `DECF`, these macros return the value **before** the update.

```lisp
(let ((cell (cons :empty nil)))
  (list (egcl-ext:cas (car cell) :empty :ready)
        (egcl-ext:cas (car cell) :empty :other)
        (car cell)))
;; => (:EMPTY :READY :READY)

(let ((counter (vector 10)))
  (list (egcl-ext:atomic-incf (svref counter 0) 3)
        (egcl-ext:atomic-decf (svref counter 0))
        (svref counter 0)))
;; => (10 13 12)
```

Shared-cell updates use lock-free compare-and-exchange operations: success
has acquire/release ordering and a failed comparison has acquire ordering.
Arithmetic updates retry if another updater wins the comparison. This is not
a transaction across several places and does not make an arbitrary object
thread-safe. All concurrent writers of the same cell must follow an atomic
protocol; do not mix these operations with unsynchronized `SETF`, `INCF`, or
`DECF`, or assume an ordinary read supplies the required synchronization.

## Memory ordering

### Memory barriers { #memory-barriers }

**Function** `(egcl-ext:memory-barrier &optional (kind :full))` → `nil`

**Function** `(egcl-ext:load-barrier)` → `nil`

**Function** `(egcl-ext:store-barrier)` → `nil`

These operations order memory accesses on the calling native thread. `:read`
and `load-barrier` use an acquire fence; `:write` and `store-barrier` use a release
fence; `:full` uses a sequentially consistent fence. `:data-dependency` uses the
stronger acquire fence. Unknown kinds signal `program-error`.

For a shared-memory publication protocol, acquire after reading the publication
word and before consuming its payload; release after producing the payload and
before publishing its word. The publication word also needs an appropriate
atomic access protocol. A fence does not make an arbitrary memory access atomic
or wait for another thread to arrive.

On x86-64, calls with a literal kind compile to a dedicated bytecode operation
and native T1/T2 fences: `lfence` for `:read` and `:data-dependency`, `sfence`
for `:write`, and `mfence` for `:full`. `load-barrier`, `store-barrier`, and
`memory-barrier` with its default kind use this path too. Dynamic kinds retain
the checked runtime helper; other architectures retain the runtime path.
The interface promises memory ordering, not a particular instruction sequence
or call latency. Compiled files preserve the intrinsic without source fallback.

## Stream timeouts

### `egcl-ext:io-timeout` { #io-timeout }

**Condition type**; a subtype of `stream-error` and `simple-condition`

Signalled when a stream operation reaches an I/O deadline, such as a socket
read timeout. `stream-error-stream` returns the stream when it is known.

## Internal functions { #internal-functions }

These functions are implementation details used by EGCL's bootstrap and its
bundled ASDF. Their names begin with `%`, and they may change or disappear
without notice. They are listed so that a reader who meets
them in a backtrace or in `lib/` knows what they do; do not call them from
applications.

**Function** `(egcl-ext::%build-fingerprint)` → string

Returns an eight-digit hexadecimal tag derived from the size and modification
time of the running executable, or a fixed tag when the executable cannot be
examined. Bundled ASDF adds it to its Lisp version string so that each EGCL
build uses a separate compiled-file cache.

**Function** `(egcl-ext::%machine-type)` → string

Returns the build's target architecture as an uppercase string such as
`"X86-64"` or `"ARM64"`, or `"UNKNOWN"`. `machine-type` and `machine-version`
return this value.

**Function** `(egcl-ext::%save-core pathname &key executable)`

Writes a core image, or an executable with `:executable` true, and terminates
the process. It is a lower-level entry point with fewer options than
[`save-lisp-and-die`](images.md#save-lisp-and-die), which applications should
use instead. Other keyword arguments are evaluated and ignored.
