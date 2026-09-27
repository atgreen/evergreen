#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
: "${JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
export JAVA_HOME
: "${TORCL_JVM_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/torcl}"
TORCL_JVM_PROBE_DIR=$(mktemp -d /tmp/torcl-jvm-guest.XXXXXX)
export TORCL_JVM_PROBE_DIR
printf 'Guest test artifacts: %s\n' "$TORCL_JVM_PROBE_DIR"
"$JAVA_HOME/bin/javac" -d "$TORCL_JVM_PROBE_DIR" tools/jvm-probe/Coexistence.java
cc -shared -fPIC -Wall -Wextra -Werror \
  -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
  tools/jvm-probe/bridge.c -L"$JAVA_HOME/lib/server" \
  -Wl,-rpath,"$JAVA_HOME/lib/server" -ljvm -lpthread \
  -o "$TORCL_JVM_PROBE_DIR/libprobe.so"
log="$TORCL_JVM_PROBE_DIR/guest.log"
scripts/torcl-limited.sh env TORCL_JVM_PROBE_ORDER=jvm-first \
  LD_PRELOAD="$JAVA_HOME/lib/libjsig.so:$TORCL_JVM_PROBE_DIR/libprobe.so" \
  "$TORCL_JVM_BIN" --no-init --load lib/torcl-jvm/tests/guest.lisp >"$log" 2>&1
cat "$log"
grep -qx JVM-GUEST-PASS "$log"
# Missing chaining must produce a diagnosed refusal, not an accepted launch or crash.
set +e
scripts/torcl-limited.sh env TORCL_JVM_PROBE_ORDER=jvm-first \
  LD_PRELOAD="$TORCL_JVM_PROBE_DIR/libprobe.so" \
  "$TORCL_JVM_BIN" --no-init --eval '(quit)' >"$TORCL_JVM_PROBE_DIR/refusal.log" 2>&1
status=$?
set -e
cat "$TORCL_JVM_PROBE_DIR/refusal.log"
test "$status" -eq 1
grep -q 'preload the JDK libjsig.so' "$TORCL_JVM_PROBE_DIR/refusal.log"
printf 'JVM-SIGNAL-REFUSAL-PASS\n'
