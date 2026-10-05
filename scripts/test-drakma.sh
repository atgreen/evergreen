#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $egcl && -f $runtime ]] || { echo 'Missing EGCL binary or ocicl runtime' >&2; exit 1; }
"$repo/scripts/egcl-limited.sh" "$egcl" --no-init \
    --load "$repo/tests/drakma/type-aliases.lisp"
"$repo/scripts/egcl-limited.sh" "$egcl" --no-init \
    --load "$repo/tests/drakma/gray-eof.lisp"
"$repo/scripts/egcl-limited.sh" "$egcl" --no-init \
    --load "$repo/tests/drakma/gray-sequence.lisp"
"$repo/scripts/egcl-limited.sh" "$egcl" --no-init \
    --load "$repo/tests/drakma/byte-types.lisp"
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-drakma-test.XXXXXX")
echo "Drakma test artifacts: $work"
cp -- "$repo/tests/drakma/ocicl.csv" "$repo/tests/drakma/check.lisp" \
    "$repo/tests/drakma/fixture.py" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/drakma/ocicl.csv" ocicl.csv
export EGCL_PORT_CACHE="$work/cache/"
for phase in cold cached; do
    "$repo/scripts/egcl-limited.sh" python3 fixture.py \
        "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'DRAKMA-HTTP-OK' "$phase.log"
    grep -Fxq 'DRAKMA-FIXTURE-OK' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    exit 1
fi
echo "DRAKMA-PASS: cold and cached GET/POST; artifacts in $work"
