// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Saved-world delivery. Analysis runs without Lisp allocations in a stopped
//! world; its result contains symbol indices, never unrooted heap pointers.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use egcl_rt::bytecode::{BytecodeFunction, Instr};
use egcl_rt::symbols;
mod generics;
mod macros;
mod writers;

fn error(message: impl Into<String>) -> EgclError {
    EgclError::ProgramError(format!("delivery: {}", message.into()))
}

struct Spec {
    entry: String,
    packages: Vec<String>,
    keep: Vec<String>,
    explicit_dynamic_roots: bool,
    specialized: bool,
    max_tier: crate::runtime_contract::NativeTier,
    runtime_keep: std::collections::BTreeSet<String>,
}

impl Spec {
    fn parse(text: &str) -> Result<Self, EgclError> {
        let mut runtime = None;
        let mut max_tier = None;
        let mut runtime_keep = std::collections::BTreeSet::new();
        let mut entry = None;
        let mut version = None;
        let mut dynamic = None;
        let mut packages = Vec::new();
        let mut keep = Vec::new();
        for (line, text) in text.lines().enumerate() {
            let text = text.trim();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let (key, value) = text
                .split_once('=')
                .ok_or_else(|| error(format!("spec line {}: expected key = value", line + 1)))?;
            let (key, value) = (key.trim(), value.trim());
            if value.is_empty() {
                return Err(error(format!("spec line {}: empty value", line + 1)));
            }
            match key {
                "version" if version.is_none() => version = Some(value),
                "entry" if entry.is_none() => entry = Some(value.to_owned()),
                "dynamic" if dynamic.is_none() => dynamic = Some(value),
                "prune-package" => packages.push(value.to_owned()),
                "keep" => keep.push(value.to_owned()),
                "runtime" if runtime.is_none() => runtime = Some(value),
                "max-tier" if max_tier.is_none() => max_tier = Some(value),
                "runtime-keep" => {
                    if !crate::runtime_contract::CAPABILITIES.contains(&value) {
                        return Err(error(format!("unknown runtime capability {value}")));
                    }
                    runtime_keep.insert(value.to_owned());
                }
                _ => {
                    return Err(error(format!(
                        "spec line {}: unknown or duplicate key {key}",
                        line + 1
                    )));
                }
            }
        }
        if version != Some("1") {
            return Err(error("spec requires version = 1"));
        }
        let explicit_dynamic_roots = match dynamic.unwrap_or("preserve") {
            "preserve" => false,
            "explicit" => true,
            _ => return Err(error("dynamic must be preserve or explicit")),
        };
        let specialized = match runtime.unwrap_or("full") {
            "full" => false,
            "specialized" => true,
            _ => return Err(error("runtime must be full or specialized")),
        };
        if !specialized && !runtime_keep.is_empty() {
            return Err(error("runtime-keep requires runtime = specialized"));
        }
        let max_tier =
            crate::runtime_contract::NativeTier::parse(max_tier.unwrap_or("t2")).map_err(error)?;
        if !specialized && max_tier != crate::runtime_contract::NativeTier::T2 {
            return Err(error(
                "omitting native tiers requires runtime = specialized",
            ));
        }
        Ok(Self {
            max_tier,
            specialized,
            runtime_keep,
            entry: entry.ok_or_else(|| error("spec requires entry"))?,
            packages,
            keep,
            explicit_dynamic_roots,
        })
    }
}

fn function_index(name: &str) -> Result<u32, EgclError> {
    definition_index(name, false)
}

fn definition_index(name: &str, allow_auxiliary: bool) -> Result<u32, EgclError> {
    let symbol = if let Some((package, bare)) = name.split_once("::") {
        let package = egcl_stdlib::packages::find_package(package)
            .ok_or_else(|| error(format!("unknown package in {name}")))?;
        egcl_stdlib::packages::find_symbol(bare, package)?
            .ok_or_else(|| error(format!("unknown function {name}")))?
            .0
    } else {
        // Legacy bootstrap helpers have an exact registry key but no package.
        // KEEP must be able to retain them when whole-world pruning is used.
        let index = symbols::find_index(name)
            .filter(|&index| {
                allow_auxiliary
                    && symbols::symbol_package(index)
                        .and_then(egcl_stdlib::packages::package_name)
                        .is_none()
            })
            .ok_or_else(|| error(format!("use PACKAGE::FUNCTION for {name}")))?;
        EgclVal::from_symbol_index(index)
    };
    let index = symbol
        .symbol_index()
        .ok_or_else(|| error(format!("unknown function {name}")))?;
    let generic_name = sym_name(symbol);
    let callable = symbols::symbol_function(index).is_some_and(|v| v != egcl_rt::value::UNBOUND)
        || GENERIC_DEFINITIONS.borrow().contains_key(&generic_name);
    let macro_root = allow_auxiliary && GLOBAL_MACROS.lock().unwrap().contains_key(&generic_name);
    let auxiliary = macro_root || (allow_auxiliary && writers::has_writer(&generic_name));
    if !callable && !auxiliary {
        return Err(error(format!("undefined function {name}")));
    }
    Ok(index)
}

