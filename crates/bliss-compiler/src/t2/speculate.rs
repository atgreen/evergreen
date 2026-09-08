//! T2 speculative type lowering — profile-guided, single-type (spec §4.5, §4.10).
//!
//! Given static operand proofs (declarations/inference) or the operand-type
//! profile gathered by the interpreter, rewrite a generic arithmetic `Call`
//! into a typed op — `FixnumMul` for fixnums, `FloatMul` for single-floats, and
//! so on. Static proof takes precedence over profile. The typed op is
//! **guard-flagged** and keeps the `Call`'s
//! `FrameState`, so a wrong-type operand (or a fixnum overflow) deopts to the
//! interpreter at the site's `bcp`. Crucially it emits **one** path — no fallback
//! for the other type. A site that is polymorphic or cold (profile `None`) is
//! left as the generic `Call`. If the speculation proves wrong at runtime it
//! deopts, the profiler observes the new type, and a later recompile commits to
//! *that* type instead — never both at once.

use crate::t2::frame_state::FrameStateId;
use crate::t2::ir::{AuxData, Function, IRType, Inst, InstFlags, Opcode, TypeBits, Value};

/// The single type a call site may be speculated as (mutually exclusive).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum SpecType {
    Fixnum,
    SingleFloat,
}

#[derive(Copy, Clone)]
enum Arith {
    Add,
    Sub,
    Mul,
}

#[derive(Copy, Clone)]
enum Cmp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
}

/// The arithmetic op a callee symbol names, if it is one we speculate.
fn arith_of(sym: u32) -> Option<Arith> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("+") => Some(Arith::Add),
        Some("-") => Some(Arith::Sub),
        Some("*") => Some(Arith::Mul),
        _ => None,
    }
}

#[derive(Copy, Clone)]
enum Bitwise {
    And,
    Or,
    Xor,
    Not,
}

/// A bitwise op we speculate (fixnum only). These are exact on the tagged
/// representation — `tagged(a) OP tagged(b) = (a OP b)<<3` — so no overflow and no
/// untag/retag. Crypto/compression/hashing are dominated by these.
fn bitwise_of(sym: u32) -> Option<Bitwise> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("LOGAND") => Some(Bitwise::And),
        Some("LOGIOR") => Some(Bitwise::Or),
        Some("LOGXOR") => Some(Bitwise::Xor),
        Some("LOGNOT") => Some(Bitwise::Not),
        _ => None,
    }
}

fn bitwise_opcode(b: Bitwise) -> Opcode {
    match b {
        Bitwise::And => Opcode::LogAnd,
        Bitwise::Or => Opcode::LogOr,
        Bitwise::Xor => Opcode::LogXor,
        Bitwise::Not => Opcode::LogNot,
    }
}

/// The comparison a callee symbol names, if it is one we speculate. These feed
/// `Brif`, so speculating them (guarded fixnum/float `Cmp`) is what lets a branch
/// on `(< x n)` reach T2 as a `cmp`+`jcc` instead of a generic call.
fn cmp_of(sym: u32) -> Option<Cmp> {
    match bliss_rt::symbols::symbol_name(sym).as_deref() {
        Some("<") => Some(Cmp::Lt),
        Some(">") => Some(Cmp::Gt),
        Some("<=") => Some(Cmp::Le),
        Some(">=") => Some(Cmp::Ge),
        Some("=") => Some(Cmp::Eq),
        _ => None,
    }
}

/// The typed comparison opcode for `(kind, speculated-type)`. Fixnum supports all
/// five; single-float has only Eq/Lt encoders, so Gt/Le/Ge on floats return
/// `None` (left as a generic call) for now.
fn typed_cmp_opcode(c: Cmp, s: SpecType) -> Option<Opcode> {
    use Opcode::*;
    Some(match (c, s) {
        (Cmp::Lt, SpecType::Fixnum) => FixnumCmpLt,
        (Cmp::Gt, SpecType::Fixnum) => FixnumCmpGt,
        (Cmp::Le, SpecType::Fixnum) => FixnumCmpLe,
        (Cmp::Ge, SpecType::Fixnum) => FixnumCmpGe,
        (Cmp::Eq, SpecType::Fixnum) => FixnumCmpEq,
        (Cmp::Lt, SpecType::SingleFloat) => FloatCmpLt,
        (Cmp::Eq, SpecType::SingleFloat) => FloatCmpEq,
        (_, SpecType::SingleFloat) => return None,
    })
}

