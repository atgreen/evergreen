// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T1 baseline code generation for ppc64le.
//!
//! The sibling of `emit_native_x86` and of the AArch64 and System Z emitters, not a
//! rewrite of any of them. Everything that is not instruction selection is shared
//! unchanged: the entry ABI (`extern "C" fn(*mut u64, *const u8) -> u64`, the
//! frame-slots pointer and the `EgclStack`), `NativeEmission`, `install_stack_map`,
//! `install_t1_code`, and the whole c2i helper surface, which is Rust and was
//! already portable.
//!
//! Register roles map onto the other emitters'. ELFv2 makes r14–r31 nonvolatile, so
//! the three activation registers survive a c2i call for the same reason
//! r12/r14/r15 do on SysV x86-64 and x19/x20/x21 on AAPCS64:
//!
//! | role                                  | x86-64 | AArch64 | ppc64le |
//! |---------------------------------------|--------|---------|---------|
//! | frame slots (local `i` at `+ 8*i`)    | r14    | x19     | r14     |
//! | operand-stack pointer, grows up       | r15    | x20     | r15     |
//! | `EgclStack`                          | r12    | x21     | r16     |
//! | accumulator / first argument / result | rax    | x0      | r3      |
//!
//! TWO THINGS POWER FORCES that neither of the others did:
//!
//! An indirect call goes through the count register — there is no
//! call-through-GPR — and `bctrl` clobbers the link register, so every call site
//! sits inside a frame whose link register was saved on entry.
//!
//! The TOC. ELFv2 callees reached at their global entry point compute r2 from r12,
//! so a call must put the target in r12, and r2 is saved and restored around it.
//! Getting this wrong is the failure mode to watch for: leaf arithmetic works and
//! anything that calls out corrupts, which is exactly how the AArch64 frame bug
//! presented before it was found.

use super::{
    BytecodeFunction, DIRECT_CALL_GEN, FixnumOp, NativeEmission, UnaryFixnumOp, c2i_alloc_cons,
    c2i_call_builtin, c2i_call_slice, c2i_clear_mv, c2i_define_env, c2i_deopt_state, c2i_eval_host,
    c2i_load_env, c2i_load_function, c2i_load_global, c2i_make_closure, c2i_osr_backedge,
    c2i_pop_env_child, c2i_push_env_child, c2i_store_env, c2i_store_global, c2i_t1_backedge,
    c2i_take_values, c2i_transfer_pending, c2i_typep_class, c2i_values_to_list,
    call_site_profile_token, inlinable_fixnum_op, inlinable_unary_fixnum_op, registry_get,
    resolve_sym, t2_backedge_threshold,
};
use egcl_rt::asm::Cc;
use egcl_rt::asm_ppc64le::{Asm, Label, frame};
use egcl_rt::bytecode::Instr;
use egcl_rt::value::{EgclVal, NIL, T};

/// Frame slots; local `i` lives at `[SLOTS + 8*i]`.
const SLOTS: u8 = 14;
/// Operand-stack pointer, growing up from `SLOTS + 8*n_locals`.
const OPSP: u8 = 15;
/// The `EgclStack` this activation belongs to.
const STACK: u8 = 16;
/// The accumulator: the ABI's first argument and its return register.
const ACC: u8 = 3;
/// Volatile scratch. r12 is the ABI's own choice for a call target, which is why an
/// indirect call must use it.
const SCRATCH: u8 = 11;
const TARGET: u8 = 12;
/// r0 reads as a literal zero in some instruction forms, so it is only ever used
/// where that cannot matter — moving the link register.
const LINK_TEMP: u8 = 0;
/// The TOC pointer.
const TOC: u8 = 2;

