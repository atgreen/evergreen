//! Execution tests for the POWER assembler: assemble a function with
//! `asm_ppc64le`, map it executable, and call it.
//!
//! The unit tests in `asm_ppc64le.rs` pin each encoding against a known-good
//! assembler, which proves the bit fields are right. These prove the whole path by
//! making the CPU run the result — the distinction that mattered on AArch64, where
//! every encoder test was green while two emitter bugs sat in the open.

#![cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]

use egcl_rt::asm::Cc;
use egcl_rt::asm_ppc64le::Asm;
use egcl_rt::jit::JitBuffer;

/// Assemble `build` into executable memory and reinterpret it as `F`.
fn compile<F: Copy>(build: impl FnOnce(&mut Asm)) -> F {
    let mut asm = Asm::new();
    build(&mut asm);
    let code = asm.finish().expect("every label bound and in range");
    assert_eq!(code.len() % 4, 0, "POWER code is a whole number of words");
    let buffer = JitBuffer::new(&code).expect("executable mapping");
    let entry = buffer.leak();
    assert_eq!(entry as usize % 4, 0, "entry must be instruction-aligned");
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const u8>());
    // SAFETY: the mapping is read+execute and leaked, and each caller declares the
    // signature its own emitted code implements.
    unsafe { std::mem::transmute_copy(&entry) }
}

#[test]
fn add_of_two_arguments() {
    // ELFv2 passes the first arguments in r3 onwards and returns in r3.
    let f: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.add(3, 3, 4);
        a.ret();
    });
    assert_eq!(f(7, 35), 42);
    assert_eq!(f(u64::MAX, 1), 0, "wraps like the hardware");
}

#[test]
fn subtract_respects_its_operand_order() {
    // `subf rD, rA, rB` computes rB - rA, which is the reverse of how it reads.
    let f: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.subf(3, 4, 3);
        a.ret();
    });
    assert_eq!(f(50, 8), 42);
}

#[test]
fn materialised_constants_come_back_intact() {
    for value in [
        0u64,
        1,
        42,
        0x7fff,
        0x8000,
        0xffff,
        0x1234_0000,
        0x1234_5678,
        u64::MAX,
        u64::MAX - 1,
        0x1234_5678_9abc_def0,
        0x8000_0000_0000_0000,
        // A heap pointer with the top bit set, where a sign-extension slip shows.
        0xb400_007d_1234_5678,
    ] {
        let f: extern "C" fn() -> u64 = compile(|a| {
            a.imm64(3, value);
            a.ret();
        });
        assert_eq!(f(), value, "materialising {value:#018x}");
    }
}

#[test]
fn shifts_round_trip_a_tagged_fixnum() {
    let roundtrip: extern "C" fn(i64) -> i64 = compile(|a| {
        a.sldi(3, 3, 3);
        a.ori(3, 3, 1);
        a.sradi(3, 3, 3);
        a.ret();
    });
    for value in [0i64, 1, -1, 42, -42, 1 << 50, -(1 << 50)] {
        assert_eq!(roundtrip(value), value, "tagging {value}");
    }
}

#[test]
fn backward_branch_drives_a_loop() {
    // Sum 1..=n, so both the conditional exit and the backward branch must resolve
    // correctly or the answer is wrong rather than merely slow.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        let top = a.label();
        let done = a.label();
        a.li(5, 0); // accumulator
        a.li(6, 1); // counter
        a.bind(top);
        a.compare(0, 6, 3);
        a.branch(Cc::G, 0, done); // counter > n
        a.add(5, 5, 6);
        a.addi(6, 6, 1);
        a.jump(top);
        a.bind(done);
        a.mov(3, 5);
        a.ret();
    });
    assert_eq!(f(0), 0);
    assert_eq!(f(1), 1);
    assert_eq!(f(10), 55);
    assert_eq!(f(100_000), 100_000 * 100_001 / 2);
}

#[test]
fn isel_selects_without_branching() {
    // A comparison producing a value, which is how a fixnum predicate avoids a
    // branch. Note isel reads r0 as a literal zero, so neither arm may be r0.
    let max: extern "C" fn(i64, i64) -> i64 = compile(|a| {
        a.compare(0, 3, 4);
        a.isel(3, 3, 4, Cc::Ge, 0);
        a.ret();
    });
    assert_eq!(max(3, 9), 9);
    assert_eq!(max(9, 3), 9);
    assert_eq!(max(-9, -3), -3);
    assert_eq!(max(7, 7), 7);

    // The inverse condition selects by swapping the operands, since `isel` has no
    // branch-if-false form.
    let min: extern "C" fn(i64, i64) -> i64 = compile(|a| {
        a.compare(0, 3, 4);
        a.isel(3, 3, 4, Cc::L, 0);
        a.ret();
    });
    assert_eq!(min(3, 9), 3);
    assert_eq!(min(9, 3), 3);
}

