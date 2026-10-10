#!/usr/bin/env python3
"""Build once, discover libtest cases, and run duration-balanced artifact shards."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import time

NATIVE = "cli::bytecode::native_transfer_tests::"
HERE = Path(__file__).resolve().parent
NESTED_REGRESSIONS = {
    "EGCL_CI_TEST_EGCL_RT_SPEC_MEMORY_GC": "egcl-rt:test:spec_memory_gc",
    "EGCL_CI_TEST_EGCL_STDLIB_SPEC_PACKAGES_BOOTSTRAP": "egcl-stdlib:test:spec_packages_bootstrap",
    "EGCL_CI_TEST_EGCL_RT_SPEC_IMAGE_OPS": "egcl-rt:test:spec_image_ops",
    "EGCL_CI_TEST_EGCL_COMPILER_LIB": "egcl-compiler:lib:egcl_compiler",
}
NESTED_ORCHESTRATOR = "egcl:test:spec_validation_infra::stress_and_regression_scenarios_run_via_real_test_binaries"


def balance(cases, count):
    if not cases or count < 1 or count > len(cases):
        raise ValueError("zero discovered tests or more shards than tests")
    shards, totals = [[] for _ in range(count)], [0.0] * count
    for case in sorted(cases, key=lambda c: (-c["seconds"], c["id"])):
        index = min(range(count), key=lambda i: (totals[i], i))
        shards[index].append(case)
        totals[index] += case["seconds"]
    return shards


def checked(command, cwd):
    return subprocess.check_output(command, cwd=cwd, text=True)


def identity(root):
    revision = checked(["git", "rev-parse", "HEAD"], root).strip()
    tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root).split(b"\0")
    digest = hashlib.sha256()
    for raw in sorted(name for name in tracked if name):
        path = root / os.fsdecode(raw)
        digest.update(raw + b"\0")
        digest.update(os.readlink(path).encode() if path.is_symlink() else path.read_bytes())
    return {"root": str(root), "revision": revision, "source_digest": digest.hexdigest()}


def fingerprint(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def target_kind(target):
    for kind in ("test", "lib", "bin", "proc-macro", "rlib"):
        if kind in target["kind"]:
            return "lib" if kind in ("proc-macro", "rlib") else kind
    return None


def relative(root, path):
    try:
        return str(Path(path).resolve().relative_to(root))
    except ValueError as error:
        raise ValueError(f"artifact outside checkout: {path}") from error


def discover(root, harness, native):
    executable = root / harness["executable"]
    if not executable.is_file():
        raise FileNotFoundError(f"missing test artifact: {executable}")
    def names(extra):
        output = checked([str(executable), "--list", "--format", "terse", *extra], root / harness["cwd"])
        return {line[:-6] for line in output.splitlines() if line.endswith(": test")}
    all_names, ignored = names([]), names(["--ignored"])
    if not ignored <= all_names:
        raise ValueError(f"inconsistent discovery: {harness['name']}")
    return [{**harness, "id": harness["name"] + "::" + name, "test": name,
             "ignored": name in ignored}
            for name in sorted(all_names)
            if name not in ignored or (native and name.startswith(NATIVE))]


def validate_expected(expected, harnesses):
    missing = set(expected) - {h["name"] for h in harnesses}
    if missing:
        raise ValueError(f"missing expected artifacts: {sorted(missing)}")


def inventory(root, args):
    metadata = json.loads(checked(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], root))
    members = set(metadata["workspace_members"])
    packages = {p["id"]: p for p in metadata["packages"] if p["id"] in members}
    targets = {}
    for package in packages.values():
        for target in package["targets"]:
            kind = target_kind(target)
            if kind and target.get("test", True):
                key = f"{package['name']}:{kind}:{target['name']}"
                targets[key] = (package, target)
    configuration = json.loads((HERE / "ci-test-suites.json").read_text())
    selected = {name: [""] for name in targets} if args.suite == "full" else dict(configuration["fast"])
    for area in args.area:
        for name, prefixes in configuration["areas"][area].items():
            selected[name] = sorted(set(selected.get(name, []) + prefixes))
    missing = set(selected) - targets.keys()
    if missing:
        raise ValueError(f"configured targets missing from Cargo metadata: {sorted(missing)}")
    return packages, targets, selected


def prepare(args):
    root = Path(args.root).resolve()
    provenance = identity(root)
    packages, targets, selected = inventory(root, args)
    artifact_path = Path(args.artifacts)
    if args.command == "build":
        command = ["cargo", "test", "--locked", "--no-run", "--message-format=json", "--profile", args.profile, "--target", args.target]
        if args.features:
            command += ["--features", args.features]
        if args.suite == "full":
            command += ["--workspace", "--lib", "--tests"]
        else:
            for package in sorted({targets[name][0]["name"] for name in selected}):
                command += ["-p", package]
            if any(name.split(":")[1] == "lib" for name in selected):
                command.append("--lib")
            for target in sorted({name.split(":")[2] for name in selected if name.split(":")[1] == "test"}):
                command += ["--test", target]
        artifact_path.parent.mkdir(parents=True, exist_ok=True)
        print("Building:", " ".join(command), flush=True)
        with artifact_path.open("w") as output:
            subprocess.run(command, cwd=root, stdout=output, check=True)
    harnesses = {}
    runtime_artifacts = set()
    for line in artifact_path.read_text().splitlines():
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if message["package_id"] not in packages:
            continue
        executable = relative(root, message["executable"])
        runtime_artifacts.add(executable)
        if not message.get("profile", {}).get("test"):
            continue
        target = message["target"]
        kind = target_kind(target)
        package = packages[message["package_id"]]
        name = f"{package['name']}:{kind}:{target['name']}"
        if name in selected:
            harnesses[name] = {"name": name, "executable": executable,
                               "cwd": relative(root, Path(package["manifest_path"]).parent)}
    validate_expected(selected, harnesses.values())
    for path in runtime_artifacts:
        if not (root / path).is_file():
            raise ValueError(f"missing runtime artifact: {path}")
    native = args.target.startswith("x86_64-") and "linux" in args.target
    timings = json.loads(Path(args.timings).read_text())
    cases = []
    for name, harness in sorted(harnesses.items()):
        found = discover(root, harness, native)
        selected_cases = [case for case in found if any(case["test"].startswith(prefix) for prefix in selected[name])]
        if not selected_cases:
            if selected[name] != [""]:
                raise ValueError(f"zero selected tests: {name}")
            print(f"Empty harness: {name}", flush=True)
            continue
        total = timings.get("harness_seconds", {}).get(name.split(":")[-1], len(selected_cases))
        for case in selected_cases:
            case["seconds"] = float(timings.get("tests", {}).get(case["id"], total / len(selected_cases)))
        cases.extend(selected_cases)
    if native:
        count = sum(case["test"].startswith(NATIVE) for case in cases)
        if count < 73:
            raise ValueError(f"native platform gate missing cases: discovered {count}, expected at least 73")
    manifest = {"version": 1, **provenance, "suite": args.suite, "target": args.target,
                "profile": args.profile, "features": args.features,
                "artifacts": {path: fingerprint(root / path) for path in sorted(runtime_artifacts)},
                "shards": balance(cases, args.shards)}
    if identity(root) != provenance:
        raise ValueError("source changed while building/discovering tests")
    path = Path(args.manifest)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Discovered {len(cases)} tests in {len(harnesses)} harnesses; shard estimates: " +
          ", ".join(f"{sum(c['seconds'] for c in shard):.1f}s" for shard in manifest["shards"]))


def dynamic_elf(path):
    """Only dynamically linked ELF executables need ldd; static musl does not."""
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) < 64 or header[:4] != b"\x7fELF":
            return False
        endian = "<" if header[5] == 1 else ">"
        if header[4] == 2:
            offset = struct.unpack_from(endian + "Q", header, 32)[0]
            size, count = struct.unpack_from(endian + "HH", header, 54)
        else:
            offset = struct.unpack_from(endian + "I", header, 28)[0]
            size, count = struct.unpack_from(endian + "HH", header, 42)
        for index in range(count):
            stream.seek(offset + index * size)
            if struct.unpack(endian + "I", stream.read(4))[0] == 3:  # PT_INTERP
                return True
    return False


def archive_files(root, manifest_path):
    """Return only runtime files, not Cargo's compile/incremental cache."""
    manifest = json.loads(manifest_path.read_text())
    artifacts = manifest["artifacts"]
    if not artifacts:
        raise ValueError("zero artifacts to archive")
    libraries = set()
    for name, expected in list(artifacts.items()):
        path = root / name
        relative(root, path)
        if not path.is_file():
            raise ValueError(f"missing expected artifact: {name}")
        if fingerprint(path) != expected:
            raise ValueError(f"artifact changed since discovery: {name}")
        if "linux" not in manifest["target"] or not dynamic_elf(path):
            continue
        output = checked(["ldd", str(path)], root)
        if "not found" in output:
            raise ValueError(f"unresolved shared library for {name}: {output}")
        for line in output.splitlines():
            dependency = line.strip().split(" => ")[-1].rsplit(" (", 1)[0]
            dependency = Path(dependency)
            if not dependency.is_absolute():
                continue
            try:
                alias = str(dependency.relative_to(root))
                resolved = relative(root, dependency)
            except ValueError:
                continue  # System/toolchain libraries come from the pinned runner.
            for name in (alias, resolved):
                artifacts[name] = fingerprint(root / name)
                libraries.add(str(Path(name).parent))
    manifest["library_dirs"] = sorted(libraries)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    return sorted(set(artifacts) | {relative(root, manifest_path)})


