#!/usr/bin/env bash
# Cross-build a Windows x86-64 CLI on Linux; optionally exercise it under Wine.
set -euo pipefail
cd "$(dirname "$0")/.."
action=${1:-build}
if [[ $# -gt 1 || ! $action =~ ^(build|test)$ ]]; then
    echo "usage: $0 [build|test]" >&2
    exit 2
fi
export RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-1.94.1}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
target=x86_64-pc-windows-gnu
command -v x86_64-w64-mingw32-gcc >/dev/null
cargo build --locked --release -p torcl --bin torcl --target "$target"
binary="${CARGO_TARGET_DIR:-target}/$target/release/torcl.exe"
echo "Built $binary"
[[ $action == test ]] || exit 0
command -v wine >/dev/null
# A private prefix keeps test configuration separate from desktop Wine apps.
export WINEPREFIX
WINEPREFIX=$(mktemp -d "${TMPDIR:-/tmp}/torcl-windows.XXXXXX")
cleanup() {
    WINEPREFIX="$WINEPREFIX" wineserver -k || true
    rm -rf -- "$WINEPREFIX"
}
trap cleanup EXIT
export WINEDEBUG=-all
scripts/torcl-limited.sh wineboot -u
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine
scripts/torcl-limited.sh cargo test --locked -p torcl-rt --target "$target" --test windows_os --test test_jit
scripts/torcl-limited.sh cargo test --locked -p torcl-compiler --target "$target" --lib declines_sysv_code_on_windows
scripts/torcl-limited.sh cargo test --locked -p torcl-stdlib --target "$target" --lib windows_image_tests
scripts/torcl-limited.sh cargo test --locked -p torcl-stdlib --target "$target" --test test_tcp_streams --test test_process -- --test-threads=1
child="${CARGO_TARGET_DIR:-target}/$target/release/windows-process-child.exe"
rustc --edition=2024 --target "$target" scripts/windows-process-child.rs -o "$child"
scripts/torcl-limited.sh python3 scripts/windows-io-smoke.py "$binary" "$child"
scripts/torcl-limited.sh python3 scripts/portability-smoke.py win64 -- wine "$binary"
