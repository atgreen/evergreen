//! Execution tests for the AArch64 assembler: assemble a function with the
//! `asm::a64` encoders, map it executable, and call it.
//!
//! The unit tests in `asm/a64.rs` pin each encoding against a known-good
//! assembler, which proves the bit fields are right. These prove the whole path
//! — encoder, label fixups, `JitBuffer`'s mapping and its instruction-cache
//! maintenance — by making the CPU run the result.

#![cfg(all(target_arch = "aarch64", unix))]

use egcl_rt::asm::a64::{self, FP, LR, SP};
use egcl_rt::asm::{Asm, Cc};
use egcl_rt::jit::JitBuffer;

/// Assemble `build` into executable memory and reinterpret it as `F`.
///
/// The buffer is leaked: these functions are called after this returns, and a
/// dropped mapping would unmap the code out from under them.
fn compile<F: Copy>(build: impl FnOnce(&mut Asm)) -> F {
    let mut asm = Asm::new();
    build(&mut asm);
    let code = asm.finish().expect("every label bound and in range");
    assert_eq!(
        code.len() % 4,
        0,
        "A64 code must be a whole number of words"
    );
    let buffer = JitBuffer::new(&code).expect("executable mapping");
    let entry = buffer.leak();
    assert_eq!(entry as usize % 4, 0, "entry must be instruction-aligned");
    // SAFETY: `F` is a function pointer, the same size as the code pointer; the
    // mapping is read+execute and leaked, and each caller declares the signature
    // its own emitted code implements.
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const u8>());
    unsafe { std::mem::transmute_copy(&entry) }
}

#[test]
fn add_of_two_arguments() {
    let f: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::add(0, 0, 1));
        a.word(a64::ret());
    });
    assert_eq!(f(7, 35), 42);
    assert_eq!(f(u64::MAX, 1), 0, "wraps like the hardware, no trap");
}

#[test]
fn materialised_constants_come_back_intact() {
    // Spans every shape `mov_imm64` chooses between: one MOVZ, one MOVN, and
    // MOVK fill-ins for two, three and four live halves.
    for value in [
        0u64,
        1,
        42,
        0xffff,
        0x1234_0000,
        u64::MAX,
        u64::MAX - 1,
        0xffff_ffff_0000_ffff,
        0x0000_5678_0000_def0,
        0x1234_5678_9abc_def0,
        0x8000_0000_0000_0000,
        // A scudo heap pointer: bit 63 set, which is where a signed-immediate
        // assumption would go wrong.
        0xb400_007d_1234_5678,
    ] {
        let f: extern "C" fn() -> u64 = compile(|a| {
            let mut words = Vec::new();
            a64::mov_imm64(0, value, &mut words);
            assert!(!words.is_empty() && words.len() <= 4);
            a.words(&words);
            a.word(a64::ret());
        });
        assert_eq!(f(), value, "materialising {value:#018x}");
    }
}

#[test]
fn backward_branch_drives_a_loop() {
    // Sum 1..=n, so both the conditional exit and the backward branch must
    // resolve correctly or the result is wrong rather than merely slow.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        let top = a.label();
        let done = a.label();
        let mut words = Vec::new();
        a64::mov_imm64(1, 0, &mut words); // accumulator
        a64::mov_imm64(2, 1, &mut words); // counter
        a.words(&words);
        a.bind(top);
        a.word(a64::cmp(2, 0));
        a.jcc(Cc::G, done); // counter > n
        a.word(a64::add(1, 1, 2));
        a.word(a64::add_imm(2, 2, 1).unwrap());
        a.jmp(top);
        a.bind(done);
        a.word(a64::mov(0, 1));
        a.word(a64::ret());
    });
    assert_eq!(f(0), 0);
    assert_eq!(f(1), 1);
    assert_eq!(f(10), 55);
    assert_eq!(f(100_000), 100_000 * 100_001 / 2);
}

#[test]
fn overflow_condition_selects_the_guard_edge() {
    // The fixnum-arithmetic shape: ADDS then a `Cc::O` branch to a deopt stub.
    // `Cc::O` must encode VS; VC would invert every fixnum guard in T1.
    let f: extern "C" fn(i64, i64) -> u64 = compile(|a| {
        let overflowed = a.label();
        a.word(a64::adds(0, 0, 1));
        a.jcc(Cc::O, overflowed);
        let mut words = Vec::new();
        a64::mov_imm64(0, 0, &mut words);
        a.words(&words);
        a.word(a64::ret());
        a.bind(overflowed);
        let mut words = Vec::new();
        a64::mov_imm64(0, 1, &mut words);
        a.words(&words);
        a.word(a64::ret());
    });
    assert_eq!(f(1, 1), 0, "no overflow");
    assert_eq!(f(i64::MAX, 1), 1, "signed overflow");
    assert_eq!(f(i64::MIN, -1), 1, "signed overflow the other way");
    assert_eq!(f(-1, 1), 0, "carry out is not overflow");
    assert_eq!(f(i64::MAX, -1), 0);
}

