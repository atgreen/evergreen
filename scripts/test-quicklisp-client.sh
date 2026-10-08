#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
[[ -x $egcl ]] || { echo 'Missing EGCL binary' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-quicklisp-test.XXXXXX")
echo "Quicklisp scenario artifacts: $work"
cp -- "$repo/tests/quicklisp-client/ocicl.csv" "$work/ocicl.csv"
cd "$work"
export OCICL_LOCAL_ONLY=1
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/quicklisp-client/ocicl.csv" ocicl.csv
source=$(awk -F', ' '$1 == "quicklisp" {
    sub(/quicklisp\/quicklisp\.asd$/, "", $3)
    print "ocicl/" $3
}' ocicl.csv)
source=$(realpath "$source")
[[ -f $source/tests/install.py ]] || { echo 'Missing pinned Quicklisp fixtures' >&2; exit 1; }
for phase in network filesystem install; do
    if ! "$repo/scripts/egcl-limited.sh" python3 "$source/tests/$phase.py" \
        "$egcl" >"$phase-egcl.log" 2>&1; then
        cat "$phase-egcl.log" >&2
        exit 1
    fi
done
grep -Fx 'QUICKLISP-NETWORK-SERVER-PASS' network-egcl.log
grep -Fx 'QUICKLISP-FILESYSTEM-PASS' filesystem-egcl.log
grep -Fx 'QUICKLISP-OFFLINE-RELOAD-PASS' install-egcl.log
if [[ -n ${SBCL_BIN:-} ]]; then
    sbcl=$(command -v "$SBCL_BIN")
    for phase in network filesystem install; do
        if ! "$repo/scripts/egcl-limited.sh" python3 "$source/tests/$phase.py" \
            "$sbcl" --sbcl >"$phase-sbcl.log" 2>&1; then
            cat "$phase-sbcl.log" >&2
            exit 1
        fi
    done
fi
if [[ ${QUICKLISP_PUBLIC_SMOKE:-0} == 1 ]]; then
    if ! "$repo/scripts/egcl-limited.sh" python3 \
        "$repo/tests/quicklisp-client/public.py" "$source" "$egcl" \
        >public-egcl.log 2>&1; then
        cat public-egcl.log >&2
        exit 1
    fi
    grep -Fx 'QUICKLISP-PUBLIC-ALEXANDRIA-PASS' public-egcl.log
fi
echo "QUICKLISP-CLIENT-PASS: native networking, filesystem, cold install and offline reload; artifacts in $work"
