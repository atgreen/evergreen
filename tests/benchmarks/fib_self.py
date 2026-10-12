#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Compare release EGCL before/after and COMPILE-FILE SBCL on untyped fib(35).

Three fresh processes per implementation, seven timed calls per process, with
rotated implementation order. All compilation, warmup, and disassembly happen
outside the timed region. EGCL loads source and waits for T2; SBCL compiles the
same source to a private FASL per process. This is not an EGCL FASL benchmark.

Example (absolute paths recommended):
  python3 tests/benchmarks/fib_self.py --before /path/before --after /path/after \
    --sbcl /path/sbcl --before-revision REV --after-revision REV \
    --sbcl-revision VERSION --build-description 'release; toolchain; features; logs' \
    --cpu 4 --output /tmp/fib-comparison

The runner rejects changed code, wrong checksums, missing native T2, or missing
AFTER ordinary-ABI/direct-self-call evidence. Raw logs and partial manifest are
retained even on failure. No processes are launched by importing this module.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time


SIDES = ("before", "after", "sbcl")
SAMPLES = 7
TRIALS = 3
CHECKSUM = 9227465


def parse_trial(stdout, side, sample_count=SAMPLES):
    listings = re.findall(r"FIB-DISASSEMBLY-BEGIN\s*(.*?)FIB-DISASSEMBLY-END", stdout, re.S)
    if len(listings) != 2:
        raise ValueError("expected two disassembly snapshots")
    if listings[0] != listings[1]:
        raise ValueError("installed code changed during trial")
    listing = listings[0]
    if not re.search(r"\bFIB\b", listing, re.I):
        raise ValueError("disassembly does not identify FIB")
    evidence = {"listing_sha256": hashlib.sha256(listing.encode()).hexdigest()}
    if side == "sbcl":
        calls = re.findall(r"\bCALL\s+L0\b", listing, re.I)
        if len(calls) != 2:
            raise ValueError("SBCL must show two compiled self calls to L0")
        evidence["self_call_count"] = len(calls)
    else:
        if "; T2 —" not in listing or "installed native code." not in listing:
            raise ValueError("EGCL must show installed T2 native code")
        if side == "after":
            if "; Ordinary x86-64 Lisp ABI — installed native code." not in listing:
                raise ValueError("AFTER must use ordinary ABI")
            base_match = re.search(r"^FIB-CODE-BASE (\d+)\s*$", listing, re.M)
            if not base_match:
                raise ValueError("missing installed code base")
            base = int(base_match[1])
            instructions = dict((int(offset, 16), text.strip().lower()) for offset, text in
                                re.findall(r"^\s*\+([0-9a-fA-F]+):\s*(.*?)\s*$", listing, re.M))
            # Identify the accepted public arity branch, rather than assuming
            # its target starts with PUSH RBP: it may now contain an early return.
            # This also rejects unrelated internal prologues as self-call proof.
            ordered = list(instructions.items())
            accepted = set()
            for index in range(len(ordered) - 3):
                gate = [text for _, text in ordered[index:index + 4]]
                if (re.fullmatch(r"cmp\s+rcx,\s*1h?", gate[0])
                        and re.match(r"mov\s+r11,", gate[2])
                        and gate[3] == "jmp r11"):
                    match = re.fullmatch(r"je\s+(?:near\s+)?(?:0x([0-9a-f]+)|([0-9a-f]+)h)", gate[1])
                    if match:
                        target = int(match[1] or match[2], 16) - base
                        if target in instructions:
                            accepted.add(target)
            # Numeric near CALLs are RIP-relative in 64-bit mode. Indirect
            # calls do not match; both edges must target the accepted entry.
            calls = []
            for offset, instruction in instructions.items():
                match = re.match(r"call\s+(?:0x([0-9a-f]+)|([0-9a-f]+)h)(?:\s|$)", instruction)
                if match:
                    target = int(match[1] or match[2], 16) - base
                    if target in accepted:
                        calls.append((offset, target))
            counts = Counter(target for _, target in calls)
            if len(accepted) != 1 or len(calls) != 2 or len(counts) != 1:
                raise ValueError("AFTER must show two relative self calls to the accepted ordinary entry")
            evidence.update(self_call_count=2, self_call_target_offset=calls[0][1],
                            self_call_offsets=[offset for offset, _ in calls], code_base=base)
    rows = re.findall(r"^FIB-SAMPLE (\d+) (\d+) (\d+) (\d+)\s*$", stdout, re.M)
    if len(rows) != sample_count:
        raise ValueError("incomplete sample set")
    samples = []
    for index, row in enumerate(rows):
        sample, ticks, units, checksum = map(int, row)
        if sample != index or ticks <= 0 or units <= 0 or checksum != CHECKSUM:
            raise ValueError("invalid sample order, duration, units, or checksum")
        samples.append(dict(index=sample, ticks=ticks, units_per_second=units,
                            seconds=ticks / units, checksum=checksum))
    if len({sample["units_per_second"] for sample in samples}) != 1:
        raise ValueError("sample clock units changed")
    return dict(samples=samples, **evidence)


