// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A tiny label-based x86-64 assembler, shared by the T1 baseline emitter
//! (`egcl` crate) and the T2 optimising emitter (`egcl-compiler`). It lives in
//! `egcl-rt` so both tiers emit through the same assembler without a dependency
//! cycle (same rationale as the relocated bytecode types).
//!
//! AArch64 selects its own branch encodings here. Other hosts retain the x86
//! encoding API so the shared T2 emitter can compile; System Z execution uses
//! the separate `asm_s390x` assembler.
//!
//! The emitter used to carry three parallel hand-rolled fixup tables — branch
//! targets (`offsets`/`patches`), guard→deopt-stub sites (`deopt_sites`), and
//! the OSR entry stubs — each recomputing `rel32 = target - (site + 4)` and
//! poking four bytes back into the buffer by hand. Every one of those is the
//! same operation: "emit a branch now to a position I'll know later." This type
//! makes that a single primitive — a `Label` you can `bind` and `jmp`/`jcc` to
//! in any order — and resolves every displacement once, in `finish`, with the
//! range checks in exactly one place.
//!
//! It owns the code buffer, so raw instruction bytes go through `emit`/`push`
//! while control flow goes through the label API; the two share one buffer so a
//! label's recorded offset is directly the displacement target.
//!
//! ## Why this shape survives the optimizing compiler
//!
//! The `Label`/`bind`/`jmp`/`jcc` protocol is the part that is backend-agnostic.
//! Today `bind` records a byte offset and a branch becomes a patched `rel32`.
//! When the optimizing compiler lands, the same walk targets an IR instead: a
//! `Label` becomes a basic-block header, `bind` starts that block, and a branch
//! becomes a CFG edge + block terminator. This assembler is not replaced by that
//! work — it becomes its final lowering stage (IR → passes → these bytes). So
//! the label API is deliberately written as the seam a future `IrBuilder` can
//! share, not as an x86 detail.

/// A branch target. Allocate with [`Asm::label`], mark its position with
/// [`Asm::bind`], and reference it from [`Asm::jmp`]/[`Asm::jcc`] before or
/// after it is bound — forward and backward branches are handled identically.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Label(usize);

/// x86-64 condition codes, as the second byte of a two-byte `0F 8x` conditional
/// near jump. Only the codes the emitter needs are listed; add as required.
#[derive(Clone, Copy, Debug)]
pub enum Cc {
    /// ZF=1 (equal / zero).
    E,
    /// ZF=0 (not equal / nonzero).
    Ne,
    /// OF=1 (signed overflow) — the fixnum-overflow guard edge.
    O,
    /// OF=0 (no signed overflow). Exists so that `inverse` is total: a partial
    /// inversion that quietly returns the condition unchanged is a wrong-branch
    /// bug waiting for its first caller.
    No,
    /// Signed less (SF≠OF).
    L,
    /// Signed less-or-equal (ZF=1 or SF≠OF).
    Le,
    /// Signed greater (ZF=0 and SF=OF).
    G,
    /// Signed greater-or-equal (SF=OF).
    Ge,
}

impl Cc {
    /// The AArch64 condition field (C4.1, `cond`), for `B.cond` and the
    /// conditional-select family. Every code the emitter uses has a direct
    /// counterpart; `O` is AArch64's `VS`.
    pub(crate) fn a64_cond(self) -> u32 {
        match self {
            Cc::E => 0b0000,  // EQ
            Cc::Ne => 0b0001, // NE
            Cc::O => 0b0110,  // VS
            Cc::No => 0b0111, // VC
            Cc::Ge => 0b1010, // GE
            Cc::L => 0b1011,  // LT
            Cc::G => 0b1100,  // GT
            Cc::Le => 0b1101, // LE
        }
    }

