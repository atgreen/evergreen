// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Wave-1 cross-parcel integration: P1 (build) → P2 (verify) → P3 (infer).
//!
//! Each parcel was unit-tested in isolation against hand-built IR. This proves
//! they actually compose: bytecode built by P1 must satisfy P2's verifier and be
//! analysable by P3's inference, all against the frozen Phase-0 contracts.

use egcl_compiler::t2::build::build_from_bytecode;
use egcl_compiler::t2::infer::infer;
use egcl_compiler::t2::lower::lower;
use egcl_compiler::t2::opt_dce::Dce;
use egcl_compiler::t2::opt_escape::EscapeAnalysis;
use egcl_compiler::t2::opt_fold::ConstFold;
use egcl_compiler::t2::opt_guard::GuardElim;
use egcl_compiler::t2::opt_gvn::Gvn;
use egcl_compiler::t2::opt_licm::Licm;
use egcl_compiler::t2::pass::PassManager;
use egcl_compiler::t2::regalloc::allocate;
use egcl_compiler::t2::verify::verify;
use egcl_rt::bytecode::{BytecodeFunction, Instr};
#[cfg(all(target_arch = "x86_64", unix))]
use egcl_rt::bytecode::typep_class;
#[cfg(all(target_arch = "x86_64", unix))]
use egcl_rt::object::{ConsCell, ObjectHeader, type_id};
use egcl_rt::value::{NIL, EgclVal};
#[cfg(all(target_arch = "x86_64", unix))]
use egcl_rt::value::T;