#[test]
fn multiply_overflow_is_detected_without_the_sticky_flag() {
    // The reason mulhd is here: XER[SO] is sticky and mcrxr no longer exists, so a
    // per-operation overflow guard compares the high half of the product against
    // the low half's sign extension. Returns 1 on overflow, 0 otherwise.
    let overflows: extern "C" fn(i64, i64) -> u64 = compile(|a| {
        let overflow = a.label();
        a.mulhd(5, 3, 4);
        a.mulld(6, 3, 4);
        a.sradi(7, 6, 63);
        a.compare(0, 5, 7);
        a.branch(Cc::Ne, 0, overflow);
        a.li(3, 0);
        a.ret();
        a.bind(overflow);
        a.li(3, 1);
        a.ret();
    });
    assert_eq!(overflows(6, 7), 0, "42 fits");
    assert_eq!(overflows(-6, 7), 0, "so does -42");
    assert_eq!(overflows(i64::MAX, 1), 0, "times one never overflows");
    // The exact boundary: isqrt(i64::MAX) is 3_037_000_499, so its square is the
    // largest that fits and the next integer's square exceeds it by 145_474_193 —
    // a margin small enough that a guard comparing the wrong halves would miss it.
    assert_eq!(
        overflows(3_037_000_499, 3_037_000_499),
        0,
        "the largest square that fits"
    );
    assert_eq!(
        overflows(3_037_000_500, 3_037_000_500),
        1,
        "the first that does not"
    );
    assert_eq!(overflows(4_000_000_000, 4_000_000_000), 1);
    assert_eq!(overflows(i64::MAX, 2), 1);
    assert_eq!(overflows(i64::MIN, -1), 1);
}

extern "C" fn triple(value: u64) -> u64 {
    value * 3
}

#[test]
fn a_frame_survives_an_indirect_call() {
    // POWER has no call-through-register: an indirect call loads CTR and uses
    // bctrl, which clobbers the link register — so a non-leaf must save it, and the
    // saved copy lives in the caller's frame at a fixed ELFv2 offset.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        a.move_from_link(0);
        a.store(0, 1, 16).unwrap(); // LR save slot in the caller's frame
        a.store_update(1, 1, -64).unwrap(); // claim a frame
        a.imm64(12, triple as *const () as u64);
        a.move_to_count(12);
        a.call_count();
        a.addi(3, 3, 1);
        a.addi(1, 1, 64); // release the frame
        a.load(0, 1, 16).unwrap();
        a.move_to_link(0);
        a.ret();
    });
    assert_eq!(f(14), 43);
    assert_eq!(
        f(100),
        301,
        "and again, so a leaked link register would show"
    );
}

#[test]
fn values_move_through_a_frame() {
    let f: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.store_update(1, 1, -96).unwrap();
        a.store(3, 1, 32).unwrap();
        a.store(4, 1, 40).unwrap();
        a.load(5, 1, 32).unwrap();
        a.load(6, 1, 40).unwrap();
        a.add(3, 5, 6);
        a.addi(1, 1, 96);
        a.ret();
    });
    assert_eq!(f(1000, 234), 1234);
}

#[test]
fn double_arithmetic_runs_in_the_float_registers() {
    // POWER moves directly between the register files, so unboxing needs no round
    // trip through memory.
    let add: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.move_to_float(1, 3);
        a.move_to_float(2, 4);
        a.add_double(1, 1, 2);
        a.move_from_float(3, 1);
        a.ret();
    });
    assert_eq!(
        f64::from_bits(add(0.5f64.to_bits(), 0.25f64.to_bits())),
        0.75
    );

    let multiply: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.move_to_float(1, 3);
        a.move_to_float(2, 4);
        a.multiply_double(1, 1, 2);
        a.move_from_float(3, 1);
        a.ret();
    });
    assert_eq!(
        f64::from_bits(multiply(1.5f64.to_bits(), 4.0f64.to_bits())),
        6.0
    );
}

#[test]
fn logical_operations_mask_as_written() {
    let untag: extern "C" fn(u64) -> u64 = compile(|a| {
        a.imm64(4, 0xffff_ffff_ffff_fff8);
        a.and(3, 3, 4);
        a.ret();
    });
    assert_eq!(untag(0xb400_007d_1234_5679), 0xb400_007d_1234_5678);

    let complement: extern "C" fn(u64) -> u64 = compile(|a| {
        a.nand(3, 3, 3);
        a.ret();
    });
    assert_eq!(complement(0), u64::MAX);
    assert_eq!(complement(0xff), !0xff);
}

#[test]
fn many_branches_to_one_stub_all_land() {
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        let stub = a.label();
        for value in 1..=3i16 {
            a.compare_imm(0, 3, value);
            a.branch(Cc::E, 0, stub);
            for _ in 0..64 {
                a.mov(9, 9); // filler, so each fixup carries a different offset
            }
        }
        a.li(3, 0);
        a.ret();
        a.bind(stub);
        a.li(3, 999);
        a.ret();
    });
    assert_eq!(f(1), 999);
    assert_eq!(f(2), 999);
    assert_eq!(f(3), 999);
    assert_eq!(f(4), 0);
}
