#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# Fetch the pinned native usocket port and prove incremental loopback I/O.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
[[ -x $egcl && -f $runtime ]] || { echo 'Missing EGCL binary or ocicl runtime' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-usocket-test.XXXXXX")
echo "Usocket test artifacts: $work"
cp -- "$repo/tests/usocket-fork/ocicl.csv" "$repo/tests/usocket-fork/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/usocket-fork/ocicl.csv" ocicl.csv
fixture=$(awk -F', ' '$1 == "usocket" {
    sub(/usocket\.asd$/, "tests/egcl-client-fixture.py", $3)
    print "ocicl/" $3
}' ocicl.csv)
[[ -f $fixture ]] || { echo 'Missing pinned usocket fixture' >&2; exit 1; }
export EGCL_PORT_CACHE="$work/cache/"
for phase in cold cached; do
    "$repo/scripts/egcl-limited.sh" python3 "$fixture" \
        "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1
    grep -Fxq 'USOCKET-NATIVE-CLIENT-OK' "$phase.log"
    grep -Fxq 'USOCKET-NATIVE-SERVER-OK' "$phase.log"
    grep -Fxq 'USOCKET-READ-TIMEOUT-PASS' "$phase.log"
    grep -Fxq 'USOCKET-LOOPBACK-FIXTURE-OK' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo "Cached load unexpectedly compiled source; see $work/cached.log" >&2
    exit 1
fi
if [[ ${EGCL_PORT_STRESS:-0} == 1 ]]; then
    EGCL_GC_STRESS=1000 EGCL_GC_POISON=1 EGCL_GC_VERIFY=1 USOCKET_TEST_TIMEOUT=330 \
        "$repo/scripts/egcl-limited.sh" python3 "$fixture" \
        "$egcl" --no-init --load check.lisp >stress.log 2>&1
    grep -Fxq 'USOCKET-NATIVE-CLIENT-OK' stress.log
    grep -Fxq 'USOCKET-NATIVE-SERVER-OK' stress.log
    grep -Fxq 'USOCKET-READ-TIMEOUT-PASS' stress.log
    grep -Fxq 'USOCKET-LOOPBACK-FIXTURE-OK' stress.log
fi
if [[ -n ${SBCL_BIN:-} ]]; then
    EGCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/egcl-limited.sh" \
        python3 "$fixture" "$SBCL_BIN" --noinform --no-sysinit --no-userinit \
        --non-interactive --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'USOCKET-NATIVE-CLIENT-OK' sbcl.log
    grep -Fxq 'USOCKET-LOOPBACK-FIXTURE-OK' sbcl.log
fi
echo "USOCKET-FORK-PASS: cold and cached native TCP client, server, and read timeouts; artifacts in $work"
