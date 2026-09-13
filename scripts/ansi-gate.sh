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
ENABLED_CHAPTERS=(cons)

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
    echo "ansi-gate: build it with: cargo build -p bliss-cli --bin bliss-cli" >&2
    exit 2
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
