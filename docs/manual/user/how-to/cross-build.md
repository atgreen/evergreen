# Build for another platform

This guide uses the container-free target RPMs on x86-64 Fedora. Install the
base `torcl` package and the matching target package from your local RPM build.
The [platform reference](../reference/platforms.md) lists the commands.

## Create an IBM Z Linux executable

Install `torcl-target-s390x-linux` alongside the same release of `torcl`.
Create `build.lisp`:

```lisp
(defun main () (format t "Hello from IBM Z!~%"))
(save-lisp-and-die "hello-s390x" :executable t :toplevel #'main)
```

Build and inspect the result:

```sh
torcl-s390x-linux --no-init --load build.lisp
file hello-s390x
```

`file` identifies an IBM S/390 ELF executable. Copy it to a compatible s390x
Linux system and run `./hello-s390x` there.

## Test on the build host

The launcher supplies a private runtime library tree to QEMU. For a dumped
executable, explicitly provide that same tree:

```sh
qemu-s390x -L /usr/libexec/torcl/s390x-linux/sysroot ./hello-s390x
```

If running `./hello-s390x` reports that `/lib/ld64.so.1` is missing, the host's
transparent QEMU invocation did not select an s390x library tree. Use the
explicit QEMU command above. Do not put foreign libraries in your host `/lib`.

## Select a different target

Use `torcl-aarch64-linux` or `torcl-windows` in place of the s390x launcher.
Give Windows executable outputs an `.exe` suffix. Windows creation runs under
Wine; the Linux target tools run under QEMU.

A target executable still needs its operating system's runtime libraries. The
Fedora-built Linux runtimes are not a promise of compatibility with older
Linux distributions. QEMU and Wine are build-host tools, not dependencies of
the resulting program on its native target.

For Android graphical applications, use the [APK workflow](android.md).
`torcl-android` produces an ARM64 command-line executable, not an APK.
