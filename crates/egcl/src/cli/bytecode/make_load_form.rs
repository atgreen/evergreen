// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Deferred instance constants. The load plan creates objects after preceding
//! definitions, patches only their pending graph edges, and initializes them
//! before publishing a function or evaluating a form that consumes them.
use super::super::quote_sym;
use super::*;
use std::collections::HashSet;

type Action = (u8, u8, u32, u32, u32);
pub(super) const CREATE: u8 = 13;
pub(super) const INITIALIZE: u8 = 14;

#[derive(Default)]
pub(super) struct Registry {
    records: Vec<Record>,
}
struct Record {
    object: EgclVal,
    reference: u32,
    creation: EgclVal,
    initialization: EgclVal,
    prepared: Option<Prepared>,
    created: bool,
    initialized: bool,
}
#[derive(Clone)]
struct Prepared {
    creation: u32,
    initialization: u32,
    creation_deps: Vec<u32>,
    initialization_deps: Vec<u32>,
}
impl egcl_rt::gc::TraceHostRoots for Registry {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for record in &mut self.records {
            visit(&mut record.object);
            visit(&mut record.creation);
            visit(&mut record.initialization);
        }
    }
}
impl Registry {
    pub(super) fn len(&self) -> usize {
        self.records.len()
    }
    pub(super) fn truncate(&mut self, count: usize) {
        self.records.truncate(count);
    }
    fn index(&self, reference: u32) -> Result<usize, EgclError> {
        self.records
            .iter()
            .position(|r| r.reference == reference)
            .ok_or_else(|| bbu_error("missing MAKE-LOAD-FORM identity"))
    }
}
impl BbuConstPool {
    pub(super) fn reserve_load_form(&mut self, object: EgclVal) -> u32 {
        if let Some(record) = self.load_forms.records.iter().find(|r| r.object == object) {
            return record.reference;
        }
        // Identity, not equal creation bytes: distinct instances remain distinct.
        let reference = self.entries.len() as u32;
        self.entries.push(vec![30]);
        self.load_forms.records.push(Record {
            object,
            reference,
            creation: NIL,
            initialization: NIL,
            prepared: None,
            created: false,
            initialized: false,
        });
        reference
    }
    fn encoded_constants(&self) -> Result<Vec<BbuConstant>, EgclError> {
        self.entries
            .iter()
            .map(|bytes| {
                let mut cursor = BbuCursor::new(bytes);
                let constant = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION)?;
                if cursor.pos != bytes.len() {
                    return Err(bbu_error("invalid constant encoding"));
                }
                Ok(constant)
            })
            .collect()
    }
}

