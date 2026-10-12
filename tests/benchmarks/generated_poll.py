#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Compare matched release binaries; report loop throughput, not isolated poll cost."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time


def parse_trial(stdout, log, iterations, samples):
    if not re.search(r"POLL-GUARD-BENCH: optimized worker result installed with mapped T2 ABI", log):
        raise ValueError("named workload lacks mapped T2 publication")
    listings = re.findall(r"POLL-DISASSEMBLY-BEGIN\s*(.*?)POLL-DISASSEMBLY-END", stdout, re.S)
    if len(listings) != 2 or any("POLL-GUARD-BENCH" not in listing or
                                "; T2 —" not in listing for listing in listings):
        raise ValueError("both current-code disassemblies must identify workload T2")
    rows = re.findall(r"^POLL-SAMPLE (\d+) (\d+) (\d+)\s*$", stdout, re.M)
    if len(rows) != samples:
        raise ValueError("incomplete sample set")
    expected = iterations * (iterations - 1) // 2
    elapsed = []
    for index, (sample, duration, checksum) in enumerate(rows):
        if int(sample) != index or int(checksum) != expected or int(duration) <= 0:
            raise ValueError("invalid sample order, duration, or checksum")
        elapsed.append(int(duration))
    return elapsed


def summarize(samples, iterations):
    ordered = sorted(value / iterations for value in samples)

    def percentile(fraction):
        position = (len(ordered) - 1) * fraction
        lower = int(position)
        upper = min(lower + 1, len(ordered) - 1)
        return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)

    return {"sample_count": len(samples),
            "p05_ns_per_iteration": percentile(.05),
            "median_ns_per_iteration": statistics.median(ordered),
            "p95_ns_per_iteration": percentile(.95)}


def workload(iterations, samples):
    listing = """(format t "POLL-DISASSEMBLY-BEGIN~%")
(disassemble 'poll-guard-bench)
(format t "POLL-DISASSEMBLY-END~%")"""
    return f"""(defun poll-guard-bench (n)
  (declare (type fixnum n))
  (let ((sum 0))
    (declare (type fixnum sum))
    (dotimes (i n sum) (setq sum (+ sum i)))))
(dotimes (warm 3) (poll-guard-bench {iterations}))
{listing}
(dotimes (sample {samples})
  (let* ((start (egcl-ext:real-time-nanoseconds))
         (checksum (poll-guard-bench {iterations}))
         (elapsed (- (egcl-ext:real-time-nanoseconds) start)))
    (format t "POLL-SAMPLE ~D ~D ~D~%" sample elapsed checksum)))
{listing}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for side in ("before", "after"):
        parser.add_argument(f"--{side}", type=Path, required=True)
        parser.add_argument(f"--{side}-revision", required=True)
    parser.add_argument("--build-description", required=True,
                        help="matching toolchain, profile, features, and build log paths")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=10000000)
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--cpu", type=int, required=True)
    args = parser.parse_args()
    if min(args.iterations, args.samples, args.trials) < 1 or args.iterations > 100000000:
        parser.error("positive counts required; iterations limited to 100000000 for fixnum sums")
    if args.cpu not in os.sched_getaffinity(0):
        parser.error("CPU is outside this process's allowed affinity")
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    source = output / "workload.lisp"
    source.write_text(workload(args.iterations, args.samples))
    root = Path(__file__).resolve().parents[2]
    manifest = {"build_description": args.build_description,
                "platform": platform.platform(), "cpu": args.cpu,
                "iterations": args.iterations, "samples_per_trial": args.samples,
                "workload_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                "metric": "loop ns/iteration including arithmetic and branches; not isolated poll cost",
                "binaries": {}, "trials": []}
    for side in ("before", "after"):
        binary = getattr(args, side).resolve()
        manifest["binaries"][side] = {"path": str(binary),
            "revision": getattr(args, f"{side}_revision"),
            "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    # Remove inherited tuning/stress flags so the two sides share one explicit setup.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("EGCL_")}
    env.update(EGCL_FORCE_TIER="t2", EGCL_MEM_MAX="1G", EGCL_TIMEOUT="300")
    for trial in range(args.trials):
        for side in (("before", "after") if trial % 2 == 0 else ("after", "before")):
            name = f"{side}-{trial}"
            log = output / f"{name}.t2.log"
            env["EGCL_T2_LOG"] = str(log)
            command = [str(root / "scripts/egcl-limited.sh"), "taskset", "-c", str(args.cpu),
                       manifest["binaries"][side]["path"], "--no-init", "--load", str(source)]
            record = {"side": side, "trial": trial, "started_unix": time.time(),
                      "load_before": os.getloadavg(), "command": command}
            result = subprocess.run(command, env=env, cwd=root, capture_output=True, text=True)
            (output / f"{name}.stdout").write_text(result.stdout)
            (output / f"{name}.stderr").write_text(result.stderr)
            record.update(returncode=result.returncode, load_after=os.getloadavg())
            manifest["trials"].append(record)
            try:
                if result.returncode:
                    raise ValueError(f"process exited {result.returncode}")
                record["elapsed_ns"] = parse_trial(result.stdout, log.read_text(),
                                                    args.iterations, args.samples)
                record["summary"] = summarize(record["elapsed_ns"], args.iterations)
            finally:
                (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    manifest["summary"] = {
        side: summarize([duration for record in manifest["trials"] if record["side"] == side
                         for duration in record["elapsed_ns"]], args.iterations)
        for side in ("before", "after")}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest["summary"], indent=2))
    print("Inspect saved before/after disassemblies for the actual loop poll sequence.")


if __name__ == "__main__":
    main()
