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
cargo build --locked --release -p egcl --bin egcl --target "$target"
binary="${CARGO_TARGET_DIR:-target}/$target/release/egcl.exe"
echo "Built $binary"
[[ $action == test ]] || exit 0
command -v wine >/dev/null
# A private prefix keeps test configuration separate from desktop Wine apps.
export WINEPREFIX
WINEPREFIX=$(mktemp -d "${TMPDIR:-/tmp}/egcl-windows.XXXXXX")
cleanup() {
    WINEPREFIX="$WINEPREFIX" wineserver -k || true
    rm -rf -- "$WINEPREFIX"
}
trap cleanup EXIT
export WINEDEBUG=-all
scripts/egcl-limited.sh wineboot -u
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine
for fixture in scalars callbacks aggregates; do
    x86_64-w64-mingw32-gcc -shared -O2 "crates/egcl-rt/tests/fixtures/ffi_${fixture}.c" \
        -o "$WINEPREFIX/ffi-${fixture}.dll"
done
export EGCL_FFI_SCALARS_DLL="Z:$WINEPREFIX/ffi-scalars.dll"
export EGCL_FFI_CALLBACKS_DLL="Z:$WINEPREFIX/ffi-callbacks.dll"
export EGCL_FFI_AGGREGATES_DLL="Z:$WINEPREFIX/ffi-aggregates.dll"
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test windows_os --test test_jit
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --lib allocator_admission_tests
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --lib registration_tests
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --lib join_completion_tests
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --lib idle_carrier_steals_external_work_from_blocked_peer
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test fiber_roots --test fiber_fault_state --test fiber_preemption_state --test test_scheduler --test test_fiber_sync
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test fiber_ordered_locks
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test fiber_join_gc --test fiber_native_join --test fiber_finish_error --test fiber_join_managed
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test fiber_join_gc --test fiber_native_join --test fiber_finish_error --test fiber_join_managed
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test fiber_roots --test fiber_preemption_state
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test ffi_jit --test ffi_callback_jit --test ffi_callback_runtime
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test ffi_callback_runtime
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --test ffi_callback_cli
scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test ffi_aggregate_jit
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-rt --target "$target" --test ffi_aggregate_jit aggregate_calls_preserve_callback_gc_transitions_and_error_containment
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --test ffi_aggregate_cli
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --lib windows_t1_frame_restores_nonvolatile_state_at_all_boundaries
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --test windows_native --test t1_native --test t1_deopt --test t1_fuzz --test direct_call_invalidation
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --test native_transfer_cli --test tier_observability
scripts/egcl-limited.sh cargo test --locked -p egcl-compiler --target "$target" --lib declines_sysv_code_on_windows
scripts/egcl-limited.sh cargo test --locked -p egcl-compiler --target "$target" --lib runtime_helper
scripts/egcl-limited.sh cargo test --locked -p egcl-compiler --target "$target" --lib windows_t2
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --lib windows_image_tests
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --test test_tcp_streams --test test_process -- --test-threads=1
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --test process_scheduling --test process_pipes --test process_lifecycle
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --lib process::tests
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --lib streams::output_flush_tests
scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --lib streams::fiber_tests
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --lib streams::fiber_tests
scripts/egcl-limited.sh cargo test --locked -p egcl --target "$target" --test stream_roots_cli
EGCL_GC_STRESS=1 EGCL_GC_POISON=1 scripts/egcl-limited.sh cargo test --locked -p egcl-stdlib --target "$target" --test process_scheduling --test process_pipes --test process_lifecycle
child="${CARGO_TARGET_DIR:-target}/$target/release/windows-process-child.exe"
rustc --edition=2024 --target "$target" scripts/windows-process-child.rs -o "$child"
scripts/egcl-limited.sh python3 scripts/windows-io-smoke.py "$binary" "$child"
scripts/egcl-limited.sh python3 scripts/portability-smoke.py win64 -- wine "$binary"
