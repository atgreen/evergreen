// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Public generic function bindings are separate from retained method metadata.
//! A native generic uses a self-symbol function-cell marker; saved wrappers may
//! still invoke its existing method registry after that public name is unbound.
use super::*;

fn cached_wrapper_is(name: &str, function: EgclVal) -> bool {
    let Some(cached) = generic_wrapper_cache().borrow().values.get(name).copied() else {
        return false;
    };
    let cached = reified_closure(cached).unwrap_or(cached);
    let function = reified_closure(function).unwrap_or(function);
    cached == function
}

fn name_index(name: &str) -> Option<u32> {
    // A display key can collide with an interned spelling. The recorded
    // generic name owns the symbol identity, including a GENSYM's high ID.
    let generic = GENERIC_DEFINITIONS
        .borrow()
        .get(name)
        .map(|g| g.generic_function);
    generic
        .and_then(egcl_stdlib::clos::generic_function_name)
        .and_then(EgclVal::symbol_index)
        .or_else(|| reader::find_symbol_index(name))
}

/// No allocation or callback: this is safe on native accessor cache miss paths.
pub(super) fn current(name: &str) -> bool {
    if !GENERIC_DEFINITIONS.contains_key(name) && !METHOD_DEFINITIONS.contains_key(name) {
        return false;
    }
    if let Some(function) = setf_writer_replacement(name) {
        return cached_wrapper_is(name, function);
    }
    let Some(index) = name_index(name) else {
        return false;
    };
    let Some(function) = egcl_rt::symbols::symbol_function(index) else {
        return false;
    };
    function == EgclVal::from_symbol_index(index) || cached_wrapper_is(name, function)
}

/// A known generic's absent physical binding must not be reconstructed from
/// its retained metadata, including when its name also has a builtin fallback.
pub(super) fn undefined(name: &str) -> bool {
    if !GENERIC_DEFINITIONS.contains_key(name) && !METHOD_DEFINITIONS.contains_key(name) {
        return false;
    }
    if setf_writer_replacement(name).is_some() {
        return false;
    }
    name_index(name)
        .and_then(egcl_rt::symbols::symbol_function)
        .is_none_or(|function| function == egcl_rt::value::UNBOUND || function.is_nil())
}

/// Publish the native binding explicitly. DEFMETHOD may revive an unbound name,
/// but does not silently override an installed ordinary function. DEFGENERIC
/// passes FORCE after its normal definition checks to establish its definition.
pub(super) fn publish(name: &str, symbol: Option<u32>, force: bool) {
    let existing = name_index(name).and_then(installed_function);
    let setter = setf_writer_replacement(name);
    if !force && (existing.is_some() || setter.is_some() || current(name)) {
        return;
    }
    if force
        && let Some(place) = name
            .strip_prefix("(SETF ")
            .and_then(|s| s.strip_suffix(')'))
    {
        with_global_setf_fns(|table| {
            table.borrow_mut().remove(name);
        });
        if let Some(index) = reader::find_symbol_index(&setf_writer_symbol_name(place)) {
            egcl_rt::symbols::set_symbol_function(index, egcl_rt::value::UNBOUND);
            bytecode::clear_lazy_state(index);
        }
    }
    let index = symbol.unwrap_or_else(|| reader::intern_symbol(name));
    egcl_rt::symbols::set_symbol_function(index, EgclVal::from_symbol_index(index));
    bytecode::bump_direct_call_gen_for(index);
}
