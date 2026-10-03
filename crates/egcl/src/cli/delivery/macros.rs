// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Macro definitions follow their names instead of keeping every body alive.
use super::*;

pub(super) struct MacroDefinitions {
    pub candidates: BTreeMap<u32, String>,
    pub edges: HashMap<u32, Vec<EgclVal>>,
    pub roots: Vec<EgclVal>,
    pub source: HashSet<u32>,
    pub names: HashMap<u32, String>,
    pub callback_owners: HashMap<u64, u32>,
}

impl MacroDefinitions {
    pub fn discover(env: &Env, packages: &HashSet<String>) -> Self {
        let mut definitions = GLOBAL_MACROS.lock().unwrap().clone();
        definitions.extend(env.macros.borrow().clone());
        let mut result = Self {
            candidates: BTreeMap::new(),
            edges: HashMap::new(),
            roots: Vec::new(),
            source: HashSet::new(),
            names: HashMap::new(),
            callback_owners: HashMap::new(),
        };
        // The caller holds a nonallocating heap snapshot. Clones here retain
        // references only for that traversal, never across Lisp allocation.
        for (name, mut definition) in definitions {
            let mut references = Vec::new();
            let mut state = EnvRootVisitState::default();
            visit_macro_def_roots(&mut definition, &mut state, &mut |slot| {
                references.push(unsafe { *slot });
            });
            if let Some(function) = &definition.bytecode {
                references.extend(bytecode_references(&function.lock().unwrap()));
            }
            let Some(index) = reader::find_symbol_index(&name) else {
                // Legacy registry keys without a symbol cannot be candidates.
                result.roots.extend(references);
                continue;
            };
            if definition.bytecode.is_none() && definition.function.is_none() {
                result.source.insert(index);
            }
            result.names.insert(index, name);
            result.edges.insert(index, references);
            if definition_selected(index, packages) {
                result.candidates.insert(index, qualified_name(index));
            } else {
                result.roots.push(EgclVal::from_symbol_index(index));
            }
        }
        let name_indices: HashMap<_, _> = result
            .names
            .iter()
            .map(|(&index, name)| (name.clone(), index))
            .collect();
        for (name, entry) in MACRO_FN_CACHE.lock().unwrap().iter() {
            let references = [
                EgclVal(entry.params_bits),
                EgclVal(entry.body_bits),
                entry.symbol,
            ];
            if let Some(&index) = name_indices.get(name) {
                result.edges.entry(index).or_default().extend(references);
                result.callback_owners.insert(entry.handle.0, index);
            } else {
                result.roots.extend(references);
            }
        }
        let captures = frozen_macro_captures().lock().unwrap();
        for weak in captures.iter() {
            let Some(capture) = weak.upgrade() else {
                continue;
            };
            let mut capture = capture.lock().unwrap();
            let Some((name, handle)) = capture.registration.clone() else {
                continue;
            };
            let mut references = vec![capture.params_form, capture.body];
            let mut frames = HashSet::new();
            visit_frozen_env_frame_roots(&capture.captured_frame, &mut frames, &mut |slot| {
                references.push(unsafe { *slot });
            });
            for definition in capture.funs.values_mut() {
                visit_fun_def_roots(definition, &mut EnvRootVisitState::default(), &mut |slot| references.push(unsafe { *slot }));
            }
            references.extend(capture.symbol_macros.values().copied());
            if let Some(&index) = name_indices.get(&name) {
                result.edges.entry(index).or_default().extend(references);
                result.callback_owners.insert(handle.0, index);
            } else {
                result.roots.extend(references);
            }
        }
        result
    }
}

pub(super) fn retain(env: &mut Env, removed: &[(u32, String)]) {
    for (index, name) in removed {
        global_macro_remove(name);
        env.macros_mut().remove(name);
        compiler_macroexpand::undefine_global_macro(EgclVal::from_symbol_index(*index));
        MACRO_FN_CACHE.lock().unwrap().remove(name);
    }
}
