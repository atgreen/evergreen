#!/usr/bin/env python3
"""Contracts for artifact reuse and fail-closed libtest sharding."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("ci_tests", Path(__file__).with_name("ci-tests.py"))


class RunnerTests(unittest.TestCase):
    def load(self):
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        return module

    def test_duration_balancing_is_deterministic_and_complete(self):
        runner = self.load()
        cases = [{"id": name, "seconds": seconds} for name, seconds in [("a", 9), ("b", 8), ("c", 2), ("d", 1)]]
        shards = runner.balance(cases, 2)
        self.assertEqual(shards, runner.balance(list(reversed(cases)), 2))
        self.assertEqual([sum(c["seconds"] for c in shard) for shard in shards], [10, 10])
        self.assertEqual(sorted(c["id"] for shard in shards for c in shard), ["a", "b", "c", "d"])
        with self.assertRaises(ValueError):
            runner.balance([], 2)

    def fixture(self, root, mode="pass"):
        executable = root / "fake-test"
        executable.write_text("#!/usr/bin/env python3\nimport sys\n"
            "if '--list' in sys.argv:\n"
            " print('ignored: test' if '--ignored' in sys.argv else 'ordinary: test\\nignored: test')\n"
            "else:\n"
            + (" print('test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'); sys.exit(101)\n" if mode == "fail" else
               " print('test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out')\n" if mode == "zero" else
               " print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out')\n"))
        executable.chmod(0o755)
        return {"name": "fake", "executable": "fake-test", "cwd": "."}

    def test_discovery_excludes_ignored_and_rejects_missing(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            self.assertEqual([c["test"] for c in runner.discover(root, harness, False)], ["ordinary"])
            (root / "fake-test").unlink()
            with self.assertRaises((ValueError, FileNotFoundError)):
                runner.discover(root, harness, False)

    def test_real_process_failure_and_zero_execution_fail_the_shard(self):
        runner = self.load()
        for mode, expected in [("pass", True), ("fail", False), ("zero", False)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                harness = self.fixture(root, mode)
                case = {**harness, "test": "ordinary", "id": "fake::ordinary", "ignored": False}
                result = runner.run_case(root, case, 5)
                self.assertEqual(result["passed"], expected)
                self.assertIn("seconds", result)

    def test_native_ignored_cases_require_explicit_supported_admission(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            executable = root / harness["executable"]
            executable.write_text(executable.read_text().replace("ignored: test", runner.NATIVE + "boundary: test"))
            ordinary = runner.discover(root, harness, False)
            admitted = runner.discover(root, harness, True)
            self.assertEqual(len(ordinary), 1)
            self.assertEqual(len(admitted), 2)
            self.assertTrue(next(case for case in admitted if case["test"].startswith(runner.NATIVE))["ignored"])

    def test_shard_rejects_modified_artifact_and_wrong_provenance(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            case = {**harness, "test": "ordinary", "id": "fake::ordinary", "ignored": False}
            provenance = {"root": str(root), "revision": "source", "source_digest": "digest"}
            manifest = {"version": 1, **provenance, "artifacts": {"fake-test": runner.fingerprint(root / "fake-test")}, "shards": [[case]]}
            path = root / "manifest.json"
            path.write_text(json.dumps(manifest))
            args = SimpleNamespace(root=str(root), manifest=str(path), shard=0, timeout=5, results=str(root / "results.json"))
            with patch.object(runner, "identity", return_value=provenance):
                self.assertEqual(runner.run(args), 0)
                (root / "fake-test").write_text("changed")
                with self.assertRaisesRegex(ValueError, "artifact changed"):
                    runner.run(args)
            with patch.object(runner, "identity", return_value={**provenance, "revision": "different"}):
                with self.assertRaisesRegex(ValueError, "provenance"):
                    runner.run(args)

    def test_archive_omits_build_cache_and_keeps_exact_manifest_artifacts(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            cache = root / "target" / "incremental" / "cache.bin"
            cache.parent.mkdir(parents=True)
            cache.write_bytes(b"not needed at runtime")
            manifest = {"version": 1, "target": "x86_64-unknown-linux-musl",
                        "artifacts": {harness["executable"]: runner.fingerprint(root / harness["executable"])}}
            path = root / "target" / "manifest.json"
            path.write_text(json.dumps(manifest))
            files = runner.archive_files(root, path)
            self.assertEqual(files, ["fake-test", "target/manifest.json"])
            self.assertNotIn("target/incremental/cache.bin", files)
            (root / "fake-test").unlink()
            with self.assertRaisesRegex(ValueError, "missing"):
                runner.archive_files(root, path)

    def test_archive_keeps_local_shared_library_alias_and_runtime_search_path(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            library = root / "target" / "deps" / "libfixture.so.1"
            library.parent.mkdir(parents=True)
            library.write_bytes(b"shared-library-fixture")
            alias = library.with_name("libfixture.so")
            alias.symlink_to(library.name)
            path = root / "target" / "manifest.json"
            path.write_text(json.dumps({"target": "x86_64-unknown-linux-gnu",
                "artifacts": {harness["executable"]: runner.fingerprint(root / harness["executable"])}}))
            with patch.object(runner, "dynamic_elf", return_value=True), patch.object(
                    runner, "checked", return_value=f"libfixture.so => {alias} (0x123)\n"):
                files = runner.archive_files(root, path)
            self.assertIn("target/deps/libfixture.so", files)
            self.assertIn("target/deps/libfixture.so.1", files)
            self.assertEqual(json.loads(path.read_text())["library_dirs"], ["target/deps"])

    def test_nested_regressions_receive_only_unambiguous_manifest_executables(self):
        runner = self.load()
        case = {"name": "egcl-compiler:lib:egcl_compiler", "executable": "target/compiler-test"}
        manifest = {"shards": [[case]], "library_dirs": []}
        root = Path("/checkout")
        env = runner.test_environment(root, manifest)
        self.assertEqual(env["EGCL_CI_TEST_EGCL_COMPILER_LIB"], str(root / case["executable"]))
        manifest["shards"].append([{**case, "executable": "target/other"}])
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            runner.test_environment(root, manifest)

    def test_missing_expected_artifacts_are_fatal(self):
        runner = self.load()
        with self.assertRaisesRegex(ValueError, "missing"):
            runner.validate_expected({"egcl:lib:egcl"}, [])


if __name__ == "__main__":
    unittest.main()