/// The typed opcode a `(kind, speculated-type)` lowers to.
fn typed_opcode(a: Arith, s: SpecType) -> Opcode {
    use Opcode::*;
    match (a, s) {
        (Arith::Add, SpecType::Fixnum) => FixnumAdd,
        (Arith::Sub, SpecType::Fixnum) => FixnumSub,
        (Arith::Mul, SpecType::Fixnum) => FixnumMul,
        (Arith::Add, SpecType::SingleFloat) => FloatAdd,
        (Arith::Sub, SpecType::SingleFloat) => FloatSub,
        (Arith::Mul, SpecType::SingleFloat) => FloatMul,
    }
}

fn result_type(s: SpecType) -> IRType {
    IRType::of(match s {
        SpecType::Fixnum => TypeBits::FIXNUM,
        SpecType::SingleFloat => TypeBits::SINGLE_FLOAT,
    })
}

/// T or NIL — a comparison's result. Both are non-pointer immediates (NIL/T are
/// tag-111 specials), so typing the result this way lets the emitter's
/// safepoint-root filter drop a live-across boolean from GC shadow sync
/// (bliss-x5y.25 option b).
fn boolean_type() -> IRType {
    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL))
}

fn frame_state_bcp(f: &Function, fs: FrameStateId) -> u32 {
    f.frame_states
        .get(fs)
        .scopes
        .last()
        .map(|s| s.bcp)
        .unwrap_or(0)
}

/// Resolve a numeric specialization entirely from SSA types. This is the path
/// used by declared parameters: no sampled type is needed, and a stale profile
/// can never contradict the source assertion. A fixnum constant participates
/// in a single-float operation through CL float contagion.
fn statically_proven_spec_type(f: &Function, args: &[crate::t2::ir::Value]) -> Option<SpecType> {
    let subset =
        |bits: TypeBits, allowed: TypeBits| !bits.is_bottom() && bits.meet(allowed) == bits;
    if !args.is_empty()
        && args
            .iter()
            .all(|&arg| subset(f.value(arg).ty.bits, TypeBits::FIXNUM))
    {
        return Some(SpecType::Fixnum);
    }

    let numeric = TypeBits::FIXNUM.join(TypeBits::SINGLE_FLOAT);
    let all_single_numeric = !args.is_empty()
        && args
            .iter()
            .all(|&arg| subset(f.value(arg).ty.bits, numeric));
    let has_single = args
        .iter()
        .any(|&arg| subset(f.value(arg).ty.bits, TypeBits::SINGLE_FLOAT));
    (all_single_numeric && has_single).then_some(SpecType::SingleFloat)
}

