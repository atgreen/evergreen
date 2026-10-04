#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
mkdir -p "$repo/target"
work=$(mktemp -d "$repo/target/trivial-backtrace-test.XXXXXX")
echo "Trivial-backtrace test artifacts: $work"
cp -- "$repo/tests/trivial-backtrace/ocicl.csv" "$repo/tests/trivial-backtrace/"*.lisp "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime" EGCL_PORT_CACHE="$work/cache/"
unset EGCL_BACKEND EGCL_DISABLE_T2 EGCL_T2_DISCARD EGCL_T2_NO_QUEUE EGCL_PROFILING_DISABLED
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/trivial-backtrace/ocicl.csv" ocicl.csv
for phase in cold cached; do
    EGCL_FORCE_TIER=t0 "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'TRIVIAL-BACKTRACE-PASS' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo 'Cached load unexpectedly compiled source' >&2
    exit 1
fi
for tier in interp t1 t2; do
    EGCL_FORCE_TIER="$tier" "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$tier.log" 2>&1
    grep -Fxq 'TRIVIAL-BACKTRACE-PASS' "$tier.log"
done
if [[ ${EGCL_PORT_STRESS:-0} == 1 ]]; then
    EGCL_FORCE_TIER=t0 EGCL_GC_STRESS=1 EGCL_GC_POISON=1 EGCL_GC_VERIFY=1 \
        "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >stress.log 2>&1
    grep -Fxq 'TRIVIAL-BACKTRACE-PASS' stress.log
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    EGCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/egcl-limited.sh" "$SBCL_BIN" \
        --noinform --no-sysinit --no-userinit --non-interactive --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'TRIVIAL-BACKTRACE-PASS' sbcl.log
fi
echo 'TRIVIAL-BACKTRACE-PASS: pinned cold/cached loads, tiered frames, arguments, output and live conditions'
