// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Open-coded `CHAR`, `SCHAR` and `ELT` on simple strings (GitHub #487).
//!
//! # Purpose
//!
//! A call to one of these accessors costs a full call through the runtime even
//! when its operand is a simple string and its index is in range, which is
//! nearly always. `CHAR` is itself `(ELT S I)`, so a T2 caller of `CHAR` paid
//! two calls per character. This pass gives each such call a fast path that
//! reads the character inline and keeps the original call for everything else.
//!
//! # Shape
//!
//! A selected `Call` in block `B` becomes
//!
//! ```text
//! B:     ...; c = StringCharOrNil(s, i); Brif c -> Fast, Slow
//! Fast:  ClearMv; Jump Join(c)
//! Slow:  r = Call CHAR(s, i); Jump Join(r)
//! Join:  <the rest of B, reading the parameter instead of r>
//! ```
//!
//! When the call is immediately returned, both arms return directly instead of
//! joining, which keeps the slow arm's `Call; Return` tail shape that ordinary
//! emission recognizes.
//!
//! # Contract
//!
//! * `StringCharOrNil` never signals, allocates, deoptimizes or collects. NIL
//!   is never a character, so it is an unambiguous "not handled" answer.
//! * The slow arm is the original call with its original frame state, so every
//!   case the fast path declines — a non-string, a complex or displaced string,
//!   a list or vector for `ELT`, a non-fixnum or out-of-range index — produces
//!   exactly the value, error, restart and backtrace it produced before.
//! * The fast arm resets the multiple-value state, as the call's single-valued
//!   return does; redundant resets are removed later.
//! * Only the canonical `COMMON-LISP` symbols are matched. Redefining a
//!   standard CL function is unsupported (as for the open-coded `CAR`), and
//!   a same-named symbol in another package stays an ordinary call.
//! * Only a target whose emitter has the arm emits `StringCharOrNil` -- see
//!   `target_support::simple_string_char_access`; others keep the call.
//!   `EGCL_INLINE_STRING_CHAR=0` keeps the call everywhere, for comparison.

use std::collections::{HashMap, HashSet};

use super::ir::{
    AuxData, BlockCall, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits,
    ValueRepresentation,
};

/// `EGCL_INLINE_STRING_CHAR=0` keeps every accessor an ordinary call, for
/// comparison. Read once.
fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("EGCL_INLINE_STRING_CHAR").is_none_or(|value| value != "0")
    })
}

/// Whether `symbol` names the canonical `CL:CHAR`, `CL:SCHAR` or `CL:ELT`.
pub(crate) fn is_string_char_accessor(symbol: u32) -> bool {
    if !super::target_support::simple_string_char_access() || !enabled() {
        return false;
    }
    let Some(key) = egcl_rt::symbols::registry_key(symbol) else {
        return false;
    };
    matches!(key.as_str(), "CHAR" | "SCHAR" | "ELT")
        && egcl_rt::symbols::find_index(&key) == Some(symbol)
}

