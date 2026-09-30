#!/usr/bin/env bash
# ANSI conformance gate (bliss-30be): run every ENABLED ansi-test chapter and
# fail unless each passes 100%.
#
# Gate policy (from the cl-amiga crib sheet): NO expected-failures file — a
# chapter is either in ENABLED_CHAPTERS and passes every test, or it is not
# listed. Enabling a newly-clean chapter is a one-line edit below.
#
# Usage:
#   scripts/ansi-gate.sh                # run all enabled chapters
#   scripts/ansi-gate.sh cons symbols   # run just these (still 100%-gated)
#
# Environment:
#   ANSI_TEST_DIR   ansi-test checkout (default: ~/git/ansi-test)
#   EGCL_BIN       egcl binary (default: the musl debug egcl)
#   EGCL_TIMEOUT   per-chapter wall-clock seconds (default 2400)
#
# Each chapter runs in a fresh egcl under scripts/egcl-limited.sh (memory
# cap; see AGENTS.md). Stale *.fasl files are cleared first: compile-and-load
# keys on mtime, and fasls compiled under older egcl semantics silently
# mis-load (bliss-89lj). A run with no parseable tally is ERRORED, never ok.

set -u

# Chapters currently at 100%. Add a chapter here once it passes completely.
# hash-tables added 2026-09-19: measured ok (158/158).
# characters added 2026-09-19: ok (259/259) once bliss-l3d7 (non-ASCII symbol
# names stored as Latin-1 mojibake) was fixed -- that was its only failure.
# strings added 2026-09-19: ok (509/509) once bliss-7kmw (STRING modelled as a
# specialized array type rather than a union) was fixed -- that one lattice
# change cleared all 7 of its failures.
# Remaining survey from that day, for whoever enables the next one:
# symbols 1135/10 (one is bliss-0dtf, the ratio-literal reader bug),
# iteration 794/49, types-and-classes 565/58.
ENABLED_CHAPTERS=(cons data-and-control-flow hash-tables characters strings)

ANSI_TEST_DIR="${ANSI_TEST_DIR:-$HOME/git/ansi-test}"
EGCL_BIN="${EGCL_BIN:-target/x86_64-unknown-linux-musl/debug/egcl}"
export EGCL_TIMEOUT="${EGCL_TIMEOUT:-2400}"
export EGCL_MEM_MAX="${EGCL_MEM_MAX:-8G}"

cd "$(dirname "$0")/.."

if [ ! -d "$ANSI_TEST_DIR" ]; then
    echo "ansi-gate: ERROR: ansi-test checkout not found at $ANSI_TEST_DIR" >&2
    exit 2
fi
if [ ! -x "$EGCL_BIN" ]; then
    echo "ansi-gate: ERROR: egcl binary not found at $EGCL_BIN" >&2
    echo "ansi-gate: build it with: cargo test --workspace --no-run" >&2
    echo "ansi-gate: (NOT 'cargo build' — see the profile note below / bliss-em8x)" >&2
    exit 2
fi

# Which binary is this, and is it the fast one?
#
# The underlying trap is FIXED (bliss-em8x): profile.dev now gives egcl-rt,
# egcl-stdlib and egcl-compiler opt-level 2 to match profile.test, so `cargo
# build` and `cargo test --no-run` produce binaries of the same speed (measured
# 40 ms each on the same benchmark that used to split 322 ms vs 40 ms) and it no
# longer matters which ran last.
#
# This check is kept as a BACKSTOP against that config regressing -- drop those
# profile.dev.package entries and the unoptimized build reappears at ~100MB,
# which this still catches. It is NOT a live speed test: post-fix the two builds
# are 110.7MB and 112.3MB and both are fast, so size no longer tracks opt-level
# among healthy binaries. It only distinguishes "someone lost the profile
# overrides" from "normal".
#
# The size split is a HEURISTIC, not a guarantee.
#
# This REFUSES rather than warns (bliss-wzb3). It was advisory, and advice at
# this spot does not work: the banner scrolls past, and a run on the slow
# binary yields a number that reads like a real result while being ~9x off. It
# has already produced one fictitious regression (bliss-em8x) and one wasted
# 2400s timeout. A chapter tally from the slow binary is not wrong so much as
# uninterpretable, and the gate should not emit uninterpretable numbers.
#
# Escape hatch for a deliberate correctness-only run, where speed is irrelevant:
#   EGCL_ALLOW_SLOW_BIN=1 scripts/ansi-gate.sh <chapter>
egcl_bin_size="$(stat -c%s "$EGCL_BIN" 2>/dev/null || echo 0)"
echo "ansi-gate: binary $EGCL_BIN (${egcl_bin_size} bytes)"
if [ "$egcl_bin_size" -gt 0 ] && [ "$egcl_bin_size" -lt 106000000 ]; then
    if [ "${EGCL_ALLOW_SLOW_BIN:-0}" = 1 ]; then
        echo "ansi-gate: WARNING: slow-looking binary, continuing because"
        echo "ansi-gate:   EGCL_ALLOW_SLOW_BIN=1. Pass/fail tallies are still"
        echo "ansi-gate:   meaningful; TIMINGS AND TIMEOUTS ARE NOT (bliss-em8x)."
    else
        echo "ansi-gate: ERROR: this looks like a 'cargo build' binary, whose" >&2
        echo "ansi-gate:   dependencies are unoptimized and which has measured" >&2
        echo "ansi-gate:   ~9x slower than a 'cargo test --workspace --no-run'" >&2
        echo "ansi-gate:   build. Timings are not comparable and a chapter can" >&2
        echo "ansi-gate:   time out that would otherwise pass (bliss-em8x)." >&2
        echo "ansi-gate:" >&2
        echo "ansi-gate:   Rebuild the fast binary:" >&2
        echo "ansi-gate:     cargo test --workspace --no-run" >&2
        echo "ansi-gate:" >&2
        echo "ansi-gate:   NOTE: a warning-clean 'cargo build --workspace' check" >&2
        echo "ansi-gate:   overwrites this same path and puts you right back" >&2
        echo "ansi-gate:   here. Use 'cargo check --workspace' for that (it does" >&2
        echo "ansi-gate:   not write the binary) -- see bliss-wzb3." >&2
        echo "ansi-gate:" >&2
        echo "ansi-gate:   To run anyway (correctness only, timings void):" >&2
        echo "ansi-gate:     EGCL_ALLOW_SLOW_BIN=1 $0 $*" >&2
        exit 2
    fi
