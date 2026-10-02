#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $egcl && -f $runtime ]] || { echo 'Missing EGCL image or ocicl runtime' >&2; exit 1; }
mkdir -p "$repo/target"
work=$(mktemp -d "$repo/target/babel-test.XXXXXX")
echo "Babel test artifacts: $work"
cp -- "$repo/tests/babel/ocicl.csv" "$repo/tests/babel/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/babel/ocicl.csv" ocicl.csv
export EGCL_PORT_CACHE="$work/cache/"
status=0
for phase in cold cached; do
    if ! "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1 ||
       ! grep -Fxq 'BABEL-SUITE-PASS' "$phase.log"; then
        echo "Babel $phase run failed; see $work/$phase.log" >&2
        status=1
    fi
done
if grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    status=1
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    if ! EGCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/egcl-limited.sh" "$SBCL_BIN" \
        --noinform --no-sysinit --no-userinit --non-interactive --load check.lisp >sbcl.log 2>&1 ||
       ! grep -Fxq 'BABEL-SUITE-PASS' sbcl.log; then
        echo "Babel SBCL baseline failed; see $work/sbcl.log" >&2
        status=1
    fi
fi
if (( status == 0 )); then
    echo "BABEL-SUITE-PASS: cold and cached upstream tests; artifacts in $work"
fi
exit "$status"
