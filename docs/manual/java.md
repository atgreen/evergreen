# Java integration

`torcl-jvm` connects TorCL to an in-process HotSpot JVM. Lisp programs can
construct Java objects, call methods, and implement Java interfaces with Lisp
functions. The `JAVA` package is the primary interface; `TORCL-JVM` provides
explicit descriptor-based calls. Both are loaded by the same ASDF system.
The two runtimes keep their own heaps and collectors. This is a checked API
boundary, not a sandbox for untrusted Java or native code.

The supported target is **native x86-64 Linux with glibc**. Install
the native Fedora RPM, or build this checkout with `torcl-rt/c-ffi`. The static
musl executable and Android ART are not supported.

## Setup and first calls { #setup }

The native Fedora `torcl` RPM includes both APIs and a prebuilt bridge, and
requires a Java runtime (17 or newer). Start `torcl` from any directory and load
`(asdf:load-system :torcl-jvm)`; no compiler or JDK is needed. The installed HTML
manual starts at `/usr/share/doc/torcl/manual/index.html`.

For a source checkout, install a JDK (17 or newer, including `javac` and JNI headers), a C compiler,
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

ASDF loads the Lisp API. The first `start-jvm` loads the installed bridge
without running build tools. In a source checkout it runs Make to update the
bridge; that source directory must be writable.
`:java-home` overrides `JAVA_HOME`, which overrides Java found on `PATH`.
`:classpath` is a list of directory/JAR names; `:options` is a list of JVM option
strings. Options are trusted configuration, not an isolation boundary. The
bridge supplies `-Xrs` so TorCL retains responsibility for termination signals.
A bad/missing JDK reports a condition; it does not install tools automatically.

