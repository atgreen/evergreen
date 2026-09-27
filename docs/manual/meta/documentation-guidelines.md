# Writing documentation

Use these rules when adding or changing the TorCL manual. They follow the
Gloopy manual's docs-as-code and Diátaxis approach.

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

Open `http://127.0.0.1:8000/torcl/`. The static output is in `site/`. Both the virtual
environment and generated output are ignored by Git. The build uses
[Material for MkDocs](https://squidfunk.github.io/mkdocs-material/).

## Choose one page type

| Type | Reader's need | Write it as |
| --- | --- | --- |
| Tutorial | Learn by doing | One tested path, with expected results |
| How-to | Complete a task | A recipe for a reader who knows the goal |
| Reference | Look up a fact | Structured syntax, arguments, defaults, and limits |
| Explanation | Understand | Connected reasoning, context, and tradeoffs |

Navigation landing pages only direct readers to those pages. Every content page
has one primary type. State the type when describing a documentation change.
Move option catalogs out of tutorials and rationale out of reference pages.

## Two entry points, shared concepts

The **User guide** serves Lisp programmers and application builders.
**Contributing** serves people changing the runtime, compiler, and library.
Define execution tiers and images once in the user explanation pages; link
there from platform, deployment, and contributor pages.

Keep proposals and historical investigations in `docs/design/` or `spec/`.
Link to them when useful, with their status made explicit. Do not present a
specification's planned API as an implemented public interface.

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
Use relative links for assets so the site works below `/torcl/` as well as in a
local preview. The site uses system fonts and local search assets.

## Publication and versions

The intended address is `https://atgreen.github.io/torcl/`. Publication is not
enabled yet. The documentation workflow only builds and uploads a preview
artifact; it does not deploy Pages or push a branch. Keep deployment disabled
until the owner authorizes publication and the destination repository is ready.

This first edition documents the development checkout. Do not label it as a
stable release manual. When supported release versions need separate manuals,
add versioned publication tied to TorCL releases and retain old references.
Unlike Gloopy, TorCL does not currently have a separately versioned control
protocol that should drive manual versioning.
