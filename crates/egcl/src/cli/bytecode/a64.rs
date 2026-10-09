// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T1 baseline code generation for AArch64.
//!
//! This is the sibling of `emit_native`, not a rewrite of it. The two share
//! everything that is not instruction selection: the entry ABI
//! (`extern "C" fn(*mut u64, *const u8) -> u64`, the frame-slots pointer and the
//! `EgclStack`), the `NativeEmission` output, `install_stack_map`,
//! `install_t1_code`, and the whole c2i helper surface — those helpers are Rust
//! functions and were already portable.
//!
//! Register assignment mirrors the x86 baseline one for one. AAPCS64 makes
//! x19–x28 callee-saved, so the three activation registers survive a c2i call
//! for exactly the reason r12/r14/r15 do on SysV:
//!
//! | role                                   | x86-64 | AArch64 |
//! |----------------------------------------|--------|---------|
//! | frame slots (local `i` at `+ 8*i`)     | r14    | x19     |
//! | operand-stack pointer, grows up        | r15    | x20     |
//! | `EgclStack`                           | r12    | x21     |
//! | accumulator / return value             | rax    | x0      |
//!
//! WHAT THIS EMITTER DOES NOT DO (each its own bead, all blocked on this one):
//! the speculative fixnum templates and their deopt state transfer
//! (bliss-wpdu6), OSR entry stubs (bliss-cerlf), direct native-to-native calls
//! (bliss-vpul9), and the opcodes beyond the subset below (bliss-gekdt). Every
//! call therefore goes through c2i to the interpreter, which is what makes this
//! slice correct by construction: arithmetic and calls produce exactly what
//! interpretation produces, while dispatch and the operand stack run natively.
//! Nothing here can deoptimize, so `has_deopt` is false and no guard exists that
//! could disagree with T0.

use super::{
    BytecodeFunction, DIRECT_CALL_GEN, NativeEmission, NativeEnvNames, c2i_alloc_cons, c2i_call_builtin,
    c2i_call_slice, c2i_clear_mv, c2i_define_env, c2i_eval_host, c2i_load_env, c2i_load_function,
    c2i_load_global, c2i_make_closure, c2i_osr_backedge, c2i_pop_env_child, c2i_push_env_child,
    c2i_set_native_sigsegv_recovery, c2i_store_env, c2i_store_global, c2i_t1_backedge,
    c2i_take_values, c2i_transfer_pending, c2i_typep_class, c2i_values_to_list,
    call_site_profile_token, registry_get, resolve_sym, t2_backedge_threshold,
};
use egcl_rt::asm::a64;
use egcl_rt::asm::{Asm, Cc};
use egcl_rt::bytecode::Instr;
use egcl_rt::value::EgclVal;

/// Frame slots; local `i` lives at `[SLOTS + 8*i]`.
const SLOTS: a64::Reg = 19;
/// Operand-stack pointer, growing up from `SLOTS + 8*n_locals`.
const OPSP: a64::Reg = 20;
/// The `EgclStack` this activation belongs to.
const STACK: a64::Reg = 21;
/// The accumulator, and the ABI's return register.
const ACC: a64::Reg = 0;
/// IP0/IP1: the architecture reserves these for exactly this use, so no call
/// they straddle can expect them preserved.
const SCRATCH: a64::Reg = 16;
const SCRATCH2: a64::Reg = 17;

/// Bytes the prologue claims. This is the shared JIT save area, not the three
/// registers T1 actually needs: one SIGSEGV recovery epilogue has to unwind
/// whichever tier faulted, so T1 and T2 keep a single layout. See
/// `a64::JIT_SAVE_BYTES`.
const FRAME_BYTES: i32 = a64::JIT_SAVE_BYTES;

