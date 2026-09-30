# §4.7 Code Emission & Register Allocation

Code emission transforms optimised IR (§4.4) into executable machine code.
This section specifies lowering from the block-based SSA IR (§4.3) to
machine-specific nodes, backend instruction selection for x86-64 and AArch64,
register allocation, code buffer management, runtime patching, GC stack maps, and
code region lifecycle.  The design mirrors HotSpot — an SSA-based register
allocator with live-range splitting, relocatable code buffers, and CAS-based
code patching.

---

## 4.7.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R4.42 | The compiler MUST lower all IR nodes to platform-specific `MachNode`s before register allocation. Unlowered nodes MUST cause a compilation abort. | MUST |
| R4.43 | The x86-64 backend MUST comply with System V AMD64 ABI for FFI calls and MUST use SSE2 as the baseline float ISA. SSE4.1 and AVX2 MAY be emitted when detected via `CPUID`. | MUST/MAY |
| R4.44 | The AArch64 backend MUST comply with AAPCS64. NEON MUST be used for float/SIMD. SVE MAY be emitted when detected. | MUST/MAY |
| R4.45 | Register allocation MUST use an SSA-based allocator with live-range splitting and spill-slot coalescing — the `regalloc2` library (the Cranelift allocator) or an equivalent. It MUST support at least two register classes (integer/GPR for tagged, unboxed-integer, and pointer values; float/XMM for unboxed floats), honour fixed-register constraints imposed by the SysV/AAPCS64 ABI and the c2i calling convention, and — per §4.10 R4.65 — finalise both the GC stack map (R4.46) and every FrameState `Location` binding at each safepoint and deopt point. | MUST |
| R4.46 | Every safepoint MUST have a GC stack map. Missing maps MUST be detected at code installation, not GC time. | MUST |
| R4.47 | Code patching MUST be atomic w.r.t. concurrent threads. x86-64: CAS or single-instruction writes. AArch64: `ISB` barrier after patching. | MUST |

---

## 4.7.2 Lowering: IR → MachNode

Lowering is a bottom-up maximal-munch pass over the scheduled block list.
Each IR node is replaced by one or more platform-specific `MachNode`s (D4.11).

**Rules:** (1) Select the largest matching tile to minimise instruction
count. (2) Fold base+index+displacement into addressing modes. (3) Fold
constants into immediate operands when they fit the ISA field. (4) Fuse
`IRCmp`+`IRBranch` into compare-and-branch. (5) Proven-unboxed fixnum ops
lower to native integer instructions without tag manipulation.

**Post-lowering invariants:** no IR nodes remain; every MachNode has fixed
register constraints; control flow is a doubly-linked `MachBlock` list.

---

## 4.7.3 Data Structure D4.11 — MachNode

```rust
pub struct MachNode {
    pub id: MachNodeId,
    pub opcode: MachOpcode,              // X64Op or A64Op enum
    pub operands: SmallVec<[MachOperand; 3]>,
    pub def: Option<VReg>,               // virtual register defined
    pub clobbers: RegSet,                // additional clobbered phys regs
    pub constraints: SmallVec<[RegConstraint; 3]>,
    pub flags: MachFlags,                // is_call, is_safepoint, is_branch, may_trap
    pub block: MachBlockId,
    pub src_loc: Option<SourceLoc>,
}

pub enum MachOperand {
    VReg(VReg),
    PReg(PReg),                          // pre-coloured physical register
    Imm(i64),
    Mem(MemOperand),                     // base + index*scale + disp
    Label(MachBlockId),
}

pub enum RegConstraint {
    Any(RegClass),                       // any register in class
    Fixed(PReg),                         // must be this physical register
    Tied(u8),                            // must share reg with operand N
    None,
}

pub enum RegClass { GPR, FPR }

pub struct MachBlock {
    pub id: MachBlockId,
    pub nodes: Vec<MachNodeId>,
    pub successors: SmallVec<[MachBlockId; 2]>,
    pub predecessors: SmallVec<[MachBlockId; 2]>,
    pub loop_depth: u16,
    pub frequency: f32,
}
```

---

## 4.7.4 x86-64 Backend

### 4.7.4.1 Instruction Selection