fn children(constant: &BbuConstant) -> Vec<u32> {
    match constant {
        BbuConstant::Cons(a, b) | BbuConstant::Ratio(a, b) => vec![*a, *b],
        BbuConstant::Vector(refs)
        | BbuConstant::HashTable {
            entry_refs: refs, ..
        }
        | BbuConstant::MdArray {
            element_refs: refs, ..
        } => refs.clone(),
        BbuConstant::MdStorage { storage_ref, .. }
        | BbuConstant::ComplexArray { storage_ref, .. }
        | BbuConstant::MdPackedU8 { storage_ref, .. } => vec![*storage_ref],
        _ => Vec::new(),
    }
}
fn reachable(constants: &[BbuConstant], roots: &[u32]) -> Result<Vec<usize>, EgclError> {
    let mut pending = roots
        .iter()
        .copied()
        .filter(|r| *r != BBU_NO_INDEX)
        .collect::<Vec<_>>();
    let mut seen = vec![false; constants.len()];
    let mut result = Vec::new();
    while let Some(reference) = pending.pop() {
        let index = bbu_index(reference, constants.len(), "deferred constant graph")?;
        if seen[index] {
            continue;
        }
        seen[index] = true;
        result.push(index);
        pending.extend(children(&constants[index]));
    }
    Ok(result)
}
fn objects(constants: &[BbuConstant], roots: &[u32]) -> Result<Vec<u32>, EgclError> {
    Ok(reachable(constants, roots)?
        .into_iter()
        .filter(|&i| matches!(constants[i], BbuConstant::LoadFormObject))
        .map(|i| i as u32)
        .collect())
}
fn function_roots(function: &BbuFunction) -> Vec<u32> {
    let mut refs = function.literal_refs.clone();
    refs.extend([function.name_ref, function.lambda_list_ref]);
    refs
}
fn action_roots(action: Action) -> Vec<u32> {
    match action.0 {
        1 | 6 => vec![action.2, action.3, action.4],
        2 => vec![action.2, action.3],
        3 | 4 | 5 | 8 => vec![action.3, action.4],
        9 | 11 | 12 => vec![action.2],
        CREATE | INITIALIZE => vec![action.3],
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|&r| r != BBU_NO_INDEX)
    .collect()
}
fn prepare(pool: &mut BbuConstPool, reference: u32, env: &Env) -> Result<(), EgclError> {
    let index = pool.load_forms.index(reference)?;
    if pool.load_forms.records[index].prepared.is_some() {
        return Ok(());
    }
    // The identity was reserved before this hook. Do not borrow registry fields
    // while invoking Lisp: both the object and returned forms may relocate.
    egcl_rt::rooted!(object = pool.load_forms.records[index].object);
    egcl_rt::rooted!(quoted = vec_to_list(&[quote_sym(), *object]));
    let _environment_scope = super::super::MacroexpandEnvScope::new();
    egcl_rt::rooted!(macro_environment = super::super::macroexpand_environment_from_cli(env));
    egcl_rt::rooted!(
        environment = super::super::store_macroexpand_environment((*macro_environment).clone())
    );
    egcl_rt::rooted!(quoted_environment = vec_to_list(&[quote_sym(), *environment]));
    egcl_rt::rooted!(
        operator =
            resolve_sym("CL:MAKE-LOAD-FORM").ok_or_else(|| bbu_error("missing MAKE-LOAD-FORM"))?
    );
    egcl_rt::rooted!(call = vec_to_list(&[*operator, *quoted, *quoted_environment]));
    let mut hook_env = env.null_lexical_child();
    egcl_rt::rooted_ref!(_env_root = &mut hook_env);
    let (primary, values) = super::super::eval_form_collecting_values(*call, &mut hook_env)?;
    pool.load_forms.records[index].creation = values.first().copied().unwrap_or(primary);
    pool.load_forms.records[index].initialization = values.get(1).copied().unwrap_or(NIL);
    egcl_rt::rooted!(creation = pool.load_forms.records[index].creation);
    egcl_rt::rooted!(initialization = pool.load_forms.records[index].initialization);
    // Resolve lexical macros now, rather than looking them up again on LOAD.
    *creation = compiler_macroexpand::macroexpand_all(*creation, &macro_environment)?;
    *initialization = compiler_macroexpand::macroexpand_all(*initialization, &macro_environment)?;
    let creation_ref = pool
        .value(*creation)
        .ok_or_else(|| bbu_error("MAKE-LOAD-FORM creation is not externalizable"))?;
    let initialization_ref = pool
        .value(*initialization)
        .ok_or_else(|| bbu_error("MAKE-LOAD-FORM initialization is not externalizable"))?;
    let constants = pool.encoded_constants()?;
    pool.load_forms.records[index].prepared = Some(Prepared {
        creation: creation_ref,
        initialization: initialization_ref,
        creation_deps: objects(&constants, &[creation_ref])?,
        initialization_deps: objects(&constants, &[initialization_ref])?,
    });
    Ok(())
}

