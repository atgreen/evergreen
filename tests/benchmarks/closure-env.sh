#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#
# Closure / captured-environment microbenchmarks per execution tier
# (Backpack stage 0, bliss-gg3i1.7.15.1).
#
#   tests/benchmarks/closure-env.sh                 # every workload, every tier
#   TIERS="t0 t2" tests/benchmarks/closure-env.sh counter deep-read
#
# Each tier runs in its own process with EGCL_FORCE_TIER pinned (interp, t0,
# t1, t2; "default" leaves normal tiering on). The workload file reports
# ns/op per workload; this script prints the minimum of N runs (default 5),
# since wall clock on a shared host drifts. Use an LTO release binary for
# numbers you intend to compare, and record its revision.
#
# Env overrides: EGCL (binary), N (runs), TIERS, MIN_MS (per-batch floor).
set -u

cd "$(dirname "$0")/../.." || exit 1

EGCL=${EGCL:-target/x86_64-unknown-linux-musl/release/egcl}
N=${N:-5}
TIERS=${TIERS:-"t0 t1 t2 default"}
MIN_MS=${MIN_MS:-300}
WORKLOAD=tests/benchmarks/closure-env-workload.lisp

if [ ! -x "$EGCL" ]; then
    echo "error: egcl binary not found at $EGCL" >&2
    exit 1
fi

names=""
for name in "$@"; do
    names="$names \"$name\""
done
form="(progn (load \"$WORKLOAD\") (setq *closure-env-min-ms* $MIN_MS) (closure-env-main (quote ($names))))"

declare -A best
order=()
for tier in $TIERS; do
    for _ in $(seq "$N"); do
        if [ "$tier" = default ]; then
            out=$(env -u EGCL_FORCE_TIER "$EGCL" --no-init --eval "$form" 2>&1)
        else
            out=$(EGCL_FORCE_TIER=$tier "$EGCL" --no-init --eval "$form" 2>&1)
        fi
        while read -r _ name ns _; do
            key="$tier/$name"
            if [ -z "${best[$key]+x}" ]; then
                order+=("$key")
                best[$key]=$ns
            elif awk "BEGIN {exit !($ns < ${best[$key]})}"; then
                best[$key]=$ns
            fi
        done < <(printf '%s\n' "$out" | grep '^BENCH-NS ')
        if ! printf '%s\n' "$out" | grep -q '^BENCH-NS '; then
            echo "error: tier $tier produced no results:" >&2
            printf '%s\n' "$out" | tail -5 >&2
            break
        fi
    done
done

printf "%-8s %-12s %12s\n" tier workload ns/op
for key in "${order[@]}"; do
    printf "%-8s %-12s %12s\n" "${key%%/*}" "${key#*/}" "${best[$key]}"
done
