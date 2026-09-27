//! POWER (ppc64le) instruction encoding for the native compiler.
//!
//! A self-contained assembler, as the System Z one is, rather than an extension of
//! the shared `asm.rs`: that file's branch encoding is already cfg-split two ways,
//! and a third would make it harder to read than either backend deserves.
//!
//! Conditions are the exception and come from the shared [`Cc`], because POWER's
//! eight branch aliases turn out to be exactly that enum's eight variants — four
//! condition-register bits times two senses, with inversion being the sense flip.
//! `Cc::ppc_condition` documents the mapping and the reason `Cc::O` must not be
//! used here (`XER[SO]` is sticky and `mcrxr` no longer exists).
//!
//! Instructions are fixed 32-bit words stored **little-endian** on this ABI —
//! unlike System Z, where they are big-endian regardless of the host.

use crate::asm::Cc;

/// The frame convention every ppc64le JIT tier follows.
///
/// T1, T2 and a future SIGSEGV recovery epilogue must agree, because a fault
/// anywhere in native code is redirected to one address that has to unwind
/// whichever tier was running. T2's frame is sized per function while T1's is
/// fixed, so agreeing on a frame SIZE — the approach taken on AArch64, where it
/// was reached late and cost a change to both emitters (bliss-7t9a4) — would not
/// work here.
///
/// ELFv2 supplies the answer: nonvolatile registers are saved at the TOP of the
/// frame, so their addresses are fixed relative to the CALLER's stack pointer
/// rather than to this frame's size. An epilogue can therefore follow the back
/// chain at `0(r1)` to recover the caller's pointer and restore from there,
/// whatever size frame it lands in.
pub mod frame {
    /// Where a callee of this frame saves its caller's (our) link register. This
    /// frame's own return address lives in the *caller's* slot, written before the
    /// frame is claimed.
    pub const LINK_SLOT: i32 = 16;
    /// The TOC save slot, preserved around calls that may set up their own.
    pub const TOC_SLOT: i32 = 24;
    /// A scratch doubleword for spilling a result across a helper call.
    pub const SCRATCH_SLOT: i32 = 32;
    /// The back-edge poll counter.
    pub const POLL_SLOT: i32 = 40;
    /// First byte available for a tier's own slots.
    pub const LOCALS_BASE: i32 = 48;

    /// Every nonvolatile register a JIT tier may use: the three activation
    /// registers, then the optimizing tier's allocation pool.
    ///
    /// BOTH tiers save all of them, although T1 needs only the first three. A
    /// recovery epilogue cannot know which tier it is unwinding, so it cannot know
    /// how many to restore — and restoring a register a tier never saved would
    /// hand the caller a word of uninitialised stack. Ten redundant stores buy that
    /// whole class of bug away, the same trade taken on AArch64.
    pub const SAVED: [u8; 13] = [14, 15, 16, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29];

    /// Bytes the save area occupies at the top of the frame.
    pub const SAVED_BYTES: i32 = 8 * SAVED.len() as i32;

    /// Where `SAVED[index]` lives in a frame of `size` bytes. Measured down from
    /// the top, so the address is fixed relative to the CALLER's stack pointer and
    /// an epilogue can find it through the back chain at `0(r1)` without knowing
    /// this frame's size.
    pub const fn saved_offset(size: i32, index: usize) -> i32 {
        size - 8 * (index as i32 + 1)
    }

    /// T1's fixed frame: its own slots plus the shared save area, rounded to the 16
    /// bytes the ABI requires at every call.
    pub const T1_BYTES: i32 = (LOCALS_BASE + SAVED_BYTES + 15) & !15;
}

/// A word-aligned branch destination in one assembler.
#[derive(Clone, Copy, Debug)]
pub struct Label(usize);

/// Which displacement field a pending branch patches. The two differ by more than
/// width: a conditional branch reaches only ±32 KiB, which a large function runs
/// out of long before the unconditional ±32 MiB.
#[derive(Clone, Copy)]
enum BranchKind {
    /// `b`/`bl`: signed 24-bit word displacement in bits 2..25.
    Immediate24,
    /// `bc`: signed 14-bit word displacement in bits 2..15.
    Conditional14,
}

