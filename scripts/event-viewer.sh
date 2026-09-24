#!/usr/bin/env bash
# event-viewer.sh — render a torcl run's JIT/GC event stream as a self-contained,
# JITWatch-style HTML page (bliss-3gme, profiling epic bliss-bfxm).
#
# TorCL records a JFR-style event stream (tier promotions, deopt-with-reason,
# OSR entries, GC pauses) into a low-overhead ring buffer when TORCL_EVENTS=1.
# This wrapper runs a torcl command with recording on, has it dump the stream as
# JSON via TORCL-EXT:EVENTS-JSON, and injects that JSON into
# tools/event-viewer/viewer.html to produce a standalone page (no server, no
# external assets) you open in a browser — the offline model of JITWatch / JDK
# Mission Control.
#
# Usage:
#   scripts/event-viewer.sh [--out FILE] -- <torcl command...>
#
# The command runs exactly as given (add --load / --eval yourself); this wrapper
# sets TORCL_EVENTS=1 (record) and TORCL_EVENTS_DUMP=<file> (torcl writes the
# stream as JSON to that file as it exits), so it works with any workload —
# --load, --eval, or a REPL session — without editing your program.
#
# Options:
#   --out FILE   output HTML path (default: /tmp/torcl-events.html)
#
# Example:
#   scripts/event-viewer.sh --out /tmp/babel-jit.html -- \
#     target/x86_64-unknown-linux-musl/release/torcl --no-init --load babel-load.lisp
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$HERE/tools/event-viewer/viewer.html"
OUT="/tmp/torcl-events.html"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --) shift; break ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "event-viewer.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
[[ $# -gt 0 ]] || { echo "event-viewer.sh: no torcl command given (use: ... -- <torcl ...>)" >&2; exit 2; }
[[ -f "$TEMPLATE" ]] || { echo "event-viewer.sh: template not found: $TEMPLATE" >&2; exit 1; }

WORK="$(mktemp -d /tmp/torcl-events.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
DUMP="$WORK/events.json"

# TORCL_EVENTS_DUMP makes torcl write the ring buffer as JSON to $DUMP as it
# exits — after the whole workload has run, whatever way it ends.
echo "[event-viewer] recording (TORCL_EVENTS=1) ..." >&2
TORCL_EVENTS=1 TORCL_EVENTS_DUMP="$DUMP" "$@" >/dev/null 2>&1 || true

JSON="$(grep -m1 '^{"events":' "$DUMP" 2>/dev/null || true)"
if [[ -z "$JSON" ]]; then
  echo "[event-viewer] no event JSON captured (is $DUMP written?)." >&2
  echo "  The command must reach process exit so torcl can flush the dump." >&2
  exit 1
fi

# Inject: replace the placeholder-delimited default sample with the live JSON.
# Use a Python splice so the JSON (which contains regex-special chars) is treated
# literally — no sed escaping hazards.
OUT="$OUT" TEMPLATE="$TEMPLATE" JSON="$JSON" python3 - <<'PY'
import os, re
tpl = open(os.environ["TEMPLATE"], encoding="utf-8").read()
json = os.environ["JSON"]
# Replace everything between the two markers (the default sample) with live data.
# Use a replacement *function*, not a string: a string replacement would have
# re interpret backslash escapes in the JSON (turning \n / \\ into real control
# chars and corrupting it). A callable's return value is inserted verbatim.
repl = "/*__EVENT_DATA__*/ " + json + " /*__END__*/"
new, n = re.subn(r"/\*__EVENT_DATA__\*/.*?/\*__END__\*/",
                 lambda m: repl,
                 tpl, count=1, flags=re.DOTALL)
if n != 1:
    raise SystemExit("event-viewer.sh: could not find injection markers in template")
open(os.environ["OUT"], "w", encoding="utf-8").write(new)
PY

COUNT="$(printf '%s' "$JSON" | grep -o '"seq"' | wc -l | tr -d ' ')"
echo "[event-viewer] wrote $OUT  (${COUNT} events)" >&2
echo "               open it in a browser." >&2