| IR Pattern | X64Op | Notes |
|------------|-------|-------|
| `IRAdd(a, b)` int | `Add64rr` / `Add64ri` | Immediate when b ≤ 32 bits |
| `IRMul(a, b)` int | `Imul64rr` / `Imul64ri` | 3-operand form |
| `IRLoad(base, off)` | `Mov64rm` | Folds base+idx*scale+disp |
| `IRStore(base, off, val)` | `Mov64mr` | Same addressing |
| `IRFAdd/FMul` f64 | `Addsd` / `Mulsd` | SSE2; AVX2 prefers `VADDSD` (3-op) |
| `IRRound(x, mode)` f64 | `Roundsd` | SSE4.1; see §4.7.4.3 fallback |
| `IRCall` | `Call64r` / `Call64m` | System V ABI |
| `IRBranch(cmp, t, f)` | `Jcc` | Fused compare-and-branch |
| `IRSafepoint` | `Nop` + stack map | Metadata only |

### 4.7.4.2 System V AMD64 ABI

| Aspect | Rule |
|--------|------|
| Integer args 1-6 | `RDI`, `RSI`, `RDX`, `RCX`, `R8`, `R9` |
| FP args 1-8 | `XMM0`–`XMM7` |
| Return | `RAX` (int), `XMM0` (float) |
| Callee-saved | `RBX`, `RBP`, `R12`–`R15` |
| Stack alignment | 16-byte before `CALL` |
| Red zone | MUST NOT be used (signal handlers / GC may clobber) |

**EGCL-internal convention (x86-64):**

| Aspect | Register |
|--------|----------|
| Closure pointer | `R13` (pinned, callee-saved) |
| Arg count | `RCX` |
| First 4 Lisp args | `RDI`, `RSI`, `RDX`, `R8` |
| Return value | `RAX` |
| Execution-context pointer | `R13` (reserved in generated code, callee-saved) |

**R4.72** Generated Lisp code MUST reserve `R13` for the current Lisp execution
context pointer (§2.3.1). `R14` and `R15` MUST NOT be used for it: T1 holds the
frame-slots pointer in `R14` and the operand-stack pointer in `R15`, and the T2
emitter maps its `regalloc2` edit scratch onto `R15`. T2 MUST exclude `R13` from
its allocatable pool. [S6]

**Why `R13`** (ABI audit, `bliss-q861`, 2026-09-26). The choice is settled by
fiber switching, not by instruction cost:

- The fiber context switch (`egcl-rt/src/context.rs`) already saves and
  restores the callee-saved set — `rbp rbx r12 r13 r14 r15` — on each fiber's
  own stack. A context pointer in `R13` therefore **travels with the fiber for
  free**: it is saved when the fiber swaps out and restored when it resumes,
  including onto a *different* carrier, so migration requires no action and
  there is no per-switch bookkeeping to omit. A thread-local slot is per
  *carrier* instead, so it would have to be rewritten at every mount, unmount,
  migration and nested foreign→Lisp re-entry, and a single missed store would
  make generated code read another fiber's context.
- Measured on Meteorlake: for the realistic one-read-per-region pattern,
  `mov r, %fs:disp` and `mov r, [reg+disp]` are indistinguishable (1.44 vs
  1.45 cycles/iteration). Segment-prefixed loads lose only when saturated
  (2.5–3× lower throughput at eight loads per iteration). Performance does not
  decide this.
- `R13` rather than `R12`: `R12` encodes as an alias of `RSP` in ModRM, forcing
  a SIB byte for fixed displacements. SBCL documents exactly this reason for the
  same choice (`src/compiler/x86-64/vm.lisp`).
- Prior art agrees: HotSpot pins `r15` (`assembler_x86.hpp`), Go pins `R14`, and
  SBCL pins `r13`. Go's ABI notes it keeps a TLS copy on amd64 yet still pins the
  register "for simplicity and for consistency with other architectures".

**R4.73** Rust code MUST NOT read `R13` via inline assembly to obtain the
context: `rustc` allocates `R13` freely, so a Rust function may already have
clobbered it. Generated code MUST pass the context pointer as an ordinary
argument to the runtime helpers it calls; where the runtime needs it without
such a call (GC root scanning, signal handlers), it MUST come from the
scheduler's per-carrier record of the mounted fiber, not from the register. [S6]

