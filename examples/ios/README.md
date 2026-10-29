# iOS source application scaffold

This is an **unbuilt, unexecuted** SwiftUI host scaffold for the `egcl-ios`
library. It is not a tested application project or a device-support claim.
The Swift `@main` application owns the UI/main thread; the Rust adapter starts
Lisp on a worker. The demo loads bundled source, writes `result.txt` into the
application's support directory, and calls `QUIT` without terminating Swift.

When SDK builds are authorized, on a Mac with the corresponding Xcode SDK:

1. Build the `egcl-ios` archive for the chosen device or ARM64 simulator target,
   using `cargo rustc -p egcl-ios --release --target <target> --crate-type staticlib`.
   Obtain Rust's target-specific native link libraries with
   `-- --print native-static-libs` and add the reported requirements to the host
   target. No native-link-library list has been verified for this scaffold.
2. Create an iOS SwiftUI application target and use `EGCLIOSDemo.swift` as its
   application entry. Add `Lisp` as a copied folder reference so the bundle
   retains `Lisp/app.lisp` rather than flattening the directory.
3. Use `EGCLIOSDemo.xcconfig` for the app target, set
   `EGCL_REPOSITORY_ROOT` to the checkout's absolute path, and add the
   target-specific native link libraries from the archive build. The config
   selects separate device/simulator archive directories. It requires an ARM64
   simulator host and sets iOS 15 for this SwiftUI demo; it does not establish
   the runtime's oldest supported deployment version.
4. Build/link and run in the simulator, then on a physical device. Check the
   UI stays responsive, the result file contains `answer=42`, and `QUIT` leaves
   the application alive. Repeat focused lifecycle/GC/tier cases before making
   platform claims. None of these steps have been executed here.

The API accepts one start attempt for the process lifetime after synchronous
ABI/string/path-shape validation. Filesystem validation happens asynchronously;
its failures consume the accepted attempt. Completion does not permit a second world or promise to release
all process-global runtime metadata. Descendant OS/TLS retirement and idle
carrier threads can remain after active Lisp execution is terminal. Stop is asynchronous and cooperative:
non-cancellable waits and foreign code can delay completion indefinitely.
`ROOT_COMPLETED` means the root returned and Lisp attachments remain active; only
`FINISHED`/`FAILED` mean the root and active descendants have stopped.

The adapter copies C configuration strings, does not mutate the process
working directory/environment, and supplies separate source and writable Lisp
pathnames. Diagnostics are copied into caller-owned storage. It contains Rust
unwinds with the workspace's default unwind profiles; it does not contain
native memory faults, aborting panics or allocation failure. The app must keep
valid C input/output buffers for each call. No Lisp-to-Swift callback is needed.

See [runtime details](../../docs/ios-runtime.md), the
[C header](../../crates/egcl-ios/include/egcl_ios.h), and Apple's
[app launch guidance](https://developer.apple.com/documentation/uikit/responding-to-the-launch-of-your-app).
