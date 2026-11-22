// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Remembered expansions of DEFTYPE names for [`super::quick_typep`].
//!
//! SBCL's compiler checks slots and declarations against its own DEFTYPEs --
//! `INDEX`, `(OR INDEX NULL)`, `ATTRIBUTES`, `SB-XC:FIXNUM` -- and each check
//! used to run the type's expander and then the general TYPEP. A leaf can do
//! neither, since an expander is Lisp code that allocates. So the evaluated
//! TYPEP handlers record a name's full expansion here, and the quick check,
//! leaf or not, decides the expansion from then on.
//!
//! An entry is valid while `*TYPE-EXPANDERS*`, `*TYPE-DEFINITIONS*` and the
//! class registry stay as they were when it was made: a DEFTYPE conses a new
//! registry head, and a new class can take a name before its expansion. Any
//! change empties the table. The heads are traced roots like the entries, so
//! a head cannot be reclaimed and its address reused by a different list.

use std::cell::RefCell;
use std::collections::HashMap;

use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};

/// What the general TYPEP does before expanding a name, keyed by bare name
/// (`typep_matches`): an expansion decides a name with one of these only if
/// that earlier step would not have answered first.
const NAMES_DECIDED_BEFORE_EXPANSION: &[&str] = &[
    "HEAP-OBJECT",
    "T",
    "ATOM",
    "FUNCTION",
    "COMPILED-FUNCTION",
    "GENERIC-FUNCTION",
    "STANDARD-GENERIC-FUNCTION",
    "STANDARD-OBJECT",
    "FUNCALLABLE-STANDARD-OBJECT",
    "STRUCTURE-OBJECT",
    "CONS",
    "LIST",
    "METHOD",
    "STANDARD-METHOD",
    "STREAM",
    "STRUCTURE-CLASS",
    "STANDARD-CLASS",
    "BUILT-IN-CLASS",
    "CLASS",
    "METAOBJECT",
];

struct Expansions {
    expanders: EgclVal,
    definitions: EgclVal,
    classes: usize,
    /// Name symbol index to its full expansion, or UNBOUND for a name that
    /// is not a DEFTYPE the quick check may expand.
    entries: HashMap<u32, EgclVal, egcl_rt::fxhash::FxBuildHasher>,
}

static EXPANSIONS: egcl_rt::execution_local::ExecutionLocal<RefCell<Expansions>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        RefCell::new(Expansions {
            expanders: NIL,
            definitions: NIL,
            classes: 0,
            entries: HashMap::default(),
        })
    })
};

/// The symbol indices of the two registry variables, interned once.
static REGISTRIES: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();

pub(super) fn init() {
    REGISTRIES.get_or_init(|| {
        (
            egcl_rt::symbols::intern("*TYPE-EXPANDERS*"),
            egcl_rt::symbols::intern("*TYPE-DEFINITIONS*"),
        )
    });
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan));
}

fn scan(visit: &mut dyn FnMut(*mut EgclVal)) {
    // SAFETY: root scanners run with every execution stopped.
    unsafe {
        EXPANSIONS.scan(|expansions| {
            let mut expansions = expansions.borrow_mut();
            visit(&mut expansions.expanders);
            visit(&mut expansions.definitions);
            for expansion in expansions.entries.values_mut() {
                if *expansion != UNBOUND {
                    visit(expansion);
                }
            }
        });
    }
}

/// The current registry heads and class count. No allocation.
fn current() -> Option<(EgclVal, EgclVal, usize)> {
    let &(expanders, definitions) = REGISTRIES.get()?;
    let value = |index| egcl_rt::symbols::symbol_value(index).unwrap_or(NIL);
    Some((
        value(expanders),
        value(definitions),
        egcl_stdlib::class_registry_size(),
    ))
}

impl Expansions {
    /// Empty the table unless it describes the current registries.
    fn validate(&mut self, (expanders, definitions, classes): (EgclVal, EgclVal, usize)) {
        if self.expanders != expanders || self.definitions != definitions || self.classes != classes
        {
            self.entries.clear();
            self.expanders = expanders;
            self.definitions = definitions;
            self.classes = classes;
        }
    }
}

