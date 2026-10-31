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
/// x86-64 `emit.rs` (bliss-9bw59), AArch64 `emit_a64.rs`, ppc64le
/// `emit_ppc64le.rs` and System Z `emit_s390x.rs` (bliss-x4cxy) all guard each
/// operand to be a single-float, compare, and select `T`/`NIL`, deopting on an
/// unordered compare so the interpreter — whose NaN ordering is not IEEE's —
/// decides that case. To add a target: give its emitter that arm, then name it
/// here.
///
/// System Z reaches the same place from the other direction: `CEBR` reports
/// unordered as condition code 3, a code *distinct* from less, equal and
/// greater, so there the hazard is not that an ordering mask reads true for a
/// NaN but that every one of them reads false — a mask-only implementation
/// would quietly answer `NIL` for `(= nan nan)`. Taking the CC3 exit first
/// settles both shapes.
pub const fn single_float_comparison() -> bool {
    cfg!(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "powerpc64", target_endian = "little"),
        target_arch = "s390x"
    ))
}

/// Whether the host emitter encodes `FixnumMin`/`FixnumMax` on tagged fixnums.
///
/// Separate from [`fixnum_division`] because it is a far smaller arm and a
/// port reaches it first: both operands are tagged by the same shift, so the
/// comparison is the operands' own order, the winner is already tagged, and
/// nothing can overflow. ppc64le has this and not the division family. To add
/// a target: give its emitter that select, then name it here.
///
/// System Z reaches it by compare-and-branch rather than a conditional select:
/// `LOCGR` needs the load-store-on-cond facility (z196) and that emitter
/// targets z10, so it branches as its `FixnumCmp` arm already does.
pub const fn fixnum_select() -> bool {
    cfg!(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "s390x",
        all(target_arch = "powerpc64", target_endian = "little")
    ))
}

/// Whether the host emitter encodes the
/// `FixnumDiv`/`FixnumFloor`/`FixnumRem`/`FixnumMod` family on tagged fixnums.
///
/// The quotient forms are the discarded-remainder `TRUNCATE` and `FLOOR`;
/// `speculate.rs` only selects them where every normal successor discards the
/// secondary value. To add a target: give its emitter those arms — including
/// the divide-by-zero exit and the quotient range check, which `MIN-FIXNUM /
/// -1` needs on any machine — then name it here.
///
/// The two exits are not symmetric across hosts, which is worth knowing before
/// porting. AArch64's `SDIV` returns a value for both cases, so its checks are
/// about correctness alone. System Z's `DSGR` *faults* on either, so there the
/// zero test is what stops a `SIGFPE` — while its overflow fault turns out to
/// be unreachable, because a tagged fixnum divisor is a multiple of eight and
/// `-2^63 / -1` needs a divisor of -1. The range check is still required on
/// every host, since it guards the retagged quotient rather than the divide.
pub const fn fixnum_division() -> bool {
    cfg!(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "s390x",
        all(target_arch = "powerpc64", target_endian = "little")
    ))
}

/// Whether the host emitter encodes `StringCharOrNil`, the inline simple-string
/// character read `CHAR`/`SCHAR`/`ELT` get a fast path from.
///
/// The pass in `string_char` rewrites the call into a `StringCharOrNil` test
/// and a `Brif` over a fast arm and the original call, so a target without the
/// arm must keep the plain call: emitting the opcode would decline the whole
/// function instead. To add a target: give its emitter that arm -- the heap
/// tag, fixnum and bounds tests all answering NIL rather than deopting, and
/// both the one-byte and four-byte simple layouts -- then name it here.
///
/// One portability trap on the way: the header's type id is its HIGH byte, so
/// it is at offset 7 on a little-endian host and offset 0 on a big-endian one.
/// emit_s390x reads it through a helper that already knows which end that is,
/// rather than repeating a literal offset per site.
pub const fn simple_string_char_access() -> bool {
    cfg!(all(
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "s390x"
        ),
        target_os = "linux"
    ))
}

/// Whether the host emitter encodes `IntegerLowBits`, the native constant-width
/// `LDB` / `LOGAND`-mask extraction that `integer_extract` selects.
///
/// The pass rewrites the call outright, so a target without the arm must not be
/// offered it: the opcode would decline the whole function. The arm needs a
/// fixnum fast path AND a bignum one -- the low bits of a negative magnitude
/// are the low bits of its negation, even across limbs, because higher limbs
/// are multiples of 2^64 -- plus the deopt exits for a non-integer, a
/// non-canonical bignum and an unexpected sign. To add a target: give its
/// emitter that arm, then name it here.
pub const fn integer_low_bits() -> bool {
    cfg!(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))
}

