# torcl-jvm

An experimental, in-process Java interface for TorCL. Start HotSpot, construct
Java objects, call methods, and implement Java interfaces with Lisp functions.
The two runtimes keep their own heaps and collectors. This is a checked API
boundary, not a sandbox for untrusted Java or native code.

The initial target is **native x86-64 Linux with glibc**. Use a TorCL built from
this checkout with `torcl-rt/c-ffi`; the static musl executable and Android ART
are not supported. No containers or phone are involved.

## Build and load

Install a JDK (17 or newer, including `javac` and JNI headers), a C compiler,
Make and Python 3. Select the JDK root containing `bin/javac`, `include/jni.h`,
and `lib/server/libjvm.so`:

```sh
export JAVA_HOME=/path/to/jdk
cargo build --target x86_64-unknown-linux-gnu --features torcl-rt/c-ffi -p torcl
make -C lib/torcl-jvm
scripts/torcl-limited.sh target/x86_64-unknown-linux-gnu/debug/torcl --no-init
```

From the repository root:

```lisp
(require :asdf)
(asdf:load-asd (truename "lib/torcl-jvm/torcl-jvm.asd"))
(asdf:load-system :torcl-jvm)
(defparameter *jvm* (java:start-jvm :options '("-Xmx256m")))

(java:static "java.lang.Integer" "parseInt" "42")
;; => 42

(java:with-scope ()
  (let ((items (java:new "java.util.ArrayList")))
    (java:call items "add" "hello")
    (java:call items "add" 42)
    (java:to-list items)))
;; => ("hello" 42)
```

`JAVA` (the nickname of `TORCL-JAVA`) is the **primary API**, loaded by the
`torcl-jvm` ASDF system. The descriptor-level `TORCL-JVM` package remains
available for existing code and precise low-level calls. Loading diagnoses an
existing, unrelated package named `JAVA` rather than modifying it.

ASDF loads the Lisp API. The first `start-jvm` runs Make if necessary and loads
the private bridge; its source directory must be writable for an initial build.
`:java-home` overrides `JAVA_HOME`, which overrides Java found on `PATH`.
`:classpath` is a list of directory/JAR names; `:options` is a list of JVM option
strings. Options are trusted configuration, not an isolation boundary. The
bridge supplies `-Xrs` so TorCL retains responsibility for termination signals.
A bad/missing JDK reports a condition; it does not install tools automatically.

## Calls, overloads, and bindings

Java names are case-sensitive strings. Class arguments can also be Java `Class`
proxies or symbols registered with `define-class`. Aliases and named functions
can be defined before the JVM starts:

```lisp
(java:define-class array-list "java.util.ArrayList")
(java:define-call parse-int ("java.lang.Integer" "parseInt")
  :static t :parameters ("java.lang.String") :returns :int)

(mapcar #'parse-int '("10" "20"))     ; => (10 20)
(java:verify 'parse-int)              ; resolve without invoking
(java:describe-class 'array-list)     ; list public member descriptions
```

An instance binding takes the receiver first. Omitting `:parameters` selects
an overload at each call; explicit parameters and an optional `:returns` check
a specific method. `verify` requires explicit parameters. Member resolution is
lazy, and its cache keys include actual Java Class identities and argument types.
Caches are cleared at shutdown. Only public constructors, methods and fields
are exposed; the bridge does not suppress Java access checks.

`new`, `call`, and `static` select applicable overloads without trying calls.
Primitive widening and reference assignability are considered before boxing or
unboxing. The unique most-specific method wins. Ambiguity signals
`java:ambiguous-call` with argument types and candidate signatures. Use an exact
selector or `as` to disambiguate:

```lisp
(java:call items '("remove" :int) 1)                    ; remove index
(java:call items '("remove" "java.lang.Object") 42)     ; remove Integer value
(java:static "java.lang.Math" "abs" (java:as :long -42))
(java:as "java.lang.String" java:+null+)                ; typed null argument
```

`as` is an argument annotation, validated when used in a call. Integer narrowing
requires an explicit annotation and is range-checked. Floating-point values
are never silently truncated to integers. Varargs are not spread automatically;
pass an explicitly constructed Java array.