fn qualified_name(index: u32) -> String {
    let name = symbols::symbol_name(index).unwrap_or_default();
    let bare = name.rsplit(':').next().unwrap_or(&name);
    let package = symbols::symbol_package(index).and_then(egcl_stdlib::packages::package_name);
    match package {
        Some(package) => format!("{package}::{bare}"),
        None => name,
    }
}

fn definition_selected(index: u32, packages: &HashSet<String>) -> bool {
    packages.contains("*")
        || symbols::symbol_package(index)
            .and_then(egcl_stdlib::packages::package_name)
            .is_some_and(|package| packages.contains(&package))
}

/// Include instruction operands as well as GC-visible constants: CallNamed and
/// LoadFunction store symbol indices as raw integers, not tagged GC references.
pub(super) fn bytecode_references(function: &BytecodeFunction) -> Vec<EgclVal> {
    bytecode_references_with_params(function, true)
}

pub(super) fn bytecode_callable_references(function: &BytecodeFunction) -> Vec<EgclVal> {
    bytecode_references_with_params(function, false)
}

fn bytecode_references_with_params(
    function: &BytecodeFunction,
    include_params: bool,
) -> Vec<EgclVal> {
    let mut refs = function.constants.clone();
    if include_params {
        refs.push(function.params_form);
    }
    refs.extend(function.load_time_values.iter().map(|(_, value)| *value));
    for instruction in &function.code {
        if let Instr::CallNamed { sym, .. }
        | Instr::LoadFunction(sym)
        | Instr::LoadGlobal(sym)
        | Instr::StoreGlobal(sym)
        | Instr::BindSpecial(sym) = instruction
        {
            refs.push(EgclVal::from_symbol_index(*sym));
        }
    }
    for handler in &function.handler_binds {
        refs.extend(handler.bindings.iter().map(|(_, form)| *form));
    }
    for restart in &function.restart_cases {
        for entry in &restart.restarts {
            refs.extend(bytecode_references_with_params(
                &entry.function,
                include_params,
            ));
        }
    }
    for nested in &function.nested_functions {
        refs.extend(bytecode_references_with_params(nested, include_params));
    }
    refs
}

struct Plan {
    builtins: std::collections::BTreeSet<String>,
    unreachable_macro_callbacks: Vec<u64>,
    reachable_functions: Vec<u32>,
    unreachable_setf_writers: Vec<String>,
    unreachable_macros: Vec<(u32, String)>,
    generic_candidates: BTreeMap<String, u64>,
    retained_generics: BTreeMap<String, String>,
    live_clos_definitions: HashSet<u64>,
    unreachable_source_closures: Vec<u64>,
    unreachable_closures: Vec<u32>,
    capability_roots: std::collections::BTreeSet<String>,
    walker_roots: std::collections::BTreeSet<String>,
    capabilities: std::collections::BTreeSet<String>,
    candidates: BTreeMap<u32, String>,
    retained: BTreeMap<u32, String>,
}

// The ordinary evaluator scanner roots the source closure registry during GC.
// Delivery replaces that scanner with ownership edges, so its temporary Env
// root must not independently root the whole registry again.
struct DeliveryEnvironment<'a>(&'a mut Env);

impl egcl_rt::gc::TraceHostRoots for DeliveryEnvironment<'_> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.0.visit_delivery_roots(visit);
    }
}