## Calls, overloads, and bindings { #calls }

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
(java:with-scope ()
  (let ((items (java:new 'array-list)))
    (java:call items "add" 42)
    (java:call items "add" 99)
    (java:call items '("remove" :int) 1)                ; => 99 (index)
    (java:call items '("remove" "java.lang.Object") 42))) ; => T (value)
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

## Scopes and ownership { #ownership }

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

## Java calling Lisp { #callbacks }

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

## Collections, arrays, and fields { #collections }

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

## Conditions { #conditions }

`java:java-error`, a subtype of `java:jvm-error`, represents Java exceptions.
Lisp callback errors become Java `IllegalStateException`; they never unwind
through Java frames. `error-message` returns the diagnostic (limited to 4095
UTF-16 units). Throwable identity and stack traces are not retained.

```lisp
(handler-case (java:static "java.lang.Integer" "parseInt" "oops")
  (java:java-error (condition)
    (format t "Java failed: ~A~%" (java:error-message condition))))
```

## Streams { #streams }

Java's `System.out` reaches `*standard-output*` and `System.err` reaches
`*error-output*`, so the two runtimes' output interleaves in program order and a
`WITH-OUTPUT-TO-STRING` or a rebinding captures both:

```lisp
(with-output-to-string (s)
  (let ((*standard-output* s))
    (write-string "lisp ")
    (java:call (java:static-field "java.lang.System" "out") "print" "java")))
;; => "lisp java"
```

`System.out` and `System.err` are redirected into in-memory buffers whose contents
are written to the Lisp streams at the end of each crossing. **Output therefore
appears when the call returns**, not as it is produced, so a long computation's
progress prints arrive together at the end. `JAVA:FLUSH` writes out what has
accumulated so far, and a callback into Lisp drains on entry, so a Java computation
that calls back can report as it goes. Output printed before a Java exception also
arrives.

Three consequences worth knowing:

* The redirection is at the Java level, not `dup2` on the file descriptor, so native
  writes from inside the JVM — a JNI library, `-Xlog` GC logging, a crash report —
  go to the real file descriptor 1 where a reader expects them.
* `System.out` does not wrap a real file descriptor, so Java code that reaches for
  one (`System.console()`, `ProcessBuilder.INHERIT_IO`, tty detection) will notice.
* If your own code calls `System.setOut`, the next drain reinstalls the capture
  rather than losing everything written from then on.

`System.in` reads file descriptor 0 directly rather than `*standard-input*`, for the
same reason Python's `input()` does: input is pulled rather than pushed.

## Process lifecycle { #lifecycle }

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

The API does not implement CLOS Java metaclasses, MethodHandle call-site
optimization, JVM bytecode compilation, cross-heap cycle collection, Java virtual
thread guarantees, or JVM resurrection from saved images.

## Primary API dictionary

Names below use the `JAVA` nickname of `TORCL-JAVA`. Use package-qualified
names: `JAVA:LAMBDA` and `JAVA:FIND-CLASS` are distinct from the Common Lisp
operators. Lifecycle, condition, identity, and strong-reference operations
are also exported from `TORCL-JVM` as the same symbols.

### Starting and stopping a JVM { #start-jvm }

**Functions**

```lisp
(java:start-jvm &key java-home (classpath nil) (options nil) attach) ; => session
(java:jvm-running-p)                                              ; => boolean
(java:stop-jvm session &key (timeout 10))                           ; => T
```

`java-home` is a JDK directory designator. `classpath` lists directory or JAR
names; `options` lists individual option strings without newlines. With `attach`
true, a JVM must already exist. The returned session identifies the bridge's
lifecycle, not a Java object: pass it to `stop-jvm`, not `release`.
`timeout` is in seconds and must be a nonnegative real no greater than 2147483.
`jvm-running-p` is false before startup and during or after shutdown.

Startup, outstanding-reference, and shutdown-timeout failures signal
`jvm-error`. See [Process lifecycle](#lifecycle) for signal handling, guest
attachment, shutdown retries, and image restrictions.

### Constructing objects and calling methods { #new-call-static }

**Functions**

```lisp
(java:new class &rest arguments)             ; => owned proxy
(java:call object method &rest arguments)    ; => converted result
(java:static class method &rest arguments)   ; => converted result
```

`class` is a case-sensitive name, a Java Class proxy, or an alias symbol from
`define-class`. `method` is a string or a list `(name parameter-type...)`; the
latter fixes the parameter types. Type designators include the primitive keywords
`:boolean`, `:byte`, `:short`, `:int`, `:long`, `:float`, `:double`, and `:char`,
class-name strings, and aliases. Array type names use descriptor notation, such
as `"[I"` or `"[Ljava.lang.String;"`. Exact selectors require names rather than
Class proxies for their parameter types.

Results follow [Calls and conversions](#calls). New proxy references belong to
the current scope or, outside a scope, to the caller. A failed resolution,
conversion, or Java invocation signals `java-error`; incomparable applicable
methods signal `ambiguous-call`. A released receiver signals `jvm-error`.

### Explicit argument types { #as }

**Function** `(java:as type value)` → argument annotation.

The annotation supplies a declared Java type during a call; it is not a Java
object or an immediate conversion. Its value is checked when used. Reference
types accept null and compatible objects; primitive annotations check their
representable range. `:void` is only a return type for bindings, not an argument
type. Examples and the widening/boxing rules are under [Calls](#calls).

### Class aliases and lookup { #classes }

**Macro** `(java:define-class name class)` → class designator.

Associates the unevaluated Lisp symbol `name` with the evaluated class designator.
It neither starts the JVM nor resolves the class. An alias does not extend a
Class proxy's lifetime; keep such a proxy alive for as long as the alias is used.

**Function** `(java:find-class name &optional loader)` → owned Class proxy.

Resolves a name or alias using the supplied Java class-loader proxy, or the
system loader when omitted. Resolution can initialize the class. Loader and
class identity matter even when two classes have the same printed name. The
returned reference belongs to the current scope or caller.

### Named bindings and inspection { #bindings }

**Macro**

```lisp
(java:define-call name (class method)
  &key static (parameters nil) returns) ; => name
```

Defines an ordinary Lisp function. A static binding accepts only method
arguments; an instance binding accepts the receiver first and checks it against
`class`. Omitting `parameters` requests inferred overload selection; explicitly
supplying `:parameters ()` selects a zero-argument method. `returns` requires
explicit parameters and checks the exact return type, including `:void`.
Definitions can precede JVM startup. Call results have the same conversions and
ownership as `call` and `static`.

**Functions**

```lisp
(java:verify binding)       ; => T
(java:describe-class class) ; => list of strings
```

`verify` takes a symbol naming a `define-call` binding with explicit parameters.
It resolves its public method, static/instance status, and optional return type
without calling that method. Class resolution can initialize the class.
Unknown bindings or missing parameters signal `jvm-error`; mismatched Java
members signal `java-error`. `describe-class` returns sorted descriptions of
public constructors, methods, and fields as ordinary Lisp strings.

### Reference scopes and retention { #with-scope }

**Macro** `(java:with-scope () &body body)` → values of body.

Establishes the dynamic ownership scope described under [Scopes and
ownership](#ownership). Cleanup runs on every exit and attempts to release all
tracked references. A cleanup failure signals a condition and can supersede a
pending exit. Low-level `TORCL-JVM` results and explicit `retain` copies are not
registered with this scope.

**Functions**

```lisp
(java:retain object)       ; => independently owned strong proxy
(java:release object)      ; => NIL
(java:java-object-p value) ; => boolean
(java:same-object-p a b)   ; => boolean
```

`retain` accepts a live proxy, including a borrowed callback argument, and
shares ownership of a callback registration when present. Release its result
explicitly. `release` is idempotent, but revoking the last owner of an active
callback signals `jvm-error`; wait for it to return and retry. `java-object-p`
recognizes proxy wrappers, including released wrappers, so it is not a liveness
test. `same-object-p` compares live references by Java identity rather than Lisp
wrapper identity. These functions are also exported by `TORCL-JVM`.

### Closing resources { #with-resource }

**Macro** `(java:with-resource (name expression) &body body)` → values of body.

Evaluates `expression` once, binds its owned resource proxy to `name`, and calls
`close()` followed by `release` on every exit. A cleanup failure is signaled on
normal completion; cleanup errors are suppressed when the body already has a
pending error or nonlocal exit. This macro owns the supplied reference; use a
retained copy if another caller must keep its reference. Returned proxies
created in the body still need their own scope or explicit release.

### Interface adapters { #interface-adapters }

**Macros**

```lisp
(java:lambda interface (argument...) &body body) ; => owned interface proxy
(java:implement interface
  (selector (argument...) form...) ...)         ; => owned interface proxy
```

`interface` is a class designator for one public interface. `lambda` binds the
converted arguments of its single abstract method. `implement` uses a method
name or an exact `(name parameter-type...)` selector in each clause. Supply
fixed argument lists and a clause for every abstract method; invalid method
sets or arities signal `jvm-error`. Each body supplies the Java return value;
return conversion errors become Java exceptions. Both forms close over Lisp
lexical bindings and register their result with the current scope, if any.
See [Java calling Lisp](#callbacks) for borrowing, default methods, thread
entry, exceptions, and the lifetime of retained callbacks.

### Collections, arrays, and fields { #accessors }

**Functions and SETF places**

```lisp
(java:to-list iterable)                ; => Lisp list
(java:new-array component length)     ; => owned Java array proxy
(java:array-length array)             ; => nonnegative integer
(java:array-ref array index)          ; => converted element
(setf (java:array-ref array index) value)
(java:field object name)              ; => converted field value
(setf (java:field object name) value)
(java:static-field class name)        ; => converted field value
(setf (java:static-field class name) value)
```

`to-list` copies an Iterable and releases its temporary iterator. Object-valued
elements remain owned references, subject to the surrounding scope.
`component` is a primitive type keyword, class name, alias, or Class proxy;
`length` is a nonnegative Java-int-sized integer. Arrays are zero-indexed.
Object-valued reads and new arrays belong to the scope or caller. Field names
are case-sensitive strings; static access accepts class designators. SETF
returns the supplied Lisp value. Invalid bounds, incompatible values, missing
fields, and Java access violations signal `java-error`.

### Output { #java-output }

**Functions**

```lisp
(java:flush)          ; => (values)
```

Writes everything Java has printed since the last drain to `*standard-output*` and
`*error-output*`. Every call into Java already drains on the way out, including when
it signals, so `java:flush` is only needed to pull output *during* a call — from a
callback, or from another thread watching a long computation. It never signals: it
runs in cleanup positions where an error would mask the Java failure being unwound.

Also exported from `TORCL-JVM` as `torcl-jvm:drain-output`, with
`torcl-jvm:draining` wrapping a body so it drains on both normal and non-local exit.

See [Streams](#streams) for what is captured and what is not.

### Null and conditions { #java-conditions }

**Constant** `java:+null+` represents Java null. NIL represents Java false.
The constant is also exported as `torcl-jvm:+null+`.

**Condition types** `java:jvm-error`, `java:java-error`, `java:ambiguous-call`.
`jvm-error` is an ERROR subtype for lifecycle and bridge failures; `java-error`
is its subtype for Java exceptions; `ambiguous-call` is a `java-error` subtype
for overload-resolution ambiguity. The first two are also exported from
`TORCL-JVM`.

**Function** `(java:error-message condition)` → diagnostic string.
This accessor is also exported as `torcl-jvm:error-message`. See
[Conditions](#conditions) for callback error translation and diagnostic limits.

## Descriptor API { #descriptor-api }

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

### Method descriptors

A descriptor contains parenthesized parameter types followed by a return type.
`V` is only a return type. Object names use slashes and a terminating semicolon;
`[` prefixes an array component type.

| Code | Java type |
| --- | --- |
| `Z`, `B`, `S`, `C` | boolean, byte, short, char |
| `I`, `J`, `F`, `D` | int, long, float, double |
| `V` | void |
| `Ljava/lang/String;` | String |
| `[I` | int array |
| `[Ljava/lang/Object;` | Object array |

Examples: `"()V"` is a zero-argument constructor or void method,
`"(II)I"` takes two ints and returns int, and `"()[I"` returns an int array.
A constructor descriptor must end in `V`, although `new` returns a proxy.

### Explicit calls and class lookup { #descriptor-calls }

**Functions**

```lisp
(torcl-jvm:new class signature &rest arguments)          ; => owned proxy
(torcl-jvm:call object method signature &rest arguments) ; => converted result
(torcl-jvm:call-static class method signature &rest arguments)
(torcl-jvm:find-java-class name &optional loader)         ; => owned Class proxy
```

Class arguments accept a case-sensitive string or Class proxy; they do not
resolve `JAVA:DEFINE-CLASS` aliases. Method names and descriptors are strings.
The parameter and return types must match the public method exactly. Primitive
parameters are checked for range; float-to-integer truncation is rejected.
Reference parameters require compatible objects. A Lisp integer boxes as Long,
so construct an Integer proxy explicitly when a reference parameter requires
`java.lang.Integer`. Other scalar and string conversions follow the primary API.
Void methods return NIL; null returns `+null+`. Low-level proxies belong to the
caller, even when a `java:with-scope` is active. Lifecycle and invocation failures
use the [shared conditions](#java-conditions).

### Bound references and weak references { #with-java-objects }

**Macro** `(torcl-jvm:with-java-objects ((name expression)...) &body body)`
→ values of body.

Evaluates and binds expressions sequentially, releasing Java proxy bindings in
reverse order on exit. It owns only those bindings; unnamed intermediate
results still require explicit release. It does not call Java `close()`.

**Functions**

```lisp
(torcl-jvm:weak-reference object) ; => owned weak proxy
(torcl-jvm:promote object)        ; => owned strong proxy, or NIL if collected
```

Weak references permit collection of their Java referent. Release the weak
proxy itself, and release successful promotions independently. These results
are not tracked by `java:with-scope`. Expired or released references cannot be
used as ordinary live receivers. Strong retention, release, predicates, and
identity comparison use the [shared reference operations](#with-scope).

### Descriptor-level callbacks { #descriptor-callbacks }

**Function** `(torcl-jvm:implement interface function)` → owned interface proxy.

`function` receives the Java method name as a string, followed by converted
arguments. It must return a value compatible with the Java method's return type.
Unlike `java:implement`, this function does not validate a complete method set
in advance or distinguish overloads by a signature. Default methods also enter
this Lisp dispatcher. Object identity methods are handled by the proxy itself.

```lisp
(torcl-jvm:with-java-objects
    ((increment
       (torcl-jvm:implement "java.util.function.IntUnaryOperator"
         (lambda (method value)
           (assert (string= method "applyAsInt"))
           (1+ value)))))
  (torcl-jvm:call increment "applyAsInt" "(I)I" 41))
;; => 42
```

Arguments are borrowed for the invocation, and returned references are copied
before argument cleanup. Low-level callbacks do not establish a high-level
scope for other references created in their body; manage those explicitly or
use `java:with-scope` for temporary work. Keep a Lisp-owned reference to the
adapter while Java may invoke it. Callback copies share the same registration;
releasing the last owner revokes it. See [Callback lifetime](#callbacks).

### Descriptor-level arrays { #descriptor-arrays }

**Functions**

```lisp
(torcl-jvm:array-length array)          ; => nonnegative integer
(torcl-jvm:array-ref array index)      ; => converted element
(torcl-jvm:array-set array index value) ; => +null+
```

Indices are zero-based. Object-valued reads return caller-owned proxies.
Writes check the Java array component type and use the descriptor API's
conversion rules, including Long boxing for Lisp integers. Bounds and type
errors signal `java-error`. Use `java:new-array` to construct an array, or obtain
one from a Java method.

## Ending the example session

Run the examples in this chapter in the JVM session started under [Setup](#setup).
After releasing references and callbacks, end the session:

```lisp
(java:stop-jvm *jvm*)
```

## Implementation references

The [Lisp API](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/torcl-jvm/api.lisp),
[descriptor bridge](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/torcl-jvm/jvm.lisp),
and [native integration tests](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/torcl-jvm/tests)
provide the corresponding implementation and examples. Build and validation
commands for contributors are in the
[package README](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/torcl-jvm/README.md).
