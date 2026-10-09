# Images and applications

A saved image starts from a running Lisp world. You load libraries, define
functions, create objects, and then ask EGCL to preserve that world. Restoring
it is different from reading the source files again: the image contains live
heap data and the runtime registries needed to interpret it.

## A core needs a runtime

A core file is restored by a compatible EGCL runtime. A saved executable puts
that runtime and the core together, allowing the operating system to start the
application directly. Its `:toplevel` function supplies the application's entry
point; without one, the saved executable opens a REPL.

For an application with a known set of entry points, the separate
[shaker](../how-to/save-executable.md#shake-an-application-from-a-saved-image)
can consume a saved core and a retention specification. Its first pass removes
unreachable named functions in opted-in packages while preserving the full
runtime and global data. Ordinary image saving still preserves the loaded world.

The shaker builds a new image rather than leaving holes in the input. It removes
unreachable function bindings and bytecode registry entries, then traces the
remaining image roots and writes only reachable heap objects. This separate
serialization pass matters because restored objects are pinned: ordinary GC
cannot reclaim all dead objects in those regions. Their pinning in the shaker's
process does not require retaining them in the output file.

Heap objects are stored as individual records carrying their previous addresses.
On restore, the loader allocates the objects again and fixes references through
an old-to-new address map. Symbol identity records and section-alignment padding
remain, but omitted function bodies and unreachable constants occupy no reserved
holes. By default, the native runtime is copied in full. With
`runtime = specialized`, the shaker builds a matching Rust runtime from a
capability manifest before appending the reduced image. The initial removable
capability is disassembly; interpreter and tiered compilation remain available.
See [native specialization](../how-to/save-executable.md#specialize-the-native-runtime).

Embedding the runtime does not make an executable independent of its operating
system. A dynamically linked Linux runtime still needs its loader and compatible
libraries. A saved executable built for IBM Z is still an IBM Z executable.

## Cross-target creation runs the target runtime

The Fedora tools use QEMU or Wine to run a target runtime on the build machine.
That runtime loads the application and saves an image in its own format. The
build host is providing execution services; it is not translating an existing
x86-64 heap into a different architecture's heap.

This is why you select `egcl-s390x-linux`, for example, before loading and
saving the application. The [cross-build guide](../how-to/cross-build.md) gives
the complete procedure and the QEMU library-path requirement.

## Android uses an application lifecycle

A phone application must participate in Android's Activity lifecycle, package
installation, and signing. The current APK workflow supplies a native library
that starts a Lisp worker and loads source assets. Surface creation calls the
Lisp application; surface destruction asks it to release graphics resources.

This shares the goal of shipping Lisp applications, but does not currently use
a dumped core as the APK payload. An ARM64 `egcl-android` command-line
executable and a NativeActivity APK are different deployment artifacts.
