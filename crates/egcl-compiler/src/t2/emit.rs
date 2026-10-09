// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 x86-64 machine-code emission.
//!
//! # Purpose
//!
//! Produce executable x86-64 bytes for a T2 function through the shared label
//! assembler (`egcl_rt::asm::Asm`); the caller maps the result with
//! `egcl_rt::jit::JitBuffer`. Two emitters live here:
//!
//! * [`emit`], the **compact** backend: walks a [`MachFunc`] after register
//!   allocation and encodes each `MachInst` from `inst_allocations`, replaying
//!   `allocation_edits` before and after it. Integer-only, straight-line, and
//!   limited to a handful of mnemonics (`MOV`, `MOV_IMM`/`MOV_TAGGED`,
//!   `ADD`/`SUB`, `LIVENESS`, `RET`). It is the exact-allocation reference path
//!   used by drive.rs and the unit tests; it refuses `INVOKE`.
//! * [`emit_framed`] / [`emit_framed_with_activation_slots`], the **framed**
//!   backend that produces production T2 code. It works from the IR
//!   [`Function`] directly, expanding each instruction from a template with
//!   guards, deopt stubs, runtime-call sequences, shadow-root synchronisation,
//!   OSR entries, and (on Linux) the native-transfer protocol. Lowering and
//!   regalloc2 run only to decide where values live.
//!
//! On AArch64, ppc64le and s390x, `emit_framed_with_activation_slots` dispatches
//! to the sibling emitters; this file is the x86-64 implementation and the
//! shared entry point. Any shape an emitter cannot encode returns an
//! [`EmitError`] and the function stays at T1.
//!
//! # Framed route: value homes and registers
//!
//! 1. `lower::lower` then `regalloc::allocate_framed` (reduced pool first, full
//!    pool on `TooManyLiveRegs`).
//! 2. `x64_frame::select_frame_homes` turns the allocator's split-aware
//!    `value_locations` into one [`ValueHome`](crate::t2::x64_frame::ValueHome)
//!    per value: a callee-saved register if every range agrees, otherwise an
//!    RSP-relative stack slot. Templates address operands only through these
//!    homes.
//! 3. Register discipline: value homes come from the allocator's framed pool
//!    (rcx, r8, rsi, rbx; plus r12–r15 in the full pool). `rax`/`rdx` are
//!    template scratch, `r9`–`r11` are the per-instruction temporary bank
//!    `prepare_framed_inst` loads spilled operands into, and `rdi` holds the
//!    activation slots pointer on entry. The prologue pushes exactly the
//!    callee-saved registers that homes use, pads to keep `rsp` 16-aligned at
//!    calls, and reserves `native_spill_slots * 8` bytes.
//!
//! # Framed route: entries
//!
//! * **Interpreter entry** at offset 0: `extern "C" fn(*mut u64) -> u64`
//!   receiving the EgclStack activation's slots pointer (the frame header is
//!   0x28 bytes below it; see docs/design/t2-frame-layout.md). The prologue
//!   stores that pointer into `frame_base_home` if the function needs it,
//!   loads entry params from `slots[i]` into their homes, and falls into the
//!   entry block.
//! * **Register entry** (`FramedCode::compiled_entry`), when the function is
//!   non-variadic, has no checked parameter declarations, needs no frame base,
//!   and takes at most four params: args arrive in rcx, r8, r9, r10 and are
//!   moved into their homes as one parallel move. Self-calls use it directly
//!   (`emit_call`), bypassing c2i dispatch, with an `rsp` guard that falls back
//!   to the c2i path when the published stack limit is near (bliss-b4fd).
//! * **OSR entries** (`FramedCode::osr_entries`), one per IR `osr_entries`
//!   record: load the live interpreter slots into the homes of the values that
//!   are live at the target block and jump into the loop body.
//!
//! # Framed route: deoptimisation
//!
//! A function with no call-like instruction is pure here and uses one shared
//! whole-function stub that re-enters T0 from the start via `c2i_deopt`. A
//! function that can commit a side effect before a later guard fails gets
//! **precise** per-guard stubs (bliss-mba): each serialises the guard's
//! FrameState scopes, outermost first, as `[function, bcp, nlocals, nstack,
//! locals…, stack…]` into an `rsp` buffer, with each slot read from its home,
//! an immediate, or a `Remat` recipe evaluated in `rax`, then calls
//! `c2i_deopt_t2`. Every instruction that can reach a guard template must carry
//! a FrameState or emission declines. Values named by any FrameState or OSR
//! entry form the deopt-live set and are never recycled early.
//!
//! # Framed route: safepoints and GC roots
//!
//! The collector does not inspect registers. Before every runtime call the
//! emitter stores the exact set of live tagged values into **shadow root
//! slots** appended to the activation after `activation_slots`, clears unused
//! shadow slots to NIL, and after the call reloads each value into its home,
//! because a moving collection updates the activation slot, not the host-stack
//! home. Liveness comes from `value_locations` at program point `mi*2 + 1`
//! (live *after* the call), plus the call's own early uses, restricted to
//! `Tagged`-represented values with a home and excluding values whose inferred
//! type is confined to non-pointer immediates. `shadow_root_slots` is the
//! maximum root count over all sites plus the slice reserved for wide calls
//! (more than three args, or `Invoke`) whose arguments are passed through the
//! activation. Each site is recorded in `root_sync_sites` with its native
//! offset and root counts, and installation refuses the artifact if the count
//! disagrees with `emitted_safepoints`.
//!
//! # Native transfers (Linux x86-64 only)
//!
//! `emit_framed_transfers*` and `emit_framed_native_*` emit through a helper-v2
//! veneer: calls pass a [`TransferCallRequest`] and receive either a value or a
//! pending transfer, cleanup/catch/handler landings are emitted as verified
//! cold edges, and polls are placed at loop headers and every 64 straight-line
//! instructions so any cycle reaches a GC/signal check. The result carries a
//! `SysvTransferTable` binding each site to its emitted return PC
//! (transfer_sites.rs). Ordinary `emit_framed` rejects IR containing cleanup,
//! catch, handler, `Invoke` or `NlxTransfer` instructions.
//!
//! # Output
//!
//! [`FramedCode`]: the bytes, entry offsets, OSR entries, spill and
//! shadow-slot counts, root-sync sites, heap-constant pool slots the client
//! must keep rooted, a sparse bcp→offset map for the JIT viewer, and
//! `has_deopt` (true iff some deopt stub is actually referenced), which gates
//! direct native→native calls.
//!
//! # Debugging
//!
//! * `EGCL_IR_FULL=1` — dump the IR as the emitter sees it.
//! * `EGCL_RA_DBG=1` — each value's home with its allocator ranges, every
//!   FrameState-carrying `MachInst`, every FrameState, and OSR slot decisions.
//! * `EGCL_SAFEPOINT_DBG=1` — roots chosen at each safepoint and the raw
//!   allocator ranges.
//! * `EGCL_FRAMESTATE_DBG=1`, `EGCL_DEOPT_SRC_DBG=1` — the scopes and slot
//!   sources serialised by each precise deopt stub.
//! * `EGCL_NO_DIRECT_SELF_CALL=1` — route self-calls through c2i.

use egcl_rt::asm::{Asm, Cc};

use crate::t2::ir::Function;
use crate::t2::mach::{EditPosition, Location, MachFunc, MachInst, PhysReg, RegClass, VReg};
use crate::t2::slot_map;
use crate::t2::slot_map::ConversionKind;
use crate::t2::x64_frame::{select_frame_homes, ValueHome as FramedHome, GPR_X86};

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "frame_tests.rs"]
mod frame_tests;

/// Why emission could not complete (the function stays at T1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// An opcode this emitter does not encode yet.
    UnsupportedOp(u32),
    /// regalloc2 could not allocate this function.
    RegAlloc(String),
    /// An instruction operand was unexpectedly left in memory after edit insertion.
    Spilled(VReg),
    /// A vreg had no allocation (P6 did not run, or left it unbound).
    Unallocated(VReg),
    /// A `MOV_IMM` with no immediate (lowering did not populate it).
    MissingImm,
    /// A float/XMM operand reached the integer-only compact emitter.
    FloatUnsupported,
    /// Branch resolution failed (out-of-range / unbound label).
    BadBranch,
}

/// x86-64 encodings of the callee-saved GPRs SysV requires a function to
/// preserve (rbx, r12–r15). rbp/rsp are handled separately.
const CALLEE_SAVED_X86: [u8; 5] = [3, 12, 13, 14, 15];

const RAX: u8 = 0;
/// Dedicated GPR scratch exposed to regalloc2 (abstract encoding 13).
const RA_SCRATCH_GPR: u8 = 15;

/// The x86 encoding a GPR `PhysReg` maps to.
fn gpr_enc(p: PhysReg) -> Result<u8, EmitError> {
    if p.class != RegClass::Gpr {
        return Err(EmitError::FloatUnsupported);
    }
    GPR_X86
        .get(p.encoding as usize)
        .copied()
        .ok_or(EmitError::UnsupportedOp(0))
}

// ── x86-64 instruction encoders (REX.W 64-bit forms) ────────────────

/// `REX.W` prefix with the R (reg-field) and B (rm-field) extension bits.
fn rex_w(reg: u8, rm: u8) -> u8 {
    0x48 | ((reg >> 3) & 1) << 2 | ((rm >> 3) & 1)
}

/// ModRM for register-direct (mod=11): reg field + rm field.
fn modrm_rr(reg: u8, rm: u8) -> u8 {
    0xC0 | ((reg & 7) << 3) | (rm & 7)
}

/// `mov r64, imm64` (REX.W, B8+rd, imm64).
fn mov_imm64(a: &mut Asm, dst: u8, imm: i64) {
    a.push(0x48 | ((dst >> 3) & 1)); // REX.W (+B for r8..r15)
    a.push(0xB8 + (dst & 7));
    a.extend_from_slice(&imm.to_le_bytes());
}

/// A two-operand ALU/mov of the form `op r/m64, r64` (dst is r/m, src is reg).
fn alu_rr(a: &mut Asm, opcode: u8, dst: u8, src: u8) {
    a.push(rex_w(src, dst));
    a.push(opcode);
    a.push(modrm_rr(src, dst));
}

fn mov_rr(a: &mut Asm, dst: u8, src: u8) {
    if dst != src {
        alu_rr(a, 0x89, dst, src);
    }
}

fn push_reg(a: &mut Asm, r: u8) {
    if r >= 8 {
        a.push(0x41);
    }
    a.push(0x50 + (r & 7));
}

fn pop_reg(a: &mut Asm, r: u8) {
    if r >= 8 {
        a.push(0x41);
    }
    a.push(0x58 + (r & 7));
}

/// `mov dst, [base + 8*i]` — load an argument/local from its interpreter frame
/// slot. `base` is the slots-pointer register (rdi in the frame-less fast path,
/// or r14 when a frame is set up).
fn mov_from_frame(a: &mut Asm, dst: u8, base: u8, i: usize) {
    let mut rex = 0x48; // REX.W
    if dst >= 8 {
        rex |= 0x04; // REX.R
    }
    if base >= 8 {
        rex |= 0x01; // REX.B
    }
    a.push(rex);
    a.push(0x8B); // mov r64, r/m64
    a.push(0x40 | ((dst & 7) << 3) | (base & 7)); // mod=01(disp8), rm=base
    a.push((8 * i) as u8);
}

/// `mov dst, qword ptr [base + disp8]` for bases other than rsp/r12.
fn mov_mem64_disp8(a: &mut Asm, dst: u8, base: u8, disp: u8) {
    debug_assert_ne!(base & 7, 4, "rsp/r12 needs a SIB byte");
    a.push(rex_w(dst, base));
    a.push(0x8B);
    a.push(0x40 | ((dst & 7) << 3) | (base & 7));
    a.push(disp);
}

/// `mov dst, qword ptr [base + disp]` with an arbitrary signed displacement.
/// `base` must not be rsp/r12 (which requires a SIB byte).
fn load_mem64_disp(a: &mut Asm, dst: u8, base: u8, disp: i32) {
    debug_assert_ne!(base & 7, 4, "rsp/r12 needs a SIB byte");
    a.push(rex_w(dst, base));
    a.push(0x8B);
    if disp == 0 && (base & 7) != 5 {
        a.push(((dst & 7) << 3) | (base & 7));
    } else if (-128..=127).contains(&disp) {
        a.push(0x40 | ((dst & 7) << 3) | (base & 7));
        a.push(disp as u8);
    } else {
        a.push(0x80 | ((dst & 7) << 3) | (base & 7));
        a.extend_from_slice(&disp.to_le_bytes());
    }
}

/// `mov qword ptr [base + disp], src`, paired with [`load_mem64_disp`].
fn store_mem64_disp(a: &mut Asm, base: u8, disp: i32, src: u8) {
    debug_assert_ne!(base & 7, 4, "rsp/r12 needs a SIB byte");
    a.push(rex_w(src, base));
    a.push(0x89);
    if disp == 0 && (base & 7) != 5 {
        a.push(((src & 7) << 3) | (base & 7));
    } else if (-128..=127).contains(&disp) {
        a.push(0x40 | ((src & 7) << 3) | (base & 7));
        a.push(disp as u8);
    } else {
        a.push(0x80 | ((src & 7) << 3) | (base & 7));
        a.extend_from_slice(&disp.to_le_bytes());
    }
}

/// `cmp lhs, qword ptr [base + disp8]` for bases other than rsp/r12.
fn cmp_mem64_disp8(a: &mut Asm, lhs: u8, base: u8, disp: u8) {
    debug_assert_ne!(base & 7, 4, "rsp/r12 needs a SIB byte");
    a.push(rex_w(lhs, base));
    a.push(0x3B);
    a.push(0x40 | ((lhs & 7) << 3) | (base & 7));
    a.push(disp);
}

/// `movzx dst32, byte ptr [base + index + disp8]`. The 32-bit destination
/// zero-extends to 64 bits; using the same register for `dst` and `index` is
/// valid because address calculation reads the old index first.
fn movzx_mem8(a: &mut Asm, dst: u8, base: u8) {
    // `movzx dst, byte [base]` — mod=01/disp8=0. rsp/rbp bases would need a
    // SIB or different mod; the only caller passes SCRATCH (rdx).
    debug_assert!(
        base & 7 != 4 && base & 7 != 5,
        "base needs SIB/disp handling"
    );
    let rex = 0x40 | (((dst >> 3) & 1) << 2) | ((base >> 3) & 1);
    a.push(rex);
    a.extend_from_slice(&[0x0F, 0xB6]);
    a.push(0x40 | ((dst & 7) << 3) | (base & 7)); // mod=01, rm=base
    a.push(0); // disp8 = 0
}

/// `sar r64, imm8`.
/// `mov [rsp + disp], src` (REX.W). rsp as the base register forces a SIB byte
/// (rm=100), so this cannot reuse `mov_from_frame`'s no-SIB encoding. Used by the
/// precise-deopt stubs (bliss-mba) to spill reconstructed slot values.
fn store_to_rsp(a: &mut Asm, src: u8, disp: i32) {
    let mut rex = 0x48u8; // REX.W
    if src >= 8 {
        rex |= 0x04; // REX.R
    }
    a.push(rex);
    a.push(0x89); // mov r/m64, r64
    if disp == 0 {
        a.push(((src & 7) << 3) | 0x04); // mod=00, rm=100 (SIB follows)
        a.push(0x24); // SIB: scale=0 index=none base=rsp
    } else if (-128..=127).contains(&disp) {
        a.push(0x40 | ((src & 7) << 3) | 0x04); // mod=01 (disp8)
        a.push(0x24);
        a.push(disp as u8);
    } else {
        a.push(0x80 | ((src & 7) << 3) | 0x04); // mod=10 (disp32)
        a.push(0x24);
        a.extend_from_slice(&disp.to_le_bytes());
    }
}

/// `mov dst, [rsp + disp]` (REX.W), the reload counterpart to
/// [`store_to_rsp`].
fn load_from_rsp(a: &mut Asm, dst: u8, disp: i32) {
    let mut rex = 0x48u8;
    if dst >= 8 {
        rex |= 0x04;
    }
    a.push(rex);
    a.push(0x8B);
    if disp == 0 {
        a.push(((dst & 7) << 3) | 0x04);
        a.push(0x24);
    } else if (-128..=127).contains(&disp) {
        a.push(0x40 | ((dst & 7) << 3) | 0x04);
        a.push(0x24);
        a.push(disp as u8);
    } else {
        a.push(0x80 | ((dst & 7) << 3) | 0x04);
        a.push(0x24);
        a.extend_from_slice(&disp.to_le_bytes());
    }
}

fn sar_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xF8 | (r & 7)); // /7
    a.push(imm);
}

/// `imul r64, r/m64` (two-operand: dst *= src).
fn imul_rr(a: &mut Asm, dst: u8, src: u8) {
    a.push(rex_w(dst, src));
    a.push(0x0F);
    a.push(0xAF);
    a.push(modrm_rr(dst, src));
}

/// `neg r64` (two's-complement negate; sets OF when negating i64::MIN).
fn neg_r(a: &mut Asm, r: u8) {
    a.push(rex_w(0, r));
    a.push(0xF7);
    a.push(0xD8 | (r & 7)); // /3
}

/// `not r64` (one's complement).
fn not_r(a: &mut Asm, r: u8) {
    a.push(rex_w(0, r));
    a.push(0xF7);
    a.push(0xD0 | (r & 7)); // /2
}

// ── Single-float (XMM) encoders ─────────────────────────────────────
//
// A single-float EgclVal is the immediate `(f32_bits << 32) | 0b100`: the raw
// f32 lives in the high dword, tag 0b100 in the low bits. So the float path
// unpacks with `shr 32` + `movd` into an XMM, computes with a scalar-single SSE
// op, then repacks with `movd` + `shl 32` + `or 4`.

/// `mov r32, imm32` (zero-extends into r64).
fn mov_imm32(a: &mut Asm, r: u8, imm: u32) {
    if r >= 8 {
        a.push(0x41); // REX.B
    }
    a.push(0xB8 + (r & 7));
    a.extend_from_slice(&imm.to_le_bytes());
}

/// `movd xmm, r32` — GPR low dword → XMM (66 0F 6E /r).
fn movd_xmm_r32(a: &mut Asm, xmm: u8, r: u8) {
    a.push(0x66);
    if xmm >= 8 || r >= 8 {
        a.push(0x40 | (((xmm >> 3) & 1) << 2) | ((r >> 3) & 1)); // REX.R(xmm)+B(r)
    }
    a.extend_from_slice(&[0x0F, 0x6E]);
    a.push(modrm_rr(xmm, r)); // reg=xmm, rm=r
}

/// `movd r32, xmm` — XMM low dword → GPR, zero-extended (66 0F 7E /r).
fn movd_r32_xmm(a: &mut Asm, r: u8, xmm: u8) {
    a.push(0x66);
    if xmm >= 8 || r >= 8 {
        a.push(0x40 | (((xmm >> 3) & 1) << 2) | ((r >> 3) & 1)); // REX.R(xmm)+B(r)
    }
    a.extend_from_slice(&[0x0F, 0x7E]);
    a.push(modrm_rr(xmm, r)); // reg=xmm, rm=r
}

/// A scalar-single SSE op `<op>ss xmm_dst, xmm_src` (F3 0F <sub> /r):
/// addss=0x58, mulss=0x59, subss=0x5C.
fn ss_op(a: &mut Asm, sub: u8, dst: u8, src: u8) {
    a.push(0xF3);
    if dst >= 8 || src >= 8 {
        a.push(0x40 | (((dst >> 3) & 1) << 2) | ((src >> 3) & 1));
    }
    a.extend_from_slice(&[0x0F, sub]);
    a.push(modrm_rr(dst, src));
}

/// `shr r64, imm8` (/5).
fn shr_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xE8 | (r & 7));
    a.push(imm);
}

/// `shl r64, imm8` (/4).
fn shl_imm(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0xC1);
    a.push(0xE0 | (r & 7));
    a.push(imm);
}

/// `or r64, imm8` sign-extended (/1).
fn or_imm8(a: &mut Asm, r: u8, imm: u8) {
    a.push(rex_w(0, r));
    a.push(0x83);
    a.push(0xC8 | (r & 7));
    a.push(imm);
}

/// Guard that `r` holds a single-float (tag == 0b100), else deopt.
fn guard_single_float(a: &mut Asm, r: u8, deopt: egcl_rt::asm::Label) {
    alu_rr(a, 0x89, SCRATCH, r); // mov rdx, r
    a.extend_from_slice(&[0x80, 0xE2, 0x07]); // and dl, 7
    a.extend_from_slice(&[0x80, 0xFA, 0x04]); // cmp dl, 4
    a.jcc(Cc::Ne, deopt);
}

// ── Framed emitter (T2, interpreter-callable) ───────────────────────
//
// Emits a straight-line speculated function as native code callable by the T0
// run_native adapter. The slots pointer arrives in rdi; arguments are read
// straight from `[rdi + 8*i]`. The fast path makes no call, so it needs no stack
// frame and no callee-saved register — it works entirely in registers and
// returns in rax. A guard miss jumps to a cold stub that aligns the stack and
// calls `c2i_deopt` (the no-resume deopt that makes run_native re-run the pure
// function in the interpreter). Instruction selection folds a constant operand
// into the multiply immediate and never materialises it, and only the *variable*
// operand is tag-checked — a statically-known fixnum constant needs no guard.

const SCRATCH: u8 = 2; // rdx — reserved for guard temporaries, never a value reg

/// `test <r low byte>, 7 ; jne deopt` — the fixnum-tag guard on one operand.
/// Correct for every GPR. Registers 4..=7 need a bare REX prefix so the r/m8
/// encoding names their LOW byte (spl/bpl/sil/dil) — without it those
/// encodings mean ah/ch/dh/bh, silently testing the wrong register's high
/// byte (bliss-x5y.29: rsi joined the allocatable pool and every fixnum guard
/// on an rsi-homed value became `test dh, 7`, a deopt storm or corruption).
fn guard_fixnum(a: &mut Asm, r: u8, deopt: egcl_rt::asm::Label) {
    if r >= 8 {
        a.push(0x41); // REX.B → r8b..r15b
    } else if r >= 4 {
        a.push(0x40); // bare REX → spl/bpl/sil/dil, not ah/ch/dh/bh
    }
    a.push(0xF6); // test r/m8, imm8  (/0)
    a.push(0xC0 | (r & 7)); // mod=11, rm=r
    a.push(0x07);
    a.jcc(Cc::Ne, deopt);
}

/// `imul dst, src, imm32` — dst = src * imm, setting OF on overflow. Multiplying
/// the *tagged* operand by the raw constant yields the tagged product directly
/// (tagged(x)·c = (x<<3)·c = (x·c)<<3 = tagged(x·c)), so no untag/retag is needed.
fn imul_imm(a: &mut Asm, dst: u8, src: u8, imm: i32) {
    a.push(rex_w(dst, src)); // REX.W + R(dst) + B(src)
    a.push(0x69); // imul r64, r/m64, imm32
    a.push(modrm_rr(dst, src)); // reg=dst, rm=src
    a.extend_from_slice(&imm.to_le_bytes());
}

// Fixnum value range: 3 tag bits leave 61 signed value bits.
const FIXNUM_MAX: i64 = (1 << 60) - 1;
const FIXNUM_MIN: i64 = -(1 << 60);

/// The SIB scale bits for `x·c` expressed as `lea [x + x*(c-1)]`, i.e. c-1 ∈
/// {1,2,4,8}. Returns `None` for multipliers `lea` can't do in one instruction.
fn lea_scale(c: i64) -> Option<u8> {
    match c {
        2 => Some(0b00), // x + x*1
        3 => Some(0b01), // x + x*2
        5 => Some(0b10), // x + x*4
        9 => Some(0b11), // x + x*8
        _ => None,
    }
}

/// `lea dst, [base + base*scale]` — dst = base·(1+2^scale_field). Because it works
/// on the *tagged* operand (tagged(x)·c = tagged(x·c)) it needs no untag; because
/// `lea` sets no flags it can only be used when overflow is proven impossible.
fn lea_mul(a: &mut Asm, dst: u8, base: u8, scale: u8) {
    let mut rex = 0x48; // REX.W
    if dst >= 8 {
        rex |= 0x04; // REX.R (reg = dst)
    }
    if base >= 8 {
        rex |= 0x03; // REX.X + REX.B (both index and base are `base`)
    }
    a.push(rex);
    a.push(0x8D); // lea r64, m
    a.push(((dst & 7) << 3) | 0x04); // mod=00, reg=dst, rm=100 → SIB
    a.push((scale << 6) | ((base & 7) << 3) | (base & 7)); // scale, index=base, base=base
}

/// Whether `range · c` is provably inside the fixnum range — so the tagged
/// product `(range·c)<<3` cannot overflow a 64-bit register and no `jo`/deopt is
/// needed. Conservative: an unknown range, a non-positive `c`, or any arithmetic
/// overflow in the check itself all answer "no".
fn mul_cannot_overflow(range: Option<crate::t2::ir::Range>, c: i64) -> bool {
    let Some(r) = range else { return false };
    if c <= 0 {
        return false;
    }
    match (r.lo.checked_mul(c), r.hi.checked_mul(c)) {
        (Some(lo), Some(hi)) => lo >= FIXNUM_MIN && hi <= FIXNUM_MAX,
        _ => false,
    }
}

/// Get value `v` into a register: an already-assigned one, or a deferred fixnum
/// constant materialised (tagged) into a fresh register on demand.
fn framed_mat(
    a: &mut Asm,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    v: crate::t2::ir::Value,
) -> Result<u8, EmitError> {
    let _ = pool;
    if let Some(&r) = reg.get(&v) {
        return Ok(r);
    }
    if let Some(&c) = consts.get(&v) {
        // Materialise into the scratch register, NOT a pool register: the pool's
        // emit-time state is not a valid free list (block-local reuse returns
        // registers still live at earlier points), so popping it could clobber a
        // live value. The scratch is safe because a general two-operand path uses
        // at most one materialised constant (the other operand is a variable, and
        // both-constant cases are folded upstream / rejected below).
        mov_imm64(a, SCRATCH, egcl_rt::value::EgclVal::from_fixnum(c).0 as i64);
        return Ok(SCRATCH);
    }
    Err(EmitError::UnsupportedOp(0xF2))
}

/// Assign a register to an instruction result — honouring a pre-assignment (the
/// return value is bound to rax so the arithmetic lands in the return register).
fn framed_alloc(
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    v: crate::t2::ir::Value,
) -> Result<u8, EmitError> {
    if let Some(&r) = reg.get(&v) {
        return Ok(r);
    }
    let r = pool.pop().ok_or(EmitError::UnsupportedOp(0xF1))?;
    reg.insert(v, r);
    Ok(r)
}

/// Load one operand of a float op into `xmm`. A fixnum constant is coerced to
/// single-float at compile time (float contagion — `(* 2.5 5)` = `12.5`) and
/// materialised; a variable is guarded to be a single-float, then its f32 bits
/// (high dword of the tagged value) are unpacked into the XMM register.
// Keep the emitter's separate operand maps borrowed; bundling them solely for
// argument count would obscure which parts of the allocation state are read.
#[allow(clippy::too_many_arguments)]
fn float_operand_to_xmm(
    a: &mut Asm,
    xmm: u8,
    v: crate::t2::ir::Value,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    float_consts: &std::collections::HashMap<crate::t2::ir::Value, u32>,
    proven_single_float: &std::collections::HashSet<crate::t2::ir::Value>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    if let Some(&bits) = float_consts.get(&v) {
        mov_imm32(a, SCRATCH, bits);
        movd_xmm_r32(a, xmm, SCRATCH);
    } else if let Some(&c) = consts.get(&v) {
        mov_imm32(a, SCRATCH, (c as f32).to_bits());
        movd_xmm_r32(a, xmm, SCRATCH);
    } else {
        let r = *reg.get(&v).ok_or(EmitError::UnsupportedOp(0xF2))?;
        if !proven_single_float.contains(&v) {
            guard_single_float(a, r, deopt);
        }
        // Preserve the tagged SSA value for later uses and deopt reconstruction.
        mov_rr(a, SCRATCH, r);
        shr_imm(a, SCRATCH, 32); // low dword now holds the f32 bits
        movd_xmm_r32(a, xmm, SCRATCH);
    }
    Ok(())
}

/// A framed T2 compilation with its entry points.
///
/// The code has a single body reached by two entries:
/// - **interpreter entry** at offset 0 — `extern "C" fn(*mut u64 slots) -> u64`
///   (the run_native ABI): a short prologue loads arguments from the frame slots
///   into the argument registers, then falls into the body.
/// - **compiled entry** at [`compiled_entry`](Self::compiled_entry) — arguments
///   are already in the argument registers `[rcx, r8, r9, r10]` (arg0..arg3), so
///   a compiled caller skips the frame load entirely. Result in rax.
pub struct FramedCode {
    pub code: Vec<u8>,
    /// Owned unwind records for the independently callable Windows entries.
    #[cfg(all(target_arch = "x86_64", windows))]
    pub windows_unwind: Vec<WindowsUnwindRange>,
    /// Byte offset of the compiled-caller entry within `code`.
    pub compiled_entry: usize,
    /// Bytecode loop-header bcp to alternate entry offset.  Each entry accepts
    /// the live frame-slot pointer through the platform C ABI (RDI on SysV,
    /// RCX on Win64), reconstructs SSA registers, and enters the optimized loop
    /// without restarting the function.
    pub osr_entries: Vec<(u32, usize)>,
    /// Bytecode→native position map for the tiered-JIT viewer (bliss-zmmb):
    /// `bcp_offsets[bcp]` is the earliest native offset carrying that bytecode
    /// position (`u32::MAX` if none). Sparse — populated only at frame-state
    /// instructions, since the optimizer has no 1:1 mapping. Not used by
    /// execution.
    pub bcp_offsets: Vec<u32>,
    /// Total eight-byte native stack homes reserved by the framed emitter.
    pub native_spill_slots: u32,
    /// Spill slots chosen directly by regalloc2 before split ranges are assigned
    /// stable homes for the rich framed templates.
    pub regalloc_spill_slots: u32,
    /// Spill/reload/live-range-split moves produced by regalloc2.
    pub allocation_edits: usize,
    /// Extra tagged slots appended to the EgclStack activation. At each
    /// runtime safepoint these hold the exact live native roots and are restored
    /// to their GPR/spill homes after a moving collection.
    pub shadow_root_slots: u16,
    /// Number of runtime-call safepoints emitted. Installation verifies that it
    /// matches `root_sync_sites.len()` before publishing code.
    pub emitted_safepoints: usize,
    /// Emitted native offsets and live-root counts for installation validation.
    pub root_sync_sites: Vec<RootSyncSite>,
    /// Constant-pool slot addresses loaded by emitted heap-literal instructions.
    /// The compiler client must keep their owning rooted bodies alive.
    pub heap_constant_slots: Vec<usize>,
    /// True iff the code contains a speculation-guard deopt point (bliss-zhvn):
    /// a non-deopting T2 function is eligible for a direct native→native call.
    pub has_deopt: bool,
}

#[cfg(all(target_arch = "x86_64", windows))]
pub struct WindowsUnwindRange {
    pub begin: u32,
    pub end: u32,
    pub unwind_info: Vec<u8>,
}

/// Common Win64 layout for interpreter, compiled-register and OSR entries.
/// Spill homes remain RSP-relative; RBP anchors unwinding across temporary
/// argument/deopt buffers and runtime helper home-space reservations.
#[cfg(all(target_arch = "x86_64", windows))]
struct WindowsFrame {
    saved: Vec<u8>,
    allocation: u32,
}

#[cfg(all(target_arch = "x86_64", windows))]
impl WindowsFrame {
    fn new(saved: &[u8], spill_bytes: usize) -> Result<Self, EmitError> {
        let mut saved = saved.to_vec();
        // Templates use RSI/RDI even when regalloc did not choose them as homes.
        saved.extend([5, 6, 7]);
        saved.sort_unstable();
        saved.dedup();
        let pad = if saved.len() % 2 == 0 { 8 } else { 0 };
        let allocation = spill_bytes
            .checked_add(pad)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or(EmitError::UnsupportedOp(0xFD))? as u32;
        Ok(Self { saved, allocation })
    }

    fn prologue(&self, a: &mut Asm) -> Vec<u8> {
        let start = a.here();
        let mut codes = Vec::new();
        for &reg in &self.saved {
            push_reg(a, reg);
            codes.push(vec![(a.here() - start) as u8, reg << 4]);
        }
        probe_windows_stack(a, self.allocation);
        if self.allocation != 0 {
            a.extend_from_slice(&[0x48, 0x81, 0xEC]);
            a.extend_from_slice(&self.allocation.to_le_bytes());
            let offset = (a.here() - start) as u8;
            let alloc = self.allocation;
            if alloc <= 128 {
                codes.push(vec![offset, (((alloc - 8) / 8) as u8) << 4 | 2]);
            } else if alloc / 8 <= u16::MAX as u32 {
                let mut code = vec![offset, 1]; // UWOP_ALLOC_LARGE, scaled u16
                code.extend_from_slice(&((alloc / 8) as u16).to_le_bytes());
                codes.push(code);
            } else {
                let mut code = vec![offset, 0x11]; // UWOP_ALLOC_LARGE, unscaled u32
                code.extend_from_slice(&alloc.to_le_bytes());
                codes.push(code);
            }
        }
        mov_rr(a, 5, 4); // mov rbp, rsp
        let prologue_len = (a.here() - start) as u8;
        codes.push(vec![prologue_len, 3]); // UWOP_SET_FPREG
        let slots: usize = codes.iter().map(|code| code.len() / 2).sum();
        let mut info = vec![1, prologue_len, slots as u8, 5];
        for code in codes.into_iter().rev() {
            info.extend(code);
        }
        info.resize((info.len() + 3) & !3, 0);
        info
    }

