#!/bin/sh
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
"$repo/scripts/install-egcl-forks" --dry-run |
    grep -Fx 'ocicl install git+https://github.com/atgreen/usocket@egcl-support'
"$repo/scripts/install-egcl-forks" --dry-run |
    grep -Fx 'ocicl install git+https://github.com/atgreen/atomics@egcl'
"$repo/scripts/install-egcl-forks" --dry-run |
    grep -Fx 'ocicl install git+https://github.com/atgreen/quicklisp-client@egcl'
for port in ironclad@egcl pure-tls@egcl chipz@egcl archive@egcl zip@egcl unix-opts@egcl with-user-abort@egcl local-time@egcl parachute@egcl; do
    "$repo/scripts/install-egcl-forks" --dry-run |
        grep -Fx "ocicl install git+https://github.com/atgreen/$port"
done
