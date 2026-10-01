#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
# Run pinned Ironclad regressions; EGCL_IRONCLAD_FULL_TESTS=1 selects its full suite.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-ironclad-test.XXXXXX")
echo "Ironclad test artifacts: $work"
cp -- "$repo/tests/ironclad/ocicl.csv" "$repo/tests/ironclad/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime" EGCL_PORT_CACHE="$work/cache/"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp -- "$repo/tests/ironclad/ocicl.csv" ocicl.csv
EGCL_MEM_MAX="${EGCL_MEM_MAX:-8G}" EGCL_TIMEOUT="${EGCL_TIMEOUT:-2400}" \
  "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >tests.log 2>&1
grep -Fxq 'IRONCLAD-WHIRLPOOL-OK' tests.log
grep -Fxq 'IRONCLAD-REGRESSIONS-OK' tests.log
echo "IRONCLAD-PASS: artifacts in $work"
