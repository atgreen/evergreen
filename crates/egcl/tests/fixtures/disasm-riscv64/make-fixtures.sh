#!/usr/bin/env bash
# Build the riscv64 disassembler fixtures on an RV64 host with GNU binutils
# (objdump 2.44). t1-native.bin/t2-native.bin come from capture.py against a
# native egcl; see crates/egcl/tests/disasm_riscv64_lisp.rs.
set -euo pipefail
cd "$(dirname "$0")"
ref() { objdump -D -b binary -m riscv:rv64 "$1" | awk -F'\t' '/^ *[0-9a-f]+:/ && NF>=3 {sub(/[ \t]+$/,"",$3); print $3 (NF>=4 ? "\t" $4 : "")}' | sed 's/[ \t]*$//' > "$2"; }
hexof() { od -An -v -tx1 "$1" | tr -d ' \n' > "$2"; echo >> "$2"; }
for name in general compressed; do
  gcc -c -march=rv64gc "$name.s" -o "$name.o"
  objcopy -O binary -j .text "$name.o" "$name.bin"
  hexof "$name.bin" "$name.hex"; ref "$name.bin" "$name.ref"
done
# 6 KiB of seeded random bytes: every compressed shape, reserved encoding and
# multi-halfword length rule, the way a literal pool hits them.
python3 -c 'import random; random.seed(20261008); open("random.bin","wb").write(bytes(random.getrandbits(8) for _ in range(6144)))'
hexof random.bin random.hex; ref random.bin random.ref
for name in t1-native t2-native; do
  if [ -f "$name.bin" ]; then hexof "$name.bin" "$name.hex"; ref "$name.bin" "$name.ref"; fi
done
wc -l *.ref