`R13` is callee-saved under SysV, so generated code may rely on it surviving
calls into the runtime — the same property T1 already depends on for `R14`/`R15`
across `c2i`.

### 4.7.4.3 Float Operations

SSE2 scalar: `ADDSD/SUBSD/MULSD/DIVSD/UCOMISD` (double),
`ADDSS/SUBSS/…` (single). Conversions: `CVTSI2SD`, `CVTTSD2SI`.
`UCOMISD` sets PF+ZF — backend MUST check both for CL `=` semantics.
MXCSR FTZ/DAZ MUST NOT be set (ANSI CL requires valid denormals).

**SSE4.1 instructions (when `EGCL_ENABLE_SSE41 ≠ off`):**

| Instruction | CL Use | Notes |
|-------------|--------|-------|
| `ROUNDSD imm8` | `ROUND`, `TRUNCATE`, `FLOOR`, `CEILING` | imm8 mode: 0=round-nearest, 1=floor, 2=ceil, 3=truncate |
| `ROUNDSS imm8` | Same, single-precision | |
| `PTEST` | Efficient bit-vector zero test | Used for type-tag checking, `LOGTEST` |
| `BLENDVPD` | Branchless conditional float select | Pattern: `(if test float-a float-b)` |

`ROUNDSD` is the primary motivation for SSE4.1 support — without it, CL
rounding functions (`ROUND`, `TRUNCATE`, `FLOOR`, `CEILING`) require a
multi-instruction software sequence: save MXCSR, set rounding mode,
`CVTSD2SI`, `CVTSI2SD`, restore MXCSR (5+ instructions vs. 1).

**SSE4.1 fallback (SSE2-only):** When SSE4.1 is unavailable, rounding
operations MUST use the MXCSR rounding-mode sequence. The backend MUST
save and restore MXCSR around each rounding operation to avoid corrupting
the rounding mode for subsequent FP instructions.

---

## 4.7.5 AArch64 Backend

### 4.7.5.1 Instruction Selection

| IR Pattern | A64Op | Notes |
|------------|-------|-------|
| `IRAdd(a, b)` int | `AddX` / `AddXi` | 12-bit imm, optional shift |
| `IRMul(a, b)` int | `MulX` | 3-register |
| `IRLoad/Store` | `LdrX` / `StrX` | Scaled/unscaled/register offset |
| `IRFAdd/FMul` f64 | `FaddD` / `FmulD` | NEON scalar |
| `IRCall` | `Blr` | Via `X16`/`X17` scratch |
| `IRBranch` | `Cbz`/`B.cond` | Compare-and-branch / conditional |

### 4.7.5.2 AAPCS64

| Aspect | Rule |
|--------|------|
| Integer args 1-8 | `X0`–`X7` |
| FP args 1-8 | `D0`–`D7` |
| Return | `X0` (int), `D0` (float) |
| Callee-saved | `X19`–`X28`, `D8`–`D15` |
| Frame pointer | `X29` (MUST maintain for unwinding) |
| Link register | `X30` (saved by callee) |
| Stack alignment | 16-byte at all times |

**EGCL-internal (AArch64):** Closure=`X20`, argc=`X2`, args=`X0,X1,X3,X4`,
return=`X0`. The dedicated execution-context register follows §2.3.1;
the execution-context register is `X21`, confirmed by the same audit
(`bliss-q861`): it matches SBCL's arm64 thread register and is not otherwise
reserved here, and AArch64's callee-saved range makes it ride fiber switches the
same way `R13` does on x86-64.

NEON float: `FADD/FSUB/FMUL/FDIV/FCMP` scalar double. `FCMP` sets NZCV;
MUST check V flag for NaN. Conversions: `SCVTF`, `FCVTZS`. FPCR default
NaN and FTZ MUST NOT be modified.

---

## 4.7.6 Register Allocation — Algorithm A4.10 (Linear Scan)

### 4.7.6.1 Live-Interval Construction

1. **Number instructions** sequentially (even positions; odd reserved
   for resolution moves).