fn bytecode_fn(
    name: &str,
    code: Vec<Instr>,
    constants: Vec<EgclVal>,
    n_locals: u16,
    max_stack: u16,
    arity: u16,
) -> BytecodeFunction {
    BytecodeFunction {
        code,
        constants,
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals,
        max_stack,
        arity,
        name: name.to_string(),
        params_form: NIL,
        min_args: arity,
        max_args: Some(arity),
        variadic: false,
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn named_calls_reload_fixed_slots_without_resolving_symbols() {
    use egcl_compiler::t2::emit::{emit_framed_with_direct_natives, NamedCallTarget};
    use std::sync::atomic::{AtomicUsize, Ordering};

    extern "C" fn first(cell: u64, n: u64, _: u64, _: u64, _: u64, profile: u64) -> u64 {
        assert_eq!(cell, 0x1234);
        assert!(n <= 3);
        assert_eq!(profile, 0);
        EgclVal::from_fixnum(41).0
    }
    extern "C" fn second(cell: u64, n: u64, _: u64, _: u64, _: u64, profile: u64) -> u64 {
        assert_eq!(cell, 0x1234);
        assert!(n <= 3);
        assert_eq!(profile, 0);
        EgclVal::from_fixnum(99).0
    }
    extern "C" fn first_slice(cell: u64, n: u64, args: *const EgclVal, profile: u64) -> u64 {
        assert_eq!((cell, n, profile), (0x1234, 4, 0));
        assert_eq!(unsafe { *args.add(3) }, NIL);
        EgclVal::from_fixnum(41).0
    }
    extern "C" fn second_slice(cell: u64, n: u64, args: *const EgclVal, profile: u64) -> u64 {
        assert_eq!((cell, n, profile), (0x1234, 4, 0));
        assert_eq!(unsafe { *args.add(3) }, NIL);
        EgclVal::from_fixnum(99).0
    }
    // Deliberately absent from the symbol registry: execution must consume the
    // provided slot, not look up a symbol or resolve a runtime function value.
    let symbol = 0x7fff_ffff;
    for nargs in [0, 1, 3, 4] {
        let registers = AtomicUsize::new(first as *const () as usize);
        let slice = AtomicUsize::new(first_slice as *const () as usize);
        let target = NamedCallTarget {
            symbol,
            cell_address: 0x1234,
            register_entry: &registers as *const AtomicUsize as u64,
            slice_entry: &slice as *const AtomicUsize as u64,
        };
        let mut instructions = vec![Instr::Const(0); nargs as usize];
        instructions.push(Instr::CallNamed { sym: symbol, nargs });
        instructions.push(Instr::Return);
        let body = bytecode_fn("slot-caller", instructions, vec![NIL], 0, nargs.max(1), 0);
        let ir = build_from_bytecode(&body).unwrap();
        let framed = emit_framed_with_direct_natives(
            &ir, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            body.num_slots(), None, &[], 0, &[target], Default::default(),
        ).unwrap();
        let code = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
        let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
        let mut slots = vec![NIL.0; (body.num_slots() + framed.shadow_root_slots) as usize];
        assert_eq!(run(slots.as_mut_ptr()), EgclVal::from_fixnum(41).0);
        registers.store(second as *const () as usize, Ordering::Release);
        slice.store(second_slice as *const () as usize, Ordering::Release);
        assert_eq!(run(slots.as_mut_ptr()), EgclVal::from_fixnum(99).0);
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn deoptimizing_native_entries_report_their_own_code_and_frame_kind() {
    use egcl_compiler::t2::emit::emit_framed_with_direct_natives;
    use egcl_compiler::t2::speculate::{speculate, SpecType};
    use std::sync::atomic::{AtomicU64, Ordering};

    static ENTRY: AtomicU64 = AtomicU64::new(0);
    extern "C" fn resume(scopes: u64, words: u64, values: *const u64, entry: u64) -> u64 {
        assert_eq!(scopes, 1);
        assert!(words >= 4);
        assert!(!values.is_null());
        ENTRY.store(entry, Ordering::Relaxed);
        EgclVal::from_fixnum(987).0
    }
    extern "C" fn legacy_resume() -> u64 { EgclVal::from_fixnum(-1).0 }
    let plus = egcl_rt::symbols::intern("+");
    let body = bytecode_fn("native-deopt-callee", vec![
        Instr::LoadLocal(0), Instr::Const(0),
        Instr::CallNamed { sym: plus, nargs: 2 }, Instr::Return,
    ], vec![EgclVal::from_fixnum(1)], 1, 2, 1);
    let mut ir = build_from_bytecode(&body).unwrap();
    assert!(speculate(&mut ir, &|_| Some(SpecType::Fixnum)) > 0);
    let code_id = 0x1234;
    let framed = emit_framed_with_direct_natives(
        &ir, legacy_resume as *const () as u64, resume as *const () as u64, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        body.num_slots(), None, &[], code_id, &[], Default::default(),
    ).unwrap();
    let code = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
    let mut slots = vec![NIL.0; (body.num_slots() + framed.shadow_root_slots) as usize];
    assert_eq!(run(slots.as_mut_ptr()), EgclVal::from_fixnum(987).0);
    assert_eq!(ENTRY.load(Ordering::Relaxed), code_id, "frame entry owns its callee frame");
    assert_ne!(framed.compiled_entry, 0);
    let registers: extern "C" fn(u64, u64, u64, u64) -> u64 = unsafe {
        std::mem::transmute(code.as_ptr().add(framed.compiled_entry))
    };
    // The existing compiled entry receives its first Lisp argument in RCX.
    assert_eq!(registers(0, 0, 0, NIL.0), EgclVal::from_fixnum(987).0);
    assert_eq!(ENTRY.load(Ordering::Relaxed), code_id | (1 << 63),
        "register entry asks the deoptimizer to materialize its own frame");
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn native_transfer_check_covers_pure_self_recursion_and_mixed_calls() {
    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;

    let self_sym = egcl_rt::symbols::intern("TRANSFER-CHECK-SELF");
    let mixed_sym = egcl_rt::symbols::intern("TRANSFER-CHECK-MIXED");
    let other_sym = egcl_rt::symbols::intern("TRANSFER-CHECK-OTHER");

    let pure = bytecode_fn(
        "TRANSFER-CHECK-SELF",
        vec![
            Instr::CallNamed {
                sym: self_sym,
                nargs: 0,
            },
            Instr::Return,
        ],
        vec![],
        0,
        1,
        0,
    );
    let mixed = bytecode_fn(
        "TRANSFER-CHECK-MIXED",
        vec![
            Instr::CallNamed {
                sym: other_sym,
                nargs: 0,
            },
            Instr::Return,
        ],
        vec![],
        0,
        1,
        0,
    );

    let pure_ir = build_from_bytecode(&pure).expect("build pure self recursion");
    let mixed_ir = build_from_bytecode(&mixed).expect("build mixed call");
    let emit = |f: &egcl_compiler::t2::ir::Function, sym| {
        emit_framed_with_activation_slots(
            f,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0xfeed,
            1,
            1,
            Some(sym),
        )
        .expect("emit transfer-check probe")
    };

    let pure_with_poll = emit(&pure_ir, self_sym);
    let pure_without_poll = emit_framed_with_activation_slots(
        &pure_ir, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, Some(self_sym),
    )
    .expect("emit pure self without transfer poll");
    assert_ne!(
        pure_with_poll.code,
        pure_without_poll.code,
        "pure self recursion must check transfers from its stack-limit fallback"
    );

    let mixed_with_poll = emit(&mixed_ir, mixed_sym);
    let mixed_without_poll = emit_framed_with_activation_slots(
        &mixed_ir,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        1,
        Some(mixed_sym),
    )
    .expect("emit mixed call without transfer poll");
    assert_ne!(
        mixed_with_poll.code,
        mixed_without_poll.code,
        "mixed calls must retain the transfer-status branch"
    );
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn direct_self_call_stack_fallback_propagates_transfers() {
    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const CHILD: &str = "EGCL_TEST_SELF_STACK_FALLBACK";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "direct_self_call_stack_fallback_propagates_transfers",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("EGCL_NO_DIRECT_SELF_CALL")
            .output()
            .expect("run isolated stack-limit test");
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static POLLS: AtomicUsize = AtomicUsize::new(0);
    static PENDING: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn call(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        CALLS.fetch_add(1, Ordering::Relaxed);
        EgclVal::from_fixnum(42).0
    }
    extern "C" fn pending() -> u64 {
        POLLS.fetch_add(1, Ordering::Relaxed);
        PENDING.load(Ordering::Relaxed) as u64
    }

    let sym = egcl_rt::symbols::intern("STACK-FALLBACK-SELF");
    for arity in [0u16, 1, 3] {
        let mut instructions: Vec<_> = (0..arity).map(Instr::LoadLocal).collect();
        instructions.extend([
            Instr::CallNamed { sym, nargs: arity },
            Instr::Pop,
            Instr::Const(0),
            Instr::Return,
        ]);
        let bf = bytecode_fn(
            "STACK-FALLBACK-SELF",
            instructions,
            vec![EgclVal::from_fixnum(99)],
            arity,
            arity + 1,
            arity,
        );
        let mut ir = build_from_bytecode(&bf).expect("build recursive function");
        // Model fixnum-specialized inputs so GC roots do not require an
        // activation frame and disable direct register calls in this probe.
        for param in ir.block(ir.entry()).params.clone() {
            ir.refine_type(
                param,
                egcl_compiler::t2::ir::IRType::of(egcl_compiler::t2::ir::TypeBits::FIXNUM),
            );
        }
        let framed = emit_framed_with_activation_slots(
            &ir,
            0,
            0,
            call as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            pending as *const () as usize as u64,
            0,
            bf.num_slots(),
            Some(sym),
        )
        .expect("emit recursive function");
        assert_ne!(framed.compiled_entry, 0, "exercise direct register recursion");
        let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
        let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
        let mut frame = vec![EgclVal::from_fixnum(12).0; usize::from(bf.num_slots())];
        for nested in [false, true] {
            for transfer in [false, true] {
                CALLS.store(0, Ordering::Relaxed);
                POLLS.store(0, Ordering::Relaxed);
                PENDING.store(usize::from(transfer), Ordering::Relaxed);
                let rsp: u64;
                unsafe {
                    core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
                }
                let limit = if nested { rsp - 1024 } else { i64::MAX as u64 };
                egcl_rt::stack::NATIVE_STACK_LIMIT.store(limit, Ordering::Release);
                let result = run(frame.as_mut_ptr());
                egcl_rt::stack::NATIVE_STACK_LIMIT.store(0, Ordering::Release);
                assert_eq!(
                    CALLS.load(Ordering::Relaxed), 1,
                    "must reach the bounded fallback"
                );
                assert_eq!(
                    result,
                    if transfer { NIL.0 } else { EgclVal::from_fixnum(99).0 },
                    "arity {arity}, nested {nested}, transfer {transfer}"
                );
                if nested && transfer {
                    assert!(POLLS.load(Ordering::Relaxed) > 1, "unwind native callers too");
                }
            }
        }
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn self_calls_through_activation_slices_stop_on_pending_transfers() {
    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;

    extern "C" fn call_slice(_: u64, _: u64, _: *const EgclVal, _: u64) -> u64 {
        NIL.0
    }
    extern "C" fn pending() -> u64 {
        1
    }

    let sym = egcl_rt::symbols::intern("TRANSFER-SLICE-SELF");
    for arity in [4u16, 5, 7] {
        let mut instructions: Vec<_> = (0..arity).map(Instr::LoadLocal).collect();
        instructions.extend([
            Instr::CallNamed { sym, nargs: arity },
            Instr::Pop,
            Instr::Const(0),
            Instr::Return,
        ]);
        let bf = bytecode_fn(
            "TRANSFER-SLICE-SELF",
            instructions,
            vec![EgclVal::from_fixnum(99)],
            arity,
            arity + 1,
            arity,
        );
        let ir = build_from_bytecode(&bf).expect("build self-call slice");
        let framed = emit_framed_with_activation_slots(
            &ir,
            0,
            0,
            0,
            call_slice as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            pending as *const () as usize as u64,
            0,
            bf.num_slots(),
            Some(sym),
        )
        .expect("emit self-call slice");
        assert_eq!(framed.compiled_entry, 0, "slice call needs an activation");
        let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).expect("executable memory");
        let run: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        let mut frame = vec![NIL.0; usize::from(bf.num_slots() + framed.shadow_root_slots)];
        assert_eq!(
            run(frame.as_mut_ptr()),
            NIL.0,
            "arity {arity}: pending transfer must exit before returning 99"
        );
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn emitted_heap_literal_is_loaded_from_its_constant_pool_slot() {
    use egcl_compiler::t2::emit::emit_framed;

    let first = EgclVal(0x1001);
    let second = EgclVal(0x2002);
    let mut bf = bytecode_fn(
        "heap-literal-slot",
        vec![Instr::Const(0), Instr::Return],
        vec![first],
        0,
        1,
        0,
    );
    let f = build_from_bytecode(&bf).expect("build heap literal");
    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).expect("emit heap literal");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [0u64];

    assert_eq!(EgclVal(run(frame.as_mut_ptr())), first);
    bf.constants[0] = second;
    assert_eq!(EgclVal(run(frame.as_mut_ptr())), second);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn inlined_heap_literal_uses_the_saved_body_constant_slot() {
    use std::sync::Arc;
    use egcl_compiler::t2::build::build_from_bytecode_with_inline_options;
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::inlining::InlineOptions;
    use egcl_compiler::t2::ir::Opcode;

    let helper = egcl_rt::symbols::intern("INLINE-HEAP-LITERAL-HELPER");
    let caller = egcl_rt::symbols::intern("INLINE-HEAP-LITERAL-CALLER");
    let first = EgclVal(0x3001);
    let second = EgclVal(0x4002);
    let mut helper_body = Arc::new(bytecode_fn(
        "INLINE-HEAP-LITERAL-HELPER",
        vec![Instr::Const(0), Instr::Return],
        vec![first],
        0,
        1,
        0,
    ));
    let caller_body = bytecode_fn(
        "INLINE-HEAP-LITERAL-CALLER",
        vec![
            Instr::CallNamed {
                sym: helper,
                nargs: 0,
            },
            Instr::Return,
        ],
        vec![],
        0,
        1,
        0,
    );
    let options = InlineOptions::default()
        .with_root_symbol(caller)
        .with_body(helper, Arc::clone(&helper_body));
    let f = build_from_bytecode_with_inline_options(&caller_body, options)
        .expect("inline heap-literal helper");
    assert!(f.block_order().iter().all(|&block| {
        f.block(block)
            .insts
            .iter()
            .all(|&inst| f.inst(inst).opcode != Opcode::Call)
    }));
    let framed =
        emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, Some(caller)).expect("emit inlined heap literal");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [0u64];

    assert_eq!(EgclVal(run(frame.as_mut_ptr())), first);
    Arc::get_mut(&mut helper_body)
        .expect("options dropped")
        .constants[0] = second;
    assert_eq!(EgclVal(run(frame.as_mut_ptr())), second);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn integerp_intrinsic_accepts_fixnums_and_bignums_without_a_call() {
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::ir::Opcode;

    let integerp = egcl_rt::symbols::intern("INTEGERP");
    let bf = bytecode_fn(
        "integer-predicate",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: integerp,
                nargs: 1,
            },
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    );
    let f = build_from_bytecode(&bf).expect("build INTEGERP intrinsic");
    assert!(f.block_order().iter().any(|&b| {
        f.block(b)
            .insts
            .iter()
            .any(|&i| f.inst(i).opcode == Opcode::TypeCheck)
    }));
    assert!(!f.block_order().iter().any(|&b| {
        f.block(b)
            .insts
            .iter()
            .any(|&i| f.inst(i).opcode == Opcode::Call)
    }));

    let typep = egcl_rt::symbols::intern("TYPEP");
    let integer = egcl_rt::symbols::intern("INTEGER");
    let typep_bf = bytecode_fn(
        "integer-typep",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed {
                sym: typep,
                nargs: 2,
            },
            Instr::Return,
        ],
        vec![EgclVal::from_symbol_index(integer)],
        1,
        2,
        1,
    );
    let typep_f = build_from_bytecode(&typep_bf).expect("build TYPEP INTEGER intrinsic");
    assert!(typep_f.block_order().iter().any(|&b| {
        typep_f
            .block(b)
            .insts
            .iter()
            .any(|&i| typep_f.inst(i).opcode == Opcode::TypeCheck)
    }));
    assert!(!typep_f.block_order().iter().any(|&b| {
        typep_f
            .block(b)
            .insts
            .iter()
            .any(|&i| typep_f.inst(i).opcode == Opcode::Call)
    }));

    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).expect("emit INTEGERP intrinsic");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let run = |value: EgclVal| {
        let mut frame = [value.0, 0u64];
        EgclVal(func(frame.as_mut_ptr()))
    };

    assert_eq!(run(EgclVal::from_fixnum(42)), T);
    assert_eq!(run(NIL), NIL);

    let bignum = Box::new(ObjectHeader::new(type_id::BIGNUM, 1));
    let bignum = unsafe { EgclVal::from_heap_ptr(Box::into_raw(bignum).cast::<u8>()) };
    assert_eq!(run(bignum), T);

    let string = Box::new(ObjectHeader::new(type_id::SIMPLE_BASE_STRING, 1));
    let string = unsafe { EgclVal::from_heap_ptr(Box::into_raw(string).cast::<u8>()) };
    assert_eq!(run(string), NIL);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn bytecode_typep_boolean_executes_and_clears_multiple_values() {
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::ir::{AuxData, Opcode};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CLEARS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn clear_values(_primary: u64, _dst: *mut EgclVal, count: u64) {
        assert_eq!(count, 0);
        CLEARS.fetch_add(1, Ordering::Relaxed);
    }

    let bf = bytecode_fn(
        "boolean-typep-opcode",
        vec![
            Instr::LoadLocal(0),
            Instr::TypeP(typep_class::BOOLEAN),
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    );
    let f = build_from_bytecode(&bf).expect("build TypeP BOOLEAN");
    let check = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter())
        .map(|&i| f.inst(i))
        .find(|d| d.opcode == Opcode::TypeCheck)
        .expect("TypeP must become TypeCheck");
    assert!(matches!(
        check.aux,
        AuxData::TypepClass(typep_class::BOOLEAN)
    ));
    assert!(!check.flags.effectful);
    assert!(!check.flags.guard);
    assert!(check.frame_state.is_none());

    let framed = emit_framed(
        &f, 0, 0, 0, 0, 0, 0, 0,
        clear_values as *const () as usize as u64,
        None,
    )
    .expect("emit TypeP BOOLEAN");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let run = |value: EgclVal| {
        let mut frame = [value.0, 0];
        EgclVal(func(frame.as_mut_ptr()))
    };

    assert_eq!(run(NIL), T);
    assert_eq!(run(T), T);
    assert_eq!(run(EgclVal::from_fixnum(0)), NIL);
    assert_eq!(
        run(EgclVal::from_symbol_index(egcl_rt::symbols::intern(
            "NOT-A-BOOLEAN"
        ))),
        NIL
    );
    assert_eq!(CLEARS.load(Ordering::Relaxed), 4);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn take_values_to_locals_materializes_secondary_ssa_results() {
    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;
    use egcl_compiler::t2::ir::Opcode;

    extern "C" fn take_values(primary: u64, dst: *mut EgclVal, n: u64) {
        assert_eq!(n, 3);
        unsafe {
            dst.write(EgclVal(primary));
            dst.add(1).write(T);
            dst.add(2).write(NIL);
        }
    }

    let bf = bytecode_fn(
        "take-three-values",
        vec![
            Instr::LoadLocal(0),
            Instr::TakeValuesToLocals {
                nvars: 3,
                slot_base: 1,
            },
            Instr::LoadLocal(2),
            Instr::Return,
        ],
        vec![],
        4,
        1,
        1,
    );
    let f = build_from_bytecode(&bf).expect("build TakeValuesToLocals");
    assert!(f.block_order().iter().any(|&block| {
        f.block(block)
            .insts
            .iter()
            .any(|&inst| f.inst(inst).opcode == Opcode::TakeValuesToLocals)
    }));

    let framed = emit_framed_with_activation_slots(
        &f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        take_values as *const () as usize as u64,
        0,
        0,
        0,
        bf.num_slots(),
        None,
    )
    .expect("emit TakeValuesToLocals");
    assert_eq!(
        framed.compiled_entry, 0,
        "MV copy requires its frame pointer"
    );
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let primary = EgclVal::from_fixnum(42);
    let mut frame = vec![NIL.0; bf.num_slots() as usize + framed.shadow_root_slots as usize];
    frame[0] = primary.0;

    assert_eq!(EgclVal(func(frame.as_mut_ptr())), T);
    assert_eq!(EgclVal(frame[1]), primary);
    assert_eq!(EgclVal(frame[2]), T);
    assert_eq!(EgclVal(frame[3]), NIL);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn stringp_retains_semantic_call_instead_of_simple_string_typecheck() {
    use egcl_compiler::t2::ir::{AuxData, Opcode};

    let stringp = egcl_rt::symbols::intern("STRINGP");
    let bf = bytecode_fn(
        "string-predicate",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: stringp,
                nargs: 1,
            },
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    );
    let f = build_from_bytecode(&bf).expect("build STRINGP call");
    verify(&f).expect("STRINGP fallback has valid frame state and operands");
    // STRINGP also accepts adjustable, displaced, and fill-pointer strings.
    // The raw STRING type check only recognizes simple strings (bliss-c02n),
    // so metadata must decline that expansion and retain the semantic call.
    let call = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter())
        .map(|&i| f.inst(i))
        .find(|d| d.opcode == Opcode::Call)
        .expect("STRINGP must preserve the full string predicate");
    assert!(matches!(call.aux, AuxData::CallTarget(sym) if sym == stringp));
    assert_eq!(call.args.len(), 1);
    assert!(!f.block_order().iter().any(|&b| {
        f.block(b)
            .insts
            .iter()
            .any(|&i| f.inst(i).opcode == Opcode::TypeCheck)
    }));
}

#[cfg(all(target_arch = "x86_64", unix))]
static CONS_ACCESS_DEOPTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(target_arch = "x86_64", unix))]
extern "C" fn cons_access_deopt() {
    CONS_ACCESS_DEOPTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(all(target_arch = "x86_64", unix))]
struct NativeRootMoveState {
    frame: usize,
    activation_slots: usize,
    root_count: usize,
    clear_symbol: u32,
    move_symbol: u32,
    check_symbols: Vec<u32>,
    old_values: Vec<u64>,
    expected_by_check: std::collections::HashMap<u32, u64>,
    checks_seen: usize,
    moved: bool,
    failed: bool,
}

#[cfg(all(target_arch = "x86_64", unix))]
static NATIVE_ROOT_MOVE_STATE: std::sync::Mutex<Option<NativeRootMoveState>> =
    std::sync::Mutex::new(None);

#[cfg(all(target_arch = "x86_64", unix))]
extern "C" fn moving_gc_c2i(
    sym: u64,
    _nargs: u64,
    arg0: u64,
    _arg1: u64,
    _arg2: u64,
    _reserved: u64,
) -> u64 {
    let (frame, activation_slots, root_count, clear_symbol, move_symbol) = {
        let state = NATIVE_ROOT_MOVE_STATE.lock().unwrap();
        let state = state.as_ref().expect("native-root move state");
        (
            state.frame as *mut egcl_rt::Frame,
            state.activation_slots,
            state.root_count,
            state.clear_symbol,
            state.move_symbol,
        )
    };
    if sym as u32 == clear_symbol {
        // Make the interpreter-visible activation deliberately stale. From this
        // point onward the objects exist only in native homes and the shadow
        // roots synchronized at each runtime boundary.
        unsafe {
            let slots = egcl_rt::EgclStack::frame_slots_mut(frame);
            for slot in &mut slots[..activation_slots] {
                *slot = NIL;
            }
        }
    } else if sym as u32 == move_symbol {
        // Run the actual moving minor collector while the activation slots are
        // stale and the objects exist only in synchronized native-root slots.
        let collected = egcl_rt::gc::collect_t0_minor().is_ok();
        let relocated = unsafe {
            let slots = egcl_rt::EgclStack::frame_slots_mut(frame);
            slots[activation_slots..activation_slots + root_count].to_vec()
        };
        let mut state = NATIVE_ROOT_MOVE_STATE.lock().unwrap();
        let state = state.as_mut().expect("native-root move state");
        state.failed |= !collected || relocated.len() != state.check_symbols.len();
        state.moved = relocated
            .iter()
            .zip(&state.old_values)
            .all(|(new, old)| new.0 != *old);
        for (&check, value) in state.check_symbols.iter().zip(relocated) {
            state.expected_by_check.insert(check, value.0);
        }
    } else {
        let mut state = NATIVE_ROOT_MOVE_STATE.lock().unwrap();
        let state = state.as_mut().expect("native-root move state");
        match state.expected_by_check.get(&(sym as u32)) {
            Some(&expected) if expected == arg0 => state.checks_seen += 1,
            _ => state.failed = true,
        }
    }
    NIL.0
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn moving_gc_relocates_t2_roots_in_registers_and_native_spills() {
    const CHILD: &str = "EGCL_T2_ROOT_MOVE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // This fixture performs a process-global stop-the-world collection and
        // needs the full framed register pool so it exercises both register and
        // spill relocation. Run it in an isolated test process: parallel sibling
        // tests otherwise race the safepoint handshake (bliss-reoy), while the
        // production reduced pool legitimately spills every long-lived root and
        // defeats the register-root coverage assertion (bliss-exvx).
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "moving_gc_relocates_t2_roots_in_registers_and_native_spills",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("EGCL_T2_FRAME_ENV", "full")
            .output()
            .expect("spawn isolated moving-GC fixture");
        assert!(
            output.status.success(),
            "isolated moving-GC fixture failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;
    use egcl_rt::CodeInfo;
    use egcl_rt::stack::StackMapEntry;

    const ROOTS: usize = 8;
    // Private callback IDs avoid interning symbols on the GC heap. Symbols are
    // pinned, and the current collector conservatively retains their complete
    // nursery region, which would make this moving-GC fixture non-moving.
    let clear_symbol = 0x7f00_1000;
    let move_symbol = 0x7f00_1001;
    let check_symbols: Vec<_> = (0..ROOTS).map(|i| 0x7f00_1010 + i as u32).collect();
    let mut code = vec![
        Instr::CallNamed {
            sym: clear_symbol,
            nargs: 0,
        },
        Instr::Pop,
        Instr::CallNamed {
            sym: move_symbol,
            nargs: 0,
        },
        Instr::Pop,
    ];
    for (i, &check) in check_symbols.iter().enumerate() {
        code.extend([
            Instr::LoadLocal(i as u16),
            Instr::CallNamed {
                sym: check,
                nargs: 1,
            },
            Instr::Pop,
        ]);
    }
    code.extend([Instr::LoadLocal(0), Instr::Return]);
    let bf = bytecode_fn(
        "native-root-move",
        code,
        vec![],
        ROOTS as u16,
        1,
        ROOTS as u16,
    );
    let f = build_from_bytecode(&bf).expect("build native-root moving-GC fixture");
    let activation_slots = bf.num_slots();
    let framed = emit_framed_with_activation_slots(
        &f,
        0,
        0,
        moving_gc_c2i as *const () as usize as u64,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        activation_slots,
        None,
    )
    .expect("emit native-root moving-GC fixture");
    assert_eq!(
        framed.compiled_entry, 0,
        "rooted code requires an owning frame"
    );
    assert!(framed.shadow_root_slots as usize >= ROOTS);
    let move_site = framed
        .root_sync_sites
        .get(1)
        .expect("moving-GC call site map");
    assert!(
        move_site.register_roots > 0,
        "fixture must keep a root in a GPR"
    );
    assert!(
        move_site.spill_roots > 0,
        "fixture must keep a root in a native spill"
    );
    assert_eq!(
        framed.emitted_safepoints,
        framed.root_sync_sites.len(),
        "every runtime call has synchronization metadata"
    );

    let total_slots = activation_slots + framed.shadow_root_slots;
    let mut bitmap = vec![0u8; (total_slots as usize).div_ceil(8)];
    for i in 0..total_slots as usize {
        bitmap[i / 8] |= 1 << (i % 8);
    }
    let bitmap: &'static [u8] = Box::leak(bitmap.into_boxed_slice());
    let maps: &'static [StackMapEntry] = Box::leak(
        vec![StackMapEntry {
            pc_offset: 0,
            bytes: bitmap.as_ptr() as usize,
            len: bitmap.len(),
        }]
        .into_boxed_slice(),
    );
    let code_info = CodeInfo::new(&[], maps);
    let stack = egcl_rt::current_stack();
    let frame = stack
        .push_frame(NIL, code_info as *const CodeInfo, total_slots, 0)
        .expect("EgclStack frame");

    // Building the IR interns its function name. Drain that pinned metadata
    // region first so the probe objects below land in a fresh movable nursery.
    egcl_rt::gc::collect_t0_minor().expect("isolate pinned compiler metadata");
    let roots: Vec<_> = (0..ROOTS)
        .map(|_| {
            let body =
                egcl_rt::gc::alloc_typed(16, type_id::STANDARD_OBJECT).expect("GC test object");
            // `alloc_typed` returns the payload address, while non-cons tagged
            // heap values point at the object's header.
            unsafe { EgclVal::from_heap_ptr(body.sub(8)) }
        })
        .collect();
    unsafe {
        let slots = egcl_rt::EgclStack::frame_slots_mut(frame);
        for (slot, old) in slots.iter_mut().zip(&roots) {
            *slot = *old;
        }
    }
    *NATIVE_ROOT_MOVE_STATE.lock().unwrap() = Some(NativeRootMoveState {
        frame: frame as usize,
        activation_slots: activation_slots as usize,
        root_count: ROOTS,
        clear_symbol,
        move_symbol,
        check_symbols: check_symbols.clone(),
        old_values: roots.iter().map(|value| value.0).collect(),
        expected_by_check: std::collections::HashMap::new(),
        checks_seen: 0,
        moved: false,
        failed: false,
    });

    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let slots = unsafe { frame.add(1) as *mut u64 };
    let result = EgclVal(run(slots));
    let state = NATIVE_ROOT_MOVE_STATE.lock().unwrap().take().unwrap();
    assert!(
        !state.failed,
        "every post-GC native use observes its relocated value"
    );
    assert!(
        state.moved,
        "the minor collector must relocate every native-only root: old={:?}, new={:?}",
        state.old_values,
        check_symbols
            .iter()
            .map(|symbol| state.expected_by_check.get(symbol).copied())
            .collect::<Vec<_>>()
    );
    assert_eq!(state.checks_seen, ROOTS);
    assert_eq!(
        result.0, state.expected_by_check[&check_symbols[0]],
        "the returned GPR/spill home was restored"
    );
    stack.pop_frame();
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn car_cdr_metadata_emit_guarded_field_loads_and_share_the_cons_proof() {
    use std::sync::atomic::Ordering;
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::ir::{AuxData, Opcode, TypeBits};

    let car = egcl_rt::symbols::intern("CAR");
    let cdr = egcl_rt::symbols::intern("CDR");
    let cell = Box::leak(Box::new(ConsCell {
        car: EgclVal::from_fixnum(17),
        cdr: EgclVal::from_fixnum(29),
    }));
    let cons = unsafe { EgclVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };

    for (name, symbol, opcode, expected) in [
        ("car-intrinsic", car, Opcode::Car, cell.car),
        ("cdr-intrinsic", cdr, Opcode::Cdr, cell.cdr),
    ] {
        let bf = bytecode_fn(
            name,
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: symbol,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let f = build_from_bytecode(&bf).expect("build table-driven cons accessor");
        let insts: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .map(|i| f.inst(i))
            .collect();
        assert_eq!(
            insts
                .iter()
                .filter(|d| {
                    d.opcode == Opcode::Guard
                        && matches!(d.aux, AuxData::TypeTag(t) if t.bits == TypeBits::CONS)
                })
                .count(),
            1
        );
        assert_eq!(insts.iter().filter(|d| d.opcode == opcode).count(), 1);
        assert!(!insts.iter().any(|d| d.opcode == Opcode::Call));

        let framed = emit_framed(
            &f,
            cons_access_deopt as *const () as usize as u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
        )
        .expect("emit guarded cons accessor");
        let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
        let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let mut frame = [cons.0, 0];
        CONS_ACCESS_DEOPTED.store(false, Ordering::SeqCst);
        assert_eq!(EgclVal(run(frame.as_mut_ptr())), expected);
        assert!(!CONS_ACCESS_DEOPTED.load(Ordering::SeqCst));

        frame[0] = EgclVal::from_fixnum(7).0;
        CONS_ACCESS_DEOPTED.store(false, Ordering::SeqCst);
        let _ = run(frame.as_mut_ptr());
        assert!(CONS_ACCESS_DEOPTED.load(Ordering::SeqCst));
    }

    // A CAR proof dominates the CDR expansion on the same value. The general
    // guard pass forwards CDR's refined input to CAR's guard result.
    let both = bytecode_fn(
        "car-then-cdr",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed { sym: car, nargs: 1 },
            Instr::Pop,
            Instr::LoadLocal(0),
            Instr::CallNamed { sym: cdr, nargs: 1 },
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    );
    let mut f = build_from_bytecode(&both).expect("build CAR/CDR pair");
    let guard_count = |f: &egcl_compiler::t2::ir::Function, binding: bool| {
        f.block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter())
            .filter(|&&i| {
                let inst = f.inst(i);
                inst.flags.guard && if binding {
                    matches!(inst.aux, AuxData::BuiltinBinding { .. })
                } else {
                    matches!(inst.aux, AuxData::TypeTag(t) if t.bits == TypeBits::CONS)
                }
            })
            .count()
    };
    assert_eq!(guard_count(&f, false), 2);
    assert_eq!(guard_count(&f, true), 2);
    let mut pm = PassManager::new();
    pm.add(Box::new(GuardElim));
    pm.add(Box::new(Dce));
    pm.run(&mut f);
    assert_eq!(
        guard_count(&f, false),
        1,
        "CAR and CDR share one dominating cons guard"
    );
    assert_eq!(guard_count(&f, true), 2, "each binding must still be checked");
    verify(&f).expect("optimized CAR/CDR IR verifies");

    let framed = emit_framed(
        &f,
        cons_access_deopt as *const () as usize as u64,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        None,
    )
    .expect("emit optimized CAR/CDR pair");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [cons.0, 0];
    CONS_ACCESS_DEOPTED.store(false, Ordering::SeqCst);
    assert_eq!(EgclVal(run(frame.as_mut_ptr())), cell.cdr);
    assert!(!CONS_ACCESS_DEOPTED.load(Ordering::SeqCst));
}

#[test]
fn library_function_name_retains_its_call_and_deopt_state() {
    use egcl_compiler::t2::ir::{AuxData, Opcode};

    let first_char = egcl_rt::symbols::intern("UIOP/UTILITY:FIRST-CHAR");
    let bf = bytecode_fn(
        "first-char-caller",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed { sym: first_char, nargs: 1 },
            Instr::Return,
        ],
        vec![], 1, 1, 1,
    );
    let mut f = build_from_bytecode(&bf).expect("build ordinary library call");
    let mut pm = PassManager::new();
    pm.add(Box::new(GuardElim));
    pm.add(Box::new(Dce));
    pm.run(&mut f);
    verify(&f).expect("library call retains valid SSA and deopt state");
    let insts: Vec<_> = f.block_order().iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .map(|i| f.inst(i)).collect();
    let calls: Vec<_> = insts.iter().filter(|d| d.opcode == Opcode::Call).collect();
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert!(matches!(call.aux, AuxData::CallTarget(sym) if sym == first_char));
    assert_eq!(call.args.len(), 1);
    assert!(call.flags.effectful && call.flags.safepoint);
    let state = f.frame_states.get(call.frame_state.expect("call FrameState"));
    assert_eq!(state.scopes[0].bcp, 1);
    assert_eq!(state.scopes[0].stack.len(), 1);
    assert!(!insts.iter().any(|d| matches!(d.opcode,
        Opcode::StringByteLength | Opcode::StringAsciiCharAt)));
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn constant_typep_operands_execute_in_native_code() {
    use egcl_compiler::t2::emit::emit_framed;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CLEARS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn clear_values(_primary: u64, _dst: *mut EgclVal, count: u64) {
        assert_eq!(count, 0);
        CLEARS.fetch_add(1, Ordering::Relaxed);
    }

    let typep = egcl_rt::symbols::intern("TYPEP");
    let symbol = EgclVal::from_symbol_index(egcl_rt::symbols::intern("PREDICATE-CONSTANT"));
    for (kind, values) in [
        ("SYMBOL", vec![(NIL, true), (T, true), (symbol, true),
            (EgclVal::from_fixnum(42), false), (EgclVal::from_char('a'), false)]),
        ("INTEGER", vec![(NIL, false), (T, false), (symbol, false),
            (EgclVal::from_fixnum(42), true), (EgclVal::from_char('a'), false)]),
    ] {
        let type_name = EgclVal::from_symbol_index(egcl_rt::symbols::intern(kind));
        for (value, matches) in values {
            let input = bytecode_fn("constant-typep", vec![
                Instr::Const(0), Instr::Const(1),
                Instr::CallNamed { sym: typep, nargs: 2 }, Instr::Return,
            ], vec![value, type_name], 0, 2, 0);
            let function = build_from_bytecode(&input).expect("build constant TYPEP");
            verify(&function).expect("constant predicate has valid binding guard");
            let native = emit_framed(&function, 0, 0, 0, 0, 0, 0, 0,
                clear_values as *const () as usize as u64, None)
                .expect("emit constant TYPEP");
            let buffer = egcl_rt::jit::JitBuffer::new(&native.code).expect("mmap");
            let run: extern "C" fn(*mut u64) -> u64 =
                unsafe { std::mem::transmute(buffer.as_ptr()) };
            let mut frame = [0; 4];
            assert_eq!(EgclVal(run(frame.as_mut_ptr())), if matches { T } else { NIL },
                "TYPEP {value:?} {kind}");
        }
    }
    assert_eq!(CLEARS.load(Ordering::Relaxed), 10);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn post_inline_guard_elimination_merges_independent_callee_proofs() {
    use std::sync::Arc;
    use egcl_compiler::t2::build::build_from_bytecode_with_inline_options;
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::inlining::InlineOptions;
    use egcl_compiler::t2::ir::{AuxData, Opcode, TypeBits};

    let car = egcl_rt::symbols::intern("CAR");
    let helper = egcl_rt::symbols::intern("GENERAL-GUARDED-LEAF");
    let caller = egcl_rt::symbols::intern("GENERAL-GUARDED-CALLER");
    let helper_body = Arc::new(bytecode_fn(
        "GENERAL-GUARDED-LEAF",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: car,
                nargs: 1,
            },
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    ));
    let caller_body = bytecode_fn(
        "GENERAL-GUARDED-CALLER",
        vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: helper,
                nargs: 1,
            },
            Instr::Pop,
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: helper,
                nargs: 1,
            },
            Instr::Return,
        ],
        vec![],
        1,
        1,
        1,
    );
    let options = InlineOptions::default()
        .with_root_symbol(caller)
        .with_body(helper, helper_body);
    let mut f = build_from_bytecode_with_inline_options(&caller_body, options)
        .expect("inline two guarded callee bodies");
    let cons_guard_count = |f: &egcl_compiler::t2::ir::Function| {
        f.block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| {
                f.inst(i).opcode == Opcode::Guard && matches!(&f.inst(i).aux, AuxData::TypeTag(t) if t.bits == TypeBits::CONS)
            })
            .count()
    };
    assert_eq!(
        cons_guard_count(&f),
        2,
        "each independently built callee contributes its own proof"
    );

    let mut pm = PassManager::new();
    pm.add(Box::new(GuardElim));
    pm.add(Box::new(Dce));
    pm.run(&mut f);
    verify(&f).expect("post-inline guard elimination preserves valid SSA/deopt state");
    assert_eq!(
        cons_guard_count(&f),
        1,
        "the dominating proof eliminates the guard cloned by the second call"
    );

    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, Some(caller))
        .expect("emit caller after general guard elimination");
    let cell = Box::new(ConsCell {
        car: EgclVal::from_fixnum(17),
        cdr: EgclVal::from_fixnum(29),
    });
    let cons = unsafe { EgclVal::from_cons_ptr(&*cell as *const ConsCell as *mut u8) };
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [cons.0, 0, 0];
    assert_eq!(EgclVal(run(frame.as_mut_ptr())), cell.car);
}

/// `(lambda () 42)` — the smallest real function: push a constant, return it.
/// P1 builds it, P2 must accept it, P3 must see the fixnum constant.
#[test]
fn const_return_builds_verifies_and_infers() {
    let bf = bytecode_fn(
        "const42",
        vec![Instr::Const(0), Instr::Return],
        vec![EgclVal::from_fixnum(42)],
        0,
        1,
        0,
    );

    let f = build_from_bytecode(&bf).expect("P1 should build a const-return function");

    // P2: the IR P1 produced must be well-formed.
    verify(&f).expect("P2 must accept P1's IR");

    // P3: inference must run on it and find at least one known fact.
    let facts = infer(&f);
    assert!(
        facts.known_count() >= 1,
        "P3 should infer the constant's type"
    );
}

/// A branch + merge: `(if <x> 1 2)`-shaped bytecode exercises block parameters on
/// the merge, which is where P1/P2/P3 most need to agree.
#[test]
fn branch_merge_builds_and_verifies() {
    // 0: Const c0 (the test value)     -> stack [c0]
    // 1: BrIfFalse 4                   -> pop; false→4, true→2
    // 2: Const c1                      -> stack [c1]
    // 3: Br 5
    // 4: Const c2                      -> stack [c2]
    // 5: Return                        -> pop and return the merged value
    let bf = bytecode_fn(
        "branch",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            EgclVal::from_fixnum(0),
            EgclVal::from_fixnum(1),
            EgclVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );

    let f = build_from_bytecode(&bf).expect("P1 should build a branch/merge function");
    verify(&f).expect("P2 must accept P1's branch/merge IR (block params on the merge)");
    let _ = infer(&f); // must not panic on merged block parameters
}

/// Wave-2 mid-end: the four optimisation passes (P4a–d) must compose on real
/// P1-built branching IR and preserve well-formedness (P2 still accepts it).
#[test]
fn optimisation_passes_compose_and_preserve_wellformedness() {
    let bf = bytecode_fn(
        "opt",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            EgclVal::from_fixnum(0),
            EgclVal::from_fixnum(1),
            EgclVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );
    let mut f = build_from_bytecode(&bf).expect("build");
    verify(&f).expect("pre-opt IR must verify");

    let mut pm = PassManager::new();
    pm.add(Box::new(ConstFold)); // P4f
    pm.add(Box::new(Gvn)); // P4a
    pm.add(Box::new(Licm)); // P4b
    pm.add(Box::new(EscapeAnalysis)); // P4e
    pm.add(Box::new(GuardElim)); // P4d
    pm.add(Box::new(Dce)); // P4c
    pm.run(&mut f);

    // The whole mid-end must leave the IR well-formed (spec §4.10 R4.60 etc.).
    verify(&f).expect("post-opt IR must still verify");
}

/// Speculative lowering on real P1 IR: `(* x 5)` with a fixnum-hot profile at the
/// call site becomes a single guarded `FixnumMul` (no float path), and the result
/// still verifies. This is the profile → single-type-speculation step end to end.
#[test]
fn fixnum_profile_speculates_the_call() {
    use egcl_compiler::t2::ir::Opcode;
    use egcl_compiler::t2::speculate::{SpecType, speculate};

    // (lambda (x) (* x 5)): LoadLocal 0, Const 5, CallNamed *, Return. The
    // CallNamed is at bcp 2.
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
        name: "mul5".to_string(),
        params_form: NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
    };

    let mut f = build_from_bytecode(&bf).expect("build");
    verify(&f).expect("pre-speculation IR verifies");

    // The site at bcp 2 is fixnum-hot → speculate FIXNUM.
    let n = speculate(&mut f, &|bcp| {
        if bcp == 2 {
            Some(SpecType::Fixnum)
        } else {
            None
        }
    });
    assert_eq!(n, 1, "the one arithmetic call site must be speculated");

    // Exactly one FixnumMul, guard-flagged, and no FloatMul anywhere.
    let mut fixnum_muls = 0;
    let mut float_muls = 0;
    for b in f.block_order().to_vec() {
        for &inst in &f.block(b).insts {
            match f.inst(inst).opcode {
                Opcode::FixnumMul => {
                    fixnum_muls += 1;
                    assert!(
                        f.inst(inst).flags.guard,
                        "FixnumMul must be a guarded deopt point"
                    );
                }
                Opcode::FloatMul => float_muls += 1,
                _ => {}
            }
        }
    }
    assert_eq!(fixnum_muls, 1, "one guarded FixnumMul");
    assert_eq!(float_muls, 0, "no float path emitted (mutually exclusive)");

    verify(&f).expect("speculated IR still verifies");
}

/// Branching reaches T2: `(x) -> (if (< x 100) (* x 2) 999)` speculates BOTH the
/// comparison (fused cmp+jcc) and the multiply, emits a multi-block framed
/// function, and executes correctly on both arms.
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn branching_if_speculates_and_runs() {
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::speculate::{SpecType, speculate};
    let lt = egcl_rt::symbols::intern("<");
    let mul = egcl_rt::symbols::intern("*");
    // 0 LoadLocal 0 ; 1 Const 100 ; 2 (< x 100) ; 3 BrIfFalse->8(else)
    // 4 LoadLocal 0 ; 5 Const 2 ; 6 (* x 2) ; 7 Br->9 ; 8 Const 999 ; 9 Return
    let bf = bytecode_fn(
        "clamp",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed { sym: lt, nargs: 2 },
            Instr::BrIfFalse(8),
            Instr::LoadLocal(0),
            Instr::Const(1),
            Instr::CallNamed { sym: mul, nargs: 2 },
            Instr::Br(9),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            EgclVal::from_fixnum(100),
            EgclVal::from_fixnum(2),
            EgclVal::from_fixnum(999),
        ],
        1,
        3,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build branching");
    let n = speculate(&mut f, &|bcp| {
        (bcp == 2 || bcp == 6).then_some(SpecType::Fixnum)
    });
    assert_eq!(n, 2, "both the comparison and the multiply are speculated");
    verify(&f).expect("speculated branching IR verifies");

    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).expect("emit branching function");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

    let mut frame = [EgclVal::from_fixnum(50).0, 0u64, 0u64, 0u64];
    assert_eq!(
        EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
        100,
        "50<100 → 50*2"
    );
    let mut frame = [EgclVal::from_fixnum(99).0, 0u64, 0u64, 0u64];
    assert_eq!(
        EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
        198,
        "99<100 → 99*2"
    );
    let mut frame = [EgclVal::from_fixnum(100).0, 0u64, 0u64, 0u64];
    assert_eq!(
        EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
        999,
        "100≮100 → 999"
    );
    let mut frame = [EgclVal::from_fixnum(200).0, 0u64, 0u64, 0u64];
    assert_eq!(
        EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
        999,
        "200≥100 → 999"
    );
}

