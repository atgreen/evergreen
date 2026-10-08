// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! riscv64 baseline native compiler, sharing T0's GC-scanned activation slots.
//!
//! The shape is the s390x emitter's: every Lisp value lives in an activation
//! slot or on the operand stack that the collector already scans, and native
//! code only ever holds a value in a register between two instructions that
//! cannot allocate. Register use:
//!
//! - `s2` activation slots, `s3` next operand slot, `s4` the OSR stub's live
//!   tagbody id, `s5` a helper result retained across `c2i_transfer_pending`.
//!   All four are callee-saved raw addresses or small integers, never
//!   unrooted Lisp values.
//! - `a0`-`a3` carry the first four LP64 integer arguments; `a0` is the result.
//! - `t0`-`t3` are scratch; `t6` belongs to the assembler.

use super::*;
use egcl_rt::asm_riscv64::{
    Asm as RvAsm, Cond, Label as RvLabel,
    reg::{A0, A1, A2, A3, S2, S3, S4, S5, T0, T1, T2, T3, ZERO},
};

fn prologue(a: &mut RvAsm, locals: u16) {
    a.prologue();
    a.mov(S2, A0);
    a.address(S3, S2, 8 * i32::from(locals));
}

fn push(a: &mut RvAsm) {
    a.store(A0, S3, 0);
    a.add_imm(S3, S3, 8);
}

fn pop(a: &mut RvAsm, reg: u8) {
    a.add_imm(S3, S3, -8);
    a.load(reg, S3, 0);
}

fn call(a: &mut RvAsm, address: u64) {
    a.imm64(T0, address);
    a.call_reg(T0);
}

/// The helper's arguments are already rooted in activation slots (or rooted
/// by the helper before its first allocation). After it returns, check for a
/// pending transfer before executing any more Lisp instructions. The result
/// may live in s5 across this check because c2i_transfer_pending cannot GC.
fn helper(a: &mut RvAsm, address: u64, exit: RvLabel) {
    call(a, address);
    a.mov(S5, A0);
    call(a, c2i_transfer_pending as *const () as u64);
    a.mov(T0, A0);
    a.mov(A0, S5);
    a.branch(Cond::Ne, T0, ZERO, exit);
}

fn guard_fixnum(a: &mut RvAsm, reg: u8, deopt: RvLabel) {
    const { assert!(egcl_rt::value::TAG_MASK <= 2047, "tag mask must fit ANDI") };
    a.and_imm(T1, reg, egcl_rt::value::TAG_MASK as i32);
    a.branch(Cond::Ne, T1, ZERO, deopt);
}

/// `a0 = a0 + a1`, branching to `deopt` on signed overflow. RISC-V has no
/// flags: the sum overflowed exactly when both inputs disagree with it in
/// sign, i.e. `(sum ^ a) & (sum ^ b)` is negative.
fn add_overflow(a: &mut RvAsm, deopt: RvLabel) {
    a.add(T0, A0, A1);
    a.xor(T1, T0, A0);
    a.xor(T2, T0, A1);
    a.and(T1, T1, T2);
    a.mov(A0, T0);
    a.branch(Cond::Lt, T1, ZERO, deopt);
}

/// `a0 = a0 - a1`, branching to `deopt` on signed overflow: the difference
/// overflowed when the inputs differ in sign and the result's sign differs
/// from the minuend's, i.e. `(a ^ b) & (a ^ diff)` is negative.
fn sub_overflow(a: &mut RvAsm, deopt: RvLabel) {
    a.sub(T0, A0, A1);
    a.xor(T1, A0, A1);
    a.xor(T2, A0, T0);
    a.and(T1, T1, T2);
    a.mov(A0, T0);
    a.branch(Cond::Lt, T1, ZERO, deopt);
}

fn boolean(a: &mut RvAsm, cond: Cond, lhs: u8, rhs: u8) {
    let yes = a.label();
    let end = a.label();
    a.branch(cond, lhs, rhs, yes);
    a.imm64(A0, NIL.0);
    a.jump(end);
    a.bind(yes);
    a.imm64(A0, T.0);
    a.bind(end);
}