2. **Compute live sets** via reverse dataflow: `live_out = ∪ live_in(succ)`;
   walk bottom-to-top removing defs, adding uses.
3. **Build intervals:** for each VReg, `[first_def, last_use]`, extended
   across loop back-edges. Record use positions with register-class
   annotations.

```rust
pub struct LiveInterval {
    pub vreg: VReg,
    pub reg_class: RegClass,
    pub ranges: SmallVec<[LiveRange; 2]>,      // sorted [start, end) pairs
    pub use_positions: SmallVec<[UsePos; 4]>,   // sorted by position
    pub assignment: Option<PReg>,
    pub spill_slot: Option<SpillSlot>,
    pub spill_weight: f32,
}
pub struct LiveRange { pub start: u32, pub end: u32 }
pub struct UsePos { pub pos: u32, pub kind: UsePosKind } // MustHaveReg|ShouldHaveReg|AnyLocation
```

### 4.7.6.2 Allocation Walk

Process intervals by increasing start position:

1. **Expire** active intervals ending before `current.start`; free regs.
2. **Allocate** if a free register exists; add to active set.
3. **Split/evict** if no free register: evict the active interval with
   the furthest next use if it is further than `current`'s next use;
   otherwise split and spill `current`.
4. **Resolution:** insert moves at block boundaries where a value's
   register assignment differs between predecessor and successor.

### 4.7.6.3 Splitting & Spill Slots

Split positions prefer: outside current loop > block boundary > odd
instruction position. Child intervals are re-processed independently.

Spill slots grow downward from the frame pointer. Slots are 8 bytes
(GPR) or 16 bytes (FPR). Non-overlapping intervals of the same class
MAY share a slot (coalescing). Total spill area is recorded in the
function prologue.

**Spill weight:**
```text
weight = Σ(use_kind_cost × 10^loop_depth) / interval_length
  MustHaveReg=4.0, ShouldHaveReg=2.0, AnyLocation=0.5
```
Pinned intervals (physical register constraints) have weight ∞.

---

## 4.7.7 Data Structure D4.12 — CodeBuffer

```rust
pub struct CodeBuffer {
    code: Vec<u8>,                               // raw machine code
    relocs: Vec<Relocation>,                     // pending relocations
    stack_maps: Vec<(u32, StackMap)>,             // offset → GC map
    constants: Vec<ConstantEntry>,               // pool appended after code
    pending_labels: HashMap<MachBlockId, Vec<PatchSite>>,
    label_offsets: HashMap<MachBlockId, u32>,
    src_map: Vec<(u32, SourceLoc)>,              // debug source mapping
}

pub struct Relocation {
    pub offset: u32,
    pub kind: RelocKind,      // Abs64, PcRel32, Branch26, Adrp, AddImm12
    pub target: RelocTarget,  // RuntimeFn | CodePtr | HeapObject | ExternSym
    pub addend: i64,
}
```

**Emission protocol:** (1) Walk MachBlocks in order, emitting MachNodes.
(2) `bind_label` records each block's offset. (3) Branches emit
placeholder offsets → `pending_labels`. (4) After emission, resolve all
labels. If offset overflows: x86-64 upgrades `Jcc rel8` → `rel32`;
AArch64 inserts a veneer for >±128 MB. (5) Append constants pool (16-byte
aligned). (6) Emit external relocations.

---

## 4.7.8 Code Patching Infrastructure

### 4.7.8.1 Use Cases

| Use Case | Trigger | Mechanism |
|----------|---------|-----------|
| IC miss | Polymorphic call | Patch jump target in IC stub |
| Tier promotion | Counter threshold | Redirect old → new code |
| Deoptimisation | Guard failure | Patch return addr to deopt stub |
| GC code update | Moving collector | Patch all incoming references |

### 4.7.8.2 x86-64 Patching

x86-64 guarantees atomic aligned writes ≤8 bytes. Patching `CALL/JMP rel32`
MUST use a single aligned 4-byte write to the displacement field. IC sites
MUST be preceded by a 5-byte NOP sled (`0F 1F 44 00 00`) so concurrent
threads see either old NOP or new jump, never partial. No fence needed
(strong ordering), but the patching thread MUST issue `CPUID`/`MFENCE`
if it may re-enter the patched code.

