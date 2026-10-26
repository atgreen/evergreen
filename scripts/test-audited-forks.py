#!/usr/bin/env python3
"""Fetch pinned compatibility forks and run their committed semantic smoke tests."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parent.parent
SCENARIO = REPO / "tests/audited-forks"


def run(command, work, env, log):
    with log.open("w") as output:
        result = subprocess.run(command, cwd=work, env=env, stdout=output,
                                stderr=subprocess.STDOUT)
    if result.returncode:
        raise SystemExit(f"Exit {result.returncode}: {' '.join(command)}\nSee {log}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", nargs="+", help="Run only these fork names")
    parser.add_argument("--work", type=Path, help="Reuse a retained scenario directory")
    parser.add_argument("--prepare-unicode", action="store_true",
                        help="Generate portable CL-Unicode source data with SBCL before EGCL checks")
    args = parser.parse_args()
    forks = json.loads((SCENARIO / "forks.json").read_text())
    # CL-INTERPOL's cold CL-UNICODE data generation is slow; expose other
    # compatibility failures before waiting for that independent dependency.
    forks.sort(key=lambda entry: entry["name"] == "cl-interpol")
    if args.only:
        unknown = set(args.only) - {entry["name"] for entry in forks}
        if unknown:
            parser.error(f"Unknown forks: {sorted(unknown)}")
    work = args.work.resolve() if args.work else Path(tempfile.mkdtemp(prefix="egcl-audited-forks."))
    work.mkdir(parents=True, exist_ok=True)
    runs = work / "runs"
    runs.mkdir(exist_ok=True)
    logs = Path(tempfile.mkdtemp(prefix="run-", dir=runs))
    print(f"Audited fork artifacts: {work}; run logs: {logs}", flush=True)
    pin = SCENARIO / "ocicl.csv"
    shutil.copyfile(pin, work / "ocicl.csv")
    env = os.environ.copy()
    env.update(EGCL_MEM_MAX=env.get("EGCL_MEM_MAX", "2G"),
               EGCL_TIMEOUT=env.get("EGCL_TIMEOUT", "600"), OCICL_LOCAL_ONLY="1")
    ocicl = shutil.which(env.get("OCICL_BIN", "ocicl"))
    egcl = Path(env.get("EGCL_BIN", str(REPO / "target/egcl"))).resolve()
    runtime = Path(env.get("OCICL_RUNTIME", str(Path(env.get("XDG_DATA_HOME", str(Path.home() / ".local/share"))) / "ocicl/ocicl-runtime.lisp"))).resolve()
    if not ocicl or not egcl.is_file() or not runtime.is_file():
        parser.error("Set OCICL_BIN, EGCL_BIN and OCICL_RUNTIME to existing executables/runtime")
    with egcl.open("rb") as artifact:
        digest = hashlib.file_digest(artifact, "sha256").hexdigest()
    (logs / "runtime.json").write_text(json.dumps(
        {"executable": str(egcl), "sha256": digest}, indent=2) + "\n")
    print(f"EGCL artifact SHA-256: {digest}", flush=True)
    limited = str(REPO / "scripts/egcl-limited.sh")
    run([limited, ocicl, "install"], work, env, logs / "install.log")
    if pin.read_bytes() != (work / "ocicl.csv").read_bytes():
        raise SystemExit(f"ocicl changed pinned revisions; inspect {work / 'ocicl.csv'}")
    with pin.open(newline="") as source:
        rows = {row[0].strip(): [field.strip() for field in row] for row in csv.reader(source)}
    directories = {}
    system_files = {}
    for entry in forks:
        row = rows[entry["system"]]
        expected = "git+" + entry["fork"] + "@" + entry["sha"]
        if row[1].split("#", 1)[0].removesuffix(".git") != expected:
            raise SystemExit(f"Unexpected source for {entry['name']}: {row[1]}")
        directories[entry["name"]] = work / "ocicl" / row[2].split("/", 1)[0]
        # Older ocicl versions use systems/ rather than ocicl/.
        if not directories[entry["name"]].is_dir():
            directories[entry["name"]] = work / "systems" / row[2].split("/", 1)[0]
        system_files[entry["name"]] = directories[entry["name"]].parent / row[2]
        for check in entry["checks"]:
            if not (directories[entry["name"]] / check).is_file():
                raise SystemExit(f"Missing pinned check: {entry['name']}/{check}")
    # Tests that reset ASDF configuration inherit only these pinned directories.
    ordered = sorted(directories, key=lambda name: (name != "named-readtables", name))
    registry = "(:source-registry " + " ".join(
        "(:directory " + json.dumps(str(directories[name]) + "/") + ")" for name in ordered
    ) + " :ignore-inherited-configuration)"
    env.update(CL_SOURCE_REGISTRY=registry, EGCL_PORT_RUNTIME=str(runtime),
               EGCL_PORT_CACHE=str(work / "cache" / digest[:16]) + "/")
    setup = logs / "setup.lisp"
    setup.write_text('''(require :asdf)
(asdf:initialize-source-registry (read-from-string (uiop:getenv "CL_SOURCE_REGISTRY")))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE")) :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*local-only* t ocicl-runtime:*download* nil)
''')
    if args.prepare_unicode:
        sbcl = shutil.which(env.get("SBCL_BIN", "sbcl"))
        if not sbcl:
            parser.error("--prepare-unicode requires SBCL_BIN or sbcl on PATH")
        unicode_asd = work / "ocicl" / rows["cl-unicode"][2]
        if not unicode_asd.is_file():
            unicode_asd = work / "systems" / rows["cl-unicode"][2]
        prepare = logs / "prepare-unicode.lisp"
        prepare.write_text("(load " + json.dumps(str(setup)) + ")\n"
                           + '(assert (equal (truename '
                           + json.dumps(str(unicode_asd))
                           + ') (truename (asdf:system-source-file (asdf:find-system "cl-unicode")))))\n'
                           + '(asdf:load-system "cl-unicode/build")\n')
        prepare_env = env.copy()
        prepare_env["EGCL_PORT_CACHE"] = str(work / "sbcl-data-cache") + "/"
        run([limited, sbcl, "--noinform", "--disable-debugger", "--script", str(prepare)],
            work, prepare_env, logs / "prepare-unicode.log")
        generated = {}
        for name in ("lists.lisp", "hash-tables.lisp", "methods.lisp"):
            source = unicode_asd.parent / name
            generated[name] = hashlib.sha256(source.read_bytes()).hexdigest()
        (logs / "prepared-unicode.json").write_text(json.dumps(generated, indent=2) + "\n")
        print("Prepared Unicode source data with SBCL; EGCL checks use a separate FASL cache", flush=True)
    passed = 0
    for entry in forks:
        if args.only and entry["name"] not in args.only:
            continue
        for index, check in enumerate(entry["checks"]):
            log = logs / f"{entry['name']}-{index + 1}.log"
            provenance = ('(assert (equal (truename ' + json.dumps(str(system_files[entry["name"]]))
                          + ') (truename (asdf:system-source-file (asdf:find-system '
                          + json.dumps(entry["system"]) + ')))))')
            driver = logs / f"{entry['name']}-{index + 1}-driver.lisp"
            driver.write_text("(load " + json.dumps(str(setup)) + ")\n"
                              + provenance + "\n(load "
                              + json.dumps(str(directories[entry["name"]] / check))
                              + ")\n" + provenance
                              + '\n(format t "AUDITED-FORK-CHECK-PASS~%")\n')
            command = [limited, str(egcl), "--no-init", "--load", str(driver)]
            run(command, work, env, log)
            output = log.read_text()
            if "; caught ERROR:" in output or "COMPILE-FAILED-WARNING" in output:
                raise SystemExit(f"Compiler reported a failure despite process success: {log}")
            if "AUDITED-FORK-CHECK-PASS" not in output:
                raise SystemExit(f"Check exited before completion: {log}")
            passed += 1
            print(f"PASS {entry['name']}: {check}", flush=True)
    print(f"AUDITED-FORKS-PASS: {passed} checks; run logs in {logs}")


if __name__ == "__main__":
    main()
