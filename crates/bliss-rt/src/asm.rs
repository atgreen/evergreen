//! A tiny label-based x86-64 assembler, shared by the T1 baseline emitter
//! (`bliss` crate) and the T2 optimising emitter (`bliss-compiler`). It lives in
//! `bliss-rt` so both tiers emit through the same assembler without a dependency
//! cycle (same rationale as the relocated bytecode types).
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
    /// The `0F`-prefixed opcode byte for the near (`rel32`) form.
    fn opcode2(self) -> u8 {
        match self {
            Cc::E => 0x84,
            Cc::Ne => 0x85,
            Cc::O => 0x80,
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
            Cc::O => Cc::O, // no NO code listed; unused for inversion
            Cc::L => Cc::Ge,
            Cc::Ge => Cc::L,
            Cc::Le => Cc::G,
            Cc::G => Cc::Le,
        }
    }
}

/// A pending `rel32` displacement: the byte offset of its 4-byte slot, and the
/// label it must reach.
struct Fixup {
    site: usize,
    label: Label,
}

/// A growable x86-64 code buffer with deferred branch resolution.
pub struct Asm {
    code: Vec<u8>,
    /// Bound offset of each label, or `None` while still forward-referenced.
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
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

    /// Bind a label to the current buffer position. A label must be bound
    /// exactly once before [`finish`](Self::finish); binding twice is a codegen
    /// bug and panics rather than silently resolving to the wrong site.
    pub fn bind(&mut self, l: Label) {
        debug_assert!(
            self.labels[l.0].is_none(),
            "label {l:?} bound twice"
        );
        self.labels[l.0] = Some(self.code.len());
    }

    /// `jmp rel32` to `l` (opcode `E9`), reserving the displacement for `finish`.
    pub fn jmp(&mut self, l: Label) {
        self.code.push(0xE9);
        self.reserve_rel32(l);
    }

    /// `jcc rel32` to `l` (opcode `0F 8x`), reserving the displacement.
    pub fn jcc(&mut self, cc: Cc, l: Label) {
        self.code.extend_from_slice(&[0x0F, cc.opcode2()]);
        self.reserve_rel32(l);
    }

    /// `call rel32` to `l` (opcode `E8`) — a direct near call to another point in
    /// this buffer (e.g. a self-recursive call to the function's own entry).
    pub fn call(&mut self, l: Label) {
        self.code.push(0xE8);
        self.reserve_rel32(l);
    }

    /// Reserve a 4-byte `rel32` slot at the current position, to be patched to
    /// reach `l` in `finish`.
    fn reserve_rel32(&mut self, l: Label) {
        self.fixups.push(Fixup {
            site: self.code.len(),
            label: l,
        });
        self.code.extend_from_slice(&[0, 0, 0, 0]);
    }

    /// Resolve every branch and return the finished machine code. Returns `None`
    /// — matching the emitter's bail-on-anything-odd contract — if any branch
    /// targets an unbound label or lies farther than `rel32` can reach.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for fx in &self.fixups {
            let target = (*self.labels.get(fx.label.0)?)? as i64;
            let rel = target - (fx.site as i64 + 4);
            let rel32 = i32::try_from(rel).ok()?;
            self.code[fx.site..fx.site + 4].copy_from_slice(&rel32.to_le_bytes());
        }
        Some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Decode the little-endian rel32 that starts at `site`.
    fn rel32_at(code: &[u8], site: usize) -> i32 {
        i32::from_le_bytes(code[site..site + 4].try_into().unwrap())
    }

    #[test]
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
}
