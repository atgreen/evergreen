//! T1 baseline code generation for AArch64.
//!
//! This is the sibling of `emit_native_x86`, not a rewrite of it. The two share
//! everything that is not instruction selection: the entry ABI
//! (`extern "C" fn(*mut u64, *const u8) -> u64`, the frame-slots pointer and the
//! `TorclStack`), the `NativeEmission` output, `install_stack_map`,
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
//! | `TorclStack`                           | r12    | x21     |
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
    BytecodeFunction, DIRECT_CALL_GEN, NativeEmission, c2i_call_builtin, c2i_call_slice,
    c2i_t1_backedge, c2i_transfer_pending, call_site_profile_token, registry_get,
    t2_backedge_threshold,
};
use torcl_rt::asm::a64;
use torcl_rt::asm::{Asm, Cc};
use torcl_rt::bytecode::Instr;
use torcl_rt::value::TorclVal;

/// Frame slots; local `i` lives at `[SLOTS + 8*i]`.
const SLOTS: a64::Reg = 19;
/// Operand-stack pointer, growing up from `SLOTS + 8*n_locals`.
const OPSP: a64::Reg = 20;
/// The `TorclStack` this activation belongs to.
const STACK: a64::Reg = 21;
/// The accumulator, and the ABI's return register.
const ACC: a64::Reg = 0;
/// IP0/IP1: the architecture reserves these for exactly this use, so no call
/// they straddle can expect them preserved.
const SCRATCH: a64::Reg = 16;
const SCRATCH2: a64::Reg = 17;

/// Bytes the prologue claims: the frame record plus the three activation
/// registers, rounded to the 16-byte stack alignment AAPCS64 requires at a call.
const FRAME_BYTES: i32 = 48;

/// Compile `bf` to native AArch64 T1 code, or `None` if it uses an opcode this
/// baseline does not handle.
pub(super) fn emit_native_a64(
    bf: &BytecodeFunction,
    _allow_speculation: bool,
    sym: u32,
    backedge_counter: u64,
    _allow_traps: bool,
) -> Option<NativeEmission> {
    macro_rules! decline {
        ($($reason:tt)*) => {{
            torcl_rt::blog!("compile", torcl_rt::log::TRACE,
                "[T1/a64] {}: declined: {}", bf.name, format_args!($($reason)*));
            return None;
        }};
    }

    // OSR compilation asks for alternate entry stubs at loop headers, which this
    // slice does not emit (bliss-cerlf). Declining leaves the loop at T0 rather
    // than installing code whose osr_entries promise entries that do not exist.
    if sym == u32::MAX {
        decline!("OSR entry stubs are not implemented for AArch64 yet");
    }
    if bf.arity > bf.num_slots() {
        decline!(
            "required arity {} exceeds activation slots {}",
            bf.arity,
            bf.num_slots()
        );
    }
    let n_locals = bf.n_locals as i32;

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

    let c2i_addr =
        c2i_call_slice as extern "C" fn(u64, u64, *const TorclVal, u64) -> u64 as usize as u64;
    let builtin_addr =
        c2i_call_builtin as extern "C" fn(u64, u64, *const TorclVal, u64) -> u64 as usize as u64;
    let transfer_addr = c2i_transfer_pending as extern "C" fn() -> u64 as usize as u64;
    let t2_backedge_addr = c2i_t1_backedge
        as extern "C" fn(u64, u64, *mut u64, *const BytecodeFunction) -> u64
        as usize as u64;

    let mut c = Asm::new();
    let bcp_labels: Vec<_> = bf.code.iter().map(|_| c.label()).collect();

    // ── Prologue ───────────────────────────────────────────────
    // x0 = frame slots, x1 = TorclStack. x29 records the frame base so the
    // epilogue restores SP from it rather than trusting the body to have left SP
    // balanced.
    c.word(a64::stp_pre(a64::FP, a64::LR, a64::SP, -FRAME_BYTES)?);
    c.word(a64::mov(a64::FP, a64::SP));
    c.word(a64::stp(SLOTS, OPSP, a64::SP, 16)?);
    c.word(a64::str_imm(STACK, a64::SP, 32)?);
    c.word(a64::mov(SLOTS, 0));
    c.word(a64::mov(STACK, 1));
    emit_add_disp(&mut c, OPSP, SLOTS, 8 * n_locals)?;

    for (bcp_idx, instr) in bf.code.iter().enumerate() {
        c.bind(bcp_labels[bcp_idx]);
        let bcp = bcp_idx as u32;
        match instr {
            Instr::Const(k) => {
                let val = bf.constants[*k as usize];
                if torcl_rt::gc::is_heap_ref(val) {
                    // A movable heap constant must never be baked in as an
                    // immediate: the moving minor GC rewrites the registry-rooted
                    // constants Vec in place and cannot patch machine code, so
                    // the immediate would go stale and native code would push a
                    // freed-nursery pointer (bliss-d0b). Load through the slot,
                    // whose address is stable for the life of the registry Rc
                    // that this code is keyed to.
                    let slot = &bf.constants[*k as usize] as *const TorclVal;
                    emit_mov_imm(&mut c, ACC, slot as u64);
                    c.word(a64::ldr_imm(ACC, ACC, 0)?);
                } else {
                    emit_mov_imm(&mut c, ACC, val.0);
                }
                emit_push(&mut c);
            }
            Instr::LoadLocal(i) => {
                c.word(a64::ldr_imm(ACC, SLOTS, 8 * u64::from(*i)).or_else(|| {
                    None // a frame deeper than the scaled load reaches
                })?);
                emit_push(&mut c);
            }
            Instr::StoreLocal(i) => {
                emit_pop(&mut c, ACC);
                c.word(a64::str_imm(ACC, SLOTS, 8 * u64::from(*i))?);
            }
            Instr::Pop => {
                c.word(a64::sub_imm(OPSP, OPSP, 8)?);
            }
            Instr::Dup => {
                c.word(a64::ldur(ACC, OPSP, -8)?);
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
                emit_c2i_call(&mut c, target, transfer_addr)?;
                // Drop the arguments and push the result.
                c.word(a64::sub_imm(OPSP, OPSP, 8 * u64::from(*nargs))?);
                emit_push(&mut c);
            }
            Instr::Br(target) => {
                c.jmp(*bcp_labels.get(*target as usize)?);
            }
            Instr::BrIfFalse(target) => {
                emit_pop(&mut c, ACC);
                emit_cmp_imm(&mut c, ACC, torcl_rt::value::NIL_BITS);
                c.jcc(Cc::E, *bcp_labels.get(*target as usize)?);
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
                c.word(a64::ldur(ACC, OPSP, -8)?);
                emit_add_disp(&mut c, OPSP, SLOTS, 8 * (n_locals + i32::from(sp)))?;
                emit_push(&mut c);
                c.jmp(*bcp_labels.get(resume_bcp as usize)?);
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
                    c.word(a64::ldr_w_imm(SCRATCH2, SCRATCH, 0)?);
                    c.word(a64::add_imm_w(SCRATCH2, SCRATCH2, 1)?);
                    c.word(a64::str_w_imm(SCRATCH2, SCRATCH, 0)?);
                    c.word(a64::cmp_imm_w(
                        SCRATCH2,
                        u64::from(t2_backedge_threshold()),
                    )?);
                    c.jcc(Cc::L, keep);
                    c.word(a64::str_w_imm(a64::XZR, SCRATCH, 0)?); // reset the sample
                    emit_mov_imm(&mut c, 0, u64::from(sym));
                    emit_mov_imm(&mut c, 1, u64::from(*target_bcp));
                    c.word(a64::mov(2, SLOTS));
                    emit_mov_imm(&mut c, 3, std::ptr::from_ref(bf) as u64);
                    emit_c2i_call(&mut c, t2_backedge_addr, transfer_addr)?;
                    emit_cmp_imm(&mut c, ACC, 0);
                    c.jcc(Cc::E, keep);
                    // Leaving the loop: T2 finished, or a signal is pending. The
                    // shared epilogue returns the first operand slot; the Rust
                    // caller re-raises any stashed error before using it.
                    c.word(a64::ldr_imm(ACC, SLOTS, 8 * n_locals as u64)?);
                    emit_epilogue(&mut c);
                    c.bind(keep);
                }
                c.jmp(*bcp_labels.get(*target_bcp as usize)?);
            }
            other => decline!("unsupported opcode {other:?}"),
        }
    }

    // A bytecode function always ends in Return, but a trailing fallthrough must
    // not run off the end of the buffer into whatever follows.
    c.word(a64::mov(ACC, a64::XZR));
    emit_epilogue(&mut c);

    Some(NativeEmission {
        code: c.finish()?,
        osr_entries: Vec::new(),
        bcp_offsets: Vec::new(),
        has_deopt: false,
        direct_calls: Vec::new(),
    })
}