fn analyze(spec: &Spec, entry: u32, keeps: &[u32], env: &mut Env) -> Result<Plan, EgclError> {
    let mut package_names = HashSet::new();
    for name in &spec.packages {
        if name == "*" {
            package_names.insert(name.clone());
            continue;
        }
        let package = egcl_stdlib::packages::find_package(name)
            .ok_or_else(|| error(format!("unknown prune-package {name}")))?;
        let canonical = egcl_stdlib::packages::package_name(package).unwrap();
        if canonical == "KEYWORD" {
            return Err(error(format!("cannot prune runtime package {canonical}")));
        }
        package_names.insert(canonical);
    }
    egcl_rt::gc::with_heap_snapshot(|| {
        generics::discard_compiled_method_sources();
        let mut candidates = BTreeMap::new();
        let mut functions = HashMap::new();
        symbols::for_each_bound_function(|index, _, function| {
            functions.insert(index, function);
            if definition_selected(index, &package_names) {
                candidates.insert(index, qualified_name(index));
            }
        });
        let generic_definitions = generics::GenericDefinitions::discover(env, &package_names);
        let macro_definitions = macros::MacroDefinitions::discover(env, &package_names);
        candidates.extend(macro_definitions.candidates.clone());
        let writer_definitions = writers::WriterDefinitions::discover(&package_names, &functions);
        candidates.extend(writer_definitions.candidates.clone());
        let dynamic_roots: Vec<_> = candidates
            .keys()
            .map(|&index| EgclVal::from_symbol_index(index))
            .chain(
                generic_definitions
                    .candidates
                    .values()
                    .map(|&id| EgclVal::from_raw(id)),
            )
            .collect();
        let indices = candidates.keys().copied().collect();
        let mut exposed_roots = symbols::delivery_roots(&indices);
        let mut roots = Vec::new();
        // SAFETY: this entire analysis runs under with_heap_snapshot and makes
        // no Lisp allocations. The omitted scanner is replaced below.
        unsafe {
            egcl_rt::gc::visit_delivery_host_roots(
                &[
                    bytecode::delivery_root_scanner(),
                    egcl_stdlib::hashtable::delivery_root_scanner(),
                    egcl_stdlib::clos::delivery_root_scanner(),
                    scan_evaluator_global_roots,
                ],
                &mut |slot| roots.push((*slot, "runtime host registry")),
            );
        }
        scan_evaluator_roots(
            &mut |slot| roots.push((unsafe { *slot }, "evaluator registry")),
            false,
        );
        env.visit_delivery_roots(&mut |slot| {
            roots.push((unsafe { *slot }, "delivery environment"))
        });
        egcl_stdlib::clos::visit_delivery_roots(&mut |slot| {
            roots.push((unsafe { *slot }, "retained CLOS class state"))
        });
        roots.extend(
            generic_definitions
                .roots
                .iter()
                .map(|&value| (value, "retained generic or class accessor")),
        );
        roots.extend(
            macro_definitions
                .roots
                .iter()
                .map(|&value| (value, "retained macro registry")),
        );
        roots.extend(
            writer_definitions
                .roots
                .iter()
                .map(|&value| (value, "unresolved SETF writer registry")),
        );
        // The runtime invokes these protocols by Rust-side names. All class
        // metadata remains retained, so preserve their installed methods even
        // when application bytecode never names the generic explicitly.
        for name in [
            "PRINT-OBJECT",
            "INITIALIZE-INSTANCE",
            "SHARED-INITIALIZE",
            "STREAM-READ-CHAR",
            "STREAM-UNREAD-CHAR",
            "STREAM-READ-BYTE",
            "STREAM-WRITE-CHAR",
            "STREAM-WRITE-BYTE",
            "STREAM-READ-LINE",
            "STREAM-WRITE-STRING",
            "STREAM-TERPRI",
            "STREAM-FRESH-LINE",
            "STREAM-FINISH-OUTPUT",
            "STREAM-FORCE-OUTPUT",
            "STREAM-CLEAR-OUTPUT",
        ] {
            if let Some(&handle) = generic_definitions.names.get(name) {
                roots.push((handle, "implicit runtime protocol"));
            }
            if let Some(index) = reader::find_symbol_index(name) {
                roots.push((
                    EgclVal::from_symbol_index(index),
                    "implicit runtime protocol",
                ));
            }
        }
        let edges = bytecode::delivery_dependencies();
        let compiled = if spec.specialized {
            bytecode::delivery_walker_dependencies()
        } else {
            HashMap::new()
        };
        for function in LOADED_COMPILER_MACRO_FUNCTIONS
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            exposed_roots.extend(bytecode_references(&function.lock().unwrap()));
        }
        let mut queue = VecDeque::new();
        queue.push_back((
            EgclVal::from_symbol_index(entry),
            "entry point".to_owned(),
            true,
        ));
        for &index in keeps {
            queue.push_back((
                EgclVal::from_symbol_index(index),
                "explicit keep".to_owned(),
                true,
            ));
        }
        if !spec.explicit_dynamic_roots || spec.runtime_keep.contains("dynamic-code") {
            for &value in &dynamic_roots {
                queue.push_back((
                    value,
                    if spec.explicit_dynamic_roots {
                        "runtime-keep = dynamic-code"
                    } else {
                        "dynamic = preserve"
                    }
                    .to_owned(),
                    true,
                ));
            }
        }
        for (value, reason) in roots {
            queue.push_back((value, reason.to_owned(), false));
        }
        for value in exposed_roots {
            queue.push_back((
                value,
                "persistent data or runtime registry".to_owned(),
                true,
            ));
        }
        let mut capability_roots = std::collections::BTreeSet::new();
        let mut walker_roots = std::collections::BTreeSet::new();
        let mut builtins = std::collections::BTreeSet::new();
        let mut capabilities = spec.runtime_keep.clone();
        if !spec.explicit_dynamic_roots {
            capabilities.extend(
                crate::runtime_contract::CAPABILITIES
                    .iter()
                    .map(|s| (*s).to_owned()),
            );
        }
        let mut retained = BTreeMap::new();
        let mut retained_generics = BTreeMap::new();
        let candidate_generic_names: HashMap<_, _> = generic_definitions
            .candidates
            .iter()
            .map(|(name, &id)| (id, name))
            .collect();
        let mut visited = HashSet::new();
        let mut reached_source_closures = HashSet::new();
        // Runtime metadata (e.g. a class named VECTOR) is not itself a call
        // to that builtin. Values also exposed through code, Lisp globals or
        // captures must be revisited in that role, regardless of visit order.
        let mut visited_roles = HashSet::new();
        // A function stored under an alias must retain the named definition and
        // its bytecode even if its heap representation has no useful name.
        let mut owners: HashMap<u64, Vec<u32>> = HashMap::new();
        for (&index, function) in &functions {
            owners.entry(function.0).or_default().push(index);
        }
        while let Some((value, reason, callable_data)) = queue.pop_front() {
            if !visited_roles.insert((value.0, callable_data)) {
                continue;
            }
            visited.insert(value.0);
            if let Some(&owner) = macro_definitions.callback_owners.get(&value.0) {
                queue.push_back((EgclVal::from_symbol_index(owner), reason.clone(), true));
            }
            if let Some(name) = candidate_generic_names.get(&value.0) {
                retained_generics.insert((*name).clone(), reason.clone());
            }
            if let Some((name, _)) = generics::function_name(value) {
                if let Some(writers) = writer_definitions.names.get(&name) {
                    for &writer in writers {
                        queue.push_back((
                            EgclVal::from_symbol_index(writer),
                            reason.clone(),
                            true,
                        ));
                    }
                }
                if let Some(&handle) = generic_definitions.names.get(&name) {
                    queue.push_back((handle, reason.clone(), false));
                }
            }
            if let Some(refs) = generic_definitions.edges.get(&value.0) {
                for &(child, callable) in refs {
                    queue.push_back((child, reason.clone(), callable));
                }
            }
            if egcl_rt::function::is_interpreted_function(value) {
                let body = egcl_rt::function::body(value);
                if !body.is_nil() {
                    queue.push_back((body, reason.clone(), true));
                }
            }
            if spec.specialized {
                if let Some(reason) = generic_definitions.source_methods.get(&value.0) {
                    walker_roots.insert(reason.clone());
                }
            }
            if is_closure_cons(value) {
                let id = cp(value).1.as_fixnum() as u64;
                if reached_source_closures.insert(id) {
                    if let Some(closure) = closure_registry().borrow().get(&id) {
                        queue.push_back((closure.params_form, reason.clone(), false));
                        queue.push_back((closure.body, reason.clone(), true));
                        let mut state = EnvRootVisitState::default();
                        let mut visit = |slot: *mut EgclVal| {
                            // SAFETY: registry-owned slots stay live throughout
                            // this stopped-world, nonallocating analysis.
                            queue.push_back((unsafe { *slot }, reason.clone(), true));
                        };
                        visit_env_frame_roots(&closure.captured_frame, &mut state, &mut visit);
                        if let Some(funs) = &closure.captured_funs {
                            visit_fun_map_roots(funs, &mut state, &mut visit);
                        }
                    }
                }
            }
            if spec.specialized {
                if egcl_rt::function::is_interpreted_function(value) {
                    let name = egcl_rt::function::name(value);
                    let reason = if !egcl_rt::function::body(value).is_nil() {
                        Some("source function body")
                    } else {
                        match name.symbol_index().and_then(|index| compiled.get(&index)) {
                            Some(reason) => *reason,
                            None => Some("function without saved bytecode"),
                        }
                    };
                    if let Some(reason) = reason {
                        let name = name
                            .symbol_index()
                            .map(qualified_name)
                            .unwrap_or_else(|| "anonymous function".to_owned());
                        walker_roots.insert(format!("{name}: {reason}"));
                    }
                }
                if value.is_cons() {
                    let head = cp(value).0;
                    if head.is_symbol() && sym_bare_name_rc(head).as_ref() == "CLOSURE" {
                        walker_roots.insert("source closure".to_owned());
                    }
                }
            }
            if spec.specialized && value.is_cons() {
                let head = cp(value).0;
                // A saved lambda has a known body, traced below like other
                // source forms. It needs the evaluator, but only actual open-
                // code operations within that body retain the whole world.
                if head.is_symbol() && sym_bare_name_rc(head).as_ref() == "LAMBDA" {
                    walker_roots.insert("saved source lambda".into());
                }
            }
            if let Some(index) = value.symbol_index() {
                if callable_data {
                    // These evaluator paths synthesize helper calls; their
                    // source forms don't contain the helpers' names.
                    let helper = match sym_bare_name_rc(value).as_ref() {
                        "SETF" => Some("EGCL::%SETF-VALUES"),
                        "DEFINE-CONDITION" => Some("%DEFINE-CONDITION-READER-DEFS"),
                        _ => None,
                    };
                    if let Some(helper) = helper.and_then(reader::find_symbol_index) {
                        queue.push_back((
                            EgclVal::from_symbol_index(helper),
                            format!("{} -> implicit source helper", qualified_name(index)),
                            true,
                        ));
                    }
                }
                if spec.specialized && !is_keyword_arg(value) && value != NIL && value != T {
                    if let Some(Some(reason)) = compiled.get(&index) {
                        walker_roots.insert(format!("{}: {reason}", qualified_name(index)));
                    }
                    // Bound functions are checked through their actual object
                    // and code, including aliases. Unbound runtime names can
                    // designate native builtins through FUNCALL or saved data.
                    if callable_data && !functions.contains_key(&index) {
                        let name = symbols::symbol_name(index).unwrap_or_default();
                        let runtime_symbol = is_builtin_function(&name)
                            || name.starts_with("EGCL")
                            || symbols::symbol_package(index)
                                .and_then(egcl_stdlib::packages::package_name)
                                .is_some_and(|package| package == "COMMON-LISP");
                        if runtime_symbol {
                            builtins.insert(name.clone());
                            // Evaluated extension dispatch normalizes these aliases.
                            for package in ["EGCL-EXT", "EGCL-INTERNAL"] {
                                if let Some(rest) = name.strip_prefix(&format!("{package}::")) {
                                    builtins.insert(format!("{package}:{rest}"));
                                }
                            }
                        }
                        if runtime_symbol && !builtin_without_source_evaluation(&name) {
                            walker_roots.insert(format!(
                                "{}: builtin evaluator dependency",
                                qualified_name(index)
                            ));
                        }
                    }
                }
                if !is_keyword_arg(value)
                    && symbols::symbol_name(index).is_some_and(|name| {
                        crate::runtime_contract::opens_code_world(
                            name.rsplit(':').next().unwrap_or(&name),
                        )
                    })
                {
                    if capabilities.insert("dynamic-code".into()) {
                        for &candidate in &dynamic_roots {
                            queue.push_back((
                                candidate,
                                format!(
                                    "reachable {} can invoke arbitrary code",
                                    qualified_name(index)
                                ),
                                true,
                            ));
                        }
                    }
                    capability_roots.insert(qualified_name(index));
                }
                if !is_keyword_arg(value)
                    && symbols::symbol_name(index)
                        .is_some_and(|name| name.rsplit(':').next() == Some("DISASSEMBLE"))
                {
                    capabilities.insert("disassembly".to_owned());
                }
                let next_reason = if let Some(name) = candidates.get(&index) {
                    retained.insert(index, reason.clone());
                    format!("{reason} -> {name}")
                } else {
                    reason.clone()
                };
                if let Some(refs) = macro_definitions.edges.get(&index) {
                    if spec.specialized && macro_definitions.source.contains(&index) {
                        walker_roots.insert(format!("{}: source macro", qualified_name(index)));
                    }
                    for &value in refs {
                        queue.push_back((value, next_reason.clone(), true));
                    }
                }
                if let Some(function) = functions.get(&index) {
                    queue.push_back((*function, next_reason.clone(), true));
                }
                if let Some(refs) = writer_definitions.edges.get(&index) {
                    for &value in refs {
                        queue.push_back((value, next_reason.clone(), true));
                    }
                }
                if let Some(refs) = edges.get(&index) {
                    for &(value, callable_data) in refs {
                        queue.push_back((value, next_reason.clone(), callable_data));
                    }
                }
            }
            if let Some(indices) = owners.get(&value.0) {
                for &index in indices {
                    queue.push_back((EgclVal::from_symbol_index(index), reason.clone(), true));
                }
            }
            if egcl_stdlib::hashtable::hash_table_p(value) {
                // Entries are off-heap. Package membership tables are not roots;
                // tables reached through application data retain both halves,
                // conservatively including weak entries.
                for (key, child) in egcl_stdlib::hashtable::hash_table_entries(value)
                    .expect("hash_table_p recognized a live table")
                {
                    queue.push_back((key, reason.clone(), callable_data));
                    queue.push_back((child, reason.clone(), callable_data));
                }
            }
            // SAFETY: values came from live roots, registered bodies, or the
            // GC's precise field visitor, all under the same stopped world.
            unsafe {
                // A callable's name and lambda list are metadata. Executable
                // operands/defaults are classified by its registered bytecode;
                // source bodies have already retained the walker above.
                let child_callable_data =
                    callable_data && !egcl_rt::function::is_interpreted_function(value);
                egcl_rt::gc::visit_delivery_references(value, &mut |child| {
                    queue.push_back((child, reason.clone(), child_callable_data))
                });
            }
        }
        if !walker_roots.is_empty() {
            capabilities.insert("tree-walker".into());
        }
        crate::runtime_contract::close_capabilities(&mut capabilities);
        let mut unreachable_closures: Vec<_> = edges
            .keys()
            .copied()
            .filter(|&symbol| {
                symbols::is_uninterned(symbol)
                    && !visited.contains(&EgclVal::from_symbol_index(symbol).0)
            })
            .collect();
        unreachable_closures.sort_unstable();
        let mut unreachable_source_closures: Vec<_> = closure_registry()
            .borrow()
            .keys()
            .filter(|id| !reached_source_closures.contains(id))
            .copied()
            .collect();
        unreachable_source_closures.sort_unstable();
        Plan {
            builtins,
            unreachable_macro_callbacks: macro_definitions
                .callback_owners
                .iter()
                .filter(|(_, index)| {
                    macro_definitions.candidates.contains_key(index)
                        && !retained.contains_key(index)
                })
                .map(|(&handle, _)| handle)
                .collect(),
            reachable_functions: functions
                .iter()
                .filter(|(_, function)| visited.contains(&function.0))
                .map(|(&index, _)| index)
                .collect(),
            unreachable_setf_writers: writer_definitions
                .records
                .into_iter()
                .filter(|(_, writers)| {
                    writers.iter().all(|index| {
                        writer_definitions.candidates.contains_key(index)
                            && !retained.contains_key(index)
                    })
                })
                .map(|(name, _)| name)
                .collect(),
            unreachable_macros: macro_definitions
                .names
                .into_iter()
                .filter(|(index, _)| {
                    macro_definitions.candidates.contains_key(index)
                        && !retained.contains_key(index)
                })
                .collect(),
            live_clos_definitions: generic_definitions
                .handles
                .iter()
                .filter(|key| visited.contains(key))
                .copied()
                .collect(),
            generic_candidates: generic_definitions.candidates,
            retained_generics,
            unreachable_source_closures,
            unreachable_closures,
            capability_roots,
            walker_roots,
            capabilities,
            candidates,
            retained,
        }
    })
}