/// Peek/guard/commit keeps both operands and the operand pointer unchanged on
/// every guard edge. The shared deopt adapter can resume the exact bytecode.
fn arithmetic(
    a: &mut RvAsm,
    sym: u32,
    nargs: u16,
    bcp: u32,
    deopts: &mut std::collections::BTreeMap<u32, RvLabel>,
) -> bool {
    if nargs == 1 {
        if let Some(op) = inlinable_unary_fixnum_op(sym) {
            let deopt = *deopts.entry(bcp).or_insert_with(|| a.label());
            a.load(A0, S3, -8);
            guard_fixnum(a, A0, deopt);
            match op {
                UnaryFixnumOp::Incr => {
                    a.add_imm(A1, ZERO, 8);
                    add_overflow(a, deopt);
                }
                UnaryFixnumOp::Decr => {
                    a.add_imm(A1, ZERO, 8);
                    sub_overflow(a, deopt);
                }
                UnaryFixnumOp::Neg => {
                    a.mov(A1, A0);
                    a.mov(A0, ZERO);
                    sub_overflow(a, deopt);
                }
            }
            a.store(A0, S3, -8);
            return true;
        }
    }
    if nargs != 2 {
        return false;
    }
    let Some(op) = inlinable_fixnum_op(sym) else {
        return false;
    };
    // Multiplication still uses the numeric runtime until its full-width
    // overflow check (MULH against the sign of MUL) is implemented.
    if matches!(op, FixnumOp::Mul) {
        return false;
    }
    let deopt = *deopts.entry(bcp).or_insert_with(|| a.label());
    a.load(A0, S3, -16);
    a.load(A1, S3, -8);
    guard_fixnum(a, A0, deopt);
    guard_fixnum(a, A1, deopt);
    match op {
        FixnumOp::Add => add_overflow(a, deopt),
        FixnumOp::Sub => sub_overflow(a, deopt),
        _ => {
            let cond = match op {
                FixnumOp::Lt => Cond::Lt,
                FixnumOp::Gt => Cond::Gt,
                FixnumOp::Le => Cond::Le,
                FixnumOp::Ge => Cond::Ge,
                FixnumOp::NumEq => Cond::Eq,
                _ => unreachable!(),
            };
            boolean(a, cond, A0, A1);
        }
    }
    a.store(A0, S3, -16);
    a.add_imm(S3, S3, -8);
    true
}

