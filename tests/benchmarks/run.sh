#!/usr/bin/env bash
# Benchmark runner: torcl vs SBCL (bliss-jpd0; spec G2 / R10.09 / R10.10).
#
#   tests/benchmarks/run.sh [bench...]      # default: every gabriel/*.lisp
#
# Protocol: each benchmark file does one untimed warmup run (so torcl is
# measured at its promoted tier — G2 is PEAK throughput) and prints
# "BENCH-MS <name> <ms>" from IN-PROCESS get-internal-real-time timing, so
# implementation startup never pollutes the number.  Each benchmark runs
# N (default 5, per R10.59) times per implementation and the MINIMUM is
# reported — wall clock on a loaded box drifts ~2x (see scripts/icount.sh),
# so min-of-N on a quiet machine is the meaningful statistic.  SBCL numbers
# at 1-2 ms are near timer resolution; treat ratios there as lower bounds.
#
# For each torcl run the T2 trace (TORCL_T2_LOG) is captured and the report
# says whether the workload actually reached T2 ("background T2 result
# published").  A ratio without that flag conflates codegen quality with
# promotion coverage (a tak-style emitter bail looks like a codegen disaster).
#
# Env overrides: TORCL (binary), SBCL (binary), N (runs per benchmark),
# CHECK=1 (enforce tests/benchmarks/thresholds.toml and exit non-zero on a
# regression — absolute torcl ms and T2-reached only; see that file for why the
# SBCL ratio is deliberately not gated).
set -u

cd "$(dirname "$0")/../.." || exit 1

TORCL=${TORCL:-target/x86_64-unknown-linux-musl/release/torcl}
SBCL=${SBCL:-sbcl}
N=${N:-5}
THRESHOLDS=tests/benchmarks/thresholds.toml
status=0

if [ ! -x "$TORCL" ]; then
    echo "error: torcl binary not found at $TORCL" >&2
    echo "       build it with: cargo build --release -p torcl --bin torcl" >&2
    exit 1
fi
have_sbcl=1
command -v "$SBCL" >/dev/null 2>&1 || have_sbcl=0
[ "$have_sbcl" = 0 ] && echo "note: sbcl not found — reporting torcl times only" >&2

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

printf "%-10s %10s %10s %8s %5s\n" benchmark torcl-ms sbcl-ms ratio T2
for b in "${benches[@]}"; do
    bench_file=tests/benchmarks/gabriel/$b.lisp
    if [ ! -f "$bench_file" ]; then
        echo "error: no such benchmark: $bench_file" >&2
        continue
    fi
    t2log=$(mktemp)
    torcl_ms=$(TORCL_T2_LOG="$t2log" min_ms "$TORCL" --no-init --load "$bench_file")
    t2=no
    grep -q "T2 result published" "$t2log" 2>/dev/null && t2=yes
    rm -f "$t2log"
    sbcl_ms=-
    ratio=-
    if [ "$have_sbcl" = 1 ]; then
        sbcl_ms=$(min_ms "$SBCL" --non-interactive --no-userinit --no-sysinit --load "$bench_file")
        if [ "$torcl_ms" != FAIL ] && [ "$sbcl_ms" != FAIL ] && [ "$sbcl_ms" -gt 0 ]; then
            ratio=$(awk "BEGIN {printf \"%.1fx\", $torcl_ms / $sbcl_ms}")
        fi
    fi
    printf "%-10s %10s %10s %8s %5s\n" "$b" "$torcl_ms" "$sbcl_ms" "$ratio" "$t2"

    # Regression tripwire. Only runs under CHECK=1 so the plain report stays a
    # report; thresholds and their rationale live in thresholds.toml.
    if [ "${CHECK:-0}" = "1" ] && [ -f "$THRESHOLDS" ]; then
        max_ms=$(awk -v b="[$b]" '$0==b{f=1;next} /^\[/{f=0} f&&/^max_ms/{print $3;exit}' "$THRESHOLDS")
        want_t2=$(awk -v b="[$b]" '$0==b{f=1;next} /^\[/{f=0} f&&/^t2_required/{print $3;exit}' "$THRESHOLDS")
        if [ "$torcl_ms" = FAIL ]; then
            echo "  REGRESSION $b: benchmark FAILED to run" >&2
            status=1
        elif [ -n "$max_ms" ] && [ "$torcl_ms" -gt "$max_ms" ]; then
            echo "  REGRESSION $b: ${torcl_ms}ms exceeds max_ms=${max_ms}" >&2
            status=1
        fi
        if [ "$want_t2" = "true" ] && [ "$t2" != yes ]; then
            echo "  REGRESSION $b: no longer reaches T2 (t2_required)" >&2
            status=1
        fi
    fi
done

if [ "${CHECK:-0}" = "1" ]; then
    if [ "$status" -eq 0 ]; then
        echo "benchmarks: all within thresholds"
    else
        echo "benchmarks: THRESHOLD REGRESSION — see above" >&2
    fi
fi
exit "$status"
