# Lisp-native Android APK builder

This builds a NativeActivity APK with Common Lisp code and the precompiled
libraries from `egcl-target-android`. APK construction needs no Android SDK,
NDK, JDK, Java, Gradle, Python, or external signing executable.

Installing `egcl-target-android` installs the builder too, as `/usr/bin/egcl-apk`:

```sh
egcl-apk my-app/
```

From this source tree, install the pinned Lisp dependencies once (ocicl and Git
are setup tools):

```sh
cd lib/egcl-apk
ocicl install
```

then build from the repository root:

```sh
scripts/egcl-apk examples/android-egl
```

`scripts/egcl-apk` is a thin wrapper that points the installed launcher
(`packaging/android/egcl-apk`) at this tree, so there is only one
implementation. It uses SBCL by default (override its executable with
`SBCL_BIN`); set `EGCL_BIN` instead to build with EGCL, which is self-hosted
but slower -- see the validation section. `/usr/bin/egcl-apk` uses EGCL.

Set `EGCL_APK_RUNTIME` if the Android RPM is extracted somewhere other than
`/usr/libexec/egcl/android`; use an absolute path. The output pathname and the
signing identity are `:apk-output` and `:apk-identity` in the project's `.asd`,
not environment variables. The default output is `PROJECT/build/NAME.apk` for
the project's primary system NAME.

The dependency tree is pinned, so `load.lisp` registers exactly the directories
`ocicl.csv` names and never consults an installed ocicl runtime -- which is why
the builder works from an RPM, where there is no ocicl.

Under EGCL, loading ASDF, the pinned dependencies, Ironclad and the builder from
source costs about 9 s of every build. The launcher therefore saves an image
(see `save.lisp`) under `${XDG_CACHE_HOME:-$HOME/.cache}/egcl/apk-builder.core`
on first use, cutting a build of the EGL demo from about 21 s to about 11 s. An
image is refused by any egcl but the one that wrote it, so the launcher rebuilds
it whenever restoring fails; `EGCL_APK_IMAGE` names a different one. The RPM
ships no image, because the Android package is built in its own `rpmbuild` with
no host egcl to write one with.

The launcher sets umask 077. The first build creates `PROJECT/.egcl-apk-key`
with a P-256 private key and self-signed certificate; subsequent builds reuse
both. Keep this file private and backed up: Android application updates need
the same signing certificate. It is a small EGCL-specific binary identity
format, not a Java keystore. It is unencrypted; library callers must supply
an appropriately restrictive umask themselves. No existing signing keys are
imported or modified. This initial builder is intended for development APKs.

## Describing the APK

An application can carry its APK configuration in its own system definition and
be built by `asdf:make`. This is the only way to describe an APK. Name
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

Then `asdf:make "my-app/apk"` writes `build/my-app.apk`, and
`egcl-apk my-app/` does the same from a shell. ASDF rejects an unknown
initarg, so a misspelled slot is an error rather than silently ignored. The system's own file
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

A plain `asdf:make` will not create a missing signing identity.
`create-identity` writes an unencrypted P-256 private key and relies on the
caller's umask, and EGCL exposes no `chmod` to repair the mode afterwards.
The `egcl-apk` launcher sets umask 077 and then binds
`egcl-apk-asdf:*allow-identity-creation*`, so the permission travels with the
only caller that has established the umask. Mint the key once through the
launcher; every later build, by either route, reuses it.

## Project contract

The APK is described by the project's ASDF system definition, and that is the
only way -- there is no `apk.sexp`. Supported slots: `:apk-package`,
`:apk-label`, `:apk-version-code`, `:apk-min-sdk`, `:apk-target-sdk`,
`:apk-debuggable`, `:apk-permissions`, `:apk-hosts`, `:apk-runtime-api`,
`:apk-runtime-version`, `:apk-entry`, `:apk-identity` and `:apk-output`, with
ASDF's own `:version` supplying the version name. See
`examples/android-egl/android-egl.asd`.

The initial scope is deliberately bounded:

- NativeActivity, no Java or DEX; minimum API 28; EGL ES 2.0.
- Literal Unicode app label and built-in fullscreen Android theme.
- Flat assets from the system's file components, including `app.lisp`; each
  file at most 64 MiB.
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
APK. It also covers the ASDF integration against a synthesised runtime, so that
part needs no `egcl-target-android` install. Setting `EGCL_APK_RUNTIME` also
tests the complete EGL demo.

EGCL now builds APKs itself, so the toolchain is self-hosted:

```sh
EGCL_BIN=/path/to/egcl scripts/egcl-apk examples/android-egl
egcl --no-init --load lib/asdf.lisp --load lib/egcl-apk/tests/run.lisp
```

reports `APK-UNIT-OK`, `APK-SIGNING-OK` and `APK-ASDF-OK`. The stack-guard
crash this previously hit at the default 512 KiB stack is fixed (`bliss-omaps`);
open-coding `LDB` removed the interpreted frames whose depth caused it. Signing
is still far slower than SBCL -- the chunk-boundary test takes minutes rather
than seconds -- so SBCL remains the quicker build host while EGCL is the
supported self-hosted one.
