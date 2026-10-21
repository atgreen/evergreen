// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A cached, allocation-free binder for the common shapes of a variadic
//! lambda list (bliss-nkzx6).
//!
//! [`super::bind_variadic`] used to bind every call by re-parsing the lambda
//! list, binding it into a throwaway child environment with the tree-walker's
//! `bind_lambda_list`, and then looking each parameter back up by name. That
//! is general, but for `(defun kw (x &key a) ...)` it cost microseconds per
//! call, and it was the bulk of what made a mapped native `&key` entry slower
//! than the T0 call it replaced (GitHub #474).
//!
//! A [`VariadicPlan`] is parsed once per body and binds the same values
//! straight into the frame. It only ever *declines*: whenever a call needs
//! something it does not model -- a default form that must be evaluated, an
//! odd or non-symbol keyword tail, `:allow-other-keys` in the call, a keyword
//! it does not recognize, a wrong argument count -- it returns `None` before
//! writing anything, and the caller runs the general binder, which also owns
//! every error message. A keyword matches by the same bare-name rule as
//! `bind_lambda_list`; the plan only remembers which symbol last matched so
//! the common case is an identity comparison.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// The value an omitted parameter takes when no form has to be evaluated.
#[derive(Clone, Copy)]
enum Initial {
    /// An immediate constant (fixnum, character, single-float, or symbol):
    /// never relocated by the collector, so it can live outside the heap.
    Constant(EgclVal),
    /// Anything else. A call that needs it declines to the general binder.
    Form,
}

struct Optional {
    initial: Initial,
    supplied_p: bool,
}

struct Key {
    /// The bare keyword name, compared exactly as `bind_lambda_list` does.
    bare: String,
    /// Raw bits of the last symbol that matched `bare` (0 before any match).
    seen: AtomicU64,
    initial: Initial,
    supplied_p: bool,
}

pub(super) struct VariadicPlan {
    required: usize,
    optionals: Vec<Optional>,
    rest: bool,
    keys: Option<Vec<Key>>,
    allow_other_keys: bool,
    aux: Vec<Initial>,
    /// Number of bound variables, in `param_layout` order.
    values: usize,
}

