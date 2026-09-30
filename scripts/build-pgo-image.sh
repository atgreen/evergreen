#!/usr/bin/env bash
# Local instrumentation PGO, used by `make image`. No installation.
# All build/profile artifacts are retained in a fresh directory for diagnosis.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
die() { echo "PGO: $*" >&2; exit 1; }
compiler=${RUSTC:-rustc}
cargo=${CARGO:-cargo}
version=$("$compiler" -vV)
host=$(sed -n 's/^host: //p' <<<"$version")
llvm=$(sed -n 's/^LLVM version: //p' <<<"$version")
[[ -n $host && -n $llvm ]] || die 'cannot identify rustc host and LLVM version'
target=${EGCL_PGO_TARGET:-x86_64-unknown-linux-musl}
case "$host/$target" in
    x86_64-unknown-linux-gnu/x86_64-unknown-linux-musl) ;;
    "$host/$host") ;;
    *) die "target $target is not runnable on host $host; PGO requires native training" ;;
esac

profdata=${LLVM_PROFDATA:-}
if [[ -z $profdata ]]; then
    sysroot=$("$compiler" --print sysroot)
    profdata="$sysroot/lib/rustlib/$host/bin/llvm-profdata"
    if [[ ! -x $profdata ]]; then
        profdata=$(command -v llvm-profdata || true)
    fi
fi
[[ -n $profdata ]] || die 'install matching llvm-tools-preview or set LLVM_PROFDATA'
profversion=$("$profdata" --version) || die "cannot run LLVM_PROFDATA=$profdata"
tool_llvm=$(sed -nE 's/.*LLVM version ([0-9]+\.[0-9]+\.[0-9]+).*/\1/p' <<<"$profversion")
[[ $tool_llvm == "$llvm" ]] || die "rustc uses LLVM $llvm, llvm-profdata uses $tool_llvm; set LLVM_PROFDATA to a matching tool"

build_root=${EGCL_PGO_ROOT:-target/pgo}
mkdir -p -- "$build_root"
build_root=$(cd -- "$build_root" && pwd)
work=$(mktemp -d "$build_root/run.XXXXXX")
echo "PGO artifacts: $work"
mkdir -- "$work/raw" "$work/prepare-profiles"

# Encoded flags preserve spaces in absolute profile paths. Match Cargo's
# whitespace-separated RUSTFLAGS fallback when no encoded override is present.
base_flags=${CARGO_ENCODED_RUSTFLAGS-}
if [[ ! ${CARGO_ENCODED_RUSTFLAGS+x} ]]; then
    read -r -d '' -a flags <<<"${RUSTFLAGS:-}" || true
    base_flags=$(IFS=$'\x1f'; echo "${flags[*]}")
fi
separator=$'\x1f'
[[ -z $base_flags ]] || base_flags+=$separator
export RUSTC="$compiler"

logged() {
    local name=$1 status
    shift
    echo "PGO: $name"
    "$@" >"$work/$name.log" 2>&1 || {
        status=$?
        tail -n 40 "$work/$name.log" >&2
        return "$status"
    }
}
require_marker() {
    grep -Fxq "$2" "$work/$1.log" || die "$1 did not report $2; see $work/$1.log"
}

logged generate env CARGO_TARGET_DIR="$work/generate" \
    CARGO_ENCODED_RUSTFLAGS="${base_flags}-Cprofile-generate=$work/raw" \
    "$cargo" build --locked --release -p egcl --target "$target"
instrumented="$work/generate/$target/release/egcl"
logged prepare env EGCL_PGO_WORK="$work/training" EGCL_PGO_PHASE=prepare \
    LLVM_PROFILE_FILE="$work/prepare-profiles/%p-%m.profraw" \
    "$instrumented" --no-init --load scripts/pgo-workload.lisp
require_marker prepare 'PGO-PREPARED 24'
shopt -s nullglob
for iteration in 1 2 3; do
    for phase in load runtime; do
        logged "$phase-$iteration" env EGCL_PGO_WORK="$work/training" EGCL_PGO_PHASE="$phase" \
            LLVM_PROFILE_FILE="$work/raw/$phase-$iteration-%p-%m.profraw" \
            "$instrumented" --no-init --load scripts/pgo-workload.lisp
        if [[ $phase == load ]]; then
            require_marker "$phase-$iteration" 'PGO-LOAD 24 300'
        else
            require_marker "$phase-$iteration" 'PGO-RUNTIME 1000 499500'
        fi
        phase_raw=("$work/raw/$phase-$iteration-"*.profraw)
        ((${#phase_raw[@]} > 0)) || die "$phase-$iteration produced no raw profile"
    done
done
raw=("$work"/raw/*.profraw)
((${#raw[@]} > 0)) || die 'training produced no raw profiles'
for profile in "${raw[@]}"; do
    [[ -s $profile ]] || die "empty raw profile: $profile"
done
logged merge "$profdata" merge -o "$work/merged.profdata" "${raw[@]}"
[[ -s $work/merged.profdata ]] || die 'merge produced no profile'
logged use env CARGO_TARGET_DIR="$work/use" \
    CARGO_ENCODED_RUSTFLAGS="${base_flags}-Cprofile-use=$work/merged.profdata${separator}-Cllvm-args=-pgo-warn-missing-function" \
    "$cargo" build --locked --release -p egcl --target "$target"
if grep -Ei 'warning:.*(profile|hash mismatch|control flow change)' "$work/use.log"; then
    die "profile-use build reported incompatible or missing profile data; see $work/use.log"
fi

# Dump beside the destination so publication is an atomic, same-filesystem
# rename. Neither a failed save nor a failed restart touches the prior image.
output=${EGCL_IMAGE_OUT:-target/egcl}
[[ ! -d $output ]] || die "image destination is a directory: $output"
mkdir -p -- "$(dirname -- "$output")"
output="$(cd -- "$(dirname -- "$output")" && pwd)/$(basename -- "$output")"
staging=$(mktemp -d "$(dirname -- "$output")/.egcl-pgo.XXXXXX")
logged image env EGCL_IMAGE_OUT="$staging/egcl" \
    "$work/use/$target/release/egcl" --no-init --load scripts/build-image.lisp
logged verify "$staging/egcl" --no-init --eval \
    '(progn (assert (find-package :asdf)) (assert (stringp (asdf:asdf-version))) (format t "PGO-IMAGE-OK~%"))'
require_marker verify 'PGO-IMAGE-OK'
mv -f -- "$staging/egcl" "$output"
rmdir -- "$staging" 2>/dev/null || echo "PGO: retained image-stage diagnostics in $staging" >&2
echo "PGO image: $output"
