#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ocicl=$(command -v "${OCICL_BIN:-ocicl}")
egcl=$(realpath "${EGCL_BIN:-$repo/target/egcl}")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
mkdir -p "$repo/target"
work=$(mktemp -d "$repo/target/portability-forks.XXXXXX")
echo "Portability fork test artifacts: $work"
cp "$repo/tests/portability-forks/ocicl.csv" "$repo/tests/portability-forks/check.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_PORT_RUNTIME="$runtime" EGCL_PORT_CACHE="$work/cache/"
"$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp "$repo/tests/portability-forks/ocicl.csv" ocicl.csv
for phase in cold cached unix-opts parachute; do
    export EGCL_PORT_EXIT="$phase"
    expected=0
    case "$phase" in unix-opts) expected=23 ;; parachute) expected=24 ;; esac
    actual=0
    "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load check.lisp >"$phase.log" 2>&1 || actual=$?
    if [[ "$actual" != "$expected" ]]; then
        echo "$phase: expected exit $expected, got $actual (see $work/$phase.log)" >&2
        exit 1
    fi
    grep -Fxq 'PORTABILITY-FORKS-PASS' "$phase.log"
done
if grep -qi 'compiling file' cached.log; then
    echo 'Cached load unexpectedly compiled source' >&2
    exit 1
fi
python3 - <<'PY'
import tarfile, zipfile
with tarfile.open('roundtrip.tar') as archive:
    entry = archive.getmember('input.dat')
    assert entry.mode == 0o640
    assert archive.extractfile(entry).read() == b'port test'
with zipfile.ZipFile('roundtrip.zip') as archive:
    entry = archive.getinfo('input.dat')
    assert (entry.external_attr >> 16) & 0o777 == 0o640
    assert archive.read(entry) == b'port test'
PY
echo 'PORTABILITY-FORKS-PASS: pinned cold/cached loads, metadata, roundtrips, clock, interrupts and exit codes'