pub(super) fn emit_native(
    bf: &BytecodeFunction,
    allow_speculation: bool,
    sym: u32,
    backedge_counter: u64,
    allow_traps: bool,
) -> Option<NativeEmission> {
    if bf.arity > bf.num_slots() {
        return None;
    }
    let values_sym = resolve_sym("VALUES")?.as_symbol_index();
    let mut a = RvAsm::new();
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
    let reset = |a: &mut RvAsm, depth: u16| {
        a.address(S3, S2, 8 * (i32::from(bf.n_locals) + i32::from(depth)));
    };
    let mut osr_headers = std::collections::BTreeSet::new();
    let mut can_osr_to_t2 = false;
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
                a.jump(target);
            }
            Instr::Const(k) => {
                // The registry-rooted constant slot is stable; its contents can
                // move during GC and must be reloaded on each execution.
                a.imm64(A0, bf.constants.get(k as usize)? as *const EgclVal as u64);
                a.load(A0, A0, 0);
                push(&mut a);
            }
            Instr::LoadLocal(i) => {
                a.load(A0, S2, 8 * i32::from(i));
                push(&mut a);
            }
            Instr::StoreLocal(i) => {
                pop(&mut a, A0);
                a.store(A0, S2, 8 * i32::from(i));
            }
            Instr::Pop => a.add_imm(S3, S3, -8),
            Instr::Dup => {
                a.load(A0, S3, -8);
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
                a.imm64(A0, u64::from(callee));
                a.imm64(A1, u64::from(nargs));
                a.address(A2, S3, -8 * i32::from(nargs));
                let profile = if registry_get(callee).is_some() {
                    call_site_profile_token(bf as *const BytecodeFunction as usize, bcp)
                } else {
                    0
                };
                a.imm64(A3, profile);
                helper(&mut a, c2i_call_slice as *const () as u64, exit);
                a.address(S3, S3, -8 * i32::from(nargs));
                push(&mut a);
            }
            Instr::Br(target) => a.jump(*labels.get(target as usize)?),
            Instr::BrIfFalse(target) | Instr::BrIfTrue(target) => {
                pop(&mut a, A0);
                a.imm64(T0, NIL.0);
                a.branch(
                    if matches!(instr, Instr::BrIfFalse(_)) {
                        Cond::Eq
                    } else {
                        Cond::Ne
                    },
                    A0,
                    T0,
                    *labels.get(target as usize)?,
                );
            }
            Instr::PushBlock { .. } | Instr::PushTag { .. } | Instr::PopHandler => {}
            Instr::Go {
                tagbody_id,
                target_bcp,
            } => {
                if sym == u32::MAX {
                    // OSR stubs identify the live tagbody in s4. A GO to a
                    // different tagbody must unwind through T0's handlers.
                    let target = *deopts.entry(bcp).or_insert_with(|| a.label());
                    a.imm64(T0, u64::from(tagbody_id));
                    a.branch(Cond::Ne, S4, T0, target);
                }
                let depth = *tags.get(&tagbody_id)?;
                reset(&mut a, depth);
                if target_bcp < bcp && depth == 0 {
                    osr_headers.insert(target_bcp);
                    if backedge_counter != 0 {
                        let keep = a.label();
                        // Match x86's per-body counter: short invocations
                        // contribute heat too. The counter is an AtomicU32, so
                        // use explicit 32-bit accesses.
                        a.imm64(T3, backedge_counter);
                        a.load_u32(A0, T3, 0);
                        a.add_imm(A0, A0, 1);
                        a.store_u32(A0, T3, 0);
                        a.imm64(A1, u64::from(t2_backedge_threshold()));
                        a.branch(Cond::Lt, A0, A1, keep);
                        a.store_u32(ZERO, T3, 0);
                        if sym == u32::MAX {
                            helper(&mut a, c2i_osr_backedge as *const () as u64, exit);
                        } else {
                            can_osr_to_t2 = true;
                            a.imm64(A0, u64::from(sym));
                            a.imm64(A1, u64::from(target_bcp));
                            a.mov(A2, S2);
                            a.imm64(A3, bf as *const BytecodeFunction as u64);
                            helper(&mut a, c2i_t1_backedge as *const () as u64, exit);
                        }
                        a.branch(Cond::Eq, A0, ZERO, keep);
                        a.load(A0, S2, 8 * i32::from(bf.n_locals));
                        a.jump(exit);
                        a.bind(keep);
                    }
                }
                a.jump(*labels.get(target_bcp as usize)?);
            }
            Instr::ReturnFrom { block_id } => {
                let &(target, depth) = blocks.get(&block_id)?;
                a.load(A0, S3, -8);
                reset(&mut a, depth);
                push(&mut a);
                a.jump(*labels.get(target as usize)?);
            }
            Instr::Return => {
                pop(&mut a, A0);
                a.jump(exit);
            }
            Instr::ClearMv => helper(&mut a, c2i_clear_mv as *const () as u64, exit),
            Instr::LoadGlobal(s) | Instr::LoadFunction(s) => {
                a.imm64(A0, u64::from(s));
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
                pop(&mut a, A1);
                a.imm64(A0, u64::from(s));
                helper(&mut a, c2i_store_global as *const () as u64, exit);
            }
            Instr::LoadEnvVar(i) => {
                a.imm64(A0, bf as *const BytecodeFunction as u64);
                a.imm64(A1, u64::from(i));
                helper(&mut a, c2i_load_env as *const () as u64, exit);
                push(&mut a);
            }
            Instr::StoreEnvVar(i) | Instr::DefineEnvVar(i) => {
                pop(&mut a, A2);
                a.imm64(A0, bf as *const BytecodeFunction as u64);
                a.imm64(A1, u64::from(i));
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
                pop(&mut a, A1);
                pop(&mut a, A0);
                helper(&mut a, c2i_alloc_cons as *const () as u64, exit);
                push(&mut a);
            }
            Instr::EvalHost(k) | Instr::MakeClosureEnv(k) => {
                a.imm64(A0, bf.constants.get(k as usize)? as *const EgclVal as u64);
                a.load(A0, A0, 0);
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
                pop(&mut a, A0);
                a.address(A1, S2, 8 * i32::from(slot_base));
                a.imm64(A2, u64::from(nvars));
                helper(&mut a, c2i_take_values as *const () as u64, exit);
            }
            Instr::ValuesToList => {
                pop(&mut a, A0);
                helper(&mut a, c2i_values_to_list as *const () as u64, exit);
                push(&mut a);
            }
            Instr::TypeP(class) => {
                pop(&mut a, A0);
                a.imm64(A1, u64::from(class));
                helper(&mut a, c2i_typep_class as *const () as u64, exit);
                push(&mut a);
            }
            _ if allow_traps => {
                let target = *deopts.entry(bcp).or_insert_with(|| a.label());
                a.jump(target);
            }
            _ => return None,
        }
    }
    a.bind(exit);
    a.epilogue();
    for (&bcp, &label) in &deopts {
        a.bind(label);
        // a1 = operand depth = ((s3 - s2) >> 3) - n_locals; a0 = bcp.
        a.sub(A1, S3, S2);
        a.shift_right_signed(A1, A1, 3);
        a.imm64(T0, u64::from(bf.n_locals));
        a.sub(A1, A1, T0);
        a.imm64(A0, u64::from(bcp));
        call(&mut a, c2i_deopt_state as *const () as u64);
        a.jump(exit);
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
        a.imm64(S4, u64::from(tagbody));
        a.jump(*labels.get(header as usize)?);
    }
    let bcp_offsets = labels
        .iter()
        .map(|&label| a.label_offset(label).unwrap() as u32)
        .collect();
    Some(NativeEmission {
        code: a.finish()?,
        osr_entries,
        bcp_offsets,
        has_deopt: !deopts.is_empty() || can_osr_to_t2,
        direct_calls: Vec::new(),
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
            try_promote_to_t1(symbol).expect("must install riscv64 T1"),
        )
    }

    #[test]
    fn riscv64_native_deopt_preserves_side_effects_and_operands() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let count = egcl_rt::symbols::intern("RISCV64-DEOPT-COUNT");
        egcl_rt::symbols::set_symbol_value(count, EgclVal::from_fixnum(0));
        let (symbol, native) = install(
            "RISCV64-DEOPT",
            "(
            (setq riscv64-deopt-count (1+ riscv64-deopt-count))
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
    fn riscv64_native_subtraction_and_negation_deopt_at_the_fixnum_boundary() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, native) = install(
            "RISCV64-SUB-NEG",
            "(
            (list (- -1152921504606846976 1) (- -1152921504606846976) (1- 5) (- 7 10)))",
            &env,
        );
        let value = run_native(&native, symbol, &[], &mut env).unwrap();
        assert_eq!(
            crate::cli::format_val(value),
            "(-1152921504606846977 1152921504606846976 4 -3)"
        );
        registry_remove(symbol);
    }

    #[test]
    fn riscv64_native_comparisons_match_the_interpreter() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, native) = install(
            "RISCV64-COMPARE",
            "(
            (list (< 1 2) (< 2 1) (> 3 -4) (> -4 3) (<= 5 5) (<= 6 5)
                  (>= 5 5) (>= 4 5) (= 9 9) (= 9 8)))",
            &env,
        );
        let value = run_native(&native, symbol, &[], &mut env).unwrap();
        assert_eq!(
            crate::cli::format_val(value),
            "(T NIL T NIL T NIL T NIL T NIL)"
        );
        registry_remove(symbol);
    }

    #[test]
    fn riscv64_native_error_stops_before_later_side_effects() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let count = egcl_rt::symbols::intern("RISCV64-ERROR-COUNT");
        egcl_rt::symbols::set_symbol_value(count, EgclVal::from_fixnum(0));
        let (symbol, native) = install(
            "RISCV64-ERROR",
            "(
            (car 17) (setq riscv64-error-count 99))",
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
    fn riscv64_native_allocation_and_multiple_values_preserve_live_roots() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, native) = install(
            "RISCV64-ROOTS",
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
    fn riscv64_native_osr_enters_live_frame_without_reinitializing_locals() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
        let mut body = compile_function("RISCV64-OSR", NIL, *form, &env, false, false).unwrap();
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
    fn riscv64_native_promotes_calls_branches_and_loops() {
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
        let symbol = egcl_rt::symbols::intern("RISCV64-NATIVE-LOOP");
        let body = compile_function("RISCV64-NATIVE-LOOP", NIL, *form, &env, false, false).unwrap();
        registry_put(symbol, Arc::new(body));
        let native = try_promote_to_t1(symbol).expect("riscv64 must install native T1 code");
        let result = run_native(&native, symbol, &[], &mut env).unwrap();
        assert_eq!(
            list_to_vec(result),
            vec![EgclVal::from_fixnum(45), EgclVal::from_fixnum(10)]
        );
        let body = registry_get(symbol).unwrap();
        let osr = compile_osr_code(&body, symbol).expect("riscv64 must emit OSR entries");
        assert!(!osr.entries.is_empty());
        registry_remove(symbol);
    }
}