    fn epilogue(&self, a: &mut Asm) {
        // This exact LEA + POP* + RET form is recognized by RtlVirtualUnwind.
        a.extend_from_slice(&[0x48, 0x8D, 0xA5]); // lea rsp, [rbp+disp32]
        a.extend_from_slice(&self.allocation.to_le_bytes());
        for &reg in self.saved.iter().rev() {
            pop_reg(a, reg);
        }
    }
}

#[cfg(all(target_arch = "x86_64", windows))]
fn probe_windows_stack(a: &mut Asm, allocation: u32) {
    if allocation >= 4096 {
        // Probe each intervening page without changing RSP or any entry
        // argument (including custom compiled arg3 in R10). Stable value homes
        // exclude both scratch registers, so this also works before temporary
        // deopt/move buffers. In a prologue, only the earlier pushes need undoing
        // if a probe faults, before ALLOC and SET_FPREG take effect.
        mov_rr(a, 11, 4);
        mov_imm32(a, RAX, allocation);
        let page = a.label();
        let last = a.label();
        a.bind(page);
        a.extend_from_slice(&[0x48, 0x3D, 0, 16, 0, 0]); // cmp rax, 4096
        a.jcc(Cc::L, last); // allocation is a checked, nonnegative i32
        a.extend_from_slice(&[0x49, 0x81, 0xEB, 0, 16, 0, 0]); // sub r11, 4096
        a.extend_from_slice(&[0x41, 0xF6, 0x03, 0]); // test byte [r11], 0
        a.extend_from_slice(&[0x48, 0x2D, 0, 16, 0, 0]); // sub rax, 4096
        a.jmp(page);
        a.bind(last);
        alu_rr(a, 0x29, 11, RAX); // sub r11, rax
        a.extend_from_slice(&[0x41, 0xF6, 0x03, 0]);
    }
}

fn jump_to_shared_body(a: &mut Asm, target: egcl_rt::asm::Label) {
    if cfg!(windows) {
        // An unconditional jump outside the current RUNTIME_FUNCTION is a
        // tail return to the Windows unwinder. This still has a live frame.
        a.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax (scratch)
        a.jcc(Cc::E, target);
    } else {
        a.jmp(target);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootSyncSite {
    pub code_offset: u32,
    pub live_roots: u16,
    pub register_roots: u16,
    pub spill_roots: u16,
}

/// A native callee a T2 `Call` site may enter directly, resolved by the
/// runtime on the execution thread before the compile job is queued
/// (bliss-6j6pk). Plain data: the compile runs on a worker that cannot see
/// the thread-local native registry, so everything the emitted code needs is
/// baked here, and `generation` is the direct-call generation captured before
/// the lookup so a concurrent redefinition can only make the baked guard
/// fail, never pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectNativeTarget {
    pub symbol: u32,
    pub nargs: u16,
    /// The callee's native entry, `fn(slots, egcl_stack)`.
    pub entry: u64,
    /// The callee's `CodeInfo` pointer, stored in the pushed frame header.
    pub code_info: u64,
    /// The callee's activation size in tagged words.
    pub num_slots: u16,
    pub generation: u64,
    /// Address of the runtime's direct-call generation word.
    pub generation_addr: u64,
    /// The frame-header flag word for an ordinary call frame.
    pub frame_flags: u32,
}

/// The branch condition a fixnum comparison opcode is true under. `None` for
/// non-fixnum-comparison opcodes (float comparisons aren't fused yet).
fn fixnum_cmp_cc(op: crate::t2::ir::Opcode) -> Option<Cc> {
    use crate::t2::ir::Opcode::*;
    Some(match op {
        FixnumCmpLt => Cc::L,
        FixnumCmpLe => Cc::Le,
        FixnumCmpGt => Cc::G,
        FixnumCmpGe => Cc::Ge,
        FixnumCmpEq => Cc::E,
        _ => return None,
    })
}

/// The condition for the same comparison with its operands swapped (`a<b` ≡ `b>a`).
fn cc_swapped(cc: Cc) -> Cc {
    match cc {
        Cc::L => Cc::G,
        Cc::G => Cc::L,
        Cc::Le => Cc::Ge,
        Cc::Ge => Cc::Le,
        Cc::E | Cc::Ne | Cc::O | Cc::No => cc,
    }
}

/// `<alu> r64, imm32` — the /digit `ext` selects add=0, sub=5, cmp=7.
fn alu_r_imm(a: &mut Asm, ext: u8, r: u8, imm: i32) {
    a.push(rex_w(0, r));
    a.push(0x81);
    a.push(0xC0 | (ext << 3) | (r & 7));
    a.extend_from_slice(&imm.to_le_bytes());
}

/// `cmp l, r` — signed compare (l − r), setting flags for a following jcc.
fn cmp_rr(a: &mut Asm, l: u8, r: u8) {
    a.push(rex_w(l, r));
    a.push(0x3B); // cmp r64, r/m64 (reg=l, rm=r)
    a.push(modrm_rr(l, r));
}

#[derive(Clone, Copy)]
enum HomeMoveSrc {
    Home(FramedHome),
    Const(u64),
}

fn framed_stack_disp(slot: u32) -> i32 {
    (slot as i32) * 8
}

fn load_home(a: &mut Asm, dst: u8, home: FramedHome, rsp_adjust: i32) {
    match home {
        FramedHome::Reg(src) => mov_rr(a, dst, src),
        FramedHome::Stack(slot) => load_from_rsp(a, dst, framed_stack_disp(slot) + rsp_adjust),
    }
}

fn store_home(a: &mut Asm, home: FramedHome, src: u8, rsp_adjust: i32) {
    match home {
        FramedHome::Reg(dst) => mov_rr(a, dst, src),
        FramedHome::Stack(slot) => store_to_rsp(a, src, framed_stack_disp(slot) + rsp_adjust),
    }
}

fn emit_shadow_root_sync(
    a: &mut Asm,
    roots: &[crate::t2::ir::Value],
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    frame_base: FramedHome,
    activation_slots: u16,
    shadow_slots: u16,
) -> Result<(), EmitError> {
    load_home(a, SCRATCH, frame_base, 0);
    // A single bitmap covers every shadow slot. Clear unused slots at each site
    // so stale values from a previous safepoint are not retained as false roots.
    mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
    for i in 0..shadow_slots {
        let slot = activation_slots as i32 + i as i32;
        store_mem64_disp(a, SCRATCH, slot * 8, RAX);
    }
    for (i, value) in roots.iter().enumerate() {
        let home = *homes.get(value).ok_or(EmitError::UnsupportedOp(0xFC))?;
        load_home(a, RAX, home, 0);
        let slot = activation_slots as i32 + i as i32;
        store_mem64_disp(a, SCRATCH, slot * 8, RAX);
    }
    Ok(())
}

fn emit_shadow_root_restore(
    a: &mut Asm,
    synced_roots: &[crate::t2::ir::Value],
    restore_roots: &std::collections::HashSet<crate::t2::ir::Value>,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    frame_base: FramedHome,
    activation_slots: u16,
) -> Result<(), EmitError> {
    load_home(a, SCRATCH, frame_base, 0);
    for (i, value) in synced_roots.iter().enumerate() {
        if !restore_roots.contains(value) {
            continue;
        }
        let slot = activation_slots as i32 + i as i32;
        load_mem64_disp(a, RAX, SCRATCH, slot * 8);
        let home = *homes.get(value).ok_or(EmitError::UnsupportedOp(0xFC))?;
        store_home(a, home, RAX, 0);
    }
    Ok(())
}

/// One home-to-home move, RAX as intermediary where x86 needs one. RAX is
/// never a value home (framed_machine_env hands out only rcx/r8/rbx/r12-r15),
/// so it is always free scratch here.
fn emit_home_move(a: &mut Asm, dst: FramedHome, src: HomeMoveSrc) {
    match (dst, src) {
        (FramedHome::Reg(d), HomeMoveSrc::Home(FramedHome::Reg(s))) => mov_rr(a, d, s),
        (FramedHome::Reg(d), HomeMoveSrc::Home(h)) => load_home(a, d, h, 0),
        (FramedHome::Reg(d), HomeMoveSrc::Const(bits)) => mov_imm64(a, d, bits as i64),
        (FramedHome::Stack(_), HomeMoveSrc::Home(h)) => {
            load_home(a, RAX, h, 0);
            store_home(a, dst, RAX, 0);
        }
        (FramedHome::Stack(_), HomeMoveSrc::Const(bits)) => {
            mov_imm64(a, RAX, bits as i64);
            store_home(a, dst, RAX, 0);
        }
    }
}

/// Materialise a simultaneous set of block-parameter moves. Identity moves are
/// dropped; the rest are emitted in an order where no destination is written
/// while a pending move still reads it (bliss-x5y.28: the previous
/// unconditional stack snapshot cost a sub-rsp + store/reload round trip PER
/// MOVE — two of them on every fib activation for what is a single register
/// move). Only a residual cycle takes the snapshot path, which captures every
/// remaining source before writing any destination.
fn parallel_home_move(a: &mut Asm, moves: &[(FramedHome, HomeMoveSrc)]) {
    let mut pending: Vec<(FramedHome, HomeMoveSrc)> = moves
        .iter()
        .filter(|(dst, src)| !matches!(src, HomeMoveSrc::Home(h) if h == dst))
        .copied()
        .collect();
    loop {
        let mut progressed = false;
        let mut i = 0;
        while i < pending.len() {
            let dst = pending[i].0;
            let still_read = pending
                .iter()
                .any(|(_, src)| matches!(src, HomeMoveSrc::Home(h) if *h == dst));
            if still_read {
                i += 1;
                continue;
            }
            let (dst, src) = pending.swap_remove(i);
            emit_home_move(a, dst, src);
            progressed = true;
        }
        if pending.is_empty() {
            return;
        }
        if !progressed {
            break; // every pending destination is still read: a cycle
        }
    }
    parallel_home_move_snapshot(a, &pending);
}

/// The general cyclic case: a temporary stack snapshot captures every source
/// before any destination is written.
fn parallel_home_move_snapshot(a: &mut Asm, moves: &[(FramedHome, HomeMoveSrc)]) {
    if moves.is_empty() {
        return;
    }
    let bytes = ((moves.len() * 8) + 15) & !15;
    #[cfg(all(target_arch = "x86_64", windows))]
    probe_windows_stack(a, bytes as u32);
    a.extend_from_slice(&[0x48, 0x81, 0xEC]);
    a.extend_from_slice(&(bytes as i32).to_le_bytes());
    for (i, (_, src)) in moves.iter().enumerate() {
        match src {
            HomeMoveSrc::Home(home) => load_home(a, RAX, *home, bytes as i32),
            HomeMoveSrc::Const(bits) => mov_imm64(a, RAX, *bits as i64),
        }
        store_to_rsp(a, RAX, (i * 8) as i32);
    }
    for (i, (dst, _)) in moves.iter().enumerate() {
        load_from_rsp(a, RAX, (i * 8) as i32);
        store_home(a, *dst, RAX, bytes as i32);
    }
    a.extend_from_slice(&[0x48, 0x81, 0xC4]);
    a.extend_from_slice(&(bytes as i32).to_le_bytes());
}

/// Emit one arithmetic instruction (result register pre-assigned). Operands are
/// read from `reg`/`consts`; the result lands in its allocated register.
// These are disjoint mutable allocation maps and immutable analysis inputs.
#[allow(clippy::too_many_arguments)]
fn emit_arith_inst(
    a: &mut Asm,
    f: &Function,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    float_consts: &std::collections::HashMap<crate::t2::ir::Value, u32>,
    val_range: &std::collections::HashMap<crate::t2::ir::Value, (i64, i64)>,
    proven: &mut std::collections::HashSet<crate::t2::ir::Value>,
    proven_single_float: &std::collections::HashSet<crate::t2::ir::Value>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    // Robustness: never index past the operands — decline (=> stay T1) instead of
    // panicking in the JIT path if a speculated op has an unexpected shape.
    let is_unary = matches!(data.opcode, Opcode::FixnumNeg | Opcode::LogNot);
    if data.results.is_empty() || data.args.len() < if is_unary { 1 } else { 2 } {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    // A binary op with BOTH operands constant should have been folded by the
    // mid-end; decline rather than risk two constants contending for the scratch.
    if !is_unary && consts.contains_key(&data.args[0]) && consts.contains_key(&data.args[1]) {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let dst = framed_alloc(reg, pool, data.results[0])?;
    let tagged32 = |c: i64| i32::try_from(egcl_rt::value::EgclVal::from_fixnum(c).0 as i64);
    // Guard a variable operand is a fixnum — unless it is a constant (known
    // fixnum) or already proven (a dominating guard, or an earlier guard in this
    // block). Recording each guarded operand makes a later use in the same block
    // skip its re-guard.
    let mut guard = |a: &mut Asm, v: crate::t2::ir::Value, r: u8| {
        if !proven.contains(&v) && !consts.contains_key(&v) {
            guard_fixnum(a, r, deopt);
            proven.insert(v);
        }
    };
    match data.opcode {
        Opcode::FixnumMul => {
            let (a0, b0) = (data.args[0], data.args[1]);
            let folded = if let Some(&c) = consts.get(&b0) {
                Some((a0, c))
            } else {
                consts.get(&a0).map(|&c| (b0, c))
            };
            if let Some((var, c)) = folded {
                // Fold the constant, or decline if it does not fit the imul immediate
                // — the general two-register path uses rdx for the guard and cannot
                // also host a materialised constant there.
                let c32 =
                    i32::try_from(c).map_err(|_| EmitError::UnsupportedOp(op_tag(data.opcode)))?;
                let range = f.value(var).ty.range;
                let x = *reg.get(&var).ok_or(EmitError::UnsupportedOp(0xF2))?;
                guard(a, var, x);
                if mul_cannot_overflow(range, c) {
                    match lea_scale(c) {
                        Some(s) => lea_mul(a, dst, x, s),
                        None => imul_imm(a, dst, x, c32),
                    }
                } else {
                    imul_imm(a, dst, x, c32);
                    a.jcc(Cc::O, deopt);
                }
                return Ok(());
            }
            let x = framed_mat(a, reg, pool, consts, a0)?;
            let y = framed_mat(a, reg, pool, consts, b0)?;
            if !(proven.contains(&a0) && proven.contains(&b0)) {
                alu_rr(a, 0x89, SCRATCH, x);
                alu_rr(a, 0x09, SCRATCH, y);
                a.extend_from_slice(&[0xF6, 0xC2, 0x07]);
                a.jcc(Cc::Ne, deopt);
            }
            if dst == x && x == y {
                // Preserve the tagged RHS before untagging the coalesced result.
                // rdx is excluded from the framed allocator and is free once the
                // combined tag guard above has completed.
                mov_rr(a, SCRATCH, y);
                sar_imm(a, dst, 3);
                imul_rr(a, dst, SCRATCH);
            } else if dst == y && dst != x {
                // Multiplication is commutative: untag the RHS in place and use
                // the untouched tagged LHS, avoiding an otherwise destructive
                // `mov dst, x`.
                sar_imm(a, dst, 3);
                imul_rr(a, dst, x);
            } else {
                mov_rr(a, dst, x);
                sar_imm(a, dst, 3);
                imul_rr(a, dst, y);
            }
            a.jcc(Cc::O, deopt);
        }
        Opcode::FixnumAdd | Opcode::FixnumSub => {
            let is_add = data.opcode == Opcode::FixnumAdd;
            let (a0, b0) = (data.args[0], data.args[1]);
            // Fold a constant right operand (add/sub var, const), or a constant left
            // operand for the commutative add.
            let folded = if let Some(&c) = consts.get(&b0) {
                Some((a0, c))
            } else if is_add {
                consts.get(&a0).map(|&c| (b0, c))
            } else {
                None
            };
            if let Some((var, c)) = folded {
                if let Ok(t32) = tagged32(c) {
                    let x = *reg.get(&var).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    guard(a, var, x);
                    mov_rr(a, dst, x);
                    alu_r_imm(a, if is_add { 0 } else { 5 }, dst, t32);
                    a.jcc(Cc::O, deopt);
                    return Ok(());
                }
            }
            let x = framed_mat(a, reg, pool, consts, a0)?;
            let y = framed_mat(a, reg, pool, consts, b0)?;
            guard(a, a0, x);
            guard(a, b0, y);
            let op = if is_add { 0x01 } else { 0x29 };
            if dst == y && dst != x {
                if is_add {
                    // Addition is commutative, so accumulate the untouched LHS
                    // into the RHS/result register without a destructive move.
                    alu_rr(a, op, dst, x);
                } else {
                    // Subtraction is not commutative: preserve RHS in reserved
                    // rdx before overwriting the coalesced destination.
                    mov_rr(a, SCRATCH, y);
                    mov_rr(a, dst, x);
                    alu_rr(a, op, dst, SCRATCH);
                }
            } else {
                mov_rr(a, dst, x);
                alu_rr(a, op, dst, y);
            }
            a.jcc(Cc::O, deopt);
        }
        Opcode::FixnumNeg => {
            let a0 = data.args[0];
            let x = framed_mat(a, reg, pool, consts, a0)?;
            guard(a, a0, x);
            mov_rr(a, dst, x);
            neg_r(a, dst); // tagged(-x) = -(x<<3)
            a.jcc(Cc::O, deopt); // negating the most-negative fixnum overflows
        }
        Opcode::LogAnd | Opcode::LogOr | Opcode::LogXor => {
            // Exact on the tagged representation: tagged(a) OP tagged(b) =
            // (a OP b)<<3 = tagged(a OP b). No untag, no overflow.
            let (a0, b0) = (data.args[0], data.args[1]);
            let (rr_op, imm_ext) = match data.opcode {
                Opcode::LogAnd => (0x21u8, 4u8),
                Opcode::LogOr => (0x09, 1),
                Opcode::LogXor => (0x31, 6),
                _ => unreachable!(),
            };
            let folded = if let Some(&c) = consts.get(&b0) {
                Some((a0, c))
            } else {
                consts.get(&a0).map(|&c| (b0, c))
            };
            if let Some((var, c)) = folded {
                if let Ok(t32) = tagged32(c) {
                    let x = *reg.get(&var).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    guard(a, var, x);
                    mov_rr(a, dst, x);
                    alu_r_imm(a, imm_ext, dst, t32); // OP dst, tagged(c)
                    return Ok(());
                }
            }
            let x = framed_mat(a, reg, pool, consts, a0)?;
            let y = framed_mat(a, reg, pool, consts, b0)?;
            guard(a, a0, x);
            guard(a, b0, y);
            if dst == y && dst != x {
                // All three bitwise operations are commutative.
                alu_rr(a, rr_op, dst, x);
            } else {
                mov_rr(a, dst, x);
                alu_rr(a, rr_op, dst, y);
            }
        }
        Opcode::LogNot => {
            let a0 = data.args[0];
            let x = framed_mat(a, reg, pool, consts, a0)?;
            guard(a, a0, x);
            mov_rr(a, dst, x);
            not_r(a, dst);
            alu_r_imm(a, 4, dst, -8); // and dst, ~7 → clears the tag bits: tagged(~a)
        }
        Opcode::FixnumShl => {
            // (ash x n) by a CONSTANT n. Left (n>=0) is x*2^n (tagged(x)*2^n =
            // tagged(x<<n)) with an overflow check; right (n<0) untags, arithmetic
            // -shifts, and retags (no overflow). A variable amount declines.
            let (a0, amt) = (data.args[0], data.args[1]);
            let n = *consts
                .get(&amt)
                .ok_or(EmitError::UnsupportedOp(op_tag(data.opcode)))?;
            let x = framed_mat(a, reg, pool, consts, a0)?;
            guard(a, a0, x);
            if n >= 0 {
                if n > 30 {
                    return Err(EmitError::UnsupportedOp(op_tag(data.opcode))); // 2^n > imm32
                }
                // If the operand's range proves x<<n stays in fixnum range, use a
                // plain shift (no overflow trap) — e.g. after a mask. Otherwise the
                // overflow-checked multiply by 2^n.
                let safe = val_range.get(&a0).is_some_and(|&(lo, hi)| {
                    lo >= 0 && hi.checked_shl(n as u32).is_some_and(|v| v <= FIXNUM_MAX)
                });
                if safe {
                    mov_rr(a, dst, x);
                    shl_imm(a, dst, n as u8); // tagged(x)<<n = tagged(x<<n)
                } else {
                    imul_imm(a, dst, x, 1i32 << n); // dst = x * 2^n, with overflow check
                    a.jcc(Cc::O, deopt);
                }
            } else {
                let k = (-n).min(60) as u8; // right shift by |n|, x86 count kept valid
                mov_rr(a, dst, x);
                sar_imm(a, dst, 3 + k); // (x<<3) sar (3+k) = x sar k
                shl_imm(a, dst, 3); // retag
            }
        }
        Opcode::FloatMul | Opcode::FloatAdd | Opcode::FloatSub => {
            float_operand_to_xmm(
                a,
                0,
                data.args[0],
                reg,
                consts,
                float_consts,
                proven_single_float,
                deopt,
            )?;
            float_operand_to_xmm(
                a,
                1,
                data.args[1],
                reg,
                consts,
                float_consts,
                proven_single_float,
                deopt,
            )?;
            let sub = match data.opcode {
                Opcode::FloatAdd => 0x58,
                Opcode::FloatMul => 0x59,
                Opcode::FloatSub => 0x5C,
                _ => unreachable!(),
            };
            ss_op(a, sub, 0, 1);
            movd_r32_xmm(a, 0, 0);
            shl_imm(a, 0, 32);
            or_imm8(a, 0, 4);
            if dst != 0 {
                mov_rr(a, dst, 0);
            }
        }
        other => return Err(EmitError::UnsupportedOp(op_tag(other))),
    }
    Ok(())
}

/// One pending call-argument move: `dst ← src`, where the source is either a
/// register or a tagged immediate.
enum ArgMoveSrc {
    Reg(u8),
    Imm(u64),
}

/// Move call arguments into their fixed argument registers as one SIMULTANEOUS
/// parallel move (bliss-lwws). A naive in-order loop is wrong: a later
/// argument's source register may itself be an argument register already
/// written by an earlier move — REDUCE's `:from-end` funcall had `acc` homed in
/// rcx, so `mov rcx,r9; mov r8,rcx` called `fn(x, x)` and silently corrupted
/// every right-fold (the ASDF STAMP failure). Emit only moves whose destination
/// no pending move still reads; break register cycles through a caller-saved
/// scratch that no pending move touches (caller-saved registers outside the
/// move set are dead here — the call is emitted immediately after, and values
/// live across a call never have caller-saved homes).
fn emit_call_arg_moves(a: &mut Asm, mut pending: Vec<(u8, ArgMoveSrc)>) -> Result<(), EmitError> {
    pending.retain(|(dst, src)| !matches!(src, ArgMoveSrc::Reg(s) if *s == *dst));
    while !pending.is_empty() {
        let mut progressed = false;
        let mut i = 0;
        while i < pending.len() {
            let dst = pending[i].0;
            let still_read = pending
                .iter()
                .any(|(_, src)| matches!(src, ArgMoveSrc::Reg(s) if *s == dst));
            if still_read {
                i += 1;
                continue;
            }
            let (dst, src) = pending.swap_remove(i);
            match src {
                ArgMoveSrc::Reg(s) => mov_rr(a, dst, s),
                ArgMoveSrc::Imm(bits) => mov_imm64(a, dst, bits as i64),
            }
            progressed = true;
        }
        if progressed {
            continue;
        }
        // Every pending destination is still read by some pending move: a
        // register cycle. Stash one source in a free caller-saved scratch and
        // redirect its readers there.
        const CALLER_SAVED: [u8; 9] = [0, 11, 10, 9, 2, 1, 8, 6, 7];
        let scratch = CALLER_SAVED
            .into_iter()
            .find(|r| {
                pending
                    .iter()
                    .all(|(dst, src)| dst != r && !matches!(src, ArgMoveSrc::Reg(s) if s == r))
            })
            .ok_or(EmitError::UnsupportedOp(0xF3))?;
        // A stall means every pending destination is still read by some pending
        // source, so a source that is itself a pending destination must exist —
        // stashing it elsewhere is what unblocks the cycle. Decline rather than
        // loop forever if that ever fails to hold.
        let victim = pending
            .iter()
            .find_map(|(_, src)| match src {
                ArgMoveSrc::Reg(s) if pending.iter().any(|(dst, _)| dst == s) => Some(*s),
                _ => None,
            })
            .ok_or(EmitError::UnsupportedOp(0xF3))?;
        mov_rr(a, scratch, victim);
        for (_, src) in pending.iter_mut() {
            if matches!(src, ArgMoveSrc::Reg(s) if *s == victim) {
                *src = ArgMoveSrc::Reg(scratch);
            }
        }
    }
    Ok(())
}

// ── Direct builtin calls (bliss-x5y.27) ───────────────────────────
//
// A compiled call to a builtin otherwise goes out through c2i and has its
// callee resolved BY NAME on every call. The table of directly-callable
// builtins lives in the interpreter crate, which depends on THIS one, so the
// runtime installs these hooks at startup rather than the compiler reaching
// upward for them.

/// Installed by the runtime: the direct-call adapter's address, an emit-time
/// resolver from (symbol, argument count) to a table slot, and a reader for the
/// invalidation generation the call site bakes.
pub struct DirectBuiltinHooks {
    /// The register-argument adapter (x86-64's convention).
    pub addr: u64,
    /// The slice-argument adapter, for emitters that pass arguments through
    /// the activation's contiguous argument area (s390x).
    pub slice_addr: u64,
    pub resolve: fn(u32, usize) -> Option<u32>,
    pub generation: fn() -> u64,
}

static DIRECT_BUILTIN: std::sync::OnceLock<DirectBuiltinHooks> = std::sync::OnceLock::new();

/// Install the direct-builtin hooks. Idempotent; later calls are ignored.
/// The rooting variant of the slice call adapter (see
/// `emit_s390x::RuntimeCalls::call_slice_rooted`), installed by the runtime.
static CALL_SLICE_ROOTED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

pub fn install_call_slice_rooted(addr: u64) {
    let _ = CALL_SLICE_ROOTED.set(addr);
}

fn call_slice_rooted_addr() -> u64 {
    CALL_SLICE_ROOTED.get().copied().unwrap_or(0)
}

pub fn install_direct_builtin_hooks(hooks: DirectBuiltinHooks) {
    let _ = DIRECT_BUILTIN.set(hooks);
}

/// `(adapter address, table slot, generation to bake)` when this call site may
/// call a builtin directly, else `None`. `None` whenever the hooks are not
/// installed, so a compiler used without the runtime simply emits c2i.
fn direct_builtin_for(sym: u32, nargs: usize) -> Option<(u64, u32, u64)> {
    let hooks = DIRECT_BUILTIN.get()?;
    let slot = (hooks.resolve)(sym, nargs)?;
    Some((hooks.addr, slot, (hooks.generation)()))
}

/// [`direct_builtin_for`] for an emitter that passes the arguments as a slice:
/// `(slice adapter address, table slot, generation to bake)`.
pub(super) fn direct_builtin_slice_for(sym: u32, nargs: usize) -> Option<(u64, u32, u64)> {
    let hooks = DIRECT_BUILTIN.get()?;
    if hooks.slice_addr == 0 {
        return None;
    }
    let slot = (hooks.resolve)(sym, nargs)?;
    Some((hooks.slice_addr, slot, (hooks.generation)()))
}

/// Emit a `Call` through the interpreter adapter. Up to three arguments use the
/// c2i argument registers (rdx, rcx, r8); wider calls use a GC-scanned slice in
/// the owning activation. Values live across the call are in callee-saved
/// registers, so the call cannot clobber them; the result returns in rax.
// Explicit maps and ABI hooks keep caller-specific emission inputs visible.
#[allow(clippy::too_many_arguments)]
fn emit_call(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    c2i_call_addr: u64,
    c2i_call_slice_addr: u64,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
    frame_base: Option<FramedHome>,
    activation_slots: Option<u16>,
    call_arg_base: u16,
    self_sym: Option<u32>,
    self_entry: Option<egcl_rt::asm::Label>,
    self_arity: usize,
    named_calls: &[NamedCallTarget],
) -> Result<(), EmitError> {
    use crate::t2::ir::AuxData;
    let sym = match data.aux {
        AuxData::CallTarget(s) => s,
        _ => return Err(EmitError::UnsupportedOp(0xF8)),
    };
    let nargs = data.args.len();
    let named = named_calls.iter().find(|target| target.symbol == sym);
    // A named slot speaks the Lisp native ABI. Only its interpreter/legacy
    // bridge crosses into Rust and changes fault-recovery state.
    let named_recovery = if named.is_some() && cfg!(all(target_arch = "x86_64", unix)) {
        0
    } else {
        c2i_recovery_toggle_addr
    };
    // Direct self-call: the target is this very function and it has a register
    // entry — place args in the arg registers and `call` our own entry, skipping
    // c2i dispatch entirely (the register entry saves/restores our callee-saved
    // registers, so our live values survive).
    if let (Some(ss), Some(entry)) = (self_sym, self_entry) {
        const ARG_REGS: [u8; 4] = [1, 8, 9, 10]; // rcx, r8, r9, r10
                                                 // EGCL_NO_DIRECT_SELF_CALL routes self-calls back through c2i.
                                                 //
                                                 // The direct self-call skips c2i_call_args, and with it the
                                                 // native_depth_cap() check that is the ONLY bound on recursion depth in
                                                 // compiled code — the T2 prologue has no stack guard. A deeply
                                                 // self-recursive function therefore runs off the C stack and returns a
                                                 // WRONG ANSWER rather than signalling: (deep 400000) answers 30, and
                                                 // (deep 200000) answers a raw stack address. T0 and T1 both raise the
                                                 // STORAGE-CONDITION they should (bliss-b4fd).
                                                 //
                                                 // This flag is the workaround and the bisection tool, not the fix. The
                                                 // fix is a stack guard in the prologue, because the optimization is
                                                 // worth far too much to simply drop: without it fib(30) goes from 3ms to
                                                 // 498ms, a 166x regression.
        let self_call_disabled = std::env::var_os("EGCL_NO_DIRECT_SELF_CALL").is_some();
        // Wider calls need an activation-backed argument slice, including on
        // the stack-limit path; they cannot use this register-only entry.
        const C2I_ARGS_SELF: [u8; 3] = [2, 1, 8];
        // A self-call with the wrong argument count must reach c2i, which
        // raises PROGRAM-ERROR; the register entry would bind whatever is in
        // the argument registers (bliss-w6aki).
        if !self_call_disabled && sym == ss && nargs <= C2I_ARGS_SELF.len() && nargs == self_arity
        {
            // Stack guard. The direct call below takes a REAL C frame and does
            // not reach c2i_call_args, so it never sees native_depth_cap() —
            // and the T2 prologue has no guard of its own. Unbounded, a deeply
            // self-recursive function runs off the stack and returns a wrong
            // answer (bliss-b4fd). Compare rsp against the published limit and
            // fall back to the c2i sequence below once the stack runs low; that
            // path enforces the depth cap and drops to a flat T0 `run()`, which
            // raises a catchable STORAGE-CONDITION.
            //
            // A zero limit (never published) compares false, so the guard is
            // inert and behaviour is exactly as before. The compare happens
            // BEFORE any argument move, because the moves clobber registers and
            // the c2i path needs the untouched inputs; SCRATCH is rdx, which is
            // not one of ARG_REGS.
            const RSP: u8 = 4;
            let limit_addr = egcl_rt::stack::native_stack_limit_addr();
            let slow = a.label();
            let join = a.label();
            mov_imm64(a, SCRATCH, limit_addr as i64);
            load_mem64_disp(a, SCRATCH, SCRATCH, 0);
            cmp_rr(a, RSP, SCRATCH);
            // Signed compare is right: stack addresses are canonical and
            // positive, and a zero limit takes the direct path.
            a.jcc(egcl_rt::asm::Cc::Le, slow);

            let mut moves = Vec::with_capacity(nargs);
            for (i, &arg) in data.args.iter().enumerate() {
                let src = if let Some(&bits) = const_tagged.get(&arg) {
                    ArgMoveSrc::Imm(bits)
                } else {
                    ArgMoveSrc::Reg(*reg.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?)
                };
                moves.push((ARG_REGS[i], src));
            }
            emit_call_arg_moves(a, moves)?;
            a.call(entry);
            a.jmp(join);

            // Slow path: the ordinary three-register c2i dispatch, emitted
            // inline so the guard has somewhere to go.
            a.bind(slow);
            let mut moves = Vec::with_capacity(nargs);
            for (i, &arg) in data.args.iter().enumerate() {
                let src = if let Some(&bits) = const_tagged.get(&arg) {
                    ArgMoveSrc::Imm(bits)
                } else {
                    ArgMoveSrc::Reg(*reg.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?)
                };
                moves.push((C2I_ARGS_SELF[i], src));
            }
            emit_call_arg_moves(a, moves)?;
            mov_imm64(a, 7, sym as i64); // rdi = sym
            mov_imm64(a, 6, nargs as i64); // rsi = nargs
            mov_imm64(a, 9, 0); // r9 = no profiling token
            mov_imm64(a, 0, c2i_call_addr as i64);
            // The common join checks both the helper and direct entries.
            emit_runtime_helper_call(a, c2i_recovery_toggle_addr, None);

            a.bind(join);
            emit_transfer_check(a, transfer_check);
            if let Some(&r0) = data.results.first() {
                mov_rr(a, *reg.get(&r0).ok_or(EmitError::UnsupportedOp(0xF2))?, 0);
            }
            return Ok(());
        }
    }
    // T2 call(sym, n, a0, a1, a2, profile_site): rdx=a0, rcx=a1, r8=a2.
    const C2I_ARGS: [u8; 3] = [2, 1, 8];
    if nargs > C2I_ARGS.len() {
        let frame_base = frame_base.ok_or(EmitError::UnsupportedOp(0xF9))?;
        let activation_slots = activation_slots.ok_or(EmitError::UnsupportedOp(0xF9))?;
        load_home(a, SCRATCH, frame_base, 0);
        for (index, &arg) in data.args.iter().enumerate() {
            if let Some(&bits) = const_tagged.get(&arg) {
                mov_imm64(a, RAX, bits as i64);
            } else {
                let home = *homes.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?;
                load_home(a, RAX, home, 0);
            }
            let slot = i32::from(activation_slots) + i32::from(call_arg_base) + index as i32;
            store_mem64_disp(a, SCRATCH, slot * 8, RAX);
        }
        mov_imm64(a, 7, sym as i64); // rdi = sym
        mov_imm64(a, 6, nargs as i64); // rsi = nargs
        mov_rr(a, 2, SCRATCH); // rdx = frame base
        alu_r_imm(
            a,
            0,
            2,
            (i32::from(activation_slots) + i32::from(call_arg_base)) * 8,
        );
        mov_imm64(a, 1, 0); // rcx = no profiling token
        if let Some(target) = named {
            mov_imm64(a, 7, target.cell_address as i64);
            mov_imm64(a, 0, target.slice_entry as i64);
        } else {
            mov_imm64(a, 0, c2i_call_slice_addr as i64);
        }
        emit_runtime_target_call(a, named_recovery, transfer_check, named.is_some());
        if let Some(&result) = data.results.first() {
            let home = *homes.get(&result).ok_or(EmitError::UnsupportedOp(0xF2))?;
            store_home(a, home, RAX, 0);
        }
        return Ok(());
    }
    let mut moves = Vec::with_capacity(nargs);
    for (i, &arg) in data.args.iter().enumerate() {
        let src = if let Some(&bits) = const_tagged.get(&arg) {
            ArgMoveSrc::Imm(bits)
        } else {
            ArgMoveSrc::Reg(*reg.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?)
        };
        moves.push((C2I_ARGS[i], src));
    }
    emit_call_arg_moves(a, moves)?;
    // Direct builtin call when the callee resolves to one: identical register
    // convention, so only rdi (which also carries the table slot), r9 (the
    // baked invalidation generation instead of a profile token — builtins do
    // not tier, so there is nothing to profile) and the target differ.
    if let Some(target) = named {
        mov_imm64(a, 7, target.cell_address as i64);
        mov_imm64(a, 6, nargs as i64);
        mov_imm64(a, 9, 0);
        mov_imm64(a, 0, target.register_entry as i64);
    } else {
        match direct_builtin_for(sym, nargs) {
            Some((addr, slot, generation)) => {
                mov_imm64(a, 7, (((slot as u64) << 32) | sym as u64) as i64); // rdi
                mov_imm64(a, 6, nargs as i64); // rsi = nargs
                mov_imm64(a, 9, generation as i64); // r9 = baked generation
                mov_imm64(a, 0, addr as i64); // rax = direct builtin adapter
            }
            None => {
                mov_imm64(a, 7, sym as i64); // mov rdi, sym
                mov_imm64(a, 6, nargs as i64); // mov rsi, nargs
                mov_imm64(a, 9, 0); // r9 = no profiling token
                mov_imm64(a, 0, c2i_call_addr as i64); // mov rax, c2i_call
            }
        }
    }
    emit_runtime_target_call(a, named_recovery, transfer_check, named.is_some());
    if let Some(&r0) = data.results.first() {
        let dst = *reg.get(&r0).ok_or(EmitError::UnsupportedOp(0xF2))?;
        mov_rr(a, dst, 0); // mov result, rax
    }
    Ok(())
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn emit_invoke_call(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    constants: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    frame_base: FramedHome,
    activation_slots: u16,
    argument_base: u16,
    veneer: u64,
    recursion: Option<(RecursiveTransfer, egcl_rt::asm::Label)>,
) -> Result<(Vec<u32>, u32), EmitError> {
    let (request_word0, request_word1, cleanup) = match data.aux {
        crate::t2::ir::AuxData::CallTarget(symbol) => {
            (u64::from(symbol), data.args.len() as u64, false)
        }
        crate::t2::ir::AuxData::TransferThrow => (TRANSFER_THROW_REQUEST, 2, false),
        crate::t2::ir::AuxData::CatchScope { push_bcp, enter } => (
            (if enter {
                TRANSFER_CATCH_ENTER_REQUEST
            } else {
                TRANSFER_CATCH_LEAVE_REQUEST
            }) | u64::from(push_bcp),
            u64::from(enter),
            false,
        ),
        crate::t2::ir::AuxData::HandlerScope { push_bcp, enter } => (
            (if enter {
                TRANSFER_HANDLER_ENTER_REQUEST
            } else {
                TRANSFER_HANDLER_LEAVE_REQUEST
            }) | u64::from(push_bcp),
            0,
            false,
        ),
        crate::t2::ir::AuxData::HandlerBindScope { push_bcp, enter } => (
            (if enter {
                TRANSFER_HANDLER_BIND_ENTER_REQUEST
            } else {
                TRANSFER_HANDLER_BIND_LEAVE_REQUEST
            }) | u64::from(push_bcp),
            data.args.len() as u64,
            false,
        ),
        crate::t2::ir::AuxData::RestartCaseScope { push_bcp, enter } => (
            (if enter {
                TRANSFER_RESTART_CASE_ENTER_REQUEST
            } else {
                TRANSFER_RESTART_CASE_LEAVE_REQUEST
            }) | u64::from(push_bcp),
            0,
            false,
        ),
        crate::t2::ir::AuxData::HostEval(index) => {
            (TRANSFER_HOST_EVAL_REQUEST | u64::from(index), 0, false)
        }
        crate::t2::ir::AuxData::FunctionLookup(symbol) => (
            TRANSFER_FUNCTION_LOOKUP_REQUEST | u64::from(symbol),
            0,
            false,
        ),
        crate::t2::ir::AuxData::CleanupContinuation {
            cleanup_bcp,
            resume_bcp,
        } => (u64::from(cleanup_bcp), u64::from(resume_bcp), true),
        _ => return Err(EmitError::UnsupportedOp(0xF8)),
    };
    load_home(a, SCRATCH, frame_base, 0);
    let args_slot = i32::from(activation_slots) + i32::from(argument_base);
    for (index, value) in data.args.iter().enumerate() {
        if let Some(&bits) = constants.get(value) {
            mov_imm64(a, RAX, bits as i64);
        } else {
            load_home(
                a,
                RAX,
                *homes.get(value).ok_or(EmitError::UnsupportedOp(0xF2))?,
                0,
            );
        }
        store_mem64_disp(a, SCRATCH, (args_slot + index as i32) * 8, RAX);
    }
    // Fixed, aligned request; rooted arguments remain in the activation. The
    // cold capture table records this exact temporary RSP adjustment.
    const _: () = {
        assert!(std::mem::size_of::<TransferCallRequest>() == 32);
        assert!(std::mem::size_of::<TransferCleanupRequest>() == 32);
        assert!(std::mem::offset_of!(TransferCleanupRequest, activation) == 24);
    };
    let temporary_bytes = if recursion.is_some() { 64 } else { 32 };
    alu_r_imm(a, 5, 4, temporary_bytes);
    store_to_rsp(a, SCRATCH, 24); // activation
    mov_imm64(a, RAX, request_word0 as i64);
    store_to_rsp(a, RAX, 0);
    mov_imm64(a, RAX, request_word1 as i64);
    store_to_rsp(a, RAX, 8);
    if cleanup {
        mov_imm64(a, SCRATCH, 0); // reserved, not an argument pointer
    } else {
        alu_r_imm(a, 0, SCRATCH, args_slot * 8);
    }
    store_to_rsp(a, SCRATCH, 16);
    mov_rr(a, 7, 4); // rdi = request
    mov_imm64(a, RAX, recursion.map_or(veneer, |(r, _)| r.prepare) as i64);
    a.extend_from_slice(&[0xff, 0xd0]);
    let mut offsets = vec![u32::try_from(a.here()).map_err(|_| EmitError::BadBranch)?];
    if let Some((recursion, entry)) = recursion {
        let returned = a.label();
        // A bounded compatibility fallback has already produced a Lisp value.
        // Otherwise prepare returns the new, precisely rooted activation slots.
        load_from_rsp(a, SCRATCH, 40); // RecursiveActivation.frame
        a.extend_from_slice(&[0x48, 0x85, 0xd2]); // test rdx,rdx
        a.jcc(egcl_rt::asm::Cc::E, returned);
        mov_rr(a, 7, RAX);
        load_from_rsp(a, 6, 48); // RecursiveActivation.stack
        a.call(entry);
        offsets.push(u32::try_from(a.here()).map_err(|_| EmitError::BadBranch)?);
        // Retirement cannot allocate, poll, signal or invoke Lisp. The value
        // may remain unrooted here while its callee activation is popped.
        store_to_rsp(a, RAX, 16);
        a.extend_from_slice(&[0x48, 0x8d, 0x7c, 0x24, 32]); // lea rdi,[rsp+32]
        mov_imm64(a, RAX, recursion.finish as i64);
        a.extend_from_slice(&[0xff, 0xd0]);
        load_from_rsp(a, RAX, 16);
        a.bind(returned);
    }
    alu_r_imm(a, 0, 4, temporary_bytes); // normal return only
    if let Some(value) = data.results.first() {
        store_home(
            a,
            *homes.get(value).ok_or(EmitError::UnsupportedOp(0xF2))?,
            RAX,
            0,
        );
    }
    Ok((offsets, temporary_bytes as u32))
}

/// `(symbol-value sym)` — a global read. Lowers to `c2i_load_global(sym) -> rax`,
/// then moves the result into its value register (bliss-mzp). The call clobbers
/// caller-saved registers, but a function containing this op is `has_calls`, so
/// its live values sit in callee-saved registers and survive.
fn emit_symbol_value(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    c2i_load_global_addr: u64,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
) -> Result<(), EmitError> {
    use crate::t2::ir::AuxData;
    let sym = match data.aux {
        AuxData::SymbolRef(s) => s,
        _ => return Err(EmitError::UnsupportedOp(0xFA)),
    };
    mov_imm64(a, 7, sym as i64); // mov rdi, sym
    mov_imm64(a, 0, c2i_load_global_addr as i64); // mov rax, c2i_load_global
    emit_runtime_helper_call(a, c2i_recovery_toggle_addr, transfer_check);
    let r0 = *data.results.first().ok_or(EmitError::UnsupportedOp(0xFA))?;
    let dst = *reg.get(&r0).ok_or(EmitError::UnsupportedOp(0xF2))?;
    mov_rr(a, dst, 0); // mov result, rax
    Ok(())
}

/// `(setf (symbol-value sym) v)` — a global write (side effect). Lowers to
/// `c2i_store_global(sym, v)`, no result (bliss-mzp).
fn emit_set_symbol_value(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    c2i_store_global_addr: u64,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
) -> Result<(), EmitError> {
    use crate::t2::ir::AuxData;
    let sym = match data.aux {
        AuxData::SymbolRef(s) => s,
        _ => return Err(EmitError::UnsupportedOp(0xFB)),
    };
    let v = *data.args.first().ok_or(EmitError::UnsupportedOp(0xFB))?;
    // rsi = value (from its callee-saved register or an immediate). Set before rdi
    // (both are scratch here); rdi = sym.
    if let Some(&bits) = const_tagged.get(&v) {
        mov_imm64(a, 6, bits as i64); // mov rsi, tagged(v)
    } else {
        mov_rr(a, 6, *reg.get(&v).ok_or(EmitError::UnsupportedOp(0xF2))?); // mov rsi, v
    }
    mov_imm64(a, 7, sym as i64); // mov rdi, sym
    mov_imm64(a, 0, c2i_store_global_addr as i64); // mov rax, c2i_store_global
    emit_runtime_helper_call(a, c2i_recovery_toggle_addr, transfer_check);
    Ok(())
}

#[derive(Clone, Copy)]
struct NativeTransferCheck {
    pending_addr: u64,
    exit: egcl_rt::asm::Label,
}

fn emit_transfer_check(a: &mut Asm, check: Option<NativeTransferCheck>) {
    let Some(check) = check else { return };
    // This callback must not allocate, safepoint, or invoke Lisp: the primary
    // result is temporarily saved on the native stack, not in a GC root. Other
    // live values already have call-preserved homes. Leave multiple values alone.
    let shadow_bytes = if cfg!(windows) { 32 } else { 0 };
    let alloc = shadow_bytes + 16;
    a.extend_from_slice(&[0x48, 0x83, 0xEC, alloc]);
    store_to_rsp(a, RAX, shadow_bytes as i32);
    mov_imm64(a, RAX, check.pending_addr as i64);
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    a.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    load_from_rsp(a, RAX, shadow_bytes as i32);
    a.extend_from_slice(&[0x48, 0x8D, 0x64, 0x24, alloc]); // preserve test flags
    a.jcc(egcl_rt::asm::Cc::Ne, check.exit);
}

/// Convert a Lisp register/slice call into a Rust runtime callback. This
/// boundary belongs to interpreted or legacy targets, not ordinary call sites.
/// The callback receives the current cell, argument count, and register/slice
/// payload. It must catch Rust panics and report Lisp transfers out of band.
#[cfg(all(target_arch = "x86_64", unix))]
pub fn emit_native_call_bridge(target: u64, recovery_toggle: u64) -> Result<Vec<u8>, EmitError> {
    let mut a = Asm::new();
    // At entry RSP is 8 mod 16. The helper emitter expects call-site alignment.
    a.extend_from_slice(&[0x48, 0x83, 0xec, 8]);
    mov_imm64(&mut a, RAX, target as i64);
    emit_runtime_helper_call(&mut a, recovery_toggle, None);
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 8, 0xc3]);
    a.finish().ok_or(EmitError::BadBranch)
}

fn emit_runtime_helper_call(
    a: &mut Asm,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
) {
    emit_runtime_target_call(a, c2i_recovery_toggle_addr, transfer_check, false);
}

#[cfg(windows)]
fn emit_runtime_target_call(
    a: &mut Asm,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
    indirect: bool,
) {
    // Templates arrange up to six word arguments in rdi/rsi/rdx/rcx/r8/r9.
    // At the C boundary translate to Win64's four registers and two stack args.
    // Reserve 32 bytes of callee-owned shadow space, 16 bytes of stack args,
    // and 56 bytes for saved operands/target, rounded to 16-byte alignment.
    // The enclosing Windows frame must use a fixed nonvolatile frame pointer
    // in its unwind info so these temporary RSP adjustments are unwindable.
    a.extend_from_slice(&[0x48, 0x83, 0xEC, 112]);
    for (i, r) in [7, 6, 2, 1, 8, 9, RAX].into_iter().enumerate() {
        store_to_rsp(a, r, 48 + i as i32 * 8);
    }
    if c2i_recovery_toggle_addr != 0 {
        mov_imm32(a, 1, 0);
        mov_imm64(a, RAX, c2i_recovery_toggle_addr as i64);
        a.extend_from_slice(&[0xFF, 0xD0]);
    }
    for (i, r) in [1, 2, 8, 9].into_iter().enumerate() {
        load_from_rsp(a, r, 48 + i as i32 * 8);
    }
    for i in 0..2 {
        load_from_rsp(a, RAX, 80 + i * 8);
        store_to_rsp(a, RAX, 32 + i * 8);
    }
    load_from_rsp(a, RAX, 96);
    a.extend_from_slice(&[0xFF, if indirect { 0x10 } else { 0xD0 }]);
    if c2i_recovery_toggle_addr != 0 {
        // Recovery toggles are leaf, nonallocating callbacks: raw operands and
        // the primary result may live here across them, never across a GC.
        store_to_rsp(a, RAX, 48);
        mov_imm32(a, 1, 1);
        mov_imm64(a, RAX, c2i_recovery_toggle_addr as i64);
        a.extend_from_slice(&[0xFF, 0xD0]);
        load_from_rsp(a, RAX, 48);
    }
    a.extend_from_slice(&[0x48, 0x83, 0xC4, 112]);
    emit_transfer_check(a, transfer_check);
}

#[cfg(not(windows))]
fn emit_runtime_target_call(
    a: &mut Asm,
    c2i_recovery_toggle_addr: u64,
    transfer_check: Option<NativeTransferCheck>,
    indirect: bool,
) {
    if c2i_recovery_toggle_addr == 0 {
        a.extend_from_slice(&[0xFF, if indirect { 0x10 } else { 0xD0 }]);
        emit_transfer_check(a, transfer_check);
        return;
    }

    // The helper target is in rax; runtime helper arguments may already occupy
    // rdi/rsi/rdx/rcx/r8/r9. Preserve that caller ABI state while disabling
    // native-frame SIGSEGV recovery for the Rust helper frame.
    a.extend_from_slice(&[0x48, 0x83, 0xEC, 0x40]); // sub rsp, 64
    a.extend_from_slice(&[0x48, 0x89, 0x3C, 0x24]); // mov [rsp], rdi
    a.extend_from_slice(&[0x48, 0x89, 0x74, 0x24, 0x08]); // mov [rsp+8], rsi
    a.extend_from_slice(&[0x48, 0x89, 0x54, 0x24, 0x10]); // mov [rsp+16], rdx
    a.extend_from_slice(&[0x48, 0x89, 0x4C, 0x24, 0x18]); // mov [rsp+24], rcx
    a.extend_from_slice(&[0x4C, 0x89, 0x44, 0x24, 0x20]); // mov [rsp+32], r8
    a.extend_from_slice(&[0x4C, 0x89, 0x4C, 0x24, 0x28]); // mov [rsp+40], r9
    a.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, 0x30]); // mov [rsp+48], rax
    mov_imm32(a, 7, 0); // mov edi, 0
    mov_imm64(a, 0, c2i_recovery_toggle_addr as i64); // mov rax, toggle
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    a.extend_from_slice(&[0x48, 0x8B, 0x44, 0x24, 0x30]); // mov rax, [rsp+48]
    a.extend_from_slice(&[0x4C, 0x8B, 0x4C, 0x24, 0x28]); // mov r9, [rsp+40]
    a.extend_from_slice(&[0x4C, 0x8B, 0x44, 0x24, 0x20]); // mov r8, [rsp+32]
    a.extend_from_slice(&[0x48, 0x8B, 0x4C, 0x24, 0x18]); // mov rcx, [rsp+24]
    a.extend_from_slice(&[0x48, 0x8B, 0x54, 0x24, 0x10]); // mov rdx, [rsp+16]
    a.extend_from_slice(&[0x48, 0x8B, 0x74, 0x24, 0x08]); // mov rsi, [rsp+8]
    a.extend_from_slice(&[0x48, 0x8B, 0x3C, 0x24]); // mov rdi, [rsp]
    a.extend_from_slice(&[0x48, 0x83, 0xC4, 0x40]); // add rsp, 64

    a.extend_from_slice(&[0xFF, if indirect { 0x10 } else { 0xD0 }]);

    a.extend_from_slice(&[0x48, 0x83, 0xEC, 0x10]); // sub rsp, 16
    a.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]); // mov [rsp], rax
    mov_imm32(a, 7, 1); // mov edi, 1
    mov_imm64(a, 0, c2i_recovery_toggle_addr as i64); // mov rax, toggle
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    a.extend_from_slice(&[0x48, 0x8B, 0x04, 0x24]); // mov rax, [rsp]
    a.extend_from_slice(&[0x48, 0x83, 0xC4, 0x10]); // add rsp, 16
    emit_transfer_check(a, transfer_check);
}

