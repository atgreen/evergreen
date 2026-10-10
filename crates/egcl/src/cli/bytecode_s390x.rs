// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! s390x baseline native compiler, sharing T0's GC-scanned activation slots.

use super::*;
use egcl_rt::asm_s390x::{Asm as ZAsm, Label as ZLabel};

// r8 = activation slots, r9 = next operand slot, r10 = OSR's live tagbody id,
// r11 = the EgclStack (the entry's second argument), r12/r7 hold the caller's
// frame pointer and stack offset across a direct native call. These are
// callee-saved raw addresses, never unrooted Lisp values.
// r2-r5 are the first four System Z integer arguments; r2 is the result.
fn prologue(a: &mut ZAsm, locals: u16) {
    a.prologue();
    a.mov(8, 2);
    a.mov(11, 3);
    a.address(9, 8, 8 * i32::from(locals));
}

/// A direct native-to-native call at a `CallNamed` site (bliss-zhvn stage 2,
/// ported from the x86-64 T1 emitter for bliss-767gn): push the callee's
/// EgclStack frame, bind `nargs` arguments from the caller's operand stack into
/// its slots, call its entry directly, then pop the frame and push the result.
/// No c2i dispatch, no name resolution, no registry probe: on a z17 the generic
/// adapter costs ~0.78 us per call against 77 ns for an inline loop iteration.
///
/// Two guards fall back to the c2i sequence at `slow`: the direct-call
/// generation (a redefinition or uninstallation since the site was baked), and
/// the EgclStack bounds, which also caps direct recursion before the C stack
/// fills (c2i raises the catchable STORAGE-CONDITION). The callee's entry
/// preserves r6-r15, so r8-r12 and the saved r7 survive it; NATIVE_ENV and the
/// recovery IPs are already right, set by the enclosing run_native. Errors and
/// deopts propagate through the transfer check exactly as after a helper call;
/// the emit-time `has_deopt` gate excludes callees that could resume T0
/// mid-call.
#[allow(clippy::too_many_arguments)]
fn direct_native_call(
    a: &mut ZAsm,
    entry: u64,
    code_info: u64,
    num_slots: u16,
    nargs: u16,
    baked_gen: u64,
    slow: ZLabel,
    exit: ZLabel,
) {
    let bs_base = egcl_rt::EgclStack::OFFSET_BASE as i32;
    let bs_sp = egcl_rt::EgclStack::OFFSET_SP_OFFSET as i32;
    let bs_fp = egcl_rt::EgclStack::OFFSET_FP as i32;
    let bs_cap = egcl_rt::EgclStack::OFFSET_CAPACITY as i32;
    let f_prev = core::mem::offset_of!(Frame, prev_fp) as i32;
    let f_ret = core::mem::offset_of!(Frame, return_pc) as i32;
    let f_func = core::mem::offset_of!(Frame, function) as i32;
    let f_ci = core::mem::offset_of!(Frame, code_info) as i32;
    let f_flags = core::mem::offset_of!(Frame, flags) as i32;
    let f_nloc = core::mem::offset_of!(Frame, num_locals) as i32;
    let hdr = core::mem::size_of::<Frame>() as i32;
    let framebytes = hdr + 8 * i32::from(num_slots);
    let nargs = i32::from(nargs);
    let nslots = i32::from(num_slots);
    // Guard 1: stability. r4 = current generation, r5 = baked; differ => slow.
    a.imm64(1, std::ptr::addr_of!(DIRECT_CALL_GEN) as u64);
    a.load(4, 1, 0);
    a.imm64(5, baked_gen);
    a.compare(4, 5);
    a.branch(6, slow);
    // r2 = base, r3 = old_sp, r4 = new_sp = old_sp + framebytes.
    a.load(2, 11, bs_base);
    a.load(3, 11, bs_sp);
    a.address(4, 3, framebytes);
    // Guard 2: EgclStack bounds. new_sp > capacity => slow.
    a.load(5, 11, bs_cap);
    a.compare(4, 5);
    a.branch(2, slow);
    // r5 = frame_start = base + old_sp; r12 = old_fp; r7 = old_sp (both
    // callee-saved, restored after the call).
    a.mov(5, 2);
    a.add(5, 3);
    a.load(12, 11, bs_fp);
    a.mov(7, 3);
    // Frame header.
    a.store(12, 5, f_prev);
    a.imm64(1, 0);
    a.store(1, 5, f_ret);
    a.imm64(1, NIL.0);
    a.store(1, 5, f_func);
    a.imm64(1, code_info);
    a.store(1, 5, f_ci);
    a.imm64(1, u64::from(FLAG_CALL));
    a.store_u32(1, 5, f_flags);
    a.imm64(1, nslots as u64);
    a.store_u16(1, 5, f_nloc);
    // Bind the arguments: slot i = caller operand [r9 - 8*nargs + 8*i].
    for i in 0..nargs {
        a.load(1, 9, -8 * nargs + 8 * i);
        a.store(1, 5, hdr + 8 * i);
    }
    // The remaining slots start as NIL.
    if nargs < nslots {
        a.imm64(1, NIL.0);
        for i in nargs..nslots {
            a.store(1, 5, hdr + 8 * i);
        }
    }
    // Publish fp = frame_start and sp_offset = new_sp so the GC scans the frame.
    a.store(5, 11, bs_fp);
    a.store(4, 11, bs_sp);
    // Call the entry: r2 = the callee's slots, r3 = the EgclStack.
    a.address(2, 5, hdr);
    a.mov(3, 11);
    a.imm64(1, entry);
    a.call_reg(1);
    // Pop the callee frame: restore fp and sp_offset from the saved copies.
    a.store(12, 11, bs_fp);
    a.store(7, 11, bs_sp);
    // Unpublished; now the ordinary post-helper transfer check (r7 is free again).
    a.mov(7, 2);
    call(a, c2i_transfer_pending as *const () as u64);
    a.imm64(0, 0);
    a.compare(2, 0);
    a.mov(2, 7);
    a.branch(6, exit);
    // Pop the arguments off the caller operand stack and push the result.
    a.address(9, 9, -8 * nargs);
    push(a);
}

