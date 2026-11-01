// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap lexical environments for eligible compilation units (Backpack,
//! bliss-gg3i1.7.15.2/.4/.5).
//!
//! The lowerer emits captured variables against the name-keyed `EnvFrame`
//! model (`PushEnvChild`/`DefineEnvVar`/`LoadEnvVar`/`StoreEnvVar`/
//! `PopEnvChild`, and `MakeClosure { capture_env: true }`). When a whole
//! compilation unit is self-contained, [`rewrite_unit`] decides at compile
//! time where every one of its bindings lives and rewrites the unit, once, to
//! the heap representation. Nothing is verified or looked up by name at run
//! time; an ineligible unit is left exactly as lowered.
//!
//! # Eligibility
//!
//! A unit is the root function plus every nested function it creates with a
//! capturing `MakeClosure`, transitively. It is eligible when
//!
//! - every captured-variable reference in it resolves, statically, to a
//!   binding made inside the unit (no free names that a tree-walker or
//!   enclosing frame would supply);
//! - it never hands a form to the tree-walker (`EvalHost`/`MakeClosureEnv`)
//!   and no restart-clause body uses a captured variable;
//! - its frame structure is the one the lowerer produces: balanced
//!   `PushEnvChild`/`PopEnvChild` in instruction order, one definition per
//!   name per frame, one creation site per nested template;
//! - no capturing closure has a lambda list whose defaults the general
//!   binder would evaluate against its captured frame.
//!
//! # Layout and assignment analysis
//!
//! A binding nobody else captures becomes a plain local slot of its function.
//! A binding that closures capture but nobody assigns (and that is made
//! before any closure that sees it is created) is copied by value into each
//! closure that needs it: the closure's captured environment is a fresh
//! `[parent, values...]`. A binding that is captured and assigned (or that a
//! LABELS closure sees before it is made) lives in a slot of its scope's heap
//! environment, which the scope allocates on entry (so a scope in a loop makes
//! a fresh binding per iteration) and keeps in a local slot. Every environment
//! a function makes chains to the next enclosing one it makes, then to its
//! own captured environment, so a closure holds a single pointer and reaches
//! any captured binding by `(depth, index)`.
//!
//! The closure attaches its environment through a carrier frame (see
//! [`carrier`]); that is the one interface to the closure representation.

use super::*;

/// One function of the unit.
struct Node {
    /// Nested-function indices from the root to this function.
    path: Vec<u32>,
    /// The creating function and the PC of its `MakeClosure`.
    creator: Option<(usize, usize)>,
    frames: Vec<Frame>,
    /// The innermost own frame active when each instruction executes.
    frame_at: Vec<Option<u16>>,
    /// The base frame (`has_env`), holding the boxed parameters.
    base: Option<u16>,
}

struct Frame {
    parent: Option<u16>,
    /// Bindings in definition order.
    bindings: Vec<usize>,
}

struct Binding {
    node: usize,
    frame: u16,
    name: String,
    /// Definition PC, or `None` for a parameter (bound before any code).
    def_pc: Option<usize>,
    /// Parameter position, for a boxed parameter.
    param: Option<usize>,
    captured: bool,
    assigned: bool,
    late: bool,
}

impl Binding {
    fn shared(&self) -> bool {
        self.captured && (self.assigned || self.late)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EnvId {
    /// Scope frame `frame` of `node`, when materialized.
    Own(usize, u16),
    /// The captured-value layer of closure `node`.
    Capture(usize),
}

struct Analysis {
    nodes: Vec<Node>,
    bindings: Vec<Binding>,
    /// (node, pc) → binding, for every Load/Store/DefineEnvVar.
    refs: HashMap<(usize, usize), usize>,
    /// Bindings defined outside each node that it or its descendants use.
    needs: Vec<std::collections::BTreeSet<usize>>,
}

fn is_env_instruction(instr: &Instr) -> bool {
    matches!(
        instr,
        Instr::LoadEnvVar(_)
            | Instr::StoreEnvVar(_)
            | Instr::DefineEnvVar(_)
            | Instr::PushEnvChild
            | Instr::PopEnvChild
            | Instr::MakeClosure {
                capture_env: true,
                ..
            }
    )
}

fn uses_legacy_env(function: &BytecodeFunction) -> bool {
    function.has_env
        || function.code.iter().any(is_env_instruction)
        || function
            .param_layout
            .iter()
            .any(|(_, location)| matches!(location, VarLoc::Boxed))
}

/// Whether a lambda list's defaults are all literal constants, so the general
/// binder never evaluates a form against the closure's captured frame.
fn constant_defaults(params_form: EgclVal) -> bool {
    fn literal(form: EgclVal) -> bool {
        !form.is_cons()
            && (!form.is_symbol()
                || form.is_nil()
                || form == T
                || super::super::is_keyword_arg(form))
    }
    list_to_vec(params_form).into_iter().all(|item| {
        if !item.is_cons() {
            return true;
        }
        let parts = list_to_vec(item);
        parts.get(1).is_none_or(|default| literal(*default))
    })
}

fn function_at<'a>(root: &'a BytecodeFunction, path: &[u32]) -> &'a BytecodeFunction {
    let mut function = root;
    for &index in path {
        function = &function.nested_functions[index as usize];
    }
    function
}

