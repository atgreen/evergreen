#!/bin/sh
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
"$repo/scripts/install-egcl-forks" --dry-run |
    grep -Fx 'ocicl install git+https://github.com/atgreen/usocket@egcl-support'
"$repo/scripts/install-egcl-forks" --dry-run |
    grep -Fx 'ocicl install git+https://github.com/atgreen/atomics@egcl'
