# Lisp-native Android APK builder

This builds a NativeActivity APK with Common Lisp code and the precompiled
libraries from `egcl-target-android`. APK construction needs no Android SDK,
NDK, JDK, Java, Gradle, Python, or external signing executable.

Installing `egcl-target-android` puts the builder in
`/usr/share/common-lisp/source/egcl-apk/`, which ASDF's default system source
registry already searches as a `(:TREE ...)`. So the builder and the
dependencies `ocicl.csv` pins are all found by name, and building an APK is
plain ASDF -- no launcher, no paths, no configuration:

```sh
cd my-app
egcl --eval '(require :asdf)' \
     --eval '(asdf:load-asd (truename "my-app.asd"))' \
     --eval '(asdf:make "my-app/apk")'
```

`(require :asdf)` is a no-op on an installed `egcl`, whose appended image
already holds ASDF; it is there so the same command works with a freshly built
one. `asdf:load-asd` wants an absolute pathname, hence `truename`.
`:defsystem-depends-on ("egcl-apk-asdf")` in the `.asd` pulls the builder in, so
nothing loads it explicitly.

From this source tree the dependencies are not in that tree, so install them
once (ocicl and Git are setup tools) and point ASDF at where they land:

```sh
cd lib/egcl-apk && ocicl install && cd -

export CL_SOURCE_REGISTRY="(:source-registry (:tree \"$PWD/lib/egcl-apk/\") :ignore-inherited-configuration)"
egcl --eval '(require :asdf)' \
     --eval '(asdf:load-asd (truename "examples/android-egl/android-egl.asd"))' \
     --eval '(asdf:make "android-egl/apk")'
```

SBCL builds the same project about 6x faster (1.0 s against 6.2 s) and is the
quicker host while iterating -- `sbcl --non-interactive` with the same three
forms, and no `(require :asdf)` needed.

Set `EGCL_APK_RUNTIME` if the Android RPM is extracted somewhere other than
`/usr/libexec/egcl/android`; use an absolute path. The output pathname and the
signing identity are `:apk-output` and `:apk-identity` in the project's `.asd`,
not environment variables. The default output is `PROJECT/build/NAME.apk` for
the project's primary system NAME.

`load.lisp` is the test and development bootstrap: it registers exactly the
directories `ocicl.csv` pins, for `tests/run.lisp` and `save.lisp`. It is not
installed, and the installed builder has no equivalent -- the `(:TREE ...)`
above does that job.

Where the 6.2 s of an EGL demo build goes, measured against SBCL phase by
phase (installed egcl, so ASDF is already in its image):

| | SBCL | EGCL |
|---|---|---|
| ASDF loading the builder and its pinned dependencies | 0.39 s | 2.95 s |
| assembling and signing the APK | 0.39 s | 1.58 s |
| the rest: startup, `load-asd`, `asdf:make` planning | 0.22 s | 1.67 s |
| **total** | **1.00 s** | **6.20 s** |

EGCL is slower at every phase except SHA-256, where the native builtin beats
Ironclad's Lisp (0.093 s against 0.128 s); CRC-32 is at parity. Most of what is
left is ASDF's own plan computation, which is about 100x SBCL and tracked
separately -- an `asdf:load-system` of an already-loaded system takes 1.30 s
under EGCL and 0.013 s under SBCL, doing no I/O either time.

`save.lisp` writes an image with ASDF, the pinned dependencies, Ironclad and the
builder already loaded, which takes a build to about 4.5 s:

```sh
EGCL_APK_IMAGE=~/.cache/egcl/apk.core \
  egcl --eval '(require :asdf)' --eval '(load "lib/egcl-apk/save.lisp")'
egcl --image ~/.cache/egcl/apk.core --eval '(asdf:make "my-app/apk")'
```

(EGCL rejects `--eval` and `--load` in the same command line, hence the
`(load ...)` form above.)

That is an opt-in for someone building many APKs in a row, not something the
package manages: an image is refused by any `egcl` but the one that wrote it
("runtime source mismatch"), so it has to be rebuilt whenever `egcl` is, and the
Android package is built in its own `rpmbuild` with no host `egcl` to write one
with anyway.

The first build creates `PROJECT/.egcl-apk-key` with a P-256 private key and
self-signed certificate; subsequent builds reuse both. `create-identity`
chmods it 0600 itself rather than relying on the caller's umask -- that is what
removed the need for a wrapper script around every build, and it holds even
under `umask 000`. Keep this file private and backed up: Android application
updates need the same signing certificate. It is a small EGCL-specific binary
identity format, not a Java keystore, and it is unencrypted. No existing signing
keys are imported or modified. This initial builder is intended for development
APKs.

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

Then `asdf:make "my-app/apk"` writes `build/my-app.apk`. ASDF rejects an unknown
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

A plain `asdf:make` creates a missing signing identity, 0600.
`create-identity` sets the key's mode itself, so this needs nothing of its
caller.

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
EGCL_APK_TEST_PROJECT=$PWD/examples/android-egl \
  egcl --no-init --eval '(require :asdf)' \
       --eval '(load "lib/egcl-apk/tests/run.lisp")'
```

reports `APK-UNIT-OK`, `APK-SIGNING-OK`, `APK-ASDF-OK`, and with
`EGCL_APK_RUNTIME` set, `APK-DEMO-OK`. The stack-guard
crash this previously hit at the default 512 KiB stack is fixed (`bliss-omaps`);
open-coding `LDB` removed the interpreted frames whose depth caused it. Signing
is still far slower than SBCL -- the chunk-boundary test takes minutes rather
than seconds -- so SBCL remains the quicker build host while EGCL is the
supported self-hosted one.
