#!/usr/bin/env bash
# Launch bliss with slynk fully loaded, serving the SLIME protocol on $1 (default 4005).
# Usage:  lib/slynk/run-bliss-slynk.sh [PORT]
# Then:   icl --connect 127.0.0.1:PORT     (or M-x sly-connect)
#
# Run from the repo root. Requires target/release/bliss-cli built.
set -euo pipefail
PORT="${1:-4005}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

LOADER="$(mktemp)"
cat > "$LOADER" <<LISP
(dolist (f (list "slynk-match" "slynk-backend" "backend/bliss" "bliss-prelude"
                 "slynk-rpc" "slynk" "slynk-completion" "slynk-apropos"
                 "bliss-slynk-patch"))
  (load (format nil "$ROOT/lib/slynk/~a.lisp" f)))
(funcall (find-symbol "INIT" (find-package "SLYNK")))
(format t "Bliss+slynk listening on port ${PORT}~%")
(finish-output)
(funcall (find-symbol "CREATE-SERVER" (find-package "SLYNK"))
         :port ${PORT} :dont-close t)
LISP

exec target/release/bliss-cli --no-init --load "$LOADER"
