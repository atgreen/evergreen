#!/usr/bin/env bash
# SessionStart orientation banner for TorCL.
# Injected into agent context by the SessionStart hook in .claude/settings.json.
# Deterministically reminds the agent of the AGENTS.md startup step so it is
# never skipped (mirrors how `bd prime` is wired). Reads current_stage from
# spec/stages.json so the banner auto-updates as the build advances.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stages="$repo_root/spec/stages.json"

stage_line="(spec/stages.json not found)"
if [[ -f "$stages" ]]; then
  stage_line="$(python3 - "$stages" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
cur = d.get("current_stage")
name = next((s.get("name") for s in d.get("stages", []) if s.get("id") == cur), "?")
gate = next((s.get("gate") for s in d.get("stages", []) if s.get("id") == cur), "")
print(f"current_stage: {cur} ({name})")
if gate:
    print(f"  gate: {gate}")
PY
)"
fi

cat <<EOF
=== TorCL session orientation (AGENTS.md startup step) ===
$stage_line

Before starting work:
  - Read spec/INDEX.md, spec/conventions.md, spec/stages.json for orientation;
    pull individual spec chapters on demand rather than reading all of spec/.
  - Only MUST requirements at or below current_stage are gated (see conventions.md).

Architecture principle: the interpreter (crates/torcl/src/cli.rs) MUST NOT
duplicate stdlib behaviour. Wire builtins to crates/torcl-stdlib; extend stdlib
rather than growing cli.rs.
==========================================================
EOF