/// Function calls reach T2: `(f n) = (* n (g n))` contains a call, so it emits via
/// the callee-saved path (values survive the c2i call) — the multiply is speculated
/// and the call is lowered. Emission must succeed (has_calls: compiled_entry = 0).
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn call_containing_function_emits() {
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::speculate::{SpecType, speculate};
    let g = egcl_rt::symbols::intern("g-callee");
    let mul = egcl_rt::symbols::intern("*");
    // 0 LoadLocal 0 (n) ; 1 LoadLocal 0 (n) ; 2 (g n) ; 3 (* n <g>) ; 4 Return
    let bf = bytecode_fn(
        "callf",
        vec![
            Instr::LoadLocal(0),
            Instr::LoadLocal(0),
            Instr::CallNamed { sym: g, nargs: 1 },
            Instr::CallNamed { sym: mul, nargs: 2 },
            Instr::Return,
        ],
        vec![],
        1,
        3,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build call-containing fn");
    // Speculate only the multiply (bcp 3); the call at bcp 2 stays a generic Call.
    let n = speculate(&mut f, &|bcp| (bcp == 3).then_some(SpecType::Fixnum));
    assert_eq!(n, 1, "the multiply is speculated; the call is not");
    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None)
        .expect("a call-containing function must emit");
    assert!(!framed.code.is_empty());
    // A call function with ≤4 params gets a register entry (for direct self-calls),
    // so its compiled entry sits past the interpreter (frame-loading) entry.
    assert!(
        framed.compiled_entry > 0,
        "call function should expose a register entry"
    );
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn wide_call_uses_a_gc_visible_activation_slice() {
    const CHILD: &str = "EGCL_T2_WIDE_CALL_GC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // This fixture invokes the process-global moving collector. Isolate it
        // from parallel sibling tests so their mutator threads cannot race its
        // safepoint handshake.
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "wide_call_uses_a_gc_visible_activation_slice",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .expect("spawn isolated wide-call GC fixture");
        assert!(
            output.status.success(),
            "isolated wide-call GC fixture failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    use egcl_compiler::t2::emit::emit_framed_with_activation_slots;
    use egcl_rt::CodeInfo;
    use egcl_rt::stack::StackMapEntry;

    extern "C" fn call_slice(_sym: u64, n: u64, args: *const EgclVal, profile: u64) -> u64 {
        assert_eq!(n, 4);
        assert_eq!(profile, 0);
        egcl_rt::gc::collect_t0_minor().expect("move wide-call arguments");
        unsafe { (*args.add(3)).0 }
    }

    let callee = egcl_rt::symbols::intern("wide-callee");
    let bf = bytecode_fn(
        "wide-caller",
        vec![
            Instr::LoadLocal(0),
            Instr::LoadLocal(1),
            Instr::LoadLocal(2),
            Instr::LoadLocal(3),
            Instr::CallNamed {
                sym: callee,
                nargs: 4,
            },
            Instr::Return,
        ],
        vec![],
        4,
        4,
        4,
    );
    let f = build_from_bytecode(&bf).expect("build wide call");
    let framed = emit_framed_with_activation_slots(
        &f,
        0,
        0,
        0,
        call_slice as *const () as usize as u64,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        bf.num_slots(),
        None,
    )
    .expect("emit wide call");
    assert_eq!(
        framed.compiled_entry, 0,
        "slice calls require an owning frame"
    );
    assert!(
        framed.shadow_root_slots >= 4,
        "argument slice must be GC-visible"
    );

    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

    let total_slots = bf.num_slots() + framed.shadow_root_slots;
    let mut bitmap = vec![0u8; (total_slots as usize).div_ceil(8)];
    for i in 0..total_slots as usize {
        bitmap[i / 8] |= 1 << (i % 8);
    }
    let bitmap: &'static [u8] = Box::leak(bitmap.into_boxed_slice());
    let maps: &'static [StackMapEntry] = Box::leak(
        vec![StackMapEntry {
            pc_offset: 0,
            bytes: bitmap.as_ptr() as usize,
            len: bitmap.len(),
        }]
        .into_boxed_slice(),
    );
    let code_info = CodeInfo::new(&[], maps);
    let stack = egcl_rt::current_stack();
    let frame = stack
        .push_frame(NIL, code_info as *const CodeInfo, total_slots, 0)
        .expect("EgclStack frame");

    // Drain the pinned compiler metadata, then put a movable object in the
    // fourth argument. The callback collects before reading that argument, so
    // its return value proves the GC rewrote the wide-call slice in place.
    egcl_rt::gc::collect_t0_minor().expect("isolate pinned compiler metadata");
    let body = egcl_rt::gc::alloc_typed(16, type_id::STANDARD_OBJECT).expect("GC test object");
    let object = unsafe { EgclVal::from_heap_ptr(body.sub(8)) };
    unsafe {
        let slots = egcl_rt::EgclStack::frame_slots_mut(frame);
        for (slot, value) in [
            EgclVal::from_fixnum(11),
            EgclVal::from_fixnum(22),
            EgclVal::from_fixnum(33),
            object,
        ]
        .into_iter()
        .enumerate()
        {
            slots[slot] = value;
        }
    }

    let slots = unsafe { frame.add(1) as *mut u64 };
    let relocated = EgclVal(func(slots));
    assert_ne!(
        relocated, object,
        "minor GC must relocate the wide argument"
    );
    assert_eq!(
        egcl_rt::gc::heap_object_type_id(relocated),
        Some(type_id::STANDARD_OBJECT)
    );
    stack.pop_frame();
}

/// Bitwise ops reach T2: `(x) -> (logand x 255)` speculates LogAnd and emits a
/// single `and` on the tagged value (tagged(a) & tagged(b) = tagged(a & b)).
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn bitwise_logand_speculates_and_runs() {
    use egcl_compiler::t2::emit::emit_framed;
    use egcl_compiler::t2::ir::Opcode;
    use egcl_compiler::t2::speculate::{SpecType, speculate};
    let logand = egcl_rt::symbols::intern("LOGAND");
    // 0 LoadLocal 0 (x) ; 1 Const 255 ; 2 (logand x 255) ; 3 Return
    let bf = bytecode_fn(
        "mask",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed {
                sym: logand,
                nargs: 2,
            },
            Instr::Return,
        ],
        vec![EgclVal::from_fixnum(255)],
        1,
        2,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build");
    let n = speculate(&mut f, &|bcp| (bcp == 2).then_some(SpecType::Fixnum));
    assert_eq!(n, 1, "the logand call is speculated");
    assert!(
        f.block_order().iter().any(|&b| f
            .block(b)
            .insts
            .iter()
            .any(|&i| f.inst(i).opcode == Opcode::LogAnd)),
        "a LogAnd op must be present"
    );
    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).expect("emit bitwise");
    let buf = egcl_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [EgclVal::from_fixnum(0x3E7).0, 0u64, 0u64];
    assert_eq!(
        EgclVal(func(frame.as_mut_ptr())).as_fixnum(),
        0x3E7 & 255,
        "999 & 255 = 231"
    );
}

