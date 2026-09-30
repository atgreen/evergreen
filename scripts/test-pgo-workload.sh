#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# Run from the repository root; wrap this script in egcl-limited.sh.
set -euo pipefail
binary=${1:?usage: bash scripts/test-pgo-workload.sh EGCL_OR_SBCL [sbcl]}
mode=${2:-egcl}
work=$(mktemp -d /tmp/egcl-pgo-workload-test.XXXXXX)
echo "PGO workload test artifacts: $work"
if [[ $mode == sbcl ]]; then
    args=(--noinform --no-sysinit --no-userinit --non-interactive)
else
    args=(--no-init)
fi
run_phase() {
    EGCL_PGO_WORK="$work/system with spaces" EGCL_PGO_PHASE="$1" \
        "$binary" "${args[@]}" --load scripts/pgo-workload.lisp
}
expect_failure() {
    local phase=$1 diagnostic=$2
    if run_phase "$phase" >"$work/$phase-rejected.log" 2>&1; then
        echo "unexpected success: $phase" >&2
        exit 1
    fi
    grep -F -m 1 "$diagnostic" "$work/$phase-rejected.log"
}
expect_failure load 'PGO workload is not prepared'
expect_failure bogus 'Unknown PGO phase'
for invalid_work in '' relative-directory; do
    if EGCL_PGO_WORK="$invalid_work" EGCL_PGO_PHASE=load \
        "$binary" "${args[@]}" --load scripts/pgo-workload.lisp \
        >"$work/invalid-work.log" 2>&1; then
        echo 'unexpected success with invalid work directory' >&2
        exit 1
    fi
    grep -F -m 1 'PGO work directory must be absolute' "$work/invalid-work.log"
done
run_phase prepare >"$work/prepare.log" 2>&1
grep -F 'PGO-PREPARED 24' "$work/prepare.log"
snapshot() {
    find "$work/system with spaces" -type f -printf '%P %s %T@\n' | sort
}
snapshot >"$work/before.txt"
for iteration in 1 2; do
    run_phase load >"$work/load-$iteration.log" 2>&1
    grep -F 'PGO-LOAD 24 300' "$work/load-$iteration.log"
    snapshot >"$work/after.txt"
    cmp "$work/before.txt" "$work/after.txt"
    run_phase runtime >"$work/runtime-$iteration.log" 2>&1
    grep -F 'PGO-RUNTIME 1000 499500' "$work/runtime-$iteration.log"
done
# A missing FASL must fail, not silently train on compilation.
fasl=$(find "$work/system with spaces/cache" -type f -print -quit)
test -n "$fasl"
mv "$fasl" "$fasl.saved"
expect_failure load 'PGO cached load attempted compilation'
mv "$fasl.saved" "$fasl"
# Preparation can also be repeated against an existing private workspace.
run_phase prepare >"$work/prepare-again.log" 2>&1
grep -F 'PGO-PREPARED 24' "$work/prepare-again.log"
run_phase load >"$work/load-again.log" 2>&1
grep -F 'PGO-LOAD 24 300' "$work/load-again.log"
echo 'PGO workload tests passed'