fn function_at_mut<'a>(root: &'a mut BytecodeFunction, path: &[u32]) -> &'a mut BytecodeFunction {
    let mut function = root;
    for &index in path {
        function = Arc::make_mut(&mut function.nested_functions[index as usize]);
    }
    function
}

fn reject(reason: &str) -> Option<Analysis> {
    if std::env::var_os("EGCL_HEAP_ENV_TRACE").is_some() {
        eprintln!("[heap-env] ineligible: {reason}");
    }
    None
}

fn analyze(root: &BytecodeFunction) -> Option<Analysis> {
    let mut analysis = Analysis {
        nodes: Vec::new(),
        bindings: Vec::new(),
        refs: HashMap::new(),
        needs: Vec::new(),
    };
    // Collect the unit: capturing creation sites, depth first.
    let mut pending = vec![(Vec::<u32>::new(), None::<(usize, usize)>)];
    let mut created = std::collections::HashSet::new();
    while let Some((path, creator)) = pending.pop() {
        let id = analysis.nodes.len();
        let function = function_at(root, &path);
        if function.code.iter().any(|instr| {
            matches!(instr, Instr::EvalHost(_) | Instr::MakeClosureEnv(_))
                || matches!(
                    instr,
                    Instr::MakeEnv(_)
                        | Instr::EnvRef { .. }
                        | Instr::EnvSet { .. }
                        | Instr::ClosureEnv
                        | Instr::MakeEnvClosure { .. }
                )
        }) {
            return reject("tree-walker handoff or already rewritten");
        }
        if function
            .restart_cases
            .iter()
            .flat_map(|info| info.restarts.iter())
            .any(|restart| uses_legacy_env(&restart.function))
        {
            return reject("restart clause uses a captured variable");
        }
        if creator.is_some() && function.variadic && !constant_defaults(function.params_form) {
            return reject("closure lambda list evaluates defaults in its frame");
        }
        for (pc, instr) in function.code.iter().enumerate() {
            if let Instr::MakeClosure { func, capture_env } = *instr {
                if !created.insert((path.clone(), func)) {
                    return reject("nested template has two creation sites");
                }
                if capture_env {
                    if function.nested_functions.get(func as usize).is_none() {
                        return reject("missing nested function");
                    }
                    let mut child = path.clone();
                    child.push(func);
                    pending.push((child, Some((id, pc))));
                }
            }
        }
        analysis.nodes.push(Node {
            path,
            creator,
            frames: Vec::new(),
            frame_at: Vec::new(),
            base: None,
        });
    }
    analysis.needs = vec![Default::default(); analysis.nodes.len()];

    // Frames and definitions.
    for id in 0..analysis.nodes.len() {
        let function = function_at(root, &analysis.nodes[id].path);
        let mut frames = Vec::new();
        let mut current = None;
        if function.has_env {
            frames.push(Frame {
                parent: None,
                bindings: Vec::new(),
            });
            current = Some(0u16);
        }
        let base = current;
        for (index, (name, location)) in function.param_layout.iter().enumerate() {
            if let VarLoc::Boxed = location {
                let Some(base) = base else {
                    return reject("boxed parameter without a base frame");
                };
                let binding = analysis.bindings.len();
                analysis.bindings.push(Binding {
                    node: id,
                    frame: base,
                    name: name.clone(),
                    def_pc: None,
                    param: Some(index),
                    captured: false,
                    assigned: false,
                    late: false,
                });
                frames[base as usize].bindings.push(binding);
            }
        }
        let mut frame_at = Vec::with_capacity(function.code.len());
        for (pc, instr) in function.code.iter().enumerate() {
            frame_at.push(current);
            match *instr {
                Instr::PushEnvChild => {
                    if frames.len() >= u16::MAX as usize {
                        return reject("too many frames");
                    }
                    frames.push(Frame {
                        parent: current,
                        bindings: Vec::new(),
                    });
                    current = Some((frames.len() - 1) as u16);
                }
                Instr::PopEnvChild => {
                    let Some(frame) = current.filter(|frame| Some(*frame) != base) else {
                        return reject("unbalanced PopEnvChild");
                    };
                    current = frames[frame as usize].parent;
                }
                Instr::DefineEnvVar(name) => {
                    let Some(frame) = current else {
                        return reject("definition without a frame");
                    };
                    let name = function.names.get(name as usize)?.clone();
                    let duplicate = frames[frame as usize]
                        .bindings
                        .iter()
                        .any(|&binding| analysis.bindings[binding].name == name);
                    if duplicate {
                        return reject("two definitions of one name in a frame");
                    }
                    let binding = analysis.bindings.len();
                    analysis.bindings.push(Binding {
                        node: id,
                        frame,
                        name,
                        def_pc: Some(pc),
                        param: None,
                        captured: false,
                        assigned: false,
                        late: false,
                    });
                    frames[frame as usize].bindings.push(binding);
                    analysis.refs.insert((id, pc), binding);
                }
                _ => {}
            }
        }
        if current != base {
            return reject("unbalanced PushEnvChild");
        }
        let node = &mut analysis.nodes[id];
        node.frames = frames;
        node.frame_at = frame_at;
        node.base = base;
    }

    // Resolve every reference statically.
    for id in 0..analysis.nodes.len() {
        let function = function_at(root, &analysis.nodes[id].path);
        for (pc, instr) in function.code.iter().enumerate() {
            let (name, store) = match *instr {
                Instr::LoadEnvVar(name) => (name, false),
                Instr::StoreEnvVar(name) => (name, true),
                _ => continue,
            };
            let name = function.names.get(name as usize)?;
            let Some((binding, late)) = resolve(&analysis, id, pc, name) else {
                return reject("free captured name");
            };
            analysis.refs.insert((id, pc), binding);
            let owner = analysis.bindings[binding].node;
            let entry = &mut analysis.bindings[binding];
            entry.assigned |= store;
            entry.late |= late;
            if owner != id {
                entry.captured = true;
                let mut node = id;
                while node != owner {
                    analysis.needs[node].insert(binding);
                    node = analysis.nodes[node].creator?.0;
                }
            }
        }
    }
    Some(analysis)
}

