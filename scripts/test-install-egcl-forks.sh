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
python3 - "$repo" <<'PY'
import json
from pathlib import Path
import subprocess
import sys
repo = Path(sys.argv[1])
commands = subprocess.check_output([repo / 'scripts/install-egcl-forks', '--dry-run'], text=True).splitlines()
for entry in json.loads((repo / 'tests/audited-forks/forks.json').read_text()):
    expected = f"ocicl install {entry['fork'].replace('https://', 'git+https://')}@{entry['branch']}"
    assert commands.count(expected) == 1, expected
print('All audited forks occur exactly once in installer output')
PY
