// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Address tables for native code linked into the process, independent of its
//! dynamic loader. Open handles still belong to the ordinary non-reused token
//! registry; compiled-in code itself has process lifetime.

use crate::error::EgclError;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub(crate) struct StaticLibrary {
    names: BTreeSet<String>,
    symbols: BTreeMap<String, usize>,
}

#[derive(Clone)]
pub(crate) enum Provider<L> {
    Static(Arc<StaticLibrary>),
    Loaded(L),
}

static LIBRARIES: Mutex<Vec<Arc<StaticLibrary>>> = Mutex::new(Vec::new());

fn error(message: impl Into<String>) -> EgclError {
    EgclError::FfiError(message.into())
}

fn checked_name(name: &str, kind: &str) -> Result<(), EgclError> {
    if name.is_empty() || name.contains('\0') {
        return Err(error(format!("invalid static {kind} name")));
    }
    Ok(())
}

/// Register the symbols of an archive linked into this executable. Registration
/// is idempotent for an identical table and is atomic on conflicting names.
/// No archive, manifest or shared-object file is needed at run time.
///
/// # Safety
/// Every address must name the advertised C symbol and remain valid for the
/// entire process lifetime. Both function and data symbols are permitted; the
/// caller of a foreign function must still supply its actual C signature.
pub unsafe fn register_static_library(
    name: &str,
    aliases: &[&str],
    symbols: &[(&str, *const ())],
) -> Result<(), EgclError> {
    checked_name(name, "library")?;
    let mut names = BTreeSet::from([name.to_owned()]);
    for alias in aliases {
        checked_name(alias, "library alias")?;
        names.insert((*alias).to_owned());
    }
    let mut entries = BTreeMap::new();
    for &(symbol, address) in symbols {
        checked_name(symbol, "symbol")?;
        if address.is_null()
            || entries
                .insert(symbol.to_owned(), address as usize)
                .is_some()
        {
            return Err(error(format!("null or duplicate static symbol '{symbol}'")));
        }
    }
    let candidate = Arc::new(StaticLibrary {
        names,
        symbols: entries,
    });
    let mut libraries = LIBRARIES.lock().unwrap();
    for library in libraries.iter() {
        if library.names == candidate.names && library.symbols == candidate.symbols {
            return Ok(());
        }
        if let Some(alias) = library.names.intersection(&candidate.names).next() {
            return Err(error(format!(
                "static library alias '{alias}' is already registered"
            )));
        }
        for (symbol, address) in &candidate.symbols {
            if library
                .symbols
                .get(symbol)
                .is_some_and(|existing| existing != address)
            {
                return Err(error(format!("conflicting static symbol '{symbol}'")));
            }
        }
    }
    libraries.push(candidate);
    Ok(())
}

pub(crate) fn lookup(name: &str) -> Option<Arc<StaticLibrary>> {
    LIBRARIES
        .lock()
        .unwrap()
        .iter()
        .find(|library| library.names.contains(name))
        .cloned()
}

impl StaticLibrary {
    pub(crate) fn symbol(&self, name: &str) -> Result<*const (), EgclError> {
        checked_name(name, "symbol")?;
        let address = self
            .symbols
            .get(name)
            .copied()
            .ok_or_else(|| error(format!("symbol '{name}' not found in static library")))?;
        let address = address as *const ();
        super::remember_foreign_symbol(address, name);
        Ok(address)
    }
}

pub(crate) fn global_symbol(name: &str) -> Option<*const ()> {
    let address = LIBRARIES
        .lock()
        .unwrap()
        .iter()
        .find_map(|library| library.symbols.get(name).copied())? as *const ();
    super::remember_foreign_symbol(address, name);
    Some(address)
}
