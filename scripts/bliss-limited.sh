#!/usr/bin/env bash
# scripts/bliss-limited.sh — run a command under a hard memory cap + timeout.
#
# Runaway bliss runs (ASDF recursion-to-OOM bugs like bliss-hlsa, hot loops)
# have driven this machine deep into swap and triggered the global OOM killer.
# Wrap EVERY ad-hoc bliss-cli invocation (and memory-hungry test runs) in this
# script so the kernel kills the bliss process group the moment it exceeds the
# cap — and never touches swap:
#
#   scripts/bliss-limited.sh target/x86_64-unknown-linux-musl/debug/bliss-cli \
#       --no-init --eval '(form)'
#   BLISS_MEM_MAX=8G BLISS_TIMEOUT=1200 scripts/bliss-limited.sh cargo test ...
#
# Distinguishing a cap kill from a real bug (IMPORTANT — do not misread):
#   exit 137 (SIGKILL) + the [bliss-limited] note  => memory cap hit. NOT a
#       GC bug, NOT memory corruption. If the workload is legitimately large,
#       rerun with a bigger BLISS_MEM_MAX. (dmesg/journal shows
#       "Memory cgroup out of memory" for the scope if you want proof.)
#   exit 124                                       => wall-clock timeout (hang).
#   exit 139 (SIGSEGV) / 134 (SIGABRT) / "already borrowed" panic => a real
#       bug (GC rooting/borrow invariants). The cap CANNOT cause these: it is
#       a cgroup MemoryMax with MemorySwapMax=0, so the kernel OOM-kills the
#       scope outright — it never manifests as a failed malloc, and therefore
#       never masquerades as an allocation-failure abort inside bliss.
#
# Defaults: 4G / 600s (the box has 30G but is usually loaded; ASDF loads fit
# comfortably in 4G). Override per-run with BLISS_MEM_MAX / BLISS_TIMEOUT.

set -u
MEM="${BLISS_MEM_MAX:-4G}"
TIMEOUT="${BLISS_TIMEOUT:-600}"

# --kill-after: T2 native loops can ignore SIGTERM (bliss-siv7), so follow the
# TERM with a KILL or a hung run escapes the timeout entirely. A KILLed timeout
# exits 137 like an OOM kill; disambiguate by elapsed wall-clock below.
start=$SECONDS
systemd-run --user --scope --quiet -p MemoryMax="$MEM" -p MemorySwapMax=0 \
  -- timeout --kill-after=30 "$TIMEOUT" "$@"
rc=$?
elapsed=$((SECONDS - start))
if [ "$rc" -eq 137 ]; then
  if [ "$elapsed" -ge "$TIMEOUT" ]; then
    echo "[bliss-limited] exit 137 after ${elapsed}s >= ${TIMEOUT}s timeout:" \
      "hung run ignored SIGTERM (cf. bliss-siv7) and was KILLed — a hang," \
      "NOT a memory-cap kill, NOT a GC bug." >&2
  else
    echo "[bliss-limited] exit 137 (SIGKILL) after ${elapsed}s: memory cap" \
      "MemoryMax=$MEM hit — cgroup OOM kill, NOT a GC/memory-corruption bug." >&2
  fi
elif [ "$rc" -eq 124 ]; then
  echo "[bliss-limited] exit 124: wall-clock timeout (${TIMEOUT}s) — hang or" \
    "runaway loop, NOT a memory-cap kill." >&2
fi
exit "$rc"