extern "C" fn triple(value: u64) -> u64 {
    value * 3
}

#[test]
fn frame_prologue_survives_an_indirect_call() {
    // Saving and restoring the link register is the whole point: without the
    // STP/LDP pair the BLR would clobber LR and RET would jump into the callee.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::stp_pre(FP, LR, SP, -16).unwrap());
        a.word(a64::mov(FP, SP));
        let mut words = Vec::new();
        a64::mov_imm64(9, triple as *const () as u64, &mut words);
        a.words(&words);
        a.word(a64::blr(9));
        a.word(a64::add_imm(0, 0, 1).unwrap());
        a.word(a64::ldp_post(FP, LR, SP, 16).unwrap());
        a.word(a64::ret());
    });
    assert_eq!(f(14), 43);
}

#[test]
fn logical_immediates_mask_as_written() {
    // Untagging by mask, and the tag test that decides whether to.
    let untag: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::and_imm(0, 0, 0xffff_ffff_ffff_fff8).unwrap());
        a.word(a64::ret());
    });
    assert_eq!(untag(0xb400_007d_1234_5679), 0xb400_007d_1234_5678);

    let tag_is_three: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::and_imm(1, 0, 0x7).unwrap());
        a.word(a64::cmp_imm(1, 3).unwrap());
        a.word(a64::cset(0, Cc::E));
        a.word(a64::ret());
    });
    assert_eq!(tag_is_three(0x1000_0003), 1);
    assert_eq!(tag_is_three(0x1000_0004), 0);

    let nonzero_low_bits: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::tst_imm(0, 0xf).unwrap());
        a.word(a64::cset(0, Cc::Ne));
        a.word(a64::ret());
    });
    assert_eq!(nonzero_low_bits(0x30), 0);
    assert_eq!(nonzero_low_bits(0x31), 1);
}

#[test]
fn shifts_round_trip_a_tagged_fixnum() {
    // Tag with a 3-bit shift, then recover the signed value arithmetically.
    let roundtrip: extern "C" fn(i64) -> i64 = compile(|a| {
        a.word(a64::lsl_imm(0, 0, 3));
        a.word(a64::orr_imm(0, 0, 0x1).unwrap());
        a.word(a64::asr_imm(0, 0, 3));
        a.word(a64::ret());
    });
    for value in [0i64, 1, -1, 42, -42, 1 << 50, -(1 << 50)] {
        assert_eq!(roundtrip(value), value, "tagging {value}");
    }

    let by_register: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::lsl_reg(0, 0, 1));
        a.word(a64::ret());
    });
    assert_eq!(by_register(1, 40), 1 << 40);
}

#[test]
fn conditional_select_computes_a_maximum() {
    let max: extern "C" fn(i64, i64) -> i64 = compile(|a| {
        a.word(a64::cmp(0, 1));
        a.word(a64::csel(0, 0, 1, Cc::Ge));
        a.word(a64::ret());
    });
    assert_eq!(max(3, 9), 9);
    assert_eq!(max(9, 3), 9);
    assert_eq!(max(-9, -3), -3);
    assert_eq!(max(7, 7), 7);
}

#[test]
fn loads_and_stores_move_values_through_a_frame() {
    // Spill to a frame slot and reload, via both the scaled and unscaled forms
    // and the single-register push/pop shapes.
    let f: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::sub_imm(SP, SP, 32).unwrap());
        a.word(a64::str_imm(0, SP, 8).unwrap()); // positive scaled offset
        a.word(a64::str_pre(1, SP, -16).unwrap()); // push, moving SP down
        a.word(a64::ldr_post(2, SP, 16).unwrap()); // pop it back into x2
        a.word(a64::ldur(3, SP, 8).unwrap()); // unscaled read of the spill
        a.word(a64::add(0, 2, 3));
        a.word(a64::add_imm(SP, SP, 32).unwrap());
        a.word(a64::ret());
    });
    assert_eq!(f(1000, 234), 1234);
}

#[test]
fn indexed_load_reads_a_vector_element() {
    let values: [u64; 4] = [10, 20, 30, 40];
    let f: extern "C" fn(*const u64, u64) -> u64 = compile(|a| {
        a.word(a64::ldr_indexed(0, 0, 1));
        a.word(a64::ret());
    });
    for (index, expected) in values.iter().enumerate() {
        assert_eq!(f(values.as_ptr(), index as u64), *expected);
    }
}

