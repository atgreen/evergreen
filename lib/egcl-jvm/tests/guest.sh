#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
: "${JAVA_HOME:=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
export JAVA_HOME
: "${EGCL_JVM_BIN:=$PWD/target/x86_64-unknown-linux-gnu/debug/egcl}"
EGCL_JVM_PROBE_DIR=$(mktemp -d /tmp/egcl-jvm-guest.XXXXXX)
export EGCL_JVM_PROBE_DIR
printf 'Guest test artifacts: %s\n' "$EGCL_JVM_PROBE_DIR"
"$JAVA_HOME/bin/javac" -d "$EGCL_JVM_PROBE_DIR" tools/jvm-probe/Coexistence.java
cc -shared -fPIC -Wall -Wextra -Werror \
  -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
  tools/jvm-probe/bridge.c -L"$JAVA_HOME/lib/server" \
  -Wl,-rpath,"$JAVA_HOME/lib/server" -ljvm -lpthread \
  -o "$EGCL_JVM_PROBE_DIR/libprobe.so"
log="$EGCL_JVM_PROBE_DIR/guest.log"
scripts/egcl-limited.sh env EGCL_JVM_PROBE_ORDER=jvm-first \
  LD_PRELOAD="$JAVA_HOME/lib/libjsig.so:$EGCL_JVM_PROBE_DIR/libprobe.so" \
  "$EGCL_JVM_BIN" --no-init --load lib/egcl-jvm/tests/guest.lisp >"$log" 2>&1
cat "$log"
grep -qx JVM-GUEST-PASS "$log"
# Missing chaining must produce a diagnosed refusal, not an accepted launch or crash.
set +e
scripts/egcl-limited.sh env EGCL_JVM_PROBE_ORDER=jvm-first \
  LD_PRELOAD="$EGCL_JVM_PROBE_DIR/libprobe.so" \
  "$EGCL_JVM_BIN" --no-init --eval '(quit)' >"$EGCL_JVM_PROBE_DIR/refusal.log" 2>&1
status=$?
set -e
cat "$EGCL_JVM_PROBE_DIR/refusal.log"
test "$status" -eq 1
grep -q 'preload the JDK libjsig.so' "$EGCL_JVM_PROBE_DIR/refusal.log"
printf 'JVM-SIGNAL-REFUSAL-PASS\n'
