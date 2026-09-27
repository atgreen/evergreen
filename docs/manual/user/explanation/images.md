# Images and applications

A saved image starts from a running Lisp world. You load libraries, define
functions, create objects, and then ask TorCL to preserve that world. Restoring
it is different from reading the source files again: the image contains live
heap data and the runtime registries needed to interpret it.

## A core needs a runtime

A core file is restored by a compatible TorCL runtime. A saved executable puts
that runtime and the core together, allowing the operating system to start the
application directly. Its `:toplevel` function supplies the application's entry
point; without one, the saved executable opens a REPL.

Embedding the runtime does not make an executable independent of its operating
system. A dynamically linked Linux runtime still needs its loader and compatible
libraries. A saved executable built for IBM Z is still an IBM Z executable.

## Cross-target creation runs the target runtime

The Fedora tools use QEMU or Wine to run a target runtime on the build machine.
That runtime loads the application and saves an image in its own format. The
build host is providing execution services; it is not translating an existing
x86-64 heap into a different architecture's heap.

This is why you select `torcl-s390x-linux`, for example, before loading and
saving the application. The [cross-build guide](../how-to/cross-build.md) gives
the complete procedure and the QEMU library-path requirement.

## Android uses an application lifecycle

A phone application must participate in Android's Activity lifecycle, package
installation, and signing. The current APK workflow supplies a native library
that starts a Lisp worker and loads source assets. Surface creation calls the
Lisp application; surface destruction asks it to release graphics resources.

This shares the goal of shipping Lisp applications, but does not currently use
a dumped core as the APK payload. An ARM64 `torcl-android` command-line
executable and a NativeActivity APK are different deployment artifacts.
