# egcl-jvm

In-process Java integration for EGCL. The [Java integration chapter](../../docs/manual/java.md)
in the main manual documents setup, the primary `JAVA` API, the descriptor-based
`EGCL-JVM` API, ownership, callbacks, conditions, and JVM lifecycle.

The native Fedora `egcl` RPM includes this ASDF system and a prebuilt bridge.
With the RPM installed, use `(asdf:load-system :egcl-jvm)` from any directory;
only a Java runtime (17+) is needed. The installed manual is at
`/usr/share/doc/egcl/manual/index.html`.

Building from source requires a native x86-64 glibc EGCL with `egcl-rt/c-ffi` and a JDK
(17 or newer). Set `JAVA_HOME` to the JDK root containing `bin/javac`, JNI headers,
and `lib/server/libjvm.so`. Build the private bridge from the repository root:

```sh
make -C lib/egcl-jvm
```

A C compiler, Make, Python 3, and the JDK development tools are required. ASDF
also runs Make on first JVM startup when necessary. See the manual for the
complete [build and first-call example](../../docs/manual/java.md#setup).

## Validation

Run from the repository root, using the JDK selected above:

```sh
lib/egcl-jvm/tests/run.sh
lib/egcl-jvm/tests/guest.sh
EGCL_GC_STRESS=1000 EGCL_GC_POISON=1 EGCL_GC_VERIFY=1 \
  EGCL_TIMEOUT=600 lib/egcl-jvm/tests/run.sh
```

The runner builds its Java fixture and tests the actual EGCL executable with
`-Xcheck:jni`, memory/time limits and a required completion marker. Override
`EGCL_JVM_BIN` for another freshly built native executable. It covers calls,
Unicode, descriptors, inferred and exact overloads, canonical boxing, lazy
bindings, constructors, arrays/fields, scope cleanup, callback ownership and
overloads, weak references, Java threads, nested callbacks, revocation,
exceptions, package conflicts, image rejection and shutdown.
Full API loading at `EGCL_GC_STRESS=1` exceeded the 600-second test budget;
this validation limitation is tracked as `bliss-6imi7`. The runtime callback
suite and both coexistence startup orders do pass every-allocation stress.
The separate [coexistence probe](../../tools/jvm-probe/README.md) checks process
startup orders and signals; the [design contract](../../docs/design/jvm-coexistence.md)
records the architecture and original findings.