fn push(a: &mut ZAsm) {
    a.store(2, 9, 0);
    a.add_imm(9, 8);
}

fn pop(a: &mut ZAsm, reg: u8) {
    a.add_imm(9, -8);
    a.load(reg, 9, 0);
}

fn call(a: &mut ZAsm, address: u64) {
    a.imm64(1, address);
    a.call_reg(1);
}

/// The helper's arguments are already rooted in activation slots (or rooted
/// by the helper before its first allocation). After it returns, check for a
/// pending transfer before executing any more Lisp instructions. The result
/// may live in r7 across this check because c2i_transfer_pending cannot GC.
fn helper(a: &mut ZAsm, address: u64, exit: ZLabel) {
    call(a, address);
    a.mov(7, 2);
    call(a, c2i_transfer_pending as *const () as u64);
    a.imm64(0, 0);
    a.compare(2, 0);
    a.mov(2, 7);
    a.branch(6, exit);
}

fn guard_fixnum(a: &mut ZAsm, reg: u8, deopt: ZLabel) {
    a.mov(4, reg);
    a.imm64(0, egcl_rt::value::TAG_MASK);
    a.and(4, 0);
    a.branch(7, deopt);
}

fn boolean(a: &mut ZAsm, mask: u8) {
    let yes = a.label();
    let end = a.label();
    a.branch(mask, yes);
    a.imm64(2, NIL.0);
    a.branch(15, end);
    a.bind(yes);
    a.imm64(2, T.0);
    a.bind(end);
}