fn edge_home_moves(
    f: &Function,
    tc: &crate::t2::ir::BlockCall,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<Vec<(FramedHome, HomeMoveSrc)>, EmitError> {
    let params = &f.block(tc.block).params;
    if params.len() != tc.args.len() {
        return Err(EmitError::UnsupportedOp(0xE7));
    }
    let mut moves = Vec::new();
    for (&param, &arg) in params.iter().zip(&tc.args) {
        let Some(&dst) = homes.get(&param) else {
            continue;
        };
        let src = if let Some(&bits) = const_tagged.get(&arg) {
            HomeMoveSrc::Const(bits)
        } else {
            HomeMoveSrc::Home(*homes.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?)
        };
        if !matches!(src, HomeMoveSrc::Home(home) if home == dst) {
            moves.push((dst, src));
        }
    }
    Ok(moves)
}

type PreparedFramedInst = (
    std::collections::HashMap<crate::t2::ir::Value, u8>,
    Vec<(FramedHome, u8)>,
);

/// Load spilled operands into the framed emitter's reserved r9-r11 temporary
/// bank and choose a register for a spilled result. The returned map is valid
/// for exactly one SSA instruction; `stores` must be committed afterward.
fn prepare_framed_inst(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<PreparedFramedInst, EmitError> {
    use std::collections::{HashMap, HashSet};
    const TEMPS: [u8; 3] = [9, 10, 11]; // r9, r10, r11
    let mut regs: HashMap<crate::t2::ir::Value, u8> = homes
        .iter()
        .filter_map(|(&value, &home)| match home {
            FramedHome::Reg(reg) => Some((value, reg)),
            FramedHome::Stack(_) => None,
        })
        .collect();
    let mut used = HashSet::new();
    for &arg in &data.args {
        if const_tagged.contains_key(&arg) || regs.contains_key(&arg) {
            continue;
        }
        let home = *homes.get(&arg).ok_or(EmitError::UnsupportedOp(0xF2))?;
        let reg = *TEMPS
            .iter()
            .find(|r| !used.contains(*r))
            .ok_or(EmitError::UnsupportedOp(0xF1))?;
        load_home(a, reg, home, 0);
        regs.insert(arg, reg);
        used.insert(reg);
    }

    let mut stores = Vec::new();
    for &result in &data.results {
        let Some(&home) = homes.get(&result) else {
            continue;
        };
        match home {
            FramedHome::Reg(reg) => {
                regs.insert(result, reg);
            }
            FramedHome::Stack(_) => {
                let reg = TEMPS
                    .iter()
                    .copied()
                    .find(|r| !used.contains(r))
                    // Calls may consume all three temps for arguments; after the
                    // call the first argument register is dead and can receive
                    // the result from rax.
                    .or_else(|| TEMPS.first().copied())
                    .ok_or(EmitError::UnsupportedOp(0xF1))?;
                regs.insert(result, reg);
                used.insert(reg);
                stores.push((home, reg));
            }
        }
    }
    Ok((regs, stores))
}

/// Recompute one FrameState source into rax for the cold deopt path. Homes are
/// addressed relative to the current rsp; recursive binary recipes use a push
/// and compensate `rsp_adjust` so spilled inputs remain addressable.
fn emit_deopt_source(
    a: &mut Asm,
    source: &crate::t2::frame_state::ValueSource,
    fs: &crate::t2::frame_state::FrameState,
    homes: &std::collections::HashMap<crate::t2::ir::Value, FramedHome>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    rsp_adjust: i32,
) -> Result<(), EmitError> {
    use crate::t2::frame_state::{RematOp, ValueSource};
    match source {
        ValueSource::Value { value, repr } => {
            if *repr != crate::t2::ir::ValueRepresentation::Tagged {
                return Err(EmitError::UnsupportedOp(0xF5));
            }
            if let Some(&bits) = const_tagged.get(value) {
                mov_imm64(a, RAX, bits as i64);
            } else {
                load_home(
                    a,
                    RAX,
                    *homes.get(value).ok_or(EmitError::UnsupportedOp(0xF6))?,
                    rsp_adjust,
                );
            }
        }
        ValueSource::Const(value) => {
            if is_moving_gc_reference(*value) {
                return Err(EmitError::UnsupportedOp(0xF8));
            }
            mov_imm64(a, RAX, value.0 as i64);
        }
        ValueSource::Unbound => mov_imm64(a, RAX, egcl_rt::value::UNBOUND.0 as i64),
        ValueSource::Remat(id) => {
            let recipe = fs
                .remat
                .get(id.0 as usize)
                .ok_or(EmitError::UnsupportedOp(0xD7))?;
            match recipe.op {
                RematOp::Const
                | RematOp::BoxFixnum
                | RematOp::UnboxFixnum
                | RematOp::BoxFloat
                | RematOp::UnboxFloat => {
                    let input = recipe
                        .inputs
                        .first()
                        .ok_or(EmitError::UnsupportedOp(0xD7))?;
                    emit_deopt_source(a, input, fs, homes, const_tagged, rsp_adjust)?;
                }
                RematOp::FixnumAdd | RematOp::FixnumSub => {
                    if recipe.inputs.len() != 2 {
                        return Err(EmitError::UnsupportedOp(0xD7));
                    }
                    emit_deopt_source(a, &recipe.inputs[0], fs, homes, const_tagged, rsp_adjust)?;
                    push_reg(a, RAX);
                    emit_deopt_source(
                        a,
                        &recipe.inputs[1],
                        fs,
                        homes,
                        const_tagged,
                        rsp_adjust + 8,
                    )?;
                    pop_reg(a, 11);
                    if recipe.op == RematOp::FixnumAdd {
                        alu_rr(a, 0x01, RAX, 11);
                    } else {
                        alu_rr(a, 0x29, 11, RAX);
                        mov_rr(a, RAX, 11);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Emit a fused fixnum comparison (guard operands, `cmp`), returning the condition
/// under which the comparison is *true* — for the branch that follows.
fn emit_fused_compare(
    a: &mut Asm,
    cmp_data: &crate::t2::ir::InstData,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    proven: &std::collections::HashSet<crate::t2::ir::Value>,
    deopt: egcl_rt::asm::Label,
) -> Result<Cc, EmitError> {
    let base =
        fixnum_cmp_cc(cmp_data.opcode).ok_or(EmitError::UnsupportedOp(op_tag(cmp_data.opcode)))?;
    let (l, r) = (cmp_data.args[0], cmp_data.args[1]);
    let tagged32 = |c: i64| {
        i32::try_from(egcl_rt::value::EgclVal::from_fixnum(c).0 as i64)
            .map_err(|_| EmitError::UnsupportedOp(0xF4))
    };
    let guard = |a: &mut Asm, v: crate::t2::ir::Value, rr: u8| {
        if !proven.contains(&v) {
            guard_fixnum(a, rr, deopt);
        }
    };
    match (consts.get(&l).copied(), consts.get(&r).copied()) {
        (None, Some(c)) => {
            let lr = *reg.get(&l).ok_or(EmitError::UnsupportedOp(0xF2))?;
            guard(a, l, lr);
            alu_r_imm(a, 7, lr, tagged32(c)?); // cmp lr, tagged
            Ok(base)
        }
        (Some(c), None) => {
            let rr = *reg.get(&r).ok_or(EmitError::UnsupportedOp(0xF2))?;
            guard(a, r, rr);
            alu_r_imm(a, 7, rr, tagged32(c)?);
            Ok(cc_swapped(base)) // operands swapped
        }
        (None, None) => {
            let lr = *reg.get(&l).ok_or(EmitError::UnsupportedOp(0xF2))?;
            let rr = *reg.get(&r).ok_or(EmitError::UnsupportedOp(0xF2))?;
            guard(a, l, lr);
            guard(a, r, rr);
            cmp_rr(a, lr, rr);
            Ok(base)
        }
        (Some(_), Some(_)) => Err(EmitError::UnsupportedOp(0xF5)), // const-const: folded upstream
    }
}

/// Is `op` a fixnum operation whose guard proves its operands are fixnums (so a
/// dominating occurrence lets a later use skip its guard)?
fn is_fixnum_guarding_op(op: crate::t2::ir::Opcode) -> bool {
    use crate::t2::ir::Opcode::*;
    matches!(
        op,
        FixnumAdd
            | FixnumSub
            | FixnumMul
            | FixnumNeg
            | LogAnd
            | LogOr
            | LogXor
            | LogNot
            | FixnumShl
            | FixnumCmpEq
            | FixnumCmpLt
            | FixnumCmpLe
            | FixnumCmpGt
            | FixnumCmpGe
    )
}

/// A fixnum-*producing* op: its result is provably a fixnum (the op yields one or
/// deopts), so any later use of the result needs no tag guard. Comparisons are
/// excluded — they produce booleans, not fixnums.
/// Emit `GenericEq` (Lisp `EQ`, and `NULL` via `EQ x NIL`) as an inline tagged
/// bit-comparison producing `T`/`NIL` — no runtime call. This is the first T2
/// intrinsic: `EQ` is bit-equality of the two tagged representations, so it is a
/// `cmp` plus a select of the two immediate results.
/// Resolve `GenericEq`'s two operands to registers (a live value reg, or a
/// tagged constant loaded into SCRATCH) and emit `cmp r0, r1`, setting ZF iff the
/// two tagged reps are equal (Lisp `EQ`). Two constants set ZF directly from
/// their known equality, without competing for the single scratch register.
/// Shared by the value-producing `emit_generic_eq` and the fused branch path.
fn emit_eq_cmp(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &std::collections::HashMap<crate::t2::ir::Value, u8>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    if data.args.len() < 2 {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::GenericEq)));
    }
    let (a0, a1) = (data.args[0], data.args[1]);
    if let (Some(left), Some(right)) = (const_tagged.get(&a0), const_tagged.get(&a1)) {
        mov_imm64(a, SCRATCH, i64::from(left != right));
        alu_r_imm(a, 7, SCRATCH, 0); // cmp scratch, 0: ZF iff the constants match
        return Ok(());
    }
    let resolve = |a: &mut Asm, v: crate::t2::ir::Value| -> Result<u8, EmitError> {
        if let Some(&r) = reg.get(&v) {
            Ok(r)
        } else if let Some(&bits) = const_tagged.get(&v) {
            mov_imm64(a, SCRATCH, bits as i64);
            Ok(SCRATCH)
        } else {
            Err(EmitError::UnsupportedOp(0xF2))
        }
    };
    let r0 = resolve(a, a0)?;
    let r1 = resolve(a, a1)?;
    cmp_rr(a, r0, r1);
    Ok(())
}

/// Emit `GenericEq` (Lisp `EQ`, and `NULL` via `EQ x NIL`) as an inline tagged
/// bit-comparison producing `T`/`NIL` — no runtime call. This is the first T2
/// intrinsic: `EQ` is bit-equality of the two tagged representations, so it is a
/// `cmp` plus a select of the two immediate results. (When the result only feeds
/// a branch, the compare is fused straight into the Brif instead — see below.)
fn emit_generic_eq(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    if data.results.is_empty() {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::GenericEq)));
    }
    // Sets flags (ZF iff equal); mov-imm below does not disturb them.
    emit_eq_cmp(a, data, reg, const_tagged)?;
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_imm64(a, dst, egcl_rt::value::T.0 as i64);
    let done = a.label();
    a.jcc(Cc::E, done);
    mov_imm64(a, dst, egcl_rt::value::NIL.0 as i64);
    a.bind(done);
    Ok(())
}

/// Emit a fixnum comparison whose result is used as a VALUE rather than fused
/// into a branch — guard the operands, `cmp`, then select `T`/`NIL`. The
/// value-producing mirror of the fused `emit_fused_compare` path, exactly as
/// `emit_generic_eq` mirrors the fused `EQ` path.
///
/// Without this the emitter could only lower a fixnum comparison that a `Brif`
/// consumed directly, so any comparison reaching an ordinary use — `(not (< y
/// x))`, or simply returning `(< y x)` — failed the whole function out of T2
/// and left it at T1. That knocked TAK, a canonical Gabriel benchmark, off T2
/// entirely (bliss-n6d1).
fn emit_fixnum_cmp_value(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    consts: &std::collections::HashMap<crate::t2::ir::Value, i64>,
    proven: &std::collections::HashSet<crate::t2::ir::Value>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    if data.results.is_empty() {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    // Guards the operands and emits `cmp`, returning the condition under which
    // the comparison is TRUE.
    let cc = emit_fused_compare(a, data, reg, consts, proven, deopt)?;
    // `mov`-immediate does not disturb the flags set above, so allocating `dst`
    // after the compare is safe even when it aliases an operand register — the
    // same ordering `emit_generic_eq` relies on.
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_imm64(a, dst, egcl_rt::value::T.0 as i64);
    let done = a.label();
    a.jcc(cc, done);
    mov_imm64(a, dst, egcl_rt::value::NIL.0 as i64);
    a.bind(done);
    Ok(())
}

/// Emit `TypeCheck` type-predicate intrinsics as inline tests producing
/// `T`/`NIL` — no runtime call. Single-tag predicates
/// (fixnum tag 000, cons tag 001) are one `and`+`cmp`; `symbolp` also accepts
/// the two special-cased symbols NIL (0x7) and T (0xF), whose tag is not the
/// SYMBOL tag (101). `dst` is written only after every read of the operand, so a
/// `dst` that aliases the operand register is safe.
fn emit_type_check(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
) -> Result<(), EmitError> {
    use crate::t2::ir::TypeBits;
    use crate::t2::ir::{AuxData, Opcode};
    use egcl_rt::bytecode::typep_class;
    if data.results.is_empty() || data.args.is_empty() {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::TypeCheck)));
    }
    let x = data.args[0];
    // A constant operand should have been folded to T/NIL upstream; decline.
    if const_tagged.contains_key(&x) {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::TypeCheck)));
    }
    let xr = *reg.get(&x).ok_or(EmitError::UnsupportedOp(0xF2))?;
    let (bits, class) = match &data.aux {
        AuxData::TypeTag(t) => (t.bits, None),
        AuxData::TypepClass(class) => (TypeBits::BOTTOM, Some(*class)),
        _ => return Err(EmitError::UnsupportedOp(op_tag(Opcode::TypeCheck))),
    };
    // ext codes for `alu_r_imm`: 4 = AND, 7 = CMP. Tag values are the low 3 bits.
    const AND: u8 = 4;
    const CMP: u8 = 7;
    let dst = framed_alloc(reg, pool, data.results[0])?;
    let found = a.label();
    let not_found = a.label();
    let end = a.label();
    let single_tag = |a: &mut Asm, tag: i32| {
        mov_rr(a, SCRATCH, xr);
        alu_r_imm(a, AND, SCRATCH, 7); // scratch = x & 7
        alu_r_imm(a, CMP, SCRATCH, tag);
    };
    // NOTE (bliss-74rl): CONS and LIST are deliberately absent from this chain
    // and fall through to the `else` that declines the op.  A cons test here
    // would be the tag test `x & 7 == 1`, but egcl represents an interpreter
    // closure as the cons `(EGCL::CLOSURE . id)`: it carries the cons tag while
    // its Common Lisp type is FUNCTION.  T0's `typep_class_matches` gets this
    // right (`v.is_cons() && !is_function_value(v)`), so emitting the bare tag
    // test made a T2-promoted CONSP / (TYPEP x 'CONS) disagree with every other
    // tier -- and since RPLACA's guard is `(unless (consp cons) (error ...))`,
    // a hot RPLACA sailed past its type check and overwrote a live function
    // object.  Declining costs a call and keeps the tiers bit-identical, which
    // is the bliss-x5y.9 rule.  Retiring the cons representation of closures
    // (bliss-fju9) is what would make an inline test sound again.
    if class == Some(typep_class::BOOLEAN) {
        alu_r_imm(a, CMP, xr, egcl_rt::value::NIL.0 as i32);
        a.jcc(Cc::E, found);
        alu_r_imm(a, CMP, xr, egcl_rt::value::T.0 as i32);
        a.jcc(Cc::E, found);
    } else if class == Some(typep_class::NULL) {
        alu_r_imm(a, CMP, xr, egcl_rt::value::NIL.0 as i32);
        a.jcc(Cc::E, found);
    } else if bits == TypeBits::FIXNUM {
        single_tag(a, 0);
        a.jcc(Cc::E, found);
    } else if bits == TypeBits::SYMBOL || class == Some(typep_class::SYMBOL) {
        single_tag(a, 5); // TAG_SYMBOL
        a.jcc(Cc::E, found);
        alu_r_imm(a, CMP, xr, egcl_rt::value::NIL.0 as i32); // NIL is a symbol
        a.jcc(Cc::E, found);
        alu_r_imm(a, CMP, xr, egcl_rt::value::T.0 as i32); // T is a symbol
        a.jcc(Cc::E, found);
    } else if bits == TypeBits::FIXNUM.join(TypeBits::BIGNUM) {
        // INTEGER = immediate fixnum OR a heap object whose header widetag is
        // BIGNUM. Guard the heap tag before dereferencing the tagged pointer.
        single_tag(a, 0);
        a.jcc(Cc::E, found);
        single_tag(a, egcl_rt::value::TAG_HEAP_OBJECT as i32);
        a.jcc(Cc::Ne, not_found);
        mov_rr(a, SCRATCH, xr);
        alu_r_imm(a, AND, SCRATCH, -8); // clear the low tag bits
                                        // cmp byte ptr [scratch + 7], BIGNUM. ObjectHeader::type_id occupies
                                        // bits 63:56, hence byte offset 7 on the supported little-endian x86-64.
        a.extend_from_slice(&[0x80, 0x7A, 0x07, egcl_rt::object::type_id::BIGNUM]);
        a.jcc(Cc::E, found);
    } else if bits == TypeBits::STRING {
        // NOTE: `typep_class::STRING` is deliberately NOT accepted here, so it
        // falls through to the declining `else` below and T2 uses a real call
        // (bliss-c02n). This test accepts only the two SIMPLE layouts, while a
        // string with a fill pointer, an adjustable one, or a DISPLACED one is
        // a COMPLEX_ARRAY -- so as a PREDICATE it answered NIL where every
        // other tier answers T, and a hot (stringp x) / (typep x 'string)
        // silently flipped. The CONS and LIST classes were removed from this
        // chain for the same reason (bliss-74rl).
        //
        // `bits == TypeBits::STRING` stays: that form reaches here only from
        // LAYOUT GUARDS, where proving the simple layout is the whole point and
        // a complex string must deopt rather than be read raw. Deopting is
        // conservative; answering NIL to a predicate is not.
        //
        // A string predicate must prove the heap tag before reading the header.
        // Both simple UTF-8 string layouts share the same length/data offsets.
        single_tag(a, egcl_rt::value::TAG_HEAP_OBJECT as i32);
        a.jcc(Cc::Ne, not_found);
        mov_rr(a, SCRATCH, xr);
        alu_r_imm(a, AND, SCRATCH, -8);
        a.extend_from_slice(&[
            0x80,
            0x7A,
            0x07,
            egcl_rt::object::type_id::SIMPLE_BASE_STRING,
        ]);
        a.jcc(Cc::E, found);
        a.extend_from_slice(&[
            0x80,
            0x7A,
            0x07,
            egcl_rt::object::type_id::SIMPLE_CHARACTER_STRING,
        ]);
        a.jcc(Cc::E, found);
    } else if matches!(
        class,
        Some(typep_class::PACKAGE) | Some(typep_class::HASH_TABLE)
    ) {
        single_tag(a, egcl_rt::value::TAG_HEAP_OBJECT as i32);
        a.jcc(Cc::Ne, not_found);
        mov_rr(a, SCRATCH, xr);
        alu_r_imm(a, AND, SCRATCH, -8);
        let type_id = if class == Some(typep_class::PACKAGE) {
            egcl_rt::object::type_id::PACKAGE
        } else {
            egcl_rt::object::type_id::HASH_TABLE
        };
        a.extend_from_slice(&[0x80, 0x7A, 0x07, type_id]);
        a.jcc(Cc::E, found);
    } else {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::TypeCheck)));
    }
    // Fall-through: not the type.
    a.bind(not_found);
    mov_imm64(a, dst, egcl_rt::value::NIL.0 as i64);
    a.jmp(end);
    a.bind(found);
    mov_imm64(a, dst, egcl_rt::value::T.0 as i64);
    a.bind(end);
    Ok(())
}

