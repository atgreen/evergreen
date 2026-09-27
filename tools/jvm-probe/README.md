# Native Linux JVM coexistence probe

This diagnostic runs **on the local Linux host**, in the real TorCL process.
It uses neither containers nor a phone, emulator, remote machine, or Java
subprocess for the workload. `javac` builds the test class; the C shim embeds
HotSpot with `JNI_CreateJavaVM` on a dedicated pthread.

Requirements: x86-64 Linux, a glibc TorCL build with `torcl-rt/c-ffi`, a local
JDK with headers, C compiler, and the user systemd bus required by
`scripts/torcl-limited.sh`.

```sh
cargo build --target x86_64-unknown-linux-gnu --features torcl-rt/c-ffi -p torcl
# Optional: point at a particular local JDK or TorCL executable.
export TORCL_JAVA_HOME=/path/to/jdk
# export TORCL_PROBE_BIN=/path/to/dynamic/torcl
tools/jvm-probe/run.sh
```

The script prints and retains a unique `/tmp/torcl-jvm-probe.*` directory with
classes, the native shim, and separate logs. Each startup order runs under
4 GiB memory / 90 second limits (plus the limiter's termination grace period).
The script returns nonzero for crashes, mismatches, or missing completion
markers. A known runtime failure is **not** converted into a passing test.

- `lisp-first`: the preloaded shim does nothing until Lisp calls `probe_start`.
- `jvm-first`: the shim starts HotSpot in its constructor, before TorCL `main`.
  TorCL then performs its ordinary initialization without probe modifications.

Use `TORCL_JVM_PROBE_ORDERS=lisp-first` to repeat an individual case. The
preload applies only to the tested TorCL process, not to the resource limiter.
The JVM uses `-Xcheck:jni`, `-Xrs`, and `-Xmx128m`. After releasing its bridge resources, the probe calls `DestroyJavaVM` on a
dedicated thread. It does not attempt to restart the JVM in the same process.

The workload checks primitive calls, Java exception containment, Java null and
stack-overflow handling, nested Java/Lisp callbacks, callbacks on Java-created
threads, collection requests on both heaps, explicit JNI global-reference
creation/deletion, orderly shutdown, and Lisp GC progress during a three-second Java call.
The overlapping test permits concurrent collection requests but does not prove
that collection phases overlap at a particular instant.

`HOTSPOT-HANDLER-PRESERVED` compares the installed SIGSEGV entry with the one
observed immediately after JVM creation. It is a diagnostic, not a complete
signal-policy test. `CACHED-JNI-NS` measures one million cached static method
calls within one attached native frame, after 20,000 warmup calls. It includes
JNI checking and an exception check per call, excludes attachment and Lisp
marshalling, and is **not** a production performance claim or an A/B comparison.

This is a diagnostic bridge, not the implementation of a supported Java API.
There are no CLOS proxies, JVM image serialization, cross-heap cycle collection,
or arbitrary Java method resolution here. See
[the coexistence contract](../../docs/design/jvm-coexistence.md).
