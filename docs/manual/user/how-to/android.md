# Build an Android app

Build an EGL application from Linux using the installed `egcl-target-android`
RPM. It supplies native libraries for ARM64 phones and x86-64 emulators, plus
`egcl-android-new`. Application builds need Python 3, GNU Make, a JDK, and the
Android SDK. They do not need Rust, the NDK, a EGCL checkout, or containers.

If you would rather not install the SDK and a JDK at all, skip to
[build without the Android SDK](#build-without-the-android-sdk): `egcl-apk`
assembles and signs an APK entirely in Lisp.

## Generate the project

```sh
egcl-android-new hello --package org.example.hello \
  --name "Hello EGCL" --host=aarch64-linux-android --template=egl
cd hello
```

Choose a new directory: the generator refuses to overwrite an existing one.

## Install SDK tools

On Fedora, install a JDK if one is not already on your `PATH`:

```sh
sudo dnf install java-21-openjdk-devel
make install-tools
make doctor
```

`install-tools` uses an existing SDK manager or downloads a pinned,
checksum-verified copy of Google's command-line tools. Read and accept the
interactive SDK license prompts if you agree. It installs build-tools,
platform-tools, and the platform named by `targetSdkVersion` in the manifest.

The default SDK path is `~/Android/Sdk`, overridden by `ANDROID_HOME` or
`ANDROID_SDK_ROOT`. To use another directory, create `local.mk`:

```make
SDK := /absolute/path/to/android-sdk
```

`local.mk` is ignored by Git. Ordinary `make` builds do not download tools.

## Build and launch on a phone

Enable USB debugging on an ARM64 phone, connect it, and authorize the computer.
Then run:

```sh
make
make install
make run
make logcat
```

The EGL template draws a color and changes it when touched. Edit
`assets/app.lisp`, then repeat `make install` and `make run`. Stop `logcat` with
Ctrl-C. If multiple devices are attached, set `SERIAL` in `local.mk` or pass it
to each device command.

## Build for an x86-64 emulator

Start an x86-64 Android emulator separately, then run:

```sh
make install HOST=x86_64-linux-android SERIAL=emulator-5554
make run SERIAL=emulator-5554
```

The tools target an already running emulator; `install-tools` does not create
an AVD or install an emulator system image. To package both architectures:

```sh
make HOSTS="aarch64-linux-android x86_64-linux-android"
```

The x86-64 APK has packaging validation; emulator execution remains a separate
validation step. See [platform support](../reference/platforms.md).

## Sign a release

Supply your own keystore and alias, and read its password into the environment:

```sh
export KEYSTORE=/absolute/path/to/release.jks KEY_ALIAS=mykey
read -rsp 'Keystore password: ' KEYSTORE_PASSWORD; echo
export KEYSTORE_PASSWORD
make release
unset KEYSTORE_PASSWORD
```

Set `KEY_PASSWORD` too if the key password differs. Release builds disable
debugging. Use `adb install` explicitly to deploy a release APK; `make install`
builds and installs the debug APK. A differently signed APK cannot update an
existing installation without addressing the signing mismatch.

## Build without the Android SDK

`egcl-apk`, also from `egcl-target-android`, builds and signs an APK with no
SDK, JDK, Make or Python — just the `egcl` the package already requires. The
project describes its APK in its own `.asd` rather than in a manifest and
Makefile:

```lisp
;; The primary system exists so ASDF accepts the secondary name below. It has
;; no components because an EGL application's sources are target code: they
;; call bindings that only exist inside the APK runtime.
(asdf:defsystem "hello"
  :components ())

(asdf:defsystem "hello/apk"
  :defsystem-depends-on ("egcl-apk-asdf")
  :class "egcl-apk-asdf:android-apk"
  :build-operation "egcl-apk-asdf:apk-op"
  :pathname "assets/"
  :version "1.0"
  :apk-package "org.example.hello"
  :apk-label "Hello EGCL"
  :apk-version-code 1
  :components ((:static-file "app.lisp")))
```

```sh
egcl-apk hello/
```

That writes `hello/build/hello.apk`, signed with a P-256 key it creates once as
`hello/.egcl-apk-key` (umask 077). Install it with `adb install`. The system's
file components *are* the APK's assets, in declaration order, so the two cannot
drift; `:apk-entry` (default `app.lisp`) names the one that is loaded first.

The first build spends about nine seconds saving an image of the loaded builder
under `${XDG_CACHE_HOME:-$HOME/.cache}/egcl`; later builds restore it. `egcl-apk`
rebuilds that image by itself after an `egcl` upgrade.

See the [builder's README](https://github.com/atgreen/evergreen/blob/main/lib/egcl-apk/README.md)
for every `:apk-*` slot, and the
[Android project reference](../reference/android.md) for build paths,
variables, lifecycle callbacks, and asset limits.