/// Rewrite generic arithmetic `Call`s into single guarded typed ops guided by
/// static SSA proof first, then `profile` (site `bcp` → the one speculated type,
/// or `None` to leave generic).
/// Returns the number of sites speculated.
pub fn speculate(f: &mut Function, profile: &impl Fn(u32) -> Option<SpecType>) -> usize {
    // Read phase: collect the calls to rewrite (keeps the borrow off `f` for the
    // mutation phase).
    let mut work: Vec<(Inst, Opcode, IRType)> = Vec::new();
    for &b in f.block_order() {
        for &inst in &f.block(b).insts {
            let data = f.inst(inst);
            if data.opcode != Opcode::Call {
                continue;
            }
            let sym = match &data.aux {
                AuxData::CallTarget(s) => *s,
                _ => continue,
            };
            // The site's bcp is on the Call's FrameState (P1 anchors it there).
            let Some(fs) = data.frame_state else { continue };
            let bcp = frame_state_bcp(f, fs);
            let Some(spec) = statically_proven_spec_type(f, &data.args).or_else(|| profile(bcp))
            else {
                continue;
            };
            let argc = f.inst(inst).args.len();
            if let Some(arith) = arith_of(sym) {
                match (arith, argc) {
                    // Binary arithmetic: the typed op's result is the numeric type.
                    (_, 2) => work.push((inst, typed_opcode(arith, spec), result_type(spec))),
                    // Unary minus → negation (fixnum only; no FloatNeg opcode yet).
                    (Arith::Sub, 1) if spec == SpecType::Fixnum => {
                        work.push((inst, Opcode::FixnumNeg, result_type(spec)))
                    }
                    // 1-arg +/* are identity; variadic (>2) forms: leave generic.
                    _ => {}
                }
            } else if let Some(cmp) = cmp_of(sym) {
                // Comparison: guarded typed compare; the result is a boolean
                // (T/NIL) — both immediates, typed as SYMBOL|NULL.
                if argc == 2 {
                    if let Some(op) = typed_cmp_opcode(cmp, spec) {
                        work.push((inst, op, boolean_type()));
                    }
                }
            } else if let Some(bit) = bitwise_of(sym) {
                // Bitwise ops are fixnum-only. Not is unary; And/Or/Xor are binary.
                let want = if matches!(bit, Bitwise::Not) { 1 } else { 2 };
                if spec == SpecType::Fixnum && argc == want {
                    work.push((inst, bitwise_opcode(bit), result_type(SpecType::Fixnum)));
                }
            } else if bliss_rt::symbols::symbol_name(sym).as_deref() == Some("ASH") {
                // Arithmetic shift by a (constant, checked at emit) amount: left is a
                // multiply by 2^n (overflow-checked), right is an untag/sar/retag.
                if spec == SpecType::Fixnum && argc == 2 {
                    work.push((inst, Opcode::FixnumShl, result_type(SpecType::Fixnum)));
                }
            }
        }
    }

    // Mutation phase: turn each into a guarded typed op.
    let n = work.len();
    let mut fixnum_sites: Vec<Inst> = Vec::new();
    for (inst, opcode, ty) in work {
        let results = f.inst(inst).results.clone();
        {
            let data = f.inst_mut(inst);
            data.opcode = opcode;
            // No longer a generic call/safepoint — now a deopt point: guard-flagged
            // and ordered (effectful), keeping its FrameState so a wrong type or
            // overflow resumes the interpreter. `aux`/`frame_state` are retained.
            data.flags = InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            };
        }
        for r in results {
            f.refine_type(r, ty);
        }
        if fixnum_family(opcode) {
            fixnum_sites.push(inst);
        }
    }
    materialize_fixnum_operand_guards(f, &fixnum_sites);
    n
}

/// The speculated opcodes whose emitter guards every variable operand as a
/// fixnum before executing (emit_arith_inst / emit_fixnum_cmp_value).
fn fixnum_family(op: Opcode) -> bool {
    use Opcode::*;
    matches!(
        op,
        FixnumAdd
            | FixnumSub
            | FixnumMul
            | FixnumNeg
            | FixnumShl
            | LogAnd
            | LogOr
            | LogXor
            | LogNot
            | FixnumCmpLt
            | FixnumCmpGt
            | FixnumCmpLe
            | FixnumCmpGe
            | FixnumCmpEq
    )
}

