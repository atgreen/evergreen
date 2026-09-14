#!/usr/bin/env bash
# Tier-differential harness (bliss-19tm).
#
# Runs one Lisp corpus under BLISS_FORCE_TIER=interp|t0|t1|t2 and requires
# byte-identical output from all four. The tree-walker (`interp`) is the
# oracle; t0/t1/t2 are the bytecode, baseline-native, and optimising-native
# tiers. Because BLISS_FORCE_TIER pins tier selection process-wide, ANY Lisp
# file works as a corpus unmodified — growing test coverage grows tier
# coverage for free.
#
# The corpus ends with a CHECKSUM line over every reported value and a
# CORPUS-DONE sentinel, so a truncated, crashed, or silently-shortened run
# cannot pass the diff.
#
# Usage:
#   scripts/tier-diff.sh                          # default corpus
#   scripts/tier-diff.sh path/to/corpus.lisp      # any Lisp file
#   BLISS_GC_STRESS=1 scripts/tier-diff.sh        # under forced collection
#
# Environment:
#   BLISS_BIN    bliss binary (default: the musl debug bliss-cli)
#   TIERS        tiers to compare (default: "interp t0 t1 t2")
#   TIER_STRESS  1 = also force tier TRANSITIONS, not just tier states
#
# About TIER_STRESS: BLISS_FORCE_TIER pins which tier code ENDS UP in, but a
# corpus can sit entirely in that tier and never exercise the crossings between
# them. Measured on this corpus, the default run performs ZERO OSR native loop
# entries (bliss-ext:profile-report "OSR: 0 native loop entries") — so the
# T1->T2 back-edge transfer is completely uncovered. That is precisely the path
# bliss-kqdr corrupted: it handed T2 code a T1-sized frame, and no corpus run
# could see it. TIER_STRESS=1 lowers the back-edge and OSR thresholds so short
# corpus loops cross them.
#
# Note the invocation thresholds are deliberately NOT set here: under
# BLISS_FORCE_TIER the T0->T1 and T1->T2 invocation thresholds are already
# forced to 1, so setting them would be a no-op. Only the back-edge/OSR
# thresholds still bite.
#
# Each tier runs under scripts/bliss-limited.sh (memory cap; see AGENTS.md).

set -u

cd "$(dirname "$0")/.."

CORPUS="${1:-tests/differential/tier-corpus.lisp}"
BLISS_BIN="${BLISS_BIN:-target/x86_64-unknown-linux-musl/debug/bliss-cli}"
TIERS="${TIERS:-interp t0 t1 t2}"

if [ ! -x "$BLISS_BIN" ]; then
    echo "tier-diff: ERROR: bliss binary not found at $BLISS_BIN" >&2
    echo "tier-diff: build it with: cargo build -p bliss-cli --bin bliss-cli" >&2
    exit 2
fi
if [ ! -f "$CORPUS" ]; then
    echo "tier-diff: ERROR: corpus not found at $CORPUS" >&2
    exit 2
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/tier-diff.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# shellcheck disable=SC2086
set -- $TIERS
oracle="$1"
status=0

for tier in $TIERS; do
    # The oracle runs unstressed: it is the pure tree-walker and has no tiers to
    # cross, so stressing it would only slow the run down.
    if [ "${TIER_STRESS:-0}" = "1" ] && [ "$tier" != "$oracle" ]; then
        BLISS_FORCE_TIER="$tier" \
        BLISS_T1_T2_BACKEDGE_THRESHOLD=5 \
        BLISS_OSR_THRESHOLD=10 \
        BLISS_ANON_OSR_THRESHOLD=10 \
            scripts/bliss-limited.sh "$BLISS_BIN" \
            --no-init --load "$CORPUS" >"$work/$tier.raw" 2>&1
    else
        BLISS_FORCE_TIER="$tier" scripts/bliss-limited.sh "$BLISS_BIN" \
            --no-init --load "$CORPUS" >"$work/$tier.raw" 2>&1
    fi
    rc=$?
    # Drop "; ..." progress/banner lines — timing noise, not results.
    grep -v '^; ' "$work/$tier.raw" >"$work/$tier.out"

    if [ "$rc" -ne 0 ]; then
        echo "tier-diff: $tier: ERRORED (exit $rc)"
        status=1
    fi
    if ! grep -q '^CORPUS-DONE' "$work/$tier.out"; then
        echo "tier-diff: $tier: INCOMPLETE (no CORPUS-DONE sentinel — crashed or truncated)"
        status=1
    fi
done

for tier in $TIERS; do
    [ "$tier" = "$oracle" ] && continue
    if diff -u "$work/$oracle.out" "$work/$tier.out" >"$work/$tier.diff"; then
        echo "tier-diff: $oracle vs $tier: ok"
    else
        echo "tier-diff: $oracle vs $tier: MISMATCH"
        sed -n '1,40p' "$work/$tier.diff"
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    checksum=$(grep '^:CHECKSUM' "$work/$oracle.out" | head -1)
    echo "tier-diff: all tiers identical ($TIERS) — $checksum"
fi
exit "$status"
