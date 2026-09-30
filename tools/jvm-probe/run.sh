#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

# The probe intentionally runs each startup order in a disposable process.
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${EGCL_JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
: "${EGCL_PROBE_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/egcl}"
export EGCL_JVM_PROBE_DIR
EGCL_JVM_PROBE_DIR=$(mktemp -d /tmp/egcl-jvm-probe.XXXXXX)
printf 'Probe artifacts: %s\n' "$EGCL_JVM_PROBE_DIR"
printf 'Source revision: %s\n' "$(git rev-parse HEAD)"
sha256sum "$EGCL_PROBE_BIN"
printf 'GC stress=%s poison=%s verify=%s\n' "${EGCL_GC_STRESS:-unset}" "${EGCL_GC_POISON:-unset}" "${EGCL_GC_VERIFY:-unset}"
"$EGCL_JAVA_HOME/bin/java" -version
"$EGCL_JAVA_HOME/bin/javac" -d "$EGCL_JVM_PROBE_DIR" tools/jvm-probe/Coexistence.java
cc -shared -fPIC -Wall -Wextra -Werror \
  -I"$EGCL_JAVA_HOME/include" -I"$EGCL_JAVA_HOME/include/linux" \
  tools/jvm-probe/bridge.c -L"$EGCL_JAVA_HOME/lib/server" \
  -Wl,-rpath,"$EGCL_JAVA_HOME/lib/server" -ljvm -lpthread \
  -o "$EGCL_JVM_PROBE_DIR/libprobe.so"
result=0
for order in ${EGCL_JVM_PROBE_ORDERS:-lisp-first jvm-first}; do
  case "$order" in lisp-first|jvm-first) ;; *) echo "Invalid order: $order" >&2; exit 2;; esac
  printf 'Running %s\n' "$order"
  # Preload only inside the limited command, never into systemd-run or timeout.
  set +e
  EGCL_TIMEOUT="${EGCL_TIMEOUT:-90}" scripts/egcl-limited.sh env \
    EGCL_JVM_PROBE_ORDER="$order" \
    LD_PRELOAD="${EGCL_JVM_PROBE_JSIG:+$EGCL_JVM_PROBE_JSIG:}$EGCL_JVM_PROBE_DIR/libprobe.so" \
    "$EGCL_PROBE_BIN" --no-init --load tools/jvm-probe/probe.lisp \
    >"$EGCL_JVM_PROBE_DIR/$order.log" 2>&1
  status=$?
  set -e
  cat "$EGCL_JVM_PROBE_DIR/$order.log"
  printf '%s exit=%s\n' "$order" "$status"
  if (( status != 0 )) || ! grep -q '^PROBE-COMPLETE$' "$EGCL_JVM_PROBE_DIR/$order.log"; then
    result=1
  fi
done
exit "$result"