/// Wave-3 milestone: with P5b (block-CFG lowering) + P6b (multi-block regalloc),
/// the FULL backend now composes on BRANCHING IR — not just straight-line. This
/// is the pipeline stage that the single-block limitation previously blocked.
#[test]
fn backend_lowers_and_allocates_branching() {
    // (if <c> 1 2)-shaped bytecode → a diamond CFG (no critical edges).
    let bf = bytecode_fn(
        "br",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            EgclVal::from_fixnum(0),
            EgclVal::from_fixnum(1),
            EgclVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );
    let f = build_from_bytecode(&bf).expect("build");

    let mut mf = lower(&f); // P5b — must produce a multi-block CFG
    assert!(
        mf.blocks.len() >= 3,
        "branch/merge must lower to a block CFG: {}",
        mf.blocks.len()
    );

    allocate(&mut mf).expect("regalloc2"); // P6b — branching CFG
    assert!(
        !mf.allocation.is_empty(),
        "P6b must allocate the branching function"
    );
}

/// Wave-2 backend: P5 lower → P6 regalloc on real P1-built straight-line IR.
/// (Branching IR needs the deferred MachFunc block-CFG before P6's single-block
/// model handles it — see the wave-2 contract-gap notes.)
#[test]
fn backend_lowers_and_allocates_straight_line() {
    let bf = bytecode_fn(
        "be",
        vec![Instr::Const(0), Instr::Return],
        vec![EgclVal::from_fixnum(7)],
        0,
        1,
        0,
    );
    let f = build_from_bytecode(&bf).expect("build");

    let mut mf = lower(&f); // P5
    assert!(!mf.insts.is_empty(), "P5 must produce machine instructions");

    allocate(&mut mf).expect("regalloc2"); // P6
    assert!(
        !mf.allocation.is_empty(),
        "P6 must assign a location to each vreg"
    );
}


