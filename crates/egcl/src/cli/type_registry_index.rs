// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Symbol-indexed lookup over the DEFTYPE registries.
//!
//! `*TYPE-EXPANDERS*` and `*TYPE-DEFINITIONS*` are alists that DEFTYPE only
//! ever conses onto, so a redefinition leaves the old entry behind and the
//! first match wins. Every TYPEP and declaration check resolves its specifier
//! through them; SBCL's host-2 build defines enough types that walking the
//! list dominated TYPEP itself (bliss-gg3i1.7).
//!
//! Each registry keeps an index from symbol to the first entry naming it,
//! valid for one list head. The head is a traced root, so it cannot be reclaimed
//! and its address reused by another list: a different current value is a
//! different list. When the new value is the old head with entries consed on
//! the front, only those entries are indexed; anything else rebuilds the index.

use std::cell::RefCell;
use std::collections::HashMap;

use egcl_rt::value::{EgclVal, NIL, T};

use super::{Env, cp};

/// Which registry an index belongs to.
#[derive(Clone, Copy)]
pub(super) enum TypeRegistry {
    Expanders = 0,
    Definitions = 1,
}

impl TypeRegistry {
    fn variable(self) -> &'static str {
        match self {
            Self::Expanders => "*TYPE-EXPANDERS*",
            Self::Definitions => "*TYPE-DEFINITIONS*",
        }
    }

    /// The registry's current dynamic value. Both are special variables, so
    /// this reads the symbol's value cell rather than searching every lexical
    /// frame by name first.
    pub(super) fn current(self, env: &Env) -> EgclVal {
        egcl_rt::symbols::find_index(self.variable())
            .and_then(|index| env.lookup_var_symbol(EgclVal::from_symbol_index(index)))
            .unwrap_or(NIL)
    }
}

struct RegistryIndex {
    /// The list this index describes; NIL before the first lookup.
    head: EgclVal,
    /// Symbol index to the first `(name value ...)` entry naming it.
    entries: HashMap<u32, EgclVal, egcl_rt::fxhash::FxBuildHasher>,
}

impl RegistryIndex {
    fn new() -> Self {
        Self {
            head: NIL,
            entries: HashMap::default(),
        }
    }

    /// Index the entries of `list` in front of the current head, or all of
    /// them when the current head is not part of `list`. No allocation.
    fn refresh(&mut self, list: EgclVal) {
        let mut fresh: Vec<(u32, EgclVal)> = Vec::new();
        let mut cursor = list;
        let mut reached_head = false;
        while cursor.is_cons() {
            if !self.head.is_nil() && cursor == self.head {
                reached_head = true;
                break;
            }
            let (entry, rest) = cp(cursor);
            if entry.is_cons() {
                let key = cp(entry).0;
                if let Some(index) = indexable_symbol(key) {
                    fresh.push((index, entry));
                }
            }
            cursor = rest;
        }
        if !reached_head {
            self.entries.clear();
        }
        // Earlier entries shadow later ones, so insert from the back.
        for (index, entry) in fresh.into_iter().rev() {
            self.entries.insert(index, entry);
        }
        self.head = list;
    }
}

/// Entries for NIL and T, whose tags carry no symbol index, are left to the
/// linear walk.
fn indexable_symbol(key: EgclVal) -> Option<u32> {
    (key.is_symbol() && key != NIL && key != T).then(|| key.as_symbol_index())
}

static INDEXES: egcl_rt::execution_local::ExecutionLocal<RefCell<[RegistryIndex; 2]>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        RefCell::new([RegistryIndex::new(), RegistryIndex::new()])
    })
};

fn scan_registry_indexes(visit: &mut dyn FnMut(*mut EgclVal)) {
    // SAFETY: root scanners run with every execution stopped.
    unsafe {
        INDEXES.scan(|indexes| {
            for index in indexes.borrow_mut().iter_mut() {
                visit(&mut index.head);
                for entry in index.entries.values_mut() {
                    visit(entry);
                }
            }
        });
    }
}

fn install_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_registry_indexes));
}

/// The value of the first entry of `list` (the current value of `registry`)
/// whose key is the symbol `name`.
pub(super) fn lookup(registry: TypeRegistry, list: EgclVal, name: EgclVal) -> Option<EgclVal> {
    let Some(symbol) = indexable_symbol(name) else {
        return walk(list, name);
    };
    if !list.is_cons() {
        return None;
    }
    install_scanner();
    let entry = INDEXES.with(|indexes| {
        let mut indexes = indexes.borrow_mut();
        let index = &mut indexes[registry as usize];
        if index.head != list {
            index.refresh(list);
        }
        index.entries.get(&symbol).copied()
    })?;
    // Read the value now rather than caching it, so the entry stays the
    // single source of truth.
    Some(cp(cp(entry).1).0)
}

fn walk(list: EgclVal, name: EgclVal) -> Option<EgclVal> {
    let mut cursor = list;
    while cursor.is_cons() {
        let (entry, rest) = cp(cursor);
        if entry.is_cons() {
            let (key, values) = cp(entry);
            if key == name {
                return Some(cp(values).0);
            }
        }
        cursor = rest;
    }
    None
}

#[cfg(test)]
mod tests;