/// Compile `bf` to native AArch64 T1 code, or `None` if it uses an opcode this
/// baseline does not handle.
pub(super) fn emit_native_a64(
    bf: &BytecodeFunction,
    _allow_speculation: bool,
    sym: u32,
    backedge_counter: u64,
    _allow_traps: bool,
) -> Option<NativeEmission> {
    let env_names = NativeEnvNames::new(bf);
    macro_rules! decline {
        ($($reason:tt)*) => {{
            egcl_rt::blog!("compile", egcl_rt::log::TRACE,
                "[T1/a64] {}: declined: {}", bf.name, format_args!($($reason)*));
            return None;
        }};
    }

    /// Take an encoder's result, declining with a reason when the operand does
    /// not fit its field. Never use a bare `?` on an encoder: a silent decline
    /// reads as "this function never got hot", which is indistinguishable from a
    /// bug -- exactly the confusion the back-edge threshold caused above.
    macro_rules! encode {
        ($what:expr, $e:expr) => {
            match $e {
                Some(word) => word,
                None => decline!("cannot encode {}", $what),
            }
        };
    }

    // `sym == u32::MAX` marks the OSR compile path, whose back-edge poll is the
    // signal-only helper and whose entry points are the loop headers rather than
    // the function's start.
    let is_osr = sym == u32::MAX;
    if bf.arity > bf.num_slots() {
        decline!(
            "required arity {} exceeds activation slots {}",
            bf.arity,
            bf.num_slots()
        );
    }
    let n_locals = bf.n_locals as i32;
    // Set when a back-edge poll can hand this activation to T2, which means it can
    // leave the loop mid-flight. `has_deopt` must report that: a caller's direct
    // call cannot handle a callee that resumes elsewhere.
    let mut can_osr_to_t2 = false;

    // Where each block's RETURN-FROM resumes, and each tagbody's entry depth.
    let mut block_targets: std::collections::HashMap<u32, (u32, u16)> =
        std::collections::HashMap::new();
    let mut tag_sp: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
    for instr in bf.code.iter() {
        match instr {
            Instr::PushBlock {
                block_id,
                resume_bcp,
                sp_restore,
                ..
            } => {
                block_targets.insert(*block_id, (*resume_bcp, *sp_restore));
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                tag_sp.insert(*tagbody_id, *sp_restore);
            }
            _ => {}
        }
    }

    // OSR-eligible loop headers: the target of a backward `Go` whose tagbody sits
    // at an empty operand stack (sp_restore == 0). Entering there needs no value
    // transfer — the live locals are already in the shared frame slots.
    let mut osr_headers: Vec<u32> = Vec::new();
    for (i, instr) in bf.code.iter().enumerate() {
        if let Instr::Go {
            tagbody_id,
            target_bcp,
        } = instr
        {
            if (*target_bcp as usize) < i
                && tag_sp.get(tagbody_id) == Some(&0)
                && !osr_headers.contains(target_bcp)
            {
                osr_headers.push(*target_bcp);
            }
        }
    }

    let c2i_addr =
        c2i_call_slice as extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 as usize as u64;
    let builtin_addr =
        c2i_call_builtin as extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 as usize as u64;
    let transfer_addr = c2i_transfer_pending as extern "C" fn() -> u64 as usize as u64;
    let recovery_toggle_addr =
        c2i_set_native_sigsegv_recovery as extern "C" fn(u64) as usize as u64;
    let clear_mv_addr = c2i_clear_mv as extern "C" fn() as usize as u64;
    let load_global_addr = c2i_load_global as extern "C" fn(u64) -> u64 as usize as u64;
    let load_function_addr = c2i_load_function as extern "C" fn(u64) -> u64 as usize as u64;
    let store_global_addr = c2i_store_global as extern "C" fn(u64, u64) as usize as u64;
    let load_env_addr =
        c2i_load_env as extern "C" fn(*const NativeEnvNames, u64) -> u64 as usize as u64;
    let store_env_addr =
        c2i_store_env as extern "C" fn(*const NativeEnvNames, u64, u64) as usize as u64;
    let define_env_addr =
        c2i_define_env as extern "C" fn(*const NativeEnvNames, u64, u64) as usize as u64;
    let push_env_addr = c2i_push_env_child as extern "C" fn() as usize as u64;
    let pop_env_addr = c2i_pop_env_child as extern "C" fn() as usize as u64;
    let eval_host_addr = c2i_eval_host as extern "C" fn(u64) -> u64 as usize as u64;
    let make_closure_addr = c2i_make_closure as extern "C" fn(u64) -> u64 as usize as u64;
    let alloc_cons_addr = c2i_alloc_cons as extern "C" fn(u64, u64) -> u64 as usize as u64;
    let take_values_addr =
        c2i_take_values as extern "C" fn(u64, *mut EgclVal, u64) as usize as u64;
    let values_to_list_addr = c2i_values_to_list as extern "C" fn(u64) -> u64 as usize as u64;
    let typep_class_addr = c2i_typep_class as extern "C" fn(u64, u64) -> u64 as usize as u64;
    let values_sym = resolve_sym("VALUES")?.as_symbol_index();
    let osr_backedge_addr = c2i_osr_backedge as extern "C" fn() -> u64 as usize as u64;
    let t2_backedge_addr = c2i_t1_backedge
        as extern "C" fn(u64, u64, *mut u64, *const BytecodeFunction) -> u64
        as usize as u64;

    let mut c = Asm::new();
    let bcp_labels: Vec<_> = bf.code.iter().map(|_| c.label()).collect();

    // ── Prologue ───────────────────────────────────────────────
    // Shared by the normal entry and every OSR entry stub: both are called as
    // `fn(*mut u64, *const u8) -> u64` and must set up the activation registers
    // identically, so the one Return epilogue balances either. x29 records the
    // frame base, so the epilogue restores SP from it rather than trusting the
    // body to have left SP balanced.
    let emit_prologue = |c: &mut Asm| -> Option<()> {
        c.word(a64::stp_pre(a64::FP, a64::LR, a64::SP, -FRAME_BYTES).expect("fixed frame size"));
        c.word(a64::mov_from_sp(a64::FP));
        for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
            c.word(a64::stp(first, second, a64::SP, 16 + index as i32 * 16).expect("fixed offset"));
        }
        c.word(a64::mov(SLOTS, 0));
        c.word(a64::mov(STACK, 1));
        emit_add_disp(c, OPSP, SLOTS, 8 * n_locals)
    };
    emit_prologue(&mut c)?;

    for (bcp_idx, instr) in bf.code.iter().enumerate() {
        c.bind(bcp_labels[bcp_idx]);
        let bcp = bcp_idx as u32;
        match instr {
            Instr::Const(k) => {
                let val = bf.constants[*k as usize];
                if egcl_rt::gc::is_heap_ref(val) {
                    // A movable heap constant must never be baked in as an
                    // immediate: the moving minor GC rewrites the registry-rooted
                    // constants Vec in place and cannot patch machine code, so
                    // the immediate would go stale and native code would push a
                    // freed-nursery pointer (bliss-d0b). Load through the slot,
                    // whose address is stable for the life of the registry Rc
                    // that this code is keyed to.
                    let slot = &bf.constants[*k as usize] as *const EgclVal;
                    emit_mov_imm(&mut c, ACC, slot as u64);
                    c.word(a64::ldr_imm(ACC, ACC, 0).expect("zero offset"));
                } else {
                    emit_mov_imm(&mut c, ACC, val.0);
                }
                emit_push(&mut c);
            }
            Instr::LoadLocal(i) => {
                c.word(encode!(
                    format_args!("LoadLocal {i}: frame slot beyond the scaled load"),
                    a64::ldr_imm(ACC, SLOTS, 8 * u64::from(*i))
                ));
                emit_push(&mut c);
            }
            Instr::StoreLocal(i) => {
                emit_pop(&mut c, ACC);
                c.word(encode!(
                    format_args!("StoreLocal {i}: frame slot beyond the scaled store"),
                    a64::str_imm(ACC, SLOTS, 8 * u64::from(*i))
                ));
            }
            Instr::Pop => {
                c.word(encode!("Pop", a64::sub_imm(OPSP, OPSP, 8)));
            }
            Instr::Dup => {
                c.word(encode!("Dup", a64::ldur(ACC, OPSP, -8)));
                emit_push(&mut c);
            }
            Instr::CallNamed { sym: callee, nargs } => {
                // Argument registers per AAPCS64, matching c2i_call_slice's
                // signature (sym, nargs, args, profile_site). A direct builtin
                // takes (slot << 32 | sym) and the invalidation generation in
                // place of a profile token, since builtins do not tier.
                let direct_builtin = super::super::direct_builtin_slot(*callee, *nargs as usize);
                let arg0 = match direct_builtin {
                    Some(slot) => ((slot as u64) << 32) | u64::from(*callee),
                    None => u64::from(*callee),
                };
                emit_mov_imm(&mut c, 0, arg0);
                emit_mov_imm(&mut c, 1, u64::from(*nargs));
                emit_sub_disp(&mut c, 2, OPSP, 8 * i32::from(*nargs))?;
                let profile_site = if direct_builtin.is_some() {
                    DIRECT_CALL_GEN.load(std::sync::atomic::Ordering::Relaxed)
                } else if registry_get(*callee).is_some() {
                    call_site_profile_token(bf as *const BytecodeFunction as usize, bcp)
                } else {
                    0
                };
                emit_mov_imm(&mut c, 3, profile_site);
                let target = if direct_builtin.is_some() {
                    builtin_addr
                } else {
                    c2i_addr
                };
                emit_c2i_call(&mut c, target, transfer_addr, recovery_toggle_addr)?;
                // Drop the arguments and push the result.
                c.word(encode!(
                    format_args!("dropping {nargs} call arguments"),
                    a64::sub_imm(OPSP, OPSP, 8 * u64::from(*nargs))
                ));
                emit_push(&mut c);
            }
            Instr::ClearMv => {
                // Reset the thread's multiple-values state; SETQ and friends are
                // not value-preserving. The accumulator is dead between
                // statements, and the activation registers are callee-saved
                // across the call, so this needs no spilling.
                emit_c2i_call(&mut c, clear_mv_addr, transfer_addr, recovery_toggle_addr)?;
            }
            Instr::SetValues(n) => {
                // Route through the ordinary VALUES function so native code and
                // the interpreter share one multiple-values contract. The values
                // sit contiguously below the operand-stack top.
                emit_mov_imm(&mut c, 0, u64::from(values_sym));
                emit_mov_imm(&mut c, 1, u64::from(*n));
                emit_sub_disp(&mut c, 2, OPSP, 8 * i32::from(*n))?;
                emit_mov_imm(&mut c, 3, 0);
                emit_c2i_call(&mut c, c2i_addr, transfer_addr, recovery_toggle_addr)?;
                c.word(encode!(
                    format_args!("dropping {n} values"),
                    a64::sub_imm(OPSP, OPSP, 8 * u64::from(*n))
                ));
                emit_push(&mut c);
            }
            // The code owner retains this version's name snapshots even
            // after redefinition.
            Instr::LoadEnvVar(name_idx) => {
                emit_mov_imm(&mut c, 0, std::ptr::from_ref(&*env_names) as u64);
                emit_mov_imm(&mut c, 1, u64::from(u32::from(*name_idx)));
                emit_c2i_call(&mut c, load_env_addr, transfer_addr, recovery_toggle_addr)?;
                emit_push(&mut c);
            }
            Instr::StoreEnvVar(name_idx) | Instr::DefineEnvVar(name_idx) => {
                emit_pop(&mut c, 2);
                emit_mov_imm(&mut c, 0, std::ptr::from_ref(&*env_names) as u64);
                emit_mov_imm(&mut c, 1, u64::from(u32::from(*name_idx)));
                let helper = if matches!(instr, Instr::StoreEnvVar(_)) {
                    store_env_addr
                } else {
                    define_env_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr, recovery_toggle_addr)?;
            }
            Instr::PushEnvChild | Instr::PopEnvChild => {
                let helper = if matches!(instr, Instr::PushEnvChild) {
                    push_env_addr
                } else {
                    pop_env_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr, recovery_toggle_addr)?;
            }
            Instr::AllocCons => {
                // The cdr is on top, so it pops first.
                emit_pop(&mut c, 1);
                emit_pop(&mut c, 0);
                emit_c2i_call(&mut c, alloc_cons_addr, transfer_addr, recovery_toggle_addr)?;
                emit_push(&mut c);
            }
            Instr::EvalHost(index) | Instr::MakeClosureEnv(index) => {
                // Like Const: the form is a movable heap cons, so it is read
                // through the GC-rewritten constants slot, never baked in.
                let Some(slot) = bf.constants.get(*index as usize) else {
                    decline!("constant index {index} is out of range");
                };
                emit_mov_imm(&mut c, 0, slot as *const EgclVal as u64);
                c.word(a64::ldr_imm(0, 0, 0).expect("zero offset"));
                let helper = if matches!(instr, Instr::EvalHost(_)) {
                    eval_host_addr
                } else {
                    make_closure_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr, recovery_toggle_addr)?;
                emit_push(&mut c);
            }
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                emit_pop(&mut c, 0);
                emit_add_disp(&mut c, 1, SLOTS, 8 * i32::from(*slot_base))?;
                emit_mov_imm(&mut c, 2, u64::from(*nvars));
                emit_c2i_call(
                    &mut c,
                    take_values_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
            }
            // Read a global or special variable's value cell and push it.
            Instr::LoadGlobal(sym) => {
                emit_mov_imm(&mut c, 0, u64::from(*sym));
                emit_c2i_call(
                    &mut c,
                    load_global_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
                emit_push(&mut c);
            }
            // `#'f` — the same shape, reading the symbol's FUNCTION cell.
            Instr::LoadFunction(sym) => {
                emit_mov_imm(&mut c, 0, u64::from(*sym));
                emit_c2i_call(
                    &mut c,
                    load_function_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
                emit_push(&mut c);
            }
            // Consumes the operand and pushes nothing; SETQ reloads for its value.
            Instr::StoreGlobal(sym) => {
                emit_pop(&mut c, 1);
                emit_mov_imm(&mut c, 0, u64::from(*sym));
                emit_c2i_call(
                    &mut c,
                    store_global_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
            }
            Instr::ValuesToList => {
                // Reads env.mv and allocates; the helper roots the primary before
                // allocating (bliss-rwiv).
                emit_pop(&mut c, 0);
                emit_c2i_call(
                    &mut c,
                    values_to_list_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
                emit_push(&mut c);
            }
            Instr::TypeP(class) => {
                emit_pop(&mut c, 0);
                emit_mov_imm(&mut c, 1, u64::from(*class as u32));
                emit_c2i_call(
                    &mut c,
                    typep_class_addr,
                    transfer_addr,
                    recovery_toggle_addr,
                )?;
                emit_push(&mut c);
            }
            Instr::Br(target) => {
                c.jmp(match bcp_labels.get(*target as usize) {
                    Some(l) => *l,
                    None => decline!("Br target {target} is out of range"),
                });
            }
            Instr::BrIfFalse(target) => {
                emit_pop(&mut c, ACC);
                emit_cmp_imm(&mut c, ACC, egcl_rt::value::NIL_BITS);
                c.jcc(
                    Cc::E,
                    match bcp_labels.get(*target as usize) {
                        Some(l) => *l,
                        None => decline!("BrIfFalse target {target} is out of range"),
                    },
                );
            }
            Instr::Return => {
                emit_pop(&mut c, ACC);
                emit_epilogue(&mut c);
            }
            // A T1-eligible function is lexically closed over its own blocks and
            // tags: lower_block/lower_go bail on captured names and
            // native_would_lose_captured_control declines any function whose
            // closures capture them. So every compiled transfer is local, the
            // interpreter's handler stack is dead here, and these publish
            // nothing native execution needs.
            Instr::PushBlock { .. } | Instr::PushTag { .. } => {}
            Instr::NamedTag { .. } | Instr::PopHandler => {}
            Instr::ReturnFrom { block_id } => {
                let (resume_bcp, sp) = match block_targets.get(block_id) {
                    Some(&t) => t,
                    None => decline!("RETURN-FROM references non-local block {block_id}"),
                };
                c.word(encode!("RETURN-FROM value", a64::ldur(ACC, OPSP, -8)));
                emit_add_disp(&mut c, OPSP, SLOTS, 8 * (n_locals + i32::from(sp)))?;
                emit_push(&mut c);
                c.jmp(match bcp_labels.get(resume_bcp as usize) {
                    Some(l) => *l,
                    None => decline!("RETURN-FROM resume {resume_bcp} is out of range"),
                });
            }
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                let Some(&sp) = tag_sp.get(tagbody_id) else {
                    decline!("GO references non-local tagbody {tagbody_id}");
                };
                emit_add_disp(&mut c, OPSP, SLOTS, 8 * (n_locals + i32::from(sp)))?;
                if (*target_bcp as usize) < bcp_idx && sp == 0 && backedge_counter != 0 {
                    // Sampled loop back-edge poll. This drives T1→T2 promotion
                    // and polls process signals, so a hot native loop stays
                    // terminable and still reaches GC stop-the-world
                    // (bliss-7rdu). Without it a promoted loop would spin past
                    // both. A non-zero return means leave through the epilogue.
                    let keep = c.label();
                    emit_mov_imm(&mut c, SCRATCH, backedge_counter);
                    // The counter is an AtomicU32, so these must be 32-bit
                    // accesses: a 64-bit one would touch the next four bytes.
                    c.word(a64::ldr_w_imm(SCRATCH2, SCRATCH, 0).expect("zero offset"));
                    c.word(a64::add_imm_w(SCRATCH2, SCRATCH2, 1).expect("increment of one"));
                    c.word(a64::str_w_imm(SCRATCH2, SCRATCH, 0).expect("zero offset"));
                    // The threshold is an arbitrary u32 (10,000 by default), so
                    // it gets a register rather than an immediate field: 10,000
                    // is neither under 4096 nor a multiple of it, so the
                    // add-immediate form cannot hold it. The accumulator is dead
                    // here — Go yields no value — and the helper's arguments are
                    // set below, past this branch.
                    emit_mov_imm(&mut c, ACC, u64::from(t2_backedge_threshold()));
                    c.word(a64::cmp_w(SCRATCH2, ACC));
                    c.jcc(Cc::L, keep);
                    // Reset the sample.
                    c.word(a64::str_w_imm(a64::XZR, SCRATCH, 0).expect("zero offset"));
                    let helper = if is_osr {
                        // Signal-only: an OSR loop has no T1 tier to promote
                        // from, but must still be terminable and still reach GC
                        // stop-the-world.
                        osr_backedge_addr
                    } else {
                        can_osr_to_t2 = true;
                        emit_mov_imm(&mut c, 0, u64::from(sym));
                        emit_mov_imm(&mut c, 1, u64::from(*target_bcp));
                        c.word(a64::mov(2, SLOTS));
                        emit_mov_imm(&mut c, 3, std::ptr::from_ref(bf) as u64);
                        t2_backedge_addr
                    };
                    emit_c2i_call(&mut c, helper, transfer_addr, recovery_toggle_addr)?;
                    emit_cmp_imm(&mut c, ACC, 0);
                    c.jcc(Cc::E, keep);
                    // Leaving the loop: T2 finished, or a signal is pending. The
                    // shared epilogue returns the first operand slot; the Rust
                    // caller re-raises any stashed error before using it.
                    c.word(encode!(
                        "first operand slot for the loop-exit epilogue",
                        a64::ldr_imm(ACC, SLOTS, 8 * n_locals as u64)
                    ));
                    emit_epilogue(&mut c);
                    c.bind(keep);
                }
                c.jmp(match bcp_labels.get(*target_bcp as usize) {
                    Some(l) => *l,
                    None => decline!("GO target {target_bcp} is out of range"),
                });
            }
            other => decline!("unsupported opcode {other:?}"),
        }
    }

    // A bytecode function always ends in Return, but a trailing fallthrough must
    // not run off the end of the buffer into whatever follows.
    c.word(a64::mov(ACC, a64::XZR));
    emit_epilogue(&mut c);

    // One alternate entry per eligible loop header: the shared prologue, then a
    // branch straight into the body. `finish` patches displacements in place, so
    // an offset captured now is still correct in the returned buffer.
    let mut osr_entries: Vec<(u32, usize)> = Vec::new();
    for header in osr_headers {
        let Some(&target) = bcp_labels.get(header as usize) else {
            decline!("OSR header {header} is out of range");
        };
        let stub_off = c.here();
        emit_prologue(&mut c)?;
        c.jmp(target);
        osr_entries.push((header, stub_off));
    }

    // Bytecode→native position map for the tier viewer: each bcp's label was
    // bound at that instruction's first native byte. u32::MAX marks a bcp that
    // emitted no code.
    let bcp_offsets: Vec<u32> = bcp_labels
        .iter()
        .map(|&l| c.label_offset(l).map_or(u32::MAX, |o| o as u32))
        .collect();

    // `finish` resolves every branch, and gives up only if one is out of reach --
    // for A64 that means a conditional branch spanning more than 1 MiB.
    let Some(code) = c.finish() else {
        decline!("a branch displacement is out of range (function too large)");
    };
    Some(NativeEmission {
        env_names,
        code,
        osr_entries,
        bcp_offsets,
        has_deopt: can_osr_to_t2,
        direct_calls: Vec::new(),
    })
}

/// `str ACC, [OPSP], #8` — store and post-increment, i.e. push the accumulator.
fn emit_push(c: &mut Asm) {
    c.word(a64::str_post(ACC, OPSP, 8).expect("8 is in the unscaled range"));
}

/// Pop into `d`: retreat the stack top, then load from it.
fn emit_pop(c: &mut Asm, d: a64::Reg) {
    c.word(a64::ldr_pre(d, OPSP, -8).expect("-8 is in the unscaled range"));
}

fn emit_epilogue(c: &mut Asm) {
    c.word(a64::mov_to_sp(a64::FP));
    for (index, (first, second)) in a64::JIT_SAVED_PAIRS.into_iter().enumerate() {
        c.word(a64::ldp(first, second, a64::SP, 16 + index as i32 * 16).expect("fixed offset"));
    }
    c.word(a64::ldp_post(a64::FP, a64::LR, a64::SP, FRAME_BYTES).expect("fixed frame size"));
    c.word(a64::ret());
}

/// Materialise a 64-bit constant into `d`.
fn emit_mov_imm(c: &mut Asm, d: a64::Reg, value: u64) {
    let mut words = Vec::new();
    a64::mov_imm64(d, value, &mut words);
    c.words(&words);
}

/// `d = src + disp`, falling back to a materialised offset when `disp` is beyond
/// the add-immediate field.
fn emit_add_disp(c: &mut Asm, d: a64::Reg, src: a64::Reg, disp: i32) -> Option<()> {
    let magnitude = u64::from(disp.unsigned_abs());
    let short = if disp >= 0 {
        a64::add_imm(d, src, magnitude)
    } else {
        a64::sub_imm(d, src, magnitude)
    };
    match short {
        Some(word) => c.word(word),
        None => {
            emit_mov_imm(c, SCRATCH2, magnitude);
            c.word(if disp >= 0 {
                a64::add(d, src, SCRATCH2)
            } else {
                a64::sub(d, src, SCRATCH2)
            });
        }
    }
    Some(())
}

fn emit_sub_disp(c: &mut Asm, d: a64::Reg, src: a64::Reg, disp: i32) -> Option<()> {
    emit_add_disp(c, d, src, -disp)
}

/// `cmp n, #value`, materialising the constant when it does not fit the
/// immediate field — tagged values generally do not.
fn emit_cmp_imm(c: &mut Asm, n: a64::Reg, value: u64) {
    match a64::cmp_imm(n, value) {
        Some(word) => c.word(word),
        None => {
            emit_mov_imm(c, SCRATCH, value);
            c.word(a64::cmp(n, SCRATCH));
        }
    }
}

/// Call a c2i helper whose arguments are already in x0–x3, then honour a pending
/// non-local transfer.
///
/// The x86 sequence also toggles native-frame SIGSEGV recovery around the call.
/// That is deliberately absent here: `native_sigsegv_recovery_ip` returns 0 on
/// every non-x86-64 target, so both sides of the toggle would install the same
/// zero. Recovery is simply not available on AArch64 yet (bliss-7t9a4), and
/// pretending to toggle it would cost two calls per c2i call for nothing.
fn emit_c2i_call(c: &mut Asm, target: u64, transfer_addr: u64, recovery_toggle: u64) -> Option<()> {
    // Disable native-frame SIGSEGV recovery while a Rust helper frame is active.
    // Recovery redirects a fault to an epilogue that unwinds a JIT frame; with a
    // helper frame on top that would restore the wrong registers and return to the
    // wrong place. The arguments are spilled around the toggle because it is an
    // ordinary call that clobbers them.
    let spill = |c: &mut Asm, store: bool| {
        for (index, register) in [0u8, 1, 2, 3].into_iter().enumerate() {
            let offset = 8 * index as i32;
            c.word(if store {
                a64::str_imm(register, a64::SP, offset as u64).expect("fixed offset")
            } else {
                a64::ldr_imm(register, a64::SP, offset as u64).expect("fixed offset")
            });
        }
    };
    c.word(a64::sub_imm(a64::SP, a64::SP, 48).expect("fixed frame"));
    spill(c, true);
    c.word(a64::str_imm(SCRATCH2, a64::SP, 32).expect("fixed offset"));
    emit_mov_imm(c, 0, 0);
    emit_mov_imm(c, SCRATCH, recovery_toggle);
    c.word(a64::blr(SCRATCH));
    spill(c, false);
    c.word(a64::ldr_imm(SCRATCH2, a64::SP, 32).expect("fixed offset"));
    c.word(a64::add_imm(a64::SP, a64::SP, 48).expect("fixed frame"));

    emit_mov_imm(c, SCRATCH, target);
    c.word(a64::blr(SCRATCH));

    // Re-enable recovery, preserving the call's result across the toggle.
    c.word(a64::str_pre(ACC, a64::SP, -16).expect("16 is in the unscaled range"));
    emit_mov_imm(c, 0, 1);
    emit_mov_imm(c, SCRATCH, recovery_toggle);
    c.word(a64::blr(SCRATCH));
    c.word(a64::ldr_post(ACC, a64::SP, 16).expect("16 is in the unscaled range"));

    // Stop T1 at a call that initiated an error, THROW or RETURN-FROM. The c2i
    // helper stashes the condition and returns NIL, so without this check native
    // execution would continue into code that must not run — a store after the
    // error lands, and a later error superseding the real one.
    let resume = c.label();
    // Spill the result, keeping SP 16-aligned for the call below.
    c.word(a64::str_pre(ACC, a64::SP, -16).expect("16 is in the unscaled range"));
    emit_mov_imm(c, SCRATCH, transfer_addr);
    c.word(a64::blr(SCRATCH));
    c.word(a64::cmp_imm(ACC, 0).expect("zero is an add-immediate"));
    // Neither the load nor the stack adjustment disturbs the flags, so the
    // comparison above still decides the branch below.
    c.word(a64::ldr_post(ACC, a64::SP, 16).expect("16 is in the unscaled range"));
    c.jcc(Cc::E, resume);
    emit_epilogue(c);
    c.bind(resume);
    Some(())
}
