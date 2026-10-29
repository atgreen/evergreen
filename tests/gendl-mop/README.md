# Gendl MOP smoke scenario

Run the official, unmodified Gendl MOP kernel's 166-check smoke test at DEVO
commit `5675eb3d8b08230476988adfc141423491b587e8`. Its source already contains
EGCL package and weak-table conditionals; this scenario applies no library
patches and does not use the older default `master` branch.

Only `gendl-mop` is loaded, with `gdl:*kernel*` bound to `:mop`. Its Gendl
dependencies are `base` and `gendl-asdf`; UIOP comes with ASDF. Loading the full
`gendl` system would bring unrelated geometry and web dependencies. The runner
requires the actual 166/0 tally, preserves upstream assertions, and checks that
all three systems came from the pinned local tree. Runtime downloads and
inherited ASDF source registries are disabled.

## Run

Use an EGCL build containing the slot-definition/finalization and using-class
protocols for GitHub issue #762. The commands below start in the Evergreen
repository, then switch to a fresh project. All executable and wrapper paths
are made absolute before that switch. Keep the printed project directory and
logs on failure.

```sh
set -euo pipefail
cd /absolute/path/to/evergreen
repo="$PWD"
egcl=$(realpath "${EGCL_BIN:-$repo/target/x86_64-unknown-linux-musl/release/egcl}")
ocicl=$(realpath "$(command -v "${OCICL_BIN:-ocicl}")")
runtime=$(realpath "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}")
work=$(mktemp -d "${TMPDIR:-/tmp}/egcl-gendl-mop.XXXXXX")
printf 'Gendl artifacts: %s\n' "$work"
cp "$repo/tests/gendl-mop/ocicl.csv" "$repo/tests/gendl-mop/run.lisp" "$work/"
cd "$work"
export OCICL_LOCAL_ONLY=1 EGCL_GENDL_RUNTIME="$runtime"
EGCL_MEM_MAX=4G EGCL_TIMEOUT=600 \
  "$repo/scripts/egcl-limited.sh" "$ocicl" install >install.log 2>&1
cmp "$repo/tests/gendl-mop/ocicl.csv" ocicl.csv
mkdir -p cache/egcl
export EGCL_GENDL_CACHE="$work/cache/egcl/"
EGCL_MEM_MAX=4G EGCL_TIMEOUT=1200 \
  "$repo/scripts/egcl-limited.sh" "$egcl" --no-init --load "$work/run.lisp" \
  >egcl.log 2>&1
grep -Fxq 'GENDL-MOP-SMOKE-166-OK' egcl.log
```

For an independent SBCL reference, stay in that same project and use a separate
cache and log:

```sh
sbcl=$(realpath "$(command -v "${SBCL_BIN:-sbcl}")")
mkdir -p cache/sbcl
EGCL_GENDL_CACHE="$work/cache/sbcl/" EGCL_MEM_MAX=4G EGCL_TIMEOUT=1200 \
  "$repo/scripts/egcl-limited.sh" "$sbcl" --noinform --non-interactive \
  --load "$work/run.lisp" >sbcl.log 2>&1
grep -Fxq 'GENDL-MOP-SMOKE-166-OK' sbcl.log
```

Use `ocicl install` with no arguments to restore the committed manifest, rather
than installing the monorepo URL anew: a new URL installation registers every
system in the repository and discovers their unrelated dependencies.

## Validation status

The GitLab pin and directory/ASD paths were checked against the official source
and ocicl's documented Git-source/parser/restore implementation. The committed
CSV was prepared from that source audit; a fresh ocicl restore and both runtime
commands remain **unrun**. This is a reproducible validation scenario, not a
claim that EGCL already passes the suite. Any later compatibility changes must
live in an `atgreen` fork and be pinned here; never patch the downloaded tree.
