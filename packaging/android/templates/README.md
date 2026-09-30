# EGCL Android application

Edit `assets/app.lisp`, then build and install:

```sh
make install-tools   # first-time SDK setup
make doctor
make
make install
make run
make logcat
```

Requires `egcl-target-android`, Python 3, GNU Make, a JDK (`keytool`), and
Android SDK build-tools, platform-tools and platform 34. No EGCL source
checkout, Rust compiler, NDK, Gradle or containers are needed for application
builds. The RPM supplies both native libraries.

Run `make install-tools` to install SDK platform-tools, build-tools 35.0.0 and the
platform matching `targetSdkVersion` in the manifest. It reuses `sdkmanager` when
available, otherwise downloads a pinned, SHA-256-verified copy of Google's Linux
command-line tools. SDK license prompts are interactive; read and accept them
if you agree. This target requires network access and a JDK; on Fedora install
the JDK with `sudo dnf install java-21-openjdk-devel`. It does not use sudo or
install system packages itself. Ordinary builds do not download tools.

The default SDK directory is `~/Android/Sdk` unless an SDK environment variable
is set. Use `make install-tools SDK=/path/to/sdk` to select another writable
directory, and use the same SDK for subsequent builds. Set `SDK_BUILD_TOOLS`
to select a different build-tools version (for example, `34.0.0`).

Set `SDK` in an ignored `local.mk`, or export `ANDROID_HOME`/`ANDROID_SDK_ROOT`.
`BUILD_TOOLS` and `ANDROID_JAR` can override the selected SDK paths.

```sh
make HOST=aarch64-linux-android            # ARM64 phone
make HOST=x86_64-linux-android             # x86-64 Android emulator
make HOSTS="aarch64-linux-android x86_64-linux-android"  # universal APK
make install HOST=x86_64-linux-android SERIAL=emulator-5554
make run SERIAL=emulator-5554
```

The default host is `@HOST@`. APKs have separate paths under
`build/<host-or-hosts>/debug/app.apk`. `install` checks the device's ABIs before
building. Enable USB debugging and authorize the computer on the phone.

`AndroidManifest.xml` controls package ID, display name, version, SDK levels and
permissions. To add an icon, put PNG resources under `res/drawable/` and set the
application's `android:icon` to `@drawable/your_icon`. `app.json` records the
runtime version/API contract; runtime upgrades with a different version require
reviewing and updating that value. `RUNTIME` overrides the installed runtime
location for testing RPM payloads.

The runtime extracts the indexed `assets/` into app-private storage on Activity
creation, then loads `android.lisp` and `app.lisp`. Ordinary Lisp `load` and file
APIs work relative to this asset directory. It is replaced on the next Activity
creation, so do not store persistent user data in it. Asset files are limited to
64 MiB each; symlinks and newline-containing names are rejected.

Define `(android-main window)`; WINDOW is a foreign pointer to ANativeWindow.
It is called for each new surface on the same Lisp worker thread. Keep application
state in Lisp globals across surface recreation. Your loop must check
`android:running-p` every iteration, return promptly when false, and release its
EGL resources with `unwind-protect`. Android waits for that cleanup before
releasing the window. `android:paused-p` lets you suspend drawing without losing
state. `android:poll-touch` returns action, x, y or NIL (primary-pointer events;
0 down, 1 up, 2 move). Coordinates are window pixels. The queue retains the last
64 events. The EGL template handles these details and changes color on touch.

`android:log` writes ASCII diagnostics to the `egcl` logcat tag. The minimal
template logs lifecycle/input activity and intentionally draws nothing.
There is one Lisp interpreter per Activity and one EGCL Activity per process.
This version starts from Lisp source assets; saved-image application payloads
are not yet supported.

Debug builds use an automatically generated `.debug.keystore`, preserved by
`make clean` and ignored by Git. Release builds require your own signing key:

```sh
export KEYSTORE=/path/to/release.jks KEY_ALIAS=mykey
read -rsp 'Keystore password: ' KEYSTORE_PASSWORD; echo
export KEYSTORE_PASSWORD
make release
unset KEYSTORE_PASSWORD
```

Set `KEY_PASSWORD` too if it differs. `make release` disables debugging and writes
`build/<hosts>/release/app.apk`; passwords go to apksigner through environment
variables. Never commit your keys or passwords. `make install` installs the debug
APK; use `adb install` explicitly for a release APK. Debug and release keys are
different identities and cannot update each other's installed app.
