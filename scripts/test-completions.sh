#!/usr/bin/env bash
# Prove the Completions stack makes a real HTTP round trip on EGCL: fetch the
# pinned sources (including the atgreen compatibility forks by immutable commit),
# then run Dexador and Completions against an Ollama-shaped local server.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $egcl && -f $runtime ]] || { echo 'Missing EGCL binary or ocicl runtime' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-completions-test.XXXXXX")
echo "Completions test artifacts: $work"
cp -- "$repo/tests/completions/ocicl.csv" "$repo/tests/completions/check.lisp" \
      "$repo/tests/completions/fake-ollama.py" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/completions/ocicl.csv" ocicl.csv
export EGCL_PORT_CACHE="$work/cache/"
# Compiling the graph (455 files) is slow; give it more room than the defaults.
for phase in cold cached; do
    EGCL_MEM_MAX="${EGCL_MEM_MAX:-8G}" EGCL_TIMEOUT="${EGCL_TIMEOUT:-2400}" \
        "$repo/scripts/egcl-limited.sh" python3 fake-ollama.py \
        "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'COMPLETIONS-DEXADOR-OK' "$phase.log"
    grep -Fxq 'COMPLETIONS-OK' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    exit 1
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    EGCL_PORT_CACHE="$work/sbcl-cache/" EGCL_TIMEOUT="${EGCL_TIMEOUT:-2400}" \
        "$repo/scripts/egcl-limited.sh" python3 fake-ollama.py \
        "$SBCL_BIN" --noinform --no-sysinit --no-userinit --non-interactive \
        --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'COMPLETIONS-DEXADOR-OK' sbcl.log
    grep -Fxq 'COMPLETIONS-OK' sbcl.log
fi
echo "COMPLETIONS-PASS: cold and cached HTTP round trips; artifacts in $work"