/// Peek/guard/commit keeps both operands and the operand pointer unchanged on
/// every guard edge. The shared deopt adapter can resume the exact bytecode.
fn arithmetic(
    a: &mut ZAsm,
    sym: u32,
    nargs: u16,
    bcp: u32,
    deopts: &mut std::collections::BTreeMap<u32, ZLabel>,
) -> bool {
    let supported = (nargs == 1 && inlinable_unary_fixnum_op(sym).is_some())
        || (nargs == 2 && inlinable_fixnum_op(sym).is_some());
    if !supported { return false; }
    let Some(egcl_compiler::t2::ir::AuxData::BuiltinBinding { address, expected }) =
        egcl_compiler::t2::builtin_binding::snapshot(sym)
    else { return false; };
    let deopt = *deopts.entry(bcp).or_insert_with(|| a.label());
    a.imm64(1, address as u64);
    a.load(2, 1, 0);
    a.imm64(3, expected);
    a.compare(2, 3);
    a.branch(6, deopt);
    if nargs == 1 {
        if let Some(op) = inlinable_unary_fixnum_op(sym) {
            let deopt = *deopts.entry(bcp).or_insert_with(|| a.label());
            a.load(2, 9, -8);
            guard_fixnum(a, 2, deopt);
            match op {
                UnaryFixnumOp::Incr => a.add_imm(2, 8),
                UnaryFixnumOp::Decr => a.add_imm(2, -8),
                UnaryFixnumOp::Neg => {
                    a.imm64(3, 0);
                    a.sub(3, 2);
                    a.mov(2, 3);
                }
            }
            a.branch(1, deopt); // arithmetic CC=3: signed overflow
            a.store(2, 9, -8);
            return true;
        }
    }
    if nargs != 2 {
        return false;
    }
    let Some(op) = inlinable_fixnum_op(sym) else {
        return false;
    };
    let deopt = *deopts.entry(bcp).or_insert_with(|| a.label());
    a.load(2, 9, -16);
    a.load(3, 9, -8);
    guard_fixnum(a, 2, deopt);
    guard_fixnum(a, 3, deopt);
    match op {
        FixnumOp::Add | FixnumOp::Sub => {
            if matches!(op, FixnumOp::Add) {
                a.add(2, 3);
            } else {
                a.sub(2, 3);
            }
            a.branch(1, deopt);
        }
        FixnumOp::Mul => {
            // The full-width check the T2 emitter uses (multiply_fixnums):
            // MLGR gives the unsigned 128-bit product of the untagged lhs and
            // the tagged rhs on every z10-compatible CPU, so the low half is
            // the tagged product. Correct the high half for signed operands,
            // then require it to equal the low half's sign extension; else
            // the product overflowed a fixnum and the site deopts with both
            // operands and the operand pointer untouched. MSGR alone would
            // wrap silently, which is why T1 called the numeric runtime for
            // every `*` before (t1_deopt saw no deopt for a bignum product).
            a.shift_right_signed(5, 2, 3);
            a.mov(4, 3);
            a.mov(3, 5);
            a.multiply_unsigned_wide(2, 4);
            a.imm64(0, 0);
            let lhs_nonnegative = a.label();
            a.compare(5, 0);
            a.branch(10, lhs_nonnegative);
            a.sub(2, 4);
            a.bind(lhs_nonnegative);
            let rhs_nonnegative = a.label();
            a.compare(4, 0);
            a.branch(10, rhs_nonnegative);
            a.sub(2, 5);
            a.bind(rhs_nonnegative);
            a.shift_right_signed(4, 3, 63);
            a.compare(2, 4);
            a.branch(6, deopt);
            a.mov(2, 3);
        }
        _ => {
            a.compare(2, 3);
            boolean(
                a,
                match op {
                    FixnumOp::Lt => 4,
                    FixnumOp::Gt => 2,
                    FixnumOp::Le => 12,
                    FixnumOp::Ge => 10,
                    FixnumOp::NumEq => 8,
                    _ => unreachable!(),
                },
            );
        }
    }
    a.store(2, 9, -16);
    a.add_imm(9, -8);
    true
}