def test_environment(root, manifest):
    executables = {}
    for shard in manifest["shards"]:
        for case in shard:
            previous = executables.setdefault(case["name"], case["executable"])
            if previous != case["executable"]:
                raise ValueError(f"ambiguous harness executable: {case['name']}")
    env = os.environ.copy()
    for variable, harness in NESTED_REGRESSIONS.items():
        env.pop(variable, None)
        if harness in executables:
            env[variable] = str(root / executables[harness])
    directories = [str(root / directory) for directory in manifest.get("library_dirs", [])]
    if directories:
        env["LD_LIBRARY_PATH"] = os.pathsep.join(directories + [env.get("LD_LIBRARY_PATH", "")])
    return env


def run_case(root, case, timeout, env=None):
    command = [str(root / case["executable"]), case["test"], "--exact", "--nocapture", "--test-threads=1"]
    if case["ignored"]:
        command.append("--include-ignored")
    started = time.monotonic()
    command += ["--color", "never"]
    child = subprocess.Popen(command, cwd=root / case["cwd"], text=True,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             start_new_session=os.name != "nt", env=env)
    try:
        output, _ = child.communicate(timeout=timeout)
        code = child.returncode
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(child.pid)], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        else:
            os.killpg(child.pid, signal.SIGKILL)
        output, _ = child.communicate()
        code = 124
    summaries = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", output)
    passed = code == 0 and bool(summaries) and summaries[-1] == ("1", "0", "0")
    return {"id": case["id"], "harness": case["name"], "seconds": time.monotonic() - started,
            "exit_code": code, "passed": passed, "output": output}