def lisp_string(path):
    return '"' + str(path).replace('\\', '\\\\').replace('"', '\\"') + '"'


def driver(side, source, fasl):
    load = (f"(load (compile-file {lisp_string(source)} :output-file {lisp_string(fasl)}))"
            if side == "sbcl" else f"(load {lisp_string(source)})")
    wait = "" if side == "sbcl" else """
(dotimes (attempt 1000)
  (when (= 2 (egcl-ext:function-tier 'fib)) (return))
  (fib 8)
  (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'fib)))
"""
    base = "" if side == "sbcl" else """
(let ((code (egcl::%native-code 'fib)))
  (assert (first code))
  (format t "FIB-CODE-BASE ~D~%" (second code)))
"""
    listing = f"""(format t "FIB-DISASSEMBLY-BEGIN~%")
{base}(disassemble 'fib)
(format t "FIB-DISASSEMBLY-END~%")
(finish-output)
"""
    return f"""{load}
(dotimes (warm 6000) (fib 8))
{wait}
(assert (= (fib 35) {CHECKSUM}))
{listing}
(dotimes (sample {SAMPLES})
  (let* ((start (get-internal-real-time))
         (checksum (fib 35))
         (elapsed (- (get-internal-real-time) start)))
    (assert (= checksum {CHECKSUM}))
    (format t "FIB-SAMPLE ~D ~D ~D ~D~%"
            sample elapsed internal-time-units-per-second checksum)
    (finish-output)))
{listing}
"""


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def summarize(samples):
    seconds = sorted(sample["seconds"] for sample in samples)
    return dict(sample_count=len(seconds), min_seconds=seconds[0],
                median_seconds=statistics.median(seconds), max_seconds=seconds[-1])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for side in SIDES:
        parser.add_argument(f"--{side}", type=Path, required=True)
        parser.add_argument(f"--{side}-revision", required=True)
    parser.add_argument("--build-description", required=True,
                        help="matching EGCL release profiles, toolchain/features and build logs; SBCL build/version")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpu", type=int, required=True)
    args = parser.parse_args()
    if platform.machine() not in ("x86_64", "amd64"):
        parser.error("ordinary x86-64 self-call evidence requires an x86-64 host")
    if args.cpu not in os.sched_getaffinity(0):
        parser.error("CPU is outside allowed affinity")
    if not shutil.which("taskset"):
        parser.error("taskset is required")
    for side in SIDES:
        binary = getattr(args, side).resolve()
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"{side} binary is not executable: {binary}")
    root = Path(__file__).resolve().parents[2]
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    source = output / "fib_self.lisp"
    shutil.copyfile(Path(__file__).with_suffix(".lisp"), source)
    # Strip inherited tier/tuning/stress choices, including earlier Bliss names.
    removed = sorted(key for key in os.environ if key.startswith(("EGCL_", "BLISS_", "SBCL_")))
    clean_env = {key: value for key, value in os.environ.items() if key not in removed}
    controls = dict(EGCL_FORCE_TIER="t2", EGCL_MEM_MAX="1G", EGCL_TIMEOUT="600")
    manifest = dict(build_description=args.build_description, cpu=args.cpu,
                    platform=platform.platform(), machine=platform.machine(),
                    python=platform.python_version(), allowed_affinity=sorted(os.sched_getaffinity(0)),
                    host_cpuinfo=Path("/proc/cpuinfo").read_text(),
                    workload_sha256=sha256(source), runner_sha256=sha256(Path(__file__)),
                    samples_per_process=SAMPLES, processes_per_implementation=TRIALS,
                    checksum=CHECKSUM, metric="wall seconds per fib(35); untyped speed=3 safety=1 debug=1",
                    load_modes={"before": "source LOAD", "after": "source LOAD", "sbcl": "COMPILE-FILE then LOAD FASL"},
                    removed_environment_names=removed, egcl_controls=controls,
                    binaries={}, trials=[])
    cpu_state = {}
    for name, path in {
        "governor": f"/sys/devices/system/cpu/cpu{args.cpu}/cpufreq/scaling_governor",
        "current_khz": f"/sys/devices/system/cpu/cpu{args.cpu}/cpufreq/scaling_cur_freq",
        "boost": "/sys/devices/system/cpu/cpufreq/boost",
        "intel_no_turbo": "/sys/devices/system/cpu/intel_pstate/no_turbo",
    }.items():
        try:
            cpu_state[name] = Path(path).read_text().strip()
        except OSError:
            cpu_state[name] = None
    manifest["cpu_frequency_state_at_start"] = cpu_state
    for side in SIDES:
        binary = getattr(args, side).resolve()
        manifest["binaries"][side] = dict(path=str(binary), sha256=sha256(binary),
                                          revision=getattr(args, f"{side}_revision"))

    def save():
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    save()
    for trial in range(TRIALS):
        for side in SIDES[trial:] + SIDES[:trial]:
            name = f"{side}-{trial}"
            script = output / f"{name}.lisp"
            script.write_text(driver(side, source, output / f"{name}.fasl"))
            binary = manifest["binaries"][side]["path"]
            env = dict(clean_env)
            command = ["taskset", "-c", str(args.cpu), binary]
            if side == "sbcl":
                command += ["--noinform", "--non-interactive", "--no-userinit", "--no-sysinit", "--load", str(script)]
            else:
                env.update(controls, EGCL_T2_LOG=str(output / f"{name}.t2.log"))
                command = [str(root / "scripts/egcl-limited.sh")] + command
                command += ["--no-init", "--load", str(script)]
            record = dict(side=side, trial=trial, command=command, driver_sha256=sha256(script),
                          started_unix=time.time(), load_before=os.getloadavg())
            manifest["trials"].append(record)
            save()
            try:
                # EGCL's limiter owns timeout and descendant cleanup. SBCL has
                # no worker child processes here; give it the same 600s budget.
                result = subprocess.run(command, cwd=root, env=env, capture_output=True,
                                        text=True, timeout=650 if side == "sbcl" else None)
                (output / f"{name}.stdout").write_text(result.stdout)
                (output / f"{name}.stderr").write_text(result.stderr)
                record.update(stdout_sha256=sha256(output / f"{name}.stdout"),
                              stderr_sha256=sha256(output / f"{name}.stderr"),
                              returncode=result.returncode, finished_unix=time.time(),
                              load_after=os.getloadavg())
                if result.returncode:
                    raise ValueError(f"{name} exited {result.returncode}")
                record.update(parse_trial(result.stdout, side))
                record["summary"] = summarize(record["samples"])
            except subprocess.TimeoutExpired as error:
                for suffix, raw in (("stdout", error.stdout), ("stderr", error.stderr)):
                    (output / f"{name}.{suffix}").write_bytes(raw or b"")
                record["error"] = "process timeout"
                raise
            except Exception as error:
                record["error"] = str(error)
                raise
            finally:
                save()
    manifest["summary"] = {side: summarize([sample for trial in manifest["trials"]
                                             if trial["side"] == side for sample in trial["samples"]])
                           for side in SIDES}
    save()
    print(json.dumps(manifest["summary"], indent=2))


if __name__ == "__main__":
    main()
