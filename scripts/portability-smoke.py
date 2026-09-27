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
    source = Path(__file__).with_name("portability-smoke.lisp").read_text()
    source = (f'(assert (string= (machine-type) "{machine}"))\n'
              f'(assert (member :{feature} *features*))\n'
              f'(assert (member :{endian} *features*))\n' + source)

    def run(args, env):
        result = subprocess.run(command + ["--no-init"] + args, env=env,
                                capture_output=True, text=True, timeout=180)
        if result.returncode:
            raise RuntimeError(f"exit {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result.stdout

    with tempfile.TemporaryDirectory(prefix="torcl-portability-") as directory:
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
            output = run(["--load", str(script)], env)
            assert "PORTABILITY-OK" in output, output
            outputs.append(output)
            print(f"{arch}: {tier or 'default'}: OK", flush=True)
        assert all(output == outputs[0] for output in outputs), outputs

        # Stress every allocation in a focused raw-runtime program. Stressing
        # the entire Lisp prelude under software emulation takes minutes before
        # the test starts; --no-bootstrap avoids that cost without a SKIP knob.
        stress_script = Path(__file__).with_name("portability-stress.lisp").resolve()
        env["TORCL_FORCE_TIER"] = "t0"
        reference = run(["--no-bootstrap", "--load", str(stress_script)], env)
        env.update(TORCL_GC_STRESS="1", TORCL_GC_POISON="1")
        stressed = run(["--no-bootstrap", "--load", str(stress_script)], env)
        assert "STRESS-OK" in stressed and stressed == reference, (reference, stressed)
        print(f"{arch}: raw-runtime GC stress + poison matches baseline: OK", flush=True)
        del env["TORCL_GC_STRESS"]
        del env["TORCL_GC_POISON"]

        # A compiled artifact must load and execute in a fresh process.
        fasl_source = Path(directory) / "compiled.lisp"
        fasl_source.write_text('(defun portable-fasl (x) (+ x 17))\n')
        fasl = Path(directory) / "compiled.bfasl"
        run(["--eval", f'(compile-file "{fasl_source}" :output-file "{fasl}")'], env)
        output = run(["--eval", f'(progn (load "{fasl}") '
                      '(assert (= (portable-fasl 25) 42)) (format t "FASL-OK~%"))'], env)
        assert "FASL-OK" in output, output
        print(f"{arch}: fresh-process compiled-file round trip: OK", flush=True)

        image = Path(directory) / "saved.image"
        run(["--eval", f'(progn (defun portable-image () 42) (save-image "{image}"))'], env)
        output = run(["--image", str(image), "--eval",
                      '(progn (assert (= (portable-image) 42)) (format t "IMAGE-OK~%"))'], env)
        assert "IMAGE-OK" in output, output
        print(f"{arch}: fresh-process heap-image round trip: OK", flush=True)


if __name__ == "__main__":
    main()