/// bliss-x5y.25 (option b): materialise each speculated fixnum site's
/// discharged operand proof as an explicit SSA `Guard` value — the same shape
/// build.rs gives CAR/CDR's cons proof — and rewrite every use the guard
/// dominates (instruction arguments, edge arguments, and FrameState sources)
/// to the narrowed value.
///
/// Why: inference assigns each SSA value one type from its definition on, so a
/// raw parameter stays TOP everywhere even though the emitter guards it before
/// the op. With the proof reified as a FIXNUM-typed value that replaces the
/// original downstream, the original dies at the guard and what stays live
/// across later call safepoints is provably immediate — the emitter's
/// safepoint-root filter drops it from GC shadow sync, and a function whose
/// roots all vanish loses its frame_base_home, unlocking the direct self-call
/// fast path (emit_call).
///
/// Soundness of the rewrite: the guard passes its input through unchanged, so
/// substituting its result anywhere it dominates preserves values exactly. The
/// guard reuses the site's FrameState (deopt before the op re-runs the op's
/// bytecode in T0 — nothing has executed yet). The site's own FrameState is
/// NOT rewritten: at a guard/op deopt the guard's result does not exist, so
/// those sources must stay the original value.
fn materialize_fixnum_operand_guards(f: &mut Function, sites: &[Inst]) {
    use crate::t2::ir::{InstData, InstFlags, ValueRepresentation};
    if sites.is_empty() {
        return;
    }
    let dom = f.dominators();
    for &site in sites {
        // Locate the site (positions shift as guards are inserted, so look it
        // up fresh each round; T2 bodies are small).
        let Some((block, _)) = f
            .block_order()
            .to_vec()
            .into_iter()
            .find_map(|b| {
                f.block(b)
                    .insts
                    .iter()
                    .position(|&i| i == site)
                    .map(|p| (b, p))
            })
        else {
            continue;
        };
        let fs = f.inst(site).frame_state;
        let source_pos = f.inst(site).source_pos;
        let args = f.inst(site).args.clone();
        let mut seen: Vec<Value> = Vec::new();
        for v in args {
            if seen.contains(&v) {
                continue;
            }
            seen.push(v);
            // Already provably fixnum (constant, arith result, earlier guard):
            // the emitter will not re-check it and there is nothing to narrow.
            let bits = f.value(v).ty.bits;
            if !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits {
                continue;
            }
            // EAGER placement for a call result (the Phase-3 half of option b):
            // a result consumed by a speculated fixnum op would normally be
            // guarded just before that op — too late to help any call
            // safepoint in between (fib's r1 stays a GC root across the second
            // recursive call). Instead anchor the guard right after the
            // defining call, at the first following FrameState-carrying
            // instruction in the same block: that state is exactly the
            // post-call interpreter state (the result on the stack), so a
            // guard failure resumes T0 there and simply re-runs the remainder
            // generically. Falls back to just-before-the-site when no such
            // anchor exists.
            let eager = match f.value(v).def {
                crate::t2::ir::ValueDef::Result { inst: def, .. }
                    if f.inst(def).opcode == Opcode::Call =>
                {
                    f.block_order().to_vec().into_iter().find_map(|db| {
                        let insts = &f.block(db).insts;
                        let dp = insts.iter().position(|&i| i == def)?;
                        insts[dp + 1..]
                            .iter()
                            .take_while(|&&i| i != site)
                            .find(|&&i| f.inst(i).frame_state.is_some())
                            .map(|&anchor| (db, anchor))
                    })
                }
                _ => None,
            };
            let (guard_block, anchor, borrowed_fs, rewrite_anchor_args) = match eager {
                Some((db, anchor)) => (db, anchor, f.inst(anchor).frame_state, false),
                None => (block, site, fs, true),
            };
            // The guard gets its own CLONE of the borrowed FrameState. Sharing
            // the id would let a later sibling guard's downstream rewrite
            // reach back into this guard's deopt state and reference a value
            // defined after it (regalloc2 EntryLivein); a private copy pins
            // the state as it exists at the guard.
            let guard_fs =
                borrowed_fs.map(|id| f.frame_states.add(f.frame_states.get(id).clone()));
            let (guard, results) = f.push_inst(
                guard_block,
                InstData {
                    opcode: Opcode::Guard,
                    args: vec![v],
                    results: vec![],
                    aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: guard_fs,
                    source_pos,
                },
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            );
            let narrowed = results[0];
            // push_inst appended the guard at the end of the block (after the
            // terminator); move it next to its anchor: immediately BEFORE a
            // site anchor (the proof precedes the op), immediately AFTER an
            // eager anchor (whose FrameState the guard borrows).
            let insts = &mut f.block_mut(guard_block).insts;
            let appended = insts.pop();
            debug_assert_eq!(appended, Some(guard));
            let anchor_pos = insts
                .iter()
                .position(|&i| i == anchor)
                .expect("anchor is in its block");
            insts.insert(
                if rewrite_anchor_args {
                    anchor_pos
                } else {
                    anchor_pos + 1
                },
                guard,
            );
            rewrite_dominated_uses(
                f,
                &dom,
                guard_block,
                anchor,
                guard,
                v,
                narrowed,
                rewrite_anchor_args,
            );
        }
    }
}