/// Guard `x` is one of the two simple UTF-8 string layouts and leave its
/// untagged object pointer in `SCRATCH`.
fn guard_simple_string(a: &mut Asm, x: u8, deopt: egcl_rt::asm::Label) {
    const AND: u8 = 4;
    const CMP: u8 = 7;
    mov_rr(a, SCRATCH, x);
    alu_r_imm(a, AND, SCRATCH, 7);
    alu_r_imm(a, CMP, SCRATCH, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    a.jcc(Cc::Ne, deopt);
    mov_rr(a, SCRATCH, x);
    alu_r_imm(a, AND, SCRATCH, -8);
    let layout_ok = a.label();
    a.extend_from_slice(&[
        0x80,
        0x7A,
        0x07,
        egcl_rt::object::type_id::SIMPLE_BASE_STRING,
    ]);
    a.jcc(Cc::E, layout_ok);
    a.extend_from_slice(&[
        0x80,
        0x7A,
        0x07,
        egcl_rt::object::type_id::SIMPLE_CHARACTER_STRING,
    ]);
    a.jcc(Cc::Ne, deopt);
    a.bind(layout_ok);
}

/// Emit an explicit simple-string layout guard.  Its result is the same tagged
/// value with a refined SSA identity, which gives later raw layout operations a
/// data dependency on the proof and lets post-inlining guard elimination forward
/// a dominated guard result to the dominating guard's result.
fn emit_string_layout_guard(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode};
    if data.args.len() != 1
        || data.results.len() != 1
        || !matches!(data.aux, AuxData::StringLayout)
        || !data.flags.guard
        || !data.flags.effectful
        || data.frame_state.is_none()
    {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::Guard)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    guard_simple_string(a, xr, deopt);
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_rr(a, dst, xr);
    Ok(())
}

/// Guard a tagged value as a cons and return the same value under a refined SSA
/// identity.  Field loads consume that identity and therefore contain no
/// private type check; redundant proofs are removed by `GuardElim` before this
/// emitter runs.
fn emit_cons_guard(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode, TypeBits};
    if data.args.len() != 1
        || data.results.len() != 1
        || !matches!(data.aux, AuxData::TypeTag(t) if t.bits == TypeBits::CONS)
        || !data.flags.guard
        || !data.flags.effectful
        || data.frame_state.is_none()
    {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::Guard)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    mov_rr(a, SCRATCH, xr);
    alu_r_imm(a, 4 /* AND */, SCRATCH, 7);
    alu_r_imm(
        a,
        7, /* CMP */
        SCRATCH,
        egcl_rt::value::TAG_CONS as i32,
    );
    a.jcc(Cc::Ne, deopt);
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_rr(a, dst, xr);
    Ok(())
}

/// A speculated fixnum operand proof reified as an SSA guard value
/// (speculate.rs materialize_fixnum_operand_guards, egcl-x5y.25b): test the
/// low three tag bits are zero, deopt otherwise, and pass the value through to
/// its result register. Downstream typed ops skip their own operand re-check
/// because the result's inferred type is FIXNUM.
fn emit_fixnum_value_guard(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::{AuxData, Opcode, TypeBits};
    if data.args.len() != 1
        || data.results.len() != 1
        || !matches!(data.aux, AuxData::TypeTag(t) if t.bits == TypeBits::FIXNUM)
        || !data.flags.guard
        || !data.flags.effectful
        || data.frame_state.is_none()
    {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::Guard)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    guard_fixnum(a, xr, deopt);
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_rr(a, dst, xr);
    Ok(())
}

/// Load CAR/CDR from a value refined by an explicit dominating cons guard.
fn emit_cons_field_load(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    if data.args.len() != 1
        || data.results.len() != 1
        || !matches!(data.opcode, Opcode::Car | Opcode::Cdr)
        || data.flags.guard
        || data.flags.effectful
        || data.frame_state.is_some()
    {
        return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    // Cons cells are headerless and the tagged pointer differs only in its low
    // bits. Keep the base in reserved SCRATCH so result/input coalescing is safe.
    mov_rr(a, SCRATCH, xr);
    alu_r_imm(a, 4 /* AND */, SCRATCH, -8);
    let dst = framed_alloc(reg, pool, data.results[0])?;
    let offset = if data.opcode == Opcode::Car { 0 } else { 8 };
    mov_mem64_disp8(a, dst, SCRATCH, offset);
    Ok(())
}

/// Raw UTF-8 byte-length load from an explicitly layout-guarded string.  The
/// result is a tagged non-negative fixnum. This is intentionally below
/// CL:LENGTH: byte length is useful for emptiness/bounds checks, but is not
/// character length for non-ASCII strings.
fn emit_string_byte_length(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    if data.args.len() != 1
        || data.results.len() != 1
        || data.flags.guard
        || data.flags.effectful
        || data.frame_state.is_some()
    {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::StringByteLength)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    mov_rr(a, SCRATCH, xr);
    alu_r_imm(a, 4 /* AND */, SCRATCH, -8);
    let dst = framed_alloc(reg, pool, data.results[0])?;
    mov_mem64_disp8(a, dst, SCRATCH, 8);
    shl_imm(a, dst, 3);
    Ok(())
}

/// Guarded ASCII fast path for `(CHAR simple-string index)`.  The string layout
/// was proved by an explicit dominating guard; this operation guards only its
/// index, bounds, and UTF-8/ASCII assumptions. UTF-8 leading bytes >= 128 deopt
/// so the stdlib performs full codepoint decoding.
fn emit_string_ascii_char_at(
    a: &mut Asm,
    data: &crate::t2::ir::InstData,
    reg: &mut std::collections::HashMap<crate::t2::ir::Value, u8>,
    pool: &mut Vec<u8>,
    const_tagged: &std::collections::HashMap<crate::t2::ir::Value, u64>,
    deopt: egcl_rt::asm::Label,
) -> Result<(), EmitError> {
    use crate::t2::ir::Opcode;
    if data.args.len() != 2
        || data.results.len() != 1
        || !data.flags.guard
        || !data.flags.effectful
        || data.frame_state.is_none()
    {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::StringAsciiCharAt)));
    }
    let xr = *reg
        .get(&data.args[0])
        .ok_or(EmitError::UnsupportedOp(0xF2))?;
    mov_rr(a, SCRATCH, xr);
    alu_r_imm(a, 4 /* AND */, SCRATCH, -8);

    let dst = framed_alloc(reg, pool, data.results[0])?;
    let index = data.args[1];
    if let Some(&tagged) = const_tagged.get(&index) {
        mov_imm64(a, dst, tagged as i64);
    } else {
        let ir = *reg.get(&index).ok_or(EmitError::UnsupportedOp(0xF2))?;
        mov_rr(a, dst, ir);
    }
    // Fixnum tag, non-negative index.
    a.push(rex_w(0, dst));
    a.extend_from_slice(&[0xF7, 0xC0 | (dst & 7)]); // test r64, imm32 (/0)
    a.extend_from_slice(&7i32.to_le_bytes());
    a.jcc(Cc::Ne, deopt);
    alu_r_imm(a, 7, dst, 0);
    a.jcc(Cc::L, deopt);
    sar_imm(a, dst, 3);
    // `index >= byte_length` is out of bounds. Both are non-negative, so the
    // signed comparison is equivalent over all representable object sizes.
    cmp_mem64_disp8(a, dst, SCRATCH, 8);
    a.jcc(Cc::Ge, deopt);
    // A byte index equals a character index only while every byte through the
    // selected element is ASCII.  Scanning the prefix prevents e.g. byte 2 of
    // "éa" from being returned as character 2; any multibyte prefix deopts to
    // the stdlib's UTF-8-aware CL:CHAR implementation.
    //
    // The scan is a moving-POINTER walk that needs no registers beyond the
    // emitter's own: SCRATCH (holding the object base) becomes the cursor over
    // &data[0]..=&data[idx], dst (holding the untagged index) becomes the
    // bound address, and RAX carries the transient byte. The old version used
    // rsi/rdi cursors, which silently corrupted values homed there the moment
    // rsi joined the allocatable pool (bliss-x5y.29).
    alu_rr(a, 0x01, dst, SCRATCH); // dst = base + idx
    alu_r_imm(a, 0, dst, 16); // dst = &data[idx] (the bound)
    alu_r_imm(a, 0, SCRATCH, 16); // SCRATCH = &data[0] (the cursor)
    let scan = a.label();
    let selected = a.label();
    a.bind(scan);
    movzx_mem8(a, RAX, SCRATCH);
    alu_r_imm(a, 7, RAX, 128);
    a.jcc(Cc::Ge, deopt);
    cmp_rr(a, SCRATCH, dst);
    a.jcc(Cc::E, selected);
    alu_r_imm(a, 0, SCRATCH, 1);
    a.jmp(scan);
    a.bind(selected);
    mov_rr(a, dst, RAX);
    shl_imm(a, dst, 3);
    or_imm8(a, dst, egcl_rt::value::TAG_CHARACTER as u8);
    Ok(())
}

fn is_fixnum_producing_op(op: crate::t2::ir::Opcode) -> bool {
    use crate::t2::ir::Opcode::*;
    matches!(
        op,
        FixnumAdd
            | FixnumSub
            | FixnumMul
            | FixnumNeg
            | LogAnd
            | LogOr
            | LogXor
            | LogNot
            | FixnumShl
            | FixnumShr
    )
}

/// A constant-producing opcode. Non-moving constants are materialised where
/// used; `ConstHeapObj` is recognized here but emitted as a rooted slot load.
fn is_const_opcode(op: crate::t2::ir::Opcode) -> bool {
    use crate::t2::ir::Opcode::*;
    matches!(
        op,
        ConstFixnum | ConstFloat | ConstChar | ConstSymbol | ConstNil | ConstT | ConstHeapObj
    )
}

fn is_moving_gc_reference(value: egcl_rt::value::EgclVal) -> bool {
    value.is_cons() || value.is_heap_object() || value.is_function()
}

/// Emit `f` (a speculated function, straight-line or branching) as native code.
/// `c2i_deopt_addr`, `c2i_call_addr`, and `c2i_call_slice_addr` are the
/// interpreter's deopt and fixed-/wide-arity call adapters. `c2i_mv_addr` clears
/// multiple values when called with a zero count and copies them to an
/// activation-frame destination otherwise. A function that contains a `Call`
/// uses callee-saved value registers (so the call cannot clobber live values)
/// and a small frame.
/// These generic call adapters receive a zero profiling token. Production
/// x86-64 callers can supply resolved linkage slots through
/// `emit_framed_with_direct_natives`; those cells must outlive the code.
// Public adapter addresses remain explicit to preserve the emitter API.
#[allow(clippy::too_many_arguments)]
pub fn emit_framed(
    f: &Function,
    c2i_deopt_addr: u64,
    c2i_deopt_t2_addr: u64,
    c2i_call_addr: u64,
    c2i_call_slice_addr: u64,
    c2i_load_global_addr: u64,
    c2i_load_function_addr: u64,
    c2i_store_global_addr: u64,
    c2i_mv_addr: u64,
    self_sym: Option<u32>,
) -> Result<FramedCode, EmitError> {
    emit_framed_inner(
        f,
        c2i_deopt_addr,
        c2i_deopt_t2_addr,
        c2i_call_addr,
        c2i_call_slice_addr,
        c2i_load_global_addr,
        c2i_load_function_addr,
        c2i_store_global_addr,
        c2i_mv_addr,
        0,
        0,
        None,
        self_sym,
        &[],
        0,
        EnvironmentCalls::default(),
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        None,
    )
}

/// Emit production T2 code whose native roots are synchronized through shadow
/// slots appended after `activation_slots` in the owning EgclStack frame.
/// A nonzero `c2i_transfer_pending_addr` is an `extern "C" fn() -> u64` that
/// returns nonzero for a pending error/nonlocal exit. It must not allocate,
/// safepoint, or call Lisp; results are saved on the native stack during it.
/// `c2i_poll_addr` is the s390x back-edge safepoint callback (`fn() -> u64`):
/// it may park for GC and returns nonzero when native execution must exit.
// Public ABI hooks and activation layout are independent emission inputs.
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_with_activation_slots(
    f: &Function,
    c2i_deopt_addr: u64,
    c2i_deopt_t2_addr: u64,
    c2i_call_addr: u64,
    c2i_call_slice_addr: u64,
    c2i_load_global_addr: u64,
    c2i_load_function_addr: u64,
    c2i_store_global_addr: u64,
    c2i_mv_addr: u64,
    c2i_recovery_toggle_addr: u64,
    c2i_transfer_pending_addr: u64,
    c2i_poll_addr: u64,
    activation_slots: u16,
    self_sym: Option<u32>,
) -> Result<FramedCode, EmitError> {
    emit_framed_with_direct_natives(
        f,
        c2i_deopt_addr,
        c2i_deopt_t2_addr,
        c2i_call_addr,
        c2i_call_slice_addr,
        c2i_load_global_addr,
        c2i_load_function_addr,
        c2i_store_global_addr,
        c2i_mv_addr,
        c2i_recovery_toggle_addr,
        c2i_transfer_pending_addr,
        c2i_poll_addr,
        activation_slots,
        self_sym,
        &[],
        0,
        &[],
        EnvironmentCalls::default(),
    )
}

/// Captured lexical access uses the original root body's name metadata.
/// `body` is an opaque runtime-owned metadata address retained with the code.
/// Helpers have C signatures
/// `(body, name_index) -> u64` and `(body, name_index, value)` respectively.
/// Capturing bodies are excluded from inlining, so all indexes name this body.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentCalls {
    pub body: u64,
    pub load: u64,
    pub store: u64,
}

/// Execution-owned linkage slots resolved before background compilation.
/// The runtime must retain these cells for the lifetime of the emitted code.
#[derive(Clone, Copy, Debug)]
pub struct NamedCallTarget {
    pub symbol: u32,
    pub cell_address: u64,
    pub register_entry: u64,
    pub slice_entry: u64,
}

/// [`emit_framed_with_activation_slots`] plus the native callees the runtime
/// resolved for this function's call sites. The s390x emitter enters a listed
/// callee directly (bliss-6j6pk); the other emitters ignore that list.
/// On x86-64, `named_calls` supplies retained linkage slots for named calls.
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_with_direct_natives(
    f: &Function,
    c2i_deopt_addr: u64,
    c2i_deopt_t2_addr: u64,
    c2i_call_addr: u64,
    c2i_call_slice_addr: u64,
    c2i_load_global_addr: u64,
    c2i_load_function_addr: u64,
    c2i_store_global_addr: u64,
    c2i_mv_addr: u64,
    c2i_recovery_toggle_addr: u64,
    c2i_transfer_pending_addr: u64,
    c2i_poll_addr: u64,
    activation_slots: u16,
    self_sym: Option<u32>,
    direct_natives: &[DirectNativeTarget],
    code_id: u64,
    named_calls: &[NamedCallTarget],
    environment: EnvironmentCalls,
) -> Result<FramedCode, EmitError> {
    if cfg!(target_arch = "aarch64") {
        return super::emit_a64::emit_framed_with_runtime(
            f,
            c2i_deopt_t2_addr,
            activation_slots,
            super::emit_a64::RuntimeCalls {
                deopt: c2i_deopt_addr,
                call_slice: c2i_call_slice_addr,
                call_slice_rooted: 0,
                load_global: c2i_load_global_addr,
                load_function: c2i_load_function_addr,
                store_global: c2i_store_global_addr,
                multiple_values: c2i_mv_addr,
                transfer_pending: c2i_transfer_pending_addr,
                poll: c2i_poll_addr,
                self_sym: None,
                direct_natives: Vec::new(),
                code_id: 0,
            },
        );
    }
    if cfg!(all(target_arch = "powerpc64", target_endian = "little")) {
        return super::emit_ppc64le::emit_framed_with_runtime(
            f,
            c2i_deopt_t2_addr,
            activation_slots,
            super::emit_ppc64le::RuntimeCalls {
                deopt: c2i_deopt_addr,
                call_slice: c2i_call_slice_addr,
                call_slice_rooted: 0,
                load_global: c2i_load_global_addr,
                load_function: c2i_load_function_addr,
                store_global: c2i_store_global_addr,
                multiple_values: c2i_mv_addr,
                transfer_pending: c2i_transfer_pending_addr,
                poll: c2i_poll_addr,
                self_sym: None,
                direct_natives: Vec::new(),
                code_id: 0,
            },
        );
    }
    if cfg!(target_arch = "riscv64") {
        return super::emit_riscv64::emit_framed_with_runtime(
            f,
            c2i_deopt_t2_addr,
            activation_slots,
            super::emit_riscv64::RuntimeCalls {
                call_slice: c2i_call_slice_addr,
                load_global: c2i_load_global_addr,
                load_function: c2i_load_function_addr,
                store_global: c2i_store_global_addr,
                multiple_values: c2i_mv_addr,
                transfer_pending: c2i_transfer_pending_addr,
                poll: c2i_poll_addr,
            },
        );
    }
    if cfg!(target_arch = "s390x") {
        return super::emit_s390x::emit_framed_with_runtime(
            f,
            c2i_deopt_t2_addr,
            activation_slots,
            super::emit_s390x::RuntimeCalls {
                deopt: c2i_deopt_addr,
                call_slice: c2i_call_slice_addr,
                call_slice_rooted: call_slice_rooted_addr(),
                load_global: c2i_load_global_addr,
                load_function: c2i_load_function_addr,
                store_global: c2i_store_global_addr,
                multiple_values: c2i_mv_addr,
                transfer_pending: c2i_transfer_pending_addr,
                poll: c2i_poll_addr,
                self_sym,
                direct_natives: direct_natives.to_vec(),
                code_id,
            },
        );
    }
    emit_framed_inner(
        f,
        c2i_deopt_addr,
        c2i_deopt_t2_addr,
        c2i_call_addr,
        c2i_call_slice_addr,
        c2i_load_global_addr,
        c2i_load_function_addr,
        c2i_store_global_addr,
        c2i_mv_addr,
        c2i_recovery_toggle_addr,
        c2i_transfer_pending_addr,
        Some(activation_slots),
        self_sym,
        named_calls,
        code_id,
        environment,
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        None,
    )
}

/// Reserved request discriminator, outside the u32 symbol-index domain.
/// Native-cleanup call veneers must implement this as THROW(tag, primary),
/// preserving the execution's existing multiple values.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const TRANSFER_THROW_REQUEST: u64 = u64::MAX;

/// Catch registration requests encode the establishing BCP in the low 32 bits.
/// Entry supplies one rooted tag argument; exit supplies none. Both preserve
/// multiple values and return through helper-v2 before any native transfer.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const TRANSFER_CATCH_ENTER_REQUEST: u64 = 1_u64 << 32;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const TRANSFER_CATCH_LEAVE_REQUEST: u64 = 2_u64 << 32;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const TRANSFER_HANDLER_ENTER_REQUEST: u64 = 3_u64 << 32;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const TRANSFER_HANDLER_LEAVE_REQUEST: u64 = 4_u64 << 32;
pub const TRANSFER_HANDLER_BIND_ENTER_REQUEST: u64 = 5_u64 << 32;
pub const TRANSFER_HANDLER_BIND_LEAVE_REQUEST: u64 = 6_u64 << 32;
pub const TRANSFER_RESTART_CASE_ENTER_REQUEST: u64 = 7_u64 << 32;
pub const TRANSFER_RESTART_CASE_LEAVE_REQUEST: u64 = 8_u64 << 32;
pub const TRANSFER_HOST_EVAL_REQUEST: u64 = 9_u64 << 32;
pub const TRANSFER_FUNCTION_LOOKUP_REQUEST: u64 = 10_u64 << 32;

/// Helper-v2 call request, live in the generated caller's temporary frame until
/// normal return or completion of cold preparation. Arguments and shadow roots
/// reside in the rooted owning activation, not in this unscanned request.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[repr(C)]
pub struct TransferCallRequest {
    pub symbol: u64,
    pub nargs: usize,
    pub args: *mut egcl_rt::value::EgclVal,
    pub activation: *mut egcl_rt::value::EgclVal,
}

/// Precise guard continuation serialized on the generated stack. The helper
/// must copy the stream into scanned interpreter slots before any GC or yield.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[repr(C)]
pub struct TransferDeoptRequest {
    pub n_scopes: u64,
    pub n_words: u64,
    pub words: *const u64,
    pub activation: *mut egcl_rt::value::EgclVal,
}

/// A generated self-call owns this record until ordinary return or cold
/// retirement. Its precise Lisp frame belongs to the same retained code as
/// its caller; the host-stack links contain no movable Lisp values.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[repr(C)]
pub struct RecursiveActivation {
    pub previous: *mut RecursiveActivation,
    pub frame: *mut egcl_rt::stack::Frame,
    pub stack: *const egcl_rt::EgclStack,
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[repr(C)]
pub struct RecursiveCallRequest {
    pub call: TransferCallRequest,
    pub activation: RecursiveActivation,
    pub reserved: u64,
}

/// The prepare veneer returns activation slots, or a completed compatibility
/// result with a null record frame. Finish only retires the supplied record;
/// it must not allocate, collect, yield, signal, or invoke Lisp.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[derive(Clone, Copy)]
pub struct RecursiveTransfer {
    pub symbol: u32,
    pub arity: usize,
    pub prepare: u64,
    pub finish: u64,
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const _: () = {
    assert!(std::mem::size_of::<TransferDeoptRequest>() == 32);
    assert!(std::mem::offset_of!(TransferDeoptRequest, activation) == 24);
    assert!(std::mem::size_of::<RecursiveCallRequest>() == 64);
    assert!(std::mem::offset_of!(RecursiveCallRequest, activation) == 32);
    assert!(std::mem::offset_of!(RecursiveActivation, frame) == 8);
    assert!(std::mem::offset_of!(RecursiveActivation, stack) == 16);
};

/// Cleanup completion uses the same temporary area size as a call request.
/// It returns a restored normal answer or a pending transfer through helper-v2.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[repr(C)]
pub struct TransferCleanupRequest {
    pub cleanup_bcp: u64,
    pub resume_bcp: u64,
    pub reserved: u64,
    pub activation: *mut egcl_rt::value::EgclVal,
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[derive(Clone, Copy)]
enum CleanupEmission {
    Normal {
        save: u64,
        restore: u64,
    },
    Native {
        save: u64,
        complete: u64,
        clear_mv: u64,
        catch_landing: u64,
        handler_landing: u64,
    },
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
struct TransferEmission {
    veneer: u64,
    named_veneers: Vec<(u32, u64)>,
    cleanup: Option<CleanupEmission>,
    poll_veneer: Option<u64>,
    recursion: Option<RecursiveTransfer>,
    deopt_veneer: Option<u64>,
    deopt_returns: Vec<u32>,
    sites: Vec<crate::t2::transfer_sites::SysvTransferSite>,
    landings: std::collections::HashMap<crate::t2::ir::Block, (u32, u32)>,
    catch_landings: std::collections::HashMap<crate::t2::ir::Block, Vec<(u32, u32, u32)>>,
    handler_landings: std::collections::HashMap<crate::t2::ir::Block, Vec<(u32, u32, u32, u32)>>,
}

/// Opt-in SysV emission through a helper-v2 veneer. The owner must root all
/// activation slots (including `shadow_root_slots`), retain code/definitions and
/// adapters, and enter through a supported native segment. This is not yet a
/// production installation API: unboxed values, guards, OSR and other helper
/// classes are refused until their contracts are wired. Legacy entries reject Invoke.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub fn emit_framed_transfers(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_framed_transfers_with_cleanup(f, call_veneer, activation_slots, None)
}

/// Cleanup helpers take `(cleanup_bcp, resume_bcp, primary)` and return the
/// restored primary (ignored for save). They must return normally without Lisp
/// allocation, yielding or signaling. The owning entry roots saved values.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub fn emit_framed_transfers_with_cleanup(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    cleanup: Option<(u64, u64)>,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_transfer_function(
        f,
        call_veneer,
        activation_slots,
        cleanup.map(|(save, restore)| CleanupEmission::Normal { save, restore }),
        None,
        None,
        None,
        &[],
    )
}

/// Emit verified exceptional cleanup edges and helper-v2 cleanup completion.
/// `clear_mv` clears secondary values without allocating, yielding or signaling.
/// `save` has the normal save-helper contract above; `complete` is a veneer for
/// a helper consuming `TransferCleanupRequest`. The owner must root the pending
/// continuation and repair source homes before entering a checked cold landing.
/// The call veneer must implement THROW and CATCH registration requests as well
/// as ordinary symbol calls; older helper protocols must not use this emitter.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub fn emit_framed_native_cleanups(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    save: u64,
    complete: u64,
    clear_mv: u64,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_transfer_function(
        f,
        call_veneer,
        activation_slots,
        Some(CleanupEmission::Native {
            save,
            complete,
            clear_mv,
            catch_landing: 0,
            handler_landing: 0,
        }),
        None,
        None,
        None,
        &[],
    )
}

/// Extend catch/cleanup emission with a noncollecting clause-delivery helper.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_native_handlers(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    save: u64,
    complete: u64,
    clear_mv: u64,
    catch_landing: u64,
    handler_landing: u64,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_framed_native_handlers_with_poll(
        f,
        call_veneer,
        activation_slots,
        save,
        complete,
        clear_mv,
        catch_landing,
        handler_landing,
        0,
    )
}

/// Native-cleanup emission with a helper veneer used at loop-header polls.
/// The veneer returns normally when the execution may continue and enters the
/// existing capture/landing path when GC, a signal, or a pending native error
/// requires the segment to leave.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_native_handlers_with_poll(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    save: u64,
    complete: u64,
    clear_mv: u64,
    catch_landing: u64,
    handler_landing: u64,
    poll_veneer: u64,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_framed_native_handlers_with_recursion(
        f,
        call_veneer,
        activation_slots,
        save,
        complete,
        clear_mv,
        catch_landing,
        handler_landing,
        poll_veneer,
        None,
        None,
    )
}

/// Extend mapped segment calls with same-definition native recursion. The
/// runtime must provide distinct precise activation records and cold retirement
/// for every admitted recursive frame; the ordinary checked ABI cannot use it.
/// A deopt veneer admits supported tagged guards and consumes
/// `TransferDeoptRequest`. It must complete the exact continuation, preserve
/// the owning frame's extent, and use its own completed-deopt failure route.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_native_handlers_with_recursion(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    save: u64,
    complete: u64,
    clear_mv: u64,
    catch_landing: u64,
    handler_landing: u64,
    poll_veneer: u64,
    recursion: Option<RecursiveTransfer>,
    deopt_veneer: Option<u64>,
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    emit_framed_native_handlers_with_entries(
        f,
        call_veneer,
        activation_slots,
        save,
        complete,
        clear_mv,
        catch_landing,
        handler_landing,
        poll_veneer,
        recursion,
        deopt_veneer,
        &[],
    )
}

