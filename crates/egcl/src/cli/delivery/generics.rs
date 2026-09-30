//! Ownership of generic functions and methods in a saved world.
use super::*;

// Called under the delivery heap snapshot in the disposable driver process.
// Reified method callables also hold a copy of the source body. Only discard
// it when the saved bytecode is independently executable after restoration.
pub(super) fn discard_compiled_method_sources() {
    let bytecode = bytecode::delivery_walker_dependencies();
    for &callable in METHOD_COMPILED.borrow().values() {
        if !egcl_rt::function::is_interpreted_function(callable) {
            continue;
        }
        let name = egcl_rt::function::name(callable);
        if name.symbol_index().and_then(|index| bytecode.get(&index)) != Some(&None) {
            continue;
        }
        // SAFETY: callable is a live interpreted function, the heap is stopped,
        // and changing its source fields allocates no Lisp objects. Dispatch
        // and image restoration preserve its registered bytecode by name.
        unsafe {
            egcl_rt::function::redefine(
                callable,
                egcl_rt::function::lambda_list(callable),
                NIL,
                egcl_rt::function::env(callable),
            );
        }
    }
}

pub(super) struct GenericDefinitions {
    pub names: HashMap<String, EgclVal>,
    pub candidates: BTreeMap<String, u64>,
    pub roots: Vec<EgclVal>,
    pub edges: HashMap<u64, Vec<(EgclVal, bool)>>,
    pub handles: HashSet<u64>,
    pub source_methods: HashMap<u64, String>,
}

impl GenericDefinitions {
    // All references are consumed inside the delivery heap snapshot.
    pub fn discover(env: &Env, packages: &HashSet<String>) -> Self {
        let definitions = egcl_stdlib::clos::delivery_definitions();
        let handles = definitions.edges.keys().map(|handle| handle.0).collect();
        let mut result = Self {
            names: HashMap::new(),
            candidates: BTreeMap::new(),
            roots: Vec::new(),
            edges: definitions
                .edges
                .into_iter()
                .map(|(handle, refs)| {
                    (
                        handle.0,
                        refs.into_iter().map(|value| (value, false)).collect(),
                    )
                })
                .collect(),
            source_methods: HashMap::new(),
            handles,
        };
        for (handle, name_form) in definitions.generics {
            let Some((name, symbol)) = function_name(name_form) else {
                result.roots.push(handle);
                continue;
            };
            result.names.insert(name.clone(), handle);
            if symbol
                .symbol_index()
                .is_some_and(|index| definition_selected(index, packages))
            {
                result.candidates.insert(name, handle.0);
            } else {
                result.roots.push(handle);
            }
        }
        // Evaluator keys also cover compound SETF names and restored aliases.
        for (name, definition) in env.generics.borrow().iter() {
            if !result.edges.contains_key(&definition.generic_function.0) {
                // Older images can lack the stdlib metadata for a host generic.
                result.roots.push(definition.generic_function);
                result
                    .edges
                    .entry(definition.generic_function.0)
                    .or_default();
            }
            result.handles.insert(definition.generic_function.0);
            result
                .names
                .insert(name.clone(), definition.generic_function);
        }
        let compiled: HashMap<_, _> = METHOD_COMPILED
            .borrow()
            .iter()
            .map(|(&id, &callable)| (id, callable))
            .collect();
        for (&id, &callable) in &compiled {
            result.handles.insert(id);
            result.edges.entry(id).or_default().push((callable, true));
            result
                .edges
                .entry(callable.0)
                .or_default()
                .push((EgclVal::from_raw(id), false));
        }
        for (name, methods) in env.methods.borrow().iter() {
            for method in methods {
                let id = method.method_id;
                result.handles.insert(id.0);
                if let Some(&owner) = result.names.get(name) {
                    result.edges.entry(owner.0).or_default().push((id, false));
                    result.edges.entry(id.0).or_default().push((owner, false));
                } else {
                    // An unmatched host method is an unknown external root.
                    result.roots.push(id);
                }
                let mut method = method.clone();
                // invoke_method always uses the saved callable when present.
                // Its original source is no longer executable, and following
                // it would retain macros used only while compiling the method.
                if compiled.contains_key(&id.0) {
                    method.body = NIL;
                }
                let mut state = EnvRootVisitState::default();
                visit_method_def_roots(&mut method, &mut state, &mut |slot| {
                    // SAFETY: the registry owns these values throughout this
                    // stopped-world traversal, which allocates no Lisp objects.
                    result
                        .edges
                        .entry(id.0)
                        .or_default()
                        .push((unsafe { *slot }, true));
                });
                if !compiled.contains_key(&id.0) {
                    result
                        .source_methods
                        .insert(id.0, format!("{name}: source generic method"));
                }
            }
        }
        // Class metadata exposes these names through introspection. They are
        // Rust strings, so ordinary heap traversal cannot discover the edges.
        let accessor_names: Vec<String> = env
            .classes
            .borrow()
            .values()
            .flat_map(|class| &class.slots)
            .flat_map(|slot| {
                slot.accessor
                    .iter()
                    .chain(&slot.readers)
                    .chain(&slot.writers)
            })
            .cloned()
            .collect();
        for name in accessor_names {
            if let Some(index) = symbols::find_index(&name) {
                result.roots.push(EgclVal::from_symbol_index(index));
            }
            for key in [name.clone(), format!("(SETF {name})")] {
                if let Some(&handle) = result.names.get(&key) {
                    result.roots.push(handle);
                }
            }
        }
        result
    }
}

pub(super) fn function_name(value: EgclVal) -> Option<(String, EgclVal)> {
    if value.is_symbol() && value != NIL && value != T {
        return Some((sym_name(value), value));
    }
    if value.is_cons() {
        let (operator, rest) = cp(value);
        if operator.is_symbol() && sym_bare_name_rc(operator).as_ref() == "SETF" && rest.is_cons() {
            let (symbol, tail) = cp(rest);
            if symbol.is_symbol() && tail.is_nil() {
                return Some((function_name_key(value), symbol));
            }
        }
    }
    None
}

pub(super) fn retain(env: &mut Env, live: &HashSet<u64>) {
    env.generics
        .borrow_mut()
        .retain(|_, def| live.contains(&def.generic_function.0));
    let compiled: HashSet<u64> = METHOD_COMPILED.borrow().keys().copied().collect();
    env.methods.borrow_mut().retain(|_, methods| {
        methods.retain(|method| live.contains(&method.method_id.0));
        for method in methods.iter_mut() {
            if compiled.contains(&method.method_id.0) {
                method.body = NIL;
            }
        }
        !methods.is_empty()
    });
    METHOD_COMPILED
        .borrow_mut()
        .retain(|id, _| live.contains(id));
    egcl_stdlib::clos::retain_delivery_definitions(live);
    invalidate_gf_dispatch_cache();
}