/// Replace uses of `from` with `to` in everything the guard placed at `anchor`
/// dominates: every instruction after the anchor in `block` (arguments, edge
/// arguments, FrameState sources) and every instruction of every block
/// strictly dominated by `block`. With `rewrite_anchor_args` (the guard sits
/// immediately BEFORE the anchor — the speculated-site placement) the anchor's
/// own arguments are rewritten too, while its FrameState keeps `from`: at a
/// deopt of the guard or the site the guard's result does not exist yet. An
/// eager anchor (guard AFTER it, borrowing its FrameState) is skipped whole
/// for the same reason.
#[allow(clippy::too_many_arguments)]
fn rewrite_dominated_uses(
    f: &mut Function,
    dom: &crate::t2::ir::DominatorTree,
    block: crate::t2::ir::Block,
    site: Inst,
    guard: Inst,
    from: Value,
    to: Value,
    rewrite_anchor_args: bool,
) {
    let rewrite_frame_state = |f: &mut Function, inst: Inst| {
        let Some(fsid) = f.inst(inst).frame_state else {
            return;
        };
        let fs = f.frame_states.get_mut(fsid);
        for scope in &mut fs.scopes {
            for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                if let crate::t2::frame_state::ValueSource::Value { value, .. } = src {
                    if *value == from {
                        *value = to;
                    }
                }
            }
        }
    };
    let rewrite_inst = |f: &mut Function, inst: Inst, with_frame_state: bool| {
        let data = f.inst_mut(inst);
        for a in data.args.iter_mut() {
            if *a == from {
                *a = to;
            }
        }
        for call in data.targets.iter_mut() {
            for a in call.args.iter_mut() {
                if *a == from {
                    *a = to;
                }
            }
        }
        if with_frame_state {
            rewrite_frame_state(f, inst);
        }
    };
    // The anchor itself: arguments only, and only for a site anchor — its
    // FrameState must keep `from` either way.
    if rewrite_anchor_args {
        rewrite_inst(f, site, false);
    }
    // The rest of the site's block, strictly after the site.
    let insts = f.block(block).insts.clone();
    let site_pos = insts
        .iter()
        .position(|&i| i == site)
        .expect("site is in its block");
    for &inst in &insts[site_pos + 1..] {
        // The guard itself consumes `from` — never rewrite it to its own result.
        if inst == guard {
            continue;
        }
        rewrite_inst(f, inst, true);
    }
    // Every block strictly dominated by the site's block.
    for b in f.block_order().to_vec() {
        if b != block && dom.dominates(block, b) {
            for inst in f.block(b).insts.clone() {
                rewrite_inst(f, inst, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::ValueRepresentation;

    // Build `(* a b)`: two params, a Call to `*` carrying a FrameState at bcp 2,
    // then a Return of the result. Returns (function, the call Inst).
    fn build_star() -> (Function, Inst) {
        let mut f = Function::new("m");
        let entry = f.entry();
        let a = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let b = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);

        let fs = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 2,
                locals: vec![],
                stack: vec![],
            }],
            remat: vec![],
        });
        let star = bliss_rt::symbols::intern("*");
        let (call, results) = f.push_inst(
            entry,
            crate::t2::ir::InstData {
                opcode: Opcode::Call,
                args: vec![a, b],
                results: vec![],
                aux: AuxData::CallTarget(star),
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: Some(fs),
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            crate::t2::ir::InstData {
                opcode: Opcode::Return,
                args: vec![results[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        (f, call)
    }

    #[test]
    fn fixnum_site_becomes_guarded_fixnum_mul() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|bcp| {
            if bcp == 2 {
                Some(SpecType::Fixnum)
            } else {
                None
            }
        });
        assert_eq!(n, 1);
        let inst = f.inst(call);
        assert_eq!(
            inst.opcode,
            Opcode::FixnumMul,
            "* speculated fixnum → FixnumMul"
        );
        assert!(
            inst.flags.guard,
            "typed op must be guard-flagged (deopt point)"
        );
        assert!(!inst.flags.call, "no longer a generic call");
        assert!(inst.frame_state.is_some(), "keeps the deopt FrameState");
        assert!(f.value(inst.results[0]).ty.bits.contains(TypeBits::FIXNUM));
        // and no second (float) op was emitted — still one instruction.
        assert_eq!(inst.results.len(), 1);
    }

    #[test]
    fn float_site_becomes_float_mul() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|_| Some(SpecType::SingleFloat));
        assert_eq!(n, 1);
        assert_eq!(
            f.inst(call).opcode,
            Opcode::FloatMul,
            "* speculated float → FloatMul"
        );
    }

    #[test]
    fn declared_fixnum_operands_override_an_absent_profile() {
        let (mut f, call) = build_star();
        let entry = f.entry();
        let params = f.block(entry).params.clone();
        for param in params {
            f.refine_type(param, IRType::of(TypeBits::FIXNUM));
        }

        let n = speculate(&mut f, &|_| None);
        assert_eq!(
            n, 1,
            "static parameter proof must not require profile samples"
        );
        assert_eq!(f.inst(call).opcode, Opcode::FixnumMul);
    }

    #[test]
    fn polymorphic_site_is_left_generic() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|_| None); // no consistent type
        assert_eq!(n, 0);
        assert_eq!(
            f.inst(call).opcode,
            Opcode::Call,
            "polymorphic/cold site stays a Call"
        );
    }
}
