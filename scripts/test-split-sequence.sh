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
work=$(mktemp -d "$repo/target/split-sequence-test.XXXXXX")
echo "Split-sequence test artifacts: $work"
cp -- "$repo/tests/split-sequence/ocicl.csv" "$repo/tests/split-sequence/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/split-sequence/ocicl.csv" ocicl.csv
export EGCL_PORT_CACHE="$work/cache/"
# Upstream includes one million randomized comparisons; do not shorten it.
export EGCL_TIMEOUT="${EGCL_TIMEOUT:-14400}"
"$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >egcl.log 2>&1
grep -Fxq 'SPLIT-SEQUENCE-SUITE-PASS' egcl.log
if [[ -n ${SBCL_BIN:-} ]]; then
    EGCL_PORT_CACHE="$work/sbcl-cache/" "$repo/scripts/egcl-limited.sh" "$SBCL_BIN" \
        --noinform --no-sysinit --no-userinit --non-interactive --load check.lisp >sbcl.log 2>&1
    grep -Fxq 'SPLIT-SEQUENCE-SUITE-PASS' sbcl.log
fi
echo "SPLIT-SEQUENCE-SUITE-PASS: complete upstream tests, including the million-iteration fuzz test; artifacts in $work"
