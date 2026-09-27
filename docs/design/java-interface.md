# A more idiomatic Java interface for TorCL

Research and proposal, 2026-09-27. Tracked in `bliss-jd785`.
**The examples below are proposed syntax, not implemented functionality.**
The existing [torcl-jvm API](../../lib/torcl-jvm/README.md) remains unchanged.

The recommendation is a small, ordinary Common Lisp interface combining dynamic
calls, named Lisp bindings, and explicit lifetime scopes. Keep the existing
checked bridge underneath it. Making descriptors optional must not make overload
selection, callback ownership, or runtime transitions implicit and unpredictable.

## What existing systems teach us

| System | Documented interface | Lesson for TorCL |
|---|---|---|
| [ABCL and JSS](https://www.abcl.org/releases/1.9.0/abcl.pdf), §§3.1, 4.5, 4.8, 5.3 | ABCL offers explicit method references and dynamic name-based calls. JSS adds `#"method"` reader syntax and shorter class lookup. ABCL also supports Java-class method specializers. | Offer easy dynamic calls and an exact escape hatch; defer reader extensions and deep CLOS integration. |
| [LispWorks](https://www.lispworks.com/documentation/lw80/lw/lw-java-ug-2.htm), §15.2 | Class imports and defining macros generate ordinary Lisp callers; callers can resolve overloads dynamically. | Named wrappers should be useful with normal Lisp tooling and higher-order functions. |
| [Clojure](https://clojure.org/reference/java_interop) | Concise member calls, type hints, explicit parameter tags, and interface implementations. | Make common calls short, preserve type-directed selection, and provide concise callback adapters. |
| [Kawa](https://www.gnu.org/software/kawa/Method-operations.html) | Class aliases, member notation, and `invoke`/`invoke-static` with applicable-method selection. | Resolve names predictably; report ambiguity rather than depending on reflection order. |
| [Allegro jLinker](https://examples.franz.com/support/documentation/jlink-ops.html) | Reflection-based API scanning generates Lisp functions; explicit Java environments and retention operations expose boundary costs. | Generated application bindings and lifetime boundaries matter as much as expression syntax. Its separate-process mode is not our architecture. |
| [CL+J](https://cl-plus-j.common-lisp.dev/) | Native JNI/CFFI bridge with dispatch-reader syntax and explicit runtime coexistence concerns. | A close architectural precedent; optional syntax cannot replace runtime safety. Its published compatibility notes describe old releases, not present-day SBCL or JDK support. |

[LispWorks' proxy documentation](https://www.lispworks.com/documentation/lw81/lw/lw-java-ug-4.htm)
also distinguishes interface definitions from proxy instances and specifies
callback reference scopes. Its
[local/global reference discussion](https://www.lispworks.com/documentation/pdf/lw80/lw-8-0.pdf)
is particularly relevant to borrowing callback arguments.

ABCL and Clojure execute inside the JVM. TorCL retains a native Lisp heap and a
separate JVM heap. Their compact syntax is useful inspiration, but it does not
supply a solution to our cross-heap cycles or native callback lifetimes.

## Package boundary and compatibility

Keep `TORCL-JVM` as the explicit, descriptor-based interface. Add an optional
ASDF system `torcl-jvm/api` exposing `TORCL-JAVA`. Use `JAVA` as its short name
in applications; installation must detect an existing conflicting package,
not change another library's `JAVA` package. Applications can always use the
full `TORCL-JAVA` name.

Do not change the old `call` to guess whether its first string argument is a
JNI descriptor. A Java method can legitimately accept a descriptor-looking
string. The two package interfaces make the migration unambiguous.

Use ordinary functions/macros first. Java names remain exact strings. Explicit
aliases introduce Lisp names without guessing capitalization:

```lisp
(java:define-class array-list "java.util.ArrayList")

(java:with-scope ()
  (let ((items (java:new 'array-list)))
    (java:call items "add" "hello")
    (java:call items "add" 42)
    (java:to-list items)))
;; proposed result: ("hello" 42)

(java:static "java.lang.Integer" "parseInt" "42")
;; proposed result: 42
```

`define-class` records metadata under an explicitly chosen Lisp symbol; it does
not yet install a CLOS class, start a JVM, or scan the classpath. Fully qualified
strings and actual Java Class proxies remain supported. A binding records its
class-loader context; identical class names from different loaders stay distinct.
Startup stays explicit through `torcl-jvm:start-jvm`.

An optional JSS-style reader could later expand into this API, using a named
readtable. Reader mutation on library load is not part of the proposal.

## Overloads without descriptors

Dynamic calls enumerate public candidates, select a unique applicable target,
then use checked conversions. They must not try calling candidates until one
works: Java side effects and exceptions make that incorrect.

Proposed conversion/selection policy:

- Unannotated integers have canonical Java type `int` when they fit signed
  32 bits, otherwise `long` when they fit signed 64 bits. Larger integers need
  an explicit BigInteger conversion. Single/double floats keep their width.
- Prefer exact primitive/reference matches and permitted widening before
  boxing/unboxing. Select a unique most-specific reference target; unrelated
  applicable types remain ambiguous. No implicit integral narrowing or
  float-to-integer truncation.
- `java:as` supplies a checked type annotation. A reference annotation also
  restricts dispatch to that declared view; it does not clone the object.
- `nil` remains Java false, `t` true, and `java:+null+` Java null. Untyped null
  can make unrelated reference overloads ambiguous. `java:as` can type it.
- Object parameters receive canonical boxes (Integer/Long, etc.). This differs
  intentionally from the existing low-level API's always-Long integer boxing;
  retaining separate packages preserves existing behavior.
- Start with explicit Java arrays for varargs. Automatic spreading needs an
  explicit design, especially when a method accepts both Object and Object[].

For an exact overload, the method designator may include its parameter types:

```lisp
(java:call items '("remove" :int) 1)
(java:call items '("remove" "java.lang.Object") 1)

(java:static "java.lang.Math" "abs" (java:as :long -42))
```

These first two calls deliberately mean different things: Java List has both
[index-removal and value-removal methods](https://docs.oracle.com/en/java/javase/26/docs/api/java.base/java/util/List.html).
The declared method selector controls boxing as well as overload selection.
Return types normally come from reflection. The old descriptor API remains
available for unusual bytecode-level distinctions such as bridge methods.

`ambiguous-call` should show candidate signatures, argument types, and an exact
call the user can write. Cache keys include actual receiver Class identity,
class-loader identity, method kind/name, argument types, and numeric range
classification. Caching only “Lisp integer” would be wrong when a value crosses
the int/long boundary. Session teardown releases cached Java references.

This is a TorCL dynamic-language policy, not a promise to reproduce every Java
source-language overload rule. It needs a documented conformance table and
adversarial tests before becoming the default application interface.

## Named Lisp functions

For reusable application code, favor selected explicit bindings over importing
an entire Java package into the current Lisp package:

```lisp
(java:define-call parse-int ("java.lang.Integer" "parseInt")
  :static t :parameters ("java.lang.String") :returns :int)

(mapcar #'parse-int '("10" "20" "30"))
;; proposed result: (10 20 30)
```

Instance bindings take the receiver first. Defining a binding stores metadata;
first use resolves it, and an explicit verification operation checks bindings
at application startup. This allows compilation without starting Java. Resolved
native references still cannot be saved in an image. Existing package/image
restrictions remain until separately addressed.

The same resolved-call representation should serve dynamic calls and bindings.
Start by caching reflected members; neither direct JVM calls nor JNI elimination
is implied. MethodHandle/JIT optimization comes after measurements justify it.

## Callbacks that read like Lisp

A functional-interface adapter should not expose a method-name argument:

```lisp
(java:with-scope ()
  (let ((increment
          (java:lambda "java.util.function.IntUnaryOperator" (x)
            (1+ x))))
    (java:call increment "applyAsInt" 41)))
;; proposed result: 42
```

`java:lambda` is a package-local macro name distinct from `cl:lambda`. It checks
that the interface has one applicable abstract method. An explicit method-body
form covers richer interfaces:

```lisp
(java:implement "java.util.Comparator"
  ("compare" (a b)
    (- (length a) (length b))))
```

The comparator returns an integer, not Lisp truth. Method selectors can carry
parameter types for overloaded interface methods. The native callback protocol
must therefore include method signature information; the current name-only
callback protocol is insufficient. Validate missing methods, arities and return
conversions at adapter creation where possible, and fail clearly at invocation
otherwise. Multiple interfaces, default-method dispatch and concrete-class
subclassing can follow separately.

Do not automatically turn arbitrary Lisp functions into persistent Java
callbacks in phase one. A library may retain its argument, so a temporary
adapter requires a lifetime contract. Java exceptions remain Lisp conditions;
Lisp callback failures remain Java exceptions, not implicit successful zeroes.

## Reference scope versus Java resource lifetime

`with-scope` tracks every high-level proxy created in its dynamic extent,
including unnamed intermediate results, and releases them in reverse order on
exit. It does not borrow ownership of an existing input proxy. It can initially
manage the bridge's global references; its name does not promise JNI local-frame
semantics. `java:retain` creates an independently owned reference that survives
the scope; the caller must later release it. Returning or storing an unretained
scoped proxy does not extend its lifetime; subsequent use signals a condition.

Scopes are thread-local. Java-created callback threads get their own invocation
scope, not the caller's ambient scope. Nested callbacks must preserve this rule.
`to-list` explicitly copies a Java Iterable to a Lisp list; object-valued elements
still have scoped proxy lifetimes. A later iteration macro can avoid bulk copies.

Callback retention needs a stronger operation than copying its Java object
handle: the registration and rooted Lisp closure must share a reference-counted
lease until all owners release it. The current low-level `retain` does not do
that. Implement this before advertising escape of callbacks from a scope.

Releasing a JNI reference does not invoke Java `close()`. A later
`with-open-resource` should call AutoCloseable.close and then release the proxy,
while preserving the primary exception if cleanup also fails. Explicit release
remains available. Finalizers could eventually enqueue best-effort cleanup on
an attached thread; they must not enter Java from Lisp collector locks and do
not solve cross-heap cycles. A live-object inspector would help explain why
shutdown is refusing outstanding references.

## Alternatives and recommended scope

A JSS-style reader gives the shortest calls but introduces a readtable dependency.
A full Java/CLOS model gives deeper dispatch integration but adds metaclass,
interface-precedence and loader-identity decisions. Ordinary calls plus binding
macros offer substantial improvement using familiar Common Lisp tools and a
smaller implementation surface. That is the recommended starting point.

Implementation should proceed in three bounded pieces after design review:

1. Shared resolver, exact selectors, type annotations, dynamic calls and named
   bindings. Preserve the old API and test ambiguous null, numeric ranges,
   boxing, inherited methods, varargs-as-arrays, and separate class loaders.
2. Scoped ownership and functional/interface adapters, including callback leases,
   thread-local scope isolation, non-local exits and revocation during callbacks.
3. SETF field/array accessors, explicit collection traversal, resource scopes and
   interactive inspection. Consider reader syntax and CLOS integration separately.

The combined examples represent the intended interface after the first two
pieces, not a claim that all of this is a thin macro layer over today's bridge.