| Lisp argument | Java type |
|---|---|
| Signed 32-bit integer | `int`, boxing to `Integer` when needed |
| Larger signed 64-bit integer | `long`, boxing to `Long` when needed |
| Single/double float | `float` / `double` |
| Character | `char`, one UTF-16 code unit |
| `nil`, `t` | `boolean` false, true |
| `java:+null+` | Java `null`, distinct from false |
| String | Java String, including supplementary characters and embedded NUL |
| Java proxy | Its actual reference type; no primitive coercion unless selected |

Returned strings and primitive wrappers become Lisp values. Other results are
owned Java proxies. **`new` always returns a proxy**, including String and wrapper
constructors. Java void results become NIL; Java null becomes `+null+`.
Unpaired UTF-16 surrogates signal a condition instead of producing invalid text.
`(java:find-class name loader)` preserves explicit class-loader identity.

## Scopes and ownership

Every returned proxy owns a JNI global reference. `with-scope` releases all
references created by high-level calls in its dynamic body, including intermediate
results, in reverse order on normal return or nonlocal exit. References supplied
by the caller remain untouched. Nested scopes own only their own new references.

A plain returned proxy expires with its scope. `retain` creates an independent
strong reference that escapes; release it explicitly later:

```lisp
(defparameter *items*
  (java:with-scope ()
    (java:retain (java:new "java.util.ArrayList"))))
(java:call *items* "add" "persistent")
(java:release *items*)
```

Outside a scope, each returned proxy belongs to the caller. There are no
finalizers: unreleased references retain Java objects and prevent shutdown.
Repeated release is harmless; using a released proxy signals `jvm-error`.
`same-object-p` compares Java identity. Releasing a reference does **not** call
Java `close()`. Use `with-resource` for a resource you own:

```lisp
(java:with-resource (reader (java:new "java.io.StringReader" "hello"))
  (java:call reader "read"))
```

That macro calls `close()` and then releases the reference, even on error. A
cleanup error cannot replace a pending error or nonlocal exit from the body. Scopes
attempt all their releases; a failed release (for example, revoking a currently
active callback) signals a condition. Arrange for asynchronous callback work to
finish before dropping its last owner. Cross-heap cycles still require explicit
release; the two collectors are independent.

## Java calling Lisp

```lisp
(java:with-scope ()
  (let ((increment (java:lambda "java.util.function.IntUnaryOperator" (x)
                     (java:static "java.lang.Math" "addExact" x 1))))
    (java:call increment "applyAsInt" 41)))
;; => 42

(java:with-scope ()
  (let ((comparator (java:implement "java.util.Comparator"
                      ("compare" (a b) (- (length a) (length b))))))
    (java:call comparator "compare" "a" "abc")))
;; => -2
```

`lambda` requires a public single-abstract-method interface. `implement` requires
a clause for every abstract method; argument lists have fixed arity. Overloaded
methods use exact selectors, for example `(("apply" :int) (x) (1+ x))`.
Missing, duplicate, ambiguous, and wrong-arity clauses fail before registration.
Default interface methods execute their Java implementation. The proxy's
`equals`, `hashCode`, and `toString` use identity semantics without calling Lisp.
`JAVA:LAMBDA` is a separate symbol; ordinary `CL:LAMBDA` is unchanged.

Callbacks can run on Java-created native threads, including nested
Lisp → Java → Lisp → Java calls. Every invocation has its own dynamic scope.
Object arguments are borrowed for that invocation; retain one to keep it.
Returned objects are preserved before invocation cleanup, so returning a freshly
constructed scoped object or a borrowed argument is supported.

Retaining a callback keeps **both** its Java object and its rooted Lisp closure
alive. References returned back through Java also share that ownership. The last
Lisp owner revokes the native trampoline. A Java-only reference does not keep a
Lisp registration alive: invoking a revoked proxy throws a Java exception.
An active callback cannot be revoked until it returns.

## Collections, arrays, and fields

`to-list` explicitly copies an `Iterable`; object-valued elements belong to the
current scope or caller. It does not create a live Lisp view of Java storage.
Arrays remain Java objects:

```lisp
(java:with-scope ()
  (let ((values (java:new-array "java.lang.String" 2)))
    (setf (java:array-ref values 0) "hello"
          (java:array-ref values 1) "world")
    (java:static "java.lang.String" '("join" "java.lang.CharSequence"
                                    "[Ljava.lang.CharSequence;")
                 " " values)))
;; => "hello world"
```

`array-length` returns the length. `field` and `static-field` read public fields;
both, and `array-ref`, support SETF. Writes use the same checked conversions as
method calls. Java access/finality rules still apply.

