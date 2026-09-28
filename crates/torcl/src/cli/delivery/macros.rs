//! Macro definitions follow their names instead of keeping every body alive.
use super::*;

pub(super) struct MacroDefinitions {
    pub candidates: BTreeMap<u32, String>,
    pub edges: HashMap<u32, Vec<TorclVal>>,
    pub roots: Vec<TorclVal>,
    pub source: HashSet<u32>,
    pub names: HashMap<u32, String>,
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
            if symbols::symbol_package(index)
                .and_then(torcl_stdlib::packages::package_name)
                .is_some_and(|package| packages.contains(&package))
            {
                result.candidates.insert(index, qualified_name(index));
            } else {
                result.roots.push(TorclVal::from_symbol_index(index));
            }
        }
        result
    }
}

pub(super) fn retain(env: &mut Env, removed: &[(u32, String)]) {
    for (index, name) in removed {
        global_macro_remove(name);
        env.macros_mut().remove(name);
        compiler_macroexpand::undefine_global_macro(TorclVal::from_symbol_index(*index));
        MACRO_FN_CACHE.lock().unwrap().remove(name);
    }
}