#[test]
fn double_arithmetic_runs_in_the_fp_registers() {
    // Arguments and results as bit patterns, so the FMOVs between the general
    // and FP register files are on the path too.
    let add: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::fmov_from_gpr(0, 0));
        a.word(a64::fmov_from_gpr(1, 1));
        a.word(a64::fadd(0, 0, 1));
        a.word(a64::fmov_to_gpr(0, 0));
        a.word(a64::ret());
    });
    assert_eq!(
        f64::from_bits(add(0.5f64.to_bits(), 0.25f64.to_bits())),
        0.75
    );

    let int_sqrt: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::scvtf(0, 0));
        a.word(a64::fsqrt(0, 0));
        a.word(a64::fcvtzs(0, 0));
        a.word(a64::ret());
    });
    assert_eq!(int_sqrt(144), 12);
    assert_eq!(int_sqrt(145), 12, "truncates toward zero");

    let compare: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::fmov_from_gpr(0, 0));
        a.word(a64::fmov_from_gpr(1, 1));
        a.word(a64::fcmp(0, 1));
        a.word(a64::cset(0, Cc::L));
        a.word(a64::ret());
    });
    assert_eq!(compare(1.0f64.to_bits(), 2.0f64.to_bits()), 1);
    assert_eq!(compare(2.0f64.to_bits(), 1.0f64.to_bits()), 0);

    let negate: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::fmov_from_gpr(0, 0));
        a.word(a64::fneg(0, 0));
        a.word(a64::fmov_to_gpr(0, 0));
        a.word(a64::ret());
    });
    assert_eq!(f64::from_bits(negate(1.5f64.to_bits())), -1.5);
}

#[test]
fn multiply_and_negate() {
    let f: extern "C" fn(i64, i64, i64) -> i64 = compile(|a| {
        a.word(a64::madd(0, 0, 1, 2));
        a.word(a64::neg(0, 0));
        a.word(a64::ret());
    });
    assert_eq!(f(6, 7, 1), -43);

    let shifted_add: extern "C" fn(u64, u64) -> u64 = compile(|a| {
        a.word(a64::add_shifted(0, 0, 1, a64::Shift::Lsl(3)));
        a.word(a64::ret());
    });
    assert_eq!(shifted_add(100, 5), 140, "base + index*8");
}

#[test]
fn many_branches_to_one_stub_all_land() {
    // The T1 shape: several guards sharing a single deopt stub, far enough apart
    // that each fixup carries a different displacement.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        let stub = a.label();
        a.word(a64::cmp_imm(0, 1).unwrap());
        a.jcc(Cc::E, stub);
        for _ in 0..64 {
            a.word(a64::nop());
        }
        a.word(a64::cmp_imm(0, 2).unwrap());
        a.jcc(Cc::E, stub);
        for _ in 0..64 {
            a.word(a64::nop());
        }
        a.word(a64::cmp_imm(0, 3).unwrap());
        a.jcc(Cc::E, stub);
        let mut words = Vec::new();
        a64::mov_imm64(0, 0, &mut words);
        a.words(&words);
        a.word(a64::ret());
        a.bind(stub);
        let mut words = Vec::new();
        a64::mov_imm64(0, 999, &mut words);
        a.words(&words);
        a.word(a64::ret());
    });
    assert_eq!(f(1), 999);
    assert_eq!(f(2), 999);
    assert_eq!(f(3), 999);
    assert_eq!(f(4), 0);
}

#[test]
fn frame_base_round_trips_through_a_register() {
    // `MOV Xd, SP` is not an ORR: register 31 reads as XZR in the logical
    // encoding, so a naive `mov(FP, SP)` yields zero and the epilogue restores
    // SP from nothing. This caught a live bug in the T1 emitter, where the frame
    // base was silently zero and only an unbalanced SP would have exposed it.
    let f: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::stp_pre(FP, LR, SP, -32).unwrap());
        a.word(a64::mov_from_sp(FP));
        // Move SP somewhere else, then prove the epilogue can get back from x29
        // alone rather than by unwinding a balanced sequence.
        a.word(a64::sub_imm(SP, SP, 64).unwrap());
        a.word(a64::str_imm(0, SP, 8).unwrap());
        a.word(a64::ldr_imm(0, SP, 8).unwrap());
        a.word(a64::mov_to_sp(FP));
        a.word(a64::ldp_post(FP, LR, SP, 32).unwrap());
        a.word(a64::ret());
    });
    assert_eq!(f(0xfeed), 0xfeed);
    // Called twice: a leaked stack pointer would show up as drift, not on the
    // first call.
    assert_eq!(f(0xbeef), 0xbeef);
}

