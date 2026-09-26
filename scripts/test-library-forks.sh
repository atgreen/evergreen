#!/usr/bin/env bash
# Fetch immutable ocicl Git sources and exercise cold/cached library loads.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
torcl=$(realpath "${TORCL_BIN:-$repo/target/torcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $torcl && -f $runtime ]] || { echo 'Missing TorCL image or ocicl runtime' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/torcl-library-test.XXXXXX")
echo "Library test artifacts: $work"
cp -- "$repo/tests/library-forks/ocicl.csv" "$work/ocicl.csv"
cp -- "$repo/tests/library-forks/"*.lisp "$repo/tests/library-forks/"*.txt "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 TORCL_PORT_RUNTIME="$runtime"
"$repo/scripts/torcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/library-forks/ocicl.csv" ocicl.csv
export TORCL_PORT_CACHE="$work/cache/"
for phase in cold cached; do
    "$repo/scripts/torcl-limited.sh" "$torcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'LIBRARY-FORKS-OK' "$phase.log"
done
# Multibyte UTF-8 through Babel's and Flexi's decoders (bliss-rnd7, bliss-bsjw).
# Its own phase because it loads check.lisp itself; the ASCII phases above would
# otherwise keep passing while every non-ASCII character was rejected.
"$repo/scripts/torcl-limited.sh" "$torcl" --no-init --load check-utf8.lisp >utf8.log 2>&1
grep -Fxq 'MULTIBYTE-UTF8-OK' utf8.log
if grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    exit 1
fi
if [[ ${TORCL_PORT_STRESS:-0} == 1 ]]; then
    TORCL_GC_STRESS=20000 TORCL_GC_POISON=1 TORCL_GC_VERIFY=1 \
        "$repo/scripts/torcl-limited.sh" "$torcl" --no-init --load check.lisp >stress.log 2>&1
    grep -Fxq 'LIBRARY-FORKS-OK' stress.log
    TORCL_GC_STRESS=20000 TORCL_GC_POISON=1 TORCL_GC_VERIFY=1 \
        "$repo/scripts/torcl-limited.sh" "$torcl" --no-init --load check-utf8.lisp \
        >utf8-stress.log 2>&1
    grep -Fxq 'MULTIBYTE-UTF8-OK' utf8-stress.log
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    TORCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/torcl-limited.sh" "$SBCL_BIN" \
        --noinform --no-sysinit --no-userinit --non-interactive --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'LIBRARY-FORKS-OK' sbcl.log
    TORCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/torcl-limited.sh" "$SBCL_BIN" \
        --noinform --no-sysinit --no-userinit --non-interactive --load check-utf8.lisp \
        >utf8-sbcl.log 2>&1
    grep -Fxq 'MULTIBYTE-UTF8-OK' utf8-sbcl.log
fi
echo "LIBRARY-FORKS-PASS: cold and cached loads, multibyte UTF-8; artifacts in $work"
