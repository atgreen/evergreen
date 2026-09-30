#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# flamegraph.sh — CPU flamegraph of a egcl run, symbolicating JIT'd (T1/T2/OSR)
# native frames (bliss-0ivo, profiling epic bliss-bfxm).
#
# EGCL already emits the symbolication data `perf` needs: set EGCL_PERF_MAP=1
# and it writes /tmp/perf-<pid>.map (addr size name) so `perf` names JIT frames;
# set EGCL_PERF_JITDUMP=1 for the richer perf jitdump (usable via `perf inject
# --jit`). This wrapper turns a egcl command into folded stacks and, if the
# Brendan Gregg FlameGraph tools are available, an SVG.
#
# Usage:
#   scripts/flamegraph.sh [options] -- <egcl command...>
#
# Options:
#   --out FILE      output path (default: /tmp/egcl-flamegraph.svg, or
#                   /tmp/egcl-profile.perf for --speedscope)
#   --speedscope    emit raw `perf script` data instead of an SVG, for
#                   https://speedscope.app (drag-and-drop; keeps time-ordering,
#                   Time Order / Left Heavy / Sandwich views). Needs no extra
#                   tools. Equivalent to --format speedscope.
#   --format M      svg (FlameGraph, default) | speedscope
#   --freq N        perf sampling frequency in Hz (default: 999)
#   --call-graph M  perf unwind mode: dwarf|fp|lbr (default: dwarf)
#   --jitdump       also emit + `perf inject --jit` the jitdump (precise JIT
#                   symbols/annotation; needs a jitdump-capable perf)
#   --pin-core N    taskset to CPU N (default: 0 — this box is a hybrid Intel
#                   CPU whose P/E-core PMUs differ; pinning avoids counter flake,
#                   see AGENTS.md "Running rr")
#
# Example:
#   scripts/flamegraph.sh --out /tmp/babel.svg -- \
#     target/x86_64-unknown-linux-musl/release/egcl --no-init \
#       --eval '(dotimes (i 5000000) (some-hot-fn))'
#
# Caveats on this machine (see AGENTS.md): the static-pie/LTO/JIT binary makes
# frame-pointer and DWARF unwinding partly unreliable; `--call-graph dwarf` is
# usually the least-bad. If stacks look truncated, try `--call-graph lbr` (Intel
# only) or build with frame pointers. perf_event_paranoid may need to be <= 1
# (no passwordless sudo here — ask the operator).
set -euo pipefail

OUT=""
FREQ=999
CALLGRAPH=dwarf
JITDUMP=0
PIN_CORE=0
FORMAT=svg   # svg (FlameGraph) | speedscope (raw perf script, speedscope.app)

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --freq) FREQ="$2"; shift 2 ;;
    --call-graph) CALLGRAPH="$2"; shift 2 ;;
    --jitdump) JITDUMP=1; shift ;;
    --pin-core) PIN_CORE="$2"; shift 2 ;;
    --format) FORMAT="$2"; shift 2 ;;
    --speedscope) FORMAT=speedscope; shift ;;
    --) shift; break ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "flamegraph.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

if [[ $# -eq 0 ]]; then
  echo "flamegraph.sh: no command given (use: ... -- <egcl ...>)" >&2
  exit 2
fi

# Default output path depends on the format.
if [[ -z "$OUT" ]]; then
  case "$FORMAT" in
    speedscope) OUT=/tmp/egcl-profile.perf ;;
    *) OUT=/tmp/egcl-flamegraph.svg ;;
  esac
fi

command -v perf >/dev/null 2>&1 || { echo "flamegraph.sh: 'perf' not found" >&2; exit 1; }

WORK="$(mktemp -d /tmp/egcl-flame.XXXXXX)"
PERF_DATA="$WORK/perf.data"
FOLDED="${OUT%.svg}.folded"

echo "[flamegraph] recording (freq=${FREQ}Hz call-graph=${CALLGRAPH} core=${PIN_CORE}) ..." >&2
# EGCL_PERF_MAP names JIT frames; -k 1 (monotonic clock) is needed for jitdump.
record_extra=()
[[ "$JITDUMP" == 1 ]] && record_extra+=(-k 1)
EGCL_PERF_MAP=1 EGCL_PERF_JITDUMP="$JITDUMP" \
  taskset -c "$PIN_CORE" \
  perf record -F "$FREQ" --call-graph "$CALLGRAPH" -o "$PERF_DATA" -- "$@" \
  || { echo "[flamegraph] perf record failed (perf_event_paranoid? hybrid-CPU PMU? see caveats above)" >&2; exit 1; }

SCRIPT_DATA="$PERF_DATA"
if [[ "$JITDUMP" == 1 ]]; then
  echo "[flamegraph] perf inject --jit ..." >&2
  if perf inject --jit --input "$PERF_DATA" --output "$WORK/perf.jit.data" 2>/dev/null; then
    SCRIPT_DATA="$WORK/perf.jit.data"
  else
    echo "[flamegraph] perf inject --jit unavailable; falling back to perf-map symbols" >&2
  fi
fi

# speedscope format: raw `perf script` output, imported natively by
# https://speedscope.app — keeps per-sample time-ordering (Time Order / Left
# Heavy / Sandwich views), unlike the aggregated flamegraph. No extra tools.
if [[ "$FORMAT" == "speedscope" ]]; then
  perf script -i "$SCRIPT_DATA" > "$OUT"
  echo "[flamegraph] wrote $OUT" >&2
  echo "             load it at https://speedscope.app (drag-and-drop)" >&2
  exit 0
fi

# Locate FlameGraph tools: PATH, then $FLAMEGRAPH_DIR, then ~/FlameGraph.
find_fg() {
  local name="$1"
  command -v "$name" 2>/dev/null && return 0
  for d in "${FLAMEGRAPH_DIR:-}" "$HOME/FlameGraph" "$HOME/src/FlameGraph"; do
    [[ -n "$d" && -x "$d/$name" ]] && { echo "$d/$name"; return 0; }
  done
  return 1
}
STACKCOLLAPSE="$(find_fg stackcollapse-perf.pl || true)"
FLAMEGRAPH="$(find_fg flamegraph.pl || true)"

if [[ -z "$STACKCOLLAPSE" ]]; then
  # No FlameGraph: still give the user the raw perf script so nothing is lost.
  perf script -i "$SCRIPT_DATA" > "$WORK/perf.script"
  echo "[flamegraph] FlameGraph tools not found." >&2
  echo "  Raw samples:   $WORK/perf.script" >&2
  echo "  To render:     git clone https://github.com/brendangregg/FlameGraph," >&2
  echo "                 then set FLAMEGRAPH_DIR=<that dir> and re-run," >&2
  echo "                 or: stackcollapse-perf.pl $WORK/perf.script | flamegraph.pl > $OUT" >&2
  exit 0
fi

perf script -i "$SCRIPT_DATA" | "$STACKCOLLAPSE" > "$FOLDED"
echo "[flamegraph] folded stacks: $FOLDED" >&2
if [[ -n "$FLAMEGRAPH" ]]; then
  "$FLAMEGRAPH" --title "egcl CPU flamegraph" "$FOLDED" > "$OUT"
  echo "[flamegraph] wrote $OUT" >&2
else
  echo "[flamegraph] flamegraph.pl not found; folded stacks are at $FOLDED" >&2
  echo "             render with: flamegraph.pl $FOLDED > $OUT" >&2
fi