## Conditions and the descriptor API

`java:java-error`, a subtype of `java:jvm-error`, represents Java exceptions.
Lisp callback errors become Java `IllegalStateException`; they never unwind
through Java frames. `error-message` returns the diagnostic (limited to 4095
UTF-16 units). Throwable identity and stack traces are not retained.

```lisp
(handler-case (java:static "java.lang.Integer" "parseInt" "oops")
  (java:java-error (condition)
    (format t "Java failed: ~A~%" (java:error-message condition))))
```

Existing `TORCL-JVM` calls still take explicit JVM descriptors and retain their
original conversion rules: Lisp integers box as Long, with range checking for
primitive integer parameters. User descriptors are checked by Java reflection,
never used as unchecked JNI signatures. Low-level results are **not** enrolled
in `java:with-scope`; release them or use `torcl-jvm:with-java-objects`:

```lisp
(torcl-jvm:with-java-objects
    ((items (torcl-jvm:new "java.util.ArrayList" "()V")))
  (torcl-jvm:call items "add" "(Ljava/lang/Object;)Z" "hello")
  (torcl-jvm:call items "get" "(I)Ljava/lang/Object;" 0))
```

`torcl-jvm:implement` retains its `(method-name &rest arguments)` callback
protocol. Its copies now share callback ownership as described above. Weak
references remain available through `torcl-jvm:weak-reference`; `promote` returns
an independently owned strong proxy or NIL if collected. Release weak references
too. Use the high-level interface for overload-aware callback dispatch.

After releasing references and callbacks:

```lisp
(java:stop-jvm *jvm*)
```

## Process lifecycle

Only one JVM session is supported per process. Repeated start, restart after
stop, and retries after a failed JVM creation attempt signal conditions. `stop-jvm` refuses outstanding references, callbacks,
or active calls. For an owned JVM it waits up to `:timeout` seconds (default 10)
for shutdown. A timeout leaves shutdown pending; call `stop-jvm` again to wait.
It cannot forcibly stop non-daemon Java threads. Repeated completed stop is safe.
The native library is never unloaded.

`:attach t` explicitly joins an existing JVM; stopping that session detaches
the bridge and does not destroy the host JVM. Classpath/options apply only when
creating a VM. A JVM started **before TorCL initialization** requires the JDK's
`libjsig.so` preloaded at process launch, ahead of the embedding library. Late
loading libjsig is insufficient. TorCL refuses detected JVM-first initialization
without signal interposition. Arbitrary third-party runtime embeddings are not
certified by this package.

Image saving is permanently inhibited once bridge preparation begins, even if
startup fails or the JVM is later stopped. Save an image **before loading this
package**, then load it and start Java after restoring. Live native VM state and
the package's mutex are not image-portable. Do not fork a process containing a
live JVM except through an immediately-executing process-spawn facility.
Calls from switched fiber stacks are rejected; use ordinary native threads.

The initial API does not implement CLOS Java metaclasses, MethodHandle call-site
optimization, JVM bytecode compilation, cross-heap cycle collection, Java virtual
thread guarantees, or JVM resurrection from saved images.

## Validation

Run from the repository root, using the JDK selected above:

```sh
lib/torcl-jvm/tests/run.sh
lib/torcl-jvm/tests/guest.sh
TORCL_GC_STRESS=1000 TORCL_GC_POISON=1 TORCL_GC_VERIFY=1 \
  TORCL_TIMEOUT=600 lib/torcl-jvm/tests/run.sh
```

The runner builds its Java fixture and tests the actual TorCL executable with
`-Xcheck:jni`, memory/time limits and a required completion marker. Override
`TORCL_JVM_BIN` for another freshly built native executable. It covers calls,
Unicode, descriptors, inferred and exact overloads, canonical boxing, lazy
bindings, constructors, arrays/fields, scope cleanup, callback ownership and
overloads, weak references, Java threads, nested callbacks, revocation,
exceptions, package conflicts, image rejection and shutdown.
Full API loading at `TORCL_GC_STRESS=1` exceeded the 600-second test budget;
this validation limitation is tracked as `bliss-6imi7`. The runtime callback
suite and both coexistence startup orders do pass every-allocation stress.
The separate [coexistence probe](../../tools/jvm-probe/README.md) checks process
startup orders and signals; the [design contract](../../docs/design/jvm-coexistence.md)
records the architecture and original findings.
