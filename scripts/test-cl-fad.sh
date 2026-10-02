#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
mkdir -p "$repo/target"
work=$(mktemp -d "$repo/target/cl-fad-test.XXXXXX")
echo "CL-FAD test artifacts: $work"
cp -- "$repo/tests/cl-fad/ocicl.csv" "$repo/tests/cl-fad/"check*.lisp "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/cl-fad/ocicl.csv" ocicl.csv
export EGCL_PORT_CACHE="$work/cache/"
export EGCL_FAD_VALUE='cl-fad test value' EGCL_FAD_EMPTY=''
unset EGCL_FAD_UNSET_892481
mkdir temporary
export TMPDIR="$work/temporary/"
for phase in cold cached; do
    "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'CL-FAD-LOAD-PASS' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo 'Cached load unexpectedly compiled source' >&2
    exit 1
fi
sources=("$work"/ocicl/cl-fad-*/temporary-files.lisp)
[[ ${#sources[@]} == 1 && -f ${sources[0]} ]]
export CL_FAD_SOURCE="${sources[0]}"
for mode in native uiop absent; do
    export CL_FAD_MODE="$mode"
    "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check-getenv.lisp >"getenv-$mode.log" 2>&1
    grep -Fxq "CL-FAD-GETENV-PASS: $mode" "getenv-$mode.log"
    if [[ -n ${SBCL_BIN:-} ]]; then
        "$repo/scripts/egcl-limited.sh" "$SBCL_BIN" --noinform --no-sysinit --no-userinit \
            --non-interactive --load check-getenv.lisp >"sbcl-$mode.log" 2>&1
        grep -Fxq "CL-FAD-GETENV-PASS: $mode" "sbcl-$mode.log"
    fi
done
echo 'CL-FAD-PASS: cold/cached loads and native/optional UIOP getenv'