### 4.7.8.3 AArch64 Patching

Instructions are 4-byte aligned; `STR W` is atomic. After patching:
`DC CVAU` → `DSB ISH` → `IC IVAU` → `DSB ISH` → `ISB`. Other threads
receive `ISB` via safepoint synchronisation (§2). For >128 MB range,
use branch islands with indirect branch via `X16`.

```rust
pub struct PatchPoint {
    pub offset: u32,
    pub length: u8,
    pub kind: PatchKind,         // IC, TierUp, Deopt, GCRelocate
    pub current_target: AtomicU64,
}
```

---

## 4.7.9 GC Stack Maps at Safepoints

```rust
pub struct StackMap {
    pub code_offset: u32,
    pub stack_bitmap: BitVec,    // bit N=1 iff slot N holds a heap ref
    pub live_regs: RegSet,       // registers holding heap refs
    pub frame_slots: u16,
}
```

**Generation:** During register allocation, record live VRegs at each
safepoint. After allocation, map to physical registers / spill slots.
Only heap-pointer types (per type inference §4.4) are recorded; unboxed
fixnums, floats, and raw pointers are excluded.

**Current x86-64 T2 boundary form:** the framed emitter realizes the same map
contract by synchronizing exact live tagged VRegs into dedicated tagged shadow
slots appended to the owning `EgclStack` activation immediately before every
runtime call. Unused shadow slots are cleared to `NIL`; after the call, possibly
relocated values are restored to their allocated GPR or native spill homes.
This makes moving-GC updates explicit without asynchronously inspecting a host
register context. Code that needs shadow roots has no frame-less compiled entry.
The emitted artifact records each sync-site offset plus its register/spill root
counts; installation rejects a missing site, an out-of-range offset, inconsistent
counts, or a shadow-slot range outside the installed activation bitmap. A future
backend may instead expose `live_regs` directly when its safepoint trampoline
publishes a complete machine context.

**Validation (R4.46):** At code installation, verify: every `is_safepoint`
instruction has a stack map entry; all slot indices are valid
(0..frame_slots); live_regs are callee-saved or preserved across the
safepoint. Failures abort installation with `compiler-bug` condition.

---

## 4.7.10 Unwind Information

EGCL emits DWARF `.eh_frame`-compatible unwind information for every
installed function. This enables: (1) debugger stack walks (§6),
(2) condition/restart stack unwinding (§5), and (3) OS signal-handler
cooperation (recovering from SIGSEGV/SIGFPE at safepoints).

```rust
pub struct UnwindInfo {
    pub format: UnwindFormat,
    pub fde_bytes: Vec<u8>,           // Frame Description Entry, DWARF .eh_frame
    pub personality: Option<*const u8>, // pointer to EGCL personality routine
    pub lsda: Option<Vec<u8>>,        // Language-Specific Data Area for condition handlers
}

pub enum UnwindFormat {
    DwarfEhFrame,     // Linux, macOS, FreeBSD — .eh_frame / __eh_frame
    WindowsSeh,       // Windows x86-64 — RUNTIME_FUNCTION + UNWIND_INFO
}
```

**Generation:** Unwind info is constructed during code emission alongside
the `CodeBuffer`. The emitter tracks frame-pointer adjustments, callee-save
register pushes/pops, and stack pointer changes, recording each as a DWARF
CFA (Call Frame Address) instruction in the FDE.

| Event during emission | CFA action |
|-----------------------|------------|
| `push rbp` / `stp x29, x30` | `DW_CFA_def_cfa_register(RBP)` |
| `sub rsp, N` / stack alloc | `DW_CFA_def_cfa_offset(N)` |
| Callee-save push | `DW_CFA_offset(reg, slot)` |
| Callee-save pop (epilogue) | `DW_CFA_restore(reg)` |

**Registration:** At code installation (§4.7.11), the FDE is registered with
the runtime's `.eh_frame` table. On Linux/macOS this uses
`__register_frame` (libgcc/libunwind). On Windows, `RtlAddFunctionTable`
registers `RUNTIME_FUNCTION` entries. Deregistration occurs when the
enclosing `CodeRegion` transitions to `Dead`.

