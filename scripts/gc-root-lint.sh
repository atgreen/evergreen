#!/usr/bin/env bash
# CI entry point for gc-root-lint (bliss-jaf): fails on unrooted-across-alloc
# candidates that are not in tools/gc-root-lint/baseline.txt.
# Bless intentional/false-positive findings with:
#   cargo run -p gc-root-lint -- --bless
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run --quiet -p gc-root-lint -- --check