/// Supply retained per-symbol adapters for mapped named calls. Each adapter
/// consumes `TransferCallRequest` and preserves the ordinary veneer contract,
/// including capture of the caller's mapped return address on exceptional exit.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_native_handlers_with_entries(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    save: u64,
    complete: u64,
    clear_mv: u64,
    catch_landing: u64,
    handler_landing: u64,
    poll_veneer: u64,
    recursion: Option<RecursiveTransfer>,
    deopt_veneer: Option<u64>,
    named_veneers: &[(u32, u64)],
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    if catch_landing == 0 || handler_landing == 0 {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    emit_transfer_function(
        f,
        call_veneer,
        activation_slots,
        Some(CleanupEmission::Native {
            save,
            complete,
            clear_mv,
            catch_landing,
            handler_landing,
        }),
        (poll_veneer != 0).then_some(poll_veneer),
        recursion,
        deopt_veneer,
        named_veneers,
    )
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
// Keep the common emitter aligned with the public helper-address entry points.
#[allow(clippy::too_many_arguments)]
fn emit_transfer_function(
    f: &Function,
    call_veneer: u64,
    activation_slots: u16,
    cleanup: Option<CleanupEmission>,
    poll_veneer: Option<u64>,
    recursion: Option<RecursiveTransfer>,
    deopt_veneer: Option<u64>,
    named_veneers: &[(u32, u64)],
) -> Result<(FramedCode, crate::t2::transfer_sites::SysvTransferTable), EmitError> {
    use crate::t2::ir::{AuxData, Opcode};
    if deopt_veneer == Some(0) || named_veneers.iter().any(|(_, entry)| *entry == 0) {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    let native_cleanups = matches!(cleanup, Some(CleanupEmission::Native { .. }));
    if cleanup.is_some_and(|helper| match helper {
        CleanupEmission::Normal { save, restore } => save == 0 || restore == 0,
        CleanupEmission::Native {
            save,
            complete,
            clear_mv,
            ..
        } => save == 0 || complete == 0 || clear_mv == 0,
    }) || call_veneer == 0
        || (0..f.num_values()).any(|i| {
            f.value(crate::t2::ir::Value(i as u32)).repr
                != crate::t2::ir::ValueRepresentation::Tagged
        })
        || usize::from(activation_slots) < f.block(f.entry()).params.len()
        || crate::t2::verify::verify(f).is_err()
    {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            if data.opcode == Opcode::HandlerLanding {
                if !matches!(cleanup, Some(CleanupEmission::Native { handler_landing, .. }) if handler_landing != 0)
                {
                    return Err(EmitError::UnsupportedOp(0xFA));
                }
                continue;
            }
            if data.opcode == Opcode::CatchLanding {
                if !matches!(cleanup, Some(CleanupEmission::Native { catch_landing, .. }) if catch_landing != 0)
                {
                    return Err(EmitError::UnsupportedOp(0xFA));
                }
                continue;
            }
            if native_cleanups && data.opcode == Opcode::Invoke {
                let edge = &data.targets[1];
                let cold = f.block(edge.block);
                // The published entry starts at NlxTransfer. Until incoming
                // edge moves and earlier cold instructions have landing maps,
                // accept only the builder's parameter-free transfer block.
                if cold.insts.len() != 1 || !cold.params.is_empty() || !edge.args.is_empty() {
                    return Err(EmitError::UnsupportedOp(0xFD));
                }
            }
            if (cleanup.is_some() && data.opcode == Opcode::CleanupSave)
                || (!native_cleanups && cleanup.is_some() && data.opcode == Opcode::CleanupRestore)
                || (native_cleanups
                    && matches!(data.opcode, Opcode::CleanupLanding | Opcode::ClearMv))
            {
                continue;
            }
            if !native_cleanups
                && (data.opcode == Opcode::Invoke
                    && matches!(
                        data.aux,
                        AuxData::CleanupContinuation { .. }
                            | AuxData::HostEval(_)
                            | AuxData::FunctionLookup(_)
                            | AuxData::TransferThrow
                            | AuxData::CatchScope { .. }
                            | AuxData::HandlerScope { .. }
                            | AuxData::HandlerBindScope { .. }
                            | AuxData::RestartCaseScope { .. }
                    )
                    || data.opcode == Opcode::NlxTransfer && !data.targets.is_empty())
            {
                return Err(EmitError::UnsupportedOp(op_tag(data.opcode)));
            }
            if deopt_veneer.is_some() && matches!(data.opcode,
                Opcode::Guard | Opcode::FixnumAdd | Opcode::FixnumSub
                    | Opcode::FixnumMul | Opcode::FixnumNeg
                    | Opcode::FixnumCmpEq | Opcode::FixnumCmpLt | Opcode::FixnumCmpLe
                    | Opcode::FixnumCmpGt | Opcode::FixnumCmpGe
                    | Opcode::LogAnd | Opcode::LogOr | Opcode::LogXor | Opcode::LogNot
                    | Opcode::FixnumShl | Opcode::FixnumShr)
            {
                continue;
            }
            if !matches!(
                f.inst(inst).opcode,
                Opcode::Invoke
                    | Opcode::NlxTransfer
                    | Opcode::Trap
                    | Opcode::Return
                    | Opcode::Jump
                    | Opcode::Brif
                    | Opcode::ConstFixnum
                    | Opcode::ConstNil
                    | Opcode::ConstT
                    | Opcode::ConstSymbol
                    | Opcode::ConstChar
                    | Opcode::GenericEq
                    | Opcode::TypeCheck
                    | Opcode::MemoryFence
            ) {
                return Err(EmitError::UnsupportedOp(op_tag(f.inst(inst).opcode)));
            }
        }
    }
    let mut transfers = TransferEmission {
        veneer: call_veneer,
        named_veneers: named_veneers.to_vec(),
        cleanup,
        poll_veneer,
        recursion,
        deopt_veneer,
        deopt_returns: Vec::new(),
        sites: vec![],
        landings: std::collections::HashMap::new(),
        catch_landings: std::collections::HashMap::new(),
        handler_landings: std::collections::HashMap::new(),
    };
    let code = emit_framed_inner(
        f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        match cleanup {
            Some(CleanupEmission::Native { clear_mv, .. }) => clear_mv,
            _ => 0,
        },
        0,
        0,
        Some(activation_slots),
        None,
        &[],
        0,
        EnvironmentCalls::default(),
        Some(&mut transfers),
    )?;
    let mut landings = Vec::new();
    let mut catch_landings = Vec::new();
    let mut handler_landings = Vec::new();
    for site in &transfers.sites {
        let cold = f.inst(site.map.call).targets[1].block;
        if let Some(entries) = transfers.handler_landings.get(&cold) {
            for &(entry_offset, push_bcp, table_index, clause_index) in entries {
                handler_landings.push(crate::t2::transfer_sites::SysvHandlerLanding {
                    return_offset: site.return_offset,
                    entry_offset,
                    push_bcp,
                    table_index,
                    clause_index,
                });
            }
        }
        if let Some(entries) = transfers.catch_landings.get(&cold) {
            for &(entry_offset, push_bcp, resume_bcp) in entries {
                catch_landings.push(crate::t2::transfer_sites::SysvCatchLanding {
                    return_offset: site.return_offset,
                    entry_offset,
                    push_bcp,
                    resume_bcp,
                });
            }
        }
        if let Some(&(entry_offset, cleanup_bcp)) = transfers.landings.get(&cold) {
            landings.push(crate::t2::transfer_sites::SysvCleanupLanding {
                return_offset: site.return_offset,
                entry_offset,
                cleanup_bcp,
            });
        }
    }
    let table = crate::t2::transfer_sites::SysvTransferTable::new(code.code.len(), transfers.sites)
        .and_then(|table| table.with_deopt_returns(transfers.deopt_returns))
        .and_then(|table| table.with_cleanup_landings(&code.code, &landings))
        .and_then(|table| table.with_catch_landings(&code.code, &catch_landings))
        .and_then(|table| {
            table.with_handler_landings(&code.code, &f.handler_cases, &handler_landings)
        })
        .map_err(|_| EmitError::UnsupportedOp(0xFD))?;
    Ok((code, table))
}

// Shared implementation mirrors both public emitter entry points above.
#[allow(clippy::too_many_arguments)]
fn emit_framed_inner(
    f: &Function,
    c2i_deopt_addr: u64,
    c2i_deopt_t2_addr: u64,
    c2i_call_addr: u64,
    c2i_call_slice_addr: u64,
    c2i_load_global_addr: u64,
    c2i_load_function_addr: u64,
    c2i_store_global_addr: u64,
    c2i_mv_addr: u64,
    c2i_recovery_toggle_addr: u64,
    c2i_transfer_pending_addr: u64,
    activation_slots: Option<u16>,
    self_sym: Option<u32>,
    named_calls: &[NamedCallTarget],
    code_id: u64,
    environment: EnvironmentCalls,
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))] mut transfers: Option<
        &mut TransferEmission,
    >,
) -> Result<FramedCode, EmitError> {
    use crate::t2::frame_state::ValueSource;
    use crate::t2::ir::{
        AuxData, Block, Inst, Opcode, TypeBits, Value, ValueDef, ValueRepresentation,
    };
    use std::collections::{HashMap, HashSet};

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let transfer_mode = transfers.is_some();
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let transfer_mode = false;
    if f.block_order().iter().any(|&b| {
        f.block(b).insts.iter().any(|&i| {
            !transfer_mode
                && matches!(
                    f.inst(i).opcode,
                    Opcode::CleanupSave
                        | Opcode::CleanupLanding
                        | Opcode::CleanupRestore
                        | Opcode::CatchLanding
                        | Opcode::HandlerLanding
                        | Opcode::Invoke
                        | Opcode::NlxTransfer
                )
        })
    }) {
        return Err(EmitError::UnsupportedOp(0xFA));
    }

    let entry = f.entry();
    if std::env::var_os("EGCL_IR_FULL").is_some() {
        eprintln!(
            "[ir] ===== function {} (entry b{}) =====",
            f.name(),
            entry.index()
        );
        for &b in f.block_order() {
            let bd = f.block(b);
            let params: Vec<String> = bd.params.iter().map(|v| format!("v{}", v.0)).collect();
            eprintln!("[ir] b{}({}):", b.index(), params.join(", "));
            for &inst in &bd.insts {
                let d = f.inst(inst);
                let res: Vec<String> = d.results.iter().map(|v| format!("v{}", v.0)).collect();
                let args: Vec<String> = d.args.iter().map(|v| format!("v{}", v.0)).collect();
                let lhs = if res.is_empty() {
                    String::new()
                } else {
                    format!("{} <- ", res.join(","))
                };
                let fs = d.frame_state.map(|id| {
                    f.frame_states
                        .get(id)
                        .scopes
                        .last()
                        .map(|s| s.bcp)
                        .unwrap_or(u32::MAX)
                });
                let fstr = fs.map(|b| format!("  fs@bcp={b}")).unwrap_or_default();
                eprintln!("[ir]   {}{:?}({}){}", lhs, d.opcode, args.join(","), fstr);
                for tc in &d.targets {
                    let ta: Vec<String> = tc.args.iter().map(|v| format!("v{}", v.0)).collect();
                    eprintln!("[ir]     -> b{}({})", tc.block.index(), ta.join(", "));
                }
            }
        }
    }
    // Does the function make a call? If so, its live values must survive the call,
    // so they go in callee-saved registers behind a small pushed frame.
    // "Makes a call" for register-allocation purposes: a plain Call, plus the
    // global-access ops which lower to c2i_load_global / c2i_store_global calls
    // (bliss-mzp). All three clobber caller-saved registers, so a function
    // containing any of them keeps its live values in callee-saved registers.
    let is_call_like = |op: Opcode| {
        matches!(
            op,
            Opcode::Call
                | Opcode::Invoke
                | Opcode::SymbolValue
                | Opcode::SymbolFunction
                | Opcode::SetSymbolValue
                | Opcode::EnvironmentValue
                | Opcode::SetEnvironmentValue
                | Opcode::ClearMv
                | Opcode::TakeValuesToLocals
                | Opcode::CleanupSave
                | Opcode::CleanupRestore
                | Opcode::CatchLanding
                | Opcode::HandlerLanding
        )
    };
    let has_ir_calls = f.block_order().iter().any(|&b| {
        f.block(b)
            .insts
            .iter()
            .any(|&i| is_call_like(f.inst(i).opcode))
    });

    let has_inlined_scopes = f.block_order().iter().any(|&b| {
        f.block(b).insts.iter().any(|&inst| {
            f.inst(inst)
                .frame_state
                .is_some_and(|id| f.frame_states.get(id).scopes.len() > 1)
        })
    });
    // Native poll and deopt veneers are real Rust calls. They can be inserted into
    // an otherwise call-free loop, so it must participate in both ABI stack
    // alignment and callee-saved allocation.  Omitting it leaves a no-call
    // frame with rsp % 16 == 8 at the generated call site; the veneer then
    // enters Rust misaligned and can corrupt unrelated runtime state.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let has_segment_calls = transfers
        .as_ref()
        .is_some_and(|transfer| transfer.poll_veneer.is_some() || transfer.deopt_veneer.is_some());
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let has_segment_calls = false;
    // A multi-scope guard calls the reconstruction callback even if the fast
    // path has no ordinary call. Give it the call-capable prologue/register set
    // so the callback is ABI-aligned and every live value survives the call.
    // Checked OSR entries call the precise reconstruction callback on failure,
    // even when the ordinary body contains no calls or guards.
    let has_checked_osr = f.osr_entries.iter().any(|osr| !osr.checks.is_empty());
    let has_calls = has_ir_calls || has_inlined_scopes || has_segment_calls || has_checked_osr
        || code_id != 0;

    // Precise state-transfer deopt (bliss-mba): a function that CALLS other code
    // can commit a visible side effect before a later guard fails, so re-running
    // it whole (the plain `c2i_deopt` path) would double that effect. For such
    // functions each guard instead reconstructs the interpreter frame at its own
    // bytecode position and resumes T0 there (like T1). We only need this where a
    // side effect can precede a guard — i.e. `has_calls` (a no-call function is
    // pure here: its only side-effect op, StoreGlobal, isn't emittable yet, so it
    // declines to T1 anyway — and re-running a pure function is observably safe).
    let precise = has_calls;
    // The SSA values named by some guard's FrameState — the *deopt-live* set. In
    // precise mode these must be reconstructable at their guard, so they may not
    // be freed early (liveness is extended to the guard below) nor overwritten by
    // an in-place result reuse (which a later overflow `jo` would deopt through).
    let mut deopt_live: HashSet<Value> = HashSet::new();
    if precise {
        for &b in f.block_order() {
            for &inst in &f.block(b).insts {
                if let Some(fsid) = f.inst(inst).frame_state {
                    let fs = f.frame_states.get(fsid);
                    for scope in &fs.scopes {
                        for src in scope.locals.iter().chain(scope.stack.iter()) {
                            if let ValueSource::Value { value, .. } = src {
                                deopt_live.insert(*value);
                            }
                        }
                    }
                }
            }
        }
    }
    // OSR entry is another simultaneous use point. Values reconstructed from
    // the live lower-tier frame must not have their registers recycled merely
    // because ordinary entry-flow liveness considers their definitions local.
    for osr in &f.osr_entries {
        let fs = f.frame_states.get(osr.frame_state);
        for scope in &fs.scopes {
            for src in scope.locals.iter().chain(scope.stack.iter()) {
                if let ValueSource::Value { value, .. } = src {
                    deopt_live.insert(*value);
                }
            }
        }
    }
    // Emit the entry block first (the prologue falls into it).
    let mut blocks: Vec<Block> = f.block_order().to_vec();
    if blocks.first() != Some(&entry) {
        if let Some(pos) = blocks.iter().position(|&b| b == entry) {
            blocks.remove(pos);
            blocks.insert(0, entry);
        }
    }

    // Record every constant. `consts` (raw fixnum) drives immediate folding,
    // `float_consts` (f32 bits) the XMM path, `const_tagged` (the tagged
    // EgclVal) the general materialisation of non-moving constants, and
    // `heap_const_slots` the stable, GC-rewritten slots for moving constants.
    let mut consts: HashMap<Value, i64> = HashMap::new();
    let mut float_consts: HashMap<Value, u32> = HashMap::new();
    let mut const_tagged: HashMap<Value, u64> = HashMap::new();
    let mut heap_const_slots: HashMap<Value, usize> = HashMap::new();
    for &b in &blocks {
        for &inst in &f.block(b).insts {
            let d = f.inst(inst);
            if d.results.is_empty() {
                continue;
            }
            let r = d.results[0];
            match (d.opcode, &d.aux) {
                (Opcode::ConstFixnum, AuxData::FixnumImm(v)) => {
                    consts.insert(r, *v);
                    const_tagged.insert(r, egcl_rt::value::EgclVal::from_fixnum(*v).0);
                }
                (Opcode::ConstFloat, AuxData::FloatImm(v)) => {
                    float_consts.insert(r, v.to_bits());
                    const_tagged.insert(r, ((v.to_bits() as u64) << 32) | 0b100);
                }
                (Opcode::ConstChar, AuxData::CharImm(c)) => {
                    const_tagged.insert(r, egcl_rt::value::EgclVal::from_char(*c).0);
                }
                (Opcode::ConstSymbol, AuxData::SymbolRef(idx)) => {
                    const_tagged.insert(r, egcl_rt::value::EgclVal::from_symbol_index(*idx).0);
                }
                (Opcode::ConstHeapObj, AuxData::HeapLiteral { slot }) => {
                    heap_const_slots.insert(r, *slot);
                }
                (Opcode::ConstNil, _) => {
                    const_tagged.insert(r, egcl_rt::value::NIL.0);
                }
                (Opcode::ConstT, _) => {
                    const_tagged.insert(r, egcl_rt::value::T.0);
                }
                _ => {}
            }
        }
    }
    // Use counts: to decide which comparisons can be fused into a branch. The
    // terminator is itself in `block.insts`, so its own `args` are already counted
    // by the inst loop — only the edge (BlockCall) arguments are counted here.
    let mut uses: HashMap<Value, u32> = HashMap::new();
    for &b in &blocks {
        for &inst in &f.block(b).insts {
            for &v in &f.inst(inst).args {
                *uses.entry(v).or_default() += 1;
            }
        }
        if let Some(t) = f.terminator(b) {
            for tc in &f.inst(t).targets {
                for &v in &tc.args {
                    *uses.entry(v).or_default() += 1;
                }
            }
        }
    }
    for osr in &f.osr_entries {
        let fs = f.frame_states.get(osr.frame_state);
        for scope in &fs.scopes {
            for src in scope.locals.iter().chain(scope.stack.iter()) {
                if let ValueSource::Value { value, .. } = src {
                    *uses.entry(*value).or_default() += 1;
                }
            }
        }
    }

    // Fused comparisons: a comparison whose single use is its own block's Brif
    // condition is emitted as cmp+jcc and needs no register — a fixnum comparison
    // (FixnumCmp*) or an EQ/NULL identity compare (GenericEq), which fuses to
    // cmp+je and skips materialising a T/NIL just to re-test it.
    let mut fused: HashSet<crate::t2::ir::Inst> = HashSet::new();
    for &b in &blocks {
        if let Some(t) = f.terminator(b) {
            let td = f.inst(t);
            if td.opcode == Opcode::Brif {
                let cond = td.args[0];
                if uses.get(&cond) == Some(&1) {
                    if let ValueDef::Result { inst, .. } = f.value(cond).def {
                        let op = f.inst(inst).opcode;
                        if ((code_id == 0 && fixnum_cmp_cc(op).is_some()) || op == Opcode::GenericEq)
                            && f.block(b).insts.contains(&inst)
                        {
                            fused.insert(inst);
                        }
                    }
                }
            }
        }
    }

    // Guard elimination: every non-terminator inst in the entry block runs
    // unconditionally before the branch, so any fixnum operand it guards is proven
    // a fixnum in every (entry-dominated) block. Those operands need no re-guard.
    // (Conservative: only the entry block, which dominates all others.)
    let mut entry_guarded: HashSet<Value> = HashSet::new();
    for &inst in &f.block(entry).insts {
        let d = f.inst(inst);
        if d.flags.guard && is_fixnum_guarding_op(d.opcode) {
            for &v in &d.args {
                if !const_tagged.contains_key(&v) {
                    entry_guarded.insert(v);
                }
            }
        }
    }
    // Parameter declarations are checked by the shared bytecode/native entry
    // boundary before this code runs. Seed the emitter's proof sets from their
    // entry SSA types so typed operations do not emit a second, redundant tag
    // guard. Declared functions do not expose the unchecked register-entry ABI
    // below, keeping this assumption true for recursive compiled calls too.
    let type_is = |value: Value, bits: TypeBits| {
        let actual = f.value(value).ty.bits;
        !actual.is_bottom() && actual.meet(bits) == actual
    };
    let declared_fixnums: HashSet<Value> = f
        .block(entry)
        .params
        .iter()
        .copied()
        .filter(|&value| f.is_entry_param_checked(value) && type_is(value, TypeBits::FIXNUM))
        .collect();
    let declared_single_floats: HashSet<Value> = f
        .block(entry)
        .params
        .iter()
        .copied()
        .filter(|&value| f.is_entry_param_checked(value) && type_is(value, TypeBits::SINGLE_FLOAT))
        .collect();
    let has_declared_params = !declared_fixnums.is_empty() || !declared_single_floats.is_empty();
    // The result of a fixnum-producing op is a fixnum wherever it is used (SSA: the
    // def dominates every use), so those uses never need a re-guard. This removes
    // the redundant intermediate guards in a chain of fixnum/bitwise ops — the bulk
    // of the guard overhead in crypto/hashing kernels.
    let mut fixnum_valued: HashSet<Value> = HashSet::new();
    for &b in &blocks {
        for &inst in &f.block(b).insts {
            let d = f.inst(inst);
            if is_fixnum_producing_op(d.opcode) {
                for &r in &d.results {
                    fixnum_valued.insert(r);
                }
            }
            // A declared FIXNUM result is a proof by construction (a guard's
            // narrowed value, a speculated site's result): its def dominates
            // every use, so no use needs a re-guard.
            for &r in &d.results {
                if type_is(r, TypeBits::FIXNUM) {
                    fixnum_valued.insert(r);
                }
            }
        }
        // A block parameter is refined to FIXNUM only when every edge into it
        // carries a fixnum and any OSR entry checks the imported value
        // (opt_guard loop-entry splitting), so it is proven as well. Entry
        // parameters stay with `declared_fixnums` and its boundary check.
        if b != entry {
            for &p in &f.block(b).params {
                if type_is(p, TypeBits::FIXNUM) {
                    fixnum_valued.insert(p);
                }
            }
        }
    }
    // Lightweight value ranges, enough to prove some left shifts can't overflow (so
    // they become a plain `shl` instead of an overflow-checked multiply): a
    // constant is exact, and `(logand x c)` with a non-negative mask c bounds the
    // result to [0, c] — the common "mask to word size" idiom. Processed in program
    // order so a def's range is available at its uses.
    let mut val_range: HashMap<Value, (i64, i64)> = HashMap::new();
    for (&v, &c) in &consts {
        val_range.insert(v, (c, c));
    }
    for &b in &blocks {
        for &inst in &f.block(b).insts {
            let d = f.inst(inst);
            if d.opcode == Opcode::LogAnd && d.results.len() == 1 && d.args.len() == 2 {
                let mask = consts.get(&d.args[0]).or_else(|| consts.get(&d.args[1]));
                if let Some(&c) = mask {
                    if c >= 0 {
                        val_range.insert(d.results[0], (0, c));
                    }
                }
            }
        }
    }

    // Production allocation: SSA -> MachFunc -> regalloc2. A value with one
    // location for its entire reported live range remains register-resident;
    // any split range gets a stable spill home consumed by the framed templates.
    let mut machine = crate::t2::lower::lower(f);
    crate::t2::regalloc::allocate_framed(&mut machine)
        .map_err(|error| EmitError::RegAlloc(format!("{error:?}")))?;
    if !machine.insts.is_empty() && machine.inst_allocations.len() != machine.insts.len() {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    let layout = select_frame_homes(f, &machine, |value| {
        const_tagged.contains_key(&value)
            || fused.iter().any(|&i| f.inst(i).results.contains(&value))
    })
    .map_err(|_| EmitError::UnsupportedOp(0xFA))?;
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let mut transfer_maps = if transfer_mode {
        crate::t2::transfer_map::lower_framed_transfer_maps(f, &machine, &layout, &const_tagged)
            .map_err(|_| EmitError::UnsupportedOp(0xFD))?
    } else {
        vec![]
    };
    let homes = layout.values;
    let mut next_stack = layout.stack_slots;
    // `EGCL_RA_DBG` dump of the three facts a deopt-clobber bug is diagnosed
    // from (bliss-x9c9): each value's stable home, which MachInsts carry a
    // FrameState (and therefore contribute `deopt_uses` liveness), and what
    // each FrameState names. A deopt reading a value whose home is a register
    // the carrying instruction writes is the bug signature.
    if std::env::var_os("EGCL_RA_DBG").is_some() {
        let mut sorted: Vec<_> = homes.iter().collect();
        sorted.sort_by_key(|(value, _)| value.0);
        for (value, home) in sorted {
            eprintln!(
                "[homes] {} v{} -> {home:?} ranges={:?}",
                f.name(),
                value.0,
                machine
                    .value_locations
                    .iter()
                    .filter(|range| range.vreg.class == RegClass::Gpr && range.vreg.num == value.0)
                    .map(|range| range.location)
                    .collect::<Vec<_>>()
            );
        }
        for (mi, inst) in machine.insts.iter().enumerate() {
            if inst.frame_state.is_none() {
                continue;
            }
            eprintln!(
                "[fsinst] {} mi={mi} op={} defs={:?} uses={:?} deopt={:?} fs={:?}",
                f.name(),
                inst.op,
                inst.defs.iter().map(|v| v.num).collect::<Vec<_>>(),
                inst.uses.iter().map(|v| v.num).collect::<Vec<_>>(),
                inst.deopt_uses.iter().map(|v| v.num).collect::<Vec<_>>(),
                inst.frame_state,
            );
        }
        for (id, fs) in f.frame_states.iter() {
            for scope in &fs.scopes {
                eprintln!(
                    "[fs] {} fs={id:?} bcp={} locals={:?} stack={:?}",
                    f.name(),
                    scope.bcp,
                    scope.locals,
                    scope.stack
                );
            }
        }
    }
    // Convert regalloc2's split-aware live ranges into exact tagged roots for
    // each runtime safepoint. The rich emitter uses stable `homes`, so these are
    // the locations that must be synchronized, not regalloc2's transient edit
    // locations. Only tagged values are GC roots; unboxed GPR values are omitted.
    // Proven-immediate root filtering (bliss-x5y.25 option b). A value whose
    // inferred type is confined to the NON-POINTER immediates — fixnum,
    // single-float, character, symbol (an interning INDEX, not a pointer), and
    // NIL/T — can never be relocated by the moving GC (`gc::is_heap_ref` is
    // false for all of them), so it needs no shadow-root sync at safepoints.
    // Soundness: the global inference map assigns each SSA value the type it
    // has FROM ITS DEFINITION on (guards define fresh narrowed values), so the
    // fact holds wherever the value is live. BOTTOM (unreached) stays rooted.
    // Fewer roots means fewer sync sites, and a function whose roots vanish
    // entirely loses its frame_base_home — unlocking the direct self-call
    // fast path in emit_call.
    let inference = crate::t2::infer::infer(f);
    let proven_immediate = |value: Value| -> bool {
        const IMMEDIATE: TypeBits = TypeBits(
            TypeBits::FIXNUM.0
                | TypeBits::SINGLE_FLOAT.0
                | TypeBits::CHARACTER.0
                | TypeBits::SYMBOL.0
                | TypeBits::NULL.0,
        );
        let bits = inference.ty(value).bits;
        !bits.is_bottom() && bits.meet(IMMEDIATE) == bits
    };
    let mut safepoint_roots: HashMap<Inst, Vec<Value>> = HashMap::new();
    if activation_slots.is_some() {
        for (mi, machine_inst) in machine.insts.iter().enumerate() {
            if !machine_inst.safepoint {
                continue;
            }
            let mi_u32 = u32::try_from(mi).map_err(|_| EmitError::UnsupportedOp(0xFD))?;
            let source = machine_inst
                .source_inst
                .ok_or(EmitError::UnsupportedOp(0xFD))?;
            // `value_locations` ranges are in regalloc2 ProgPoint units
            // (`ProgPoint::to_index()` == inst*2 + pos; see regalloc.rs), NOT raw
            // MachInst indices. A value must be a GC root here if it is live
            // ACROSS this safepoint call — i.e. live at the point just after the
            // call (its next use is later), which is program point `mi*2 + 1`.
            // Comparing the ProgPoint ranges against the bare inst index `mi` (as
            // the old code did) is a ~2x scale mismatch that silently dropped
            // every live-through value, leaving only call *args* (rooted via the
            // uses loop below) in the map. A moving GC during the call then left a
            // live-through Tagged value — e.g. a loop-carried list held in a
            // callee-saved register — stale, deopting to a corrupt frame
            // (bliss-r8pt). Over-including a root is safe (its stable home holds a
            // valid pointer); under-including corrupts the heap.
            let pp_after = mi_u32 * 2 + 1;
            let mut live = HashSet::new();
            for range in &machine.value_locations {
                if range.vreg.class != RegClass::Gpr
                    || range.start > pp_after
                    || pp_after >= range.end
                {
                    continue;
                }
                let value = Value(range.vreg.num);
                // A call result's range starts at the safepoint, but the value
                // does not exist until the call returns. Restoring a pre-call
                // shadow for it would overwrite the real result.
                if machine_inst.defs.contains(&range.vreg) {
                    continue;
                }
                if f.value(value).repr == ValueRepresentation::Tagged
                    && homes.contains_key(&value)
                    && !proven_immediate(value)
                {
                    live.insert(value);
                }
            }
            // Early uses can end at the safepoint itself. Include them
            // explicitly in case the allocator's half-open debug range ends at
            // this instruction boundary.
            for vreg in &machine_inst.uses {
                if vreg.class == RegClass::Gpr {
                    let value = Value(vreg.num);
                    if f.value(value).repr == ValueRepresentation::Tagged
                        && homes.contains_key(&value)
                        && !proven_immediate(value)
                    {
                        live.insert(value);
                    }
                }
            }
            let mut live: Vec<_> = live.into_iter().collect();
            live.sort_by_key(|value| value.0);
            if std::env::var_os("EGCL_SAFEPOINT_DBG").is_some() {
                let src = machine_inst.source_inst;
                let bcp = src.and_then(|i| f.inst(i).frame_state).map(|id| {
                    f.frame_states
                        .get(id)
                        .scopes
                        .last()
                        .map(|s| s.bcp)
                        .unwrap_or(u32::MAX)
                });
                eprintln!(
                    "[sp] fn={} mi={} src={:?} bcp={:?} roots={:?}",
                    f.name(),
                    mi,
                    src,
                    bcp,
                    live.iter().map(|v| v.0).collect::<Vec<_>>()
                );
            }
            safepoint_roots.insert(source, live);
        }
    }
    if std::env::var_os("EGCL_SAFEPOINT_DBG").is_some() {
        for range in &machine.value_locations {
            if range.vreg.class == RegClass::Gpr {
                eprintln!(
                    "[sp-range] fn={} v{} [{}, {}) loc={:?}",
                    f.name(),
                    range.vreg.num,
                    range.start,
                    range.end,
                    range.location
                );
            }
        }
    }
    // Native segment polling is placed at loop headers rather than after every
    // branch. This keeps condition flags intact and guarantees that every
    // cycle reaches a poll. Header roots use the same split-aware ranges as
    // runtime-call roots, but are evaluated at the machine block's first
    // program point so a moving GC can update the homes before the body runs.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let (backedge_headers, loop_roots): (
        HashSet<crate::t2::ir::Block>,
        HashMap<crate::t2::ir::Block, Vec<Value>>,
    ) = if transfers
        .as_ref()
        .and_then(|transfer| transfer.poll_veneer)
        .is_some()
    {
        let mut successors: HashMap<_, Vec<_>> = HashMap::new();
        for &block in &blocks {
            successors.insert(
                block,
                f.terminator(block)
                    .map(|terminator| {
                        f.inst(terminator)
                            .targets
                            .iter()
                            .map(|target| target.block)
                            .collect()
                    })
                    .unwrap_or_default(),
            );
        }
        let reaches = |start, goal| {
            let mut pending = vec![start];
            let mut seen = HashSet::new();
            while let Some(block) = pending.pop() {
                if block == goal {
                    return true;
                }
                if !seen.insert(block) {
                    continue;
                }
                if let Some(next) = successors.get(&block) {
                    pending.extend(next.iter().copied());
                }
            }
            false
        };
        let mut headers = HashSet::new();
        for &block in &blocks {
            let Some(terminator) = f.terminator(block) else {
                continue;
            };
            for target in &f.inst(terminator).targets {
                if reaches(target.block, block) {
                    headers.insert(target.block);
                }
            }
        }
        let mut roots = HashMap::new();
        for &header in &headers {
            let Some(block_index) = blocks.iter().position(|&block| block == header) else {
                return Err(EmitError::UnsupportedOp(0xFE));
            };
            let pp = u32::try_from(machine.blocks[block_index].start)
                .map_err(|_| EmitError::UnsupportedOp(0xFE))?
                * 2;
            // Polls are inserted after allocation, so its call-clobber model
            // cannot preserve even immediate values for us. Save all live
            // tagged homes; the GC safely ignores non-pointer slot contents.
            let mut live = HashSet::new();
            for range in &machine.value_locations {
                if range.vreg.class != RegClass::Gpr || range.start > pp || pp >= range.end {
                    continue;
                }
                let value = Value(range.vreg.num);
                if f.value(value).repr == ValueRepresentation::Tagged && homes.contains_key(&value)
                {
                    live.insert(value);
                }
            }
            let mut live: Vec<_> = live.into_iter().collect();
            live.sort_by_key(|value| value.0);
            roots.insert(header, live);
        }
        (headers, roots)
    } else {
        (HashSet::new(), HashMap::new())
    };
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let loop_roots: HashMap<crate::t2::ir::Block, Vec<Value>> = HashMap::new();
    // Keep long straight-line native segments interruptible as well. Polls are
    // attached to source instructions (before their first machine instruction)
    // so they never disturb a branch's condition flags. Their roots are the
    // values live at that exact machine program point, not a guessed block-wide
    // set; an incomplete map rejects this native artifact before installation.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let (straight_poll_sources, straight_poll_roots): (
        HashSet<Inst>,
        HashMap<Inst, Vec<Value>>,
    ) = if transfers
        .as_ref()
        .and_then(|transfer| transfer.poll_veneer)
        .is_some()
    {
        const POLL_INTERVAL: usize = 64;
        let mut sources = HashSet::new();
        let mut count = 0usize;
        for &block in &blocks {
            for &inst in &f.block(block).insts {
                let opcode = f.inst(inst).opcode;
                if opcode.is_terminator()
                    || opcode == Opcode::CleanupLanding
                    || is_const_opcode(opcode)
                {
                    continue;
                }
                count = count.saturating_add(1);
                if count % POLL_INTERVAL == 0 {
                    sources.insert(inst);
                }
            }
        }
        let mut roots = HashMap::new();
        for (mi, machine_inst) in machine.insts.iter().enumerate() {
            let Some(source) = machine_inst.source_inst else {
                continue;
            };
            if !sources.contains(&source) || roots.contains_key(&source) {
                continue;
            }
            let pp = u32::try_from(mi).map_err(|_| EmitError::UnsupportedOp(0xFE))? * 2;
            let mut live = HashSet::new();
            for range in &machine.value_locations {
                if range.vreg.class != RegClass::Gpr || range.start > pp || pp >= range.end {
                    continue;
                }
                let value = Value(range.vreg.num);
                if machine_inst.defs.contains(&range.vreg) {
                    continue;
                }
                if f.value(value).repr == ValueRepresentation::Tagged && homes.contains_key(&value)
                {
                    live.insert(value);
                }
            }
            let mut live: Vec<_> = live.into_iter().collect();
            live.sort_by_key(|value| value.0);
            roots.insert(source, live);
        }
        for source in &sources {
            if !roots.contains_key(source) {
                return Err(EmitError::UnsupportedOp(0xFE));
            }
        }
        (sources, roots)
    } else {
        (HashSet::new(), HashMap::new())
    };
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let straight_poll_roots: HashMap<crate::t2::ir::Inst, Vec<Value>> = HashMap::new();
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let _straight_poll_sources: HashSet<crate::t2::ir::Inst> = HashSet::new();
    let root_shadow_slots = safepoint_roots
        .values()
        .map(Vec::len)
        .chain(loop_roots.values().map(Vec::len))
        .chain(straight_poll_roots.values().map(Vec::len))
        .max()
        .unwrap_or(0);
    let root_shadow_slots =
        u16::try_from(root_shadow_slots).map_err(|_| EmitError::UnsupportedOp(0xFD))?;
    // Wide c2i calls pass a contiguous slice. Reserve that slice inside the
    // owning EgclStack activation so the precise GC scans and rewrites every
    // argument while the Rust adapter/callee runs.
    let call_arg_slots = f
        .block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter().copied())
        .map(|inst| f.inst(inst))
        .filter(|data| {
            (data.opcode == Opcode::Call && data.args.len() > 3) || data.opcode == Opcode::Invoke
        })
        .map(|data| data.args.len())
        .max()
        .unwrap_or(0);
    let call_arg_slots =
        u16::try_from(call_arg_slots).map_err(|_| EmitError::UnsupportedOp(0xFD))?;
    let shadow_root_slots = root_shadow_slots
        .checked_add(call_arg_slots)
        .ok_or(EmitError::UnsupportedOp(0xFD))?;
    let needs_activation_frame = transfer_mode
        || f.block_order().iter().any(|&block| {
            f.block(block)
                .insts
                .iter()
                .any(|&inst| f.inst(inst).opcode == Opcode::TakeValuesToLocals)
        });
    if needs_activation_frame && activation_slots.is_none() {
        return Err(EmitError::UnsupportedOp(op_tag(Opcode::TakeValuesToLocals)));
    }
    if call_arg_slots != 0 && activation_slots.is_none() {
        return Err(EmitError::UnsupportedOp(0xF9));
    }
    let frame_base_home = if shadow_root_slots == 0 && !needs_activation_frame {
        None
    } else {
        let home = FramedHome::Stack(next_stack);
        next_stack += 1;
        Some(home)
    };
    // Native callees resume their own deoptimization, including register-entry
    // recursion. Record whether this activation owns an EgclStack frame.
    let entry_kind_home = (code_id != 0).then(|| {
        let home = FramedHome::Stack(next_stack);
        next_stack += 1;
        home
    });
    let native_spill_slots = next_stack;
    let regalloc_spill_slots = machine.num_spill_slots;
    let allocation_edits = machine.allocation_edits.len();
    let spill_bytes = ((native_spill_slots as usize * 8) + 15) & !15;
    let reg: HashMap<Value, u8> = homes
        .iter()
        .filter_map(|(&value, &home)| match home {
            FramedHome::Reg(r) => Some((value, r)),
            FramedHome::Stack(_) => None,
        })
        .collect();

    let mut a = Asm::new();
    let deopt = a.label();
    let arg_regs = [1u8, 8, 9, 10]; // rcx, r8, r9, r10
                                    // Variadic and activation-backed functions require the frame entry.
    let has_reg_entry = !has_declared_params
        && !f.is_variadic()
        && frame_base_home.is_none()
        && f.block(entry).params.len() <= arg_regs.len();
    // Even pure self-recursion can enter c2i at the stack limit or deoptimize.
    // Every native caller must propagate a transfer raised by that fallback.
    let transfer_check = (c2i_transfer_pending_addr != 0).then(|| NativeTransferCheck {
        pending_addr: c2i_transfer_pending_addr,
        exit: a.label(),
    });
    // Precise deopt (bliss-mba): each guarding instruction gets its own deopt stub
    // that reconstructs the interpreter frame at that guard's bytecode position.
    // Every instruction that reaches `emit_arith_inst` (the only guard emitter)
    // MUST carry a FrameState, or we cannot resume precisely — decline to T1. In
    // practice speculation always keeps the FrameState on the ops it types, so
    // this only rejects exotic shapes.
    let mut inst_deopt: HashMap<Inst, egcl_rt::asm::Label> = HashMap::new();
    if precise {
        for &b in &blocks {
            for &inst in &f.block(b).insts {
                let d = f.inst(inst);
                if is_const_opcode(d.opcode)
                    || d.opcode.is_terminator()
                    || fused.contains(&inst)
                    || matches!(
                        d.opcode,
                        Opcode::Call
                            | Opcode::GenericEq
                            | Opcode::TypeCheck
                            | Opcode::Car
                            | Opcode::Cdr
                            | Opcode::SymbolValue
                            | Opcode::SymbolFunction
                            | Opcode::SetSymbolValue
                            | Opcode::EnvironmentValue
                            | Opcode::SetEnvironmentValue
                            | Opcode::ClearMv
                            | Opcode::TakeValuesToLocals
                            | Opcode::MemoryFence
                    )
                {
                    continue;
                }
                // Reaches emit_arith_inst → may emit a guard → needs a FrameState.
                if d.frame_state.is_none() {
                    return Err(EmitError::UnsupportedOp(op_tag(d.opcode)));
                }
                inst_deopt.insert(inst, a.label());
            }
        }
    }
    let mut block_label: HashMap<Block, egcl_rt::asm::Label> = HashMap::new();
    for &b in &blocks {
        block_label.insert(b, a.label());
    }

    // Callee-saved registers used by allocated homes, and whether an
    // 8-byte pad is needed to keep rsp 16-aligned before a call (entry rsp%16==8,
    // each push subtracts 8, so an even push count needs one pad).
    let saved: Vec<u8> = {
        let mut s: Vec<u8> = reg
            .values()
            .copied()
            .filter(|r| CALLEE_SAVED_X86.contains(r))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        s.sort_unstable();
        s
    };
    #[cfg(not(all(target_arch = "x86_64", windows)))]
    let pad = has_calls && saved.len() % 2 == 0;
    #[cfg(all(target_arch = "x86_64", windows))]
    let windows_frame = WindowsFrame::new(&saved, spill_bytes)?;
    let emit_prologue = |a: &mut Asm| {
        #[cfg(all(target_arch = "x86_64", windows))]
        {
            windows_frame.prologue(a)
        }
        #[cfg(not(all(target_arch = "x86_64", windows)))]
        {
            for &r in &saved {
                push_reg(a, r);
            }
            if pad {
                a.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]);
            }
            if spill_bytes != 0 {
                a.extend_from_slice(&[0x48, 0x81, 0xEC]);
                a.extend_from_slice(&(spill_bytes as i32).to_le_bytes());
            }
            Vec::<u8>::new()
        }
    };
    let emit_epilogue = |a: &mut Asm| {
        #[cfg(all(target_arch = "x86_64", windows))]
        windows_frame.epilogue(a);
        #[cfg(not(all(target_arch = "x86_64", windows)))]
        {
            if spill_bytes != 0 {
                a.extend_from_slice(&[0x48, 0x81, 0xC4]);
                a.extend_from_slice(&(spill_bytes as i32).to_le_bytes());
            }
            if pad {
                a.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
            }
            for &r in saved.iter().rev() {
                pop_reg(a, r);
            }
        }
    };

    // A call function with ≤4 params also gets a REGISTER entry, so a self-call can
    // enter directly (args in registers) instead of paying c2i dispatch.
    let reg_entry_label = a.label();

    // Interpreter entry (offset 0): with calls, push the callee-saved value
    // registers and pad; then load entry params from the frame slots into their
    // value registers and fall (or jump) into the entry block.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let frame_entry_label = a.label();
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    a.bind(frame_entry_label);
    let _unwind_info = emit_prologue(&mut a);
    if let Some(home) = entry_kind_home {
        mov_imm64(&mut a, RAX, code_id as i64);
        store_home(&mut a, home, RAX, 0);
    }
    if cfg!(windows) {
        mov_rr(&mut a, 7, 1); // Win64 entry RCX -> preserved frame base RDI
    }
    if let Some(home) = frame_base_home {
        store_home(&mut a, home, 7 /* rdi */, 0);
    }
    for (i, &p) in f.block(entry).params.iter().enumerate() {
        let home = *homes.get(&p).ok_or(EmitError::UnsupportedOp(0xF2))?;
        match home {
            FramedHome::Reg(r) => mov_from_frame(&mut a, r, 7 /* rdi */, i),
            FramedHome::Stack(_) => {
                mov_from_frame(&mut a, RAX, 7 /* rdi */, i);
                store_home(&mut a, home, RAX, 0);
            }
        }
    }
    let compiled_entry;
    if has_reg_entry {
        jump_to_shared_body(&mut a, block_label[&entry]); // skip register entry
        a.bind(reg_entry_label);
        compiled_entry = a.here();
        // Register entry: same prologue, but args arrive in the arg registers and
        // move into the (callee-saved) value registers; then fall into the body.
        emit_prologue(&mut a);
        if let Some(home) = entry_kind_home {
            mov_imm64(&mut a, RAX, (code_id | (1 << 63)) as i64);
            store_home(&mut a, home, RAX, 0);
        }
        let entry_moves = f
            .block(entry)
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                Ok((
                    *homes.get(p).ok_or(EmitError::UnsupportedOp(0xF2))?,
                    HomeMoveSrc::Home(FramedHome::Reg(arg_regs[i])),
                ))
            })
            .collect::<Result<Vec<_>, EmitError>>()?;
        // Allocated homes can overlap any unread incoming argument. Resolve
        // these as one simultaneous move, including register cycles/spills.
        parallel_home_move(&mut a, &entry_moves);
    } else {
        // Frameless: the body starts here with args already in their value regs.
        compiled_entry = if has_calls || cfg!(windows) {
            0
        } else {
            a.here()
        };
    }

    let mut root_sync_sites = Vec::new();
    let mut emitted_safepoints = 0usize;
    // Bytecode→native correlation for the tiered-JIT viewer (bliss-zmmb): the T2
    // optimizer has no 1:1 bcp mapping, but every instruction carrying a frame
    // state knows the bytecode position it deoptimizes to. Collect (native
    // offset, bcp) at those points; post-processed into a per-bcp map below.
    let mut bcp_sites: Vec<(u32, u32)> = Vec::new();

    // Emit each block: bind its label, emit its instructions, then its terminator
    // (with block-parameter moves on each out-edge).
    for (bi, &b) in blocks.iter().enumerate() {
        a.bind(block_label[&b]);
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        if backedge_headers.contains(&b) {
            let poll_veneer = transfers
                .as_ref()
                .and_then(|transfer| transfer.poll_veneer)
                .ok_or(EmitError::UnsupportedOp(0xFE))?;
            let frame_base = frame_base_home.ok_or(EmitError::UnsupportedOp(0xFE))?;
            let roots = loop_roots.get(&b).map(Vec::as_slice).unwrap_or(&[]);
            emit_shadow_root_sync(
                &mut a,
                roots,
                &homes,
                frame_base,
                activation_slots.ok_or(EmitError::UnsupportedOp(0xFE))?,
                shadow_root_slots,
            )?;
            // The poll helper ignores the request payload. Passing the rooted
            // activation pointer keeps the veneer ABI uniform and gives the
            // capture stub a live, stable request word if it takes the cold
            // transfer path.
            load_home(&mut a, 7, frame_base, 0);
            mov_imm64(&mut a, RAX, poll_veneer as i64);
            emit_runtime_helper_call(&mut a, 0, None);
            let restore_roots: HashSet<_> = roots.iter().copied().collect();
            emit_shadow_root_restore(
                &mut a,
                roots,
                &restore_roots,
                &homes,
                frame_base,
                activation_slots.ok_or(EmitError::UnsupportedOp(0xFE))?,
            )?;
        }
        // Fixnum operands proven by a dominating guard: the entry block dominates
        // all others, so its guards carry over (the entry block itself starts fresh).
        let mut proven: HashSet<Value> = if b == entry {
            HashSet::new()
        } else {
            entry_guarded.clone()
        };
        proven.extend(declared_fixnums.iter().copied());
        proven.extend(fixnum_valued.iter().copied());
        for &inst in &f.block(b).insts {
            let d = f.inst(inst).clone();
            #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
            if straight_poll_sources.contains(&inst) {
                let poll_veneer = transfers
                    .as_ref()
                    .and_then(|transfer| transfer.poll_veneer)
                    .ok_or(EmitError::UnsupportedOp(0xFE))?;
                let frame_base = frame_base_home.ok_or(EmitError::UnsupportedOp(0xFE))?;
                let roots = straight_poll_roots
                    .get(&inst)
                    .map(Vec::as_slice)
                    .ok_or(EmitError::UnsupportedOp(0xFE))?;
                emit_shadow_root_sync(
                    &mut a,
                    roots,
                    &homes,
                    frame_base,
                    activation_slots.ok_or(EmitError::UnsupportedOp(0xFE))?,
                    shadow_root_slots,
                )?;
                load_home(&mut a, 7, frame_base, 0);
                mov_imm64(&mut a, RAX, poll_veneer as i64);
                emit_runtime_helper_call(&mut a, 0, None);
                let restore_roots: HashSet<_> = roots.iter().copied().collect();
                emit_shadow_root_restore(
                    &mut a,
                    roots,
                    &restore_roots,
                    &homes,
                    frame_base,
                    activation_slots.ok_or(EmitError::UnsupportedOp(0xFE))?,
                )?;
            }
            if (is_const_opcode(d.opcode) && d.opcode != Opcode::ConstHeapObj)
                || d.opcode == Opcode::CleanupLanding
                || (d.opcode.is_terminator() && d.opcode != Opcode::Invoke)
                || fused.contains(&inst)
            {
                continue;
            }
            // Catch delivery consumes a rooted payload without allocating,
            // collecting or yielding. It is a register-clobbering call, but
            // does not have (or need) a safepoint synchronization map.
            let roots = if activation_slots.is_some()
                && is_call_like(d.opcode)
                && !matches!(d.opcode, Opcode::CatchLanding | Opcode::HandlerLanding)
            {
                emitted_safepoints += 1;
                Some(
                    safepoint_roots
                        .get(&inst)
                        .ok_or(EmitError::UnsupportedOp(0xFD))?,
                )
            } else {
                None
            };
            // Dying call arguments may share a stable home with the call's
            // result. They still need synchronization while the callee runs,
            // but restoring them afterward would overwrite that result. SSA
            // guarantees a genuinely live-across value cannot share a home
            // with a simultaneously live result, so home overlap is the exact
            // exclusion needed here.
            let result_homes: HashSet<_> = d
                .results
                .iter()
                .filter_map(|value| homes.get(value).copied())
                .collect();
            let restore_roots: Option<HashSet<Value>> = roots.map(|roots| {
                roots
                    .iter()
                    .copied()
                    .filter(|value| {
                        homes
                            .get(value)
                            .is_some_and(|home| !result_homes.contains(home))
                    })
                    .collect()
            });
            if let (Some(roots), Some(frame_base), Some(base_slots)) =
                (roots, frame_base_home, activation_slots)
            {
                emit_shadow_root_sync(
                    &mut a,
                    roots,
                    &homes,
                    frame_base,
                    base_slots,
                    shadow_root_slots,
                )?;
            }
            let sync_offset = a.here();
            // Record the bytecode position this instruction reconstructs, at its
            // native offset (bliss-zmmb).
            if let Some(fsid) = d.frame_state {
                if let Some(bcp) = f.frame_states.get(fsid).scopes.last().map(|s| s.bcp) {
                    if bcp != u32::MAX {
                        bcp_sites.push((sync_offset as u32, bcp));
                    }
                }
            }
            let wide_call = d.opcode == Opcode::Call && d.args.len() > 3;
            let (mut inst_reg, result_stores) = if d.opcode == Opcode::TakeValuesToLocals
                || d.opcode == Opcode::Invoke
                || matches!(
                    d.opcode,
                    Opcode::CleanupSave
                        | Opcode::CleanupRestore
                        | Opcode::CatchLanding
                        | Opcode::HandlerLanding
                )
                || wide_call
            {
                (HashMap::new(), Vec::new())
            } else {
                prepare_framed_inst(&mut a, &d, &homes, &const_tagged)?
            };
            let mut inst_pool = Vec::new();
            if d.opcode == Opcode::ConstHeapObj {
                let result = *d.results.first().ok_or(EmitError::UnsupportedOp(0xF2))?;
                let dst = *inst_reg
                    .get(&result)
                    .ok_or(EmitError::UnsupportedOp(0xF2))?;
                let slot = *heap_const_slots
                    .get(&result)
                    .ok_or(EmitError::UnsupportedOp(op_tag(Opcode::ConstHeapObj)))?;
                mov_imm64(&mut a, RAX, slot as i64);
                load_mem64_disp(&mut a, dst, RAX, 0);
            } else if d.opcode == Opcode::Invoke {
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                {
                    let transfer = transfers
                        .as_deref_mut()
                        .ok_or(EmitError::UnsupportedOp(0xFA))?;
                    let veneer = match d.aux {
                        AuxData::CallTarget(symbol) => transfer
                            .named_veneers
                            .iter()
                            .find(|(name, _)| *name == symbol)
                            .map_or(transfer.veneer, |(_, entry)| *entry),
                        AuxData::HostEval(_)
                        | AuxData::FunctionLookup(_)
                        | AuxData::TransferThrow
                        | AuxData::CatchScope { .. }
                        | AuxData::HandlerScope { .. }
                        | AuxData::HandlerBindScope { .. }
                        | AuxData::RestartCaseScope { .. } => transfer.veneer,
                        AuxData::CleanupContinuation { .. } => match transfer.cleanup {
                            Some(CleanupEmission::Native { complete, .. }) => complete,
                            _ => return Err(EmitError::UnsupportedOp(0xFA)),
                        },
                        _ => return Err(EmitError::UnsupportedOp(0xFA)),
                    };
                    let recursion = transfer.recursion.filter(|r| {
                        matches!(d.aux, AuxData::CallTarget(symbol) if symbol == r.symbol)
                            && d.args.len() == r.arity
                    });
                    let (return_offsets, call_stack_adjust) = emit_invoke_call(
                        &mut a,
                        &d,
                        &homes,
                        &const_tagged,
                        frame_base_home.ok_or(EmitError::UnsupportedOp(0xFD))?,
                        activation_slots.unwrap(),
                        root_shadow_slots,
                        veneer,
                        recursion.map(|r| (r, frame_entry_label)),
                    )?;
                    let index = transfer_maps
                        .iter()
                        .position(|map| map.call == inst)
                        .ok_or(EmitError::UnsupportedOp(0xFD))?;
                    let map = transfer_maps.remove(index);
                    let mut shadow_roots = Vec::new();
                    for (index, value) in roots
                        .ok_or(EmitError::UnsupportedOp(0xFD))?
                        .iter()
                        .enumerate()
                    {
                        let location = homes[value]
                            .location()
                            .ok_or(EmitError::UnsupportedOp(0xFD))?;
                        if map.roots.contains(&location) {
                            let slot = activation_slots
                                .unwrap()
                                .checked_add(index as u16)
                                .ok_or(EmitError::UnsupportedOp(0xFD))?;
                            shadow_roots.push((location, slot));
                        }
                    }
                    // A mapped tagged value may lack a shadow only if every
                    // value assigned this home is provably non-moving. Never
                    // let a liveness gap silently select a stale native root.
                    for root in &map.roots {
                        if shadow_roots.iter().any(|(location, _)| location == root) {
                            continue;
                        }
                        let values: Vec<_> = homes
                            .iter()
                            .filter(|(_, home)| home.location() == Some(*root))
                            .map(|(&value, _)| value)
                            .collect();
                        if values.is_empty() || !values.into_iter().all(proven_immediate) {
                            return Err(EmitError::UnsupportedOp(0xFD));
                        }
                    }
                    for return_offset in return_offsets {
                        transfer.sites.push(crate::t2::transfer_sites::SysvTransferSite {
                            return_offset,
                            stack_slots: native_spill_slots,
                            call_stack_adjust,
                            activation_slots: activation_slots
                                .unwrap()
                                .checked_add(shadow_root_slots)
                                .ok_or(EmitError::UnsupportedOp(0xFD))?,
                            shadow_roots: shadow_roots.clone(),
                            map: map.clone(),
                        });
                    }
                }
                #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
                return Err(EmitError::UnsupportedOp(0xFA));
            } else if matches!(
                d.opcode,
                Opcode::CleanupSave
                    | Opcode::CleanupRestore
                    | Opcode::CatchLanding
                    | Opcode::HandlerLanding
            ) {
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                {
                    let helper = match transfers.as_ref().and_then(|t| t.cleanup) {
                        Some(CleanupEmission::Native {
                            handler_landing, ..
                        }) if d.opcode == Opcode::HandlerLanding && handler_landing != 0 => {
                            handler_landing
                        }

                        Some(CleanupEmission::Native { catch_landing, .. })
                            if d.opcode == Opcode::CatchLanding && catch_landing != 0 =>
                        {
                            catch_landing
                        }
                        Some(CleanupEmission::Normal { save, restore }) => {
                            if d.opcode == Opcode::CleanupSave {
                                save
                            } else {
                                restore
                            }
                        }
                        Some(CleanupEmission::Native { save, .. })
                            if d.opcode == Opcode::CleanupSave =>
                        {
                            save
                        }
                        _ => return Err(EmitError::UnsupportedOp(0xFA)),
                    };
                    let (cleanup_bcp, resume_bcp) = match d.aux {
                        AuxData::CleanupContinuation {
                            cleanup_bcp,
                            resume_bcp,
                        } => (cleanup_bcp, resume_bcp),
                        AuxData::CatchDestination {
                            push_bcp,
                            resume_bcp,
                        } => (push_bcp, resume_bcp),
                        AuxData::HandlerDestination {
                            push_bcp,
                            clause_index,
                            ..
                        } => (push_bcp, clause_index),
                        _ => return Err(EmitError::UnsupportedOp(0xFA)),
                    };
                    if let Some(value) = d.args.first() {
                        if let Some(&bits) = const_tagged.get(value) {
                            mov_imm64(&mut a, 2, bits as i64);
                        } else {
                            load_home(&mut a, 2, homes[value], 0);
                        }
                    } else {
                        mov_imm64(&mut a, 2, 0);
                    }
                    mov_imm64(&mut a, 7, i64::from(cleanup_bcp));
                    mov_imm64(&mut a, 6, i64::from(resume_bcp));
                    mov_imm64(&mut a, RAX, helper as i64);
                    a.extend_from_slice(&[0xff, 0xd0]);
                    if let Some(value) = d.results.first() {
                        store_home(&mut a, homes[value], RAX, 0);
                    }
                }
                #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
                return Err(EmitError::UnsupportedOp(0xFA));
            } else if d.opcode == Opcode::Call {
                let self_entry = has_reg_entry.then_some(reg_entry_label);
                emit_call(
                    &mut a,
                    &d,
                    &inst_reg,
                    &homes,
                    &const_tagged,
                    c2i_call_addr,
                    c2i_call_slice_addr,
                    c2i_recovery_toggle_addr,
                    transfer_check,
                    frame_base_home,
                    activation_slots,
                    root_shadow_slots,
                    self_sym,
                    self_entry,
                    f.block(entry).params.len(),
                    named_calls,
                )?;
            } else if d.opcode == Opcode::MemoryFence {
                match d.aux {
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Read) => {
                        a.extend_from_slice(&[0x0f, 0xae, 0xe8]);
                    }
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Write) => {
                        a.extend_from_slice(&[0x0f, 0xae, 0xf8]);
                    }
                    AuxData::MemoryFence(egcl_rt::bytecode::MemoryFenceKind::Full) => {
                        a.extend_from_slice(&[0x0f, 0xae, 0xf0]);
                    }
                    _ => return Err(EmitError::UnsupportedOp(op_tag(Opcode::MemoryFence))),
                }
                if let Some(value) = d.results.first() {
                    let dst = *inst_reg.get(value).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    mov_imm64(&mut a, dst, egcl_rt::value::NIL.0 as i64);
                }
            } else if d.opcode == Opcode::GenericEq {
                emit_generic_eq(&mut a, &d, &mut inst_reg, &mut inst_pool, &const_tagged)?;
            } else if d.opcode == Opcode::TypeCheck {
                emit_type_check(&mut a, &d, &mut inst_reg, &mut inst_pool, &const_tagged)?;
            } else if fixnum_cmp_cc(d.opcode).is_some() {
                // A comparison the branch did not fuse: produce T/NIL.
                let label = inst_deopt.get(&inst).copied().unwrap_or(deopt);
                emit_fixnum_cmp_value(
                    &mut a,
                    &d,
                    &mut inst_reg,
                    &mut inst_pool,
                    &consts,
                    &proven,
                    label,
                )?;
            } else if d.opcode == Opcode::Guard {
                let label = inst_deopt.get(&inst).copied().unwrap_or(deopt);
                match d.aux {
                    AuxData::StringLayout => {
                        emit_string_layout_guard(&mut a, &d, &mut inst_reg, &mut inst_pool, label)?
                    }
                    AuxData::TypeTag(t) if t.bits == TypeBits::CONS => {
                        emit_cons_guard(&mut a, &d, &mut inst_reg, &mut inst_pool, label)?
                    }
                    AuxData::TypeTag(t) if t.bits == TypeBits::FIXNUM => {
                        emit_fixnum_value_guard(&mut a, &d, &mut inst_reg, &mut inst_pool, label)?;
                        // The result is fixnum by construction; later typed ops
                        // in this block need not re-test it.
                        if let Some(&r0) = d.results.first() {
                            proven.insert(r0);
                        }
                    }
                    _ => return Err(EmitError::UnsupportedOp(op_tag(Opcode::Guard))),
                }
            } else if matches!(d.opcode, Opcode::Car | Opcode::Cdr) {
                emit_cons_field_load(&mut a, &d, &mut inst_reg, &mut inst_pool)?;
            } else if d.opcode == Opcode::StringByteLength {
                emit_string_byte_length(&mut a, &d, &mut inst_reg, &mut inst_pool)?;
            } else if d.opcode == Opcode::StringAsciiCharAt {
                let label = inst_deopt.get(&inst).copied().unwrap_or(deopt);
                emit_string_ascii_char_at(
                    &mut a,
                    &d,
                    &mut inst_reg,
                    &mut inst_pool,
                    &const_tagged,
                    label,
                )?;
            } else if matches!(d.opcode, Opcode::EnvironmentValue | Opcode::SetEnvironmentValue) {
                let index = match d.aux {
                    AuxData::EnvironmentName(index) => index,
                    _ => return Err(EmitError::UnsupportedOp(op_tag(d.opcode))),
                };
                let store = d.opcode == Opcode::SetEnvironmentValue;
                let helper = if store { environment.store } else { environment.load };
                if environment.body == 0 || helper == 0 {
                    return Err(EmitError::UnsupportedOp(op_tag(d.opcode)));
                }
                if store {
                    let value = *d.args.first().ok_or(EmitError::UnsupportedOp(op_tag(d.opcode)))?;
                    if let Some(&bits) = const_tagged.get(&value) {
                        mov_imm64(&mut a, 2, bits as i64); // rdx = tagged value
                    } else {
                        mov_rr(&mut a, 2, *inst_reg.get(&value).ok_or(EmitError::UnsupportedOp(0xF2))?);
                    }
                }
                mov_imm64(&mut a, 7, environment.body as i64); // rdi = retained body
                mov_imm64(&mut a, 6, i64::from(index)); // rsi = name index
                mov_imm64(&mut a, 0, helper as i64);
                emit_runtime_helper_call(&mut a, c2i_recovery_toggle_addr, transfer_check);
                if !store {
                    let result = *d.results.first().ok_or(EmitError::UnsupportedOp(op_tag(d.opcode)))?;
                    let dst = *inst_reg.get(&result).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    mov_rr(&mut a, dst, 0);
                }
            } else if d.opcode == Opcode::SymbolValue {
                emit_symbol_value(
                    &mut a,
                    &d,
                    &inst_reg,
                    c2i_load_global_addr,
                    c2i_recovery_toggle_addr,
                    transfer_check,
                )?;
            } else if d.opcode == Opcode::SymbolFunction {
                // Same shape as a global read, different helper: it reads the
                // symbol's FUNCTION cell rather than its value cell.
                emit_symbol_value(
                    &mut a,
                    &d,
                    &inst_reg,
                    c2i_load_function_addr,
                    c2i_recovery_toggle_addr,
                    transfer_check,
                )?;
            } else if d.opcode == Opcode::SetSymbolValue {
                emit_set_symbol_value(
                    &mut a,
                    &d,
                    &inst_reg,
                    &const_tagged,
                    c2i_store_global_addr,
                    c2i_recovery_toggle_addr,
                    transfer_check,
                )?;
            } else if d.opcode == Opcode::ClearMv {
                // A zero count selects the clear operation in the shared T2 MV
                // helper. Initialise every argument because the same ABI also
                // serves TakeValuesToLocals.
                mov_imm32(&mut a, 7, 0); // edi = primary (unused)
                mov_imm32(&mut a, 6, 0); // esi = destination (null)
                mov_imm32(&mut a, 2, 0); // edx = count (clear)
                mov_imm64(&mut a, 0, c2i_mv_addr as i64);
                #[cfg(not(windows))]
                {
                    // The zero-count path is a leaf that cannot allocate or signal.
                    a.extend_from_slice(&[0xff, 0xd0]);
                }
                #[cfg(windows)]
                emit_runtime_helper_call(&mut a, c2i_recovery_toggle_addr, transfer_check);
            } else if d.opcode == Opcode::TakeValuesToLocals {
                let (nvars, slot_base) = match d.aux {
                    AuxData::ValuesLocals { nvars, slot_base } => (nvars, slot_base),
                    _ => {
                        return Err(EmitError::UnsupportedOp(op_tag(Opcode::TakeValuesToLocals)));
                    }
                };
                let primary = *d
                    .args
                    .first()
                    .ok_or(EmitError::UnsupportedOp(op_tag(Opcode::TakeValuesToLocals)))?;
                if let Some(&bits) = const_tagged.get(&primary) {
                    mov_imm64(&mut a, 7, bits as i64);
                } else {
                    let home = *homes.get(&primary).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    load_home(&mut a, 7, home, 0);
                }
                let frame_base = frame_base_home
                    .ok_or(EmitError::UnsupportedOp(op_tag(Opcode::TakeValuesToLocals)))?;
                load_home(&mut a, 6, frame_base, 0);
                alu_r_imm(&mut a, 0, 6, i32::from(slot_base) * 8); // rsi = first local
                mov_imm64(&mut a, 2, i64::from(nvars));
                mov_imm64(&mut a, 0, c2i_mv_addr as i64);
                emit_runtime_helper_call(&mut a, c2i_recovery_toggle_addr, transfer_check);

                // The helper writes the activation slots. Materialise each SSA
                // result into its stable home one at a time; this also handles
                // arbitrarily many bindings without exhausting temp registers.
                load_home(&mut a, SCRATCH, frame_base, 0);
                for (offset, &result) in d.results.iter().enumerate() {
                    load_mem64_disp(
                        &mut a,
                        RAX,
                        SCRATCH,
                        (i32::from(slot_base) + offset as i32) * 8,
                    );
                    let home = *homes.get(&result).ok_or(EmitError::UnsupportedOp(0xF2))?;
                    store_home(&mut a, home, RAX, 0);
                }
            } else {
                // In precise mode this inst has its own reconstruction stub (built
                // above); route its guards there instead of the whole-rerun stub.
                let inst_deopt_label = inst_deopt.get(&inst).copied().unwrap_or(deopt);
                emit_arith_inst(
                    &mut a,
                    f,
                    &d,
                    &mut inst_reg,
                    &mut inst_pool,
                    &consts,
                    &float_consts,
                    &val_range,
                    &mut proven,
                    &declared_single_floats,
                    inst_deopt_label,
                )?;
            }
            for (home, src) in result_stores {
                store_home(&mut a, home, src, 0);
            }
            if let Some(roots) = roots {
                if let (Some(frame_base), Some(base_slots)) = (frame_base_home, activation_slots) {
                    emit_shadow_root_restore(
                        &mut a,
                        roots,
                        restore_roots
                            .as_ref()
                            .expect("root-sync site has restore metadata"),
                        &homes,
                        frame_base,
                        base_slots,
                    )?;
                }
                root_sync_sites.push(RootSyncSite {
                    code_offset: u32::try_from(sync_offset)
                        .map_err(|_| EmitError::UnsupportedOp(0xFD))?,
                    live_roots: u16::try_from(roots.len())
                        .map_err(|_| EmitError::UnsupportedOp(0xFD))?,
                    register_roots: u16::try_from(
                        roots
                            .iter()
                            .filter(|value| matches!(homes.get(value), Some(FramedHome::Reg(_))))
                            .count(),
                    )
                    .map_err(|_| EmitError::UnsupportedOp(0xFD))?,
                    spill_roots: u16::try_from(
                        roots
                            .iter()
                            .filter(|value| matches!(homes.get(value), Some(FramedHome::Stack(_))))
                            .count(),
                    )
                    .map_err(|_| EmitError::UnsupportedOp(0xFD))?,
                });
            }
        }

        let next = blocks.get(bi + 1).copied();
        let t = f.terminator(b).ok_or(EmitError::UnsupportedOp(0xF3))?;
        let td = f.inst(t).clone();
        match td.opcode {
            Opcode::Invoke if transfer_mode => {
                let target = &td.targets[0];
                parallel_home_move(&mut a, &edge_home_moves(f, target, &homes, &const_tagged)?);
                if next != Some(target.block) {
                    a.jmp(block_label[&target.block]);
                }
            }
            Opcode::NlxTransfer if transfer_mode => {
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                if !td.targets.is_empty() {
                    let transfer = transfers
                        .as_deref_mut()
                        .ok_or(EmitError::UnsupportedOp(0xFA))?;
                    if !matches!(transfer.cleanup, Some(CleanupEmission::Native { .. })) {
                        return Err(EmitError::UnsupportedOp(0xFA));
                    }
                    for target in &td.targets {
                        let landing = f.inst(f.block(target.block).insts[0]);
                        let offset = u32::try_from(a.here()).map_err(|_| EmitError::BadBranch)?;
                        match landing.aux {
                            AuxData::CleanupContinuation { cleanup_bcp, .. } => {
                                transfer.landings.insert(b, (offset, cleanup_bcp));
                            }
                            AuxData::HandlerDestination {
                                push_bcp,
                                table_index,
                                clause_index,
                            } => {
                                transfer.handler_landings.entry(b).or_default().push((
                                    offset,
                                    push_bcp,
                                    table_index,
                                    clause_index,
                                ));
                            }
                            AuxData::CatchDestination {
                                push_bcp,
                                resume_bcp,
                            } => {
                                transfer
                                    .catch_landings
                                    .entry(b)
                                    .or_default()
                                    .push((offset, push_bcp, resume_bcp));
                            }
                            _ => return Err(EmitError::UnsupportedOp(0xFA)),
                        }
                        a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
                        parallel_home_move(
                            &mut a,
                            &edge_home_moves(f, target, &homes, &const_tagged)?,
                        );
                        a.jmp(block_label[&target.block]);
                    }
                    continue;
                }
                // No native destination: the runtime must leave the segment.
                a.extend_from_slice(&[0x0f, 0x0b]);
            }
            // A Trap ends a path that must not continue (bliss-wukf): today,
            // the code after a call that never returns normally.
            //
            // The GENERIC deopt suffices even though it re-runs the function,
            // because this is only reachable when the c2i helper has already
            // stashed an error -- and run_native takes NATIVE_ERROR BEFORE it
            // looks at NATIVE_DEOPT, returning the error without re-running. No
            // committed side effect is repeated, and no FrameState is needed.
            Opcode::Trap => {
                if code_id != 0 {
                    mov_imm64(&mut a, RAX, egcl_rt::value::NIL.0 as i64);
                    emit_epilogue(&mut a);
                    a.push(0xC3);
                } else {
                    a.jmp(deopt);
                }
            }
            Opcode::Return => {
                if let Some(&value) = td.args.first() {
                    if let Some(&bits) = const_tagged.get(&value) {
                        mov_imm64(&mut a, RAX, bits as i64);
                    } else {
                        load_home(
                            &mut a,
                            RAX,
                            *homes.get(&value).ok_or(EmitError::UnsupportedOp(0xF2))?,
                            0,
                        );
                    }
                }
                emit_epilogue(&mut a); // restore callee-saved (no-op frameless)
                a.push(0xC3); // ret
            }
            Opcode::Jump => {
                let tc = &td.targets[0];
                parallel_home_move(&mut a, &edge_home_moves(f, tc, &homes, &const_tagged)?);
                if next != Some(tc.block) {
                    a.jmp(block_label[&tc.block]);
                }
            }
            Opcode::Brif => {
                let cond = td.args[0];
                // A constant condition (e.g. cond's trailing `(t ...)` clause) makes
                // the branch unconditional: jump to the true target unless the
                // constant is NIL.
                if let Some(&bits) = const_tagged.get(&cond) {
                    let taken = if bits == egcl_rt::value::NIL.0 {
                        &td.targets[1]
                    } else {
                        &td.targets[0]
                    };
                    parallel_home_move(&mut a, &edge_home_moves(f, taken, &homes, &const_tagged)?);
                    if next != Some(taken.block) {
                        a.jmp(block_label[&taken.block]);
                    }
                    continue;
                }
                // Compute the branch condition code. A fused fixnum comparison
                // lowers to cmp+jcc directly; any other condition is a general
                // Lisp truthiness test — only NIL is false, so materialise the
                // value and branch to the true target (targets[0]) when it != NIL.
                let cc = match f.value(cond).def {
                    ValueDef::Result { inst, .. } if fused.contains(&inst) => {
                        let cmp_data = f.inst(inst).clone();
                        let (cmp_regs, _) =
                            prepare_framed_inst(&mut a, &cmp_data, &homes, &const_tagged)?;
                        if cmp_data.opcode == Opcode::GenericEq {
                            // EQ/NULL fused: cmp the two tagged operands; equal
                            // (ZF) is "true", taking targets[0].
                            emit_eq_cmp(&mut a, &cmp_data, &cmp_regs, &const_tagged)?;
                            Cc::E
                        } else {
                            emit_fused_compare(
                                &mut a, &cmp_data, &cmp_regs, &consts, &proven, deopt,
                            )?
                        }
                    }
                    _ => {
                        let home = *homes.get(&cond).ok_or(EmitError::UnsupportedOp(0xF2))?;
                        load_home(&mut a, 11, home, 0);
                        let cr = 11;
                        alu_r_imm(&mut a, 7, cr, egcl_rt::value::NIL.0 as i32); // cmp cr, NIL
                        Cc::Ne
                    }
                };
                let then_b = td.targets[0].block; // taken when the comparison is true
                let else_b = td.targets[1].block;
                let then_moves = edge_home_moves(f, &td.targets[0], &homes, &const_tagged)?;
                let else_moves = edge_home_moves(f, &td.targets[1], &homes, &const_tagged)?;
                match (then_moves.is_empty(), else_moves.is_empty()) {
                    // No edge moves either side: a plain two-way branch. Fall through
                    // to whichever arm is the next block; branch to the other.
                    (true, true) => {
                        if next == Some(else_b) {
                            a.jcc(cc, block_label[&then_b]);
                        } else if next == Some(then_b) {
                            a.jcc(cc.inverse(), block_label[&else_b]);
                        } else {
                            a.jcc(cc, block_label[&then_b]);
                            a.jmp(block_label[&else_b]);
                        }
                    }
                    // Only one side has moves: branch straight to the move-free side,
                    // then emit the other side's moves inline before its jump.
                    (true, false) => {
                        a.jcc(cc, block_label[&then_b]);
                        parallel_home_move(&mut a, &else_moves);
                        if next != Some(else_b) {
                            a.jmp(block_label[&else_b]);
                        }
                    }
                    (false, true) => {
                        a.jcc(cc.inverse(), block_label[&else_b]);
                        parallel_home_move(&mut a, &then_moves);
                        if next != Some(then_b) {
                            a.jmp(block_label[&then_b]);
                        }
                    }
                    // Both sides move: a trampoline for the taken arm, inline fall for
                    // the other.
                    (false, false) => {
                        let lthen = a.label();
                        a.jcc(cc, lthen);
                        parallel_home_move(&mut a, &else_moves);
                        a.jmp(block_label[&else_b]);
                        a.bind(lthen);
                        parallel_home_move(&mut a, &then_moves);
                        a.jmp(block_label[&then_b]);
                    }
                }
            }
            other => return Err(EmitError::UnsupportedOp(op_tag(other))),
        }
    }

    if let Some(check) = transfer_check {
        // Do not resume bytecode or run later side effects. The owning runtime
        // activation delivers the pending transfer after this native return.
        // Use this function's actual spill/padding/saved-register layout.
        a.bind(check.exit);
        mov_imm64(&mut a, RAX, egcl_rt::value::NIL.0 as i64);
        emit_epilogue(&mut a);
        a.push(0xC3); // ret
    }

    // Deopt stub: restore callee-saved (so the caller's registers are intact),
    // then align rsp and call c2i_deopt, unwind the alignment, return (the value is
    // ignored on deopt). After the epilogue rsp%16==8 (as at entry), so one
    // sub/add aligns the call.
    a.bind(deopt);
    if !cfg!(windows) {
        emit_epilogue(&mut a);
        a.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8
    }
    mov_imm64(&mut a, 0, c2i_deopt_addr as i64); // mov rax, c2i_deopt
                                                 // Deopt stubs have their own temporary stack layout and return immediately;
                                                 // they must finish that cleanup instead of taking the normal transfer exit.
    emit_runtime_helper_call(&mut a, c2i_recovery_toggle_addr, None);
    if cfg!(windows) {
        emit_epilogue(&mut a);
    } else {
        a.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
    }
    a.push(0xC3); // ret

    // Precise per-guard deopt stubs (bliss-mba). Each serialises every logical
    // FrameState scope — outermost to innermost, locals then live operands, all
    // still tagged (no unboxing exists yet) — for c2i_deopt_t2 to reconstruct
    // the interpreter activation chain. A side effect committed in an outer
    // scope therefore runs exactly once. The value regs are still live here
    // (read BEFORE the epilogue restores callee-saved). rsp%16==0 after the
    // call-capable prologue, so
    // a 16-multiple sub keeps the call aligned.
    enum SlotSrc {
        Home(FramedHome),
        Imm(u64),
        Remat(crate::t2::frame_state::RematRecipeId),
    }
    // An OSR entry with type checks deoptimises through its own header state
    // when an imported value fails; the stub is emitted with the others.
    let osr_check_labels: Vec<Option<egcl_rt::asm::Label>> = f
        .osr_entries
        .iter()
        .map(|o| (!o.checks.is_empty()).then(|| a.label()))
        .collect();
    let mut deopt_stubs: Vec<(
        crate::t2::frame_state::FrameStateId,
        egcl_rt::asm::Label,
        String,
    )> = Vec::new();
    for (&inst, &label) in &inst_deopt {
        let fsid = f
            .inst(inst)
            .frame_state
            .ok_or(EmitError::UnsupportedOp(0xF4))?;
        deopt_stubs.push((fsid, label, format!("{inst:?}")));
    }
    for (osr, label) in f.osr_entries.iter().zip(&osr_check_labels) {
        if let Some(label) = label {
            deopt_stubs.push((osr.frame_state, *label, format!("osr@{}", osr.bcp)));
        }
    }
    for (fsid, label, origin) in deopt_stubs {
        let fs = f.frame_states.get(fsid);
        if fs.scopes.is_empty() {
            return Err(EmitError::UnsupportedOp(0xF4));
        }
        if std::env::var_os("EGCL_FRAMESTATE_DBG").is_some() {
            for (si, scope) in fs.scopes.iter().enumerate() {
                eprintln!(
                    "[fs] inst={origin} scope#{si} fn={} bcp={} nlocals={} nstack={}",
                    scope.function,
                    scope.bcp,
                    scope.locals.len(),
                    scope.stack.len()
                );
                for (li, vs) in scope.locals.iter().enumerate() {
                    eprintln!("[fs]   local[{li}] = {vs:?}");
                }
                for (sti, vs) in scope.stack.iter().enumerate() {
                    eprintln!("[fs]   stack[{sti}] = {vs:?}");
                }
            }
        }
        // Serialized virtual-frame stream: for each outer-to-inner scope,
        // [function, bcp, nlocals, nstack, locals..., stack...].
        let n_words: usize = fs
            .scopes
            .iter()
            .map(|scope| 4 + scope.locals.len() + scope.stack.len())
            .sum();
        let mut srcs: Vec<SlotSrc> = Vec::with_capacity(n_words);
        for scope in &fs.scopes {
            srcs.push(SlotSrc::Imm(scope.function as u64));
            srcs.push(SlotSrc::Imm(scope.bcp as u64));
            srcs.push(SlotSrc::Imm(scope.locals.len() as u64));
            srcs.push(SlotSrc::Imm(scope.stack.len() as u64));
            for vs in scope.locals.iter().chain(scope.stack.iter()) {
                let s = match vs {
                    ValueSource::Value { value, repr } => {
                        if *repr != ValueRepresentation::Tagged {
                            return Err(EmitError::UnsupportedOp(0xF5));
                        }
                        if let Some(&bits) = const_tagged.get(value) {
                            SlotSrc::Imm(bits)
                        } else if let Some(&home) = homes.get(value) {
                            SlotSrc::Home(home)
                        } else {
                            return Err(EmitError::UnsupportedOp(0xF6));
                        }
                    }
                    ValueSource::Const(bv) => {
                        if is_moving_gc_reference(*bv) {
                            return Err(EmitError::UnsupportedOp(0xF8));
                        }
                        SlotSrc::Imm(bv.0)
                    }
                    ValueSource::Unbound => SlotSrc::Imm(egcl_rt::value::UNBOUND.0),
                    ValueSource::Remat(id) => SlotSrc::Remat(*id),
                };
                if std::env::var_os("EGCL_DEOPT_SRC_DBG").is_some() {
                    let desc = match &s {
                        SlotSrc::Home(h) => format!("Home({h:?})"),
                        SlotSrc::Imm(b) => format!("Imm({b:#x})"),
                        SlotSrc::Remat(_) => "Remat".to_string(),
                    };
                    eprintln!("[deopt-src] fn={} src={:?} -> {}", scope.function, vs, desc);
                }
                srcs.push(s);
            }
        }
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        let segment_deopt = transfers.as_ref().and_then(|t| t.deopt_veneer);
        #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
        let segment_deopt: Option<u64> = None;
        let request_bytes = if segment_deopt.is_some() { 32 } else { 0 };
        let alloc = ((n_words * 8) + request_bytes + 15) & !15;

        a.bind(label);
        #[cfg(all(target_arch = "x86_64", windows))]
        probe_windows_stack(&mut a, alloc as u32);
        if alloc > 0 {
            a.extend_from_slice(&[0x48, 0x81, 0xEC]); // sub rsp, imm32
            a.extend_from_slice(&(alloc as i32).to_le_bytes());
        }
        for (i, s) in srcs.iter().enumerate() {
            match s {
                SlotSrc::Home(home) => {
                    load_home(&mut a, RAX, *home, alloc as i32);
                    store_to_rsp(&mut a, RAX, (i * 8) as i32);
                }
                SlotSrc::Imm(bits) => {
                    mov_imm64(&mut a, 0 /* rax scratch */, *bits as i64);
                    store_to_rsp(&mut a, 0, (i * 8) as i32);
                }
                SlotSrc::Remat(id) => {
                    emit_deopt_source(
                        &mut a,
                        &ValueSource::Remat(*id),
                        fs,
                        &homes,
                        &const_tagged,
                        alloc as i32,
                    )?;
                    store_to_rsp(&mut a, RAX, (i * 8) as i32);
                }
            }
        }
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        if let Some(veneer) = segment_deopt {
            let request = (n_words * 8) as i32;
            mov_imm64(&mut a, RAX, fs.scopes.len() as i64);
            store_to_rsp(&mut a, RAX, request);
            mov_imm64(&mut a, RAX, n_words as i64);
            store_to_rsp(&mut a, RAX, request + 8);
            mov_rr(&mut a, RAX, 4);
            store_to_rsp(&mut a, RAX, request + 16);
            load_home(
                &mut a,
                RAX,
                frame_base_home.ok_or(EmitError::UnsupportedOp(0xFD))?,
                alloc as i32,
            );
            store_to_rsp(&mut a, RAX, request + 24);
            // lea rdi, [rsp + request]
            a.extend_from_slice(&[0x48, 0x8D, 0xBC, 0x24]);
            a.extend_from_slice(&request.to_le_bytes());
            mov_imm64(&mut a, RAX, veneer as i64);
            emit_runtime_helper_call(&mut a, 0, None);
            transfers.as_mut().unwrap().deopt_returns.push(a.here() as u32);
        }
        if segment_deopt.is_none() {
            // c2i_deopt_t2(n_scopes=rdi, n_words=rsi, buf=rdx, reserved=rcx)
            mov_imm32(&mut a, 7, fs.scopes.len() as u32);
            mov_imm32(&mut a, 6, n_words as u32);
            if let Some(home) = entry_kind_home {
                load_home(&mut a, 1, home, alloc as i32);
            } else {
                mov_imm32(&mut a, 1, 0);
            }
            mov_rr(&mut a, 2, 4); // mov rdx, rsp
            mov_imm64(&mut a, 0, c2i_deopt_t2_addr as i64);
            emit_runtime_helper_call(&mut a, c2i_recovery_toggle_addr, None);
        }
        if alloc > 0 {
            a.extend_from_slice(&[0x48, 0x81, 0xC4]); // add rsp, imm32
            a.extend_from_slice(&(alloc as i32).to_le_bytes());
        }
        emit_epilogue(&mut a);
        a.push(0xC3); // ret
    }

    // T1→T2 OSR entries.  The lower tier and T2 share the EgclStack slot
    // layout. Reload the values named by the optimized header FrameState into
    // their allocated homes, then jump directly to the optimized loop block.
    // The normal Return epilogue
    // balances this stub's prologue.
    let mut osr_entries = Vec::new();
    for (osr_idx, osr) in f.osr_entries.iter().enumerate() {
        let fs = f.frame_states.get(osr.frame_state);
        let Some(scope) = fs.scopes.first() else {
            continue;
        };
        if fs.scopes.len() != 1 || !scope.stack.is_empty() {
            continue;
        }
        // bliss-ht4: the transfer below moves a RAW machine word out of the
        // interpreter frame slot into the value's allocated home. A T0/T1
        // interpreter slot always holds a tagged EgclVal, so that is correct
        // only while every live slot's T2 representation is `Tagged`. If a pass
        // has unboxed one, the correct transfer needs the D4.09 conversion this
        // stub cannot emit — and emitting the plain move would hand the loop a
        // tagged word it reads as a raw integer, a miscompile no non-OSR test
        // can see. Decline the OSR entry instead: the loop loses its OSR fast
        // path and still runs correctly at the lower tier.
        //
        // The representations come from `slot_map`, the same producer the deopt
        // export reads, so import and export cannot disagree about this frame.
        let specs = slot_map::slot_specs(scope, fs);
        if specs
            .iter()
            .any(|s| ConversionKind::for_repr(s.repr) != Some(ConversionKind::None))
        {
            continue;
        }
        // Liveness at the target, read from regalloc2's precise per-ProgPoint
        // ranges (see the import loop below); needed first to decide whether
        // every checked value will be imported at all.
        let target_pp = f
            .block_order()
            .iter()
            .position(|&b| b == osr.block)
            .map(|bi| machine.blocks[bi].start as u32 * 2);
        let live_at_target = |value: Value| -> bool {
            let Some(pp) = target_pp else { return true };
            machine.value_locations.iter().any(|r| {
                r.vreg.class == RegClass::Gpr
                    && Value(r.vreg.num) == value
                    && r.start <= pp
                    && pp < r.end
            })
        };
        // Every checked value must be a fixnum proof on an imported, homed
        // value, or the entry cannot establish it: decline (bliss-5yz5h).
        let checks_emittable = osr.checks.iter().all(|&(v, ty)| {
            !ty.bits.is_bottom()
                && ty.bits.meet(TypeBits::FIXNUM) == ty.bits
                && homes.contains_key(&v)
                && live_at_target(v)
        });
        if !checks_emittable {
            continue;
        }
        let offset = a.here();
        emit_prologue(&mut a);
        if let Some(home) = entry_kind_home {
            mov_imm64(&mut a, RAX, code_id as i64);
            store_home(&mut a, home, RAX, 0);
        }
        if cfg!(windows) {
            mov_rr(&mut a, 7, 1);
        }
        if let Some(home) = frame_base_home {
            store_home(&mut a, home, 7 /* rdi */, 0);
        }
        // The header state names values that are not live at the target and
        // so are never imported — deopt-only loop phis. Give those homes NIL
        // first, so a later deopt stub (a failed entry check, or any guard in
        // the loop) serialises a tagged datum rather than whatever the
        // register or slot held when the entry was taken; a live import below
        // overwrites any home the two happen to share.
        for spec in &specs {
            if let ValueSource::Value { value, .. } = &spec.source {
                if !live_at_target(*value) {
                    if let Some(&home) = homes.get(value) {
                        mov_imm64(&mut a, RAX, egcl_rt::value::NIL.0 as i64);
                        store_home(&mut a, home, RAX, 0);
                    }
                }
            }
        }
        // Import only slots whose value is LIVE at the OSR target block.
        // regalloc2 reuses one register for several disjoint-range values, so
        // a slot map can legitimately name two values sharing a register home
        // — at most one is live at the target. Loading a dead slot after the
        // live one clobbered it (bliss-x5y.29: with the reduced register pool,
        // live-osr's sum and a dead local shared rcx and the OSR result was
        // the dead value). Liveness is read from regalloc2's precise
        // per-ProgPoint ranges, the same source the safepoint roots use.
        for spec in &specs {
            let slot = spec.index as usize;
            if let ValueSource::Value { value, .. } = &spec.source {
                if std::env::var_os("EGCL_RA_DBG").is_some() {
                    eprintln!(
                        "[osr] fn={} slot={} v{} live={} home={:?} target_pp={:?}",
                        f.name(),
                        slot,
                        value.0,
                        live_at_target(*value),
                        homes.get(value),
                        target_pp
                    );
                }
                if !live_at_target(*value) {
                    continue;
                }
                if let Some(&home) = homes.get(value) {
                    match home {
                        FramedHome::Reg(r) => mov_from_frame(&mut a, r, 7 /* rdi */, slot),
                        FramedHome::Stack(_) => {
                            mov_from_frame(&mut a, RAX, 7 /* rdi */, slot);
                            store_home(&mut a, home, RAX, 0);
                        }
                    }
                }
            }
        }
        // Establish the proofs the loop relies on for imported values: a
        // non-fixnum deoptimises to the header state before any iteration.
        if let Some(label) = osr_check_labels[osr_idx] {
            for &(v, _) in &osr.checks {
                match homes[&v] {
                    FramedHome::Reg(r) => guard_fixnum(&mut a, r, label),
                    home @ FramedHome::Stack(_) => {
                        load_home(&mut a, RAX, home, 0);
                        guard_fixnum(&mut a, RAX, label);
                    }
                }
            }
        }
        jump_to_shared_body(&mut a, block_label[&osr.block]);
        osr_entries.push((osr.bcp, offset));
    }

    // A deopt stub can be allocated and emitted without any hot-path branch to
    // it (for example, range inference can prove an overflow guard redundant).
    // Conversely, pure functions use the shared whole-function `deopt` stub and
    // have no entries in `inst_deopt`. Derive the capability from actual label
    // references, not from which kind of stub happened to be allocated.
    if code_id != 0 && a.label_is_referenced(deopt) {
        return Err(EmitError::UnsupportedOp(0xF4));
    }
    let has_deopt = a.label_is_referenced(deopt)
        || inst_deopt
            .values()
            .any(|&label| a.label_is_referenced(label))
        || osr_check_labels
            .iter()
            .flatten()
            .any(|&label| a.label_is_referenced(label));
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    if !transfer_maps.is_empty() {
        return Err(EmitError::UnsupportedOp(0xFD));
    }
    let code = a.finish().ok_or(EmitError::BadBranch)?;
    #[cfg(all(target_arch = "x86_64", windows))]
    let windows_unwind = {
        let mut starts = vec![0usize];
        if has_reg_entry {
            starts.push(compiled_entry);
        }
        starts.extend(osr_entries.iter().map(|&(_, offset)| offset));
        starts.push(code.len());
        starts
            .windows(2)
            .map(|range| {
                Ok(WindowsUnwindRange {
                    begin: u32::try_from(range[0]).map_err(|_| EmitError::BadBranch)?,
                    end: u32::try_from(range[1]).map_err(|_| EmitError::BadBranch)?,
                    unwind_info: _unwind_info.clone(),
                })
            })
            .collect::<Result<Vec<_>, EmitError>>()?
    };
    // Per-bcp native offset map (bliss-zmmb): the earliest native offset carrying
    // each bytecode position. Sparse — only bcps with a frame-state instruction
    // appear; the rest stay u32::MAX. Same shape as the T1 map so the viewer can
    // consume both identically.
    let bcp_offsets = if bcp_sites.is_empty() {
        Vec::new()
    } else {
        let max_bcp = bcp_sites.iter().map(|&(_, b)| b).max().unwrap_or(0) as usize;
        let mut m = vec![u32::MAX; max_bcp + 1];
        for &(off, bcp) in &bcp_sites {
            let slot = &mut m[bcp as usize];
            *slot = (*slot).min(off);
        }
        m
    };
    let mut heap_constant_slots: Vec<usize> = heap_const_slots.values().copied().collect();
    heap_constant_slots.sort_unstable();
    heap_constant_slots.dedup();
    Ok(FramedCode {
        code,
        #[cfg(all(target_arch = "x86_64", windows))]
        windows_unwind,
        compiled_entry,
        osr_entries,
        bcp_offsets,
        native_spill_slots,
        regalloc_spill_slots,
        allocation_edits,
        shadow_root_slots,
        emitted_safepoints,
        root_sync_sites,
        heap_constant_slots,
        // True iff emitted control flow can reach a speculation-deopt stub; lets
        // the caller direct-call a non-deopting T2 function with no post-call
        // deopt handling (bliss-zhvn).
        has_deopt,
    })
}