    /// The POWER condition-register bit this tests, and whether it branches when
    /// that bit is set.
    ///
    /// POWER has no flags register: a compare writes one of eight condition
    /// register FIELDS, each holding LT/GT/EQ/SO, and `bc` names a bit and a
    /// sense. That looked at first like a reason this shared enum could not serve
    /// POWER — but the mapping is exact, not approximate. The architecture's eight
    /// branch aliases disassemble as four bits times two senses:
    ///
    /// ```text
    ///   beq -> bt 2   bne -> bf 2      (bit 2 = EQ)
    ///   blt -> bt 0   bge -> bf 0      (bit 0 = LT)
    ///   bgt -> bt 1   ble -> bf 1      (bit 1 = GT)
    ///   bso -> bt 3   bns -> bf 3      (bit 3 = SO)
    /// ```
    ///
    /// which is exactly this enum's eight variants, with [`Cc::inverse`]'s pairs
    /// being exactly the sense flips. So POWER reuses `Cc` as AArch64 does, rather
    /// than keeping a private vocabulary as the System Z assembler does with its
    /// 4-bit masks.
    ///
    /// The field is the caller's to choose. A backend that only ever compares into
    /// CR0 can ignore the distinction; using several fields is what lets POWER keep
    /// more than one comparison live, and is an optimisation, not a requirement.
    ///
    /// `O`/`No` are a trap here and should not be used on POWER. The SO bit is
    /// copied from `XER[SO]`, which is STICKY — it stays set until explicitly
    /// cleared — and the instruction that cheaply cleared it, `mcrxr`, does not
    /// exist in the ISA level ppc64le targets (an assembler rejects it outright).
    /// An overflow guard there should compute overflow arithmetically instead —
    /// `mulhd` against the low half's sign for multiply, the sign test for add and
    /// subtract — and branch on an ordinary compare, exactly as AArch64 must use
    /// `SMULH` because it has no flag-setting multiply.
    #[cfg_attr(
        not(target_arch = "powerpc64"),
        allow(
            dead_code,
            reason = "consumed by the ppc64le encoder (bliss-jflix); pinned by test"
        )
    )]
    pub(crate) fn ppc_condition(self) -> (u32, bool) {
        match self {
            Cc::L => (0, true),
            Cc::Ge => (0, false),
            Cc::G => (1, true),
            Cc::Le => (1, false),
            Cc::E => (2, true),
            Cc::Ne => (2, false),
            Cc::O => (3, true),
            Cc::No => (3, false),
        }
    }

    /// The `0F`-prefixed opcode byte for the near (`rel32`) form.
    #[cfg(not(target_arch = "aarch64"))]
    fn opcode2(self) -> u8 {
        match self {
            Cc::E => 0x84,
            Cc::Ne => 0x85,
            Cc::O => 0x80,
            Cc::No => 0x81,
            Cc::L => 0x8C,
            Cc::Ge => 0x8D,
            Cc::Le => 0x8E,
            Cc::G => 0x8F,
        }
    }

    /// The condition that is true exactly when this one is false — for emitting
    /// the fall-through/taken split of a two-way branch.
    pub fn inverse(self) -> Cc {
        match self {
            Cc::E => Cc::Ne,
            Cc::Ne => Cc::E,
            Cc::O => Cc::No,
            Cc::No => Cc::O,
            Cc::L => Cc::Ge,
            Cc::Ge => Cc::L,
            Cc::Le => Cc::G,
            Cc::G => Cc::Le,
        }
    }
}

/// Which displacement field a fixup patches. The architectures disagree about
/// both the field's width and what the displacement is measured from, so the
/// kind travels with the site rather than being assumed.
#[derive(Clone, Copy)]
enum FixupKind {
    /// x86-64 `rel32`, measured from the END of the four-byte field.
    #[cfg(not(target_arch = "aarch64"))]
    Rel32,
    /// A64 `imm26` (`B`, `BL`): `(target - instruction) / 4`, ±128 MiB.
    #[cfg(target_arch = "aarch64")]
    Imm26,
    /// A64 `imm19` (`B.cond`): `(target - instruction) / 4`, ±1 MiB — a much
    /// tighter reach than the unconditional form, and the one a large function
    /// runs out of first.
    #[cfg(target_arch = "aarch64")]
    Imm19,
}

/// A pending displacement: where to patch, what shape, and the label to reach.
/// `site` is the four-byte field on x86-64 and the instruction itself on A64,
/// because that is what each displacement is relative to.
struct Fixup {
    site: usize,
    label: Label,
    kind: FixupKind,
}

