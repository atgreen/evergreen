// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A user SETF writer owns its private function cell and source metadata.
use super::*;

pub(super) struct WriterDefinitions {
    pub candidates: BTreeMap<u32, String>,
    pub names: HashMap<String, Vec<u32>>,
    pub edges: HashMap<u32, Vec<EgclVal>>,
    pub roots: Vec<EgclVal>,
    pub records: HashMap<String, Vec<u32>>,
}

pub(super) fn has_writer(place: &str) -> bool {
    [Some(place.to_owned()), alternate_qualified_spelling(place)]
        .into_iter()
        .flatten()
        .any(|name| {
            symbols::find_index(&setf_writer_symbol_name(&name))
                .and_then(symbols::symbol_function)
                .is_some_and(|function| function != egcl_rt::value::UNBOUND)
        })
}

impl WriterDefinitions {
    pub fn discover(packages: &HashSet<String>, functions: &HashMap<u32, EgclVal>) -> Self {
        let names = symbols::interned_names();
        let indices: HashMap<_, _> = names
            .iter()
            .map(|(index, name)| (name.as_str(), *index))
            .collect();
        let mut result = Self {
            candidates: BTreeMap::new(),
            names: HashMap::new(),
            edges: HashMap::new(),
            roots: Vec::new(),
            records: HashMap::new(),
        };
        let mut protected = HashSet::new();
        for (place, name) in &names {
            let mut writers = Vec::new();
            // Source SETF also probes the alternate qualified spelling.
            for spelling in [Some(name.clone()), alternate_qualified_spelling(name)]
                .into_iter()
                .flatten()
            {
                if let Some(&index) = indices.get(setf_writer_symbol_name(&spelling).as_str())
                    && functions.contains_key(&index)
                    && !writers.contains(&index)
                {
                    writers.push(index);
                }
            }
            if writers.is_empty() {
                continue;
            }
            let selected = definition_selected(*place, packages);
            for &writer in &writers {
                if selected {
                    result
                        .candidates
                        .entry(writer)
                        .or_insert_with(|| format!("(SETF {})", qualified_name(*place)));
                } else {
                    // Mangled names can alias. An owner outside the selected
                    // packages protects the shared function cell.
                    protected.insert(writer);
                }
            }
            result.names.insert(name.clone(), writers.clone());
            result.names.insert(format!("(SETF {name})"), writers);
        }
        // Legacy BFASLs can omit the original accessor symbol. A different
        // present symbol may share its lossy mangling, so inspect package
        // prefixes even when their accessor symbols are absent. Never prune a
        // cell that could belong to a package outside the requested scope.
        let mut protected_prefixes = Vec::new();
        for package in egcl_stdlib::packages::list_all_packages() {
            if let Some(name) = egcl_stdlib::packages::package_name(package)
                && !packages.contains("*")
                && !packages.contains(&name)
            {
                for spelling in
                    std::iter::once(name).chain(egcl_stdlib::packages::package_nicknames(package))
                {
                    protected_prefixes.push(format!(
                        "EGCL-INTERNAL::%SETF-WRITER-{}.",
                        spelling.replace(':', ".")
                    ));
                }
            }
        }
        result.candidates.retain(|index, _| {
            !protected.contains(index)
                && symbols::registry_key(*index).is_some_and(|name| {
                    !protected_prefixes
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
                })
        });
        let definitions = with_global_setf_fns(|functions| functions.borrow().clone());
        for (name, mut definition) in definitions {
            let mut references = Vec::new();
            visit_fun_def_roots(&mut definition, &mut |slot| {
                // The caller holds a nonallocating stopped-world snapshot.
                references.push(unsafe { *slot });
            });
            if let Some(writers) = result.names.get(&name) {
                for &writer in writers {
                    result.edges.entry(writer).or_default().extend(&references);
                }
                result.records.insert(name, writers.clone());
            } else {
                // Legacy records without a known function cell remain roots.
                result.roots.extend(references);
            }
        }
        result
    }
}

pub(super) fn remove(names: &[String]) {
    with_global_setf_fns(|functions| {
        let mut functions = functions.borrow_mut();
        for name in names {
            functions.remove(name);
        }
    });
}
