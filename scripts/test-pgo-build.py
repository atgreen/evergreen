#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Build-orchestration tests with cheap external-tool substitutes.

These test failure isolation and command contracts, not LLVM or EGCL itself.
A real capped `make image` is the separate end-to-end validation.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FAKE = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
env = os.environ
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(env["PGO_TEST_LOG"], "a") as out:
    out.write(json.dumps({"name": name, "args": args,
        "flags": env.get("CARGO_ENCODED_RUSTFLAGS", ""),
        "target_dir": env.get("CARGO_TARGET_DIR", ""),
        "profile": env.get("LLVM_PROFILE_FILE", ""),
        "phase": env.get("EGCL_PGO_PHASE", "")}) + "\n")
def fail(stage):
    if env.get("PGO_TEST_FAIL") == stage:
        sys.exit(42)
if name == "rustc":
    if args == ["-vV"]:
        print("host: x86_64-unknown-linux-gnu\nLLVM version: 21.1.8")
    else:
        print(env["PGO_TEST_SYSROOT"])
elif name == "llvm-profdata":
    if "--version" in args:
        print("LLVM version " + env.get("PGO_TEST_LLVM", "21.1.8"))
    else:
        fail("merge")
        pathlib.Path(args[args.index("-o") + 1]).write_bytes(b"merged")
elif name == "cargo":
    flags = env["CARGO_ENCODED_RUSTFLAGS"]
    fail("generate" if "profile-generate" in flags else "use")
    if env.get("PGO_TEST_FAIL") == "profile-warning" and "profile-use" in flags:
        print("warning: no profile data available for function fixture")
    target = args[args.index("--target") + 1]
    binary = pathlib.Path(env["CARGO_TARGET_DIR"]) / target / "release/egcl"
    binary.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(__file__, binary)
    binary.chmod(0o755)
else:
    if "--eval" in args:
        fail("verify")
        if env.get("PGO_TEST_FAIL") != "verify-marker":
            print("PGO-IMAGE-OK")
    elif args[-1].endswith("build-image.lisp"):
        fail("image")
        image = pathlib.Path(env["EGCL_IMAGE_OUT"])
        shutil.copyfile(__file__, image)
        image.chmod(0o755)
        if env.get("PGO_TEST_LEFTOVER"):
            (image.parent / "retained-diagnostic").write_text("diagnostic")
    else:
        phase = env["EGCL_PGO_PHASE"]
        fail(phase)
        if env.get("PGO_TEST_FAIL") != "no-profile" and not (
                env.get("PGO_TEST_FAIL") == "missing-runtime-profile" and phase == "runtime"):
            profile = pathlib.Path(env["LLVM_PROFILE_FILE"].replace("%p", str(os.getpid())).replace("%m", "signature"))
            profile.parent.mkdir(parents=True, exist_ok=True)
            profile.write_bytes(b"raw")
        if env.get("PGO_TEST_FAIL") != "training-marker":
            print({"prepare": "PGO-PREPARED 24", "load": "PGO-LOAD 24 300",
                   "runtime": "PGO-RUNTIME 1000 499500"}[phase])