#[test]
fn an_upward_growing_operand_stack_pushes_and_pops() {
    // The T1 operand stack: a pointer that advances after a store and retreats
    // before a load. `str_post` and `ldr_pre` are mirror images, and getting one
    // of them wrong is invisible whenever the accumulator happens to still hold
    // the value being returned — which is most simple functions.
    let mut cells = [0u64; 8];
    let f: extern "C" fn(*mut u64, u64, u64, u64) -> u64 = compile(|a| {
        // x0 = stack base, x1..x3 = three values to push.
        a.word(a64::mov(20, 0)); // the operand-stack pointer
        for value in 1..=3u8 {
            a.word(a64::mov(0, value));
            a.word(a64::str_post(0, 20, 8).unwrap());
        }
        // Pop them back in reverse and sum: 3 + 2*10 + 1*100 = 123.
        a.word(a64::ldr_pre(4, 20, -8).unwrap());
        a.word(a64::ldr_pre(5, 20, -8).unwrap());
        a.word(a64::ldr_pre(6, 20, -8).unwrap());
        let mut w = Vec::new();
        a64::mov_imm64(7, 10, &mut w);
        a.words(&w);
        a.word(a64::madd(4, 5, 7, 4)); // 3 + 2*10
        let mut w = Vec::new();
        a64::mov_imm64(7, 100, &mut w);
        a.words(&w);
        a.word(a64::madd(0, 6, 7, 4)); // + 1*100
        a.word(a64::ret());
    });
    assert_eq!(f(cells.as_mut_ptr(), 1, 2, 3), 123);
    assert_eq!(
        &cells[..3],
        &[1, 2, 3],
        "the pushes must reach memory in order"
    );
}

#[test]
fn register_31_is_the_stack_pointer_only_in_some_forms() {
    // The trap that cost a working T2 frame: register 31 reads as SP in the
    // add/sub-IMMEDIATE and load/store forms, and as XZR in the shifted-register
    // ones. So `add_imm(d, SP, n)` reads the stack pointer while `add(d, SP, m)`
    // reads zero — the same encoding-level distinction, opposite meanings, and no
    // assembler error either way.
    let via_immediate: extern "C" fn() -> u64 = compile(|a| {
        a.word(a64::mov_from_sp(0));
        a.word(a64::add_imm(1, SP, 8).unwrap());
        // x1 - x0 is 8 if the add really read SP.
        a.word(a64::sub(0, 1, 0));
        a.word(a64::ret());
    });
    assert_eq!(
        via_immediate(),
        8,
        "add-immediate must read register 31 as SP"
    );

    let via_shifted_register: extern "C" fn() -> u64 = compile(|a| {
        let mut w = Vec::new();
        a64::mov_imm64(2, 8, &mut w);
        a.words(&w);
        // Deliberately the wrong form: this reads XZR, so the result is 8, not
        // sp + 8. Pinned so the difference is a fact rather than a comment.
        a.word(a64::add(0, SP, 2));
        a.word(a64::ret());
    });
    assert_eq!(
        via_shifted_register(),
        8,
        "a shifted-register add reads register 31 as XZR, not SP"
    );

    // A real frame: claim stack, use it through SP-relative accesses both inside
    // and beyond the scaled immediate field, and restore from the frame pointer.
    let frame: extern "C" fn(u64) -> u64 = compile(|a| {
        a.word(a64::stp_pre(FP, LR, SP, -16).unwrap());
        a.word(a64::mov_from_sp(FP));
        a.word(a64::sub_imm(SP, SP, 4096).unwrap());
        a.word(a64::str_imm(0, SP, 8).unwrap());
        // Past the scaled field (32760 max), so via the indexed form.
        let mut w = Vec::new();
        a64::mov_imm64(3, 4088 / 8, &mut w);
        a.words(&w);
        a.word(a64::str_indexed(0, SP, 3));
        a.word(a64::ldr_imm(1, SP, 8).unwrap());
        a.word(a64::ldr_indexed(2, SP, 3));
        a.word(a64::add(0, 1, 2));
        a.word(a64::mov_to_sp(FP));
        a.word(a64::ldp_post(FP, LR, SP, 16).unwrap());
        a.word(a64::ret());
    });
    assert_eq!(
        frame(21),
        42,
        "both frame accesses must reach the same slot values"
    );
    assert_eq!(frame(100), 200);
}
