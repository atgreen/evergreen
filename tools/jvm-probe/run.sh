#!/usr/bin/env bash
# The probe intentionally runs each startup order in a disposable process.
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${TORCL_JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
: "${TORCL_PROBE_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/torcl}"
export TORCL_JVM_PROBE_DIR
TORCL_JVM_PROBE_DIR=$(mktemp -d /tmp/torcl-jvm-probe.XXXXXX)
printf 'Probe artifacts: %s\n' "$TORCL_JVM_PROBE_DIR"
printf 'Source revision: %s\n' "$(git rev-parse HEAD)"
sha256sum "$TORCL_PROBE_BIN"
printf 'GC stress=%s poison=%s verify=%s\n' "${TORCL_GC_STRESS:-unset}" "${TORCL_GC_POISON:-unset}" "${TORCL_GC_VERIFY:-unset}"
"$TORCL_JAVA_HOME/bin/java" -version
"$TORCL_JAVA_HOME/bin/javac" -d "$TORCL_JVM_PROBE_DIR" tools/jvm-probe/Coexistence.java
cc -shared -fPIC -Wall -Wextra -Werror \
  -I"$TORCL_JAVA_HOME/include" -I"$TORCL_JAVA_HOME/include/linux" \
  tools/jvm-probe/bridge.c -L"$TORCL_JAVA_HOME/lib/server" \
  -Wl,-rpath,"$TORCL_JAVA_HOME/lib/server" -ljvm -lpthread \
  -o "$TORCL_JVM_PROBE_DIR/libprobe.so"
result=0
for order in ${TORCL_JVM_PROBE_ORDERS:-lisp-first jvm-first}; do
  case "$order" in lisp-first|jvm-first) ;; *) echo "Invalid order: $order" >&2; exit 2;; esac
  printf 'Running %s\n' "$order"
  # Preload only inside the limited command, never into systemd-run or timeout.
  set +e
  TORCL_TIMEOUT="${TORCL_TIMEOUT:-90}" scripts/torcl-limited.sh env \
    TORCL_JVM_PROBE_ORDER="$order" \
    LD_PRELOAD="$TORCL_JVM_PROBE_DIR/libprobe.so" \
    "$TORCL_PROBE_BIN" --no-init --load tools/jvm-probe/probe.lisp \
    >"$TORCL_JVM_PROBE_DIR/$order.log" 2>&1
  status=$?
  set -e
  cat "$TORCL_JVM_PROBE_DIR/$order.log"
  printf '%s exit=%s\n' "$order" "$status"
  if (( status != 0 )) || ! grep -q '^PROBE-COMPLETE$' "$TORCL_JVM_PROBE_DIR/$order.log"; then
    result=1
  fi
done
exit "$result"