/// Whether the host emitter encodes the guarded element accesses
/// `array_access` selects: `VecRef`, `VecSet` and `ArrayReadDiscarded`.
///
/// This is the all-or-nothing one. The pass rewrites reads AND stores in the
/// same function, so a target that handles only some of them loses the whole
/// function the first time the other appears. An emitter claiming this must
/// therefore cover: `VecRef` for both `AREF` (SIMPLE_VECTOR then packed
/// `(unsigned-byte 8)`) and `SVREF` (SIMPLE_VECTOR only); `VecSet` both for a
/// proven simple-vector store and for the general form that probes
/// SIMPLE_VECTOR, packed-U8 and packed-`double-float` in turn; and
/// `ArrayReadDiscarded`, whose only purpose is the bounds check.
///
/// Probing every layout matters even where one seems unreachable: deopting on
/// a layout the pass admitted means deopting on EVERY call through that site,
/// which is worse than the runtime call it replaced.
///
/// `VecSetInRange` and `SmallVectorRangeP` come from the separate
/// `vector_ranges` pass, which is gated independently in the installer.
pub const fn vector_element_access() -> bool {
    cfg!(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))
}

/// Whether the host emitter encodes `TryVecRef` and `TryVecSet`, the
/// success-only forms of `VecRef` and `VecSet` that `array_access::call_fallbacks`
/// puts in front of the retained AREF/SVREF call.
///
/// A layout the native access does not cover -- an adjustable or
/// fill-pointer vector, a bit vector -- then takes the general call instead of
/// a deopt. Without it, a function whose AREF keeps meeting such an array
/// deopts on every call and is retired from T2 (bliss-gg3i1.7.8). The probes
/// share the guarded accesses' layout and bounds checks; a target claiming
/// this needs both arms, each writing nothing and answering NIL on any miss.
pub const fn vector_element_call_fallback() -> bool {
    cfg!(all(target_arch = "x86_64", target_os = "linux"))
}

/// Whether the host emitter encodes `TryIntegerShr`, the success-only
/// `(ash x -n)` probe over a fixnum or a ONE-LIMB bignum.
///
/// The pass in `integer_shift` rewrites the call into this probe plus a `Brif`
/// over the retained ASH call, so a target without the arm must not be offered
/// it. The arm earns its keep only on BIGNUM operands -- a fixnum right shift
/// is already native everywhere -- so it must carry the bignum half: one-limb
/// check, sign of +1 or -1, a logical shift of the unsigned magnitude, and
/// CL's FLOORING correction when any discarded bit was set. To add a target:
/// give its emitter that arm, then name it here.
pub const fn integer_right_shift_probe() -> bool {
    cfg!(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))
}

/// Whether the host emitter encodes `TryStoreIntegerOctets8` and
/// `TryStoreIntegerOctets1To8`, the wide-store kernel `octet_word_fusion`
/// replaces a counted integer-to-octet loop with.
///
/// Only those two: the `Layout` and `Extent` variants and the `PackedU8*`
/// family come from `array_layout` and `allocation_extent`, which run only on
/// the mapped path, so a checked-ABI target never sees them.
///
/// The kernel makes a single unaligned little-endian 64-bit write into an
/// untraced byte payload, which both x86-64 and AArch64 Linux permit. To add a
/// target: give its emitter that arm -- the canonical one-limb bignum decode
/// included, since a magnitude at or below `abs(MIN-FIXNUM)` would have been a
/// fixnum -- then name it here.
pub const fn integer_octet_word_store() -> bool {
    cfg!(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))
}

/// Whether the host emitter encodes `SmallVectorRangeP` and `VecSetInRange`,
/// the pair `vector_ranges` uses to version a counted loop.
///
/// The pass clones the loop body behind a total runtime predicate: the clone's
/// stores skip their bounds checks because the predicate proved the range.
/// Both opcodes are needed -- the predicate alone is useless and the
/// unchecked store alone is unsound -- so a target claiming this must have
/// both arms. To add a target: give its emitter those two arms, then name it
/// here.
///
/// AArch64 has emitter arms for both opcodes, but the pass remains disabled
/// there: its current code-size and safepoint cost regresses both measured
/// vector-loop shapes. Keeping the arms available lets the target be enabled
/// once the pass earns that cost without making the regression the default.
pub const fn vector_range_versioning() -> bool {
    cfg!(all(target_arch = "x86_64", target_os = "linux"))
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

    /// Dividing needs the select: `MOD` and `FLOOR` are corrected with a
    /// comparison against the divisor, and a backend that cannot select one of
    /// two tagged fixnums cannot encode that correction either.
    #[test]
    fn fixnum_division_implies_fixnum_select() {
        assert!(!super::fixnum_division() || super::fixnum_select());
    }
}
