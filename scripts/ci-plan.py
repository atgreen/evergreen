#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Select CI work conservatively from an exact Git commit pair.

Only the documentation allowlist skips runtime validation. Missing history,
invalid revisions, and empty diffs select all fast areas. This planner does not
interpret changed filenames as shell commands or emit them as workflow outputs.
"""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

AREAS = ["runtime", "compiler", "reader"]
DOC_FILES = {"README.md", "CHANGELOG.md", "AGENTS.md", "CITATION.cff", "CITATION"}
PGO_FILES = {"Makefile", "scripts/build-pgo-image.sh", "scripts/build-image.lisp",
             "scripts/pgo-workload.lisp", "scripts/test-pgo-build.py",
             "scripts/test-pgo-workload.sh"}
EVENTS = ("push", "pull_request", "schedule", "workflow_dispatch", "workflow_call", "merge_group")


def exact_commit(revision):
    return bool(revision and re.fullmatch(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})", revision)
                and set(revision) != {"0"})


def changed_paths(base, head):
    if not exact_commit(base) or (head != "HEAD" and not exact_commit(head)):
        return None
    try:
        # Resolve HEAD once; all diff arguments thereafter are full object IDs.
        head = subprocess.check_output(
            ["git", "rev-parse", "--verify", head + "^{commit}"],
            stderr=subprocess.PIPE, timeout=30, text=True).strip()
        base = subprocess.check_output(
            ["git", "rev-parse", "--verify", base + "^{commit}"],
            stderr=subprocess.PIPE, timeout=30, text=True).strip()
        if not exact_commit(base) or not exact_commit(head):
            return None
        # Disabling rename detection reports BOTH the removed and added names.
        # NUL separators preserve whitespace/newlines in tracked paths.
        output = subprocess.check_output(
            ["git", "diff", "--no-ext-diff", "--no-textconv", "--name-only",
             "--no-renames", "-z", base, head, "--"],
            stderr=subprocess.PIPE, timeout=30)
        return [os.fsdecode(path) for path in output.split(b"\0") if path]
    except (OSError, subprocess.SubprocessError):
        return None


def areas_for(path):
    if path.startswith("docs/") or path in DOC_FILES:
        return set()
    name = PurePosixPath(path).name
    if name in {"Cargo.toml", "Cargo.lock", "build.rs"}:
        return set(AREAS)
    if "reader" in name:
        return set(AREAS)
    if path.startswith("crates/egcl-compiler/"):
        return {"runtime", "compiler"}
    if path.startswith(("crates/egcl/", "crates/egcl-rt/", "crates/egcl-stdlib/")):
        return {"runtime"}
    # Shared tooling, specifications, libraries and unknown paths may affect
    # every area. New repository directories cannot silently evade testing.
    return set(AREAS)


def plan(event, suite, base, head):
    full = event in {"schedule", "merge_group"} or suite == "full"
    if full:
        return dict(runtime=True, full=True, areas=AREAS, pgo=True)
    paths = changed_paths(base, head)
    if not paths:
        print("ci-plan: missing, invalid or empty diff; running all fast areas", file=sys.stderr)
        return dict(runtime=True, full=False, areas=AREAS, pgo=False)
    selected = set().union(*(areas_for(path) for path in paths))
    return dict(runtime=bool(selected), full=False,
                areas=[area for area in AREAS if area in selected],
                pgo=bool(PGO_FILES.intersection(paths)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", choices=EVENTS, required=True)
    parser.add_argument("--suite", choices=("fast", "full"), default="fast")
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = plan(args.event, args.suite, args.base, args.head)
    if args.output:
        with args.output.open("a", encoding="utf-8") as output:
            for key, value in result.items():
                output.write(f"{key}={json.dumps(value, separators=(',', ':'))}\n")
    print(json.dumps(result, separators=(",", ":")))


if __name__ == "__main__":
    main()