fi

chapters=("${ENABLED_CHAPTERS[@]}")
if [ "$#" -gt 0 ]; then
    chapters=("$@")
fi

overall=0
for chapter in "${chapters[@]}"; do
    if [ ! -f "$ANSI_TEST_DIR/$chapter/load.lsp" ]; then
        echo "ansi-gate: $chapter: ERROR (no $chapter/load.lsp in $ANSI_TEST_DIR)"
        overall=1
        continue
    fi
    # Stale-fasl hazard (bliss-89lj): always compile fresh.
    find "$ANSI_TEST_DIR" -name '*.fasl' -delete

    log="$(mktemp "${TMPDIR:-/tmp}/ansi-gate-$chapter.XXXXXX.log")"
    # egcl rejects mixing --eval with --load, so parameterize via a tiny
    # generated driver file instead.
    driver="$(mktemp "${TMPDIR:-/tmp}/ansi-gate-$chapter.XXXXXX.lisp")"
    cat >"$driver" <<EOF
(defparameter cl-user::*ansi-chapter* "$chapter")
(defparameter cl-user::*ansi-test-root* "$ANSI_TEST_DIR/")
(load "$(pwd)/scripts/ansi-chapter.lisp")
EOF
    # Record BOTH wall and CPU time (bliss-u64o). Wall time alone is not
    # interpretable: if the host suspends mid-chapter, wall time inflates while
    # no work happens, which looks exactly like a hang or a regression. It also
    # means the EGCL_TIMEOUT cap -- which is wall-clock -- can be consumed, or
    # fail to fire, for reasons that have nothing to do with the code. A wall
    # time far above the CPU time is the signature of that, not of slow code.
    chapter_t0="$(date +%s)"
    # /proc/$$/stat, NOT /proc/self/stat: command substitution runs in a
    # SUBSHELL, so `self` names that short-lived subshell -- which has reaped
    # no children and always reports 0. `$$` stays the main shell even inside
    # $( ). Fields 16/17 are cutime/cstime, the CPU of reaped children; the
    # systemd scope in egcl-limited.sh does not hide it (verified: 81 ticks
    # accounted both with and without the scope).
    chapter_cpu_before="$(awk '{print $16+$17}' "/proc/$$/stat" 2>/dev/null)"
    scripts/egcl-limited.sh "$EGCL_BIN" --no-init --load "$driver" >"$log" 2>&1
    status=$?
    chapter_cpu_after="$(awk '{print $16+$17}' "/proc/$$/stat" 2>/dev/null)"
    chapter_wall=$(( "$(date +%s)" - chapter_t0 ))
    chapter_cpu="?"
    if [ -n "$chapter_cpu_before" ] && [ -n "$chapter_cpu_after" ]; then
        ticks="$(getconf CLK_TCK 2>/dev/null || echo 100)"
        chapter_cpu=$(( (chapter_cpu_after - chapter_cpu_before) / ticks ))
    fi
    timing="${chapter_wall}s wall"
    if [ "$chapter_cpu" != "?" ]; then
        timing="$timing / ${chapter_cpu}s cpu"
        # >2x wall vs cpu with a multi-minute run means the chapter spent most
        # of its wall time not running: host suspend, or heavy contention.
        if [ "$chapter_wall" -gt 120 ] && \
           [ "$chapter_cpu" -gt 0 ] && \
           [ "$chapter_wall" -gt $(( chapter_cpu * 2 )) ]; then
            timing="$timing -- WALL >> CPU, elapsed time not meaningful (bliss-u64o)"
        fi
    fi
    rm -f "$driver"

    passed="$(sed -n 's/^passed: \([0-9]\+\)$/\1/p' "$log" | tail -1)"
    failed="$(sed -n 's/^failed: \([0-9]\+\)$/\1/p' "$log" | tail -1)"

    if [ -z "$passed" ] || [ -z "$failed" ]; then
        # No tally = the run died (crash, timeout, load error) — ERRORED.
        echo "ansi-gate: $chapter: ERRORED (exit $status, no tally; $timing; log: $log)"
        if [ "$status" = 124 ]; then
            echo "ansi-gate:   exit 124 = wall-clock timeout after ${EGCL_TIMEOUT}s."
            echo "ansi-gate:   Before treating this as a code problem, check the"
            echo "ansi-gate:   binary note above: an unoptimized-dependency build"
            echo "ansi-gate:   is ~9x slower and times out on chapters that pass"
            echo "ansi-gate:   otherwise (bliss-em8x)."
        fi
        tail -5 "$log" | sed 's/^/ansi-gate:   /'
        overall=1
    elif [ "$failed" -ne 0 ] || [ "$passed" -eq 0 ]; then
        echo "ansi-gate: $chapter: FAILED ($passed passed, $failed failed; $timing; log: $log)"
        overall=1
    else
        echo "ansi-gate: $chapter: ok ($passed/$passed; $timing)"
        rm -f "$log"
    fi
done

exit $overall
