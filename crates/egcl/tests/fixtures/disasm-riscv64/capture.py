#!/usr/bin/env python3
"""Capture a function's installed native code from DISASSEMBLE's raw-byte
listing into NAME.bin for the objdump oracle."""
import os, re, subprocess, sys
binary, tier, name, program = sys.argv[1:5]
env = dict(os.environ, EGCL_FORCE_TIER=tier, EGCL_LAZY_COMPILE="0", EGCL_T1_T2_BACKEDGE_THRESHOLD="4")
out = subprocess.run([binary, "--no-init", "--eval", program], env=env, capture_output=True, text=True, timeout=600)
if out.returncode:
    raise SystemExit(f"egcl failed:\n{out.stdout}\n{out.stderr}")
data = bytearray()
for line in out.stdout.splitlines():
    m = re.match(r"\s*\+([0-9a-f]+):\s+\.byte\s+(.*)$", line)
    if m:
        assert int(m.group(1), 16) == len(data), line
        data.extend(int(v, 16) for v in m.group(2).split(", "))
assert data, out.stdout
open(f"{name}.bin", "wb").write(data)
print(name, len(data), "bytes")