/// The remembered expansion of the DEFTYPE named by symbol `name`. Does not
/// allocate, so a leaf may ask.
pub(super) fn lookup(name: u32) -> Option<EgclVal> {
    let expansion = entry(name).filter(|&expansion| expansion != UNBOUND);
    #[cfg(test)]
    if expansion.is_some() {
        HITS.with(|hits| hits.set(hits.get() + 1));
    }
    expansion
}

#[cfg(test)]
thread_local! {
    static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many checks a recorded expansion decided on this thread since the
/// last call.
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_hits_for_test() -> usize {
    HITS.with(|hits| hits.replace(0))
}

fn entry(name: u32) -> Option<EgclVal> {
    let now = current()?;
    EXPANSIONS.with(|expansions| {
        let mut expansions = expansions.try_borrow_mut().ok()?;
        expansions.validate(now);
        expansions.entries.get(&name).copied()
    })
}

/// Remember the expansions of the DEFTYPE names `spec` uses at its top level
/// or under OR, AND and NOT, so [`super::quick_typep`] can decide it next
/// time. Runs the expanders, so it allocates. A name whose expansion fails is
/// skipped: the general TYPEP reports that error if, and only if, it reaches
/// that name.
pub(in crate::cli) fn record(env: &crate::cli::Env, spec: EgclVal) {
    if REGISTRIES.get().is_none() {
        return;
    }
    let mut names = Vec::new();
    collect_names(spec, &mut names, 0);
    // An expansion can name further DEFTYPEs, as `(OR INDEX NULL)` does; those
    // join the list, so a check never reaches a name nobody recorded.
    let mut next = 0;
    while next < names.len() && next < 64 {
        let name = names[next];
        next += 1;
        if entry(name.as_symbol_index()).is_some() {
            continue;
        }
        let Some(before) = current() else {
            return;
        };
        let expansion = if expandable(name) {
            let Ok(expansion) = crate::cli::resolve_type_spec(env, name) else {
                continue;
            };
            expansion
        } else {
            name
        };
        let expansion = if expansion == name {
            UNBOUND
        } else {
            expansion
        };
        // An expander that defined a type or class answered under registries
        // that no longer stand.
        let Some(now) = current().filter(|&now| now == before) else {
            continue;
        };
        EXPANSIONS.with(|expansions| {
            let mut expansions = expansions.borrow_mut();
            expansions.validate(now);
            expansions.entries.insert(name.as_symbol_index(), expansion);
        });
        if expansion != UNBOUND {
            collect_names(expansion, &mut names, 0);
        }
    }
}

/// The symbols that could name a DEFTYPE where the quick check would look for
/// one. Immediates, so collecting them across later allocation is safe.
fn collect_names(spec: EgclVal, names: &mut Vec<EgclVal>, depth: u8) {
    if depth > 8 {
        return;
    }
    if spec.is_symbol() {
        if spec != NIL && spec != T && !names.contains(&spec) {
            names.push(spec);
        }
        return;
    }
    let Some((head, mut rest)) = super::cons_parts(spec) else {
        return;
    };
    if !head.is_symbol()
        || crate::cli::symbol_home_package_name(head) != "COMMON-LISP"
        || !matches!(
            crate::cli::sym_bare_name_rc(head).as_ref(),
            "OR" | "AND" | "NOT"
        )
    {
        return;
    }
    while let Some((member, next)) = super::cons_parts(rest) {
        collect_names(member, names, depth + 1);
        rest = next;
    }
}

/// A name the general TYPEP would expand before doing anything else with it:
/// not a class, not COMMON-LISP's, and not a bare name TYPEP tests first.
fn expandable(name: EgclVal) -> bool {
    egcl_stdlib::find_class(name).is_none()
        && crate::cli::symbol_home_package_name(name) != "COMMON-LISP"
        && !NAMES_DECIDED_BEFORE_EXPANSION.contains(&crate::cli::sym_bare_name_rc(name).as_ref())
}
