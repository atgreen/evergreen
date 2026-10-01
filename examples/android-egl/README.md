# Android EGL demo

An animated 3D raymarched scene, expressed as Lisp data and compiled to GLSL.
Lisp drives the camera and scene uniforms through EGCL's EGL/OpenGL ES FFI.
The Android app is named **EGCL EGL** (`org.egcl.example.egl`).

## Build and run

Install SBCL and the `egcl-target-android` RPM, then install the builder's
pinned Lisp dependencies once, from the repository root:

```sh
(cd lib/egcl-apk && ocicl install)
cd examples/android-egl
make                 # build/android-egl.apk
make install         # requires adb; install for Android user 0
make run
make logcat
```

Building uses Common Lisp: no Android SDK, NDK, JDK, Gradle, or Rust compiler
is needed. The phone must support ARM64 and Android 9/API 28 or newer.
Enable USB debugging before installing. Optional `make verify` uses Android
build-tools (`apksigner`, `zipalign`, and `aapt2`) for independent validation.

Edit `apk.sexp` for package, version, SDK levels, or target architectures.
Set machine-specific paths in the ignored `local.mk`:

```make
SBCL_BIN := /path/to/sbcl
EGCL_APK_RUNTIME := /usr/libexec/egcl/android/
# Only for make verify:
BUILD_TOOLS := /path/to/android-sdk/build-tools/34.0.0
```

Use `SERIAL=DEVICE_SERIAL` with install/run/logcat when multiple devices are
connected. The first build creates an unencrypted development signing identity
in `.egcl-apk-key`, with private permissions. Keep it to update installed apps;
`make clean` preserves it. See the
[builder documentation](../../lib/egcl-apk/README.md) for scope and limitations.

## Source

- `assets/scene.lisp`: scene data, GLSL generation, camera and animation.
- `assets/egl.lisp`: EGL setup and rendering through the FFI.
- `assets/app.lisp`: the `android-main` entry point.
- `apk.sexp`: the manifest configuration consumed by the Lisp builder.

The builder packages these assets with the RPM's native library and shared
Android Lisp API. The runtime extracts the assets and calls `android-main`
for each new surface; rendering stops when `android:running-p` becomes false
and pauses with the Activity.

This example was moved from `~/git/torcl-android-egl`. Its former repository
metadata and signing key remain there. `AndroidManifest.xml` is retained as
a reference; the builder generates the binary manifest from `apk.sexp`.
