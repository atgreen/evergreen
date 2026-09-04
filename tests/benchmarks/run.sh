#!/usr/bin/env bash
# Benchmark runner: bliss vs SBCL (bliss-jpd0; spec G2 / R10.09 / R10.10).
#
#   tests/benchmarks/run.sh [bench...]      # default: every gabriel/*.lisp
#
# Protocol: each benchmark file does one untimed warmup run (so bliss is
# measured at its promoted tier — G2 is PEAK throughput) and prints
# "BENCH-MS <name> <ms>" from IN-PROCESS get-internal-real-time timing, so
# implementation startup never pollutes the number.  Each benchmark runs
# N (default 5, per R10.59) times per implementation and the MINIMUM is
# reported — wall clock on a loaded box drifts ~2x (see scripts/icount.sh),
# so min-of-N on a quiet machine is the meaningful statistic.  SBCL numbers
# at 1-2 ms are near timer resolution; treat ratios there as lower bounds.
#
# For each bliss run the T2 trace (BLISS_T2_LOG) is captured and the report
# says whether the workload actually reached T2 ("background T2 result
# published").  A ratio without that flag conflates codegen quality with
# promotion coverage (a tak-style emitter bail looks like a codegen disaster).
#
# Env overrides: BLISS (binary), SBCL (binary), N (runs per benchmark).
set -u

cd "$(dirname "$0")/../.." || exit 1

BLISS=${BLISS:-target/x86_64-unknown-linux-musl/release/bliss-cli}
SBCL=${SBCL:-sbcl}
N=${N:-5}

if [ ! -x "$BLISS" ]; then
    echo "error: bliss binary not found at $BLISS" >&2
    echo "       build it with: cargo build --release -p bliss-cli --bin bliss-cli" >&2
    exit 1
fi
have_sbcl=1
command -v "$SBCL" >/dev/null 2>&1 || have_sbcl=0
[ "$have_sbcl" = 0 ] && echo "note: sbcl not found — reporting bliss times only" >&2

if [ $# -gt 0 ]; then
    benches=("$@")
else
    benches=()
    for f in tests/benchmarks/gabriel/*.lisp; do
        b=$(basename "$f" .lisp)
        [ "$b" = prelude ] && continue
        benches+=("$b")
    done
fi

# min_ms <impl-command...> -- runs the benchmark file N times, echoes min ms
# (or "FAIL"). The benchmark file is in $bench_file; T2 log in $t2log if set.
min_ms() {
    local min="" ms
    for _ in $(seq "$N"); do
        ms=$("$@" 2>/dev/null | sed -n 's/^BENCH-MS [^ ]* //p' | head -1)
        case "$ms" in
            ''|*[!0-9]*) echo "FAIL"; return ;;
        esac
        if [ -z "$min" ] || [ "$ms" -lt "$min" ]; then min=$ms; fi
    done
    echo "$min"
}

printf "%-10s %10s %10s %8s %5s\n" benchmark bliss-ms sbcl-ms ratio T2
for b in "${benches[@]}"; do
    bench_file=tests/benchmarks/gabriel/$b.lisp
    if [ ! -f "$bench_file" ]; then
        echo "error: no such benchmark: $bench_file" >&2
        continue
    fi
    t2log=$(mktemp)
    bliss_ms=$(BLISS_T2_LOG="$t2log" min_ms "$BLISS" --no-init --load "$bench_file")
    t2=no
    grep -q "T2 result published" "$t2log" 2>/dev/null && t2=yes
    rm -f "$t2log"
    sbcl_ms=-
    ratio=-
    if [ "$have_sbcl" = 1 ]; then
        sbcl_ms=$(min_ms "$SBCL" --non-interactive --no-userinit --no-sysinit --load "$bench_file")
        if [ "$bliss_ms" != FAIL ] && [ "$sbcl_ms" != FAIL ] && [ "$sbcl_ms" -gt 0 ]; then
            ratio=$(awk "BEGIN {printf \"%.1fx\", $bliss_ms / $sbcl_ms}")
        fi
    fi
    printf "%-10s %10s %10s %8s %5s\n" "$b" "$bliss_ms" "$sbcl_ms" "$ratio" "$t2"
done
