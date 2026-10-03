# Writing documentation

Use these rules when adding or changing the EGCL manual. They follow the
Gloopy manual's docs-as-code tooling and the SBCL manual's subject-oriented
implementation-reference structure.

## Build and preview

From the repository root:

```sh
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r requirements-docs.txt
python3 scripts/test_docs.py
mkdocs build --strict
mkdocs serve --dev-addr 127.0.0.1:8000
```

Open `http://127.0.0.1:8000/egcl/`. The static output is in `site/`. Both the virtual
environment and generated output are ignored by Git. The build uses
[Material for MkDocs](https://squidfunk.github.io/mkdocs-material/).

## Organize by implementation subject

The primary reader knows Common Lisp and needs to know how EGCL behaves.
Organize the manual into subjects such as startup, compilation, debugging,
foreign calls, memory, and concurrency. The
[SBCL manual](https://www.sbcl.org/manual/) is the structural inspiration:
chapters lead to specific behavior and dictionary entries, with symbol and
concept indexes for direct lookup.

Keep the useful distinction between tutorials, recipes, explanations, and
reference, but do not make readers choose a documentation category before they
can find a subject. A chapter may explain the subject and then present its
dictionary. Standalone tutorials still take one tested path; task guides still
solve a specific problem.

## Write an implementation dictionary

An API entry names the package and whether it is a function, macro, variable,
or type. Include the actual lambda list, return values (including secondary
values), units, ownership, lifetime, error behavior, and target restrictions
when they affect the caller. Similar spelling to SBCL is not proof of the same
contract. Read source and tests before describing compatibility.

Every documented interface belongs in the symbol index and every major topic
in the concept index. Use stable explicit anchors for dictionary entries.
Document each contract once and link to it from recipes.

## Keep internal development separate

The main chapters serve Lisp programmers. Runtime contributor notes belong in
an appendix. Keep proposals and historical investigations in `docs/design/` or
`spec/`, and identify their status when linking to them. An implementation
manual must not present a proposed API as an existing callable interface.

## Keep reference near its source

The CLI page contains a marker expanded by `docs/hooks.py` from the CLI's
`help_text()` function. Edit the source help, not a second list of options.
The generator rejects source shapes it cannot interpret. Its output exists only
in the built site, not in committed generated Markdown.

For other interfaces, update documentation with the implementation and point
reviewers at the code or tests that substantiate it. Do not generate an API
inventory by blindly publishing every internal function.

## Validate changes

Run `mkdocs build --strict`. Missing pages, navigation omissions, and broken
internal anchors are warnings promoted to failures. This does not check the
availability of external sites. Run examples on the named target where possible
and distinguish compilation, emulation, and native-device evidence.

Use normal Markdown links between manual pages. Source-code links should point
to the repository, since source files are not part of the built site.
Use relative links for assets so the site works below `/egcl/` as well as in a
local preview. The site uses system fonts and local search assets.

## Publication and versions

The manual is published at `https://atgreen.github.io/evergreen/`, versioned
with [mike](https://github.com/jimporter/mike). Each version is a directory on
the `gh-pages` branch, `versions.json` at the branch root drives the version
selector, and the root redirects to the default alias:

```
/              redirect to latest/
/versions.json
/latest/       alias for the newest release
/0.0.1/
/dev/          built from main
```

A push to `main` refreshes `dev` and nothing else; released versions and the
default alias are untouched. A release publishes its own version and moves
`latest` onto it:

```sh
gh workflow run docs.yml -f version=0.0.1 -f alias=latest
```

Material injects the version selector in the browser by fetching
`versions.json`, so it does not appear in the built HTML. An absent selector in
`site/` is expected and not a build problem.

The deploy job pushes the `gh-pages` branch and then publishes that whole
branch to Pages. The signed dnf repository metadata lives under `repo/` on the
same branch, so it is published by the same deployment. Each publisher commits
only its own subtree and cannot overwrite the other's.

Do not run `mike delete --all`. It rewrites the branch and would remove the
repository metadata along with the docs.

Links that predate versioning point at the unversioned paths, which now live
under the default alias — `/evergreen/compiler/` is `/evergreen/latest/compiler/`.
Only the site root is redirected.
