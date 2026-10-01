#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
set -euo pipefail
umask 077
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
: "${SBCL_BIN:=sbcl}"
: "${ANDROID_BUILD_TOOLS:?Set ANDROID_BUILD_TOOLS for independent APK validation}"
export EGCL_APK_TEST_DIR
EGCL_APK_TEST_DIR=$(mktemp -d /tmp/egcl-apk-test.XXXXXX)
export EGCL_APK_TEST_PROJECT="$repo/examples/android-egl"
echo "APK test artifacts: $EGCL_APK_TEST_DIR"
cd "$repo/lib/egcl-apk"
EGCL_MEM_MAX="${EGCL_MEM_MAX:-8G}" EGCL_TIMEOUT="${EGCL_TIMEOUT:-1200}" \
  "$repo/scripts/egcl-limited.sh" "$SBCL_BIN" --noinform --no-userinit --no-sysinit --non-interactive --load tests/run.lisp >"$EGCL_APK_TEST_DIR/lisp.log" 2>&1
grep -qx APK-UNIT-OK "$EGCL_APK_TEST_DIR/lisp.log"
grep -qx APK-SIGNING-OK "$EGCL_APK_TEST_DIR/lisp.log"
python3 "$repo/lib/egcl-apk/tests/verify.py" "$EGCL_APK_TEST_DIR" "$ANDROID_BUILD_TOOLS"
echo 'NATIVE-APK-PASS'