/// Give each call in `calls` an inline simple-string fast path. Each must be a
/// two-argument, one-result `Call` (not an `Invoke`) still in a block.
pub(crate) fn admit_string_char_calls(f: &mut Function, calls: &HashSet<Inst>) {
    if calls.is_empty() {
        return;
    }
    let mut renamed = HashMap::new();
    let mut slow_exits = Vec::new();
    let mut work = f.block_order().to_vec();
    while let Some(block) = work.pop() {
        let Some(pos) = f.block(block).insts.iter().position(|inst| {
            calls.contains(inst) && {
                let data = f.inst(*inst);
                data.opcode == Opcode::Call
                    && data.args.len() == 2
                    && data.results.len() == 1
                    && data.frame_state.is_some()
                    && matches!(data.aux, AuxData::CallTarget(_))
            }
        }) else {
            continue;
        };
        let call = f.block(block).insts[pos];
        let data = f.inst(call).clone();
        let result = data.results[0];
        let source_pos = data.source_pos;
        let tail = f.block_mut(block).insts.split_off(pos);
        let rest: Vec<Inst> = tail[1..].to_vec();
        // `Call; Return(result)`: both arms return.
        let returned = match rest.as_slice() {
            [only] => {
                let ret = f.inst(*only);
                (ret.opcode == Opcode::Return && ret.args == [result]).then(|| ret.clone())
            }
            _ => None,
        };
        // Inserted in reverse, so the layout reads B, Fast, Slow[, Join].
        let join = returned.is_none().then(|| f.make_block_after(block));
        let slow = f.make_block_after(block);
        let fast = f.make_block_after(block);
        f.block_mut(slow).insts = vec![call];

        let (_, character) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::StringCharOrNil,
                args: data.args.clone(),
                results: vec![],
                aux: AuxData::None,
                // Ordered after stores to the string; not a guard or call.
                flags: InstFlags {
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos,
            },
            &[(
                IRType::of(TypeBits::CHARACTER.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        let character = character[0];
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Brif,
                args: vec![character],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![
                    BlockCall {
                        block: fast,
                        args: vec![],
                    },
                    BlockCall {
                        block: slow,
                        args: vec![],
                    },
                ],
                frame_state: None,
                source_pos,
            },
        );
        // As the builder emits every reset: a runtime-helper safepoint with
        // the state the interpreter resumes in, here the call's own.
        f.push_inst(
            fast,
            InstData {
                opcode: Opcode::ClearMv,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: data.frame_state,
                source_pos,
            },
            &[],
        );
        let exit = |value| match (&returned, join) {
            (Some(ret), _) => InstData {
                args: vec![value],
                ..ret.clone()
            },
            (None, Some(join)) => InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![BlockCall {
                    block: join,
                    args: vec![value],
                }],
                frame_state: None,
                source_pos,
            },
            (None, None) => unreachable!("a non-returning split has a join"),
        };
        f.set_terminator(fast, exit(character));
        // The slow edge must carry the call's own result, so it is added
        // only after the uses are renamed below.
        slow_exits.push((slow, exit(result)));
        if let Some(join) = join {
            f.block_mut(join).insts = rest;
            let merged = f.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
            renamed.insert(result, merged);
            work.push(join);
        }
    }
    f.replace_values(&renamed);
    for (slow, exit) in slow_exits {
        f.set_terminator(slow, exit);
    }
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::super::build::{build_from_bytecode, build_from_bytecode_with_inline_options};
    use super::super::inlining::{InlineOptions, InlinePolicy};
    use super::super::ir::{Function, Opcode};
    use egcl_rt::bytecode::{BytecodeFunction, Instr};
    use egcl_rt::value::NIL;

    fn body(code: Vec<Instr>, arity: u16) -> BytecodeFunction {
        BytecodeFunction {
            self_call_sites: Vec::new(),
            notinline_sites: Vec::new(),
            code,
            constants: vec![],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: arity,
            max_stack: 3,
            arity,
            name: "string-char-probe".to_string(),
            params_form: NIL,
            min_args: arity,
            max_args: Some(arity),
            variadic: false,
            binder_cache: Default::default(),
        }
    }

    fn count(f: &Function, opcode: Opcode) -> usize {
        f.block_order()
            .iter()
            .flat_map(|&block| f.block(block).insts.iter())
            .filter(|&&inst| f.inst(inst).opcode == opcode)
            .count()
    }

    fn accessor_call(name: &str) -> Vec<Instr> {
        vec![
            Instr::LoadLocal(0),
            Instr::LoadLocal(1),
            Instr::CallNamed {
                sym: egcl_rt::symbols::intern(name),
                nargs: 2,
            },
            Instr::Return,
        ]
    }

    #[test]
    fn a_returned_accessor_call_gets_a_fast_arm_and_keeps_its_tail_call() {
        for name in ["CHAR", "SCHAR", "ELT"] {
            let f = build_from_bytecode(&body(accessor_call(name), 2)).expect("builds");
            super::super::verify::verify(&f).expect("split IR verifies");
            assert_eq!(count(&f, Opcode::StringCharOrNil), 1, "{name}");
            assert_eq!(
                count(&f, Opcode::Call),
                1,
                "{name}: the slow arm keeps the call"
            );
            assert_eq!(count(&f, Opcode::Return), 2, "{name}: both arms return");
            assert_eq!(
                count(&f, Opcode::ClearMv),
                1,
                "{name}: the fast arm resets values"
            );
            // Emission maps every reset as a runtime-helper safepoint.
            let reset = f
                .block_order()
                .iter()
                .flat_map(|&b| f.block(b).insts.iter())
                .map(|&inst| f.inst(inst))
                .find(|data| data.opcode == Opcode::ClearMv)
                .unwrap();
            assert!(reset.flags.call && reset.flags.safepoint && reset.frame_state.is_some());
            // The call is still immediately returned, with its frame state.
            let block = f
                .block_order()
                .iter()
                .copied()
                .find(|&block| {
                    f.block(block)
                        .insts
                        .iter()
                        .any(|&inst| f.inst(inst).opcode == Opcode::Call)
                })
                .unwrap();
            let insts = &f.block(block).insts;
            let call = f.inst(insts[0]);
            assert!(call.frame_state.is_some());
            assert_eq!(f.inst(insts[1]).opcode, Opcode::Return);
            assert_eq!(f.inst(insts[1]).args, call.results);
        }
    }

    #[test]
    fn a_used_accessor_result_joins_both_arms() {
        let mut code = accessor_call("CHAR");
        code.pop();
        code.extend([
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: egcl_rt::symbols::intern("EQ"),
                nargs: 2,
            },
            Instr::Return,
        ]);
        let f = build_from_bytecode(&body(code, 2)).expect("builds");
        super::super::verify::verify(&f).expect("split IR verifies");
        assert_eq!(count(&f, Opcode::StringCharOrNil), 1);
        assert_eq!(
            count(&f, Opcode::Return),
            1,
            "the rest of the block runs once, in the join"
        );
        let join = f
            .block_order()
            .iter()
            .copied()
            .find(|&block| f.block(block).params.len() == 1 && block != f.entry())
            .expect("a join with the accessor's value");
        let eq = f
            .block(join)
            .insts
            .iter()
            .map(|&inst| f.inst(inst))
            .find(|data| data.opcode == Opcode::GenericEq)
            .expect("EQ reads the join");
        assert_eq!(eq.args[0], f.block(join).params[0]);
    }

    #[test]
    fn notinline_and_foreign_symbols_keep_a_plain_call() {
        let options = InlineOptions::default().with_policy(2, InlinePolicy::NotInline);
        let f = build_from_bytecode_with_inline_options(&body(accessor_call("CHAR"), 2), options)
            .expect("builds");
        assert_eq!(
            count(&f, Opcode::StringCharOrNil),
            0,
            "NOTINLINE keeps the call"
        );
        for name in ["STRING-CHAR-PROBE::CHAR", "AREF", "CHAR-CODE"] {
            let f = build_from_bytecode(&body(accessor_call(name), 2)).expect("builds");
            assert_eq!(count(&f, Opcode::StringCharOrNil), 0, "{name}");
            assert_eq!(count(&f, Opcode::Call), 1, "{name}");
        }
        // Another arity is a different (erroneous) call; leave it alone.
        let one = vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: egcl_rt::symbols::intern("CHAR"),
                nargs: 1,
            },
            Instr::Return,
        ];
        let f = build_from_bytecode(&body(one, 1)).expect("builds");
        assert_eq!(count(&f, Opcode::StringCharOrNil), 0);
    }
}