/// The `UnsupportedOp` payload that names an opcode: the T2 log decodes the
/// 0x1000 bit as "refused Opcode #N in t2/ir.rs" (bliss-8b236f61). Shared with
/// the other emitters so their declines are greppable the same way.
pub(super) fn op_tag(op: crate::t2::ir::Opcode) -> u32 {
    op as u32 | 0x1000
}

// ── Emitter ─────────────────────────────────────────────────────────

/// Emit `mf` to x86-64 machine code. `mf.allocation` must be populated (P6).
pub fn emit(mf: &MachFunc) -> Result<Vec<u8>, EmitError> {
    use crate::t2::lower::op;
    // Exceptional call edges require the native-transfer ABI and landing-pad
    // emission. Refuse before consulting ordinary-call allocation assumptions.
    if let Some(inst) = mf
        .insts
        .iter()
        .find(|i| matches!(i.op, op::INVOKE | op::INVOKE_ROUTES))
    {
        return Err(EmitError::UnsupportedOp(inst.op));
    }
    // Which callee-saved registers the body actually uses → prologue/epilogue.
    let mut used_callee: Vec<u8> = Vec::new();
    for loc in mf
        .inst_allocations
        .iter()
        .flatten()
        .chain(mf.allocation_edits.iter().flat_map(|e| [&e.from, &e.to]))
    {
        if let Location::Register(p) = *loc {
            if let Ok(enc) = gpr_enc(p) {
                if CALLEE_SAVED_X86.contains(&enc) && !used_callee.contains(&enc) {
                    used_callee.push(enc);
                }
            }
        }
    }
    // x86's two-address ADD/SUB needs a temporary when the allocator assigns
    // the destination to the second input's register.  regalloc2's dedicated
    // scratch is free within an instruction, but it is callee-saved in SysV.
    let two_addr_needs_scratch = mf.insts.iter().enumerate().any(|(i, mi)| {
        if !matches!(mi.op, crate::t2::lower::op::ADD | crate::t2::lower::op::SUB)
            || mi.defs.len() != 1
            || mi.uses.len() != 2
        {
            return false;
        }
        let Some(allocs) = mf.inst_allocations.get(i) else {
            return false;
        };
        allocs.len() == 3 && allocs[0] == allocs[2] && allocs[0] != allocs[1]
    });
    if two_addr_needs_scratch && !used_callee.contains(&RA_SCRATCH_GPR) {
        used_callee.push(RA_SCRATCH_GPR);
    }

    let mut a = Asm::new();

    // ── Prologue: save callee-saved registers this function clobbers.
    for &r in &used_callee {
        push_reg(&mut a, r);
    }
    let spill_bytes = ((mf.num_spill_slots as usize * 8) + 15) & !15;
    if spill_bytes != 0 {
        a.extend_from_slice(&[0x48, 0x81, 0xEC]);
        a.extend_from_slice(&(spill_bytes as i32).to_le_bytes());
    }

    // Emit the body. A `RET` runs the epilogue (restore + `ret`) inline. Block
    // structure is used for labels once branches are supported; the straight-line
    // first cut walks the flat inst vector in order.
    let epilogue = |a: &mut Asm| {
        if spill_bytes != 0 {
            a.extend_from_slice(&[0x48, 0x81, 0xC4]);
            a.extend_from_slice(&(spill_bytes as i32).to_le_bytes());
        }
        for &r in used_callee.iter().rev() {
            pop_reg(a, r);
        }
        a.push(0xC3); // ret
    };

    if mf.inst_allocations.len() != mf.insts.len() {
        return Err(EmitError::Unallocated(
            mf.insts
                .iter()
                .flat_map(|mi| mi.defs.iter().chain(&mi.uses))
                .next()
                .copied()
                .unwrap_or(VReg {
                    class: RegClass::Gpr,
                    num: 0,
                }),
        ));
    }

    for (index, mi) in mf.insts.iter().enumerate() {
        for edit in mf
            .allocation_edits
            .iter()
            .filter(|e| e.inst == index && e.position == EditPosition::Before)
        {
            emit_location_move(&mut a, edit.from, edit.to)?;
        }
        emit_inst(&mut a, mi, &mf.inst_allocations[index], &epilogue)?;
        for edit in mf
            .allocation_edits
            .iter()
            .filter(|e| e.inst == index && e.position == EditPosition::After)
        {
            emit_location_move(&mut a, edit.from, edit.to)?;
        }
    }

    a.finish().ok_or(EmitError::BadBranch)
}

