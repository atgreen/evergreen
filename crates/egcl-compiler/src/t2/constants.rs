// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Immutable compiler descriptions captured while the source heap is stable.
//!
//! Capture requires the same GC exclusion as reading a live bytecode body.
//! Afterwards the descriptions and sanitized metadata contain no movable Lisp
//! values. They do not own roots: the client must retain the original bodies
//! and root their pools until all artifacts using their slot addresses retire.

use egcl_rt::bytecode::BytecodeFunction;
use egcl_rt::value::{EgclVal, NIL, T};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum ConstantDescription {
    Nil,
    True,
    Fixnum(i64),
    Character(char),
    SingleFloat(f32),
    Symbol {
        index: u32,
        name: Option<String>,
    },
    Heap {
        slot: usize,
        double_bits: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct ConstantPoolSnapshot {
    constants: Vec<ConstantDescription>,
}

impl ConstantPoolSnapshot {
    /// Read source values only while their owner excludes moving GC.
    pub fn capture(body: &BytecodeFunction) -> Self {
        let constants = body
            .constants
            .iter()
            .map(|slot| {
                let value = *slot;
                if value == NIL {
                    ConstantDescription::Nil
                } else if value == T {
                    ConstantDescription::True
                } else if value.is_fixnum() {
                    ConstantDescription::Fixnum(value.as_fixnum())
                } else if value.is_character() {
                    ConstantDescription::Character(value.as_char())
                } else if value.is_single_float() {
                    ConstantDescription::SingleFloat(value.as_single_float())
                } else if value.is_symbol() {
                    let index = value.as_symbol_index();
                    ConstantDescription::Symbol {
                        index,
                        name: crate::reader::symbol_name(index),
                    }
                } else {
                    ConstantDescription::Heap {
                        slot: slot as *const EgclVal as usize,
                        double_bits: value
                            .is_double_float()
                            .then(|| value.as_double_float().to_bits()),
                    }
                }
            })
            .collect();
        Self { constants }
    }

    pub fn get(&self, index: usize) -> Option<&ConstantDescription> {
        self.constants.get(index)
    }
    pub fn len(&self) -> usize {
        self.constants.len()
    }
    pub fn is_empty(&self) -> bool {
        self.constants.is_empty()
    }
}

/// Worker-owned metadata, deliberately unsuitable for interpretation or runtime
/// binding. Runtime/deopt metadata must refer to the retained original body.
#[derive(Clone, Debug)]
pub struct BytecodeSnapshot {
    pub body: Arc<BytecodeFunction>,
    pub constants: Arc<ConstantPoolSnapshot>,
    /// Original identity, for the client's mapping back to rooted owners.
    pub source_body: usize,
    pub nested_functions: Vec<BytecodeSnapshot>,
    pub restart_functions: Vec<BytecodeSnapshot>,
}

impl BytecodeSnapshot {
    /// Capture under the source owner's GC exclusion, then retain that owner
    /// separately. Only the returned metadata may be read without that gate.
    pub fn capture(source: &BytecodeFunction) -> Self {
        let constants = Arc::new(ConstantPoolSnapshot::capture(source));
        let mut body = source.clone();
        body.constants.fill(NIL);
        body.params_form = NIL;
        body.load_time_values.clear();
        body.binder_cache = Default::default();
        let nested_functions: Vec<_> = source
            .nested_functions
            .iter()
            .map(|nested| Self::capture(nested))
            .collect();
        body.nested_functions = nested_functions
            .iter()
            .map(|nested| Arc::clone(&nested.body))
            .collect();
        let mut restart_functions = Vec::new();
        for (original, copied) in source.restart_cases.iter().zip(&mut body.restart_cases) {
            for (original, copied) in original.restarts.iter().zip(&mut copied.restarts) {
                let snapshot = Self::capture(&original.function);
                *copied.function = (*snapshot.body).clone();
                restart_functions.push(snapshot);
            }
        }
        Self {
            body: Arc::new(body),
            constants,
            source_body: source as *const _ as usize,
            nested_functions,
            restart_functions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::{
        build,
        inlining::InlineOptions,
        ir::{AuxData, Opcode},
    };
    use egcl_rt::bytecode::{BytecodeFunction, Instr};
    use egcl_rt::value::{EgclVal, NIL};

    fn options() -> InlineOptions {
        InlineOptions::default().with_root_symbol(egcl_rt::symbols::intern("CONSTANT-SNAPSHOT"))
    }

    fn body(constants: Vec<EgclVal>) -> BytecodeFunction {
        BytecodeFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            code: vec![Instr::Const(0), Instr::Return],
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
            n_locals: 0,
            max_stack: 1,
            arity: 0,
            name: "CONSTANT-SNAPSHOT".into(),
            params_form: NIL,
            min_args: 0,
            max_args: Some(0),
            variadic: false,
            binder_cache: Default::default(),
            closure_template: Default::default(),
        }
    }

    #[test]
    fn snapshot_build_ignores_rewritten_source_constants() {
        let mut source = body(vec![EgclVal::from_fixnum(41)]);
        let snapshot = BytecodeSnapshot::capture(&source);
        source.constants[0] = EgclVal::from_fixnum(99);
        let ir = build::build_from_snapshot(&snapshot, options()).unwrap();
        assert!(
            ir.block_order()
                .iter()
                .flat_map(|&b| &ir.block(b).insts)
                .any(|&i| matches!(ir.inst(i).aux, AuxData::FixnumImm(41)))
        );
        assert_eq!(snapshot.body.constants, vec![NIL]);
    }

    #[test]
    fn double_snapshot_never_dereferences_literal_slot() {
        let mut snapshot = BytecodeSnapshot::capture(&body(vec![NIL]));
        // Not a valid address: building and specializing must use the captured
        // description, reserving this address solely for generated loads.
        snapshot.constants = std::sync::Arc::new(ConstantPoolSnapshot {
            constants: vec![ConstantDescription::Heap {
                slot: 1,
                double_bits: Some(1.25_f64.to_bits()),
            }],
        });
        let ir = build::build_from_snapshot(&snapshot, options()).unwrap();
        let literal = ir
            .block_order()
            .iter()
            .flat_map(|&b| &ir.block(b).insts)
            .map(|&i| ir.inst(i))
            .find(|i| i.opcode == Opcode::ConstHeapObj)
            .unwrap();
        assert!(matches!(literal.aux, AuxData::HeapLiteral { slot: 1 }));
        assert_eq!(
            crate::t2::speculate::double_literal_bits(&ir, literal.results[0]),
            Some(1.25_f64.to_bits())
        );
    }
    #[test]
    fn snapshots_remove_movable_fields_recursively() {
        let mut source = body(vec![EgclVal::from_fixnum(41)]);
        source.params_form = EgclVal::from_fixnum(7);
        source.load_time_values.push((0, EgclVal::from_fixnum(8)));
        let child = source.clone();
        source
            .nested_functions
            .push(std::sync::Arc::new(child.clone()));
        source
            .restart_cases
            .push(egcl_rt::bytecode::RestartCaseInfo {
                restarts: vec![egcl_rt::bytecode::RestartClauseInfo {
                    name: "AGAIN".into(),
                    function: Box::new(child),
                }],
            });
        let snapshot = BytecodeSnapshot::capture(&source);
        for metadata in [
            &*snapshot.body,
            &*snapshot.body.nested_functions[0],
            &*snapshot.body.restart_cases[0].restarts[0].function,
        ] {
            assert_eq!(metadata.constants, vec![NIL]);
            assert_eq!(metadata.params_form, NIL);
            assert!(metadata.load_time_values.is_empty());
        }
        assert_eq!(snapshot.source_body, &source as *const _ as usize);
    }

    #[test]
    fn inlining_preserves_captured_double_bits() {
        let symbol = egcl_rt::symbols::intern("SNAPSHOT-INLINE-DOUBLE");
        let mut callee = BytecodeSnapshot::capture(&body(vec![NIL]));
        callee.constants = std::sync::Arc::new(ConstantPoolSnapshot {
            constants: vec![ConstantDescription::Heap {
                slot: 1,
                double_bits: Some(2.5_f64.to_bits()),
            }],
        });
        let mut source = body(vec![]);
        source.code = vec![
            Instr::CallNamed {
                sym: symbol,
                nargs: 0,
            },
            Instr::Return,
        ];
        let caller = BytecodeSnapshot::capture(&source);
        let options = options().with_body_snapshot(symbol, &callee);
        let ir = build::build_from_snapshot(&caller, options).unwrap();
        let literal = ir
            .block_order()
            .iter()
            .flat_map(|&b| &ir.block(b).insts)
            .map(|&i| ir.inst(i))
            .find(|i| i.opcode == Opcode::ConstHeapObj)
            .expect("callee literal was inlined");
        assert_eq!(
            crate::t2::speculate::double_literal_bits(&ir, literal.results[0]),
            Some(2.5_f64.to_bits())
        );
    }
    #[test]
    fn snapshot_keeps_notinline_call_policy_after_source_mutation() {
        let mut source = body(vec![EgclVal::from_fixnum(7)]);
        let symbol = egcl_rt::symbols::intern("IDENTITY");
        source.code.insert(
            1,
            Instr::CallNamed {
                sym: symbol,
                nargs: 1,
            },
        );
        source.notinline_sites = vec![1];
        let snapshot = BytecodeSnapshot::capture(&source);
        source.notinline_sites.clear();
        assert_eq!(snapshot.body.notinline_sites, vec![1]);
        let ir = build::build_from_snapshot(&snapshot, options()).unwrap();
        assert!(
            ir.block_order()
                .iter()
                .flat_map(|&b| &ir.block(b).insts)
                .any(|&i| matches!(ir.inst(i).aux, AuxData::OpaqueCallTarget(s) if s == symbol))
        );
    }
}