#[cfg(test)]
thread_local! {
    /// Calls bound by the general binder, so tests can tell which binder ran.
    pub(super) static GENERAL_BINDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Make every plan decline, to compare it against the general binder.
    pub(super) static PLANS_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The largest plan bound with on-stack buffers; bigger ones use the heap.
const INLINE_VALUES: usize = 16;
/// Supplied keys are tracked in one word.
const MAX_KEYS: usize = 64;

fn initial(form: EgclVal) -> Initial {
    let immediate = |value: EgclVal| {
        value.is_fixnum() || value.is_character() || value.is_single_float() || value.is_symbol()
    };
    if form.is_fixnum() || form.is_character() || form.is_single_float() || form == NIL || form == T {
        return Initial::Constant(form);
    }
    if form.is_symbol() {
        // A keyword is a constant; any other symbol names a variable.
        return if super::super::is_keyword_arg(form) {
            Initial::Constant(form)
        } else {
            Initial::Form
        };
    }
    if form.is_cons() {
        let (head, tail) = cp(form);
        if head.is_symbol() && super::super::sym_name_rc(head).as_ref() == "QUOTE" && tail.is_cons() {
            let (quoted, end) = cp(tail);
            if end == NIL && immediate(quoted) {
                return Initial::Constant(quoted);
            }
        }
    }
    Initial::Form
}

impl VariadicPlan {
    /// The plan for `func`, parsed on first use and cached on the body.
    pub(super) fn of(func: &BytecodeFunction) -> Option<&VariadicPlan> {
        func.binder_cache
            .get_or_init(|| {
                Self::parse(func).map(|plan| {
                    std::sync::Arc::new(plan) as std::sync::Arc<dyn std::any::Any + Send + Sync>
                })
            })
            .and_then(|plan| plan.downcast_ref::<VariadicPlan>())
    }

    /// Parse an ordinary lambda list, or `None` for any shape left to the
    /// general binder. Parsing reads the list and allocates no Lisp objects.
    fn parse(func: &BytecodeFunction) -> Option<VariadicPlan> {
        #[derive(PartialEq)]
        enum Mode {
            Required,
            Optional,
            Rest,
            Key,
            Aux,
        }
        let mut plan = VariadicPlan {
            required: 0,
            optionals: Vec::new(),
            rest: false,
            keys: None,
            allow_other_keys: false,
            aux: Vec::new(),
            values: 0,
        };
        let mut mode = Mode::Required;
        let mut cursor = func.params_form;
        while cursor.is_cons() {
            let (element, next) = cp(cursor);
            cursor = next;
            if element.is_symbol() {
                let name = super::super::sym_name_rc(element);
                let next_mode = match name.as_ref() {
                    "&OPTIONAL" if mode == Mode::Required => Some(Mode::Optional),
                    "&REST" | "&BODY" if !plan.rest && plan.keys.is_none() && mode != Mode::Aux => {
                        Some(Mode::Rest)
                    }
                    "&KEY" if plan.keys.is_none() && mode != Mode::Aux => Some(Mode::Key),
                    "&AUX" if mode != Mode::Aux => Some(Mode::Aux),
                    "&ALLOW-OTHER-KEYS" if mode == Mode::Key => {
                        plan.allow_other_keys = true;
                        continue;
                    }
                    _ if name.starts_with('&') => return None,
                    _ => None,
                };
                if let Some(next_mode) = next_mode {
                    if next_mode == Mode::Key {
                        plan.keys = Some(Vec::new());
                    }
                    mode = next_mode;
                    continue;
                }
            }
            match mode {
                Mode::Required => {
                    if !element.is_symbol() {
                        return None;
                    }
                    plan.required += 1;
                    plan.values += 1;
                }
                Mode::Optional => {
                    if !(element.is_symbol() || element.is_cons()) {
                        return None;
                    }
                    let (_, form, supplied) = super::super::parse_var_spec(element);
                    plan.optionals.push(Optional {
                        initial: initial(form),
                        supplied_p: supplied.is_some(),
                    });
                    plan.values += 1 + usize::from(supplied.is_some());
                }
                Mode::Rest => {
                    if plan.rest || !element.is_symbol() {
                        return None;
                    }
                    plan.rest = true;
                    plan.values += 1;
                }
                Mode::Key => {
                    if !(element.is_symbol() || element.is_cons()) {
                        return None;
                    }
                    let (bare, _, form, supplied) = super::super::parse_key_spec(element);
                    let keys = plan.keys.as_mut()?;
                    if bare.is_empty() || bare == "ALLOW-OTHER-KEYS"
                        || keys.iter().any(|key| key.bare == bare) || keys.len() == MAX_KEYS
                    {
                        return None;
                    }
                    keys.push(Key {
                        bare,
                        seen: AtomicU64::new(0),
                        initial: initial(form),
                        supplied_p: supplied.is_some(),
                    });
                    plan.values += 1 + usize::from(supplied.is_some());
                }
                Mode::Aux => {
                    if !(element.is_symbol() || element.is_cons()) {
                        return None;
                    }
                    let (_, form, _) = super::super::parse_var_spec(element);
                    plan.aux.push(initial(form));
                    plan.values += 1;
                }
            }
        }
        // A dotted lambda list, or one whose variables the compiler laid out
        // differently, stays with the general binder.
        if cursor != NIL || mode == Mode::Rest && !plan.rest || plan.values != func.param_layout.len() {
            return None;
        }
        Some(plan)
    }

    /// Bind `args` into `frame` exactly as the general binder would, or return
    /// `false`, having written nothing, when this call needs the general one.
    ///
    /// Allocates a Lisp object only for a `&rest` list, after every argument
    /// has been rooted.
    pub(super) fn bind(
        &self,
        func: &BytecodeFunction,
        frame: *mut Frame,
        args: &[EgclVal],
        env_frame: Option<&Arc<SharedCell<EnvFrame>>>,
    ) -> bool {
        #[cfg(test)]
        if PLANS_DISABLED.with(std::cell::Cell::get) {
            return false;
        }
        let positional = self.required + self.optionals.len();
        if args.len() < self.required || args.len() > usize::from(u16::MAX)
            || self.keys.is_none() && !self.rest && args.len() > positional
        {
            return false;
        }
        let tail_start = args.len().min(positional);
        // Match the keyword tail before allocating anything.
        let mut supplied: u64 = 0;
        // Where each supplied key's value sits in `args`.
        let mut key_values = [0u16; MAX_KEYS];
        if let Some(keys) = &self.keys {
            let tail = &args[tail_start..];
            if tail.len() % 2 != 0 {
                return false;
            }
            for (pair_index, pair) in tail.chunks_exact(2).enumerate() {
                let key = pair[0];
                if !key.is_symbol() {
                    return false;
                }
                let index = match keys.iter().position(|spec| spec.seen.load(Ordering::Relaxed) == key.0) {
                    Some(index) => Some(index),
                    None => {
                        let bare = super::super::key_bare(key);
                        if bare == "ALLOW-OTHER-KEYS" {
                            return false;
                        }
                        let index = keys.iter().position(|spec| spec.bare == bare);
                        if let Some(index) = index {
                            keys[index].seen.store(key.0, Ordering::Relaxed);
                        } else if !self.allow_other_keys {
                            return false;
                        }
                        index
                    }
                };
                // The first occurrence of a keyword supplies it (CLHS 3.4.1.4).
                if let Some(index) = index {
                    if supplied & (1 << index) == 0 {
                        supplied |= 1 << index;
                        key_values[index] = (tail_start + 2 * pair_index + 1) as u16;
                    }
                }
            }
            for (index, key) in keys.iter().enumerate() {
                if supplied & (1 << index) == 0 && matches!(key.initial, Initial::Form) {
                    return false;
                }
            }
        }
        for (index, optional) in self.optionals.iter().enumerate() {
            if self.required + index >= args.len() && matches!(optional.initial, Initial::Form) {
                return false;
            }
        }
        if self.aux.iter().any(|initial| matches!(initial, Initial::Form)) {
            return false;
        }

        // Root everything that must survive the `&rest` list's allocation.
        let mut rest = NIL;
        egcl_rt::rooted_ref!(_rest = &mut rest);
        let mut kept: Vec<EgclVal> = Vec::new();
        egcl_rt::rooted_ref!(_kept = &mut kept);
        let args = if self.rest {
            kept.extend_from_slice(args);
            rest = super::super::vec_to_list(&kept[tail_start..]);
            &kept[..]
        } else {
            args
        };

        let mut inline = [NIL; INLINE_VALUES];
        let mut spilled = Vec::new();
        let values: &mut [EgclVal] = if self.values <= INLINE_VALUES {
            &mut inline[..self.values]
        } else {
            spilled.resize(self.values, NIL);
            &mut spilled
        };
        let mut at = 0;
        let mut push = |value: EgclVal| {
            values[at] = value;
            at += 1;
        };
        for &arg in &args[..self.required] {
            push(arg);
        }
        for (index, optional) in self.optionals.iter().enumerate() {
            match args.get(self.required + index) {
                Some(&arg) => {
                    push(arg);
                    if optional.supplied_p {
                        push(T);
                    }
                }
                None => {
                    let Initial::Constant(value) = optional.initial else { unreachable!() };
                    push(value);
                    if optional.supplied_p {
                        push(NIL);
                    }
                }
            }
        }
        if self.rest {
            push(rest);
        }
        for (index, key) in self.keys.iter().flatten().enumerate() {
            if supplied & (1 << index) != 0 {
                push(args[usize::from(key_values[index])]);
                if key.supplied_p {
                    push(T);
                }
            } else {
                let Initial::Constant(value) = key.initial else { unreachable!() };
                push(value);
                if key.supplied_p {
                    push(NIL);
                }
            }
        }
        for initial in &self.aux {
            let Initial::Constant(value) = *initial else { unreachable!() };
            push(value);
        }
        debug_assert_eq!(at, self.values);

        // Nothing below allocates a Lisp object.
        for ((name, location), &value) in func.param_layout.iter().zip(values.iter()) {
            match location {
                VarLoc::Slot(slot) => unsafe { slot_set(frame, *slot, value) },
                VarLoc::Boxed => bind_boxed_param(
                    env_frame.expect("boxed variadic parameter without a heap EnvFrame"),
                    name,
                    value,
                ),
            }
        }
        true
    }
}
