# Create an experimental iOS application

The iOS runtime and project generator are source implementations awaiting Apple
SDK, simulator and device validation. They do not establish supported iOS
application delivery. Use an Apple Silicon Mac with Xcode and the repository's
pinned Rust toolchain; install the `aarch64-apple-ios` and
`aarch64-apple-ios-sim` Rust targets before building.

From a runtime source checkout:

```sh
packaging/ios/egcl-ios-new /tmp/MyLispApp \
    --bundle-id org.example.mylispapp --name 'My Lisp App' \
    --runtime-source "$PWD"
open /tmp/MyLispApp/LispApp.xcodeproj
```

Select the `LispApp` scheme and an arm64 simulator or device. Select your
Development Team for device signing. Xcode's build environment must find
`cargo` and `python3`; the generator does not install tools or modify signing
credentials. The project supports ARM64 simulator builds, not x86-64 simulators.

The generated build phase uses Xcode's SDK and deployment target to build a
release EGCL archive. It obtains the native system-library flags from rustc
rather than assuming Linux or macOS linker inputs. Rust caches belong to the
project's intermediate directory. Each build recreates the final archive to
request its native library report; dependencies can remain cached.

Edit `Lisp/app.lisp`. The `Lisp` directory is copied intact into the application
bundle and becomes the scoped default for relative loads. Save application data
under `egcl-ios:*writable-directory*`, a separate Application Support directory.
The Swift main thread starts and polls the worker; it never joins a running
Lisp worker. The Stop button requests cooperative unwinding. Foreign calls and
noncancellable waits can delay completion indefinitely. Only one start attempt
is accepted during an application process's lifetime.

The runtime enforces T0, refuses executable mappings and process creation,
and supports scalar foreign calls through static trampolines. Lisp callbacks
and aggregate FFI arguments are outside this initial profile. Successful source
generation or a simulator launch would not prove the no-executable-memory,
signal-recovery or moving-GC properties on a physical device.

See the [project dictionary](../reference/ios.md) and the runtime contributor's
[platform notes](https://github.com/atgreen/evergreen/blob/main/docs/ios-runtime.md) for the remaining validation scope.
