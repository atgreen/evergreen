#!/usr/bin/env bash
# icl backend launcher for torcl.
#
# icl invokes a backend as:  <program> [args...] --eval "<init-form>"
# where <init-form> is icl's SBCL-flavoured slynk bootstrap and contains the
# TCP port icl picked (":port NNNNN :dont-close t"). torcl cannot run that form
# (it loads slynk via `asdf:load-system`, which does not pull in the torcl
# backend), so we IGNORE it and load torcl's own slynk copy, serving on the same
# port icl chose. icl then connects to that port exactly as it would to SBCL.
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DBG="$HOME/.cache/icl-torcl-debug.log"
mkdir -p "$(dirname "$DBG")" 2>/dev/null || true

{
  echo "=== $(date) launched ==="
  echo "argc=$#"
  i=0; for a in "$@"; do echo "arg[$i]=$a"; i=$((i+1)); done
} >> "$DBG" 2>&1

# Extract the port icl chose from the init form (last `:port N`).
PORT="$(printf '%s\n' "$@" | grep -oiE ':port +[0-9]+' | grep -oE '[0-9]+' | tail -1)"
[ -z "${PORT:-}" ] && PORT=4005
echo "extracted PORT=$PORT" >> "$DBG"

LOADER="$(mktemp)"
cat > "$LOADER" <<LISP
(dolist (f (list "slynk-match" "slynk-backend" "backend/torcl" "torcl-prelude"
                 "slynk-rpc" "slynk" "slynk-completion" "slynk-apropos"
                 "torcl-slynk-patch"))
  (load (format nil "${ROOT}/lib/slynk/~a.lisp" f)))
(funcall (find-symbol "INIT" (find-package "SLYNK")))
(funcall (find-symbol "CREATE-SERVER" (find-package "SLYNK"))
         :port ${PORT} :dont-close t)
LISP

echo "starting torcl on port $PORT (loader $LOADER)" >> "$DBG"
# Run torcl (blocks in create-server); tee its output to the debug log and to
# icl's captured stdout. No `exec` so the wrapper stays the pipeline's parent.
"${ROOT}/target/x86_64-unknown-linux-musl/release/torcl" --no-init --load "$LOADER" 2>&1 | tee -a "$DBG"
echo "=== torcl exited rc=${PIPESTATUS[0]} ===" >> "$DBG"
