#!/bin/bash
# Instructions retired for one bliss-cli run — the reliable perf metric on this box.
#
# Wall clock here is unusable below ~10%: the same binary on the same workload
# has measured 3.5s and 6.9s within one session as background load moved, which
# produced confidently wrong A/B verdicts in both directions. Instruction counts
# are load-independent and repeat to within 0.1%.
#
# taskset is required: this is a hybrid CPU, and perf otherwise reports separate
# cpu_atom/cpu_core counters. Pinning to core 0 (a P-core) keeps it to one.
#
# Usage:  scripts/icount.sh <binary> <workload.lisp> [extra args...]
#   A/B:  compare two saved binaries on the same workload and report the ratio.
#   Cost isolation: difference two workloads that differ by one operation.
set -u
bin=$1; workload=$2; shift 2
taskset -c 0 perf stat -e instructions:u -x, "$bin" "$@" --load "$workload" 2>&1 >/dev/null \
  | awk -F, '/instructions/ && $1 ~ /^[0-9]+$/ {s+=$1} END {print s}'
