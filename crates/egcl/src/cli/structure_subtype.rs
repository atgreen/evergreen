// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Positive, bounded Boolean-supertype proofs for raw structure classes.
use super::*;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 256;

#[derive(Clone, Copy)]
enum Boolean {
    And,
    Or,
    Not,
}

fn canonical_symbol(value: EgclVal, name: &str) -> bool {
    if name == "T" {
        return value == T;
    }
    reader::find_symbol_index(name).map(EgclVal::from_symbol_index) == Some(value)
}

fn boolean(form: EgclVal) -> Option<Boolean> {
    if !form.is_cons() {
        return None;
    }
    let head = cp(form).0;
    if canonical_symbol(head, "AND") {
        Some(Boolean::And)
    } else if canonical_symbol(head, "OR") {
        Some(Boolean::Or)
    } else if canonical_symbol(head, "NOT") {
        Some(Boolean::Not)
    } else {
        None
    }
}

/// No Lisp allocation occurs while checking the proper-list spine. A fixed
/// bound rejects cyclic spines without retaining pointer keys across GC.
fn operands(form: EgclVal) -> Option<Vec<EgclVal>> {
    let mut tail = cp(form).1;
    let mut values = Vec::new();
    while tail.is_cons() {
        if values.len() >= MAX_NODES {
            return None;
        }
        let (value, rest) = cp(tail);
        values.push(value);
        tail = rest;
    }
    tail.is_nil().then_some(values)
}

fn valid_tree(form: EgclVal, depth: usize, remaining: &mut usize) -> bool {
    if depth >= MAX_DEPTH || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    let Some(kind) = boolean(form) else {
        // This bounded proof supports class/type names and class objects as
        // leaves. Reject invalid atoms and unsupported compound syntax even
        // when an earlier OR arm could otherwise short-circuit to true.
        return form.is_symbol() || egcl_stdlib::clos::is_class_object(form);
    };
    egcl_rt::rooted!(
        parts = match operands(form) {
            Some(parts) => parts,
            None => return false,
        }
    );
    if matches!(kind, Boolean::Not) && parts.len() != 1 {
        return false;
    }
    for index in 0..parts.len() {
        if !valid_tree(parts[index], depth + 1, remaining) {
            return false;
        }
    }
    true
}

fn raw_class(spec: EgclVal) -> Option<EgclVal> {
    let class = if egcl_stdlib::clos::is_class_object(spec) {
        spec
    } else if spec.is_symbol() && !spec.is_nil() && spec != T {
        let class = egcl_stdlib::find_class(spec)?;
        // FIND-CLASS's legacy name fallback must not turn an uninterned or
        // recycled same-name symbol into this registered structure type.
        if egcl_stdlib::class_name(class) != spec {
            return None;
        }
        class
    } else {
        return None;
    };
    (egcl_stdlib::is_structure_class(class) && !egcl_stdlib::clos::is_forward_referenced(class))
        .then_some(class)
}

fn prove(class: EgclVal, spec: EgclVal, depth: usize, remaining: &mut usize) -> bool {
    if depth >= MAX_DEPTH || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    egcl_rt::rooted!(class = class);
    egcl_rt::rooted!(spec = spec);
    if let Some(kind) = boolean(*spec) {
        egcl_rt::rooted!(
            parts = match operands(*spec) {
                Some(parts) => parts,
                None => return false,
            }
        );
        return match kind {
            Boolean::And => {
                for index in 0..parts.len() {
                    if !prove(*class, parts[index], depth + 1, remaining) {
                        return false;
                    }
                }
                true
            }
            Boolean::Or => {
                for index in 0..parts.len() {
                    if prove(*class, parts[index], depth + 1, remaining) {
                        return true;
                    }
                }
                false
            }
            Boolean::Not if parts.len() == 1 => {
                egcl_rt::rooted!(
                    other = match raw_class(parts[0]) {
                        Some(other) => other,
                        None => return false,
                    }
                );
                // Raw DEFSTRUCT classes have single inclusion ancestry. Neither
                // inclusion direction may overlap before exclusion is proved.
                egcl_rt::rooted!(
                    left = match egcl_stdlib::compute_class_precedence_list(*class) {
                        Ok(cpl) => cpl,
                        Err(_) => return false,
                    }
                );
                egcl_rt::rooted!(
                    right = match egcl_stdlib::compute_class_precedence_list(*other) {
                        Ok(cpl) => cpl,
                        Err(_) => return false,
                    }
                );
                !left.contains(&*other) && !right.contains(&*class)
            }
            Boolean::Not => false,
        };
    }
    if canonical_symbol(*spec, "T")
        || canonical_symbol(*spec, "STRUCTURE-OBJECT")
        || canonical_symbol(*spec, "ATOM")
    {
        return true;
    }
    egcl_rt::rooted!(
        other = match raw_class(*spec) {
            Some(other) => other,
            None => return false,
        }
    );
    egcl_stdlib::compute_class_precedence_list(*class).is_ok_and(|cpl| cpl.contains(&*other))
}

/// Return only positive certainty. Recognized malformed/cyclic/over-budget
/// Boolean inputs stay unknown rather than entering older unbounded scans.
pub(super) fn raw_structure_boolean_subtypep(t1: EgclVal, t2: EgclVal) -> Option<(bool, bool)> {
    boolean(t2)?;
    egcl_rt::rooted!(class = raw_class(t1)?);
    egcl_rt::rooted!(spec = t2);
    let mut remaining = MAX_NODES;
    if !valid_tree(*spec, 0, &mut remaining) {
        return Some((false, false));
    }
    let mut remaining = MAX_NODES;
    Some(if prove(*class, *spec, 0, &mut remaining) {
        (true, true)
    } else {
        (false, false)
    })
}
