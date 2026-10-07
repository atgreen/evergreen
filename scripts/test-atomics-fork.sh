#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
mkdir -p "$repo/target"
work=$(mktemp -d "$repo/target/atomics-fork-test.XXXXXX")
echo "Atomics test artifacts: $work"
cp -- "$repo/tests/atomics-fork/ocicl.csv" "$repo/tests/atomics-fork/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime" EGCL_PORT_CACHE="$work/cache/"
unset EGCL_BACKEND EGCL_DISABLE_T2 EGCL_T2_DISCARD EGCL_T2_NO_QUEUE EGCL_PROFILING_DISABLED
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/atomics-fork/ocicl.csv" ocicl.csv
for phase in cold cached; do
    EGCL_FORCE_TIER=t0 "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'ATOMICS-EGCL-CAS-PASS' "$phase.log"
    grep -Fxq 'CL-CANCEL-ATOMIC-INIT-PASS' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo 'Cached load unexpectedly compiled source' >&2
    exit 1
fi
for tier in interp t1 t2; do
    EGCL_FORCE_TIER="$tier" "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$tier.log" 2>&1
    grep -Fxq 'ATOMICS-EGCL-CAS-PASS' "$tier.log"
    grep -Fxq 'CL-CANCEL-ATOMIC-INIT-PASS' "$tier.log"
done
if [[ ${EGCL_PORT_STRESS:-0} == 1 ]]; then
    EGCL_FORCE_TIER=t0 EGCL_GC_STRESS=1 EGCL_GC_POISON=1 EGCL_GC_VERIFY=1 \
        "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >stress.log 2>&1
    grep -Fxq 'ATOMICS-EGCL-CAS-PASS' stress.log
    grep -Fxq 'CL-CANCEL-ATOMIC-INIT-PASS' stress.log
fi
echo 'ATOMICS-FORK-PASS: pinned cold/cached CAS checks and cl-cancel atomic-state initialization'