fn spill_disp(slot: crate::t2::mach::StackSlot) -> i32 {
    (slot.0 as i32) * 8
}

fn emit_location_move(a: &mut Asm, from: Location, to: Location) -> Result<(), EmitError> {
    match (from, to) {
        (Location::Register(src), Location::Register(dst)) => {
            mov_rr(a, gpr_enc(dst)?, gpr_enc(src)?);
        }
        (Location::Register(src), Location::Stack(dst)) => {
            store_to_rsp(a, gpr_enc(src)?, spill_disp(dst));
        }
        (Location::Stack(src), Location::Register(dst)) => {
            load_from_rsp(a, gpr_enc(dst)?, spill_disp(src));
        }
        // regalloc2 guarantees that it never asks clients to encode a direct
        // memory-to-memory edit.
        (Location::Stack(_), Location::Stack(_)) => return Err(EmitError::UnsupportedOp(0xFE)),
    }
    Ok(())
}

fn emit_inst(
    a: &mut Asm,
    mi: &MachInst,
    allocations: &[Location],
    epilogue: &impl Fn(&mut Asm),
) -> Result<(), EmitError> {
    if allocations.len() != mi.defs.len() + mi.uses.len() {
        return Err(EmitError::Unallocated(
            mi.defs
                .first()
                .or_else(|| mi.uses.first())
                .copied()
                .unwrap_or(VReg {
                    class: RegClass::Gpr,
                    num: 0,
                }),
        ));
    }
    let operand_reg = |operand: usize, v: VReg| -> Result<u8, EmitError> {
        match allocations[operand] {
            Location::Register(p) => gpr_enc(p),
            Location::Stack(_) => Err(EmitError::Spilled(v)),
        }
    };
    let def = |n: usize| operand_reg(n, mi.defs[n]);
    let use_ = |n: usize| operand_reg(mi.defs.len() + n, mi.uses[n]);

    use crate::t2::lower::op;
    match mi.op {
        op::LIVENESS => {}
        op::MOV_IMM | op::MOV_TAGGED => {
            // Both materialise a 64-bit immediate into a GPR; MOV_TAGGED's is a
            // tagged EgclVal, MOV_IMM's a raw/tagged integer — identical encoding.
            let dst = def(0)?;
            let imm = mi.imm.ok_or(EmitError::MissingImm)?;
            mov_imm64(a, dst, imm);
        }
        op::MOV => {
            let dst = def(0)?;
            let src = use_(0)?;
            mov_rr(a, dst, src);
        }
        op::MEMORY_FENCE => {
            match mi.imm.ok_or(EmitError::MissingImm)? {
                1 => a.extend_from_slice(&[0x0f, 0xae, 0xe8]),
                2 => a.extend_from_slice(&[0x0f, 0xae, 0xf8]),
                3 => a.extend_from_slice(&[0x0f, 0xae, 0xf0]),
                _ => return Err(EmitError::UnsupportedOp(op::MEMORY_FENCE)),
            }
            mov_imm64(a, def(0)?, egcl_rt::value::NIL.0 as i64);
        }
        op::ADD | op::SUB => {
            // def = uses[0] (op) uses[1]. x86 ALU is 2-operand, so realise as
            // `mov def, a; op def, b` (skips the mov when def already is a).
            let dst = def(0)?;
            let a0 = use_(0)?;
            let b0 = use_(1)?;
            let opcode = if mi.op == op::ADD { 0x01 } else { 0x29 };
            if dst == b0 && dst != a0 {
                mov_rr(a, RA_SCRATCH_GPR, b0);
                mov_rr(a, dst, a0);
                alu_rr(a, opcode, dst, RA_SCRATCH_GPR);
            } else {
                mov_rr(a, dst, a0);
                alu_rr(a, opcode, dst, b0);
            }
        }
        op::RET => {
            // Move the return value into rax (SysV return register), then the
            // epilogue restores callee-saved regs and returns.
            if !mi.uses.is_empty() {
                let r = use_(0)?;
                mov_rr(a, RAX, r);
            }
            epilogue(a);
        }
        other => return Err(EmitError::UnsupportedOp(other)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::lower::op;
    use crate::t2::mach::{MachInst, VReg};
    use crate::t2::regalloc::allocate;

    fn gpr(num: u32) -> VReg {
        VReg {
            class: RegClass::Gpr,
            num,
        }
    }

    fn mi(op: u32, defs: Vec<VReg>, uses: Vec<VReg>, imm: Option<i64>) -> MachInst {
        MachInst {
            source_inst: None,
            op,
            defs,
            uses,
            imm,
            frame_state: None,
            deopt_uses: Vec::new(),
            safepoint: false,
        }
    }

    /// Build `() -> imm`: MOV_IMM v0, imm ; RET v0. Allocate, emit, and check the
    /// byte stream is non-empty and ends in `ret`.
    #[test]
    fn emits_const_return_bytes() {
        let mut mf = MachFunc {
            insts: vec![
                mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(12345)),
                mi(op::RET, vec![], vec![gpr(0)], None),
            ],
            ..MachFunc::default()
        };
        allocate(&mut mf).expect("regalloc2");
        let code = emit(&mf).expect("emit const-return");
        assert!(!code.is_empty());
        assert_eq!(*code.last().unwrap(), 0xC3, "must end in ret");
    }

    #[test]
    fn runtime_helper_call_can_bracket_recovery_for_production_c2i_crossings() {
        let mut asm = Asm::new();
        emit_runtime_helper_call(&mut asm, 0x1234_5678, None);
        let code = asm.finish().unwrap();

        assert_eq!(
            code.windows(2)
                .filter(|bytes| *bytes == [0xFF, 0xD0])
                .count(),
            3
        );
        let mov_arg0 = if cfg!(windows) { 0xB9 } else { 0xBF };
        assert!(
            code.windows(5).any(|bytes| bytes == [mov_arg0, 0, 0, 0, 0]),
            "production helper crossing must disable native recovery before Rust"
        );
        assert!(
            code.windows(5).any(|bytes| bytes == [mov_arg0, 1, 0, 0, 0]),
            "production helper crossing must restore native recovery after Rust"
        );
    }

    #[cfg(all(target_arch = "x86_64", windows))]
    #[test]
    fn windows_t2_framed_entries_execute_and_deopt() {
        use egcl_rt::jit::{JitBuffer, WindowsUnwindInfo};
        use egcl_rt::value::{EgclVal, NIL};
        extern "C" fn deopt() -> u64 {
            0x1234_5678_ABCD_EF00
        }
        let mut f = build_mul_ranged(5, None);
        let entry = f.entry();
        let value = f.block(entry).params[0];
        let frame_state = f.frame_states.add(crate::t2::frame_state::FrameState {
            scopes: vec![crate::t2::frame_state::FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![crate::t2::frame_state::ValueSource::Value {
                    value,
                    repr: crate::t2::ir::ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(crate::t2::ir::OsrEntry {
            bcp: 0,
            block: entry,
            frame_state,
            checks: Vec::new(),
        });
        let framed = emit_framed(&f, deopt as *const () as u64, 0, 0, 0, 0, 0, 0, 0, None)
            .expect("framed Windows multiply");
        let ranges: Vec<_> = framed
            .windows_unwind
            .iter()
            .map(|range| WindowsUnwindInfo {
                begin: range.begin,
                end: range.end,
                unwind_info: &range.unwind_info,
            })
            .collect();
        assert_eq!(
            ranges.len(),
            3,
            "interpreter, compiled-register and OSR entries"
        );
        let code = unsafe { JitBuffer::new_with_windows_unwind_ranges(&framed.code, &ranges) }
            .expect("register T2 unwind ranges");
        let interpreted: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr()) };
        // A single custom compiled argument is already in RCX, matching Win64.
        let compiled: extern "C" fn(u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr().add(framed.compiled_entry)) };
        let osr: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr().add(framed.osr_entries[0].1)) };
        let mut slots = [EgclVal::from_fixnum(7).0];
        assert_eq!(interpreted(slots.as_mut_ptr()), EgclVal::from_fixnum(35).0);
        assert_eq!(compiled(slots[0]), EgclVal::from_fixnum(35).0);
        assert_eq!(osr(slots.as_mut_ptr()), EgclVal::from_fixnum(35).0);
        slots[0] = NIL.0;
        assert_eq!(interpreted(slots.as_mut_ptr()), deopt());
        assert_eq!(compiled(slots[0]), deopt());
        assert_eq!(osr(slots.as_mut_ptr()), deopt());
    }

    #[cfg(all(target_arch = "x86_64", windows))]
    #[test]
    fn windows_runtime_helper_passes_six_arguments() {
        for toggle in [false, true] {
            for pending in [None, Some(false), Some(true)] {
                check_windows_runtime_helper(toggle, pending);
            }
        }
    }

    #[cfg(all(target_arch = "x86_64", windows))]
    fn check_windows_runtime_helper(toggle: bool, pending: Option<bool>) {
        use egcl_rt::jit::JitBuffer;

        const FIRST_ARGUMENT: u64 = 0x1020_3040_5060_7080;
        const RESULT: u64 = 0xFEDC_BA98_7654_3210;
        // Each leaf intentionally destroys all volatile GPRs and home slots.
        // The wrapper must preserve its operands/result outside callee storage.
        fn leaf_return(a: &mut Asm, result: u64) {
            for r in [1, 2, 8, 9, 10, 11] {
                mov_imm64(a, r, 0x1234_5678);
            }
            for offset in [8, 16, 24, 32] {
                store_to_rsp(a, 11, offset);
            }
            mov_imm64(a, RAX, result as i64);
            a.push(0xC3);
        }

        let mut observed = [0u64; 15];
        observed[8] = u64::MAX; // recovery state before the first toggle
        let address = observed.as_mut_ptr() as i64;
        let mut helper = Asm::new();
        mov_imm64(&mut helper, 10, address);
        for (i, r) in [1, 2, 8, 9].into_iter().enumerate() {
            store_mem64_disp(&mut helper, 10, i as i32 * 8, r);
        }
        for i in 4..6 {
            load_from_rsp(&mut helper, RAX, (i + 1) * 8);
            store_mem64_disp(&mut helper, 10, i * 8, RAX);
        }
        mov_rr(&mut helper, RAX, 4);
        helper.extend_from_slice(&[0x48, 0x83, 0xE0, 15]); // and rax, 15
        store_mem64_disp(&mut helper, 10, 48, RAX);
        load_mem64_disp(&mut helper, RAX, 10, 64);
        store_mem64_disp(&mut helper, 10, 56, RAX); // recovery at helper entry
        leaf_return(&mut helper, RESULT);
        let helper = JitBuffer::new(&helper.finish().unwrap()).unwrap();

        let mut recovery = Asm::new();
        mov_imm64(&mut recovery, 10, address);
        store_mem64_disp(&mut recovery, 10, 64, 1); // record RCX recovery flag
        load_mem64_disp(&mut recovery, RAX, 10, 72);
        recovery.extend_from_slice(&[0x48, 0x83, 0xC0, 1]); // add rax, 1
        store_mem64_disp(&mut recovery, 10, 72, RAX); // number of toggles
        leaf_return(&mut recovery, 0xBAD);
        let recovery = JitBuffer::new(&recovery.finish().unwrap()).unwrap();

        let mut probe = Asm::new();
        mov_imm64(&mut probe, 10, address);
        load_mem64_disp(&mut probe, RAX, 10, 64);
        store_mem64_disp(&mut probe, 10, 80, RAX); // recovery at transfer probe
        mov_rr(&mut probe, RAX, 4);
        probe.extend_from_slice(&[0x48, 0x83, 0xE0, 15]);
        store_mem64_disp(&mut probe, 10, 88, RAX);
        leaf_return(&mut probe, u64::from(pending == Some(true)));
        let probe = JitBuffer::new(&probe.finish().unwrap()).unwrap();

        let mut caller = Asm::new();
        for r in [5, 6, 7] {
            push_reg(&mut caller, r);
        }
        // Fixed RBP allows the helper crossing to reserve transient stack space.
        // Extra locals also keep the pre-fix callee's home writes off saved regs.
        caller.extend_from_slice(&[0x48, 0x83, 0xEC, 64]);
        mov_rr(&mut caller, 5, 4);
        mov_imm64(&mut caller, RAX, 0xCAFE);
        for offset in [0, 56] {
            store_to_rsp(&mut caller, RAX, offset);
        }
        for (i, r) in [7, 6, 2, 1, 8, 9].into_iter().enumerate() {
            mov_imm64(&mut caller, r, FIRST_ARGUMENT as i64 + i as i64);
        }
        mov_imm64(&mut caller, RAX, helper.as_ptr() as i64);
        let exit = caller.label();
        emit_runtime_helper_call(
            &mut caller,
            if toggle { recovery.as_ptr() as u64 } else { 0 },
            pending.map(|_| NativeTransferCheck {
                pending_addr: probe.as_ptr() as u64,
                exit,
            }),
        );
        mov_imm64(&mut caller, 10, address);
        mov_imm64(&mut caller, 11, 1);
        store_mem64_disp(&mut caller, 10, 96, 11); // later side effect
        caller.bind(exit);
        mov_imm64(&mut caller, 10, address);
        for (i, offset) in [0, 56].into_iter().enumerate() {
            load_from_rsp(&mut caller, 11, offset);
            store_mem64_disp(&mut caller, 10, 104 + i as i32 * 8, 11);
        }
        caller.extend_from_slice(&[0x48, 0x8D, 0x65, 64]); // lea rsp, [rbp+64]
        for r in [7, 6, 5] {
            pop_reg(&mut caller, r);
        }
        caller.push(0xC3);
        let bytes = caller.finish().unwrap();
        let unwind = [1, 10, 5, 5, 10, 3, 7, 0x72, 3, 0x70, 2, 0x60, 1, 0x50, 0, 0];
        // SAFETY: the metadata describes the fixed frame above, including RBP,
        // RSI and RDI saves. The helper is a leaf and only clobbers volatile regs.
        let caller = unsafe { JitBuffer::new_with_windows_unwind(&bytes, &unwind) }.unwrap();
        let call: extern "C" fn() -> u64 = unsafe { std::mem::transmute(caller.as_ptr()) };
        assert_eq!(call(), RESULT);
        assert_eq!(
            &observed[..6],
            &[0, 1, 2, 3, 4, 5].map(|i| FIRST_ARGUMENT + i)
        );
        assert_eq!(observed[6], 8);
        assert_eq!(observed[7], if toggle { 0 } else { u64::MAX });
        assert_eq!(observed[8], if toggle { 1 } else { u64::MAX });
        assert_eq!(observed[9], if toggle { 2 } else { 0 });
        if pending.is_some() {
            assert_eq!(observed[10], if toggle { 1 } else { u64::MAX });
            assert_eq!(observed[11], 8);
        }
        assert_eq!(observed[12], u64::from(pending != Some(true)));
        assert_eq!(&observed[13..], &[0xCAFE, 0xCAFE]);
    }

    /// The real milestone: emit a T2 function and EXECUTE it. `() -> 12345` must
    /// return 12345 when called as a native C function.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn emitted_code_executes() {
        let mut mf = MachFunc {
            insts: vec![
                mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(12345)),
                mi(op::RET, vec![], vec![gpr(0)], None),
            ],
            ..MachFunc::default()
        };
        allocate(&mut mf).expect("regalloc2");
        let code = emit(&mf).expect("emit");
        let buf = egcl_rt::jit::JitBuffer::new(&code).expect("mmap exec");
        // SAFETY: the emitted code is a leaf `extern "C" fn() -> u64` that only
        // writes rax and returns, preserving callee-saved registers.
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), 12345, "T2-emitted function must return its constant");
    }

    /// More simultaneously-live values than the allocatable GPR set forces
    /// regalloc2 to split ranges and spill.  The emitted reload/store edits and
    /// spill frame must make the result indistinguishable from register-only
    /// execution.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn regalloc_spills_execute_correctly() {
        const N: u32 = 24;
        let mut mf = MachFunc::default();
        for i in 0..N {
            mf.insts
                .push(mi(op::MOV_IMM, vec![gpr(i)], vec![], Some((i + 1) as i64)));
        }
        let mut acc = gpr(0);
        let mut next = N;
        for i in 1..N {
            let out = gpr(next);
            next += 1;
            mf.insts
                .push(mi(op::ADD, vec![out], vec![acc, gpr(i)], None));
            acc = out;
        }
        mf.insts.push(mi(op::RET, vec![], vec![acc], None));

        allocate(&mut mf).expect("regalloc2");
        assert!(
            mf.num_spill_slots > 0,
            "test must create actual spill pressure"
        );
        assert!(
            mf.allocation_edits.iter().any(|edit| {
                matches!(edit.from, Location::Stack(_)) || matches!(edit.to, Location::Stack(_))
            }),
            "regalloc2 must provide spill/reload edits"
        );

        let code = emit(&mf).expect("spill-capable emission");
        assert!(
            contains(&code, &[0x41, 0x54]),
            "register pressure must exercise push r12"
        );
        assert!(
            contains(&code, &[0x41, 0x5C]),
            "every return must restore r12 with pop r12"
        );
        let buf = egcl_rt::jit::JitBuffer::new(&code).expect("mmap exec");
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), (1..=N as u64).sum::<u64>());
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(all(target_arch = "x86_64", unix))]
    static DEOPTED: AtomicBool = AtomicBool::new(false);
    #[cfg(all(target_arch = "x86_64", unix))]
    extern "C" fn mock_c2i_deopt() {
        DEOPTED.store(true, Ordering::SeqCst);
    }

    // Build and speculate `(lambda (x) (* x 5))` into single-guarded-FixnumMul IR.
    #[cfg(all(target_arch = "x86_64", unix))]
    fn speculated_mul5() -> crate::t2::ir::Function {
        use crate::t2::speculate::{speculate, SpecType};
        use egcl_rt::bytecode::{BytecodeFunction, Instr};
        use egcl_rt::value::EgclVal;
        let star = egcl_rt::symbols::intern("*");
        let bf = BytecodeFunction {
            code: vec![
                Instr::LoadLocal(0),
                Instr::Const(0),
                Instr::CallNamed {
                    sym: star,
                    nargs: 2,
                },
                Instr::Return,
            ],
            constants: vec![EgclVal::from_fixnum(5)],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: 1,
            max_stack: 2,
            arity: 1,
            name: "mul5".into(),
            params_form: egcl_rt::value::NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
        };
        let mut f = crate::t2::build::build_from_bytecode(&bf).expect("build");
        speculate(&mut f, &|bcp| {
            if bcp == 2 {
                Some(SpecType::Fixnum)
            } else {
                None
            }
        });
        f
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    fn string_layout_fn(char_at: bool) -> crate::t2::ir::Function {
        use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
        use crate::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
        };
        let mut f = Function::new(if char_at {
            "string-char"
        } else {
            "string-bytes"
        });
        let entry = f.entry();
        let string = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let mut args = vec![string];
        let mut locals = vec![ValueSource::Value {
            value: string,
            repr: ValueRepresentation::Tagged,
        }];
        if char_at {
            let index = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            args.push(index);
            locals.push(ValueSource::Value {
                value: index,
                repr: ValueRepresentation::Tagged,
            });
        }
        let fs = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals,
                stack: vec![],
            }],
            remat: vec![],
        });
        let (_, checked) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::Guard,
                args: vec![string],
                results: vec![],
                aux: AuxData::StringLayout,
                flags: InstFlags {
                    effectful: true,
                    guard: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: Some(fs),
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::STRING), ValueRepresentation::Tagged)],
        );
        args[0] = checked[0];
        let opcode = if char_at {
            Opcode::StringAsciiCharAt
        } else {
            Opcode::StringByteLength
        };
        let result_ty = if char_at {
            IRType::of(TypeBits::CHARACTER)
        } else {
            IRType::of(TypeBits::FIXNUM)
        };
        let (_, results) = f.push_inst(
            entry,
            InstData {
                opcode,
                args,
                results: vec![],
                aux: AuxData::None,
                flags: if char_at {
                    InstFlags {
                        effectful: true,
                        guard: true,
                        ..Default::default()
                    }
                } else {
                    InstFlags::default()
                },
                targets: vec![],
                frame_state: char_at.then_some(fs),
                source_pos: 0,
            },
            &[(result_ty, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![results[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    fn test_string(bytes: &[u8]) -> egcl_rt::value::EgclVal {
        use egcl_rt::object::{type_id, ObjectHeader};
        let total = (16 + bytes.len() + 7) & !7;
        let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
        unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            *(ptr as *mut ObjectHeader) =
                ObjectHeader::new(type_id::SIMPLE_BASE_STRING, (total / 8) as u16);
            *((ptr as *mut u64).add(1)) = bytes.len() as u64;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(16), bytes.len());
            egcl_rt::value::EgclVal::from_heap_ptr(ptr)
        }
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn string_layout_intrinsics_run_and_guard_every_unsafe_case() {
        use egcl_rt::value::EgclVal;

        let length_ir = string_layout_fn(false);
        crate::t2::verify::verify(&length_ir).expect("string length IR verifies");
        let length_code = emit_framed(
            &length_ir,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit string byte length")
        .code;
        let length_buf = egcl_rt::jit::JitBuffer::new(&length_code).unwrap();
        let length: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(length_buf.as_ptr()) };
        let mut frame = [test_string(b"abc").0, 0];
        DEOPTED.store(false, Ordering::SeqCst);
        assert_eq!(EgclVal(length(frame.as_mut_ptr())).as_fixnum(), 3);
        assert!(!DEOPTED.load(Ordering::SeqCst));
        frame[0] = EgclVal::from_fixnum(9).0;
        let _ = length(frame.as_mut_ptr());
        assert!(
            DEOPTED.load(Ordering::SeqCst),
            "wrong-type length must deopt"
        );
        let pathname_header = Box::new(egcl_rt::object::ObjectHeader::new(
            egcl_rt::object::type_id::PATHNAME,
            2,
        ));
        frame[0] = unsafe { EgclVal::from_heap_ptr(Box::into_raw(pathname_header).cast::<u8>()).0 };
        DEOPTED.store(false, Ordering::SeqCst);
        let _ = length(frame.as_mut_ptr());
        assert!(
            DEOPTED.load(Ordering::SeqCst),
            "wrong heap-object layout must deopt"
        );

        let char_ir = string_layout_fn(true);
        crate::t2::verify::verify(&char_ir).expect("string char IR verifies");
        let char_code = emit_framed(
            &char_ir,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit string char")
        .code;
        let char_buf = egcl_rt::jit::JitBuffer::new(&char_code).unwrap();
        let char_at: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(char_buf.as_ptr()) };

        let mut char_frame = [test_string(b"abc").0, EgclVal::from_fixnum(1).0, 0];
        DEOPTED.store(false, Ordering::SeqCst);
        assert_eq!(EgclVal(char_at(char_frame.as_mut_ptr())).as_char(), 'b');
        assert!(!DEOPTED.load(Ordering::SeqCst));

        for (string, index, why) in [
            (test_string(b""), 0, "empty string"),
            (test_string(b"abc"), 3, "upper bound"),
            (test_string(b"abc"), -1, "negative index"),
            (test_string("é".as_bytes()), 0, "non-ASCII UTF-8"),
            (
                test_string("éa".as_bytes()),
                2,
                "ASCII byte after a multibyte prefix",
            ),
        ] {
            char_frame[0] = string.0;
            char_frame[1] = EgclVal::from_fixnum(index).0;
            DEOPTED.store(false, Ordering::SeqCst);
            let _ = char_at(char_frame.as_mut_ptr());
            assert!(DEOPTED.load(Ordering::SeqCst), "{why} must deopt");
        }
    }

    /// The framed-emitter milestone: a T2-emitted `(* x 5)` runs via the
    /// interpreter frame ABI — a fixnum arg gives `x*5` (fast path, operand stack
    /// gone), a float arg trips the guard and calls the deopt routine.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn framed_fixnum_mul_runs_and_deopts() {
        use egcl_rt::value::EgclVal;
        let f = speculated_mul5();
        let framed = emit_framed(
            &f,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit_framed");
        assert!(
            framed.has_deopt,
            "a speculative fixnum guard must make the artifact deopt-capable"
        );
        let code = framed.code;
        let buf = egcl_rt::jit::JitBuffer::new(&code).expect("mmap");
        // extern "C" fn(*mut u64) -> u64 : rdi = frame slots, returns rax.
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

        // Fixnum arg 7 in slot 0 → fast path → 7*5 = 35.
        let mut frame = [EgclVal::from_fixnum(7).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let r = func(frame.as_mut_ptr());
        assert!(!DEOPTED.load(Ordering::SeqCst), "fixnum arg must not deopt");
        assert_eq!(EgclVal(r).as_fixnum(), 35, "T2 must compute 7*5 = 35");

        // Float arg → guard fails → deopt routine called.
        let mut frame = [EgclVal::from_single_float(2.0).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let _ = func(frame.as_mut_ptr());
        assert!(
            DEOPTED.load(Ordering::SeqCst),
            "a float operand must deopt to the interpreter"
        );
    }

    /// Build post-speculation IR for `(x) -> x * c` where the param `x` carries
    /// the given inferred range — i.e. what inlining/the caller ABI would supply.
    // Its callers are the x86-64 range tests below; elsewhere it is unused.
    #[cfg(all(target_arch = "x86_64", unix))]
    pub(super) fn build_mul_ranged(
        c: i64,
        range: Option<crate::t2::ir::Range>,
    ) -> crate::t2::ir::Function {
        use crate::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
        };
        let mut f = Function::new("m");
        let entry = f.entry();
        let xty = IRType {
            bits: TypeBits::FIXNUM,
            range,
            class_id: None,
        };
        let x = f.add_block_param(entry, xty, ValueRepresentation::Tagged);
        let (_ci, cres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(c),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let (_mi, mres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::FixnumMul,
                args: vec![x, cres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![mres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// Range-proven-safe `* 5` strength-reduces to a single `lea` with no overflow
    /// deopt — matching SBCL's optimised form — and still executes to x*5.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn strength_reduces_to_lea_when_range_is_safe() {
        use egcl_rt::value::EgclVal;
        let f = build_mul_ranged(5, Some(crate::t2::ir::Range { lo: 0, hi: 100 }));
        let framed = emit_framed(
            &f,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit");
        assert!(
            framed.has_deopt,
            "range proof removes overflow deopt, but the argument tag guard remains"
        );
        let code = framed.code;
        // `lea rax,[rcx+rcx*4]` = 48 8D 04 89 ; and NO overflow branch (0F 80).
        assert!(code.contains(&0x8D), "must emit lea for a range-safe *5");
        assert!(
            !contains(&code, &[0x0F, 0x80]),
            "range proves no overflow → no jo deopt"
        );
        let buf = egcl_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let mut frame = [EgclVal::from_fixnum(7).0, 0u64, 0u64];
        assert_eq!(
            EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
            35,
            "lea *5 of 7 = 35"
        );
    }

    /// Without a range, the overflow-detecting `imul + jo` is kept (correctness
    /// over speed — an unbounded fixnum could overflow to a bignum).
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn keeps_imul_and_overflow_check_when_range_unknown() {
        use egcl_rt::value::EgclVal;
        let f = build_mul_ranged(5, None);
        let framed = emit_framed(
            &f,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit");
        assert!(
            framed.has_deopt,
            "the emitted overflow edge must make the artifact deopt-capable"
        );
        let code = framed.code;
        assert!(code.contains(&0x69), "unknown range → imul r64,r64,5");
        assert!(
            contains(&code, &[0x0F, 0x80]),
            "unknown range → keep the jo overflow deopt"
        );
        let buf = egcl_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let mut frame = [EgclVal::from_fixnum(7).0, 0u64, 0u64];
        assert_eq!(
            EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
            35,
            "imul *5 of 7 = 35"
        );
    }

    /// A genuinely total T2 body has no edge to either deopt stub and is safe
    /// for a native caller that cannot process a callee deopt mid-flight.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn identity_has_no_deopt_edge() {
        use crate::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
        };

        let mut f = Function::new("identity");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![x],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );

        let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).expect("emit identity");
        assert!(
            !framed.has_deopt,
            "identity has no guard branch and must be direct-call eligible"
        );
    }

    /// OSR checks can be the only deoptimising operation in an otherwise total
    /// function. Preserve their metadata and give their callback a call frame.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn osr_only_guard_reports_deopt_and_reconstructs_its_frame() {
        use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
        use crate::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, OsrEntry, TypeBits,
            ValueRepresentation,
        };
        use egcl_rt::value::EgclVal;

        extern "C" fn reconstruct(nscopes: u64, nwords: u64, words: *const u64, _: u64) -> u64 {
            if nscopes != 1 || nwords != 5 {
                return 0;
            }
            let words = unsafe { std::slice::from_raw_parts(words, nwords as usize) };
            if words != [0, 0, 1, 0, EgclVal::from_single_float(2.0).0] {
                return 0;
            }
            EgclVal::from_fixnum(99).0
        }

        let mut f = Function::new("osr-only-guard");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![x],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        let frame_state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: x,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(OsrEntry {
            bcp: 0,
            block: entry,
            frame_state,
            checks: vec![(x, IRType::of(TypeBits::FIXNUM))],
        });
        crate::t2::verify::verify(&f).expect("OSR identity IR verifies");
        let framed = emit_framed(
            &f, 0, reconstruct as *const () as usize as u64, 0, 0, 0, 0, 0, 0, None,
        )
        .expect("emit OSR identity");
        assert_eq!(framed.osr_entries.len(), 1, "checked OSR entry is emitted");
        assert!(
            framed.has_deopt,
            "an OSR guard needs deopt source snapshots even without instruction guards"
        );
        let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
        let osr: extern "C" fn(*mut u64) -> u64 = unsafe {
            std::mem::transmute(buf.as_ptr().add(framed.osr_entries[0].1))
        };
        let mut slots = [EgclVal::from_fixnum(7).0];
        assert_eq!(osr(slots.as_mut_ptr()), slots[0]);
        slots[0] = EgclVal::from_single_float(2.0).0;
        assert_eq!(osr(slots.as_mut_ptr()), EgclVal::from_fixnum(99).0);
    }

    /// Compiled-caller ABI: a compiled caller places args in registers and calls
    /// the `compiled_entry`, skipping the frame load. Here inline asm invokes it
    /// directly — arg0 in rcx, result in rax — proving the register entry runs the
    /// body with no interpreter frame (the memory load `mov rcx,[rdi]` is gone).
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn compiled_entry_takes_register_args() {
        use egcl_rt::value::EgclVal;
        let f = build_mul_ranged(5, Some(crate::t2::ir::Range { lo: 0, hi: 100 }));
        let framed = emit_framed(
            &f,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit");
        assert!(
            framed.compiled_entry > 0,
            "a register entry must sit past the frame-load prologue"
        );
        let buf = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
        let entry = buf.as_ptr() as usize + framed.compiled_entry;
        let x = EgclVal::from_fixnum(7).0;
        let result: u64;
        // SAFETY: the compiled entry is a leaf reading arg0 from rcx and writing
        // rax; every caller-saved GPR it may touch is marked clobbered.
        unsafe {
            core::arch::asm!(
                "call {e}",
                e = in(reg) entry,
                inout("rcx") x => _,
                lateout("rax") result,
                lateout("rdx") _,
                lateout("rsi") _,
                lateout("rdi") _,
                lateout("r8") _,
                lateout("r9") _,
                lateout("r10") _,
                lateout("r11") _,
            );
        }
        assert_eq!(
            EgclVal(result).as_fixnum(),
            35,
            "compiled entry: 7*5 = 35 from a register arg"
        );
    }

    /// Build `(x) -> x <op> c` as a float-speculated op: single-float param `x`,
    /// a fixnum constant `c` (coerced by contagion), guard-flagged float op.
    #[cfg(all(target_arch = "x86_64", unix))]
    fn build_float_arith(op: crate::t2::ir::Opcode, c: i64) -> crate::t2::ir::Function {
        use crate::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
        };
        let mut f = Function::new("m");
        let entry = f.entry();
        let x = f.add_block_param(
            entry,
            IRType::of(TypeBits::SINGLE_FLOAT),
            ValueRepresentation::Tagged,
        );
        let (_c, cres) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(c),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let (_m, mres) = f.push_inst(
            entry,
            InstData {
                opcode: op,
                args: vec![x, cres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(
                IRType::of(TypeBits::SINGLE_FLOAT),
                ValueRepresentation::Tagged,
            )],
        );
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![mres[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        f
    }

    /// The float second specialization: a float-speculated `(* x 5)` runs the XMM
    /// path — a single-float arg gives `x*5.0` (the fixnum constant coerced), and a
    /// fixnum arg trips the single-float guard and deopts.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn float_mul_runs_and_deopts() {
        use egcl_rt::value::EgclVal;
        let f = build_float_arith(crate::t2::ir::Opcode::FloatMul, 5);
        let code = emit_framed(
            &f,
            mock_c2i_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit")
        .code;
        let buf = egcl_rt::jit::JitBuffer::new(&code).unwrap();
        let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

        let mut frame = [EgclVal::from_single_float(2.0).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let r = func(frame.as_mut_ptr());
        assert!(
            !DEOPTED.load(Ordering::SeqCst),
            "a single-float arg must not deopt"
        );
        assert_eq!(
            EgclVal(r).as_single_float(),
            10.0,
            "2.0 * 5 = 10.0 (fixnum coerced)"
        );

        let mut frame = [EgclVal::from_fixnum(2).0, 0u64, 0u64];
        DEOPTED.store(false, Ordering::SeqCst);
        let _ = func(frame.as_mut_ptr());
        assert!(
            DEOPTED.load(Ordering::SeqCst),
            "a fixnum operand must deopt (guard is single-float)"
        );
    }

    /// A small computation `() -> 7 + 5 = 12`, executed.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn emitted_addition_executes() {
        let mut mf = MachFunc {
            insts: vec![
                mi(op::MOV_IMM, vec![gpr(0)], vec![], Some(7)),
                mi(op::MOV_IMM, vec![gpr(1)], vec![], Some(5)),
                mi(op::ADD, vec![gpr(2)], vec![gpr(0), gpr(1)], None),
                mi(op::RET, vec![], vec![gpr(2)], None),
            ],
            ..MachFunc::default()
        };
        allocate(&mut mf).expect("regalloc2");
        let code = emit(&mf).expect("emit");
        let buf = egcl_rt::jit::JitBuffer::new(&code).expect("mmap exec");
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), 12, "7 + 5 must execute to 12");
    }
}
