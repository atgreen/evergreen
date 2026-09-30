# Android projects

## Generator

```text
egcl-android-new DIRECTORY [--host HOST] [--template egl|minimal]
                  [--package PACKAGE] [--name NAME] [--runtime DIRECTORY]
```

| Option | Default / meaning |
| --- | --- |
| `--host` | `aarch64-linux-android`; also accepts `x86_64-linux-android` |
| `--template` | `egl`; `minimal` logs lifecycle/input without drawing |
| `--package` | `org.example.hello`; dotted Android application ID |
| `--name` | Destination directory name; Android display label |
| `--runtime` | Installed runtime directory; also configurable through `EGCL_ANDROID_RUNTIME` |

The destination must not exist. The runtime provides both architectures.

## Make targets

| Target | Action |
| --- | --- |
| `apk` (default) | Build and sign a debug APK |
| `install-tools` | Install SDK components; bootstrap SDK manager if needed |
| `doctor` | Check runtime metadata, libraries, SDK packaging tools, and assets |
| `verify` | Build a debug APK, check signature/alignment, and print package metadata |
| `install` | Check device ABI, then build and install the debug APK |
| `run` | Launch the installed Activity |
| `logcat` | Follow EGCL, Android runtime, and libc diagnostics |
| `release` | Build an APK using supplied release-signing credentials |
| `clean` | Remove `build/`; preserve the debug keystore |
| `help` | Print build and device-selection help |

`doctor` does not boot an emulator or validate a device connection.
`install-tools` requires network access and a JDK, and leaves license prompts
interactive. It installs SDK platform-tools, the manifest's platform, and the
selected build-tools version. It does not install system packages or the NDK.

## Variables

| Variable | Meaning |
| --- | --- |
| `HOST` | One target; defaults to the generator's choice |
| `HOSTS` | Space-separated targets; overrides `HOST` for APK contents |
| `SDK` | SDK path; defaults to `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then `~/Android/Sdk` |
| `SDK_BUILD_TOOLS` | Version to install; default `35.0.0` |
| `BUILD_TOOLS` | Explicit build-tools directory; otherwise newest installed numeric version |
| `ANDROID_JAR` | Platform JAR override; otherwise selected from the manifest |
| `SERIAL` | `adb` device selector |
| `RUNTIME` | Runtime installation; default `/usr/libexec/egcl/android` |
| `KEYSTORE`, `KEY_ALIAS`, `KEYSTORE_PASSWORD` | Required environment variables for release signing |
| `KEY_PASSWORD` | Optional separate key password |

Machine-specific Make variables belong in ignored `local.mk`. Keep signing
secrets out of source files. Debug builds create an ignored `.debug.keystore`.
APKs are written to `build/<hosts>/debug/app.apk` or
`build/<hosts>/release/app.apk`; multiple hosts are sorted and joined with `+`.

## Host and ABI mapping

| Host | Android ABI |
| --- | --- |
| `aarch64-linux-android` | `arm64-v8a` |
| `x86_64-linux-android` | `x86_64` |

## Project files

| File | Purpose |
| --- | --- |
| `AndroidManifest.xml` | Package identity, SDK levels, permissions, Activity settings |
| `app.json` | Runtime API/version contract and initial project identity |
| `assets/app.lisp` | Application code |
| `assets/android.lisp` | Runtime lifecycle and input bindings |
| `assets/egl.lisp` | EGL/OpenGL ES bindings used by the sample |
| `res/` | Optional Android resources |

Manifest identity is used for packaging and launching. Changing the recorded
runtime version in `app.json` requires checking compatibility with the installed
runtime. Asset symlinks are rejected; individual assets are limited to 64 MiB.
`egcl-assets.txt` is a reserved generated asset name.

## Lisp lifecycle API

Define `(cl-user::android-main window)`, where `window` is a foreign pointer to
an `ANativeWindow`. It is called for each surface on the same Lisp worker
thread. Keep durable-in-process application state in Lisp globals.

| Function | Contract |
| --- | --- |
| `(android:running-p)` | False when the current surface must be released |
| `(android:paused-p)` | True when rendering should pause |
| `(android:poll-touch)` | Action, x, y as multiple values; `nil` if no queued event |
| `(android:log message)` | ASCII diagnostic under the `egcl` logcat tag |

Touch coordinates are pixels. Actions are `0` down, `1` up, and `2` move;
only the primary pointer is exposed. The queue keeps the last 64 events.

Check `running-p` on every render iteration, return promptly when it becomes
false, and release EGL resources with `unwind-protect`. Android waits for cleanup
before releasing the window. There is one interpreter per Activity and one
EGCL Activity per process.

The runtime replaces the extracted asset directory on Activity creation.
Relative `load` and file operations use that directory. Do not store persistent
user data there. Saved-image APK payloads are not implemented in this version.