def run(args):
    root = Path(args.root).resolve()
    manifest = json.loads(Path(args.manifest).read_text())
    if manifest.get("version") != 1 or any(manifest.get(k) != v for k, v in identity(root).items()):
        raise ValueError("manifest source/root provenance does not match this checkout")
    for artifact in manifest["artifacts"]:
        if not (root / artifact).is_file():
            raise ValueError(f"missing expected artifact: {artifact}")
    if not 0 <= args.shard < len(manifest["shards"]):
        raise ValueError("invalid shard index")
    cases = manifest["shards"][args.shard]
    if not cases:
        raise ValueError("zero tests in selected shard")
    # Check every file exists above, but hash only this shard's harnesses and
    # shared runtime binaries. Do not reread every other shard's large binary.
    all_harnesses = {case["executable"] for shard in manifest["shards"] for case in shard}
    needed = {case["executable"] for case in cases} | (manifest["artifacts"].keys() - all_harnesses)
    environment = test_environment(root, manifest)
    if any(case["id"] == NESTED_ORCHESTRATOR for case in cases):
        for variable in NESTED_REGRESSIONS:
            if variable not in environment:
                raise ValueError(f"missing nested regression artifact: {variable}")
            needed.add(relative(root, environment[variable]))
    for artifact in sorted(needed):
        if fingerprint(root / artifact) != manifest["artifacts"][artifact]:
            raise ValueError(f"artifact changed since discovery: {artifact}")
    results, totals = [], {}
    path = Path(args.results)
    path.parent.mkdir(parents=True, exist_ok=True)
    for case in cases:
        result = run_case(root, case, args.timeout, environment)
        results.append(result)
        totals[result["harness"]] = totals.get(result["harness"], 0) + result["seconds"]
        print(f"{'PASS' if result['passed'] else 'FAIL'} {result['id']} ({result['seconds']:.2f}s)", flush=True)
        if not result["passed"]:
            print(result["output"], flush=True)
        path.write_text(json.dumps({"version": 1, "shard": args.shard, "results": results,
                                    "harness_seconds": totals, "tests": {r["id"]: r["seconds"] for r in results}}, indent=2) + "\n")
    return 0 if all(result["passed"] for result in results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ["build", "plan"]:
        p = sub.add_parser(command)
        p.add_argument("--root", default=".")
        p.add_argument("--suite", choices=["fast", "full"], required=True)
        p.add_argument("--area", action="append", choices=["compiler", "runtime", "reader"], default=[])
        p.add_argument("--target", required=True)
        p.add_argument("--profile", default="release")
        p.add_argument("--features", default="")
        p.add_argument("--shards", type=int, default=4)
        p.add_argument("--manifest", default="target/ci-tests/manifest.json")
        p.add_argument("--artifacts", default="target/ci-tests/cargo-artifacts.jsonl")
        p.add_argument("--timings", default=str(HERE / "ci-test-timings.json"))
    p = sub.add_parser("run")
    p.add_argument("--root", default=".")
    p.add_argument("--manifest", required=True)
    p.add_argument("--shard", type=int, required=True)
    p.add_argument("--results", required=True)
    p.add_argument("--timeout", type=float, default=1200)
    p = sub.add_parser("archive", help="write a NUL-separated runtime file list for tar --null -T")
    p.add_argument("--root", default=".")
    p.add_argument("--manifest", required=True)
    p.add_argument("--output", required=True)
    args = parser.parse_args()
    try:
        if args.command == "run":
            return run(args)
        if args.command == "archive":
            files = archive_files(Path(args.root).resolve(), Path(args.manifest).resolve())
            Path(args.output).write_bytes(b"\0".join(os.fsencode(name) for name in files) + b"\0")
            print(f"Archive includes {len(files)} runtime files; Cargo compile caches excluded")
            return 0
        prepare(args)
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"ci-tests: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
