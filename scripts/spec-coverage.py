#!/usr/bin/env python3
"""spec-coverage.py — requirement traceability gate for Bliss.

Enumerates the normative requirements declared in ``spec/`` (both markdown
table rows of the form ``| R6.45 | ... | MUST |`` and prose entries of the
form ``**R10.01** ... MUST ...``) and checks which are cited by at least one
test under ``crates/*/tests/`` — tests reference a requirement by its R-id,
e.g. ``// Per R6.45, (require ...) delegates to ASDF``.

Why this exists: a spec-mandated capability that no phase ever turned into a
task is never tested *and* never implemented, so it sails straight past a
green build — "missing" is not the same as "failing". This tool makes those
holes visible (and, with ``--gate``, fatal).

Usage:
    python3 scripts/spec-coverage.py            # human-readable report, exit 0
    python3 scripts/spec-coverage.py --gate     # exit 1 if any MUST is uncovered
    python3 scripts/spec-coverage.py --repo DIR # repo root (default: cwd)

Do NOT delete: this is the traceability tool invoked by bureau's verify gate.
Tests are expected to cite the requirements they exercise by R-id.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

_RID_EXACT = re.compile(r"R\d+\.\d+")
_RID_PROSE = re.compile(r"^\*\*(R\d+\.\d+)\*\*")
_LEVEL = re.compile(r"\b(MUST(?:\s*/\s*SHOULD)?|SHOULD|MAY|REQUIRED|SHALL)\b",
                    re.IGNORECASE)
_SKIP_SPEC_FILES = {"12-glossary.md"}


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


def parse_requirements(spec_dir: Path) -> dict[str, tuple[str, str]]:
    """Return {req_id: (level, spec_filename)} for every requirement."""
    reqs: dict[str, tuple[str, str]] = {}
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
            level = _level_for_text(" | ".join(cells[1:])) or cells[-1].upper()
            if cells[0] in reqs:
                continue
            reqs[cells[0]] = (level, md.name)
        for block in _split_blocks(text):
            first = block.splitlines()[0].strip()
            match = _RID_PROSE.match(first)
            if not match:
                continue
            level = _level_for_text(block)
            if not level:
                continue
            rid = match.group(1)
            if rid in reqs:
                continue
            reqs[rid] = (level, md.name)
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
                    help="exit non-zero if any MUST requirement is uncovered")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    spec_dir = repo / "spec"
    if not spec_dir.is_dir():
        print(f"spec-coverage: no spec/ directory under {repo}", file=sys.stderr)
        return 2

    reqs = parse_requirements(spec_dir)
    cited = parse_citations(repo)
    if not reqs:
        print("spec-coverage: no requirements found in spec/", file=sys.stderr)
        return 2

    must = {r: v for r, v in reqs.items() if v[0].startswith("MUST")}
    uncovered_must = sorted(r for r in must if r not in cited)
    covered_must = len(must) - len(uncovered_must)

    print("── Spec requirement coverage ─────────────────────────────")
    print(f"  requirements total : {len(reqs)}")
    print(f"  MUST requirements  : {len(must)}")
    print(f"  MUST covered       : {covered_must}/{len(must)} "
          f"({100 * covered_must // max(len(must), 1)}%)")
    print(f"  MUST UNCOVERED     : {len(uncovered_must)}")

    if uncovered_must:
        by_file: dict[str, list[str]] = defaultdict(list)
        for r in uncovered_must:
            by_file[must[r][1]].append(r)
        print("\n  Uncovered MUST requirements by spec section:")
        for fname in sorted(by_file):
            ids = by_file[fname]
            shown = ", ".join(ids[:10]) + (" …" if len(ids) > 10 else "")
            print(f"    {fname:32} {len(ids):3}  {shown}")

    if args.gate and uncovered_must:
        print(f"\nspec-coverage: GATE FAILED — {len(uncovered_must)} MUST "
              f"requirement(s) have no citing test.", file=sys.stderr)
        return 1

    print("\nspec-coverage: OK" if not uncovered_must
          else "\nspec-coverage: report only (no --gate)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
