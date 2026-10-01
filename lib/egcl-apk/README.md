# Lisp-native Android APK builder

This builds a NativeActivity APK with Common Lisp code and the precompiled
libraries from `egcl-target-android`. APK construction needs no Android SDK,
NDK, JDK, Java, Gradle, Python, or external signing executable.

Install the pinned Lisp dependencies once (ocicl and Git are setup tools):

```sh
cd lib/egcl-apk
ocicl install
```

The launcher uses SBCL (override its executable with `SBCL_BIN`). Then,
from the repository root:

```sh
scripts/egcl-apk examples/android-egl
```

Set `EGCL_APK_RUNTIME` if the Android RPM is extracted somewhere other than
`/usr/libexec/egcl/android`. `EGCL_APK_OUTPUT` selects the output pathname;
`EGCL_APK_IDENTITY` selects a persistent signing identity. Use absolute paths
for these overrides. The default output is `PROJECT/build/android-egl.apk`.
`OCICL_RUNTIME` can override the installed ocicl runtime Lisp file.

The launcher sets umask 077. The first build creates `PROJECT/.egcl-apk-key`
with a P-256 private key and self-signed certificate; subsequent builds reuse
both. Keep this file private and backed up: Android application updates need
the same signing certificate. It is a small EGCL-specific binary identity
format, not a Java keystore. It is unencrypted; library callers must supply
an appropriately restrictive umask themselves. No existing signing keys are
imported or modified. This initial builder is intended for development APKs.

## Project contract

`apk.sexp` is a single data-only property list (reader evaluation disabled).
Supported fields: `:package`, `:label`, `:version-code`, `:version-name`,
`:min-sdk`, `:target-sdk`, `:debuggable`, `:permissions`, `:hosts`,
`:runtime-api`, and `:runtime-version`. See `examples/android-egl/apk.sexp`.

The initial scope is deliberately bounded:

- NativeActivity, no Java or DEX; minimum API 28; EGL ES 2.0.
- Literal Unicode app label and built-in fullscreen Android theme.
- Flat `assets/` directory with `app.lisp`; each file at most 64 MiB.
- `android.lisp` and the asset index are supplied by the builder/runtime.
- ARM64 and/or x86-64 libraries from runtime API 4, verified against the
  SHA-256 hashes and architecture recorded in the RPM's `runtime.json`.
- Stored ZIP entries, four-byte alignment and 16 KiB native-library alignment.
- APK v2 signatures using ECDSA P-256/SHA-256 through Ironclad.

There is no general Android resource compiler, custom launcher icon, adaptive
icon, resource localization, AAB, v1/v3/v4 signing, signing-key rotation, or
Java-keystore import. Unsupported configuration is rejected rather than ignored.

Android SDK tools are useful as independent validation tools, not build inputs:
`aapt2 dump badging`, `zipalign -c`, and `apksigner verify`. Device installation
still uses `adb` (or an ordinary APK installer).

Format references:
- https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/libs/androidfw/include/androidfw/ResourceTypes.h
- https://source.android.com/docs/security/features/apksigning/v2

## Validation and current host limitation

Run the independent integration tests with:

```sh
ANDROID_BUILD_TOOLS=/path/to/android-sdk/build-tools/34.0.0 \
EGCL_APK_RUNTIME=/usr/libexec/egcl/android/ scripts/test-native-apk.sh
```

The test builds a payload crossing the 1 MiB signing chunk boundary, reloads
the signing identity, checks signatures and alignment, and rejects a tampered
APK. Setting `EGCL_APK_RUNTIME` also tests the complete EGL demo.

The Common Lisp implementation was also exercised under EGCL: small APKs
verify, but the larger signing test hits a stack guard, and increasing the
stack leaves SHA-256 prohibitively slow. This is tracked as `bliss-omaps`.
SBCL is the supported build host for now; the packaged Android runtime is EGCL.