/// Fixed-width encodings with deferred branch displacements.
#[derive(Default)]
pub struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    branches: Vec<(usize, Label, BranchKind)>,
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn here(&self) -> usize {
        self.code.len()
    }

    pub fn label(&mut self) -> Label {
        let label = Label(self.labels.len());
        self.labels.push(None);
        label
    }

    pub fn bind(&mut self, label: Label) {
        assert!(self.labels[label.0].is_none(), "label already bound");
        self.labels[label.0] = Some(self.here());
    }

    pub fn label_offset(&self, label: Label) -> Option<usize> {
        self.labels.get(label.0).copied().flatten()
    }

    /// Append one instruction word.
    pub fn word(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }

    // ── constants ────────────────────────────────────────────────

    /// `li rD, simm16` — the whole constant when it fits a signed halfword.
    pub fn li(&mut self, d: u8, value: i16) {
        self.word(0x3800_0000 | ((d as u32) << 21) | (value as u16 as u32));
    }

    /// `lis rD, simm16` — the constant in the upper halfword, lower cleared.
    pub fn lis(&mut self, d: u8, value: i16) {
        self.word(0x3C00_0000 | ((d as u32) << 21) | (value as u16 as u32));
    }

    /// `ori rA, rS, uimm16`.
    pub fn ori(&mut self, a: u8, s: u8, value: u16) {
        self.word(0x6000_0000 | ((s as u32) << 21) | ((a as u32) << 16) | value as u32);
    }

    /// `oris rA, rS, uimm16`.
    pub fn oris(&mut self, a: u8, s: u8, value: u16) {
        self.word(0x6400_0000 | ((s as u32) << 21) | ((a as u32) << 16) | value as u32);
    }

    /// Materialise an arbitrary 64-bit constant, in one to five instructions.
    ///
    /// POWER has no move-wide-immediate: a full 64-bit constant is built a halfword
    /// at a time, with a shift in the middle. The short forms matter because a
    /// tagged fixnum is usually reachable by one or two.
    pub fn imm64(&mut self, d: u8, value: u64) {
        let signed = value as i64;
        if let Ok(small) = i16::try_from(signed) {
            self.li(d, small);
            return;
        }
        if signed == (signed as i32) as i64 {
            // A signed 32-bit constant: upper halfword, then the lower if nonzero.
            self.lis(d, (signed >> 16) as i16);
            if value as u16 != 0 {
                self.ori(d, d, value as u16);
            }
            return;
        }
        self.lis(d, (value >> 48) as i16);
        if (value >> 32) as u16 != 0 {
            self.ori(d, d, (value >> 32) as u16);
        }
        self.sldi(d, d, 32);
        if (value >> 16) as u16 != 0 {
            self.oris(d, d, (value >> 16) as u16);
        }
        if value as u16 != 0 {
            self.ori(d, d, value as u16);
        }
    }

    // ── register arithmetic and logic ────────────────────────────

    fn xo(&mut self, opcode: u32, first: u8, second: u8, third: u8, extended: u32) {
        self.word(
            (opcode << 26)
                | ((first as u32) << 21)
                | ((second as u32) << 16)
                | ((third as u32) << 11)
                | extended,
        );
    }

    /// `mr rA, rS` — architecturally `or rA, rS, rS`.
    pub fn mov(&mut self, a: u8, s: u8) {
        if a != s {
            self.or(a, s, s);
        }
    }

    /// `add rD, rA, rB`.
    pub fn add(&mut self, d: u8, a: u8, b: u8) {
        self.xo(31, d, a, b, 266 << 1);
    }

    /// `subf rD, rA, rB` — computes `rB - rA`, note the operand order.
    pub fn subf(&mut self, d: u8, a: u8, b: u8) {
        self.xo(31, d, a, b, 40 << 1);
    }

    /// `neg rD, rA`.
    pub fn neg(&mut self, d: u8, a: u8) {
        self.xo(31, d, a, 0, 104 << 1);
    }

    /// `mulld rD, rA, rB` — the low 64 bits of the product.
    pub fn mulld(&mut self, d: u8, a: u8, b: u8) {
        self.xo(31, d, a, b, 233 << 1);
    }

    /// `mulhd rD, rA, rB` — the high 64 bits of the signed product.
    ///
    /// This is how a fixnum multiply detects overflow, for the same reason AArch64
    /// needs `SMULH`: compare the high half against the low half's sign extension.
    /// POWER's flag-setting multiply routes through the sticky `XER[SO]`, which is
    /// not usable for a per-operation guard.
    pub fn mulhd(&mut self, d: u8, a: u8, b: u8) {
        self.xo(31, d, a, b, 73 << 1);
    }

    fn logical(&mut self, a: u8, s: u8, b: u8, extended: u32) {
        self.word(
            (31 << 26)
                | ((s as u32) << 21)
                | ((a as u32) << 16)
                | ((b as u32) << 11)
                | (extended << 1),
        );
    }

    /// `and rA, rS, rB`.
    pub fn and(&mut self, a: u8, s: u8, b: u8) {
        self.logical(a, s, b, 28);
    }

    /// `or rA, rS, rB`.
    pub fn or(&mut self, a: u8, s: u8, b: u8) {
        self.logical(a, s, b, 444);
    }

    /// `xor rA, rS, rB`.
    pub fn xor(&mut self, a: u8, s: u8, b: u8) {
        self.logical(a, s, b, 316);
    }

    /// `nand rA, rS, rB`; with `s == b` this is a bitwise NOT.
    pub fn nand(&mut self, a: u8, s: u8, b: u8) {
        self.logical(a, s, b, 476);
    }

    /// `extsw rA, rS` — sign-extend a word to a doubleword.
    pub fn extsw(&mut self, a: u8, s: u8) {
        self.logical(a, s, 0, 986);
    }

    /// `addi rD, rA, simm16`. With `a == 0` this is `li`.
    pub fn addi(&mut self, d: u8, a: u8, value: i16) {
        self.word(0x3800_0000 | ((d as u32) << 21) | ((a as u32) << 16) | (value as u16 as u32));
    }

    // ── shifts ───────────────────────────────────────────────────

    /// `sradi rA, rS, sh` — arithmetic right shift, which is how a fixnum untags.
    pub fn sradi(&mut self, a: u8, s: u8, shift: u8) {
        debug_assert!(shift < 64);
        let sh = shift as u32;
        self.word(
            (31 << 26)
                | ((s as u32) << 21)
                | ((a as u32) << 16)
                | ((sh & 0x1f) << 11)
                | (413 << 2)
                | ((sh >> 5) << 1),
        );
    }

    /// `sldi rA, rS, sh` — architecturally `rldicr rA, rS, sh, 63 - sh`.
    pub fn sldi(&mut self, a: u8, s: u8, shift: u8) {
        debug_assert!(shift < 64);
        self.rldic(a, s, shift as u32, 63 - shift as u32, 1);
    }

    /// `srdi rA, rS, sh` — architecturally `rldicl rA, rS, 64 - sh, sh`.
    pub fn srdi(&mut self, a: u8, s: u8, shift: u8) {
        debug_assert!(shift < 64);
        let shift = shift as u32;
        self.rldic(a, s, (64 - shift) & 63, shift, 0);
    }

    /// The `rldicl`/`rldicr` pair, whose mask bound is split across two fields.
    fn rldic(&mut self, a: u8, s: u8, shift: u32, bound: u32, form: u32) {
        self.word(
            (30 << 26)
                | ((s as u32) << 21)
                | ((a as u32) << 16)
                | ((shift & 0x1f) << 11)
                | ((bound & 0x1f) << 6)
                | ((bound >> 5) << 5)
                | (form << 2)
                | ((shift >> 5) << 1),
        );
    }

    // ── comparison and selection ─────────────────────────────────

    /// `cmpd field, rA, rB` — a signed doubleword compare into a CR field.
    pub fn compare(&mut self, field: u8, a: u8, b: u8) {
        debug_assert!(field < 8);
        self.word(
            (31 << 26)
                | ((field as u32) << 23)
                | (1 << 21)
                | ((a as u32) << 16)
                | ((b as u32) << 11),
        );
    }

    /// `cmpdi field, rA, simm16`.
    pub fn compare_imm(&mut self, field: u8, a: u8, value: i16) {
        debug_assert!(field < 8);
        self.word(
            (11 << 26)
                | ((field as u32) << 23)
                | (1 << 21)
                | ((a as u32) << 16)
                | (value as u16 as u32),
        );
    }

    /// `isel rD, rA, rB, bit` — `rD = if CR bit set { rA } else { rB }`, so a
    /// comparison can produce a value without a branch, as AArch64's `csel` does.
    /// `rA == 0` reads as a literal zero rather than as r0, so the emitter must not
    /// pass r0 expecting its contents.
    pub fn isel(&mut self, d: u8, a: u8, b: u8, condition: Cc, field: u8) {
        debug_assert!(field < 8);
        let (bit, sense) = condition.ppc_condition();
        let (a, b) = if sense { (a, b) } else { (b, a) };
        self.word(
            (31 << 26)
                | ((d as u32) << 21)
                | ((a as u32) << 16)
                | ((b as u32) << 11)
                | ((field as u32 * 4 + bit) << 6)
                | (15 << 1),
        );
    }

    // ── loads and stores ─────────────────────────────────────────

    /// `ld rD, offset(rA)` — the displacement is a signed multiple of four.
    pub fn load(&mut self, d: u8, a: u8, offset: i32) -> Option<()> {
        self.ds_form(0xE800_0000, d, a, offset)
    }

    /// `std rS, offset(rA)`.
    pub fn store(&mut self, s: u8, a: u8, offset: i32) -> Option<()> {
        self.ds_form(0xF800_0000, s, a, offset)
    }

    /// `stdu rS, offset(rA)` — store and update the base, i.e. claim a frame.
    pub fn store_update(&mut self, s: u8, a: u8, offset: i32) -> Option<()> {
        self.ds_form(0xF800_0001, s, a, offset)
    }

    /// The DS form holds a 14-bit signed displacement in its top bits, so the low
    /// two bits of the offset are not available and an unaligned one cannot be
    /// encoded at all.
    fn ds_form(&mut self, base: u32, r: u8, a: u8, offset: i32) -> Option<()> {
        if offset % 4 != 0 || !(-32768..32768).contains(&offset) {
            return None;
        }
        self.word(base | ((r as u32) << 21) | ((a as u32) << 16) | (offset as u32 & 0xfffc));
        Some(())
    }

    /// `lwz rD, offset(rA)` — a 32-bit load, for the tier counters.
    pub fn load_word(&mut self, d: u8, a: u8, offset: i32) -> Option<()> {
        self.d_form(0x8000_0000, d, a, offset)
    }

    /// `stw rS, offset(rA)` — a 32-bit store.
    pub fn store_word(&mut self, s: u8, a: u8, offset: i32) -> Option<()> {
        self.d_form(0x9000_0000, s, a, offset)
    }

    /// `lfd frD, offset(rA)`.
    pub fn load_float(&mut self, d: u8, a: u8, offset: i32) -> Option<()> {
        self.d_form(0xC800_0000, d, a, offset)
    }

    /// `stfd frS, offset(rA)`.
    pub fn store_float(&mut self, s: u8, a: u8, offset: i32) -> Option<()> {
        self.d_form(0xD800_0000, s, a, offset)
    }

    fn d_form(&mut self, base: u32, r: u8, a: u8, offset: i32) -> Option<()> {
        if !(-32768..32768).contains(&offset) {
            return None;
        }
        self.word(base | ((r as u32) << 21) | ((a as u32) << 16) | (offset as u32 & 0xffff));
        Some(())
    }

    // ── moving between the register files ────────────────────────

    /// `mtvsrd frD, rA` — move a doubleword into a floating-point register. POWER
    /// can do this directly, so unboxing needs no round trip through memory.
    pub fn move_to_float(&mut self, d: u8, a: u8) {
        self.word((31 << 26) | ((d as u32) << 21) | ((a as u32) << 16) | (179 << 1));
    }

    /// `mfvsrd rA, frS` — and back.
    pub fn move_from_float(&mut self, a: u8, s: u8) {
        self.word((31 << 26) | ((s as u32) << 21) | ((a as u32) << 16) | (51 << 1));
    }

    // ── floating point ───────────────────────────────────────────

    fn fp(&mut self, opcode: u32, d: u8, a: u8, b: u8, extended: u32) {
        self.word(
            (opcode << 26)
                | ((d as u32) << 21)
                | ((a as u32) << 16)
                | ((b as u32) << 11)
                | (extended << 1),
        );
    }

    /// `fadd frD, frA, frB`.
    pub fn add_double(&mut self, d: u8, a: u8, b: u8) {
        self.fp(63, d, a, b, 21);
    }

    /// `fsub frD, frA, frB`.
    pub fn sub_double(&mut self, d: u8, a: u8, b: u8) {
        self.fp(63, d, a, b, 20);
    }

    /// `fmul frD, frA, frC` — the multiplicand is in the C field, not B.
    pub fn multiply_double(&mut self, d: u8, a: u8, c: u8) {
        self.word(
            (63 << 26) | ((d as u32) << 21) | ((a as u32) << 16) | ((c as u32) << 6) | (25 << 1),
        );
    }

    /// `fdiv frD, frA, frB`.
    pub fn divide_double(&mut self, d: u8, a: u8, b: u8) {
        self.fp(63, d, a, b, 18);
    }

    /// `fadds frD, frA, frB` — single precision.
    pub fn add_single(&mut self, d: u8, a: u8, b: u8) {
        self.fp(59, d, a, b, 21);
    }

    /// `fsubs frD, frA, frB`.
    pub fn sub_single(&mut self, d: u8, a: u8, b: u8) {
        self.fp(59, d, a, b, 20);
    }

    /// `fmuls frD, frA, frC`.
    pub fn multiply_single(&mut self, d: u8, a: u8, c: u8) {
        self.word(
            (59 << 26) | ((d as u32) << 21) | ((a as u32) << 16) | ((c as u32) << 6) | (25 << 1),
        );
    }

    /// `fcmpu field, frA, frB`.
    pub fn compare_double(&mut self, field: u8, a: u8, b: u8) {
        debug_assert!(field < 8);
        self.word((63 << 26) | ((field as u32) << 23) | ((a as u32) << 16) | ((b as u32) << 11));
    }

    // ── control flow ─────────────────────────────────────────────

    /// `mflr rD` — read the link register, which a non-leaf prologue must save.
    pub fn move_from_link(&mut self, d: u8) {
        self.word((31 << 26) | ((d as u32) << 21) | (8 << 16) | (339 << 1));
    }

    /// `mtlr rS`.
    pub fn move_to_link(&mut self, s: u8) {
        self.word((31 << 26) | ((s as u32) << 21) | (8 << 16) | (467 << 1));
    }

    /// `mtctr rS` — load the count register, which is how an indirect call is made:
    /// POWER has no call-through-register, only a branch through CTR or LR.
    pub fn move_to_count(&mut self, s: u8) {
        self.word((31 << 26) | ((s as u32) << 21) | (9 << 16) | (467 << 1));
    }

    /// `bctrl` — call through the count register.
    pub fn call_count(&mut self) {
        self.word(0x4E80_0421);
    }

    /// `bctr` — jump through the count register.
    pub fn jump_count(&mut self) {
        self.word(0x4E80_0420);
    }

    /// `blr` — return through the link register.
    pub fn ret(&mut self) {
        self.word(0x4E80_0020);
    }

    /// `b label`.
    pub fn jump(&mut self, label: Label) {
        self.branches
            .push((self.here(), label, BranchKind::Immediate24));
        self.word(0x4800_0000);
    }

    /// `bl label` — a direct call within this buffer.
    pub fn call(&mut self, label: Label) {
        self.branches
            .push((self.here(), label, BranchKind::Immediate24));
        self.word(0x4800_0001);
    }

    /// `bc label` on a condition in `field`. Reaches only ±32 KiB, so a large
    /// function can exhaust this where the unconditional form still fits.
    pub fn branch(&mut self, condition: Cc, field: u8, label: Label) {
        debug_assert!(field < 8);
        let (bit, sense) = condition.ppc_condition();
        let bo = if sense { 12 } else { 4 };
        let bi = field as u32 * 4 + bit;
        self.branches
            .push((self.here(), label, BranchKind::Conditional14));
        self.word((16 << 26) | (bo << 21) | (bi << 16));
    }

    /// Resolve every branch. `None` if a label was never bound or a displacement
    /// does not reach, which is the emitter's cue to decline rather than to emit a
    /// branch that lands somewhere else.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for &(site, label, kind) in &self.branches {
            let target = (*self.labels.get(label.0)?)? as i64;
            let displacement = target - site as i64;
            if displacement % 4 != 0 {
                return None;
            }
            let words = displacement / 4;
            let (bits, shift) = match kind {
                BranchKind::Immediate24 => (24, 2),
                BranchKind::Conditional14 => (14, 2),
            };
            let limit = 1i64 << (bits - 1);
            if words < -limit || words >= limit {
                return None;
            }
            let mask = ((1u32 << bits) - 1) << shift;
            let field = ((words as u32) << shift) & mask;
            let word = u32::from_le_bytes(self.code[site..site + 4].try_into().unwrap());
            self.code[site..site + 4].copy_from_slice(&(word | field).to_le_bytes());
        }
        Some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble one instruction and read back its word.
    fn one(build: impl FnOnce(&mut Asm)) -> u32 {
        let mut asm = Asm::new();
        build(&mut asm);
        let code = asm.finish().expect("no pending branches");
        assert_eq!(code.len(), 4, "expected exactly one instruction");
        u32::from_le_bytes(code[..4].try_into().unwrap())
    }

    fn words(build: impl FnOnce(&mut Asm)) -> Vec<u32> {
        let mut asm = Asm::new();
        build(&mut asm);
        asm.finish()
            .expect("no pending branches")
            .chunks(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect()
    }

    /// Every expected word came from a known-good assembler
    /// (`llvm-mc -triple=powerpc64le -show-encoding`) for the mnemonic in the
    /// label. A failure means this encoder drifted from the architecture, not that
    /// the test needs updating.
    fn check(cases: &[(&str, u32, u32)]) {
        let wrong: Vec<_> = cases
            .iter()
            .filter(|(_, expected, got)| expected != got)
            .map(|(asm, expected, got)| {
                format!("  {asm}: expected {expected:#010x}, got {got:#010x}")
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "encodings disagree:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn integer_operations_match_the_assembler() {
        check(&[
            ("mr 3, 4", 0x7C832378, one(|a| a.mov(3, 4))),
            ("li 3, -5", 0x3860FFFB, one(|a| a.li(3, -5))),
            ("lis 3, 0x1234", 0x3C601234, one(|a| a.lis(3, 0x1234))),
            ("ori 3, 3, 0x5678", 0x60635678, one(|a| a.ori(3, 3, 0x5678))),
            (
                "oris 3, 3, 0x9abc",
                0x64639ABC,
                one(|a| a.oris(3, 3, 0x9abc)),
            ),
            ("addi 3, 4, 100", 0x38640064, one(|a| a.addi(3, 4, 100))),
            ("add 3, 4, 5", 0x7C642A14, one(|a| a.add(3, 4, 5))),
            ("subf 3, 4, 5", 0x7C642850, one(|a| a.subf(3, 4, 5))),
            ("neg 3, 4", 0x7C6400D0, one(|a| a.neg(3, 4))),
            ("and 3, 4, 5", 0x7C832838, one(|a| a.and(3, 4, 5))),
            ("or 3, 4, 5", 0x7C832B78, one(|a| a.or(3, 4, 5))),
            ("xor 3, 4, 5", 0x7C832A78, one(|a| a.xor(3, 4, 5))),
            ("nand 3, 4, 4", 0x7C8323B8, one(|a| a.nand(3, 4, 4))),
            ("mulld 3, 4, 5", 0x7C6429D2, one(|a| a.mulld(3, 4, 5))),
            ("mulhd 3, 4, 5", 0x7C642892, one(|a| a.mulhd(3, 4, 5))),
            ("extsw 3, 4", 0x7C8307B4, one(|a| a.extsw(3, 4))),
        ]);
    }

    #[test]
    fn shifts_match_the_assembler() {
        check(&[
            ("sradi 3, 4, 3", 0x7C831E74, one(|a| a.sradi(3, 4, 3))),
            ("sldi 3, 4, 3", 0x78831F24, one(|a| a.sldi(3, 4, 3))),
            ("sldi 3, 3, 32", 0x786307C6, one(|a| a.sldi(3, 3, 32))),
            ("srdi 3, 4, 3", 0x7883E8C2, one(|a| a.srdi(3, 4, 3))),
        ]);
    }

    #[test]
    fn comparison_and_selection_match_the_assembler() {
        check(&[
            ("cmpd 0, 3, 4", 0x7C232000, one(|a| a.compare(0, 3, 4))),
            ("cmpdi 0, 3, 7", 0x2C230007, one(|a| a.compare_imm(0, 3, 7))),
            (
                "iseleq 3, 4, 5",
                0x7C64289E,
                one(|a| a.isel(3, 4, 5, Cc::E, 0)),
            ),
            // `isel` has no branch-if-false form, so the inverse condition selects
            // by swapping the operands instead.
            (
                "iseleq 3, 5, 4 (as isel ne)",
                0x7C65209E,
                one(|a| a.isel(3, 4, 5, Cc::Ne, 0)),
            ),
        ]);
    }

    #[test]
    fn loads_and_stores_match_the_assembler() {
        check(&[
            (
                "ld 3, 16(4)",
                0xE8640010,
                one(|a| a.load(3, 4, 16).unwrap()),
            ),
            (
                "std 3, 16(4)",
                0xF8640010,
                one(|a| a.store(3, 4, 16).unwrap()),
            ),
            (
                "stdu 1, -96(1)",
                0xF821FFA1,
                one(|a| a.store_update(1, 1, -96).unwrap()),
            ),
            (
                "lwz 3, 16(4)",
                0x80640010,
                one(|a| a.load_word(3, 4, 16).unwrap()),
            ),
            (
                "stw 3, 16(4)",
                0x90640010,
                one(|a| a.store_word(3, 4, 16).unwrap()),
            ),
            (
                "lfd 1, 16(4)",
                0xC8240010,
                one(|a| a.load_float(1, 4, 16).unwrap()),
            ),
            (
                "stfd 1, 16(4)",
                0xD8240010,
                one(|a| a.store_float(1, 4, 16).unwrap()),
            ),
        ]);
    }

    #[test]
    fn unencodable_displacements_are_refused() {
        // The DS form has no room for the low two bits, so an unaligned doubleword
        // displacement cannot be encoded at all — unlike the D form, which can.
        let mut asm = Asm::new();
        assert!(asm.load(3, 4, 12).is_some());
        assert!(
            asm.load(3, 4, 13).is_none(),
            "DS form needs a multiple of four"
        );
        assert!(
            asm.load_word(3, 4, 13).is_some(),
            "the D form takes any offset"
        );
        assert!(asm.store(3, 4, 32768).is_none());
        assert!(asm.store(3, 4, -32768).is_some());
    }

    #[test]
    fn floating_point_matches_the_assembler() {
        check(&[
            ("fadd 1, 2, 3", 0xFC22182A, one(|a| a.add_double(1, 2, 3))),
            ("fsub 1, 2, 3", 0xFC221828, one(|a| a.sub_double(1, 2, 3))),
            (
                "fmul 1, 2, 3",
                0xFC2200F2,
                one(|a| a.multiply_double(1, 2, 3)),
            ),
            (
                "fdiv 1, 2, 3",
                0xFC221824,
                one(|a| a.divide_double(1, 2, 3)),
            ),
            ("fadds 1, 2, 3", 0xEC22182A, one(|a| a.add_single(1, 2, 3))),
            ("fsubs 1, 2, 3", 0xEC221828, one(|a| a.sub_single(1, 2, 3))),
            (
                "fmuls 1, 2, 3",
                0xEC2200F2,
                one(|a| a.multiply_single(1, 2, 3)),
            ),
            (
                "fcmpu 0, 1, 2",
                0xFC011000,
                one(|a| a.compare_double(0, 1, 2)),
            ),
            ("mtvsrd 1, 3", 0x7C230166, one(|a| a.move_to_float(1, 3))),
            ("mfvsrd 3, 1", 0x7C230066, one(|a| a.move_from_float(3, 1))),
        ]);
    }

    #[test]
    fn control_flow_matches_the_assembler() {
        check(&[
            ("mflr 0", 0x7C0802A6, one(|a| a.move_from_link(0))),
            ("mtlr 0", 0x7C0803A6, one(|a| a.move_to_link(0))),
            ("mtctr 12", 0x7D8903A6, one(|a| a.move_to_count(12))),
            ("bctr", 0x4E800420, one(|a| a.jump_count())),
            ("bctrl", 0x4E800421, one(|a| a.call_count())),
            ("blr", 0x4E800020, one(|a| a.ret())),
        ]);
    }

    /// The eight conditions, as an assembler disassembles their aliases: four
    /// condition-register bits times two senses.
    #[test]
    fn conditional_branches_encode_bit_and_sense() {
        for (name, condition, expected) in [
            ("beq -> bt 2", Cc::E, 0x4182_0000u32),
            ("bne -> bf 2", Cc::Ne, 0x4082_0000),
            ("blt -> bt 0", Cc::L, 0x4180_0000),
            ("bge -> bf 0", Cc::Ge, 0x4080_0000),
            ("bgt -> bt 1", Cc::G, 0x4181_0000),
            ("ble -> bf 1", Cc::Le, 0x4081_0000),
            ("bso -> bt 3", Cc::O, 0x4183_0000),
            ("bns -> bf 3", Cc::No, 0x4083_0000),
        ] {
            let mut asm = Asm::new();
            let target = asm.label();
            asm.branch(condition, 0, target);
            asm.bind(target);
            let code = asm.finish().expect("bound");
            let word = u32::from_le_bytes(code[..4].try_into().unwrap());
            // Displacement 4 lands in bits 2..15.
            assert_eq!(word, expected | 4, "{name}");
        }
    }

    /// A CR field other than 0 shifts the bit index by four, since `BI` addresses
    /// all 32 condition bits rather than a bit within a chosen field.
    #[test]
    fn a_later_condition_register_field_shifts_the_bit_index() {
        let mut asm = Asm::new();
        let target = asm.label();
        asm.branch(Cc::E, 3, target);
        asm.bind(target);
        let code = asm.finish().unwrap();
        let word = u32::from_le_bytes(code[..4].try_into().unwrap());
        // An assembler renders this as `bt 14`: BI addresses all 32 condition bits,
        // so field 3's EQ is bit 4*3 + 2.
        assert_eq!(word, 0x418E_0000 | 4);
    }

    #[test]
    fn branch_displacements_resolve_both_directions() {
        // Forward: b over one instruction.
        let forward = words(|a| {
            let done = a.label();
            a.jump(done);
            a.ret();
            a.bind(done);
        });
        assert_eq!(
            forward[0],
            0x4800_0000 | 8,
            "displacement counts bytes from the branch"
        );

        // Backward: a negative displacement in two's complement, masked to its field.
        let backward = words(|a| {
            let top = a.label();
            a.bind(top);
            a.ret();
            a.jump(top);
        });
        assert_eq!(backward[1], 0x4800_0000 | ((-4i32 as u32) & 0x03ff_fffc));
    }

    #[test]
    fn out_of_reach_branches_are_refused() {
        // A conditional branch reaches only ±32 KiB, where the unconditional form
        // reaches ±32 MiB — so a span can be fine for one and not the other.
        let span = 40_000 / 4;
        let mut near = Asm::new();
        let far = near.label();
        near.branch(Cc::E, 0, far);
        for _ in 0..span {
            near.ret();
        }
        near.bind(far);
        assert!(near.finish().is_none(), "bc cannot reach 40 KiB");

        let mut wide = Asm::new();
        let far = wide.label();
        wide.jump(far);
        for _ in 0..span {
            wide.ret();
        }
        wide.bind(far);
        assert!(wide.finish().is_some(), "b reaches 40 KiB easily");
    }

    #[test]
    fn unbound_labels_make_finish_bail() {
        let mut asm = Asm::new();
        let never = asm.label();
        asm.jump(never);
        assert!(asm.finish().is_none());
    }

    #[test]
    fn constants_take_the_shortest_form() {
        assert_eq!(
            words(|a| a.imm64(3, 7)).len(),
            1,
            "a small positive fits li"
        );
        assert_eq!(
            words(|a| a.imm64(3, -7i64 as u64)).len(),
            1,
            "and a small negative"
        );
        assert_eq!(
            words(|a| a.imm64(3, 0x1234_0000)).len(),
            1,
            "lis alone when the lower half is zero"
        );
        assert_eq!(words(|a| a.imm64(3, 0x1234_5678)).len(), 2, "lis + ori");
        assert_eq!(
            words(|a| a.imm64(3, 0x1234_5678_9abc_def0)).len(),
            5,
            "a full 64-bit constant is built a halfword at a time"
        );
        // The short forms must still be exactly what the assembler emits.
        check(&[("li 3, 7", 0x38600007, words(|a| a.imm64(3, 7))[0])]);
    }
}