/// AArch64 instruction encoding. Compiled on every host — the encoders are pure
/// functions and their tests are the only thing pinning them to the manual, so
/// they must not be reachable only from an AArch64 build.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
pub mod a64;

/// OR a resolved displacement into the instruction already at `site`. A free
/// function rather than a method: the fixup loop borrows `self.fixups`, so a
/// `&mut self` method would conflict with it.
#[cfg(target_arch = "aarch64")]
fn patch_a64(code: &mut [u8], site: usize, bits: u32) {
    let word = u32::from_le_bytes(code[site..site + 4].try_into().unwrap());
    code[site..site + 4].copy_from_slice(&(word | bits).to_le_bytes());
}

/// A64 branch displacements count INSTRUCTIONS, not bytes, and are relative to
/// the branch itself. Returns `None` — the emitter's bail-on-anything-odd
/// contract — for a misaligned target or one out of the field's signed range.
#[cfg(target_arch = "aarch64")]
fn branch_words(target: i64, site: usize, bits: u32) -> Option<u32> {
    let rel = target - site as i64;
    if rel % 4 != 0 {
        return None;
    }
    let words = rel / 4;
    let limit = 1i64 << (bits - 1);
    if words < -limit || words >= limit {
        return None;
    }
    Some(words as u32)
}

