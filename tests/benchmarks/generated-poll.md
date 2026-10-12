# Generated mapped T2 poll comparison

`generated_poll.py` measures a fixed call-free summation loop in two separately
built release binaries. It reports **loop throughput in nanoseconds per
iteration**, including arithmetic and branches. It does not isolate the poll's
instruction cost or establish a rendezvous latency bound.

Build the exact before/after revisions with the same Rust toolchain, target,
features and release profile. Preserve both binaries and build logs. Use private
Cargo targets for distinct worktrees, and coordinate memory-heavy builds per
AGENTS.md. For the initial generated guard change, compare `3e326969` and
`aec3aa68`; later ABI or cadence changes would confound that comparison.

On a quiet host, with a CPU selected from the process's allowed affinity:

```sh
python3 tests/benchmarks/generated_poll.py \
  --before /absolute/path/before-egcl --before-revision 3e326969 \
  --after /absolute/path/after-egcl --after-revision aec3aa68 \
  --build-description 'Rust version; target; release profile/features; build log paths' \
  --cpu 4 --output /tmp/generated-poll-results
```

The output directory must not already exist. The runner pins each process,
forces T2, excludes warmup/publication and printing from its timed regions,
checks the sum, and alternates before/after process order across trials. It
requires the named workload's mapped T2 publication and saves current-code
disassemblies before and after measurement. Inspect those listings to confirm
the selected backward edge uses the expected unconditional veneer or generated
guard; forcing T2 alone does not rule out compiler fallback.

`manifest.json` records binary and workload hashes, caller-supplied revision and
build provenance, platform, CPU, load snapshots, every sample, and per-trial and
pooled p5/median/p95 (linearly interpolated percentiles). Raw stdout, stderr and
T2 logs remain alongside it. Load snapshots are context, not proof of a quiet
host: coordinate with other jobs, and report interference rather than selecting
only favorable trials. The script cannot independently authenticate the supplied
binary-to-revision mapping; retain the build logs.

Inherited `EGCL_*` variables are removed so stress/profiling/tiering flags do not
silently differ. Each process runs through `scripts/egcl-limited.sh` with a 1 GiB
memory cap and 300-second timeout. Host setup variables needed by that script,
such as `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS`, remain inherited.

Parser checks:

```sh
python3 -m unittest discover -s tests/benchmarks -p test_generated_poll.py
```