fn prepare_source_functions(
    spec: &Spec,
    entry: u32,
    keeps: &[u32],
    env: &mut Env,
) -> Result<Plan, EgclError> {
    let mut plan = analyze(spec, entry, keeps, env)?;
    if !spec.specialized {
        return Ok(plan);
    }
    let mut attempted = HashSet::new();
    loop {
        let mut changed = false;
        // Stable symbol indices survive compilation and any moving collection.
        let mut functions = plan.reachable_functions.clone();
        functions.sort_unstable();
        for index in functions {
            if !attempted.insert(index) {
                continue;
            }
            let Some(function) = symbols::symbol_function(index)
                .filter(|&value| egcl_rt::function::is_interpreted_function(value))
            else {
                continue;
            };
            egcl_rt::rooted!(function = function);
            if egcl_rt::function::name(*function).symbol_index() != Some(index)
                || egcl_rt::function::body(*function).is_nil()
                || bytecode::closure_captured_env(*function).is_some()
            {
                continue;
            }
            egcl_rt::rooted!(params = egcl_rt::function::lambda_list(*function));
            egcl_rt::rooted!(body = egcl_rt::function::body(*function));
            let name = symbols::symbol_name(index).unwrap_or_default();
            // Even a declined compilation can expand macros and mutate the
            // restored world. Recompute liveness after every compilation pass.
            changed = true;
            if !bytecode::lazy_compile_defun(index, &name, *params, *body, env)
                || symbols::symbol_function(index) != Some(*function)
                || bytecode::delivery_walker_dependencies().get(&index) != Some(&None)
            {
                continue;
            }
            egcl_rt::gc::with_heap_snapshot(|| {
                // SAFETY: this is the same live function compiled above, with
                // independently restorable bytecode and no captured frame.
                // The stopped heap protects its source fields from readers.
                unsafe {
                    egcl_rt::function::redefine(*function, *params, NIL, NIL);
                }
            })?;
        }
        if !changed {
            return Ok(plan);
        }
        // Macro expansion and newly compiled call edges can change liveness.
        plan = analyze(spec, entry, keeps, env)?;
    }
}