/// A growable code buffer with deferred branch resolution.
pub struct Asm {
    code: Vec<u8>,
    /// Bound offset of each label, or `None` while still forward-referenced.
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl Default for Asm {
    fn default() -> Self {
        Self::new()
    }
}

impl Asm {
    pub fn new() -> Self {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    /// Current end of the buffer — the offset the next emitted byte will take.
    #[inline]
    pub fn here(&self) -> usize {
        self.code.len()
    }

    /// Append raw instruction bytes. Named to mirror `Vec` so a byte-emitting
    /// routine can target an `Asm` with a plain variable rename; [`here`] is the
    /// same idea for `.len()`.
    ///
    /// [`here`]: Self::here
    #[inline]
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// Alias of [`here`](Self::here), for `Vec`-style call sites.
    #[inline]
    pub fn len(&self) -> usize {
        self.code.len()
    }

    /// Return whether no instruction bytes have been emitted yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }

    /// Append a single raw byte.
    #[inline]
    pub fn push(&mut self, b: u8) {
        self.code.push(b);
    }

    /// Overwrite a single already-emitted byte. For short intra-instruction
    /// `rel8` jumps that a routine resolves itself; the [`Label`] API handles the
    /// `rel32` control flow between instructions.
    #[inline]
    pub fn patch_u8(&mut self, at: usize, b: u8) {
        self.code[at] = b;
    }

    /// Allocate a fresh, unbound label.
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// The buffer offset a bound label resolves to, or `None` if unbound. Offsets
    /// are final: all control flow uses fixed `rel32` displacements, so `finish`
    /// patches in place without moving code (no jump relaxation). Used to build
    /// the bytecode→native position map for the tiered-JIT viewer (bliss-zmmb).
    #[inline]
    pub fn label_offset(&self, l: Label) -> Option<usize> {
        self.labels[l.0]
    }

    /// Return whether emitted control flow references `l`.
    ///
    /// This lets an emitter distinguish an allocated cold stub from one the
    /// generated hot path can actually reach.
    #[inline]
    pub fn label_is_referenced(&self, l: Label) -> bool {
        self.fixups.iter().any(|fixup| fixup.label == l)
    }

    /// Bind a label to the current buffer position. A label must be bound
    /// exactly once before [`finish`](Self::finish); binding twice is a codegen
    /// bug and panics rather than silently resolving to the wrong site.
    pub fn bind(&mut self, l: Label) {
        debug_assert!(self.labels[l.0].is_none(), "label {l:?} bound twice");
        self.labels[l.0] = Some(self.code.len());
    }

    /// `jmp rel32` to `l` (opcode `E9`), reserving the displacement for `finish`.
    #[cfg(not(target_arch = "aarch64"))]
    pub fn jmp(&mut self, l: Label) {
        self.code.push(0xE9);
        self.reserve_rel32(l);
    }

    /// `jcc rel32` to `l` (opcode `0F 8x`), reserving the displacement.
    #[cfg(not(target_arch = "aarch64"))]
    pub fn jcc(&mut self, cc: Cc, l: Label) {
        self.code.extend_from_slice(&[0x0F, cc.opcode2()]);
        self.reserve_rel32(l);
    }

    /// `call rel32` to `l` (opcode `E8`) — a direct near call to another point in
    /// this buffer (e.g. a self-recursive call to the function's own entry).
    #[cfg(not(target_arch = "aarch64"))]
    pub fn call(&mut self, l: Label) {
        self.code.push(0xE8);
        self.reserve_rel32(l);
    }

    /// Append one A64 instruction word. Instructions are fixed-width and
    /// little-endian, so this is the only way code enters the buffer between
    /// branches.
    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    pub fn word(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }

    /// Append a run of A64 instruction words, e.g. a constant materialisation.
    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    pub fn words(&mut self, words: &[u32]) {
        for word in words {
            self.word(*word);
        }
    }

    /// Reserve a 4-byte `rel32` slot at the current position, to be patched to
    /// reach `l` in `finish`.
    #[cfg(not(target_arch = "aarch64"))]
    fn reserve_rel32(&mut self, l: Label) {
        self.fixups.push(Fixup {
            site: self.code.len(),
            label: l,
            kind: FixupKind::Rel32,
        });
        self.code.extend_from_slice(&[0, 0, 0, 0]);
    }

    /// `B` to `l` (spec §4.7.5.1). Every A64 branch is one 4-byte instruction
    /// whose displacement is relative to the instruction itself, so the whole
    /// instruction is reserved rather than a field inside it.
    #[cfg(target_arch = "aarch64")]
    pub fn jmp(&mut self, l: Label) {
        self.reserve_a64(l, 0x1400_0000, FixupKind::Imm26);
    }

    /// `B.cond` to `l`.
    #[cfg(target_arch = "aarch64")]
    pub fn jcc(&mut self, cc: Cc, l: Label) {
        self.reserve_a64(l, 0x5400_0000 | cc.a64_cond(), FixupKind::Imm19);
    }

    /// `BL` to `l` — a direct call within this buffer.
    #[cfg(target_arch = "aarch64")]
    pub fn call(&mut self, l: Label) {
        self.reserve_a64(l, 0x9400_0000, FixupKind::Imm26);
    }

    #[cfg(target_arch = "aarch64")]
    fn reserve_a64(&mut self, l: Label, template: u32, kind: FixupKind) {
        self.fixups.push(Fixup {
            site: self.code.len(),
            label: l,
            kind,
        });
        self.code.extend_from_slice(&template.to_le_bytes());
    }

    /// Resolve every branch and return the finished machine code. Returns `None`
    /// — matching the emitter's bail-on-anything-odd contract — if any branch
    /// targets an unbound label or lies farther than `rel32` can reach.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for fx in &self.fixups {
            let target = (*self.labels.get(fx.label.0)?)? as i64;
            match fx.kind {
                #[cfg(not(target_arch = "aarch64"))]
                FixupKind::Rel32 => {
                    let rel = target - (fx.site as i64 + 4);
                    let rel32 = i32::try_from(rel).ok()?;
                    self.code[fx.site..fx.site + 4].copy_from_slice(&rel32.to_le_bytes());
                }
                #[cfg(target_arch = "aarch64")]
                FixupKind::Imm26 => {
                    let imm = branch_words(target, fx.site, 26)?;
                    patch_a64(&mut self.code, fx.site, imm & 0x03FF_FFFF);
                }
                #[cfg(target_arch = "aarch64")]
                FixupKind::Imm19 => {
                    let imm = branch_words(target, fx.site, 19)?;
                    patch_a64(&mut self.code, fx.site, (imm & 0x0007_FFFF) << 5);
                }
            }
        }
        Some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Decode the little-endian rel32 that starts at `site`.
    #[cfg(not(target_arch = "aarch64"))]
    fn rel32_at(code: &[u8], site: usize) -> i32 {
        i32::from_le_bytes(code[site..site + 4].try_into().unwrap())
    }