/// `str ACC, [OPSP], #8` — store and post-increment, i.e. push the accumulator.
fn emit_push(c: &mut Asm) {
    // `str_pre`'s sibling: the post-index form adjusts after the access, which is
    // exactly a push onto an upward-growing stack.
    c.word(0xF800_8400 | ((8u32 & 0x1ff) << 12) | ((OPSP as u32) << 5) | ACC as u32);
}

/// Pop into `d`: retreat the stack top, then load from it.
fn emit_pop(c: &mut Asm, d: a64::Reg) {
    c.word(0xF800_8C00 | ((-8i32 as u32 & 0x1ff) << 12) | ((OPSP as u32) << 5) | d as u32);
}

fn emit_epilogue(c: &mut Asm) {
    c.word(a64::mov(a64::SP, a64::FP));
    c.word(a64::ldr_imm(STACK, a64::SP, 32).expect("fixed frame offset"));
    c.word(a64::ldp(SLOTS, OPSP, a64::SP, 16).expect("fixed frame offset"));
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
fn emit_c2i_call(c: &mut Asm, target: u64, transfer_addr: u64) -> Option<()> {
    emit_mov_imm(c, SCRATCH, target);
    c.word(a64::blr(SCRATCH));

    // Stop T1 at a call that initiated an error, THROW or RETURN-FROM. The c2i
    // helper stashes the condition and returns NIL, so without this check native
    // execution would continue into code that must not run — a store after the
    // error lands, and a later error superseding the real one.
    let resume = c.label();
    c.word(a64::str_pre(ACC, a64::SP, -16)?); // keep SP 16-aligned
    emit_mov_imm(c, SCRATCH, transfer_addr);
    c.word(a64::blr(SCRATCH));
    c.word(a64::cmp_imm(ACC, 0)?);
    // Neither the load nor the stack adjustment disturbs the flags, so the
    // comparison above still decides the branch below.
    c.word(a64::ldr_post(ACC, a64::SP, 16)?);
    c.jcc(Cc::E, resume);
    emit_epilogue(c);
    c.bind(resume);
    Some(())
}
