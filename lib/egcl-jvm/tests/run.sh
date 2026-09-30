#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

set -euo pipefail
cd "$(dirname "$0")/../../.."
: "${JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
export JAVA_HOME
: "${EGCL_JVM_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/egcl}"
make -C lib/egcl-jvm all build/tests/JvmFixture.class JAVA_HOME="$JAVA_HOME"
log=$(mktemp /tmp/egcl-jvm-api.XXXXXX.log)
printf 'Native JVM test log: %s\n' "$log"
"$JAVA_HOME/bin/java" -version
printf 'GC stress=%s poison=%s verify=%s\n' "${EGCL_GC_STRESS:-unset}" "${EGCL_GC_POISON:-unset}" "${EGCL_GC_VERIFY:-unset}"
set +e
EGCL_TIMEOUT="${EGCL_TIMEOUT:-180}" scripts/egcl-limited.sh \
  "$EGCL_JVM_BIN" --no-init --load lib/egcl-jvm/tests/api.lisp >"$log" 2>&1
status=$?
set -e
cat "$log"
if (( status != 0 )); then exit "$status"; fi
# A load error or premature exit must never count as success.
grep -qx JVM-API-PASS "$log"
if grep -Eq 'WARNING.*JNI|FATAL ERROR|WARNING in native method' "$log"; then exit 1; fi

startup_log=$(mktemp /tmp/egcl-jvm-startup.XXXXXX.log)
EGCL_TIMEOUT="${EGCL_TIMEOUT:-180}" scripts/egcl-limited.sh \
  "$EGCL_JVM_BIN" --no-init --load lib/egcl-jvm/tests/startup-error.lisp >"$startup_log" 2>&1
cat "$startup_log"
grep -qx JVM-STARTUP-ERROR-PASS "$startup_log"

for scenario in ergonomic package-conflict; do
  scenario_log=$(mktemp "/tmp/egcl-java-${scenario}.XXXXXX.log")
  printf 'Java %s test log: %s\n' "$scenario" "$scenario_log"
  EGCL_TIMEOUT="${EGCL_TIMEOUT:-180}" scripts/egcl-limited.sh \
    "$EGCL_JVM_BIN" --no-init --load "lib/egcl-jvm/tests/${scenario}.lisp" >"$scenario_log" 2>&1
  cat "$scenario_log"
  case "$scenario" in
    ergonomic) grep -qx JAVA-ERGONOMIC-PASS "$scenario_log" ;;
    package-conflict) grep -qx JAVA-PACKAGE-CONFLICT-PASS "$scenario_log" ;;
  esac
  if grep -Eq 'WARNING.*JNI|FATAL ERROR|WARNING in native method' "$scenario_log"; then exit 1; fi
done
