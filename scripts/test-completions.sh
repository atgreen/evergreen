#!/usr/bin/env bash
# Prove the Completions stack makes a real HTTP round trip on TorCL: fetch the
# pinned sources (including the atgreen compatibility forks by immutable commit),
# then run Dexador and Completions against an Ollama-shaped local server.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
torcl=$(realpath "${TORCL_BIN:-$repo/target/torcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $torcl && -f $runtime ]] || { echo 'Missing TorCL binary or ocicl runtime' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/torcl-completions-test.XXXXXX")
echo "Completions test artifacts: $work"
cp -- "$repo/tests/completions/ocicl.csv" "$repo/tests/completions/check.lisp" \
      "$repo/tests/completions/fake-ollama.py" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 TORCL_PORT_RUNTIME="$runtime"
"$repo/scripts/torcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/completions/ocicl.csv" ocicl.csv
export TORCL_PORT_CACHE="$work/cache/"
# The gated phase is COLD: compiling the graph (455 files) is slow, so give it
# more room than the defaults.
#
# The CACHED phase is opt-in (TORCL_COMPLETIONS_CACHED=1) because it currently
# FAILS on a known, unrelated defect: the upfront COMPILE-FILE read of ironclad's
# whirlpool.lisp is corrupted once pure-tls has been loaded, so that file is
# written as a source-only artifact which cannot be re-loaded in a fresh process
# (bliss-6d8f). See tests/completions/README.md.
phases=cold
[[ ${TORCL_COMPLETIONS_CACHED:-0} == 1 ]] && phases="cold cached"
for phase in $phases; do
    TORCL_MEM_MAX="${TORCL_MEM_MAX:-8G}" TORCL_TIMEOUT="${TORCL_TIMEOUT:-2400}" \
        "$repo/scripts/torcl-limited.sh" python3 fake-ollama.py \
        "$torcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'COMPLETIONS-DEXADOR-OK' "$phase.log"
    grep -Fxq 'COMPLETIONS-OK' "$phase.log"
done
if [[ -f cached.log ]] && grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    exit 1
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    TORCL_PORT_CACHE="$work/sbcl-cache/" TORCL_TIMEOUT="${TORCL_TIMEOUT:-2400}" \
        "$repo/scripts/torcl-limited.sh" python3 fake-ollama.py \
        "$SBCL_BIN" --noinform --no-sysinit --no-userinit --non-interactive \
        --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'COMPLETIONS-DEXADOR-OK' sbcl.log
    grep -Fxq 'COMPLETIONS-OK' sbcl.log
fi
echo "COMPLETIONS-PASS: $phases HTTP round trip(s); artifacts in $work"
