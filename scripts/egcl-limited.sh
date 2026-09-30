#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# scripts/egcl-limited.sh — run a command under a hard memory cap + timeout.
#
# Runaway egcl runs (ASDF recursion-to-OOM bugs like bliss-hlsa, hot loops)
# have driven this machine deep into swap and triggered the global OOM killer.
# Wrap EVERY ad-hoc egcl invocation (and memory-hungry test runs) in this
# script so the kernel kills the egcl process group the moment it exceeds the
# cap — and never touches swap:
#
#   scripts/egcl-limited.sh target/x86_64-unknown-linux-musl/debug/egcl \
#       --no-init --eval '(form)'
#   EGCL_MEM_MAX=8G EGCL_TIMEOUT=1200 scripts/egcl-limited.sh cargo test ...
#
# Distinguishing a cap kill from a real bug (IMPORTANT — do not misread):
#   exit 137 (SIGKILL) + the [egcl-limited] note  => memory cap hit. NOT a
#       GC bug, NOT memory corruption. If the workload is legitimately large,
#       rerun with a bigger EGCL_MEM_MAX. (dmesg/journal shows
#       "Memory cgroup out of memory" for the scope if you want proof.)
#   exit 124                                       => wall-clock timeout (hang).
#   exit 139 (SIGSEGV) / 134 (SIGABRT) / "already borrowed" panic => a real
#       bug (GC rooting/borrow invariants). The cap CANNOT cause these: it is
#       a cgroup MemoryMax with MemorySwapMax=0, so the kernel OOM-kills the
#       scope outright — it never manifests as a failed malloc, and therefore
#       never masquerades as an allocation-failure abort inside egcl.
#
# Defaults: 4G / 600s (the box has 30G but is usually loaded; ASDF loads fit
# comfortably in 4G). Override per-run with EGCL_MEM_MAX / EGCL_TIMEOUT.

set -u
MEM="${EGCL_MEM_MAX:-4G}"
TIMEOUT="${EGCL_TIMEOUT:-600}"

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
    echo "[egcl-limited] exit 137 after ${elapsed}s >= ${TIMEOUT}s timeout:" \
      "hung run ignored SIGTERM (cf. bliss-siv7) and was KILLed — a hang," \
      "NOT a memory-cap kill, NOT a GC bug." >&2
  else
    echo "[egcl-limited] exit 137 (SIGKILL) after ${elapsed}s: memory cap" \
      "MemoryMax=$MEM hit — cgroup OOM kill, NOT a GC/memory-corruption bug." >&2
  fi
elif [ "$rc" -eq 124 ]; then
  echo "[egcl-limited] exit 124: wall-clock timeout (${TIMEOUT}s) — hang or" \
    "runaway loop, NOT a memory-cap kill." >&2
fi
exit "$rc"
