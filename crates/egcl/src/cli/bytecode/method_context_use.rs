// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Compile-time proof for the two compiler-owned method lexical functions.
//! State contains identities/keys only, never unrooted Lisp values. No source
//! scan establishes absence: only successfully completed scoped lowering does.
use super::*;
use std::collections::HashSet;

#[derive(Clone)]
pub(in crate::cli) struct Spec {
    pub context_key: String,
    pub marker: u32,
    pub initializer: u32,
    pub helpers: [u32; 3],
}
pub(super) type Bindings = HashMap<String, bool>;
#[derive(Default)]
struct Attempt {
    bindings: Bindings,
    used: bool,
    unknown: bool,
    generated: usize,
    saw_bindings: bool,
}
struct Session {
    spec: Spec,
    attempt: Option<Attempt>,
    unused: bool,
}
static SESSION: egcl_rt::execution_local::ExecutionLocal<RefCell<Option<Session>>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { RefCell::new(None) }) };
fn with_attempt<R>(f: impl FnOnce(&Spec, &mut Attempt) -> R) -> Option<R> {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let session = slot.as_mut()?;
        Some(f(&session.spec, session.attempt.as_mut()?))
    })
}
// Only audited canonical CL definitions may be context-independent. Their
// standard definitions cannot be replaced with arbitrary env-reading aliases
// by a conforming program. Mutable/user/foreign calls remain Unknown even when
// their current target looks harmless: an earlier call can rebind a later one.
const CLOSED_CL_NAMES: &[&str] = &[
    "SLOT-VALUE",
    "REMOVE-METHOD",
    "CAR",
    "CDR",
    "CONS",
    "LIST",
    "LIST*",
    "VALUES",
    "VALUES-LIST",
    "EQ",
    "EQL",
    "NULL",
    "NOT",
    "CONSP",
    "SYMBOLP",
    "NUMBERP",
    "REALP",
    "INTEGERP",
    "FLOATP",
    "+",
    "-",
    "*",
    "/",
    "1+",
    "1-",
    "=",
    "/=",
    "<",
    "<=",
    ">",
    ">=",
    "LENGTH",
    "AREF",
    "SVREF",
];
pub(super) struct Scope(Option<Session>);
impl Scope {
    pub fn enter(spec: Spec) -> Self {
        Self(SESSION.with(|slot| {
            slot.replace(Some(Session {
                spec,
                attempt: None,
                unused: false,
            }))
        }))
    }
    pub fn unused(&self) -> bool {
        SESSION.with(|slot| slot.borrow().as_ref().is_some_and(|s| s.unused))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        SESSION.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}

fn proper_items(mut form: EgclVal) -> Option<Vec<EgclVal>> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    while form.is_cons() {
        if !seen.insert(form.to_raw()) || seen.len() > 100_000 {
            return None;
        }
        let (head, tail) = cp(form);
        items.push(head);
        form = tail;
    }
    form.is_nil().then_some(items)
}
// This identifies the exact generated initializer binding, not whether the
// user body uses it. The marker is a fresh uninterned identity supplied by the
// method compiler; a capped/malformed walk never produces a proof.
pub(super) struct AttemptScope(bool);
impl AttemptScope {
    pub fn enter(body: EgclVal) -> Self {
        let key = SESSION.with(|slot| slot.borrow().as_ref().map(|s| s.spec.context_key.clone()));
        let Some(key) = key else {
            return Self(false);
        };
        let mut pending = vec![body];
        let mut seen = HashSet::new();
        let mut found = false;
        while let Some(form) = pending.pop() {
            if !form.is_cons() || !seen.insert(form.to_raw()) {
                continue;
            }
            if seen.len() > 100_000 {
                return Self(false);
            }
            let (head, tail) = cp(form);
            if head.is_symbol() && sym_name(head) == "LET" {
                let (bindings, _) = cp(tail);
                let Some(bindings) = proper_items(bindings) else {
                    return Self(false);
                };
                for binding in bindings {
                    if binding.is_cons()
                        && cp(binding).0.is_symbol()
                        && var_name(cp(binding).0) == key
                    {
                        found = true;
                    }
                }
            }
            pending.push(head);
            pending.push(tail);
        }
        let owned = found
            && SESSION.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return false;
                };
                if let Some(a) = s.attempt.as_mut() {
                    a.unknown = true;
                    false
                } else {
                    s.attempt = Some(Attempt::default());
                    s.unused = false;
                    true
                }
            });
        Self(owned)
    }
}
impl Drop for AttemptScope {
    fn drop(&mut self) {
        if self.0 {
            SESSION.with(|slot| {
                if let Some(s) = slot.borrow_mut().as_mut() {
                    s.attempt = None;
                }
            });
        }
    }
}
fn generated_definition(definition: EgclVal, spec: &Spec) -> bool {
    let (_, tail) = cp(definition);
    let (_, body) = cp(tail);
    for form in list_to_vec(body) {
        if !form.is_cons() || !cp(form).0.is_symbol() || sym_name(cp(form).0) != "DECLARE" {
            break;
        }
        for declaration in list_to_vec(cp(form).1) {
            if declaration.is_cons() {
                let (name, args) = cp(declaration);
                if name.symbol_index() == Some(spec.marker) && args.is_nil() {
                    return true;
                }
            }
        }
    }
    false
}
pub(super) struct GeneratedScope(bool);
impl GeneratedScope {
    pub fn enter(definition: EgclVal) -> Self {
        Self(
            with_attempt(|spec, a| {
                let generated = generated_definition(definition, spec);
                if generated {
                    a.generated += 1;
                }
                generated
            })
            .unwrap_or(false),
        )
    }
}
impl Drop for GeneratedScope {
    fn drop(&mut self) {
        if self.0 {
            with_attempt(|_, a| a.generated -= 1);
        }
    }
}
pub(super) fn bindings() -> Bindings {
    with_attempt(|_, a| a.bindings.clone()).unwrap_or_default()
}
pub(super) struct BindingScope(Option<Bindings>);
impl BindingScope {
    pub fn replace(bindings: Bindings) -> Self {
        Self(with_attempt(|_, a| {
            std::mem::replace(&mut a.bindings, bindings)
        }))
    }
    pub fn enter(definitions: &[EgclVal]) -> Self {
        let old = with_attempt(|spec, a| {
            let old = a.bindings.clone();
            for &definition in definitions {
                let name = sym_name(cp(definition).0);
                if matches!(name.as_str(), "CALL-NEXT-METHOD" | "NEXT-METHOD-P") {
                    let generated = generated_definition(definition, spec);
                    a.saw_bindings |= generated;
                    a.bindings.insert(name, generated);
                }
            }
            old
        });
        Self(old)
    }
}
impl Drop for BindingScope {
    fn drop(&mut self) {
        if let Some(old) = self.0.take() {
            with_attempt(|_, a| a.bindings = old);
        }
    }
}
pub(super) fn reference(name: &str) {
    with_attempt(|_, a| {
        if a.generated == 0 && a.bindings.get(name) == Some(&true) {
            a.used = true;
        }
    });
}
pub(super) fn open_call(symbol: u32) {
    let active = with_attempt(|_, a| a.generated == 0).unwrap_or(false);
    if !active {
        return;
    }
    let private = with_attempt(|spec, _| spec.helpers.contains(&symbol)).unwrap_or(false);
    let closed =
        egcl_rt::symbols::symbol_name_of(EgclVal::from_symbol_index(symbol)).is_some_and(|name| {
            CLOSED_CL_NAMES.contains(&name.as_str())
                && egcl_rt::symbols::find_index(&name) == Some(symbol)
        });
    if private || !closed {
        unknown();
    }
}
pub(super) fn generated_helper() -> bool {
    with_attempt(|_, a| a.generated != 0).unwrap_or(false)
}
pub(super) fn unknown() {
    with_attempt(|_, a| {
        if a.generated == 0 {
            a.unknown = true;
        }
    });
}
pub(super) fn initializer_binding(key: &str) -> bool {
    with_attempt(|spec, a| a.generated == 0 && key == spec.context_key).unwrap_or(false)
}
pub(super) fn initializer_instruction(instruction: &Instr) -> bool {
    with_attempt(|spec, a| {
        a.generated == 0
            && matches!(instruction, Instr::CallNamed { sym, nargs: 0 } if *sym == spec.initializer)
    })
    .unwrap_or(false)
}
pub(super) fn finish(initializer: Option<usize>, lo: &mut Lowerer<'_>) {
    let unused =
        with_attempt(|_, a| !a.used && !a.unknown && a.saw_bindings && initializer.is_some())
            .unwrap_or(false);
    if unused {
        let index = initializer.unwrap();
        let nil = if let Some(existing) = lo.constants.iter().position(|value| value.is_nil()) {
            let Ok(existing) = u16::try_from(existing) else {
                return;
            };
            existing
        } else {
            if lo.constants.len() > u16::MAX as usize {
                return;
            }
            lo.add_const(NIL)
        };
        lo.code[index] = Instr::Const(nil);
        // These exact creation sites belong to controlled generated helper
        // bindings. With complete non-use proved, creating their function
        // objects is a private allocation effect only. Keep stack/slot layout.
        for &helper in &lo.method_context_helper_values {
            lo.code[helper] = Instr::Const(nil);
        }
        lo.notinline_sites.retain(|site| *site != index as u32);
        lo.self_call_sites.retain(|site| *site != index as u32);
        SESSION.with(|slot| {
            if let Some(s) = slot.borrow_mut().as_mut() {
                s.unused = true;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{heap_test_lock, method_continuations, read_eval_all_env};

    fn compiled_case(source: &str, expected_unused: bool, setup: Option<&str>) {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        if let Some(setup) = setup {
            read_eval_all_env(setup, &mut env).unwrap();
        }
        egcl_rt::rooted!(expression = reader::read_from_string(source).unwrap().0);
        egcl_rt::rooted!(body = arena_cons(*expression, NIL));
        egcl_rt::rooted!(params = reader::read_from_string("(object)").unwrap().0);
        let mut metadata = None;
        *body = method_continuations::wrap_body(*body, &mut metadata).unwrap();
        let metadata = metadata.unwrap();
        let initializer = metadata.initializer;
        let (function, proof) = compile_and_reify_method_lambda(
            "(METHOD CONTEXT-PROOF)",
            *params,
            *body,
            &env,
            metadata,
        )
        .expect("regression must complete real scoped lowering");
        egcl_rt::rooted!(function = function);
        let (_, bytecode) =
            bytecode_for_callable(egcl_rt::function::definition_index(*function), *function)
                .unwrap();
        assert_eq!(proof.is_some(), expected_unused, "{source}");
        assert_eq!(bytecode.code.iter().any(|instruction| matches!(instruction, Instr::CallNamed { sym, nargs: 0 } if *sym == initializer)), !expected_unused,
            "only the exact generated initializer may be removed: {source}");
        if expected_unused {
            assert!(
                !bytecode
                    .code
                    .iter()
                    .any(|instruction| matches!(instruction, Instr::MakeClosure { .. })),
                "constant/slot/shadow fixture must not allocate unused generated functions"
            );
        }
        crate::cli::set_compiled_method_cell(*function, false);
    }

    #[test]
    fn constant_and_slot_methods_prove_unused_despite_generated_rest_helper() {
        compiled_case("42", true, None);
        compiled_case("(slot-value object 'cell)", true, None);
    }
    #[test]
    fn final_macroexpansion_and_first_class_or_nested_references_keep_context() {
        compiled_case(
            "(produced-next)",
            false,
            Some("(defmacro produced-next () '(function cl:call-next-method))"),
        );
        compiled_case(
            "(macrolet ((produced () '(cl:call-next-method))) (produced))",
            false,
            None,
        );
        compiled_case("#'cl:call-next-method", false, None);
        compiled_case("#'(lambda () (cl:call-next-method))", false, None);
    }
    #[test]
    fn shadowed_same_named_function_is_a_different_lexical_binding() {
        compiled_case(
            "(flet ((cl:call-next-method () 12)) (cl:call-next-method))",
            true,
            None,
        );
    }
    #[test]
    fn runtime_eval_and_macroexpansion_keep_unknown_context() {
        compiled_case("(eval '(function cl:call-next-method))", false, None);
        compiled_case(
            "(macroexpand '(runtime-next))",
            false,
            Some("(defmacro runtime-next () '(function cl:call-next-method))"),
        );
    }
    #[test]
    fn mutable_foreign_calls_are_unknown_regardless_of_leaf_spelling() {
        compiled_case(
            "(method-context-foreign:eval object)",
            false,
            Some(
                "(defpackage :method-context-foreign (:use :cl) (:shadow :eval) (:export :eval)) (defun method-context-foreign:eval (object) object)",
            ),
        );
        compiled_case(
            "(method-context-plain object)",
            false,
            Some("(defun method-context-plain (object) object)"),
        );
    }
    #[test]
    fn user_host_closure_fallback_is_unknown_not_unused() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(expression = reader::read_from_string("#'(lambda () 7)").unwrap().0);
        egcl_rt::rooted!(body = arena_cons(*expression, NIL));
        let mut metadata = None;
        *body = method_continuations::wrap_body(*body, &mut metadata).unwrap();
        let scope = Scope::enter(metadata.unwrap());
        let bytecode =
            compile_function("(METHOD HOST-CONTEXT)", NIL, *body, &env, false, false).unwrap();
        assert!(
            bytecode.code.iter().any(|instruction| matches!(
                instruction,
                Instr::EvalHost(_) | Instr::MakeClosureEnv(_)
            )),
            "the fixture must exercise actual host fallback"
        );
        assert!(!scope.unused());
    }
}
