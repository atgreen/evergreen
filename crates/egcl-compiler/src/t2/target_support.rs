// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Which speculation families the *host's* T2 emitter can actually encode.
//!
//! # Why this module exists
//!
//! A speculation pass and the emitter that has to encode its output are in
//! different files, and the pass runs first. When a pass rewrites a generic
//! call into a typed opcode the host emitter has no arm for, the emitter
//! declines — and a decline is not a slow path, it costs the WHOLE FUNCTION
//! its T2 code. So every such pass is gated on the host.
//!
//! Those gates were written as `cfg!(target_arch = "x86_64")` at each site,
//! which has a failure mode this module exists to remove: when a port grows
//! the emitter arm, nothing points at the pass that still refuses to produce
//! the opcode. The AArch64 emitter had `FloatAdd`/`FloatSub`/`FloatMul` arms
//! for some time while `typed_cmp_opcode` went on returning `None` for every
//! single-float comparison, so `(< x 1.0)` kept its generic call and any
//! function containing one stayed at T1.
//!
//! Each predicate below is therefore the single place a family is enabled, and
//! its name says which emitter arm it is asserting about. Adding a target to
//! one is the second half of porting that family, and the comment on each says
//! what the first half is.
//!
//! These are host properties, not options: T2 compiles for the machine it runs
//! on, so `cfg!` is the right question and the answers fold at compile time.
//!
//! # Families still x86-64-only
//!
//! Not yet moved here, because each is gated at more than one site and wants
//! its own port (bliss-dkqep): the raw `UnboxedF64` double pipeline
//! (`unbox_f64`, `ConstDouble`, `FloatDiv`/`Neg`/`Sqrt`, `PackedF64*`), guarded
//! array element access (`array_access`), the simple-string character fast
//! path (`string_char`), unboxed fixnum representation (`unbox_fixnum`),
//! success-only integer shifts (`integer_shift`) and the integer-to-octet word
//! fusion (`octet_word_fusion`).

/// Whether the host emitter encodes `FloatCmpEq`/`Lt`/`Le`/`Gt`/`Ge` on
/// **boxed single-floats**.
///
/// x86-64 `emit.rs` (bliss-9bw59) and AArch64 `emit_a64.rs` both guard each
/// operand to be a single-float, compare, and select `T`/`NIL`, deopting on an
/// unordered compare so the interpreter — whose NaN ordering is not IEEE's —
/// decides that case. To add a target: give its emitter that arm, then name it
/// here.
pub const fn single_float_comparison() -> bool {
    cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

/// Whether the host emitter encodes `FixnumMin`/`FixnumMax` and the
/// `FixnumDiv`/`FixnumFloor`/`FixnumRem`/`FixnumMod` family on tagged fixnums.
///
/// The quotient forms are the discarded-remainder `TRUNCATE` and `FLOOR`;
/// `speculate.rs` only selects them where every normal successor discards the
/// secondary value. To add a target: give its emitter those arms — including
/// the divide-by-zero exit and the quotient range check, which `MIN-FIXNUM /
/// -1` needs on any machine — then name it here.
pub const fn fixnum_division() -> bool {
    cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

/// Whether the host emitter encodes the **raw `UnboxedF64`** double-float
/// pipeline: `ConstDouble`, raw `FloatAdd`/`Sub`/`Mul`/`Div`/`Neg`/`Sqrt` and
/// the raw double comparisons.
///
/// This is a representation path, not a single arm: `unbox_f64` has to rewrite
/// values to `UnboxedF64` homes and the emitter has to keep raw words in
/// registers across the frame. Only x86-64 has it.
pub const fn double_float() -> bool {
    cfg!(target_arch = "x86_64")
}

#[cfg(test)]
mod tests {
    /// The point of the module: the host's own answers are consistent, and a
    /// family is never claimed where the double-float representation path it
    /// would need is absent.
    #[test]
    fn double_float_implies_single_float_comparison() {
        assert!(!super::double_float() || super::single_float_comparison());
    }
}
