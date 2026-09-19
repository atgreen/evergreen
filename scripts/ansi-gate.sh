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
#   BLISS_BIN       bliss binary (default: the musl debug bliss-cli)
#   BLISS_TIMEOUT   per-chapter wall-clock seconds (default 2400)
#
# Each chapter runs in a fresh bliss under scripts/bliss-limited.sh (memory
# cap; see AGENTS.md). Stale *.fasl files are cleared first: compile-and-load
# keys on mtime, and fasls compiled under older bliss semantics silently
# mis-load (bliss-89lj). A run with no parseable tally is ERRORED, never ok.

set -u

# Chapters currently at 100%. Add a chapter here once it passes completely.
ENABLED_CHAPTERS=(cons data-and-control-flow)

ANSI_TEST_DIR="${ANSI_TEST_DIR:-$HOME/git/ansi-test}"
BLISS_BIN="${BLISS_BIN:-target/x86_64-unknown-linux-musl/debug/bliss-cli}"
export BLISS_TIMEOUT="${BLISS_TIMEOUT:-2400}"
export BLISS_MEM_MAX="${BLISS_MEM_MAX:-8G}"

cd "$(dirname "$0")/.."

if [ ! -d "$ANSI_TEST_DIR" ]; then
    echo "ansi-gate: ERROR: ansi-test checkout not found at $ANSI_TEST_DIR" >&2
    exit 2
fi
if [ ! -x "$BLISS_BIN" ]; then
    echo "ansi-gate: ERROR: bliss binary not found at $BLISS_BIN" >&2
    echo "ansi-gate: build it with: cargo test --workspace --no-run" >&2
    echo "ansi-gate: (NOT 'cargo build' — see the profile note below / bliss-em8x)" >&2
    exit 2
fi

# Which binary is this, and is it the fast one? Cargo.toml gives profile.test
# opt-level 3 for ALL crates but profile.dev opt-level 2 for the bliss-cli
# package ONLY, so `cargo build` leaves bliss-rt/bliss-stdlib/bliss-compiler —
# where the hot code lives — at opt-level 0. Both write this same path, so the
# binary can be ~9x slower with nothing to show for it, which is enough to turn
# a passing chapter into a wall-clock timeout (bliss-em8x).
#
# The size split is a HEURISTIC, not a guarantee: measured ~112MB for the
# cargo-test build and ~100MB for the cargo-build one. It is advisory only —
# the gate still runs either way.
bliss_bin_size="$(stat -c%s "$BLISS_BIN" 2>/dev/null || echo 0)"
echo "ansi-gate: binary $BLISS_BIN (${bliss_bin_size} bytes)"
if [ "$bliss_bin_size" -gt 0 ] && [ "$bliss_bin_size" -lt 106000000 ]; then
    echo "ansi-gate: WARNING: this looks like a 'cargo build' binary, whose"
    echo "ansi-gate:   dependencies are unoptimized and which has measured ~9x"
    echo "ansi-gate:   slower than a 'cargo test --workspace --no-run' build."
    echo "ansi-gate:   Timings from it are not comparable, and a chapter may"
    echo "ansi-gate:   time out that would otherwise pass. See bliss-em8x."
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
    # bliss-cli rejects mixing --eval with --load, so parameterize via a tiny
    # generated driver file instead.
    driver="$(mktemp "${TMPDIR:-/tmp}/ansi-gate-$chapter.XXXXXX.lisp")"
    cat >"$driver" <<EOF
(defparameter cl-user::*ansi-chapter* "$chapter")
(defparameter cl-user::*ansi-test-root* "$ANSI_TEST_DIR/")
(load "$(pwd)/scripts/ansi-chapter.lisp")
EOF
    scripts/bliss-limited.sh "$BLISS_BIN" --no-init --load "$driver" >"$log" 2>&1
    status=$?
    rm -f "$driver"

    passed="$(sed -n 's/^passed: \([0-9]\+\)$/\1/p' "$log" | tail -1)"
    failed="$(sed -n 's/^failed: \([0-9]\+\)$/\1/p' "$log" | tail -1)"

    if [ -z "$passed" ] || [ -z "$failed" ]; then
        # No tally = the run died (crash, timeout, load error) — ERRORED.
        echo "ansi-gate: $chapter: ERRORED (exit $status, no tally; log: $log)"
        if [ "$status" = 124 ]; then
            echo "ansi-gate:   exit 124 = wall-clock timeout after ${BLISS_TIMEOUT}s."
            echo "ansi-gate:   Before treating this as a code problem, check the"
            echo "ansi-gate:   binary note above: an unoptimized-dependency build"
            echo "ansi-gate:   is ~9x slower and times out on chapters that pass"
            echo "ansi-gate:   otherwise (bliss-em8x)."
        fi
        tail -5 "$log" | sed 's/^/ansi-gate:   /'
        overall=1
    elif [ "$failed" -ne 0 ] || [ "$passed" -eq 0 ]; then
        echo "ansi-gate: $chapter: FAILED ($passed passed, $failed failed; log: $log)"
        overall=1
    else
        echo "ansi-gate: $chapter: ok ($passed/$passed)"
        rm -f "$log"
    fi
done

exit $overall