/// The binding `name` denotes at `pc` of `node`, and whether it was found as
/// a LABELS-style definition made after the closure that sees it.
fn resolve(analysis: &Analysis, node: usize, pc: usize, name: &str) -> Option<(usize, bool)> {
    let function_key = lexical_identity::function_base(name).is_some();
    let mut node = node;
    let mut frame = analysis.nodes[node].frame_at[pc];
    let mut limit = pc;
    let mut closure = false;
    loop {
        while let Some(current) = frame {
            let frame_info = &analysis.nodes[node].frames[current as usize];
            for &binding in &frame_info.bindings {
                let entry = &analysis.bindings[binding];
                if entry.name != name {
                    continue;
                }
                match entry.def_pc {
                    None => return Some((binding, false)),
                    Some(def) if def < limit => return Some((binding, false)),
                    Some(_) if closure && function_key => return Some((binding, true)),
                    Some(_) => {}
                }
            }
            frame = frame_info.parent;
        }
        let (creator, creation) = analysis.nodes[node].creator?;
        node = creator;
        frame = analysis.nodes[node].frame_at[creation];
        limit = creation;
        closure = true;
    }
}

/// Per-function decisions.
struct Layout {
    /// Local slot of each own non-shared binding (and of every boxed parameter).
    local: HashMap<usize, u16>,
    /// Environment index of each own shared binding.
    env_slot: HashMap<usize, u16>,
    /// Local slot holding each materialized own frame's environment.
    frame_local: HashMap<u16, u16>,
    /// Slot count of each materialized frame.
    frame_size: HashMap<u16, u16>,
    /// Local slot holding the function's captured environment.
    closure_local: Option<u16>,
    /// Values this closure's creator copies into its capture layer.
    flat: Vec<usize>,
    /// Environments reachable from the captured environment, nearest first.
    chain: Vec<EnvId>,
    n_locals: u16,
}

fn plan(root: &BytecodeFunction, analysis: &Analysis) -> Option<Vec<Layout>> {
    let mut layouts: Vec<Layout> = Vec::with_capacity(analysis.nodes.len());
    for (id, node) in analysis.nodes.iter().enumerate() {
        let function = function_at(root, &node.path);
        let mut next = function.n_locals;
        let mut fresh = || {
            let slot = next;
            next = next.checked_add(1)?;
            Some(slot)
        };
        let mut layout = Layout {
            local: HashMap::new(),
            env_slot: HashMap::new(),
            frame_local: HashMap::new(),
            frame_size: HashMap::new(),
            closure_local: None,
            flat: Vec::new(),
            chain: Vec::new(),
            n_locals: 0,
        };
        if !analysis.needs[id].is_empty() {
            layout.closure_local = Some(fresh()?);
        }
        for (frame_index, frame) in node.frames.iter().enumerate() {
            let mut size = 0u16;
            for &binding in &frame.bindings {
                let entry = &analysis.bindings[binding];
                if entry.shared() {
                    layout.env_slot.insert(binding, size);
                    size = size.checked_add(1)?;
                }
                if !entry.shared() || entry.param.is_some() {
                    layout.local.insert(binding, fresh()?);
                }
            }
            if size > 0 {
                layout.frame_local.insert(frame_index as u16, fresh()?);
                layout.frame_size.insert(frame_index as u16, size);
            }
        }
        if let Some((creator, _)) = node.creator {
            layout.flat = analysis.needs[id]
                .iter()
                .copied()
                .filter(|&binding| {
                    let entry = &analysis.bindings[binding];
                    entry.node == creator && !entry.shared()
                })
                .collect();
        }
        layout.n_locals = next;
        layouts.push(layout);
    }
    // Chains, parents first (a creator precedes its closures in `nodes`).
    for id in 0..analysis.nodes.len() {
        if analysis.needs[id].is_empty() {
            continue;
        }
        let (creator, creation) = analysis.nodes[id].creator?;
        let mut chain = Vec::new();
        if !layouts[id].flat.is_empty() {
            chain.push(EnvId::Capture(id));
        }
        let creator_node = &analysis.nodes[creator];
        let mut frame = creator_node.frame_at[creation];
        while let Some(current) = frame {
            if layouts[creator].frame_local.contains_key(&current) {
                chain.push(EnvId::Own(creator, current));
            }
            frame = creator_node.frames[current as usize].parent;
        }
        if !analysis.needs[creator].is_empty() {
            let outer = layouts[creator].chain.clone();
            chain.extend(outer);
        }
        if chain.is_empty() || chain.len() > u8::MAX as usize {
            return None;
        }
        layouts[id].chain = chain;
    }
    Some(layouts)
}

