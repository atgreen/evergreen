#!/usr/bin/env python3
"""spec-coverage.py — staged requirement traceability gate for Bliss.

Enumerates the normative requirements declared in ``spec/`` (both markdown
table rows of the form ``| R6.45 | ... | MUST |`` and prose entries of the
form ``**R10.01** ... MUST ...``) and checks which are cited by at least one
test under ``crates/*/tests/`` — tests reference a requirement by its R-id,
e.g. ``// Per R6.45, (require ...) delegates to ASDF``.

Why this exists: a spec-mandated capability that no phase ever turned into a
task is never tested *and* never implemented, so it sails straight past a
green build — "missing" is not the same as "failing". This tool makes those
holes visible (and, with ``--gate``, fatal).

STAGING: the build is incremental (see ``spec/stages.json``,
``spec/00-overview.md``, and ``spec/11-phasing-roadmap.md``). Each
requirement belongs to a stage — its explicit trailing ``[Sn]`` tag if
present, else the stage of the spec file it is defined in (per
``spec/stages.json``'s ``files`` map), else "unstaged". ``--gate`` only
requires MUST requirements *at or below the current stage* to be covered, so
the project can ship a working vertical slice before covering the whole
language. Requirements above the current stage (or unstaged) are reported but
do not fail the gate. Advance the current stage only when the stage's Gate
genuinely passes end-to-end through the real binary.

Usage:
    python3 scripts/spec-coverage.py            # human-readable staged report
    python3 scripts/spec-coverage.py --gate     # fail if an in-scope MUST is uncovered
    python3 scripts/spec-coverage.py --stage N  # override the current stage
    python3 scripts/spec-coverage.py --all      # gate the whole spec (legacy, stage-agnostic)
    python3 scripts/spec-coverage.py --repo DIR # repo root (default: cwd)

The current stage defaults to spec/stages.json's ``current_stage``, overridable
by ``--stage`` or the ``BLISS_STAGE`` env var. If stages.json is absent the tool
falls back to legacy behavior (every MUST is in scope).

Do NOT delete: this is the traceability tool invoked by bureau's verify gate.
Tests are expected to cite the requirements they exercise by R-id.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import defaultdict
from pathlib import Path

_RID_EXACT = re.compile(r"R\d+\.\d+")
_RID_PROSE = re.compile(r"^\*\*(R\d+\.\d+)\*\*")
_LEVEL = re.compile(r"\b(MUST(?:\s*/\s*SHOULD)?|SHOULD|MAY|REQUIRED|SHALL)\b",
                    re.IGNORECASE)
_STAGE_TAG_SUFFIX = re.compile(r"\[S(\d+)\]\s*$")
_SKIP_SPEC_FILES = {"12-glossary.md"}

# Requirement stage when neither an inline tag nor the file map assigns one.
UNSTAGED = None


def _split_blocks(text: str) -> list[str]:
    blocks: list[str] = []
    current: list[str] = []
    for raw in text.splitlines():
        line = raw.rstrip()
        if not line.strip():
            if current:
                blocks.append("\n".join(current))
                current = []
            continue
        current.append(line)
    if current:
        blocks.append("\n".join(current))
    return blocks


def _level_for_text(text: str) -> str | None:
    match = _LEVEL.search(text)
    return match.group(1).upper() if match else None


def _stage_for_text(text: str) -> int | None:
    match = _STAGE_TAG_SUFFIX.search(text.strip())
    return int(match.group(1)) if match else None


class Req:
    __slots__ = ("rid", "level", "file", "stage")

    def __init__(self, rid: str, level: str, file: str, stage: int | None):
        self.rid = rid
        self.level = level
        self.file = file
        self.stage = stage


def load_stages(spec_dir: Path) -> tuple[int | None, dict[str, int], list[dict]]:
    """Return (current_stage, {spec_filename: stage}, stage_defs).

    current_stage is None when stages.json is absent (legacy mode)."""
    path = spec_dir / "stages.json"
    if not path.is_file():
        return None, {}, []
    data = json.loads(path.read_text())
    files = {k: int(v) for k, v in data.get("files", {}).items()}
    return data.get("current_stage"), files, data.get("stages", [])


def parse_requirements(spec_dir: Path, file_stage: dict[str, int]) -> dict[str, Req]:
    """Return {req_id: Req} for every requirement, with its resolved stage."""
    reqs: dict[str, Req] = {}

    def record(rid: str, level: str, fname: str, text: str) -> None:
        if rid in reqs:
            return
        stage = _stage_for_text(text)
        if stage is None:
            stage = file_stage.get(fname, UNSTAGED)
        reqs[rid] = Req(rid, level, fname, stage)

    for md in sorted(spec_dir.rglob("*.md")):
        if md.name in _SKIP_SPEC_FILES:
            continue
        text = md.read_text(errors="replace")
        for raw in text.splitlines():
            line = raw.strip()
            if not line.startswith("|"):
                continue
            cells = [c.strip() for c in line.strip("|").split("|")]
            if len(cells) < 2 or not _RID_EXACT.fullmatch(cells[0]):
                continue
            body = " | ".join(cells[1:])
            level = _level_for_text(body) or cells[-1].upper()
            record(cells[0], level, md.name, cells[1])
        for block in _split_blocks(text):
            first = block.splitlines()[0].strip()
            match = _RID_PROSE.match(first)
            if not match:
                continue
            level = _level_for_text(block)
            if not level:
                continue
            record(match.group(1), level, md.name, block)
    return reqs


def parse_citations(repo: Path) -> set[str]:
    """Return the set of requirement ids cited by any test source file."""
    cited: set[str] = set()
    for rs in (repo / "crates").rglob("*.rs"):
        parts = set(rs.parts)
        if "tests" not in parts and not rs.name.startswith("test"):
            continue
        cited.update(_RID_EXACT.findall(rs.read_text(errors="replace")))
    return cited


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default=".", help="repo root (default: cwd)")
    ap.add_argument("--gate", action="store_true",
                    help="exit non-zero if an in-scope MUST requirement is uncovered")
    ap.add_argument("--stage", type=int, default=None,
                    help="override current stage (default: spec/stages.json or $BLISS_STAGE)")
    ap.add_argument("--all", action="store_true",
                    help="ignore staging; gate the entire spec (legacy behavior)")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    spec_dir = repo / "spec"
    if not spec_dir.is_dir():
        print(f"spec-coverage: no spec/ directory under {repo}", file=sys.stderr)
        return 2

    manifest_stage, file_stage, stage_defs = load_stages(spec_dir)
    # Resolve the current stage: --stage > $BLISS_STAGE > stages.json.
    current_stage: int | None
    if args.all:
        current_stage = None
    elif args.stage is not None:
        current_stage = args.stage
    elif os.environ.get("BLISS_STAGE"):
        current_stage = int(os.environ["BLISS_STAGE"])
    else:
        current_stage = manifest_stage

    reqs = parse_requirements(spec_dir, file_stage)
    cited = parse_citations(repo)
    if not reqs:
        print("spec-coverage: no requirements found in spec/", file=sys.stderr)
        return 2

    must = {r: q for r, q in reqs.items() if q.level.startswith("MUST")}

    def in_scope(q: Req) -> bool:
        # --all / legacy (no stages.json): every MUST is in scope.
        if current_stage is None:
            return True
        return q.stage is not None and q.stage <= current_stage

    in_scope_must = {r: q for r, q in must.items() if in_scope(q)}
    uncovered_scope = sorted(r for r in in_scope_must if r not in cited)
    covered_scope = len(in_scope_must) - len(uncovered_scope)

    deferred = [q for r, q in must.items()
                if not in_scope(q) and q.stage is not None]
    unstaged = [q for r, q in must.items() if q.stage is UNSTAGED]

    stage_name = ""
    if current_stage is not None:
        for s in stage_defs:
            if s.get("id") == current_stage:
                stage_name = f" ({s.get('name', '')})"
                break

    print("── Spec requirement coverage ─────────────────────────────")
    if current_stage is None:
        print("  mode               : whole-spec (stage-agnostic)")
    else:
        print(f"  current stage      : {current_stage}{stage_name}")
    print(f"  MUST requirements  : {len(must)} total")
    print(f"  in scope (<= stage): {len(in_scope_must)}")
    print(f"  in-scope covered   : {covered_scope}/{len(in_scope_must)} "
          f"({100 * covered_scope // max(len(in_scope_must), 1)}%)")
    print(f"  in-scope UNCOVERED : {len(uncovered_scope)}")
    if current_stage is not None:
        print(f"  deferred (> stage) : {len(deferred)}")
        print(f"  unstaged (no stage): {len(unstaged)}")

    if uncovered_scope:
        by_file: dict[str, list[str]] = defaultdict(list)
        for r in uncovered_scope:
            by_file[must[r].file].append(r)
        print("\n  In-scope uncovered MUST requirements by spec section:")
        for fname in sorted(by_file):
            ids = by_file[fname]
            shown = ", ".join(ids[:10]) + (" …" if len(ids) > 10 else "")
            print(f"    {fname:32} {len(ids):3}  {shown}")

    if unstaged and current_stage is not None:
        by_file = defaultdict(list)
        for q in unstaged:
            by_file[q.file].append(q.rid)
        print("\n  Unstaged MUST requirements (assign a stage in stages.json "
              "or an inline [Sn] tag):")
        for fname in sorted(by_file):
            ids = by_file[fname]
            shown = ", ".join(ids[:10]) + (" …" if len(ids) > 10 else "")
            print(f"    {fname:32} {len(ids):3}  {shown}")

    if args.gate and uncovered_scope:
        scope = "spec" if current_stage is None else f"stage <= {current_stage}"
        print(f"\nspec-coverage: GATE FAILED — {len(uncovered_scope)} in-scope "
              f"({scope}) MUST requirement(s) have no citing test.",
              file=sys.stderr)
        return 1

    print("\nspec-coverage: OK" if not uncovered_scope
          else "\nspec-coverage: report only (no --gate)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
