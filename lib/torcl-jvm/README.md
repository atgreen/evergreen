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
(defparameter *jvm* (torcl-jvm:start-jvm :options '("-Xmx256m")))

(torcl-jvm:call-static "java.lang.Integer" "parseInt"
                      "(Ljava/lang/String;)I" "42")
;; => 42

(torcl-jvm:with-java-objects
    ((list (torcl-jvm:new "java.util.ArrayList" "()V")))
  (torcl-jvm:call list "add" "(Ljava/lang/Object;)Z" "hello")
  (torcl-jvm:call list "get" "(I)Ljava/lang/Object;" 0))
;; => "hello"
```

ASDF loads the Lisp API. The first `start-jvm` runs Make if necessary and loads
the private bridge; its source directory must be writable for an initial build.
`:java-home` overrides `JAVA_HOME`, which overrides Java found on `PATH`.
`:classpath` is a list of directory/JAR names; `:options` is a list of JVM option
strings. Options are trusted configuration, not an isolation boundary. The
bridge supplies `-Xrs` so TorCL retains responsibility for termination signals.
A bad/missing JDK reports a condition; it does not install tools automatically.

## Java calling Lisp

```lisp
(torcl-jvm:with-java-objects
    ((callback
       (torcl-jvm:implement "java.util.function.IntUnaryOperator"
         (lambda (method value)
           (assert (string= method "applyAsInt"))
           (torcl-jvm:call-static "java.lang.Math" "addExact" "(II)I"
                                 value 1)))))
  (torcl-jvm:call callback "applyAsInt" "(I)I" 41))
;; => 42

(torcl-jvm:stop-jvm *jvm*)
```

`implement` creates a real Java proxy implementing one public interface. Its
Lisp function receives the method name followed by converted arguments. Calls
on Java-created native threads and nested Lisp → Java → Lisp → Java calls are
supported. Java `equals`, `hashCode` and `toString` on the proxy use identity
semantics and do not call Lisp. Overloads have only their name and arguments
at the callback boundary; use a distinct interface when that is ambiguous.

## Calls and conversions

`new`, `call`, and `call-static` require an explicit JVM method descriptor.
Java reflection checks it before invocation; user descriptors never become
unchecked JNI calls. Only public methods and constructors are exposed. There
is no automatic overload resolution or varargs expansion.

| Descriptor | Meaning |
|---|---|
| `()V` | No arguments, void result (also the constructor result descriptor) |
| `(II)I` | Two Java ints, int result |
| `(Ljava/lang/String;)I` | String argument, int result |
| `()[I` | No arguments, int array result |

Class and method names are case-sensitive strings. Class arguments also accept
a Java `Class` proxy. `(find-java-class name loader)` uses an explicit class
loader; retaining the resulting Class proxy preserves loader identity.

| Lisp value | Java value |
|---|---|
| `+null+` | `null` (distinct from false) |
| `nil`, `t` | Boolean false, true |
| Signed 64-bit integer | Long when boxed; range-checked for primitive integer parameters |
| Single/double float | Float/Double; checked for float overflow |
| Character | One UTF-16 code unit; supplementary characters require a string |
| String | UTF-16 String, including supplementary characters and embedded NUL |
| Java proxy | Underlying Java reference |

Returned strings and primitive wrappers convert to Lisp values. Other return
values are owned Java proxies. **`new` always returns an owned proxy**, including
for String and primitive-wrapper constructors. For a Java parameter of type
`Integer` rather than primitive `int`, explicitly construct `java.lang.Integer`;
ordinary Lisp integers box as Long. Arrays remain Java objects; use
`array-length`, `array-ref`, and `array-set`. No collection copying occurs.
Unpaired UTF-16 surrogates signal an error instead of producing invalid Lisp text.

## Ownership and conditions

Every returned proxy owns a JNI global reference. Call `release` or use
`with-java-objects`, which releases bindings in reverse order even on a condition.
There are deliberately no finalizers: forgetting to release a proxy retains it
and prevents shutdown. Releasing an already released proxy is harmless; using
it signals `jvm-error`.

`retain` creates an independently owned strong reference. `weak-reference`
creates an owned weak reference; `promote` returns a new strong proxy or NIL if
collected. Release weak references too. `same-object-p` compares Java identity.
Cross-heap cycles require explicit release; the collectors are not unified.

Object arguments passed to a Lisp callback are borrowed for that invocation.
Use `retain` to keep them afterwards. Returning such an argument is supported:
the bridge preserves it before callback cleanup. Releasing an interface proxy
revokes its callback before freeing the Lisp trampoline. If Java still retains
the proxy, a subsequent invocation throws an exception. An active callback
cannot be revoked until it returns. Retaining another reference to the Java
proxy does not transfer ownership of the original callback registration.

`java-error` (a subtype of `jvm-error`) represents Java exceptions. Lisp callback
errors become Java `IllegalStateException`; they never unwind through Java
frames. `error-message` returns the diagnostic (limited to 4095 UTF-16 units).
Java Throwable identity and stack traces are not retained in this first API.

```lisp
(handler-case
    (torcl-jvm:call-static "java.lang.Integer" "parseInt"
                          "(Ljava/lang/String;)I" "oops")
  (torcl-jvm:java-error (condition)
    (format t "Java failed: ~A~%" (torcl-jvm:error-message condition))))
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
Unicode, descriptors, constructors, arrays, ownership, weak references, Java
threads, nested callbacks, revocation, exceptions, image rejection and shutdown.
Full API loading at `TORCL_GC_STRESS=1` exceeded the 600-second test budget;
this validation limitation is tracked as `bliss-6imi7`. The runtime callback
suite and both coexistence startup orders do pass every-allocation stress.
The separate [coexistence probe](../../tools/jvm-probe/README.md) checks process
startup orders and signals; the [design contract](../../docs/design/jvm-coexistence.md)
records the architecture and original findings.