/// Where `binding` lives as seen from `node`'s code.
enum Place {
    Local(u16),
    Env { local: u16, depth: u8, index: u16 },
}

fn place(analysis: &Analysis, layouts: &[Layout], node: usize, binding: usize) -> Option<Place> {
    let entry = &analysis.bindings[binding];
    let layout = &layouts[node];
    if entry.node == node {
        return Some(if entry.shared() {
            Place::Env {
                local: *layout.frame_local.get(&entry.frame)?,
                depth: 0,
                index: *layout.env_slot.get(&binding)?,
            }
        } else {
            Place::Local(*layout.local.get(&binding)?)
        });
    }
    let (target, index) = if entry.shared() {
        (
            EnvId::Own(entry.node, entry.frame),
            *layouts[entry.node].env_slot.get(&binding)?,
        )
    } else {
        // The closure created directly in the binding's function on the way
        // down to `node` holds the copied value.
        let mut holder = node;
        while analysis.nodes[holder].creator?.0 != entry.node {
            holder = analysis.nodes[holder].creator?.0;
        }
        let index = layouts[holder].flat.iter().position(|&b| b == binding)?;
        (EnvId::Capture(holder), u16::try_from(index).ok()?)
    };
    let depth = layout.chain.iter().position(|&id| id == target)?;
    Some(Place::Env {
        local: layout.closure_local?,
        depth: u8::try_from(depth).ok()?,
        index,
    })
}

/// Rewrite `root`'s unit to heap environments when it is eligible. Returns
/// whether it did; an ineligible unit is unchanged.
pub(super) fn rewrite_unit(root: &mut BytecodeFunction) -> bool {
    if !enabled() || !unit_uses_env(root) {
        return false;
    }
    if !within_bisect_limit()
        || std::env::var("EGCL_HEAP_ENV_SKIP")
            .is_ok_and(|skip| skip.split(',').any(|name| root.name.contains(name)))
    {
        return false;
    }
    let Some(analysis) = analyze(root) else {
        if std::env::var_os("EGCL_HEAP_ENV_TRACE").is_some() {
            eprintln!("[heap-env]   ... unit {}", root.name);
        }
        return false;
    };
    let Some(layouts) = plan(root, &analysis) else {
        let _ = reject("layout");
        return false;
    };
    // Build every new body before changing any, so a failure leaves the unit
    // untouched.
    let mut rewritten = Vec::with_capacity(analysis.nodes.len());
    for id in 0..analysis.nodes.len() {
        let function = function_at(root, &analysis.nodes[id].path);
        match rewrite_function(function, id, &analysis, &layouts) {
            Some(body) => rewritten.push(body),
            None => {
                let _ = reject("rewrite");
                return false;
            }
        }
    }
    for (id, body) in rewritten.into_iter().enumerate() {
        let function = function_at_mut(root, &analysis.nodes[id].path);
        body.install(function);
    }
    if std::env::var_os("EGCL_HEAP_ENV_TRACE").is_some() {
        eprintln!(
            "[heap-env] rewrote {} ({} functions)",
            root.name,
            analysis.nodes.len()
        );
    }
    true
}

fn unit_uses_env(root: &BytecodeFunction) -> bool {
    uses_legacy_env(root)
        || root.code.iter().any(|instr| {
            matches!(
                instr,
                Instr::MakeClosure {
                    capture_env: true,
                    ..
                }
            )
        })
}

/// `EGCL_HEAP_ENV_LIMIT=N` rewrites only the first N candidate units, to
/// bisect a misbehaving rewrite to one unit.
fn within_bisect_limit() -> bool {
    static LIMIT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    match LIMIT.get_or_init(|| {
        std::env::var("EGCL_HEAP_ENV_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
    }) {
        None => true,
        Some(limit) => SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < *limit,
    }
}

/// `EGCL_HEAP_ENV=0` keeps every unit on the name-keyed frames.
pub(super) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("EGCL_HEAP_ENV").as_deref(),
            Ok("0" | "off" | "no" | "false")
        )
    })
}

