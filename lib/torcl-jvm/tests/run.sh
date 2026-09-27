#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
: "${JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
export JAVA_HOME
: "${TORCL_JVM_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/torcl}"
make -C lib/torcl-jvm all build/tests/JvmFixture.class JAVA_HOME="$JAVA_HOME"
log=$(mktemp /tmp/torcl-jvm-api.XXXXXX.log)
printf 'Native JVM test log: %s\n' "$log"
"$JAVA_HOME/bin/java" -version
printf 'GC stress=%s poison=%s verify=%s\n' "${TORCL_GC_STRESS:-unset}" "${TORCL_GC_POISON:-unset}" "${TORCL_GC_VERIFY:-unset}"
set +e
TORCL_TIMEOUT="${TORCL_TIMEOUT:-180}" scripts/torcl-limited.sh \
  "$TORCL_JVM_BIN" --no-init --load lib/torcl-jvm/tests/api.lisp >"$log" 2>&1
status=$?
set -e
cat "$log"
if (( status != 0 )); then exit "$status"; fi
# A load error or premature exit must never count as success.
grep -qx JVM-API-PASS "$log"
if grep -Eq 'WARNING.*JNI|FATAL ERROR|WARNING in native method' "$log"; then exit 1; fi

startup_log=$(mktemp /tmp/torcl-jvm-startup.XXXXXX.log)
TORCL_TIMEOUT="${TORCL_TIMEOUT:-180}" scripts/torcl-limited.sh \
  "$TORCL_JVM_BIN" --no-init --load lib/torcl-jvm/tests/startup-error.lisp >"$startup_log" 2>&1
cat "$startup_log"
grep -qx JVM-STARTUP-ERROR-PASS "$startup_log"
