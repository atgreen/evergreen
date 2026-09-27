# Platform support

This table describes the development checkout. It does not claim complete
ANSI conformance, full FFI parity, or equivalent performance across targets.
Tiers are defined in [How Lisp runs](../explanation/execution.md).

| Runtime target | Native compilation | Distribution / validation |
| --- | --- | --- |
| Linux x86-64 | T1 and T2, OSR and deoptimization | Static musl build or dynamic glibc RPM; host tests |
| Linux s390x | T1 and T2, OSR and deoptimization | Fedora target RPM; QEMU JIT, image, and GC stress checks |
| Linux AArch64 | T1 for supported opcode shapes; no AArch64 T2 backend | Fedora target RPM; baseline backend is under development |
| Linux ppc64le | Interpreter and T0; no native backend | Source cross-build path |
| Windows x86-64 | T1 and T2 for supported shapes | Target RPM runs via Wine; native OS validation remains distinct |
| Android ARM64 CLI | Static runtime; current source includes partial AArch64 T1 | `torcl-android`; QEMU image checks |
| Android ARM64 APK | Dynamic NativeActivity runtime | EGL demo checked on an ARM64 phone |
| Android x86-64 APK | Dynamic NativeActivity runtime | APK and native-library checks; emulator execution not yet verified |

An older installed RPM may predate backend additions in this checkout.
Unsupported native compilation shapes remain in a supported lower tier.
The AArch64 port does not yet provide the x86-64 fiber context-switch and
foreign-callback implementations. This is not full architecture parity.

## Fedora package commands

| Package | Command | Host execution |
| --- | --- | --- |
| `torcl` | `torcl` | Native x86-64 |
| `torcl-target-s390x-linux` | `torcl-s390x-linux` | QEMU s390x |
| `torcl-target-aarch64-linux` | `torcl-aarch64-linux` | QEMU AArch64 |
| `torcl-target-windows` | `torcl-windows` | Wine |
| `torcl-target-android` | `torcl-android` | QEMU ARM64 CLI |
| `torcl-target-android` | `torcl-android-new` | Native Python project generator |

The initial RPM baseline is Fedora 44. Target packages require the exact
version and release of the base package. The Android APK libraries target API
28 or newer; new project manifests currently target API 34.

## Binary compatibility

Heap images are architecture-specific. Linux target executables require
compatible glibc/libgcc on the destination. A static CLI's library-loading
limits differ from a dynamic runtime's. Android native-library alignment has
been checked for 16 KiB pages, but that check does not establish full behavior
on a 16 KiB device.
