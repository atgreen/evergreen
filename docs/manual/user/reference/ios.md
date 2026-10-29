# iOS projects

```text
egcl-ios-new DIRECTORY [--bundle-id IDENTIFIER] [--name NAME]
             [--team TEAM] [--runtime-source CHECKOUT]
```

The generator lives in `packaging/ios/egcl-ios-new` in a source checkout. Its
destination must not exist. It writes a source Xcode application and a shared
`LispApp` scheme; it does not run Xcode, Cargo, a simulator, signing, or an
installer. All generator and SDK assertions remain unrun in this implementation.

| Option | Default / meaning |
| --- | --- |
| `--bundle-id` | `org.example.lispapp`; dotted application identifier |
| `--name` | `Lisp App`; application display name |
| `--team` | Empty; select an Apple development team in Xcode |
| `--runtime-source` | Checkout containing the generator; must include `egcl-ios` and its source scaffold |

Display names cannot contain control characters, `$`, `#` or `//`. Runtime
source paths are transported as JSON data instead of Xcode configuration text;
spaces, quotes, non-ASCII characters and `$` remain literal path characters.

| Generated file | Purpose |
| --- | --- |
| `LispApp.xcodeproj` | ARM64 device/simulator application target and shared scheme |
| `LispApp.swift` | Main-thread SwiftUI owner and asynchronous worker status |
| `LispApp-Bridging-Header.h`, `egcl_ios.h` | Versioned copied-string C boundary |
| `Lisp/app.lisp` | Bundled source entry point |
| `Project.xcconfig` | Bundle ID, signing team and deployment target |
| `RuntimeSource.json` | Gitignored machine-specific runtime checkout, versioned JSON |
| `Info.plist` | Display name and application metadata, escaped XML |
| `BuildRuntime.py` | Build selected runtime archive and ordered native linker response |

The default deployment target is iOS 17.0; changing that setting does not prove
compatibility with an older device. Both Xcode configurations currently build a
release Rust runtime; Debug keeps Swift debug information. Native outputs and
Cargo caches are separated by Xcode's selected platform/intermediate paths.

The C boundary offers start, status, cooperative stop and diagnostic copying.
Configuration strings are copied before start returns. The host accepts one
start attempt per process. `ROOT_COMPLETED` means Lisp descendants remain;
`FINISHED` means the root worker was joined and active Lisp children stopped.
It does not mean that all child OS/TLS state, carriers, metadata or heap storage
was destroyed. An explicit Lisp exit supplies a numeric status without exiting
the Swift host. No restart, foreign Lisp callback or App Store support claim is
made. See [embedding ownership](https://github.com/atgreen/evergreen/blob/main/docs/embedding.md).

Rust specifies that the order and duplication in
[`--print native-static-libs`](https://doc.rust-lang.org/rustc/command-line-arguments/print-options.html#native-static-libs)
can matter. The helper retains them in the Clang linker response file.