struct NewBody {
    code: Vec<Instr>,
    constants_nil: Option<u16>,
    n_locals: u16,
    extra_stack: u16,
    extra_names: Vec<String>,
    param_layout: Vec<(String, VarLoc)>,
    handler_pcs: Vec<Vec<u32>>,
    var_slots: Vec<Vec<Option<u16>>>,
    self_call_sites: Vec<u32>,
    notinline_sites: Vec<u32>,
}

impl NewBody {
    fn install(self, function: &mut BytecodeFunction) {
        function.code = self.code;
        if let Some(index) = self.constants_nil
            && usize::from(index) == function.constants.len()
        {
            function.constants.push(NIL);
        }
        function.n_locals = self.n_locals;
        function.names.extend(self.extra_names);
        function.max_stack = function.max_stack.saturating_add(self.extra_stack);
        function.param_layout = self.param_layout;
        function.has_env = false;
        for (info, pcs) in function.handler_cases.iter_mut().zip(self.handler_pcs) {
            for (clause, pc) in info.clauses.iter_mut().zip(pcs) {
                clause.body_bcp = pc;
            }
        }
        for (info, slots) in function.handler_cases.iter_mut().zip(self.var_slots) {
            for (clause, slot) in info.clauses.iter_mut().zip(slots) {
                clause.var_slot = slot;
            }
        }
        function.self_call_sites = self.self_call_sites;
        function.notinline_sites = self.notinline_sites;
    }
}

