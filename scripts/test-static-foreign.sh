#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
# Opt-in archive-link/delivery regression. Builds its own isolated release binary.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/egcl-static-foreign.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$repo"
target=${EGCL_TEST_TARGET:-x86_64-unknown-linux-musl}
cc_tool=${EGCL_TEST_CC:-cc}
ar_tool=${EGCL_TEST_AR:-ar}
cat > "$scratch/demo.c" <<'C'
int egcl_archive_counter = 7;
int egcl_archive_add(int x, int y) { return x + y; }
C
# Separate member: whole-archive must retain its constructor even though no
# manifest export names a symbol in this object.
cat > "$scratch/init.c" <<'C'
extern int egcl_archive_counter;
__attribute__((constructor)) static void init(void) { egcl_archive_counter = 31; }
C
"$cc_tool" -fPIC -fno-stack-protector -c "$scratch/demo.c" -o "$scratch/demo.o"
"$cc_tool" -fPIC -fno-stack-protector -c "$scratch/init.c" -o "$scratch/init.o"
"$ar_tool" crs "$scratch/libdemo.a" "$scratch/demo.o" "$scratch/init.o"
cat > "$scratch/demo.foreign" <<'MANIFEST'
version = 1
library = egcl-archive-demo
alias = libegcl-archive-demo.so
archive = libdemo.a
symbol = egcl_archive_add
symbol = egcl_archive_counter
MANIFEST
EGCL_STATIC_FOREIGN_MANIFEST="$scratch/demo.foreign" \
  EGCL_MEM_MAX=8G EGCL_TIMEOUT=1800 scripts/egcl-limited.sh \
  cargo build --release --locked -p egcl --bin egcl --target "$target" \
  --target-dir "$scratch/target"
binary="$scratch/target/$target/release/egcl"
# Retain the original contract for the changed-configuration rejection below.
scripts/egcl-limited.sh "$binary" --runtime-info > "$scratch/with-archives.contract"
cat > "$scratch/check.lisp" <<'LISP'
(defun archive-add (library)
  (egcl-ffi:foreign-call
    (egcl-ffi:foreign-symbol-pointer "egcl_archive_add" library)
    :int '(:int :int) '(20 22)))
(defvar *archive-library* (egcl-ffi:load-foreign-library "libegcl-archive-demo.so"))
(assert (= 42 (archive-add *archive-library*)))
(assert (= 42 (egcl-ffi:foreign-call
  (egcl-ffi:foreign-symbol-pointer "egcl_archive_add") :int '(:int :int) '(20 22))))
(assert (= 31 (egcl-ffi:mem-ref
  (egcl-ffi:foreign-symbol-pointer "egcl_archive_counter" *archive-library*) :int)))
(egcl-ffi:mem-set 99
  (egcl-ffi:foreign-symbol-pointer "egcl_archive_counter" *archive-library*) :int)
(egcl-ffi:close-foreign-library *archive-library*)
(assert (handler-case
  (progn (egcl-ffi:foreign-symbol-pointer "egcl_archive_add" *archive-library*) nil)
  (error () t)))
(setf *archive-library* (egcl-ffi:load-foreign-library "egcl-archive-demo"))
(assert (= 42 (archive-add *archive-library*)))
(format t "STATIC-ARCHIVE-CALLS-OK~%")
LISP
scripts/egcl-limited.sh "$binary" --no-init --load "$scratch/check.lisp" \
  --eval "(egcl-ext:save-lisp-and-die \"$scratch/demo.core\")" > "$scratch/save.log" 2>&1
grep -q 'STATIC-ARCHIVE-CALLS-OK' "$scratch/save.log"
scripts/egcl-limited.sh "$binary" --no-init --image "$scratch/demo.core" \
  --eval '(assert (= 42 (archive-add *archive-library*)))' \
  --eval '(assert (= 31 (egcl-ffi:mem-ref (egcl-ffi:foreign-symbol-pointer "egcl_archive_counter" *archive-library*) :int)))' \
  --eval "(egcl-ext:save-lisp-and-die \"$scratch/app\" :executable t)" > "$scratch/restore.log" 2>&1
# Prove delivery uses linked providers after both original inputs are absent.
cp "$scratch/libdemo.a" "$scratch/archive.backup"
rm "$scratch/demo.foreign" "$scratch/libdemo.a" "$scratch/demo.o" "$scratch/init.o"
scripts/egcl-limited.sh "$scratch/app" --no-init \
  --eval '(assert (= 42 (archive-add *archive-library*)))' \
  --eval '(assert (= 31 (egcl-ffi:mem-ref (egcl-ffi:foreign-symbol-pointer "egcl_archive_counter" *archive-library*) :int)))' \
  --eval '(format t "STATIC-ARCHIVE-DELIVERY-OK~%")' > "$scratch/delivery.log" 2>&1
grep -q 'STATIC-ARCHIVE-DELIVERY-OK' "$scratch/delivery.log"
# A source rebuild must not silently substitute a different archive configuration.
mv "$scratch/archive.backup" "$scratch/libdemo.a"
cat > "$scratch/demo.foreign" <<'MANIFEST'
version = 1
library = egcl-archive-demo
alias = changed-library-alias
archive = libdemo.a
symbol = egcl_archive_add
symbol = egcl_archive_counter
MANIFEST
if EGCL_STATIC_FOREIGN_MANIFEST="$scratch/demo.foreign" \
  EGCL_RUNTIME_MANIFEST="$scratch/with-archives.contract" \
  EGCL_MEM_MAX=8G EGCL_TIMEOUT=1800 scripts/egcl-limited.sh \
  cargo build --release --locked -p egcl --bin egcl --target "$target" \
  --target-dir "$scratch/target" > "$scratch/mismatch.log" 2>&1; then
  echo "changed archive configuration incorrectly accepted" >&2
  exit 1
fi
grep -q 'native runtime build is incompatible with shake driver' "$scratch/mismatch.log"
cat "$scratch/save.log" "$scratch/restore.log" "$scratch/delivery.log"
