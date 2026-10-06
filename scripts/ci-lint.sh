#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# Independent checks must all report, even while another gate is red. Keep each
# command's output and make any failure fatal to the overall job (bliss-5jrek).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

failures=0
check() {
    local label=$1
    shift
    printf '\n=== %s ===\n' "$label"
    if "$@"; then
        printf 'PASS: %s\n' "$label"
    else
        local status=$?
        printf 'FAIL: %s (exit %s)\n' "$label" "$status"
        failures=$((failures + 1))
    fi
}

check 'coverage policy contracts' python3 scripts/test_spec_coverage.py
check 'spec coverage' python3 scripts/spec-coverage.py --gate --debt-baseline spec/coverage-debt.json
check 'strict Clippy' make clippy
check 'GC root lint' bash scripts/gc-root-lint.sh
check 'validation contracts' cargo test -p egcl --test spec_validation_infra ansi_expected_failures_and_ci_contracts

printf '\nLint gates: %s failed\n' "$failures"
if (( failures > 0 )); then
    exit 1
fi
