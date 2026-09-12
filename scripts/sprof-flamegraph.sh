#!/usr/bin/env bash
# sprof-flamegraph.sh — a self-contained HTML flamegraph of a bliss run's LISP
# call stacks, across tiers (treewalk / T0 bytecode / T1 / T2), from the
# Lisp-aware sampler (bliss-sc4t). Needs no external tools — unlike perf +
# FlameGraph, it renders directly in a browser.
#
# The sampler already produces Brendan-Gregg collapsed stacks (each frame is
# `NAME [tier]`), so this also works with flamegraph.pl / speedscope; this
# wrapper just gives you a zero-dependency HTML view.
#
# Usage:
#   # profile a workload and open the flamegraph:
#   scripts/sprof-flamegraph.sh --out /tmp/flame.html -- \
#       target/.../bliss-cli --no-init --load prog.lisp
#
#   # or render an existing folded file (from BLISS_SPROF_OUT):
#   scripts/sprof-flamegraph.sh --folded run.folded --out /tmp/flame.html
#
# Options:
#   --out FILE     output HTML (default: /tmp/bliss-flame.html)
#   --hz N         sampling frequency for a profiled run (default: 1000)
#   --folded FILE  render an existing folded-stacks file instead of running
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$HERE/tools/sprof-flamegraph/flame.html"
OUT="/tmp/bliss-flame.html"
HZ=1000
FOLDED=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --hz) HZ="$2"; shift 2 ;;
    --folded) FOLDED="$2"; shift 2 ;;
    --) shift; break ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "sprof-flamegraph.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
[[ -f "$TEMPLATE" ]] || { echo "sprof-flamegraph.sh: template not found: $TEMPLATE" >&2; exit 1; }

WORK="$(mktemp -d /tmp/bliss-flame.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

if [[ -z "$FOLDED" ]]; then
  [[ $# -gt 0 ]] || { echo "sprof-flamegraph.sh: give a bliss command after -- (or use --folded FILE)" >&2; exit 2; }
  FOLDED="$WORK/run.folded"
  echo "[sprof-flamegraph] recording (BLISS_SPROF=$HZ) ..." >&2
  BLISS_SPROF="$HZ" BLISS_SPROF_OUT="$FOLDED" "$@" >/dev/null 2>&1 || true
fi
[[ -s "$FOLDED" ]] || { echo "[sprof-flamegraph] no samples captured in $FOLDED (did the workload run long enough?)" >&2; exit 1; }

# Splice the folded stacks into the template. Use a replacement callable so the
# data (which contains no backslashes but may contain regex metachars) is
# inserted verbatim.
OUT="$OUT" TEMPLATE="$TEMPLATE" FOLDED="$FOLDED" python3 - <<'PY'
import os, re
tpl = open(os.environ["TEMPLATE"], encoding="utf-8").read()
data = open(os.environ["FOLDED"], encoding="utf-8").read().rstrip("\n")
# escape backtick and backslash for the JS template literal
data = data.replace("\\", "\\\\").replace("`", "\\`")
new, n = re.subn(r"/\*__FOLDED__\*/.*?/\*__END__\*/",
                 lambda m: "/*__FOLDED__*/ `" + data + "` /*__END__*/",
                 tpl, count=1, flags=re.DOTALL)
if n != 1:
    raise SystemExit("sprof-flamegraph.sh: injection markers not found in template")
open(os.environ["OUT"], "w", encoding="utf-8").write(new)
PY

N="$(awk '{s+=$NF} END{print s+0}' "$FOLDED")"
echo "[sprof-flamegraph] wrote $OUT  ($N samples) — open it in a browser." >&2
