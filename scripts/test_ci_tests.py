#!/usr/bin/env python3
"""Contracts for artifact reuse and fail-closed libtest sharding."""
import importlib.util
import json
import os
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
        for prefix in (runner.NATIVE, runner.GENERATED_NATIVE):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                harness = self.fixture(root)
                executable = root / harness["executable"]
                executable.write_text(executable.read_text().replace("ignored: test", prefix + "boundary: test"))
                ordinary = runner.discover(root, harness, False)
                admitted = runner.discover(root, harness, True)
                self.assertEqual(len(ordinary), 1)
                self.assertEqual(len(admitted), 2)
                self.assertTrue(next(case for case in admitted if case["test"].startswith(prefix))["ignored"])

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
            with patch.object(runner, "checked", return_value="/toolchain/lib\n"):
                files = runner.archive_files(root, path)
            self.assertEqual(files, ["fake-test", "target/manifest.json"])
            self.assertNotIn("target/incremental/cache.bin", files)
            (root / "fake-test").unlink()
            with patch.object(runner, "checked", return_value="/toolchain/lib\n"), self.assertRaisesRegex(ValueError, "missing"):
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
                    runner, "checked", side_effect=lambda command, cwd, **kwargs:
                        "/toolchain/lib\n" if command[0] == "rustc" else f"libfixture.so => {alias} (0x123)\n"):
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

    def test_binary_inventory_requires_cargo_test_eligibility(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            configuration = {"fast": {"egcl:lib:egcl": ["library::"],
                                      "egcl:bin:egcl": ["host_memory_tests::"]}, "areas": {}}
            (root / "ci-test-suites.json").write_text(json.dumps(configuration))
            binary = {"kind": ["bin"], "name": "egcl", "test": False}
            metadata = {"workspace_members": ["package"], "packages": [{
                "id": "package", "name": "egcl", "manifest_path": str(root / "Cargo.toml"),
                "targets": [{"kind": ["lib"], "name": "egcl", "test": True}, binary,
                            {"kind": ["bin"], "name": "not-tested", "test": False}]}]}
            args = SimpleNamespace(suite="fast", area=[])
            with patch.object(runner, "HERE", root), patch.object(
                    runner, "checked", side_effect=lambda *a, **k: json.dumps(metadata)):
                with self.assertRaisesRegex(ValueError, "egcl:bin:egcl"):
                    runner.inventory(root, args)
                binary["test"] = True
                _, targets, selected = runner.inventory(root, args)
                self.assertEqual(set(targets), {"egcl:lib:egcl", "egcl:bin:egcl"})
                self.assertEqual(selected, configuration["fast"])
                args.suite = "full"
                self.assertEqual(runner.inventory(root, args)[2],
                                 {"egcl:lib:egcl": [""], "egcl:bin:egcl": [""]})

    def test_build_invocation_includes_selected_binary_without_changing_full_flags(self):
        runner = self.load()
        # Intercept prepare's actual subprocess boundary before any build. This
        # checks the command used by CI rather than a separate command replica.
        class BuildIntercept(Exception):
            pass
        for suite, include_binary in (("fast", True), ("fast", False), ("full", True)):
            with self.subTest(suite=suite, binary=include_binary), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                args = SimpleNamespace(root=str(root), command="build", suite=suite,
                    artifacts=str(root / "artifacts.jsonl"), profile="release",
                    target="x86_64-unknown-linux-musl", features="alloc-count")
                selected = {"egcl:lib:egcl": ["library::"],
                            "egcl:test:host_pressure_closure_cli": [""]}
                if include_binary:
                    selected["egcl:bin:egcl"] = ["host_memory_tests::"]
                targets = {name: ({"name": "egcl"}, {}) for name in selected}
                targets["egcl:bin:not-selected"] = ({"name": "egcl"}, {})
                with patch.object(runner, "identity", return_value={}), patch.object(
                        runner, "inventory", return_value=({}, targets, selected)), patch.object(
                        runner.subprocess, "run", side_effect=BuildIntercept) as build:
                    with self.assertRaises(BuildIntercept):
                        runner.prepare(args)
                expected = ["cargo", "test", "--locked", "--no-run", "--message-format=json",
                            "--profile", "release", "--target", args.target,
                            "--features", "alloc-count"]
                if suite == "full":
                    expected += ["--workspace", "--lib", "--tests"]
                else:
                    expected += ["-p", "egcl", "--lib"]
                    if include_binary:
                        expected += ["--bin", "egcl"]
                    expected += ["--test", "host_pressure_closure_cli"]
                build.assert_called_once()
                self.assertEqual(build.call_args.args[0], expected)
                self.assertEqual(build.call_args.kwargs["cwd"], root)
                self.assertTrue(build.call_args.kwargs["check"])

    def test_host_rust_libraries_reach_discovery_and_test_processes(self):
        runner = self.load()
        for target in ("x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                harness = self.fixture(root)
                host = root.parent / "host toolchain" / "lib"
                target_lib = root.parent / "target toolchain" / "lib"
                executable = root / harness["executable"]
                executable.write_text(executable.read_text().replace("import sys\n",
                    "import sys, os\n"
                    + f"if {str(host)!r} not in os.environ.get('LD_LIBRARY_PATH', '').split(os.pathsep):\n"
                    + " print('missing Rust host libstd'); sys.exit(127)\n"))
                case = {**harness, "test": "ordinary", "id": "fake::ordinary", "ignored": False}
                manifest = {"target": target, "artifacts": {"fake-test": runner.fingerprint(executable)},
                            "shards": [[case]], "library_dirs": ["native libs"]}
                original_checked = runner.checked
                def checked(command, cwd, **kwargs):
                    if command[:3] == ["rustc", "--print", "target-libdir"]:
                        return str(target_lib if "--target" in command else host) + "\n"
                    return original_checked(command, cwd, **kwargs)
                with patch.object(runner, "checked", side_effect=checked), patch.dict(
                        os.environ, {"LD_LIBRARY_PATH": "/inherited/library"}):
                    environment = runner.test_environment(root, manifest)
                    result = runner.run_case(root, case, 5, environment)
                    self.assertTrue(result["passed"], result["output"])
                    self.assertEqual([case["test"] for case in runner.discover(
                        root, harness, False, environment)], ["ordinary"])
                search = environment["LD_LIBRARY_PATH"].split(os.pathsep)
                self.assertIn(str(target_lib), search)
                self.assertIn(str(root / "native libs"), search)
                self.assertIn(str(root), search)
                self.assertIn("/inherited/library", search)
                self.assertNotIn("", search)

    def test_archive_resolves_host_rust_libraries_and_shards_relocate_toolchain(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = self.fixture(root)
            host = root.parent / "producer rust" / "lib"
            consumer = root.parent / "consumer rust" / "lib"
            case = {**harness, "test": "ordinary", "id": "fake::ordinary", "ignored": False}
            path = root / "manifest.json"
            path.write_text(json.dumps({"target": "x86_64-unknown-linux-musl",
                "artifacts": {"fake-test": runner.fingerprint(root / "fake-test")}, "shards": [[case]]}))
            def checked(command, cwd, **kwargs):
                if command[:3] == ["rustc", "--print", "target-libdir"]:
                    return str(host) + "\n"
                self.assertEqual(command, ["ldd", str(root / "fake-test")])
                self.assertIn(str(host), kwargs["env"]["LD_LIBRARY_PATH"].split(os.pathsep))
                return f"libstd-fixture.so => {host / 'libstd-fixture.so'} (0x123)\n"
            with patch.object(runner, "dynamic_elf", return_value=True), patch.object(
                    runner, "checked", side_effect=checked):
                self.assertEqual(runner.archive_files(root, path), ["fake-test", "manifest.json"])
            manifest = json.loads(path.read_text())
            self.assertNotIn(str(host), path.read_text(), "producer toolchain paths must not be serialized")
            with patch.object(runner, "checked", return_value=str(consumer) + "\n"):
                environment = runner.test_environment(root, manifest)
            self.assertIn(str(consumer), environment["LD_LIBRARY_PATH"].split(os.pathsep))
            self.assertNotIn(str(host), environment["LD_LIBRARY_PATH"].split(os.pathsep))


if __name__ == "__main__":
    unittest.main()