fn rewrite_function(
    function: &BytecodeFunction,
    id: usize,
    analysis: &Analysis,
    layouts: &[Layout],
) -> Option<NewBody> {
    let node = &analysis.nodes[id];
    let layout = &layouts[id];
    let mut nil_constant = function.constants.iter().position(|c| *c == NIL);
    let mut nil_added = None;
    let mut nil = |code: &mut Vec<Instr>| -> Option<()> {
        let index = match nil_constant {
            Some(index) => index,
            None => {
                let index = function.constants.len();
                nil_constant = Some(index);
                nil_added = Some(u16::try_from(index).ok()?);
                index
            }
        };
        code.push(Instr::Const(u16::try_from(index).ok()?));
        Some(())
    };
    // The environment a new environment or capture layer created at `frame`
    // gets as its parent.
    let parent = |frame: Option<u16>| -> Option<Instr> {
        let mut frame = frame;
        while let Some(current) = frame {
            if let Some(&local) = layout.frame_local.get(&current) {
                return Some(Instr::LoadLocal(local));
            }
            frame = node.frames[current as usize].parent;
        }
        layout.closure_local.map(Instr::LoadLocal)
    };

    let mut code = Vec::with_capacity(function.code.len() + 8);
    let mut extra_stack = 2u16;
    let mut extra_names = Vec::new();
    // Prologue: cache the captured environment, then make the base
    // environment and move its shared parameters into it.
    if let Some(local) = layout.closure_local {
        code.push(Instr::ClosureEnv);
        code.push(Instr::StoreLocal(local));
    }
    let mut param_layout = function.param_layout.clone();
    for (index, (_, location)) in param_layout.iter_mut().enumerate() {
        if let VarLoc::Boxed = location {
            let binding = node.base.and_then(|base| {
                node.frames[base as usize]
                    .bindings
                    .iter()
                    .copied()
                    .find(|&b| analysis.bindings[b].param == Some(index))
            })?;
            *location = VarLoc::Slot(*layout.local.get(&binding)?);
        }
    }
    if let Some(base) = node.base
        && let Some(&local) = layout.frame_local.get(&base)
    {
        match parent(node.frames[base as usize].parent) {
            Some(load) => code.push(load),
            None => nil(&mut code)?,
        }
        code.push(Instr::MakeEnv(*layout.frame_size.get(&base)?));
        code.push(Instr::StoreLocal(local));
        for &binding in &node.frames[base as usize].bindings {
            let entry = &analysis.bindings[binding];
            if entry.param.is_some() && entry.shared() {
                code.push(Instr::LoadLocal(*layout.local.get(&binding)?));
                code.push(Instr::LoadLocal(local));
                let name = match name_index(function, &entry.name) {
                    Some(index) => index,
                    None => {
                        let index = function.names.len() + extra_names.len();
                        extra_names.push(entry.name.clone());
                        u16::try_from(index).ok()?
                    }
                };
                code.push(Instr::EnvSet {
                    depth: 0,
                    idx: *layout.env_slot.get(&binding)?,
                    name,
                });
            }
        }
    }

    let mut map = Vec::with_capacity(function.code.len() + 1);
    for (pc, instr) in function.code.iter().enumerate() {
        map.push(u32::try_from(code.len()).ok()?);
        match *instr {
            Instr::PushEnvChild => {
                // The frame this instruction opens is the next one active.
                let frame = node.frame_at.get(pc + 1).copied().flatten()?;
                if let Some(&local) = layout.frame_local.get(&frame) {
                    match parent(node.frames[frame as usize].parent) {
                        Some(load) => code.push(load),
                        None => nil(&mut code)?,
                    }
                    code.push(Instr::MakeEnv(*layout.frame_size.get(&frame)?));
                    code.push(Instr::StoreLocal(local));
                }
            }
            Instr::PopEnvChild => {}
            Instr::DefineEnvVar(name) | Instr::StoreEnvVar(name) | Instr::LoadEnvVar(name) => {
                let binding = *analysis.refs.get(&(id, pc))?;
                let load = matches!(instr, Instr::LoadEnvVar(_));
                match place(analysis, layouts, id, binding)? {
                    Place::Local(local) => code.push(if load {
                        Instr::LoadLocal(local)
                    } else {
                        Instr::StoreLocal(local)
                    }),
                    Place::Env {
                        local,
                        depth,
                        index,
                    } => {
                        code.push(Instr::LoadLocal(local));
                        code.push(if load {
                            Instr::EnvRef {
                                depth,
                                idx: index,
                                name,
                            }
                        } else {
                            Instr::EnvSet {
                                depth,
                                idx: index,
                                name,
                            }
                        });
                    }
                }
            }
            Instr::MakeClosure {
                func,
                capture_env: true,
            } => {
                let child = analysis
                    .nodes
                    .iter()
                    .position(|n| n.creator == Some((id, pc)))?;
                if analysis.needs[child].is_empty() {
                    code.push(Instr::MakeClosure {
                        func,
                        capture_env: false,
                    });
                } else {
                    match parent(node.frame_at[pc]) {
                        Some(load) => code.push(load),
                        None => nil(&mut code)?,
                    }
                    let flat = &layouts[child].flat;
                    for &binding in flat {
                        match place(analysis, layouts, id, binding)? {
                            Place::Local(local) => code.push(Instr::LoadLocal(local)),
                            Place::Env { .. } => return None,
                        }
                    }
                    let nflat = u16::try_from(flat.len()).ok()?;
                    extra_stack = extra_stack.max(nflat.saturating_add(1));
                    code.push(Instr::MakeEnvClosure { func, nflat });
                }
            }
            other => code.push(other),
        }
    }
    map.push(u32::try_from(code.len()).ok()?);
    let target = |pc: u32| map.get(pc as usize).copied();
    for instr in &mut code {
        retarget(instr, &target)?;
    }
    let handler_pcs = function
        .handler_cases
        .iter()
        .map(|info| {
            info.clauses
                .iter()
                .map(|clause| target(clause.body_bcp))
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    let self_call_sites = function
        .self_call_sites
        .iter()
        .map(|&pc| target(pc))
        .collect::<Option<Vec<_>>>()?;
    let notinline_sites = function
        .notinline_sites
        .iter()
        .map(|&pc| target(pc))
        .collect::<Option<Vec<_>>>()?;
    // Every tier binds parameter i to local slot i, so renumber the locals:
    // the parameters first, in order, then every other local in its old order
    // (which keeps each MULTIPLE-VALUE-BIND range contiguous).
    let mut order: Vec<u16> = Vec::with_capacity(usize::from(layout.n_locals));
    for (_, location) in &param_layout {
        let VarLoc::Slot(slot) = location else {
            return None;
        };
        if order.contains(slot) {
            return None;
        }
        order.push(*slot);
    }
    for slot in 0..layout.n_locals {
        if !order.contains(&slot) {
            order.push(slot);
        }
    }
    let mut renumber = vec![0u16; usize::from(layout.n_locals)];
    for (new, &old) in order.iter().enumerate() {
        renumber[usize::from(old)] = u16::try_from(new).ok()?;
    }
    let local = |slot: u16| renumber.get(usize::from(slot)).copied();
    for instr in &mut code {
        match instr {
            Instr::LoadLocal(slot) | Instr::StoreLocal(slot) => *slot = local(*slot)?,
            Instr::TakeValuesToLocals { nvars, slot_base } => {
                let base = local(*slot_base)?;
                for k in 0..*nvars {
                    if local(slot_base.checked_add(k)?)? != base.checked_add(k)? {
                        return None;
                    }
                }
                *slot_base = base;
            }
            _ => {}
        }
    }
    for (_, location) in &mut param_layout {
        if let VarLoc::Slot(slot) = location {
            *slot = local(*slot)?;
        }
    }
    let var_slots = function
        .handler_cases
        .iter()
        .map(|info| {
            info.clauses
                .iter()
                .map(|clause| match clause.var_slot {
                    Some(slot) => local(slot).map(Some),
                    None => Some(None),
                })
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    Some(NewBody {
        var_slots,
        code,
        constants_nil: nil_added,
        n_locals: layout.n_locals,
        extra_stack,
        extra_names,
        param_layout,
        handler_pcs,
        self_call_sites,
        notinline_sites,
    })
}

fn name_index(function: &BytecodeFunction, name: &str) -> Option<u16> {
    function
        .names
        .iter()
        .position(|candidate| candidate == name)
        .and_then(|index| u16::try_from(index).ok())
}

/// Map every PC operand of `instr` through `target`. Instructions the rewrite
/// copied keep their old PCs until this runs.
fn retarget(instr: &mut Instr, target: &impl Fn(u32) -> Option<u32>) -> Option<()> {
    match instr {
        Instr::Br(pc) | Instr::BrIfFalse(pc) | Instr::BrIfTrue(pc) => *pc = target(*pc)?,
        Instr::PushCatch { resume_bcp, .. }
        | Instr::PushBlock { resume_bcp, .. }
        | Instr::PushRestartCase { resume_bcp, .. } => *resume_bcp = target(*resume_bcp)?,
        Instr::PushUnwind { cleanup_bcp, .. } => *cleanup_bcp = target(*cleanup_bcp)?,
        Instr::Go { target_bcp, .. } => *target_bcp = target(*target_bcp)?,
        Instr::NamedTag { tag_bcp, .. } => *tag_bcp = target(*tag_bcp)?,
        Instr::EnterCleanupNormal {
            cleanup_bcp,
            resume_bcp,
        } => {
            *cleanup_bcp = target(*cleanup_bcp)?;
            *resume_bcp = target(*resume_bcp)?;
        }
        _ => {}
    }
    Some(())
}

// ── Closure attachment (TEMPORARY interface for the stage-2 merge) ─────
//
// A heap-environment closure carries exactly one value: its captured
// environment. Until closure instances own their state (Backpack stage 2,
// bliss-gg3i1.7.15.3: `ClosureState` on the function object), it rides in a
// *carrier* frame passed where a captured `EnvFrame` goes today. Every tier
// already installs a closure's captured frame as its activation's frame
// (`Activation::env_frame`, `NATIVE_ENV_FRAME`), so `ClosureEnv` reads it back
// from there with one indexed read: no name is compared. At the merge, replace
// `carrier`/`captured_env` with a traced environment field on the closure
// state, and nothing else changes.

/// Key of the carrier frame's only entry. Never a Lisp-visible name.
const CARRIER_KEY: &str = "\0egcl-backpack-environment";

/// A carrier frame holding `env` (TEMPORARY; see above).
pub(super) fn carrier(env: EgclVal) -> Arc<FrameCell> {
    let mut vars = super::super::VecMap::default();
    vars.insert(CARRIER_KEY.to_string(), env);
    FrameCell::new_arc(EnvFrame {
        vars,
        symbol_vars: super::super::VecMap::default(),
        function_vars: super::super::VecMap::default(),
        parent: None,
    })
}

/// The environment a heap-environment closure captured, given the frame its
/// activation runs with: NIL when there is none.
pub(super) fn captured_env(frame: Option<&Arc<FrameCell>>) -> EgclVal {
    let Some(frame) = frame else {
        return NIL;
    };
    let frame = frame.borrow();
    match frame.vars.iter().next() {
        Some((key, value)) => {
            debug_assert_eq!(key, CARRIER_KEY);
            *value
        }
        None => NIL,
    }
}

/// Make the closure `MakeEnvClosure` describes: `values` (already popped,
/// in push order) over `parent`. Roots its inputs across the allocations.
pub(super) fn make_closure(
    nested: &Arc<BytecodeFunction>,
    parent: EgclVal,
    values: &[EgclVal],
    env: &Env,
) -> Option<EgclVal> {
    egcl_rt::rooted!(values = values.to_vec());
    let captured = if values.is_empty() {
        parent
    } else {
        let layer = egcl_rt::lexenv::make(parent, values.len())?;
        for (index, value) in values.iter().enumerate() {
            egcl_rt::lexenv::store(layer, 0, index as u16, *value);
        }
        layer
    };
    egcl_rt::rooted!(captured = captured);
    let frame = carrier(*captured);
    Some(make_template_closure(nested, Some(frame), env))
}

/// The variable names of every heap-environment slot `function` addresses,
/// keyed by the local slot (or captured-environment depth) it reaches the
/// environment through and the slot index: the compile-time name table a
/// debugger shows for an environment. No runtime path consults it.
pub(super) fn slot_names(function: &BytecodeFunction) -> Vec<(u16, u8, u16, String)> {
    let mut names = Vec::new();
    for window in function.code.windows(2) {
        if let [
            Instr::LoadLocal(local),
            Instr::EnvRef { depth, idx, name } | Instr::EnvSet { depth, idx, name },
        ] = window
            && let Some(text) = function.names.get(usize::from(*name))
        {
            let entry = (
                *local,
                *depth,
                *idx,
                lexical_identity::name(text).to_owned(),
            );
            if !names.contains(&entry) {
                names.push(entry);
            }
        }
    }
    names
}

// ── Native helpers (T1 and T2) ──────────────────────────────────────────
//
// Every native tier reaches the heap-environment instructions through these
// for now: they take the environment value and `(depth, index)`, never a
// name. (The packed `name` is read only to name the variable in the
// UNBOUND-VARIABLE error of a slot read before its binding was made.) Inline
// two-to-three-load access in the emitters is the follow-up.

/// Count one heap-environment slot access `depth` links away in
/// `(egcl::%env-access-counts)`. A disabled counter costs one relaxed load.
#[inline]
pub(super) fn count_access(depth: u8) {
    use crate::cli::env_counts::{self, Counter};
    env_counts::bump(Counter::SlotOps, 1);
    if depth != 0 {
        env_counts::bump(Counter::SlotHops, u64::from(depth));
    }
}

#[cfg(target_arch = "x86_64")]
/// Pack `EnvRef`/`EnvSet` operands into one immediate word.
pub(super) fn pack(depth: u8, idx: u16, name: u16) -> u64 {
    (u64::from(name) << 32) | (u64::from(depth) << 16) | u64::from(idx)
}

#[cfg(target_arch = "x86_64")]
fn unpack(packed: u64) -> (u8, u16, u64) {
    ((packed >> 16) as u8, packed as u16, (packed >> 32) & 0xffff)
}

#[cfg(target_arch = "x86_64")]
fn unbound(names: *const NativeEnvNames, name: u64) -> EgclError {
    // SAFETY: emitted code retains its name table through its code owner.
    let symbol = (!names.is_null())
        .then(|| unsafe { &*names }.get(name))
        .flatten()
        .and_then(|resolved| lexical_identity::resolve(resolved.name()))
        .unwrap_or(NIL);
    EgclError::UnboundVariable(symbol)
}

#[cfg(target_arch = "x86_64")]
/// `MakeEnv`: allocates, so it is a c2i helper whose inputs are rooted here.
pub(super) extern "C" fn c2i_heap_make_env(parent: u64, size: u64) -> u64 {
    match guard_c2i(|| {
        egcl_rt::lexenv::make(EgclVal(parent), size as usize).ok_or_else(|| {
            EgclError::Internal(egcl_rt::gc::allocation_failure_message(
                "a lexical environment",
            ))
        })
    }) {
        Ok(value) => value.0,
        Err(error) => {
            stash_native_error(error);
            NIL.0
        }
    }
}

#[cfg(target_arch = "x86_64")]
/// `EnvRef`: no allocation, no lock, no name lookup.
pub(super) extern "C" fn c2i_heap_env_ref(
    env: u64,
    packed: u64,
    names: *const NativeEnvNames,
) -> u64 {
    let (depth, idx, name) = unpack(packed);
    count_access(depth);
    match egcl_rt::lexenv::load(EgclVal(env), depth, idx) {
        Some(value) if value != egcl_rt::value::UNBOUND => value.0,
        _ => {
            stash_native_error(unbound(names, name));
            NIL.0
        }
    }
}

#[cfg(target_arch = "x86_64")]
/// `EnvSet`: a barriered store; no allocation.
pub(super) extern "C" fn c2i_heap_env_set(
    env: u64,
    value: u64,
    packed: u64,
    names: *const NativeEnvNames,
) {
    let (depth, idx, name) = unpack(packed);
    count_access(depth);
    if !egcl_rt::lexenv::store(EgclVal(env), depth, idx, EgclVal(value)) {
        stash_native_error(unbound(names, name));
    }
}

#[cfg(target_arch = "x86_64")]
/// `ClosureEnv`: the running closure's environment, from the frame the native
/// entry installed for its activation (see [`carrier`]).
pub(super) extern "C" fn c2i_heap_closure_env() -> u64 {
    NATIVE_ENV_FRAME.with(|slot| captured_env(slot.borrow().as_ref()).0)
}

#[cfg(target_arch = "x86_64")]
/// `MakeEnvClosure`: `operands` addresses the parent followed by `nflat`
/// values, still on the (scanned) native operand stack.
pub(super) extern "C" fn c2i_heap_make_env_closure(
    nested: *const Arc<BytecodeFunction>,
    operands: *const u64,
    nflat: u64,
) -> u64 {
    match guard_c2i(|| {
        let env = NATIVE_ENV.with(|slot| slot.get());
        if nested.is_null() || env.is_null() || operands.is_null() {
            return Err(EgclError::Internal(
                "native closure has no body or environment".into(),
            ));
        }
        // SAFETY: `operands` addresses `nflat + 1` live operand words.
        let words = unsafe { std::slice::from_raw_parts(operands, nflat as usize + 1) };
        let values: Vec<EgclVal> = words[1..].iter().map(|&word| EgclVal(word)).collect();
        // SAFETY: the executing code retains its body and this nested template.
        make_closure(unsafe { &*nested }, EgclVal(words[0]), &values, unsafe {
            &*env
        })
        .ok_or_else(|| {
            EgclError::Internal(egcl_rt::gc::allocation_failure_message(
                "a closure environment",
            ))
        })
    }) {
        Ok(value) => value.0,
        Err(error) => {
            stash_native_error(error);
            NIL.0
        }
    }
}