    /// The eight conditions are four POWER condition-register bits times two
    /// senses, and inversion is exactly the sense flip. The ppc backend's branch
    /// encoding rests on both halves of that, so both are pinned here.
    #[test]
    fn ppc_conditions_are_four_bits_times_two_senses() {
        let all = [Cc::E, Cc::Ne, Cc::L, Cc::Ge, Cc::G, Cc::Le, Cc::O, Cc::No];
        let mut seen: Vec<(u32, bool)> = all.iter().map(|cc| cc.ppc_condition()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            8,
            "every condition must be a distinct (bit, sense)"
        );
        assert!(
            seen.iter().all(|(bit, _)| *bit < 4),
            "a condition register field holds only bits 0..3"
        );
        for cc in all {
            let (bit, sense) = cc.ppc_condition();
            let (inverse_bit, inverse_sense) = cc.inverse().ppc_condition();
            assert_eq!(bit, inverse_bit, "{cc:?}: inversion must keep the bit");
            assert_ne!(
                sense, inverse_sense,
                "{cc:?}: inversion must flip the sense"
            );
        }
        // The architecture's own alias names, as an assembler disassembles them.
        assert_eq!(Cc::E.ppc_condition(), (2, true), "beq is bt 2");
        assert_eq!(Cc::Ne.ppc_condition(), (2, false), "bne is bf 2");
        assert_eq!(Cc::L.ppc_condition(), (0, true), "blt is bt 0");
        assert_eq!(Cc::G.ppc_condition(), (1, true), "bgt is bt 1");
        assert_eq!(Cc::O.ppc_condition(), (3, true), "bso is bt 3");
    }

    #[test]
    #[cfg(not(target_arch = "aarch64"))]
    fn forward_branch_resolves_to_signed_offset() {
        let mut a = Asm::new();
        let done = a.label();
        // jmp done ; <2 filler bytes> ; done:
        a.jmp(done); //   E9 [rel32] occupies bytes 0..5
        let site = 1; // rel32 slot begins right after the E9
        a.extend_from_slice(&[0x90, 0x90]); // two nops between the branch and its target
        a.bind(done);
        let code = a.finish().unwrap();
        // target = 7 (5-byte jmp + 2 nops); rel = 7 - (1 + 4) = 2.
        assert_eq!(rel32_at(&code, site), 2);
        assert_eq!(code[0], 0xE9);
    }

    #[test]
    #[cfg(not(target_arch = "aarch64"))]
    fn backward_branch_is_negative() {
        let mut a = Asm::new();
        let top = a.label();
        a.bind(top); // top: at offset 0
        a.extend_from_slice(&[0x90, 0x90, 0x90]); // 3 bytes of body
        a.jmp(top); // jmp top at offset 3; rel32 slot at 4
        let code = a.finish().unwrap();
        // rel = 0 - (4 + 4) = -8.
        assert_eq!(rel32_at(&code, 4), -8);
    }

    #[test]
    #[cfg(not(target_arch = "aarch64"))]
    fn jcc_emits_two_byte_opcode() {
        let mut a = Asm::new();
        let l = a.label();
        a.jcc(Cc::O, l); // 0F 80 [rel32]
        a.extend_from_slice(&[0x90]);
        a.bind(l);
        let code = a.finish().unwrap();
        assert_eq!(&code[0..2], &[0x0F, 0x80]);
        // target = 7 (6-byte jcc + 1 nop); rel = 7 - (2 + 4) = 1.
        assert_eq!(rel32_at(&code, 2), 1);
    }

    #[test]
    fn label_reference_tracks_emitted_control_flow() {
        let mut a = Asm::new();
        let referenced = a.label();
        let unreferenced = a.label();
        a.jcc(Cc::Ne, referenced);
        assert!(a.label_is_referenced(referenced));
        assert!(!a.label_is_referenced(unreferenced));
        a.bind(referenced);
        a.bind(unreferenced);
        assert!(a.finish().is_some());
    }