'''


class PgoBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="egcl pgo build ")
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.tools = self.work / "tools"
        self.tools.mkdir()
        for name in ("rustc", "cargo", "llvm-profdata"):
            path = self.tools / name
            path.write_text(FAKE)
            path.chmod(0o755)
        self.image = self.work / "output" / "egcl"
        self.image.parent.mkdir()
        self.image.write_bytes(b"previous image")
        self.log = self.work / "commands.jsonl"
        self.env = os.environ.copy()
        for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "LLVM_PROFILE_FILE",
                     "RUSTC", "CARGO", "EGCL_PGO_TARGET"):
            self.env.pop(name, None)
        self.env.update(
            PATH=str(self.tools) + os.pathsep + os.environ["PATH"],
            LLVM_PROFDATA=str(self.tools / "llvm-profdata"),
            EGCL_PGO_ROOT=str(self.work / "builds"),
            EGCL_IMAGE_OUT=str(self.image),
            PGO_TEST_LOG=str(self.log), PGO_TEST_SYSROOT=str(self.work / "sysroot"),
        )

    def run_build(self, **overrides):
        return subprocess.run(
            ["bash", str(ROOT / "scripts/build-pgo-image.sh")], cwd=ROOT,
            env=dict(self.env, **overrides), text=True, capture_output=True,
        )

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def run_make(self, target, **overrides):
        return subprocess.run(
            ["make", "--no-print-directory", target], cwd=ROOT,
            env=dict(self.env, **overrides), text=True, capture_output=True,
        )

    def test_make_image_uses_pgo_and_honors_output_override(self):
        result = self.run_make("image")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        builds = [c for c in self.commands() if c["name"] == "cargo"]
        self.assertEqual(len(builds), 2)
        self.assertIn("-Cprofile-generate=", builds[0]["flags"])
        self.assertIn("-Cprofile-use=", builds[1]["flags"])
        self.assertNotEqual(self.image.read_bytes(), b"previous image")

    def test_make_image_failure_does_not_fall_back_or_replace_image(self):
        result = self.run_make("image", PGO_TEST_LLVM="23.1.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.image.read_bytes(), b"previous image")
        self.assertFalse(any(c["name"] == "cargo" for c in self.commands()))

    def test_image_aliases_share_one_build(self):
        result = subprocess.run(
            ["make", "--no-print-directory", "-n", "image", "pgo-image"],
            cwd=ROOT, env=self.env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.count("bash scripts/build-pgo-image.sh"), 1)

    def test_explicit_non_pgo_and_install_remain_independent(self):
        for target in ("image-no-pgo", "install"):
            with self.subTest(target=target):
                result = subprocess.run(
                    ["make", "--no-print-directory", "-n", target], cwd=ROOT,
                    env=self.env, text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("build-pgo-image.sh", result.stdout)
                if target == "image-no-pgo":
                    self.assertIn("build --workspace --release", result.stdout)
                    self.assertIn("scripts/build-image.lisp", result.stdout)
                else:
                    self.assertNotIn("cargo build", result.stdout)

    def test_success_uses_only_selected_profiles_and_preserves_base_flags(self):
        base = "-Copt-level=3\x1f--cfg=fixture"
        result = self.run_build(CARGO_ENCODED_RUSTFLAGS=base)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotEqual(self.image.read_bytes(), b"previous image")
        commands = self.commands()
        builds = [c for c in commands if c["name"] == "cargo"]
        self.assertEqual(len(builds), 2)
        self.assertNotEqual(builds[0]["target_dir"], builds[1]["target_dir"])
        for build in builds:
            self.assertTrue(Path(build["target_dir"]).is_absolute())
            self.assertTrue(build["flags"].startswith(base + "\x1f"))
            self.assertIn("--release", build["args"])
            self.assertIn("--locked", build["args"])
        training = [c for c in commands if c["phase"] in ("load", "runtime")]
        self.assertEqual(len(training), 6)
        merge = next(c for c in commands if "merge" in c["args"])
        raw = [Path(arg) for arg in merge["args"] if arg.endswith(".profraw")]
        self.assertEqual(len(raw), 6)
        self.assertTrue(all(path.is_absolute() for path in raw))
        self.assertFalse(any("prepare" in str(path) for path in raw))

    def test_every_failed_phase_preserves_previous_image(self):
        for phase in ("generate", "prepare", "load", "runtime", "merge", "use",
                      "image", "verify", "no-profile", "training-marker", "verify-marker",
                      "missing-runtime-profile", "profile-warning"):
            with self.subTest(phase=phase):
                result = self.run_build(PGO_TEST_FAIL=phase)
                expected = 1 if phase in ("no-profile", "training-marker", "verify-marker",
                                           "missing-runtime-profile", "profile-warning") else 42
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                self.assertEqual(self.image.read_bytes(), b"previous image")

    def test_mismatched_llvm_fails_before_build(self):
        result = self.run_build(PGO_TEST_LLVM="23.1.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("LLVM", result.stderr)
        self.assertFalse(any(c["name"] == "cargo" for c in self.commands()))

    def test_cross_target_fails_before_build(self):
        result = self.run_build(EGCL_PGO_TARGET="aarch64-unknown-linux-gnu")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("target", result.stderr)
        self.assertFalse(any(c["name"] == "cargo" for c in self.commands()))

    def test_each_build_has_fresh_profile_directory(self):
        for _ in range(2):
            result = self.run_build()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        builds = [c for c in self.commands() if c["name"] == "cargo"]
        self.assertEqual(len({c["target_dir"] for c in builds}), 4)

    def test_plain_rustflags_preserves_all_whitespace_separated_flags(self):
        result = self.run_build(RUSTFLAGS="-Copt-level=3\n--cfg=fixture")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        builds = [c for c in self.commands() if c["name"] == "cargo"]
        for build in builds:
            self.assertTrue(build["flags"].startswith("-Copt-level=3\x1f--cfg=fixture\x1f"))

    def test_discovers_rust_toolchain_profdata_before_path_tool(self):
        self.env.pop("LLVM_PROFDATA")
        bundled = self.work / "sysroot/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata"
        bundled.parent.mkdir(parents=True)
        bundled.write_text(FAKE)
        bundled.chmod(0o755)
        (self.tools / "llvm-profdata").unlink()
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_cleanup_does_not_report_failure_after_publishing_valid_image(self):
        result = self.run_build(PGO_TEST_LEFTOVER="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotEqual(self.image.read_bytes(), b"previous image")
        self.assertEqual(len(list(self.image.parent.glob(".egcl-pgo.*/retained-diagnostic"))), 1)


if __name__ == "__main__":
    unittest.main()
