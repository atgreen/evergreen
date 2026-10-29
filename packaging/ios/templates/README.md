# LispApp

This experimental source project embeds EGCL's T0-only, scalar-FFI iOS worker
runtime. The generator, SDK build and device execution have not been validated.

Open `LispApp.xcodeproj`, select the `LispApp` scheme and an arm64 simulator or
iOS device, then select your signing team. Install the runtime checkout's pinned
Rust toolchain and both `aarch64-apple-ios` and `aarch64-apple-ios-sim` targets
first. Xcode must be able to find `cargo` and `python3` in its build environment.

The first build phase builds `egcl-ios` as a release static library using
Xcode's selected SDK and deployment target. Rust dependencies are cached under
this project's intermediate directory. The final Rust archive is recreated to
obtain the exact ordered native library list from rustc; the linker consumes
that list through a response file. Device and simulator outputs are separate.
No SDK downloads, signing credential changes, or Rust target installation occur
automatically.

Edit `Lisp/app.lisp` to change the application. The whole `Lisp` folder is a
bundle resource, so relative source loads work from that directory. Data belongs
under `egcl-ios:*writable-directory*`; bundle resources are read-only.

The Swift main thread starts and polls the worker. Stop is cooperative: foreign
calls or noncancellable waits can leave it pending. One start attempt is accepted
per process lifetime. Completion means the root was joined and active Lisp
children stopped; it does not destroy all runtime metadata or join every OS
thread. Do not restart Lisp after completion. FFI callbacks and aggregate calls
are unsupported. Lisp EXIT does not terminate the Swift application.

`RuntimeSource.json` records the machine-specific runtime checkout as JSON
data and is gitignored. Update `runtime_source` when moving it. Keep
`Project.xcconfig` profile settings unless you intend to change the supported
profile. `Info.plist` stores the display name as escaped XML. No verified App Store distribution,
signal-recovery, GC-stress or physical-device support is claimed.