pub(super) fn emit_native(
    bf: &BytecodeFunction,
    allow_speculation: bool,
    sym: u32,
    backedge_counter: u64,
    allow_traps: bool,
) -> Option<NativeEmission> {
    let env_names = NativeEnvNames::new(bf);
    // Parameters live in slots unless the function captures them into a
    // heap EnvFrame, in which case the bytecode reaches them through
    // LoadEnvVar and the activation may be narrower than the arity: a closure
    // factory with five captured parameters has one slot. Only the former
    // shape can violate the slot count, and then only through an invariant
    // breach the emitter must not build on.
    if bf.arity > bf.num_slots() && !bf.has_env {
        return None;
    }
    let values_sym = resolve_sym("VALUES")?.as_symbol_index();
    let mut a = ZAsm::new();
    let labels: Vec<_> = bf.code.iter().map(|_| a.label()).collect();
    let exit = a.label();
    let mut deopts = std::collections::BTreeMap::new();
    let mut blocks = HashMap::new();
    let mut tags = HashMap::new();
    for instr in &bf.code {
        match *instr {
            Instr::PushBlock {
                block_id,
                resume_bcp,
                sp_restore,
                ..
            } => {
                blocks.insert(block_id, (resume_bcp, sp_restore));
            }
            Instr::PushTag {
                tagbody_id,
                sp_restore,
            } => {
                tags.insert(tagbody_id, sp_restore);
            }
            _ => {}
        }
    }
    let reset = |a: &mut ZAsm, depth: u16| {
        a.address(9, 8, 8 * (i32::from(bf.n_locals) + i32::from(depth)));
    };
    let mut osr_headers = std::collections::BTreeSet::new();
    let mut can_osr_to_t2 = false;
    // Callees called directly; retained so their code outlives a registry
    // replacement while this caller's code still jumps into it.
    let mut direct_calls = Vec::new();
    prologue(&mut a, bf.n_locals);
    for (index, instr) in bf.code.iter().enumerate() {
        let bcp = index as u32;
        a.bind(labels[index]);
        match *instr {
            // OSR inherits the interpreter's active handler stack. Native
            // scope elision is sound for ordinary T1 entry, but an OSR exit
            // must let T0 actually establish/unwind/pop scopes. Otherwise a
            // later deopt can deliver THROW to an already expired CATCH and
            // replay the side effects following that catch.
            Instr::PushBlock { .. }
            | Instr::PushTag { .. }
            | Instr::PopHandler
            | Instr::ReturnFrom { .. }
                if sym == u32::MAX =>
            {
                let target = *deopts.entry(bcp).or_insert_with(|| a.label());
                a.branch(15, target);
            }
            Instr::Const(k) => {
                // The registry-rooted constant slot is stable; its contents can
                // move during GC and must be reloaded on each execution.
                a.imm64(2, bf.constants.get(k as usize)? as *const EgclVal as u64);
                a.load(2, 2, 0);
                push(&mut a);
            }
            Instr::LoadLocal(i) => {
                a.load(2, 8, 8 * i32::from(i));
                push(&mut a);
            }
            Instr::StoreLocal(i) => {
                pop(&mut a, 2);
                a.store(2, 8, 8 * i32::from(i));
            }
            Instr::Pop => a.add_imm(9, -8),
            // A full fence for every kind: z/Architecture orders loads and
            // stores already, and BCR 14,0 serializes the rest. Pushes NIL,
            // the barrier's value, as the x86-64 emitter does.
            Instr::MemoryFence(_) => {
                a.serialize();
                a.imm64(2, NIL.0);
                push(&mut a);
            }
            Instr::Dup => {
                a.load(2, 9, -8);
                push(&mut a);
            }
            Instr::CallNamed { nargs, .. } | Instr::SetValues(nargs) => {
                let callee = match *instr {
                    Instr::CallNamed { sym, .. } => sym,
                    _ => values_sym,
                };
                if allow_speculation
                    && matches!(instr, Instr::CallNamed { .. })
                    && arithmetic(&mut a, callee, nargs, bcp, &mut deopts)
                {
                    continue;
                }
                // Direct native->native fast path with a runtime c2i fallback
                // (bliss-zhvn, bliss-767gn). On a guard miss the emitted code
                // jumps to `slow`, which falls into the c2i emission below; on
                // success it jumps past c2i to `direct_after`. The eligibility
                // test is the x86-64 emitter's, verbatim.
                let mut direct_after: Option<ZLabel> = None;
                if matches!(instr, Instr::CallNamed { .. }) && nn_direct_enabled() {
                    // Capture the guard before the lookup: a concurrent
                    // replacement must not stamp an old target with its newer
                    // generation.
                    let baked_gen = direct_call_gen();
                    if let (Some(cbf), Some(cnc)) = (
                        registry_get(callee),
                        NATIVE_REGISTRY.with(|r| r.borrow().get(&callee).cloned()),
                    ) {
                        let fixed = !cbf.variadic
                            && cbf.max_args == Some(cbf.min_args)
                            && cbf.min_args == nargs;
                        let no_types = cbf
                            .param_types
                            .iter()
                            .all(|t| matches!(t, DeclaredType::Any));
                        let not_closure = !cbf.has_env
                            && closure_captured_env(EgclVal::from_symbol_index(callee)).is_none()
                            && !closure_controls().borrow().contains_key(&callee);
                        if replacement_function(callee).is_none()
                            && native_transfer_abi_compatible(&cnc)
                            // A deopting T2 callee resumes T0 inline and
                            // returns a finished value (bliss-w6aki); a T1
                            // callee's deopt still waits for run_native.
                            && (!cnc.has_deopt || cnc.is_t2)
                            && fixed
                            && no_types
                            && not_closure
                        {
                            egcl_rt::blog!(
                                "compile",
                                egcl_rt::log::TRACE,
                                "[T1] {}: direct call to {} [T{}]",
                                display_fn_name(&bf.name),
                                display_fn_name(&cbf.name),
                                if cnc.is_t2 { 2 } else { 1 }
                            );
                            let slow = a.label();
                            let after = a.label();
                            direct_native_call(
                                &mut a,
                                cnc.entry as u64,
                                cnc.code_info as *const CodeInfo as u64,
                                cnc.num_slots,
                                nargs,
                                baked_gen,
                                slow,
                                exit,
                            );
                            direct_calls.push(cnc);
                            a.branch(15, after);
                            a.bind(slow);
                            direct_after = Some(after);
                        }
                    }
                }
                // A leaf builtin the call site resolved at compile time is
                // called through the direct adapter with its table slot baked
                // in and the invalidation generation in place of a profile
                // token (builtins do not tier, so there is nothing to profile),
                // exactly as the x86-64 T1 emitter does (bliss-x5y.27). Same
                // slice convention, so only r2, r5 and the target differ. Every
                // call used to be resolved BY NAME in c2i (bliss-flzmi).
                let direct_builtin = super::super::direct_builtin_slot(callee, usize::from(nargs));
                match direct_builtin {
                    Some(slot) => a.imm64(2, (u64::from(slot) << 32) | u64::from(callee)),
                    None => a.imm64(2, u64::from(callee)),
                }
                a.imm64(3, u64::from(nargs));
                a.address(4, 9, -8 * i32::from(nargs));
                let profile = if direct_builtin.is_some() {
                    direct_call_gen()
                } else if registry_get(callee).is_some() {
                    call_site_profile_token(bf as *const BytecodeFunction as usize, bcp)
                } else {
                    0
                };
                a.imm64(5, profile);
                let adapter = if direct_builtin.is_some() {
                    c2i_call_builtin as *const () as u64
                } else {
                    c2i_call_slice as *const () as u64
                };
                helper(&mut a, adapter, exit);
                a.address(9, 9, -8 * i32::from(nargs));
                push(&mut a);
                // The direct path's success edge lands past the c2i sequence.
                if let Some(after) = direct_after {
                    a.bind(after);
                }
            }
            Instr::Br(target) => a.branch(15, *labels.get(target as usize)?),
            Instr::BrIfFalse(target) | Instr::BrIfTrue(target) => {
                pop(&mut a, 2);
                a.imm64(0, NIL.0);
                a.compare(2, 0);
                a.branch(
                    if matches!(instr, Instr::BrIfFalse(_)) {
                        8
                    } else {
                        6
                    },
                    *labels.get(target as usize)?,
                );
            }
            // NamedTag only publishes a tag on the shared control-token stack
            // so a non-local GO from a nested closure can reach it; local GO
            // is a jump to the tag's label. The native_would_lose_captured_
            // control guard keeps any function whose closure captures such a
            // tag at T0, so a NamedTag reaching this emitter is local-only and
            // the publish is dead, as emit.rs concludes for x86-64 (bliss-x5y).
            Instr::PushBlock { .. }
            | Instr::PushTag { .. }
            | Instr::NamedTag { .. }
            | Instr::PopHandler => {}
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                if sym == u32::MAX {
                    // OSR stubs identify the live tagbody in r10. A GO to a
                    // different tagbody must unwind through T0's handlers.
                    let target = *deopts.entry(bcp).or_insert_with(|| a.label());
                    a.imm64(0, u64::from(tagbody_id));
                    a.compare(10, 0);
                    a.branch(6, target);
                }
                let depth = *tags.get(&tagbody_id)?;
                reset(&mut a, depth);
                if target_bcp < bcp && depth == 0 {
                    osr_headers.insert(target_bcp);
                    if backedge_counter != 0 {
                        let keep = a.label();
                        // Match x86's per-body counter: short invocations
                        // contribute heat too. Use explicit 32-bit accesses;
                        // a 64-bit access would overrun the AtomicU32 and also
                        // misread its value on this big-endian target.
                        a.imm64(1, backedge_counter);
                        a.load_u32(2, 1, 0);
                        a.add_imm(2, 1);
                        a.store_u32(2, 1, 0);
                        a.imm64(3, u64::from(t2_backedge_threshold()));
                        a.compare(2, 3);
                        a.branch(4, keep);
                        a.imm64(2, 0);
                        a.store_u32(2, 1, 0);
                        if sym == u32::MAX {
                            helper(&mut a, c2i_osr_backedge as *const () as u64, exit);
                        } else {
                            can_osr_to_t2 = true;
                            a.imm64(2, u64::from(sym));
                            a.imm64(3, u64::from(target_bcp));
                            a.mov(4, 8);
                            a.imm64(5, bf as *const BytecodeFunction as u64);
                            helper(&mut a, c2i_t1_backedge as *const () as u64, exit);
                        }
                        a.imm64(0, 0);
                        a.compare(2, 0);
                        a.branch(8, keep);
                        a.load(2, 8, 8 * i32::from(bf.n_locals));
                        a.branch(15, exit);
                        a.bind(keep);
                    }
                }
                a.branch(15, *labels.get(target_bcp as usize)?);
            }
            Instr::ReturnFrom { block_id } => {
                let &(target, depth) = blocks.get(&block_id)?;
                a.load(2, 9, -8);
                reset(&mut a, depth);
                push(&mut a);
                a.branch(15, *labels.get(target as usize)?);
            }
            Instr::Return => {
                pop(&mut a, 2);
                a.branch(15, exit);
            }
            Instr::ClearMv => helper(&mut a, c2i_clear_mv as *const () as u64, exit),
            Instr::LoadGlobal(s) | Instr::LoadFunction(s) => {
                a.imm64(2, u64::from(s));
                helper(
                    &mut a,
                    if matches!(instr, Instr::LoadGlobal(_)) {
                        c2i_load_global as *const () as u64
                    } else {
                        c2i_load_function as *const () as u64
                    },
                    exit,
                );
                push(&mut a);
            }
            Instr::StoreGlobal(s) => {
                pop(&mut a, 3);
                a.imm64(2, u64::from(s));
                helper(&mut a, c2i_store_global as *const () as u64, exit);
            }
            Instr::LoadEnvVar(i) => {
                a.imm64(2, std::ptr::from_ref(&*env_names) as u64);
                a.imm64(3, u64::from(i));
                helper(&mut a, c2i_load_env as *const () as u64, exit);
                push(&mut a);
            }
            Instr::StoreEnvVar(i) | Instr::DefineEnvVar(i) => {
                pop(&mut a, 4);
                a.imm64(2, std::ptr::from_ref(&*env_names) as u64);
                a.imm64(3, u64::from(i));
                helper(
                    &mut a,
                    if matches!(instr, Instr::StoreEnvVar(_)) {
                        c2i_store_env as *const () as u64
                    } else {
                        c2i_define_env as *const () as u64
                    },
                    exit,
                );
            }
            Instr::PushEnvChild | Instr::PopEnvChild => {
                helper(
                    &mut a,
                    if matches!(instr, Instr::PushEnvChild) {
                        c2i_push_env_child as *const () as u64
                    } else {
                        c2i_pop_env_child as *const () as u64
                    },
                    exit,
                );
            }
            Instr::AllocCons => {
                pop(&mut a, 3);
                pop(&mut a, 2);
                helper(&mut a, c2i_alloc_cons as *const () as u64, exit);
                push(&mut a);
            }
            // A LAMBDA in native code: the nested body pointer and the capture
            // flag go to the adapter, which builds the closure over the current
            // native environment frame. The executing NativeCode retains its
            // body, nested functions included, so the pointer stays valid even
            // if the global name is redefined meanwhile. Without this arm any
            // function containing a LAMBDA declined T1 on s390x and stayed
            // interpreted (native_portable_closure's four factory tests).
            Instr::MakeClosure { func, capture_env } => {
                let nested =
                    bf.nested_functions.get(func as usize)? as *const Arc<BytecodeFunction>;
                a.imm64(2, nested as u64);
                a.imm64(3, u64::from(capture_env));
                helper(&mut a, c2i_make_bytecode_closure as *const () as u64, exit);
                push(&mut a);
            }
            Instr::EvalHost(k) | Instr::MakeClosureEnv(k) => {
                a.imm64(2, bf.constants.get(k as usize)? as *const EgclVal as u64);
                a.load(2, 2, 0);
                helper(
                    &mut a,
                    if matches!(instr, Instr::EvalHost(_)) {
                        c2i_eval_host as *const () as u64
                    } else {
                        c2i_make_closure as *const () as u64
                    },
                    exit,
                );
                push(&mut a);
            }
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                pop(&mut a, 2);
                a.address(3, 8, 8 * i32::from(slot_base));
                a.imm64(4, u64::from(nvars));
                helper(&mut a, c2i_take_values as *const () as u64, exit);
            }
            Instr::ValuesToList => {
                pop(&mut a, 2);
                helper(&mut a, c2i_values_to_list as *const () as u64, exit);
                push(&mut a);
            }
            Instr::TypeP(class) => {
                pop(&mut a, 2);
                a.imm64(3, u64::from(class));
                helper(&mut a, c2i_typep_class as *const () as u64, exit);
                push(&mut a);
            }
            _ if allow_traps => {
                let target = *deopts.entry(bcp).or_insert_with(|| a.label());
                a.branch(15, target);
            }
            unsupported => {
                // Name the refusal: a silent None here cost a function its
                // T1 tier with no trace at all (bliss-xltiz found a factory
                // and an OSR loop declining this way on s390x).
                egcl_rt::blog!(
                    "compile",
                    egcl_rt::log::TRACE,
                    "[T1] {}: s390x emitter declined on {:?} at bcp {}",
                    display_fn_name(&bf.name),
                    unsupported,
                    bcp
                );
                return None;
            }
        }
    }
    a.bind(exit);
    a.epilogue();
    for (&bcp, &label) in &deopts {
        a.bind(label);
        a.mov(3, 9);
        a.sub(3, 8);
        a.shift_right_signed(3, 3, 3);
        a.imm64(0, u64::from(bf.n_locals));
        a.sub(3, 0);
        a.imm64(2, u64::from(bcp));
        call(&mut a, c2i_deopt_state as *const () as u64);
        a.branch(15, exit);
    }
    let mut osr_entries = Vec::new();
    for header in osr_headers {
        osr_entries.push((header, a.here()));
        prologue(&mut a, bf.n_locals);
        let tagbody = bf.code.iter().find_map(|instr| match *instr {
            Instr::Go {
                tagbody_id,
                target_bcp,
            } if target_bcp == header => Some(tagbody_id),
            _ => None,
        })?;
        a.imm64(10, u64::from(tagbody));
        a.branch(15, *labels.get(header as usize)?);
    }
    let bcp_offsets = labels
        .iter()
        .map(|&label| a.label_offset(label).unwrap() as u32)
        .collect();
    Some(NativeEmission {
        env_names,
        code: a.finish()?,
        osr_entries,
        bcp_offsets,
        has_deopt: !deopts.is_empty() || can_osr_to_t2,
        direct_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn install(name: &str, source: &str, env: &Env) -> (u32, Rc<NativeCode>) {
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(form = reader::read_from_string(source).unwrap().0);
        let body = compile_function(name, NIL, *form, env, false, false).unwrap();
        registry_put(symbol, Arc::new(body));
        (
            symbol,
            try_promote_to_t1(symbol).expect("must install s390x T1"),
        )
    }

    #[test]
    fn s390x_native_deopt_preserves_side_effects_and_operands() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let count = egcl_rt::symbols::intern("S390X-DEOPT-COUNT");
        egcl_rt::symbols::set_symbol_value(count, EgclVal::from_fixnum(0));
        let (symbol, native) = install(
            "S390X-DEOPT",
            "(
            (setq s390x-deopt-count (1+ s390x-deopt-count))
            (list 'before (+ 1152921504606846975 1)))",
            &env,
        );
        assert!(native.has_deopt);
        let before = deopt_count();
        let value = run_native(&native, symbol, &[], &mut env).unwrap();
        assert_eq!(
            crate::cli::format_val(value),
            "(BEFORE 1152921504606846976)"
        );
        assert_eq!(
            egcl_rt::symbols::symbol_value(count),
            Some(EgclVal::from_fixnum(1))
        );
        assert!(
            deopt_count() > before,
            "must execute the overflow deopt edge"
        );
        registry_remove(symbol);
    }

    #[test]
    fn s390x_native_error_stops_before_later_side_effects() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let count = egcl_rt::symbols::intern("S390X-ERROR-COUNT");
        egcl_rt::symbols::set_symbol_value(count, EgclVal::from_fixnum(0));
        let (symbol, native) = install(
            "S390X-ERROR",
            "(
            (car 17) (setq s390x-error-count 99))",
            &env,
        );
        assert!(run_native(&native, symbol, &[], &mut env).is_err());
        assert_eq!(
            egcl_rt::symbols::symbol_value(count),
            Some(EgclVal::from_fixnum(0))
        );
        registry_remove(symbol);
    }

    #[test]
    fn s390x_native_allocation_and_multiple_values_preserve_live_roots() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, native) = install(
            "S390X-ROOTS",
            "(
            (let ((x (list 1 2)))
              (multiple-value-bind (a b) (values (list x x) (list x))
                (list a b x '(constant root)))))",
            &env,
        );
        for _ in 0..4 {
            let value = run_native(&native, symbol, &[], &mut env).unwrap();
            assert_eq!(
                crate::cli::format_val(value),
                "(((1 2) (1 2)) ((1 2)) (1 2) (CONSTANT ROOT))"
            );
        }
        registry_remove(symbol);
    }

    #[test]
    fn s390x_native_osr_enters_live_frame_without_reinitializing_locals() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
        let mut body = compile_function("S390X-OSR", NIL, *form, &env, false, false).unwrap();
        let plus = resolve_sym("+").unwrap().as_symbol_index();
        let gt = resolve_sym(">").unwrap().as_symbol_index();
        let dec = resolve_sym("1-").unwrap().as_symbol_index();
        body.n_locals = 2;
        body.max_stack = 2;
        body.constants = vec![EgclVal::from_fixnum(0)];
        body.code = vec![
            Instr::PushTag {
                tagbody_id: 0,
                sp_restore: 0,
            },
            Instr::LoadLocal(1),
            Instr::Const(0),
            Instr::CallNamed { sym: gt, nargs: 2 },
            Instr::BrIfFalse(13),
            Instr::LoadLocal(0),
            Instr::LoadLocal(1),
            Instr::CallNamed {
                sym: plus,
                nargs: 2,
            },
            Instr::StoreLocal(0),
            Instr::LoadLocal(1),
            Instr::CallNamed { sym: dec, nargs: 1 },
            Instr::StoreLocal(1),
            Instr::Go {
                tagbody_id: 0,
                target_bcp: 1,
            },
            Instr::LoadLocal(0),
            Instr::Return,
        ];
        let body = Arc::new(body);
        let osr = compile_osr_code(&body, u32::MAX).expect("must compile OSR");
        let thread = egcl_rt::current_thread();
        let stack = thread.stack();
        let frame = stack
            .push_frame(NIL, osr.code_info, osr.num_slots, FLAG_CALL)
            .unwrap();
        unsafe {
            slot_set(frame, 0, EgclVal::from_fixnum(10));
            slot_set(frame, 1, EgclVal::from_fixnum(5));
        }
        let result = run_native_osr(&osr, osr.entries[&1], frame, None, &mut env);
        assert!(matches!(result, Ok(OsrOutcome::Finished(v)) if v == EgclVal::from_fixnum(25)));

        // Heat belongs to the compiled body, not an individual activation:
        // multiple short calls must accumulate toward a backedge sample.
        let counter = std::sync::atomic::AtomicU32::new(0);
        let emitted =
            super::emit_native(&body, true, u32::MAX, &counter as *const _ as u64, false).unwrap();
        let buffer = egcl_rt::jit::JitBuffer::new(&emitted.code).unwrap();
        let entry: unsafe extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(buffer.as_ptr().add(emitted.osr_entries[0].1)) };
        for expected in 1..=2 {
            unsafe {
                slot_set(frame, 0, EgclVal::from_fixnum(10));
                slot_set(frame, 1, EgclVal::from_fixnum(1));
                assert_eq!(entry(frame.add(1).cast()), EgclVal::from_fixnum(11).0);
            }
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::Relaxed),
                expected % t2_backedge_threshold()
            );
        }
        stack.pop_frame();
    }

    #[test]
    fn s390x_native_promotes_calls_branches_and_loops() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            form = reader::read_from_string(
                "((let ((i 0) (sum 0)) (block done (tagbody top
                (if (>= i 10) (return-from done (list sum i)))
                (setq sum (+ sum i)) (setq i (1+ i)) (go top)))))"
            )
            .unwrap()
            .0
        );
        let symbol = egcl_rt::symbols::intern("S390X-NATIVE-LOOP");
        let body = compile_function("S390X-NATIVE-LOOP", NIL, *form, &env, false, false).unwrap();
        registry_put(symbol, Arc::new(body));
        let native = try_promote_to_t1(symbol).expect("s390x must install native T1 code");
        let result = run_native(&native, symbol, &[], &mut env).unwrap();
        assert_eq!(
            list_to_vec(result),
            vec![EgclVal::from_fixnum(45), EgclVal::from_fixnum(10)]
        );
        let body = registry_get(symbol).unwrap();
        let osr = compile_osr_code(&body, symbol).expect("s390x must emit OSR entries");
        assert!(!osr.entries.is_empty());
        registry_remove(symbol);
    }
}
