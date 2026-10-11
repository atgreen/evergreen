#!/usr/bin/env python3
"""Integration checks for the real user-systemd memory/timeout wrapper."""
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

WRAPPER = Path(__file__).with_name("egcl-limited.sh")


class LimitedCommandTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if sys.platform != "linux" or not shutil.which("systemctl") or not hasattr(os, "pidfd_open"):
            raise unittest.SkipTest("Linux systemd and pidfd support are required")
        if subprocess.run(["systemctl", "--user", "show-environment"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            raise unittest.SkipTest("a running systemd user manager is required")

    def environment(self, timeout):
        return dict(os.environ, EGCL_MEM_MAX="64M", EGCL_TIMEOUT=str(timeout))

    def test_preserves_stdin_output_and_exit_status(self):
        for status in (7, 134, 137, 139):
            with self.subTest(status=status):
                result = subprocess.run(
                    [str(WRAPPER), sys.executable, "-c",
                     f"import sys; print(sys.stdin.read(), end=''); sys.exit({status})"],
                    input="through the scope\n", text=True, capture_output=True,
                    env=self.environment(10), timeout=15)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual(result.stdout, "through the scope\n")
                self.assertNotIn("scope cleanup failed", result.stderr)
                if status == 137:
                    self.assertIn("memory cap", result.stderr)
                else:
                    self.assertNotIn("memory cap", result.stderr)

    def test_timeout_stops_detached_term_ignoring_descendant(self):
        with tempfile.TemporaryDirectory(prefix="egcl-limited-test-") as directory:
            root = Path(directory)
            child = root / "child.py"
            ready = root / "ready.json"
            child.write_text('''import json, os, pathlib, signal, sys, time
os.setsid()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
cgroup = next(line.split(":", 2)[2] for line in pathlib.Path("/proc/self/cgroup").read_text().splitlines() if line.startswith("0::"))
base = pathlib.Path("/sys/fs/cgroup") / cgroup.lstrip("/")
ready = pathlib.Path(sys.argv[1])
staged = ready.with_suffix(".tmp")
staged.write_text(json.dumps({"pid": os.getpid(), "memory": (base / "memory.max").read_text().strip(), "swap": (base / "memory.swap.max").read_text().strip()}))
staged.replace(ready)
time.sleep(30)
''')
            parent = root / "parent.py"
            parent.write_text('''import subprocess, sys, time
subprocess.Popen([sys.executable, sys.argv[1], sys.argv[2]], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(30)
''')
            process = subprocess.Popen(
                [str(WRAPPER), sys.executable, str(parent), str(child), str(ready)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                env=self.environment(2))
            pidfd = None
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(ready.exists(), "descendant did not start")
                info = json.loads(ready.read_text())
                pidfd = os.pidfd_open(info["pid"])
                self.assertEqual(info["memory"], str(64 * 1024 * 1024))
                self.assertEqual(info["swap"], "0")
                _, stderr = process.communicate(timeout=8)
                self.assertEqual(process.returncode, 124, stderr)
                self.assertIn("wall-clock timeout", stderr)
                # A zombie has exited but may await the host's reaper. Polling
                # a pidfd observes exit without mistaking a reused PID for it.
                import select
                poll = select.poll()
                poll.register(pidfd, select.POLLIN)
                self.assertTrue(poll.poll(1000), "timeout left its descendant running")
            finally:
                if pidfd is not None:
                    try:
                        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    os.close(pidfd)
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