pub(super) fn plan(
    pool: &mut BbuConstPool,
    functions: &[BbuFunction],
    function_start: usize,
    actions: &[Action],
    env: &Env,
) -> Result<Vec<Action>, EgclError> {
    if pool.load_forms.records.iter().all(|r| r.initialized) {
        return Ok(Vec::new());
    }
    let mut roots = functions[function_start..]
        .iter()
        .flat_map(function_roots)
        .collect::<Vec<_>>();
    roots.extend(actions.iter().copied().flat_map(action_roots));
    let mut pending = objects(&pool.encoded_constants()?, &roots)?;
    let mut closure = Vec::new();
    while let Some(reference) = pending.pop() {
        if closure.contains(&reference) {
            continue;
        }
        closure.push(reference);
        prepare(pool, reference, env)?;
        let index = pool.load_forms.index(reference)?;
        let prepared = pool.load_forms.records[index].prepared.as_ref().unwrap();
        pending.extend(prepared.creation_deps.iter().copied());
        pending.extend(prepared.initialization_deps.iter().copied());
    }
    schedule(pool, &closure)
}

fn hashes_ready(
    constants: &[BbuConstant],
    roots: &[u32],
    created: impl Fn(u32) -> bool,
    initialized: impl Fn(u32) -> bool,
) -> Result<bool, EgclError> {
    for index in reachable(constants, roots)? {
        if let BbuConstant::HashTable { test, entry_refs } = &constants[index] {
            for object in objects(constants, entry_refs)? {
                if !if *test < 2 {
                    created(object)
                } else {
                    initialized(object)
                } {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn initialization_depends_on(
    pool: &BbuConstPool,
    reference: u32,
    owner: u32,
) -> Result<bool, EgclError> {
    let mut pending = pool.load_forms.records[pool.load_forms.index(reference)?]
        .prepared
        .as_ref()
        .unwrap()
        .initialization_deps
        .clone();
    let mut seen = HashSet::new();
    while let Some(reference) = pending.pop() {
        if reference == owner {
            return Ok(true);
        }
        if !seen.insert(reference) {
            continue;
        }
        let prepared = pool.load_forms.records[pool.load_forms.index(reference)?]
            .prepared
            .as_ref()
            .unwrap();
        pending.extend(prepared.creation_deps.iter().copied());
        pending.extend(prepared.initialization_deps.iter().copied());
    }
    Ok(false)
}

// Two nodes per object: creation then initialization. Independent initialization
// closures precede a dependent form; only initialization ordering edges involved
// in object cycles are omitted. Creation requirements are never omitted.
fn schedule(pool: &mut BbuConstPool, closure: &[u32]) -> Result<Vec<Action>, EgclError> {
    let constants = pool.encoded_constants()?;
    let positions = closure
        .iter()
        .enumerate()
        .map(|(i, &r)| (r, i))
        .collect::<HashMap<_, _>>();
    let mut dependencies = vec![HashSet::new(); closure.len() * 2];
    let mut done = vec![false; dependencies.len()];
    for (index, &reference) in closure.iter().enumerate() {
        let record = &pool.load_forms.records[pool.load_forms.index(reference)?];
        let prepared = record.prepared.as_ref().unwrap();
        done[index * 2] = record.created;
        done[index * 2 + 1] = record.initialized;
        dependencies[index * 2 + 1].insert(index * 2);
        for (phase, refs) in [
            (0, &prepared.creation_deps),
            (1, &prepared.initialization_deps),
        ] {
            let node = index * 2 + phase;
            for &other in refs {
                let other_node = positions[&other] * 2;
                dependencies[node].insert(other_node);
                if other != reference && !initialization_depends_on(pool, other, reference)? {
                    dependencies[node].insert(other_node + 1);
                }
            }
            let form = if phase == 0 {
                prepared.creation
            } else {
                prepared.initialization
            };
            for table in reachable(&constants, &[form])? {
                if let BbuConstant::HashTable { test, entry_refs } = &constants[table] {
                    for other in objects(&constants, entry_refs)? {
                        dependencies[node].insert(positions[&other] * 2 + usize::from(*test >= 2));
                    }
                }
            }
        }
    }
    let mut output = Vec::new();
    loop {
        if done.iter().all(|d| *d) {
            break;
        }
        // Initialize immediately whenever data flow permits it.
        let ready = (0..closure.len())
            .map(|i| i * 2 + 1)
            .chain((0..closure.len()).map(|i| i * 2))
            .find(|&node| !done[node] && dependencies[node].iter().all(|&d| done[d]));
        let Some(node) = ready else {
            return Err(bbu_error(
                "cyclic MAKE-LOAD-FORM creation or initialization dependency",
            ));
        };
        let reference = closure[node / 2];
        let index = pool.load_forms.index(reference)?;
        let prepared = pool.load_forms.records[index].prepared.as_ref().unwrap();
        let initialization = node % 2 == 1;
        output.push((
            if initialization { INITIALIZE } else { CREATE },
            0,
            reference,
            if initialization {
                prepared.initialization
            } else {
                prepared.creation
            },
            BBU_NO_INDEX,
        ));
        done[node] = true;
        if initialization {
            pool.load_forms.records[index].initialized = true;
        } else {
            pool.load_forms.records[index].created = true;
        }
    }
    Ok(output)
}

pub(super) fn discard_unused(pool: &mut BbuConstPool) {
    // Failed speculative lowerings can leave unused pool entries. They must not
    // invoke hooks or leave unpaired deferred declarations in the final unit.
    for record in &pool.load_forms.records {
        if !record.created {
            pool.entries[record.reference as usize] = vec![0];
        }
    }
}

pub(super) fn has_deferred(constants: &[BbuConstant], roots: &[u32]) -> Result<bool, EgclError> {
    Ok(!objects(constants, roots)?.is_empty())
}

pub(super) fn validate(
    constants: &[BbuConstant],
    functions: &[EncodedBbuFunction],
    actions: &[BbuLoadAction],
    version: u16,
) -> Result<(), EgclError> {
    if !constants
        .iter()
        .any(|c| matches!(c, BbuConstant::LoadFormObject))
        && !actions
            .iter()
            .any(|a| matches!(a.kind, CREATE | INITIALIZE))
    {
        return Ok(());
    }
    for (index, constant) in constants.iter().enumerate() {
        if let BbuConstant::HashTable { entry_refs, .. } = constant {
            if entry_refs.len() % 2 != 0 || entry_refs.iter().any(|&r| r as usize >= index) {
                return Err(bbu_error("hash table entry refers forward"));
            }
            for child in reachable(constants, entry_refs)? {
                if child >= index && matches!(constants[child], BbuConstant::HashTable { .. }) {
                    return Err(bbu_error("cyclic or forward hash table dependency"));
                }
            }
        }
    }
    let mut created = vec![false; constants.len()];
    let mut initialized = vec![false; constants.len()];
    for action in actions {
        if matches!(action.kind, CREATE | INITIALIZE) {
            if version < 0x0206 || action.flags != 0 || action.arg2 != BBU_NO_INDEX {
                return Err(bbu_error("invalid MAKE-LOAD-FORM action arguments"));
            }
            let target = bbu_index(action.arg0, constants.len(), "MAKE-LOAD-FORM target")?;
            if !matches!(constants[target], BbuConstant::LoadFormObject) {
                return Err(bbu_error("MAKE-LOAD-FORM target is not deferred"));
            }
            bbu_index(action.arg1, constants.len(), "MAKE-LOAD-FORM form")?;
            let dependencies = objects(constants, &[action.arg1])?;
            if dependencies.iter().any(|&r| !created[r as usize]) {
                return Err(bbu_error("cyclic or premature MAKE-LOAD-FORM dependency"));
            }
            if !hashes_ready(
                constants,
                &[action.arg1],
                |r| created[r as usize],
                |r| initialized[r as usize],
            )? {
                return Err(bbu_error("uninitialized MAKE-LOAD-FORM hash dependency"));
            }
            if action.kind == CREATE {
                if created[target] {
                    return Err(bbu_error("duplicate MAKE-LOAD-FORM creation"));
                }
                created[target] = true;
            } else {
                if !created[target] || initialized[target] {
                    return Err(bbu_error(
                        "duplicate or premature MAKE-LOAD-FORM initialization",
                    ));
                }
                initialized[target] = true;
            }
            continue;
        }
        let mut roots = action_roots((
            action.kind,
            action.flags,
            action.arg0,
            action.arg1,
            action.arg2,
        ));
        let function = match action.kind {
            3 | 4 | 5 | 7 | 8 | 10 => Some(action.arg0),
            11 => Some(action.arg1),
            _ => None,
        };
        if let Some(function) = function {
            let index = bbu_index(function, functions.len(), "function consumer")?;
            // Nested/restart records precede their owner. Including every earlier
            // record is conservative and covers all their metadata/defaults too.
            for function in &functions[..=index] {
                roots.extend(function.literal_refs.iter().copied());
                roots.extend([function.lambda_list_ref, function.name_ref]);
            }
        }
        if objects(constants, &roots)?
            .iter()
            .any(|&r| !initialized[r as usize])
        {
            return Err(bbu_error("consumer precedes MAKE-LOAD-FORM initialization"));
        }
    }
    for (i, constant) in constants.iter().enumerate() {
        if matches!(constant, BbuConstant::LoadFormObject) && (!created[i] || !initialized[i]) {
            return Err(bbu_error(
                "missing MAKE-LOAD-FORM creation or initialization",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Edge {
    Car,
    Cdr,
    Element(usize),
    MdElement(usize),
}
struct Fixup {
    owner: usize,
    edge: Edge,
    target: usize,
}
pub(super) struct Runtime {
    fixups: Vec<Fixup>,
    created: Vec<bool>,
    initialized: Vec<bool>,
    hash_done: Vec<bool>,
}
impl Runtime {
    pub(super) fn new(constants: &[BbuConstant]) -> Result<Self, EgclError> {
        let mut fixups = Vec::new();
        for (owner, constant) in constants.iter().enumerate() {
            let edges = match constant {
                BbuConstant::Cons(a, b) => vec![(Edge::Car, *a), (Edge::Cdr, *b)],
                BbuConstant::Vector(refs) => refs
                    .iter()
                    .enumerate()
                    .map(|(i, &r)| (Edge::Element(i), r))
                    .collect(),
                BbuConstant::MdArray { element_refs, .. } => element_refs
                    .iter()
                    .enumerate()
                    .map(|(i, &r)| (Edge::MdElement(i), r))
                    .collect(),
                _ => Vec::new(),
            };
            for (edge, target) in edges {
                let target = bbu_index(target, constants.len(), "deferred graph edge")?;
                if matches!(constants[target], BbuConstant::LoadFormObject) {
                    fixups.push(Fixup {
                        owner,
                        edge,
                        target,
                    });
                }
            }
        }
        let hash_done = constants
            .iter()
            .map(|c| match c {
                BbuConstant::HashTable { entry_refs, .. } => {
                    has_deferred(constants, entry_refs).map(|b| !b)
                }
                _ => Ok(true),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            fixups,
            created: vec![false; constants.len()],
            initialized: vec![false; constants.len()],
            hash_done,
        })
    }
    pub(super) fn apply_fixups(&self, target: usize, values: &[EgclVal]) -> Result<(), EgclError> {
        for fixup in self.fixups.iter().filter(|f| f.target == target) {
            match fixup.edge {
                Edge::Car => {
                    super::super::store_cons_field(values[fixup.owner], values[target], true)?
                }
                Edge::Cdr => {
                    super::super::store_cons_field(values[fixup.owner], values[target], false)?
                }
                Edge::Element(index) => {
                    egcl_stdlib::set_elt(values[fixup.owner], index, values[target])?;
                }
                Edge::MdElement(index) => {
                    let storage = egcl_rt::types::md_array_storage(values[fixup.owner])
                        .ok_or_else(|| bbu_error("missing deferred array storage"))?;
                    egcl_stdlib::set_elt(storage, index, values[target])?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn finish_creation(
        &mut self,
        target: usize,
        constants: &[BbuConstant],
        values: &[EgclVal],
    ) -> Result<(), EgclError> {
        self.created[target] = true;
        self.fill_ready_hashes(constants, values)
    }
    pub(super) fn finish_initialization(
        &mut self,
        target: usize,
        constants: &[BbuConstant],
        values: &[EgclVal],
    ) -> Result<(), EgclError> {
        self.initialized[target] = true;
        self.fill_ready_hashes(constants, values)
    }
    fn fill_ready_hashes(
        &mut self,
        constants: &[BbuConstant],
        values: &[EgclVal],
    ) -> Result<(), EgclError> {
        for (index, constant) in constants.iter().enumerate() {
            if self.hash_done[index] {
                continue;
            }
            let BbuConstant::HashTable { test, entry_refs } = constant else {
                continue;
            };
            let ready = objects(constants, entry_refs)?.iter().all(|&r| {
                if *test < 2 {
                    self.created[r as usize]
                } else {
                    self.initialized[r as usize]
                }
            });
            let nested_ready = if *test < 2 {
                true
            } else {
                reachable(constants, entry_refs)?.iter().all(|&i| {
                    !matches!(constants[i], BbuConstant::HashTable { .. }) || self.hash_done[i]
                })
            };
            if ready && nested_ready {
                for pair in entry_refs.chunks_exact(2) {
                    egcl_stdlib::set_gethash(
                        values[pair[0] as usize],
                        values[index],
                        values[pair[1] as usize],
                    )?;
                }
                self.hash_done[index] = true;
            }
        }
        Ok(())
    }
}

pub(super) fn instantiate(
    index: usize,
    templates: &[Arc<BytecodeFunction>],
    encoded: &[EncodedBbuFunction],
    nested: &[Vec<usize>],
    restart_tables: &[Vec<Vec<(u32, u32)>>],
    values: &[EgclVal],
) -> Arc<BytecodeFunction> {
    let mut needed = HashSet::new();
    let mut pending = vec![index];
    while let Some(index) = pending.pop() {
        if !needed.insert(index) {
            continue;
        }
        pending.extend(nested[index].iter().copied());
        pending.extend(
            restart_tables[index]
                .iter()
                .flatten()
                .map(|&(_, child)| child as usize),
        );
    }
    let mut instantiated = HashMap::<usize, Arc<BytecodeFunction>>::new();
    // Verified nested/restart references always point backwards.
    for index in 0..=index {
        if !needed.contains(&index) {
            continue;
        }
        let mut function = (*templates[index]).clone();
        function.constants = encoded[index]
            .literal_refs
            .iter()
            .map(|&r| values[r as usize])
            .collect();
        if encoded[index].lambda_list_ref != BBU_NO_INDEX {
            function.params_form = values[encoded[index].lambda_list_ref as usize];
        }
        function.nested_functions = nested[index]
            .iter()
            .map(|i| Arc::clone(&instantiated[i]))
            .collect();
        for (table, refs) in function
            .restart_cases
            .iter_mut()
            .zip(&restart_tables[index])
        {
            for (clause, &(_, child)) in table.restarts.iter_mut().zip(refs) {
                clause.function = Box::new((*instantiated[&(child as usize)]).clone());
            }
        }
        instantiated.insert(index, Arc::new(function));
    }
    Arc::clone(&instantiated[&index])
}

pub(super) fn validate_print_groups(
    constants: &[BbuConstant],
    actions: &[BbuLoadAction],
    groups: Option<&[BbuPrintGroup]>,
) -> Result<(), EgclError> {
    if !constants
        .iter()
        .any(|c| matches!(c, BbuConstant::LoadFormObject))
    {
        return Ok(());
    }
    for group in groups.into_iter().flatten().filter(|g| g.result_kind == 2) {
        let initialized = actions
            .iter()
            .take(group.end as usize)
            .filter(|a| a.kind == INITIALIZE)
            .map(|a| a.arg0)
            .collect::<HashSet<_>>();
        if objects(constants, &[group.result])?
            .iter()
            .any(|r| !initialized.contains(r))
        {
            return Err(bbu_error(
                "printed constant precedes MAKE-LOAD-FORM initialization",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