/// Audited native entry points with complete evaluated-argument dispatch.
/// This is an implementation dependency catalog, not a delivery allowlist:
/// unknown handlers retain the walker until their source dependencies are
/// removed. Lisp callbacks remain edges through the saved object/code graph.
fn builtin_without_source_evaluation(name: &str) -> bool {
    matches!(
        name,
        "+" | "-"
            | "*"
            | "/"
            | "1+"
            | "1-"
            | "="
            | "/="
            | "<"
            | ">"
            | "<="
            | ">="
            | "CAR"
            | "FIRST"
            | "CDR"
            | "REST"
            | "CONS"
            | "CONSP"
            | "ATOM"
            | "LISTP"
            | "NULL"
            | "NOT"
            | "APPEND"
            | "REVERSE"
            | "ENDP"
            | "EQ"
            | "VALUES"
            | "VALUES-LIST"
            | "FUNCALL"
            | "APPLY"
            | "WRITE-LINE"
    )
}

pub(super) fn run(args: &CliArgs, env: &mut Env) -> Result<i32, EgclError> {
    let mut delivery_env = DeliveryEnvironment(env);
    egcl_rt::rooted_ref!(_env_root = &mut delivery_env);
    let env = &mut *delivery_env.0;
    let input = Path::new(args.image.as_ref().unwrap());
    let spec_path = Path::new(args.deliver.as_ref().unwrap());
    let output = Path::new(args.output.as_ref().unwrap());
    let manifest_path = PathBuf::from(format!("{}.manifest", output.display()));
    for destination in [output, manifest_path.as_path()] {
        if destination.is_dir() {
            return Err(error(format!(
                "output is a directory: {}",
                destination.display()
            )));
        }
        for source in [input, spec_path] {
            if std::fs::canonicalize(destination)
                .ok()
                .is_some_and(|p| Some(p) == std::fs::canonicalize(source).ok())
            {
                return Err(error(
                    "output and manifest must not replace the input image or specification",
                ));
            }
        }
    }
    let text = std::fs::read_to_string(spec_path)
        .map_err(|e| error(format!("read specification: {e}")))?;
    let spec = Spec::parse(&text)?;
    let entry = function_index(&spec.entry)?;
    let keeps = spec
        .keep
        .iter()
        .map(|name| definition_index(name, true))
        .collect::<Result<Vec<_>, _>>()?;
    // This process is disposable. Override the input entry without invoking it;
    // the original on-disk image is never written.
    let top = symbols::intern(IMAGE_TOPLEVEL_VAR);
    symbols::set_symbol_value(top, EgclVal::from_symbol_index(entry));
    egcl_stdlib::pathnames::clear_delivery_string_caches()?;
    let plan = prepare_source_functions(&spec, entry, &keeps, env)?;
    let mut selected = native_runtime::contract();
    if !spec.specialized
        && (selected.builtins.is_some()
            || selected.max_tier != crate::runtime_contract::NativeTier::T2)
    {
        return Err(error("runtime = full requires a full delivery driver"));
    }
    if !spec.specialized
        && crate::runtime_contract::CAPABILITIES
            .iter()
            .any(|name| !selected.capabilities.contains(*name))
    {
        return Err(error("runtime = full requires a full delivery driver"));
    }
    if spec.specialized {
        selected.max_tier = spec.max_tier;
        selected.capabilities = plan.capabilities.clone();
        // Source evaluation still has implicit builtin calls. Keep its complete
        // dispatch until those dependencies are expressed as graph edges.
        selected.builtins = if plan.capabilities.contains("tree-walker") {
            None
        } else {
            Some(plan.builtins.clone())
        };
    }
    native_runtime::contract()
        .accepts(&selected)
        .map_err(error)?;
    let input_size = std::fs::metadata(input)
        .map_err(|e| error(e.to_string()))?
        .len();
    let mut header = [0u8; 12];
    std::fs::File::open(input)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|e| error(e.to_string()))?;
    // The core was validated by load_core_image_bytes before delivery started.
    let image_version = u32::from_ne_bytes(header[8..12].try_into().unwrap());
    let mut report = format!(
        "egcl-delivery-manifest = 1\nruntime = {}\negcl-version = {}\nplatform-tag = {}\ninput-bytes = {input_size}\nentry = {}\ndynamic = {}\n",
        if spec.specialized {
            "specialized"
        } else {
            "full"
        },
        env!("CARGO_PKG_VERSION"),
        egcl_rt::image::current_platform_tag(),
        spec.entry,
        if spec.explicit_dynamic_roots {
            "explicit"
        } else {
            "preserve"
        }
    );
    report.push_str(&format!(
        "native-contract-begin\n{}native-contract-end\n",
        selected.encode()
    ));
    for root in &plan.capability_roots {
        report.push_str(&format!(
            "native-root = {root} -> dynamic-code -> all native capabilities\n"
        ));
    }
    for root in &plan.walker_roots {
        report.push_str(&format!("native-root = {root} -> tree-walker\n"));
    }
    report.push_str(&format!("input-image-format = {image_version}\n"));
    report.push_str(&format!(
        "private-code-removed = {}\n",
        plan.unreachable_closures.len()
    ));
    report.push_str(&format!(
        "source-closures-removed = {}\n",
        plan.unreachable_source_closures.len()
    ));
    for package in &spec.packages {
        report.push_str(&format!("prune-package = {package}\n"));
    }
    for capability in &spec.runtime_keep {
        report.push_str(&format!("explicit-native-root = {capability}\n"));
    }
    for name in &spec.keep {
        report.push_str(&format!("explicit-keep = {name}\n"));
    }
    let mut rows: Vec<_> = plan.candidates.iter().collect();
    rows.sort_by(|a, b| a.1.cmp(b.1));
    for (index, name) in rows {
        if let Some(reason) = plan.retained.get(index) {
            report.push_str(&format!("keep {name}: {reason}\n"));
        } else {
            report.push_str(&format!(
                "remove {name}: unreachable within declared delivery policy\n"
            ));
        }
    }
    for name in plan.generic_candidates.keys() {
        if let Some(reason) = plan.retained_generics.get(name) {
            report.push_str(&format!("keep {name}: {reason}\n"));
        } else {
            report.push_str(&format!("remove {name}: unreachable generic function\n"));
        }
    }
    if args.dry_run {
        print!("{report}");
        return Ok(0);
    }
    // Reserve both outputs before changing the restored world. A failed build
    // leaves any previous executable untouched.
    let executable_stage = TemporaryFile::new(output).map_err(|e| error(e.to_string()))?;
    let manifest_stage = TemporaryFile::new(&manifest_path).map_err(|e| error(e.to_string()))?;
    let mut runtime = if spec.specialized {
        eprintln!(";; building matching native runtime (cached Cargo release build)");
        native_runtime::build(args.runtime_source.as_deref(), &selected)?
    } else {
        current_runtime_bytes().map_err(|e| error(e.to_string()))?
    };
    remove_embedded_images(&mut runtime).map_err(|e| error(e.to_string()))?;
    let runtime_bytes = runtime.len();
    for &index in plan
        .candidates
        .keys()
        .filter(|index| !plan.retained.contains_key(index))
    {
        symbols::set_symbol_function(index, egcl_rt::value::UNBOUND);
        bytecode::clear_lazy_state(index);
        bytecode::clear_closure_env(index);
    }
    for &symbol in &plan.unreachable_closures {
        bytecode::remove_delivery_closure(symbol);
    }
    for id in &plan.unreachable_source_closures {
        closure_registry().borrow_mut().remove(id);
    }
    generics::retain(env, &plan.live_clos_definitions);
    macros::retain(env, &plan.unreachable_macros);
    for &handle in &plan.unreachable_macro_callbacks {
        compiler_macroexpand::unregister_macro_function(EgclVal(handle));
    }
    writers::remove(&plan.unreachable_setf_writers);
    native_runtime::with_save_contract(&selected, || {
        save_core(
            executable_stage
                .path
                .to_str()
                .ok_or_else(|| error("output path is not UTF-8"))?,
            false,
            true,
            env,
        )
    })?;
    let core = std::fs::read(&executable_stage.path).map_err(|e| error(e.to_string()))?;
    report.push_str(&format!(
        "native-bytes = {runtime_bytes}\nimage-bytes = {}\n",
        core.len()
    ));
    runtime.extend_from_slice(&core);
    runtime.extend_from_slice(EXE_IMAGE_MAGIC);
    runtime.extend_from_slice(&(core.len() as u64).to_le_bytes());
    write_atomic(&executable_stage.path, &runtime, true).map_err(|e| error(e.to_string()))?;
    report.push_str(&format!(
        "output-bytes = {}\n",
        std::fs::metadata(&executable_stage.path)
            .map_err(|e| error(e.to_string()))?
            .len()
    ));
    write_atomic(&manifest_stage.path, report.as_bytes(), false)
        .map_err(|e| error(e.to_string()))?;
    let previous_manifest = match std::fs::read(&manifest_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(error(format!("read previous manifest: {e}"))),
    };
    std::fs::rename(&manifest_stage.path, &manifest_path)
        .map_err(|e| error(format!("publish manifest: {e}")))?;
    if let Err(e) = std::fs::rename(&executable_stage.path, output) {
        let rollback = match previous_manifest {
            Some(bytes) => write_atomic(&manifest_path, &bytes, false),
            None => std::fs::remove_file(&manifest_path),
        };
        if let Err(rollback) = rollback {
            return Err(error(format!(
                "publish executable: {e}; restoring previous manifest also failed: {rollback}"
            )));
        }
        return Err(error(format!("publish executable: {e}")));
    }
    print!("{report}");
    Ok(0)
}