/// Compile `bf` to native ppc64le T1 code, or `None` if it uses an opcode this
/// baseline does not handle.
pub(super) fn emit_native_ppc64le(
    bf: &BytecodeFunction,
    allow_speculation: bool,
    sym: u32,
    backedge_counter: u64,
    _allow_traps: bool,
) -> Option<NativeEmission> {
    macro_rules! decline {
        ($($reason:tt)*) => {{
            egcl_rt::blog!("compile", egcl_rt::log::TRACE,
                "[T1/ppc64le] {}: declined: {}", bf.name, format_args!($($reason)*));
            return None;
        }};
    }
    /// Take an encoder's result, declining with a reason when an operand does not
    /// fit its field. Never a bare `?`: a silent decline reads exactly like "this
    /// function never got hot", which is the confusion that cost a debugging cycle
    /// on AArch64.
    macro_rules! encode {
        ($what:expr, $e:expr) => {
            match $e {
                Some(value) => value,
                None => decline!("cannot encode {}", $what),
            }
        };
    }

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
    // leave the loop mid-flight; `has_deopt` must report that.
    let mut can_osr_to_t2 = false;

    // One cold stub per guarded bytecode position: a failed fixnum guard records
    // (bcp, operand depth) and leaves, and T0 resumes exactly there.
    let mut deopts: std::collections::BTreeMap<u32, Label> = std::collections::BTreeMap::new();
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

    // OSR-eligible loop headers: the target of a backward `Go` whose tagbody sits at
    // an empty operand stack, so an entry needs only the prologue and a branch.
    let mut osr_headers: Vec<u32> = Vec::new();
    for (index, instr) in bf.code.iter().enumerate() {
        if let Instr::Go {
            tagbody_id,
            target_bcp,
        } = instr
        {
            if (*target_bcp as usize) < index
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
    let clear_mv_addr = c2i_clear_mv as extern "C" fn() as usize as u64;
    let load_global_addr = c2i_load_global as extern "C" fn(u64) -> u64 as usize as u64;
    let load_function_addr = c2i_load_function as extern "C" fn(u64) -> u64 as usize as u64;
    let store_global_addr = c2i_store_global as extern "C" fn(u64, u64) as usize as u64;
    let load_env_addr =
        c2i_load_env as extern "C" fn(*const BytecodeFunction, u64) -> u64 as usize as u64;
    let store_env_addr =
        c2i_store_env as extern "C" fn(*const BytecodeFunction, u64, u64) as usize as u64;
    let define_env_addr =
        c2i_define_env as extern "C" fn(*const BytecodeFunction, u64, u64) as usize as u64;
    let push_env_addr = c2i_push_env_child as extern "C" fn() as usize as u64;
    let pop_env_addr = c2i_pop_env_child as extern "C" fn() as usize as u64;
    let eval_host_addr = c2i_eval_host as extern "C" fn(u64) -> u64 as usize as u64;
    let make_closure_addr = c2i_make_closure as extern "C" fn(u64) -> u64 as usize as u64;
    let alloc_cons_addr = c2i_alloc_cons as extern "C" fn(u64, u64) -> u64 as usize as u64;
    let take_values_addr = c2i_take_values as extern "C" fn(u64, *mut EgclVal, u64) as usize as u64;
    let values_to_list_addr = c2i_values_to_list as extern "C" fn(u64) -> u64 as usize as u64;
    let typep_class_addr = c2i_typep_class as extern "C" fn(u64, u64) -> u64 as usize as u64;
    let osr_backedge_addr = c2i_osr_backedge as extern "C" fn() -> u64 as usize as u64;
    let t2_backedge_addr = c2i_t1_backedge
        as extern "C" fn(u64, u64, *mut u64, *const BytecodeFunction) -> u64
        as usize as u64;
    let values_sym = resolve_sym("VALUES")?.as_symbol_index();

    let mut c = Asm::new();
    let bcp_labels: Vec<_> = bf.code.iter().map(|_| c.label()).collect();

    // ── Prologue ───────────────────────────────────────────────
    // Shared by the normal entry and every OSR entry stub, so the one Return
    // epilogue balances either. r3 = frame slots, r4 = EgclStack.
    //
    // The link register is saved in the CALLER's frame, as the ABI prescribes, and
    // therefore before this frame is claimed.
    let emit_prologue = |c: &mut Asm| -> Option<()> {
        c.move_from_link(LINK_TEMP);
        c.store(LINK_TEMP, 1, frame::LINK_SLOT)?;
        c.store_update(1, 1, -frame::T1_BYTES)?;
        // Saved at the top of the frame, so their addresses are fixed relative to
        // the caller's stack pointer rather than to this frame's size.
        for (index, register) in frame::SAVED.into_iter().enumerate() {
            c.store(register, 1, frame::saved_offset(frame::T1_BYTES, index))?;
        }
        c.mov(SLOTS, 3);
        c.mov(STACK, 4);
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
                    // A movable heap constant is read through its stable constants
                    // slot, never baked in as an immediate: the moving minor GC
                    // rewrites that Vec in place and cannot patch machine code
                    // (bliss-d0b).
                    let slot = &bf.constants[*k as usize] as *const EgclVal;
                    c.imm64(ACC, slot as u64);
                    encode!("heap constant load", c.load(ACC, ACC, 0));
                } else {
                    c.imm64(ACC, val.0);
                }
                emit_push(&mut c)?;
            }
            Instr::LoadLocal(i) => {
                encode!(
                    format_args!("LoadLocal {i}: slot beyond the displacement field"),
                    c.load(ACC, SLOTS, 8 * i32::from(*i))
                );
                emit_push(&mut c)?;
            }
            Instr::StoreLocal(i) => {
                emit_pop(&mut c, ACC)?;
                encode!(
                    format_args!("StoreLocal {i}: slot beyond the displacement field"),
                    c.store(ACC, SLOTS, 8 * i32::from(*i))
                );
            }
            Instr::Pop => {
                c.addi(OPSP, OPSP, -8);
            }
            Instr::Dup => {
                encode!("Dup", c.load(ACC, OPSP, -8));
                emit_push(&mut c)?;
            }
            Instr::ClearMv => {
                // Resetting the multiple-values state cannot signal, throw or
                // allocate, so it needs neither the transfer check nor a root
                // map: a bare leaf call, not the full helper crossing. SETQ
                // emits one of these per assignment, so in a counted loop they
                // were two of the five crossings per iteration.
                emit_leaf_call(&mut c, clear_mv_addr)?;
            }
            Instr::CallNamed { sym: callee, nargs } => {
                if allow_speculation
                    && encode!(
                        "fixnum template operand",
                        emit_fixnum_template(&mut c, *callee, *nargs, bcp, &mut deopts)
                    )
                {
                    continue;
                }
                let direct_builtin = super::super::direct_builtin_slot(*callee, *nargs as usize);
                let arg0 = match direct_builtin {
                    Some(slot) => ((slot as u64) << 32) | u64::from(*callee),
                    None => u64::from(*callee),
                };
                c.imm64(3, arg0);
                c.imm64(4, u64::from(*nargs));
                emit_add_disp(&mut c, 5, OPSP, -8 * i32::from(*nargs))?;
                let profile_site = if direct_builtin.is_some() {
                    DIRECT_CALL_GEN.load(std::sync::atomic::Ordering::Relaxed)
                } else if registry_get(*callee).is_some() {
                    call_site_profile_token(bf as *const BytecodeFunction as usize, bcp)
                } else {
                    0
                };
                c.imm64(6, profile_site);
                let target = if direct_builtin.is_some() {
                    builtin_addr
                } else {
                    c2i_addr
                };
                emit_c2i_call(&mut c, target, transfer_addr)?;
                emit_add_disp(&mut c, OPSP, OPSP, -8 * i32::from(*nargs))?;
                emit_push(&mut c)?;
            }
            Instr::SetValues(n) => {
                c.imm64(3, u64::from(values_sym));
                c.imm64(4, u64::from(*n));
                emit_add_disp(&mut c, 5, OPSP, -8 * i32::from(*n))?;
                c.imm64(6, 0);
                emit_c2i_call(&mut c, c2i_addr, transfer_addr)?;
                emit_add_disp(&mut c, OPSP, OPSP, -8 * i32::from(*n))?;
                emit_push(&mut c)?;
            }
            Instr::LoadEnvVar(name_idx) => {
                c.imm64(3, std::ptr::from_ref(bf) as u64);
                c.imm64(4, u64::from(u32::from(*name_idx)));
                emit_c2i_call(&mut c, load_env_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::StoreEnvVar(name_idx) | Instr::DefineEnvVar(name_idx) => {
                emit_pop(&mut c, 5)?;
                c.imm64(3, std::ptr::from_ref(bf) as u64);
                c.imm64(4, u64::from(u32::from(*name_idx)));
                let helper = if matches!(instr, Instr::StoreEnvVar(_)) {
                    store_env_addr
                } else {
                    define_env_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr)?;
            }
            Instr::PushEnvChild | Instr::PopEnvChild => {
                let helper = if matches!(instr, Instr::PushEnvChild) {
                    push_env_addr
                } else {
                    pop_env_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr)?;
            }
            Instr::AllocCons => {
                // The cdr is on top, so it pops first.
                emit_pop(&mut c, 4)?;
                emit_pop(&mut c, 3)?;
                emit_c2i_call(&mut c, alloc_cons_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::EvalHost(index) | Instr::MakeClosureEnv(index) => {
                let Some(slot) = bf.constants.get(*index as usize) else {
                    decline!("constant index {index} is out of range");
                };
                c.imm64(3, slot as *const EgclVal as u64);
                encode!("host form load", c.load(3, 3, 0));
                let helper = if matches!(instr, Instr::EvalHost(_)) {
                    eval_host_addr
                } else {
                    make_closure_addr
                };
                emit_c2i_call(&mut c, helper, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                emit_pop(&mut c, 3)?;
                emit_add_disp(&mut c, 4, SLOTS, 8 * i32::from(*slot_base))?;
                c.imm64(5, u64::from(*nvars));
                emit_c2i_call(&mut c, take_values_addr, transfer_addr)?;
            }
            Instr::LoadGlobal(global) => {
                c.imm64(3, u64::from(*global));
                emit_c2i_call(&mut c, load_global_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::LoadFunction(global) => {
                c.imm64(3, u64::from(*global));
                emit_c2i_call(&mut c, load_function_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::StoreGlobal(global) => {
                emit_pop(&mut c, 4)?;
                c.imm64(3, u64::from(*global));
                emit_c2i_call(&mut c, store_global_addr, transfer_addr)?;
            }
            Instr::ValuesToList => {
                emit_pop(&mut c, 3)?;
                emit_c2i_call(&mut c, values_to_list_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::TypeP(class) => {
                emit_pop(&mut c, 3)?;
                c.imm64(4, u64::from(*class as u32));
                emit_c2i_call(&mut c, typep_class_addr, transfer_addr)?;
                emit_push(&mut c)?;
            }
            Instr::Br(target) => {
                c.jump(match bcp_labels.get(*target as usize) {
                    Some(label) => *label,
                    None => decline!("Br target {target} is out of range"),
                });
            }
            Instr::BrIfFalse(target) => {
                emit_pop(&mut c, ACC)?;
                c.imm64(SCRATCH, egcl_rt::value::NIL_BITS);
                c.compare(0, ACC, SCRATCH);
                c.branch(
                    Cc::E,
                    0,
                    match bcp_labels.get(*target as usize) {
                        Some(label) => *label,
                        None => decline!("BrIfFalse target {target} is out of range"),
                    },
                );
            }
            Instr::Return => {
                emit_pop(&mut c, ACC)?;
                emit_epilogue(&mut c)?;
            }
            // A T1-eligible function is lexically closed over its own blocks and
            // tags, so every compiled transfer is local and the interpreter's
            // handler stack is dead here.
            Instr::PushBlock { .. } | Instr::PushTag { .. } => {}
            Instr::NamedTag { .. } | Instr::PopHandler => {}
            Instr::ReturnFrom { block_id } => {
                let (resume_bcp, sp) = match block_targets.get(block_id) {
                    Some(&target) => target,
                    None => decline!("RETURN-FROM references non-local block {block_id}"),
                };
                encode!("RETURN-FROM value", c.load(ACC, OPSP, -8));
                emit_add_disp(&mut c, OPSP, SLOTS, 8 * (n_locals + i32::from(sp)))?;
                emit_push(&mut c)?;
                c.jump(match bcp_labels.get(resume_bcp as usize) {
                    Some(label) => *label,
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
                    // Sampled loop back-edge poll: this drives T1→T2 promotion and
                    // polls process signals, so a hot native loop stays terminable
                    // and still reaches GC stop-the-world (bliss-7rdu).
                    let keep = c.label();
                    c.imm64(SCRATCH, backedge_counter);
                    // The counter is an AtomicU32, so these are 32-bit accesses: a
                    // 64-bit one would read and write the four bytes past it. It
                    // starts at ZERO and counts UP to the threshold, as on the other
                    // three targets; counting down from that zero start meant the
                    // first poll fired only when the u32 wrapped, so installed T1
                    // loops never escalated to T2 and never polled signals or GC
                    // safepoints (bliss-fsqh3).
                    encode!("poll counter load", c.load_word(ACC, SCRATCH, 0));
                    c.addi(ACC, ACC, 1);
                    encode!("poll counter store", c.store_word(ACC, SCRATCH, 0));
                    c.imm64(4, u64::from(t2_backedge_threshold()));
                    c.compare(0, ACC, 4);
                    c.branch(Cc::L, 0, keep);
                    c.li(ACC, 0);
                    encode!("poll reset", c.store_word(ACC, SCRATCH, 0));
                    let helper = if is_osr {
                        // Signal-only: an OSR loop has no T1 tier to promote from,
                        // but must still be terminable and reach stop-the-world.
                        osr_backedge_addr
                    } else {
                        can_osr_to_t2 = true;
                        c.imm64(3, u64::from(sym));
                        c.imm64(4, u64::from(*target_bcp));
                        c.mov(5, SLOTS);
                        c.imm64(6, std::ptr::from_ref(bf) as u64);
                        t2_backedge_addr
                    };
                    emit_c2i_call(&mut c, helper, transfer_addr)?;
                    c.compare_imm(0, ACC, 0);
                    c.branch(Cc::E, 0, keep);
                    // Leaving the loop: T2 finished, or a signal is pending. The
                    // shared epilogue returns the first operand slot.
                    encode!(
                        "first operand slot for the loop exit",
                        c.load(ACC, SLOTS, 8 * n_locals)
                    );
                    emit_epilogue(&mut c)?;
                    c.bind(keep);
                }
                c.jump(match bcp_labels.get(*target_bcp as usize) {
                    Some(label) => *label,
                    None => decline!("GO target {target_bcp} is out of range"),
                });
            }
            other => decline!("unsupported opcode {other:?}"),
        }
    }

    // A bytecode function always ends in Return, but a trailing fallthrough must not
    // run off the end of the buffer into whatever follows.
    c.li(ACC, 0);
    emit_epilogue(&mut c)?;

    // Cold deopt stubs. The operand stack was not moved before the guard failed, so
    // the live locals and the untouched operands are still in the frame slots;
    // report the bytecode position and the operand depth and leave through the
    // ordinary epilogue. `run_native` sees the recorded state and resumes T0 there.
    for (&deopt_bcp, &label) in &deopts {
        c.bind(label);
        c.subf(4, SLOTS, OPSP);
        c.sradi(4, 4, 3);
        emit_add_disp(&mut c, 4, 4, -n_locals)?;
        c.imm64(3, u64::from(deopt_bcp));
        emit_leaf_call(
            &mut c,
            c2i_deopt_state as extern "C" fn(u64, u64) as usize as u64,
        )?;
        c.imm64(ACC, NIL.0);
        emit_epilogue(&mut c)?;
    }

    // One alternate entry per eligible loop header: the shared prologue, then a
    // branch into the body.
    let mut osr_entries: Vec<(u32, usize)> = Vec::new();
    for header in osr_headers {
        let Some(&target) = bcp_labels.get(header as usize) else {
            decline!("OSR header {header} is out of range");
        };
        let stub_offset = c.here();
        emit_prologue(&mut c)?;
        c.jump(target);
        osr_entries.push((header, stub_offset));
    }

    let bcp_offsets: Vec<u32> = bcp_labels
        .iter()
        .map(|&label| c.label_offset(label).map_or(u32::MAX, |o| o as u32))
        .collect();

    // A conditional branch reaches only ±32 KiB here, 32 times tighter than
    // AArch64's, so a large function exhausts it sooner.
    let Some(code) = c.finish() else {
        decline!("a branch displacement is out of range (function too large)");
    };
    Some(NativeEmission {
        code,
        osr_entries,
        bcp_offsets,
        has_deopt: can_osr_to_t2 || !deopts.is_empty(),
        direct_calls: Vec::new(),
    })
}

/// Push the accumulator: store at the stack top, then advance it.
fn emit_push(c: &mut Asm) -> Option<()> {
    c.store(ACC, OPSP, 0)?;
    c.addi(OPSP, OPSP, 8);
    Some(())
}

/// Pop into `register`: retreat the stack top, then load from it.
fn emit_pop(c: &mut Asm, register: u8) -> Option<()> {
    c.addi(OPSP, OPSP, -8);
    c.load(register, OPSP, 0)
}

/// Inline guarded fixnum fast path for an arithmetic `CallNamed`, or `Some(false)`
/// when the callee or arity has no template (`None` only for an unencodable
/// operand displacement). PEEK-guard-commit, as on x86 and System Z: the operands
/// are read in place and the operand-stack pointer is not moved until every guard
/// has passed, so a failed guard leaves the frame exactly as T0 expects at `bcp`.
///
/// Multiplication is left to the runtime: its overflow test needs `mulhd` and the
/// untagging shift, and the baseline tier gains little from it.
fn emit_fixnum_template(
    c: &mut Asm,
    sym: u32,
    nargs: u16,
    bcp: u32,
    deopts: &mut std::collections::BTreeMap<u32, Label>,
) -> Option<bool> {
    if nargs == 1 {
        let Some(op) = inlinable_unary_fixnum_op(sym) else {
            return Some(false);
        };
        let deopt = *deopts.entry(bcp).or_insert_with(|| c.label());
        c.load(ACC, OPSP, -8)?;
        emit_guard_fixnum(c, ACC, deopt);
        match op {
            UnaryFixnumOp::Incr => {
                c.li(4, 8);
                emit_add_checked(c, 4, deopt);
            }
            UnaryFixnumOp::Decr => {
                c.li(4, 8);
                emit_sub_checked(c, 4, deopt);
            }
            UnaryFixnumOp::Neg => {
                c.mov(4, ACC);
                c.li(ACC, 0);
                emit_sub_checked(c, 4, deopt);
            }
        }
        c.store(ACC, OPSP, -8)?;
        return Some(true);
    }
    if nargs != 2 {
        return Some(false);
    }
    let Some(op) = inlinable_fixnum_op(sym) else {
        return Some(false);
    };
    if matches!(op, FixnumOp::Mul) {
        return Some(false);
    }
    let deopt = *deopts.entry(bcp).or_insert_with(|| c.label());
    c.load(ACC, OPSP, -16)?;
    c.load(4, OPSP, -8)?;
    emit_guard_fixnum(c, ACC, deopt);
    emit_guard_fixnum(c, 4, deopt);
    match op {
        FixnumOp::Add => emit_add_checked(c, 4, deopt),
        FixnumOp::Sub => emit_sub_checked(c, 4, deopt),
        comparison => {
            // Branchless: `isel` picks T or NIL on the compare result.
            c.compare(0, ACC, 4);
            c.imm64(5, T.0);
            c.imm64(6, NIL.0);
            let cc = match comparison {
                FixnumOp::Lt => Cc::L,
                FixnumOp::Gt => Cc::G,
                FixnumOp::Le => Cc::Le,
                FixnumOp::Ge => Cc::Ge,
                FixnumOp::NumEq => Cc::E,
                FixnumOp::Add | FixnumOp::Sub | FixnumOp::Mul => unreachable!(),
            };
            c.isel(ACC, 5, 6, cc, 0);
        }
    }
    c.store(ACC, OPSP, -16)?;
    c.addi(OPSP, OPSP, -8);
    Some(true)
}

/// Deopt unless `register` carries the fixnum tag (low three bits clear).
fn emit_guard_fixnum(c: &mut Asm, register: u8, deopt: Label) {
    c.li(SCRATCH, egcl_rt::value::TAG_MASK as i16);
    c.and(SCRATCH, register, SCRATCH);
    c.compare_imm(0, SCRATCH, 0);
    c.branch(Cc::Ne, 0, deopt);
}

/// `ACC = ACC + rhs`, deopting on signed overflow. Tagged fixnums add directly.
/// `XER[SO]` is sticky and cannot serve a per-operation guard (see the T2 emitter),
/// so the sign test is explicit: overflow iff the operands agree in sign and the
/// result disagrees with them.
fn emit_add_checked(c: &mut Asm, rhs: u8, deopt: Label) {
    c.add(5, ACC, rhs);
    c.xor(6, ACC, 5);
    c.xor(SCRATCH, rhs, 5);
    c.and(6, 6, SCRATCH);
    c.compare_imm(0, 6, 0);
    c.mov(ACC, 5);
    c.branch(Cc::L, 0, deopt);
}

/// `ACC = ACC - rhs`, deopting on signed overflow: overflow iff the operands
/// differ in sign and the result's sign differs from the minuend's. Subtracting
/// directly, rather than negating and adding, keeps the most negative fixnum
/// correct.
fn emit_sub_checked(c: &mut Asm, rhs: u8, deopt: Label) {
    c.subf(5, rhs, ACC);
    c.xor(6, ACC, rhs);
    c.xor(SCRATCH, ACC, 5);
    c.and(6, 6, SCRATCH);
    c.compare_imm(0, 6, 0);
    c.mov(ACC, 5);
    c.branch(Cc::L, 0, deopt);
}

/// Call a Rust leaf that neither allocates nor re-enters Lisp, with the arguments
/// already in r3 onwards. Only the TOC and the call itself; no transfer check.
fn emit_leaf_call(c: &mut Asm, target: u64) -> Option<()> {
    c.store(TOC, 1, frame::TOC_SLOT)?;
    c.imm64(TARGET, target);
    c.move_to_count(TARGET);
    c.call_count();
    c.load(TOC, 1, frame::TOC_SLOT)
}

fn emit_epilogue(c: &mut Asm) -> Option<()> {
    for (index, register) in frame::SAVED.into_iter().enumerate() {
        c.load(register, 1, frame::saved_offset(frame::T1_BYTES, index))?;
    }
    c.addi(1, 1, frame::T1_BYTES as i16);
    c.load(LINK_TEMP, 1, frame::LINK_SLOT)?;
    c.move_to_link(LINK_TEMP);
    c.ret();
    Some(())
}

/// `destination = source + displacement`, materialising the offset when it is
/// beyond the add-immediate field.
fn emit_add_disp(c: &mut Asm, destination: u8, source: u8, displacement: i32) -> Option<()> {
    match i16::try_from(displacement) {
        Ok(small) => c.addi(destination, source, small),
        Err(_) => {
            c.imm64(SCRATCH, displacement as i64 as u64);
            c.add(destination, source, SCRATCH);
        }
    }
    Some(())
}

/// Call a c2i helper whose arguments are already in r3 onwards, then honour a
/// pending non-local transfer.
///
/// An indirect call goes through the count register, and the ABI expects the
/// target's address in r12 so that a callee entered at its global entry point can
/// compute its own TOC. r2 is saved and restored around the call because of that.
///
/// Unlike the x86-64 baseline, this does not bracket the call with the
/// SIGSEGV-recovery toggle. Recovery cannot resume on ppc64le yet
/// (`rewrite_ucontext_ip` is unimplemented here, bliss-bdly0), so publishing a
/// zero recovery address around each helper changed nothing observable and cost
/// about 36 instructions and two extra calls per crossing (bliss-8klqs). When
/// recovery arrives on this target, publish it once per native entry rather than
/// per call (bliss-fgtb).
fn emit_c2i_call(c: &mut Asm, target: u64, transfer_addr: u64) -> Option<()> {
    c.store(TOC, 1, frame::TOC_SLOT)?;
    c.imm64(TARGET, target);
    c.move_to_count(TARGET);
    c.call_count();
    c.load(TOC, 1, frame::TOC_SLOT)?;

    // Stop T1 at a call that initiated an error, THROW or RETURN-FROM. The helper
    // stashes the condition and returns NIL, so without this check native execution
    // would continue into code that must not run.
    let resume = c.label();
    c.store(ACC, 1, frame::SCRATCH_SLOT)?;
    c.imm64(TARGET, transfer_addr);
    c.move_to_count(TARGET);
    c.call_count();
    c.compare_imm(0, ACC, 0);
    c.load(TOC, 1, frame::TOC_SLOT)?;
    c.branch(Cc::E, 0, resume);
    // A transfer is pending: leave through the epilogue with the saved result.
    c.load(ACC, 1, frame::SCRATCH_SLOT)?;
    emit_epilogue(c)?;
    c.bind(resume);
    c.load(ACC, 1, frame::SCRATCH_SLOT)?;
    Some(())
}