    #[test]
    #[cfg(not(target_arch = "aarch64"))]
    fn many_labels_to_one_target_all_resolve() {
        // Mirrors several guards sharing one deopt stub.
        let mut a = Asm::new();
        let stub = a.label();
        a.jcc(Cc::Ne, stub); // guard 1
        a.jcc(Cc::O, stub); // guard 2
        a.jmp(stub); // unconditional into the stub region
        a.bind(stub);
        a.extend_from_slice(&[0xC3]); // ret
        let code = a.finish().unwrap();
        // Layout: jcc=0F 8x+rel32 (6 bytes), jmp=E9+rel32 (5 bytes); stub @17.
        // Guard 1 (0F 85 @0) slot @2 → 17-(2+4)=11.
        assert_eq!(rel32_at(&code, 2), 11);
        // Guard 2 (0F 80 @6) slot @8 → 17-(8+4)=5.
        assert_eq!(rel32_at(&code, 8), 5);
        // jmp (E9 @12) slot @13 → 17-(13+4)=0.
        assert_eq!(rel32_at(&code, 13), 0);
        assert_eq!(*code.last().unwrap(), 0xC3);
    }

    #[test]
    fn unbound_label_makes_finish_bail() {
        let mut a = Asm::new();
        let never = a.label();
        a.jmp(never);
        assert!(a.finish().is_none());
    }

    /// Decode the 4-byte instruction at `site`.
    #[cfg(target_arch = "aarch64")]
    fn word_at(code: &[u8], site: usize) -> u32 {
        u32::from_le_bytes(code[site..site + 4].try_into().unwrap())
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn forward_b_counts_instructions_from_itself() {
        let mut a = Asm::new();
        let done = a.label();
        a.jmp(done); // B occupies bytes 0..4
        a.extend_from_slice(&0xd503_201fu32.to_le_bytes()); // NOP
        a.extend_from_slice(&0xd503_201fu32.to_le_bytes()); // NOP
        a.bind(done);
        let code = a.finish().unwrap();
        // target = 12; rel = 12 - 0 = 12 bytes = 3 instructions.
        assert_eq!(word_at(&code, 0), 0x1400_0000 | 3);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn backward_b_is_negative_in_two_s_complement() {
        let mut a = Asm::new();
        let top = a.label();
        a.bind(top);
        for _ in 0..3 {
            a.extend_from_slice(&0xd503_201fu32.to_le_bytes());
        }
        a.jmp(top); // at offset 12; rel = -12 = -3 instructions
        let code = a.finish().unwrap();
        assert_eq!(
            word_at(&code, 12),
            0x1400_0000 | (((-3i32) as u32) & 0x03FF_FFFF)
        );
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn bcond_places_the_condition_and_imm19() {
        let mut a = Asm::new();
        let l = a.label();
        a.jcc(Cc::O, l); // B.VS
        a.extend_from_slice(&0xd503_201fu32.to_le_bytes());
        a.bind(l);
        let code = a.finish().unwrap();
        // cond=VS=0b0110 in bits 0..3, imm19=2 instructions in bits 5..23.
        assert_eq!(word_at(&code, 0), 0x5400_0000 | (2 << 5) | 0b0110);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn call_is_bl_not_b() {
        let mut a = Asm::new();
        let entry = a.label();
        a.bind(entry);
        a.call(entry); // self-recursive call at offset 0... bound at 0
        let code = a.finish().unwrap();
        assert_eq!(word_at(&code, 0), 0x9400_0000);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn misaligned_target_bails() {
        let mut a = Asm::new();
        let odd = a.label();
        a.jmp(odd);
        a.push(0x00); // pushes the label off a 4-byte boundary
        a.bind(odd);
        assert!(a.finish().is_none());
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn bcond_runs_out_of_reach_before_b_does() {
        // B.cond reaches ±1 MiB and B reaches ±128 MiB, so a span that is fine
        // for one is not for the other. A T1 function long enough to hit this
        // must bail rather than emit a branch that lands somewhere else.
        let span = 1usize << 20;
        let mut near = Asm::new();
        let far = near.label();
        near.jcc(Cc::E, far);
        near.extend_from_slice(&vec![0u8; span]);
        near.bind(far);
        assert!(near.finish().is_none());

        let mut wide = Asm::new();
        let far = wide.label();
        wide.jmp(far);
        wide.extend_from_slice(&vec![0u8; span]);
        wide.bind(far);
        assert!(wide.finish().is_some());
    }
}