/// Drop all historical payloads, including executables produced by older
/// runtimes which appended new images without removing the preceding one.
pub(super) fn remove_embedded_images(bytes: &mut Vec<u8>) -> io::Result<()> {
    while bytes.len() >= 16 && &bytes[bytes.len() - 16..bytes.len() - 8] == EXE_IMAGE_MAGIC {
        let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
        let size = usize::try_from(size)
            .map_err(|_| io::Error::other("embedded image length overflow"))?;
        let start = bytes
            .len()
            .checked_sub(16)
            .and_then(|n| n.checked_sub(size))
            .ok_or_else(|| io::Error::other("invalid embedded image length"))?;
        if start == 0 || size == 0 {
            return Err(io::Error::other("empty runtime or embedded image"));
        }
        bytes.truncate(start);
    }
    Ok(())
}

pub(super) struct TemporaryFile {
    pub(super) path: PathBuf,
}
impl TemporaryFile {
    pub(super) fn new(destination: &Path) -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut name = destination
                .file_name()
                .ok_or_else(|| io::Error::other("output must name a file"))?
                .to_os_string();
            name.push(format!(".delivery-{}-{serial}.tmp", std::process::id()));
            let path = destination.with_file_name(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
    let temporary = TemporaryFile::new(path)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&temporary.path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary.path, path)
}
