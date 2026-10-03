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

## Describing the APK in an .asd

An application can carry its APK configuration in its own system definition and
be built by `asdf:make`, instead of keeping a sibling `apk.sexp`. Name
`egcl-apk-asdf` in `:defsystem-depends-on` and give the system the APK class and
build operation:

```lisp
(defsystem "my-app"                       ; loadable and testable as usual
  :components ((:file "scene") (:file "egl") (:file "app")))

(defsystem "my-app/apk"
  :defsystem-depends-on ("egcl-apk-asdf")
  :class "egcl-apk-asdf:android-apk"
  :build-operation "egcl-apk-asdf:apk-op"
  :depends-on ("my-app")
  :version "0.1"                          ; ASDF's :version is the version name
  :apk-package "org.example.app"
  :apk-label "My App"
  :apk-version-code 1
  :apk-hosts ("aarch64-linux-android" "x86_64-linux-android")
  :apk-permissions ("android.permission.INTERNET")
  :components ((:static-file "app.lisp") (:static-file "scene.lisp")))
```

Then `asdf:make "my-app/apk"` writes `build/my-app.apk`. Every `apk.sexp` field
has an `:apk-` keyword; ASDF rejects an unknown initarg, so a misspelled one is
an error rather than silently ignored, as before. The system's own file
components become the flat assets in declaration order — the asset list is the
component list, so the two cannot drift — and `:apk-entry` (default `app.lisp`)
must name one of them.

A **separate** `/apk` system rather than slots on `my-app`, because `:class` and
`:build-operation` are per-system: putting them on the application would make
`asdf:make` always mean "build an APK" and would stop the system loading at all
on a machine with no Android runtime.

The runtime location stays out of the `.asd` — it describes the build host, not
the application. Set `EGCL_APK_RUNTIME`, or bind
`egcl-apk-asdf:*runtime-directory*`. The `.asd` may only *constrain* it, through
`:apk-runtime-api` and `:apk-runtime-version`, which are checked against the
runtime's own `runtime.json`.

Unlike the CLI, this path will not create a signing identity. `create-identity`
writes an unencrypted P-256 private key and relies on the caller's umask; the
CLI sets 077 first, but `asdf:make` inherits whatever you happen to have and
EGCL exposes no `chmod` to repair the mode afterwards. Mint the key once with
`scripts/egcl-apk`, and the build will reuse it.

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