#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn backedge_to_bytecode_zero_preserves_parameters_and_osr_entry() {
    use egcl_compiler::t2::build::build_from_bytecode_for_transfers;
    use egcl_compiler::t2::emit::emit_framed;
    let bf = bytecode_fn(
        "entry-loop",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed { sym: egcl_rt::symbols::intern(">"), nargs: 2 },
            Instr::BrIfFalse(9),
            Instr::LoadLocal(0),
            Instr::Const(1),
            Instr::CallNamed { sym: egcl_rt::symbols::intern("-"), nargs: 2 },
            Instr::StoreLocal(0),
            Instr::Br(0),
            Instr::LoadLocal(0),
            Instr::Return,
        ],
        vec![EgclVal::from_fixnum(0), EgclVal::from_fixnum(1)],
        1, 2, 1,
    );
    let transfer = build_from_bytecode_for_transfers(&bf).unwrap();
    verify(&transfer).unwrap();
    let mut function = build_from_bytecode(&bf).unwrap();
    use egcl_compiler::t2::speculate::{speculate, SpecType};
    assert_eq!(speculate(&mut function, &|bcp| {
        (bcp == 2 || bcp == 6).then_some(SpecType::Fixnum)
    }), 2);
    verify(&function).unwrap();
    assert_eq!(function.osr_entries.len(), 1);
    assert_ne!(function.osr_entries[0].block, function.entry());
    let framed = emit_framed(&function, 0, 0, 0, 0, 0, 0, 0, 0, None).unwrap();
    assert_eq!(framed.osr_entries.len(), 1, "OSR entry must be emitted");
    assert_eq!(framed.osr_entries[0].0, 0);
    let code = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
    let entries = std::iter::once(0).chain(framed.osr_entries.iter().map(|&(_, offset)| offset));
    for offset in entries {
        let run: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(code.as_ptr().add(offset)) };
        for iterations in [0, 1, 254, 255, 256, 511, 1024, 10_000, 1] {
            let mut frame = vec![NIL.0; 3 + framed.shadow_root_slots as usize];
            frame[0] = EgclVal::from_fixnum(iterations).0;
            assert_eq!(run(frame.as_mut_ptr()), EgclVal::from_fixnum(0).0,
                "entry {offset}, iterations {iterations}");
        }
    }
}
