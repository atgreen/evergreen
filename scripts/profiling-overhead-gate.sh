#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${TORCL_BIN:-$repo_root/target/x86_64-unknown-linux-musl/release/torcl}"
reps="${TORCL_PROFILE_GATE_REPS:-5}"
iterations="${TORCL_PROFILE_GATE_ITERATIONS:-250000}"

if [[ ! -x "$bin" ]]; then
  echo "missing executable: $bin" >&2
  echo "build one with: cargo build -p torcl --release --target x86_64-unknown-linux-musl" >&2
  exit 2
fi

program="(defun prof-gate (n)
  (let ((s 0))
    (dotimes (i n s)
      (setq s (+ s i)))))
(dotimes (k 8) (prof-gate 10000))
(format t \"~a~%\" (prof-gate $iterations))"

median_ns() {
  local mode="$1"
  local -a times=()
  local i start end elapsed
  for ((i = 0; i < reps; i++)); do
    start="$(date +%s%N)"
    if [[ "$mode" == disabled ]]; then
      TORCL_PROFILING_DISABLED=1 "$bin" --no-init --eval "$program" >/dev/null
    else
      env -u TORCL_PROFILING_DISABLED "$bin" --no-init --eval "$program" >/dev/null
    fi
    end="$(date +%s%N)"
    elapsed=$((end - start))
    times+=("$elapsed")
  done
  printf '%s\n' "${times[@]}" | sort -n | sed -n "$(((reps + 1) / 2))p"
}

enabled_ns="$(median_ns enabled)"
disabled_ns="$(median_ns disabled)"

printf 'profiling enabled median:  %s ns\n' "$enabled_ns"
printf 'profiling disabled median: %s ns\n' "$disabled_ns"

if (( enabled_ns * 100 > disabled_ns * 105 )); then
  overhead_bp=$(((enabled_ns * 10000 / disabled_ns) - 10000))
  printf 'profiling overhead gate FAILED: +%d.%02d%% > +5.00%%\n' \
    "$((overhead_bp / 100))" "$((overhead_bp % 100))" >&2
  exit 1
fi

printf 'profiling overhead gate passed: enabled <= disabled + 5%%\n'
