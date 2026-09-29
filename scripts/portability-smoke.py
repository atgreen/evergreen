#!/usr/bin/env python3
"""Exercise a real CLI through a supplied native or QEMU command.

Run under scripts/torcl-limited.sh. Example:
  scripts/torcl-limited.sh python3 scripts/portability-smoke.py aarch64 -- \
    qemu-aarch64 -L /path/to/sysroot target/aarch64-unknown-linux-gnu/release/torcl
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ARCHES = {
    "win64": ("X86-64", "X86-64", "LITTLE-ENDIAN"),
    "x86_64": ("X86-64", "X86-64", "LITTLE-ENDIAN"),
    "aarch64": ("ARM64", "ARM64", "LITTLE-ENDIAN"),
    "ppc64le": ("PPC64LE", "PPC64", "LITTLE-ENDIAN"),
    "s390x": ("S390X", "S390X", "BIG-ENDIAN"),
}


def main():
    arch = sys.argv[1]
    machine, feature, endian = ARCHES[arch]
    command = sys.argv[2:]
    if command[0] == "--":
        command = command[1:]
    def target_path(path):
        path = Path(path).resolve()
        if arch == "win64" and os.name != "nt":
            # Wine's default Z: drive maps the Unix filesystem root.
            return "Z:" + path.as_posix()
        return path.as_posix()

    source = Path(__file__).with_name("portability-smoke.lisp").read_text()
    source = (f'(assert (string= (machine-type) "{machine}"))\n'
              f'(assert (member :{feature} *features*))\n'
              f'(assert (member :{endian} *features*))\n' + source)

    if arch == "win64":
        source = '(assert (member :windows *features*))\n(assert (not (member :unix *features*)))\n' + source

    def run(args, env):
        result = subprocess.run(command + ["--no-init"] + args, env=env,
                                capture_output=True, text=True, timeout=180)
        if result.returncode:
            raise RuntimeError(f"exit {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result.stdout

    with tempfile.TemporaryDirectory(prefix="torcl portability-" if arch == "win64" else "torcl-portability-") as directory:
        script = Path(directory) / "smoke.lisp"
        script.write_text(source)
        outputs = []
        for tier in ["interp", "t0", None, "t2"]:
            env = os.environ.copy()
            for key in list(env):
                if key.startswith("TORCL_"):
                    del env[key]
            if tier:
                env["TORCL_FORCE_TIER"] = tier
            output = run(["--load", target_path(script)], env)
            assert "PORTABILITY-OK" in output, output
            outputs.append(output)
            print(f"{arch}: {tier or 'default'}: OK", flush=True)
        assert all(output == outputs[0] for output in outputs), outputs

        if arch == "ppc64le":
            # The parity loop may use the checked fallback.  This separate
            # opt-in probe proves that a leaf body reaches the ELFv2 segment
            # adapter at the Rust/native invocation boundary.
            native_env = env.copy()
            native_env["TORCL_FORCE_TIER"] = "t2"
            native_env["TORCL_NATIVE_TRANSFER"] = "1"
            native_env["TORCL_NATIVE_TRANSFER_DEBUG"] = "1"
            native_env["TORCL_NN_DIRECT"] = "0"
            native = subprocess.run(
                command + [
                    "--no-init",
                    "--eval",
                    "(progn (defun ppc-segment-identity () 41) "
                    "(defun ppc-segment-argument (x) x) "
                    "(defun ppc-segment-branch (x) (if x 1 2)) "
                    "(defun ppc-segment-add (x) (+ x 1)) "
                    "(assert (= (ppc-segment-identity) 41)) "
                    "(assert (= (ppc-segment-argument 42) 42)) "
                    "(assert (= (ppc-segment-branch t) 1)) "
                    "(assert (= (ppc-segment-branch nil) 2)) "
                    "(assert (= (ppc-segment-add 41) 42)) "
                    "(format t \"PPC-NATIVE-SEGMENT-OK~%\"))",
                ],
                env=native_env,
                capture_output=True,
                text=True,
                timeout=180,
            )
            if native.returncode:
                raise RuntimeError(
                    f"PPC native segment exit {native.returncode}\n"
                    f"{native.stdout}\n{native.stderr}"
                )
            assert "PPC-NATIVE-SEGMENT-OK" in native.stdout, native.stdout
            assert native.stderr.count("[native-transfer/ppc64le] direct segment:") >= 3, native.stderr
            print(f"{arch}: opt-in native segment entry: OK", flush=True)

        if arch == "win64":
            home_env = env.copy()
            home_env.pop("HOME", None)
            home = target_path(directory) + "/"
            profile = target_path(directory).replace("/", "\\").replace("\\", "\\\\")
            output = run(["--eval", f'(progn (torcl-ext:setenv "USERPROFILE" "{profile}") '
                          f'(assert (string= (namestring (user-homedir-pathname)) "{home}")) '
                          f'(assert (string= (namestring (parse-namestring "~/probe.txt")) "{home}probe.txt")) '
                          '(format t "HOME-OK~%"))'], home_env)
            assert "HOME-OK" in output, output
            print(f"{arch}: Windows profile home directory: OK", flush=True)
            output = run(["--eval", f'(let ((p (truename "{target_path(script)}"))) '
                          '(assert (not (typep p (quote logical-pathname)))) '
                          '(assert (probe-file p)) '
                          '(with-open-file (s p) (assert (read-line s nil nil))) '
                          '(format t "TRUENAME-OK~%"))'], home_env)
            assert "TRUENAME-OK" in output, output
            print(f"{arch}: canonical pathname reopens its file: OK", flush=True)
            output = run(["--eval", f'(let* ((p (pathname "{target_path(script)}")) '
                          '(pattern (make-pathname :defaults p :name :wild))) '
                          '(assert (find (truename p) (directory pattern) :test (function equal))) '
                          '(assert (equal (pathname-device (truename p)) (pathname-device p))) '
                          '(format t "DIRECTORY-OK~%"))'], home_env)
            assert "DIRECTORY-OK" in output, output
            print(f"{arch}: wildcard directory preserves its drive: OK", flush=True)
            for tier in ["interp", "t0"]:
                home_env["TORCL_FORCE_TIER"] = tier
                output = run(["--eval", '(progn (defun recurse (n) (if (= n 0) 0 '
                              '(1+ (funcall (symbol-function (quote recurse)) (1- n))))) '
                              '(handler-case (recurse 10000) (storage-condition () '
                              '(format t "STACK-CAUGHT~%"))))'], home_env)
                assert "STACK-CAUGHT" in output, output
            print(f"{arch}: recursive stack exhaustion is catchable: OK", flush=True)

        # Stress every allocation in a focused raw-runtime program. Stressing
        # the entire Lisp prelude under software emulation takes minutes before
        # the test starts; --no-bootstrap avoids that cost without a SKIP knob.
        stress_script = Path(__file__).with_name("portability-stress.lisp").resolve()
        env["TORCL_FORCE_TIER"] = "t0"
        reference = run(["--no-bootstrap", "--load", target_path(stress_script)], env)
        env.update(TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
        stressed = run(["--no-bootstrap", "--load", target_path(stress_script)], env)
        assert "STRESS-OK" in stressed and stressed == reference, (reference, stressed)
        print(f"{arch}: raw-runtime GC stress + poison matches baseline: OK", flush=True)
        del env["TORCL_GC_STRESS"]
        del env["TORCL_GC_POISON"]

        if arch == "win64":
            paths = Path(__file__).with_name("windows-pathnames.lisp").resolve()
            for tier in ["interp", "t0"]:
                env["TORCL_FORCE_TIER"] = tier
                output = run(["--load", target_path(paths)], env)
                assert "WINDOWS-PATHNAMES-OK" in output, output
            print(f"{arch}: pathname components, merging, matching and hashing: OK", flush=True)

        # A compiled artifact must load and execute in a fresh process.
        fasl_source = Path(directory) / "compiled.lisp"
        pathname_setup = ('(defparameter *portable-drive* #p"C:/Work/demo.lisp") '
                          '(defparameter *portable-unc* #p"//server/share/Work/demo.lisp") '
                          if arch == "win64" else '')
        pathname_check = ('(assert (equal (pathname-device *portable-drive*) "C")) '
                          '(assert (equal (pathname-directory *portable-drive*) (quote (:absolute "Work")))) '
                          '(assert (equal (pathname-host *portable-unc*) "server")) '
                          '(assert (equal (pathname-device *portable-unc*) "share")) '
                          '(assert (string= (namestring (make-pathname :defaults *portable-unc* :name "new")) '
                          '"//server/share/Work/new.lisp")) ' if arch == "win64" else '')
        fasl_source.write_text('(defun portable-fasl (x) (+ x 17))\n' + pathname_setup)
        fasl = Path(directory) / "compiled.bfasl"
        run(["--eval", f'(compile-file "{target_path(fasl_source)}" :output-file "{target_path(fasl)}")'], env)
        output = run(["--eval", f'(progn (load "{target_path(fasl)}") '
                      f'{pathname_check}(assert (= (portable-fasl 25) 42)) (format t "FASL-OK~%"))'], env)
        assert "FASL-OK" in output, output
        print(f"{arch}: fresh-process compiled-file round trip: OK", flush=True)

        image = Path(directory) / "saved.image"
        run(["--eval", f'(progn {pathname_setup}(defun portable-image () 42) (save-lisp-and-die "{target_path(image)}"))'], env)
        output = run(["--image", target_path(image), "--eval",
                      f'(progn {pathname_check}(assert (= (portable-image) 42)) (format t "IMAGE-OK~%"))'], env)
        assert "IMAGE-OK" in output, output
        print(f"{arch}: fresh-process heap-image round trip: OK", flush=True)


if __name__ == "__main__":
    main()