**Personality routine:** EGCL installs a custom DWARF personality routine
(`egcl_personality`) that cooperates with the CL condition system (§5).
It reads the LSDA to determine active `HANDLER-BIND`/`HANDLER-CASE`
frames and routes conditions to the appropriate restart.

---

## 4.7.11 Code Region Management

```rust
pub struct CodeRegion {
    pub base: *mut u8,
    pub size: usize,                     // multiple of page size
    pub cursor: AtomicUsize,
    pub functions: Vec<InstalledFunction>,
    pub state: AtomicU8,                 // Active | Full | Dead
}

pub struct InstalledFunction {
    pub function_id: FunctionId,
    pub code_start: *const u8,
    pub code_size: u32,
    pub stack_maps: Vec<StackMap>,
    pub patch_points: Vec<PatchPoint>,
    pub relocations: Vec<Relocation>,
    pub unwind_info: UnwindInfo,
}
```

**Installation protocol:** (1) Acquire region write lock. (2) `mprotect`
→ RW. (3) Copy code. (4) Apply relocations. (5) Validate stack maps.
(6) Register unwind info (§4.7.10) via `__register_frame` or
`RtlAddFunctionTable`. (7) Register in global code index. (8) `mprotect`
→ RX. (9) AArch64: I-cache invalidation. (10) Advance cursor (16-byte
aligned). (11) Release.

**Lifecycle:** Active → Full (no space) → Dead (all functions superseded;
region unmapped). Region size: 2 MB default (one huge page). No code
compaction within live regions — dead functions are reclaimed only when
the entire region is dead.

---

## 4.7.12 Error Handling

| Error | Response |
|-------|----------|
| Unlowered IR node | Abort; `compiler-bug` condition |
| Impossible register constraint | Abort; `compiler-bug` condition |
| Branch offset overflow | Re-emit with long branch/veneer; abort if still fails |
| Code region exhaustion | Allocate new region; `storage-condition` if OS refuses |
| Stack map validation failure | Abort installation; `compiler-bug` condition |
| `mprotect` failure | `system-error` with errno |
| Unwind registration failure | Abort installation; `system-error` with details |

---

## 4.7.13 Concurrency

- **Installation** serialised per region (mutex); multiple regions concurrent.
- **Patching** lock-free (atomic writes), coordinated via GC safepoints.
- **Allocation & emission** operate on thread-local state, no shared data.
- **Region allocation** (`mmap`) serialised via global allocator lock.

---

## 4.7.14 Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `EGCL_CODE_REGION_SIZE` | `2097152` | Code region size (bytes) |
| `EGCL_MAX_CODE_REGIONS` | `1024` | Max live code regions |
| `EGCL_SPILL_WEIGHT_LOOP_FACTOR` | `10.0` | Loop-depth spill weight base |
| `EGCL_ENABLE_AVX2` | `auto` | `auto` / `on` / `off` |
| `EGCL_ENABLE_SSE41` | `auto` | `auto` / `on` / `off` |
| `EGCL_ENABLE_SVE` | `auto` | `auto` / `on` / `off` |
| `EGCL_IC_MAX_ENTRIES` | `8` | Polymorphic IC → megamorphic threshold |

---

## 4.7.15 Test Strategy

| Category | Method |
|----------|--------|
| Instruction selection | Compile IR patterns → disassemble → assert opcodes |
| ABI compliance | Rust↔EGCL cross-calls with varied signatures, both platforms |
| Register allocation | Synthetic IR with forced splits; verify move resolution |
| Patching atomicity | Concurrent reader/writer stress test on patched loop body |
| GC stack maps | Force GC at every safepoint; verify all live objects traced |
| Code region lifecycle | Fill regions → supersede → verify unmapping via `/proc/self/maps` |
| Branch overflow | Large functions exceeding `rel8`/26-bit limits; verify veneers |
| Float edge cases | NaN comparisons, denormal arithmetic, ±0 semantics |
| Unwind info | Stack walk via `_Unwind_Backtrace`; verify frames match expected call chain |
| SSE4.1 rounding | `ROUND`/`TRUNCATE`/`FLOOR`/`CEILING` correctness with/without SSE4.1 |
