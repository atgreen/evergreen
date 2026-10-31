// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Compile-time proof for the two compiler-owned method lexical functions.
//! State contains identities/keys only, never unrooted Lisp values. No source
//! scan establishes absence: only successfully completed scoped lowering does.
use super::*;
use std::collections::HashSet;

pub(super) mod helper_values;

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
    lexical_opaque: bool,
    generated: usize,
    saw_bindings: bool,
}
struct Session {
    spec: Spec,
    attempt: Option<Attempt>,
    unused: bool,
    /// The generated helper values were proved unreachable and elided: the
    /// body never reads the method context or its helpers lexically.
    helpers_elided: bool,
    raw_reader: Option<RawReaderSpec>,
    helper_sites: Option<helper_values::Sites>,
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
                helpers_elided: false,
                raw_reader: None,
                helper_sites: None,
            }))
        }))
    }
    pub fn raw_reader(&self) -> Option<RawReaderSpec> {
        SESSION.with(|slot| slot.borrow().as_ref().and_then(|s| s.raw_reader.clone()))
    }
    pub fn unused(&self) -> bool {
        SESSION.with(|slot| slot.borrow().as_ref().is_some_and(|s| s.unused))
    }
    /// Whether the compiled body is proved never to read the method context
    /// or its CALL-NEXT-METHOD/NEXT-METHOD-P helpers lexically, either by the
    /// complete [`Self::unused`] proof or by helper elision. The dynamic
    /// context may still be needed by open calls.
    pub fn lexically_unused(&self) -> bool {
        SESSION.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|s| s.unused || s.helpers_elided)
        })
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
        if !found {
            with_attempt(|_, attempt| {
                if attempt.generated == 0 {
                    attempt.lexical_opaque = true;
                }
            });
        }
        let owned = found
            && SESSION.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return false;
                };
                if let Some(a) = s.attempt.as_mut() {
                    a.unknown = true;
                    a.lexical_opaque = true;
                    false
                } else {
                    s.attempt = Some(Attempt::default());
                    s.unused = false;
                    s.helpers_elided = false;
                    s.raw_reader = None;
                    s.helper_sites = None;
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
        // Open calls need the dynamic method context. They do not by themselves
        // export the activation's typed lexical function slots to ordinary Env.
        with_attempt(|_, attempt| {
            if attempt.generated == 0 {
                attempt.unknown = true;
            }
        });
    }
    if [
        "EVAL",
        "COMPILE",
        "MACROEXPAND",
        "MACROEXPAND-1",
        "MACROEXPAND-ALL",
        "GET-SETF-EXPANSION",
    ]
    .iter()
    .any(|name| reader::find_symbol_index(name) == Some(symbol))
    {
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
            a.lexical_opaque = true;
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
/// Compiler-owned sites are kept only until final immutable-body verification.
#[derive(Clone)]
pub(super) struct RawReaderSpec {
    pub slot: u32,
    pub primitive: u32,
    pub helpers: [u32; 3],
    initializer: usize,
    helper_values: [usize; 2],
    bindings: [String; 3],
}

impl RawReaderSpec {
    pub fn matches(&self, body: &BytecodeFunction) -> bool {
        self.read_slot(&body.code, &body.constants, &body.names) == Some(self.slot)
    }

    fn read_slot(&self, code: &[Instr], constants: &[EgclVal], names: &[String]) -> Option<u32> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Value {
            Argument,
            Symbol(u32),
            Generated,
            Result,
        }
        let mut stack = Vec::new();
        let mut result = None;
        let mut depth = 0usize;
        let mut blocks = Vec::new();
        for (pc, instruction) in code.iter().enumerate() {
            match *instruction {
                Instr::CallNamed { sym, nargs: 0 }
                    if pc == self.initializer && sym == self.helpers[0] =>
                {
                    stack.push(Value::Generated)
                }
                Instr::MakeClosure { .. } if self.helper_values.contains(&pc) => {
                    stack.push(Value::Generated)
                }
                Instr::Const(index) => stack.push(Value::Symbol(
                    constants.get(index as usize)?.symbol_index()?,
                )),
                Instr::LoadLocal(0) => stack.push(Value::Argument),
                Instr::DefineEnvVar(index)
                    if depth != 0 && self.bindings.contains(names.get(index as usize)?) =>
                {
                    if stack.pop()? != Value::Generated {
                        return None;
                    }
                }
                Instr::PushEnvChild => depth += 1,
                Instr::PopEnvChild => depth = depth.checked_sub(1)?,
                Instr::PushBlock {
                    resume_bcp,
                    sp_restore,
                    register: false,
                    ..
                } if usize::from(sp_restore) == stack.len() => blocks.push(resume_bcp),
                Instr::PopHandler if blocks.pop()? as usize == pc + 1 => {}
                Instr::ClearMv => {}
                Instr::CallNamed { sym, nargs: 2 } if sym == self.primitive && result.is_none() => {
                    let Value::Symbol(slot) = stack.pop()? else {
                        return None;
                    };
                    if stack.pop()? != Value::Argument {
                        return None;
                    }
                    result = Some(slot);
                    stack.push(Value::Result);
                }
                Instr::Return if pc + 1 == code.len() && depth == 0 && blocks.is_empty() => {
                    if stack.pop()? != Value::Result || !stack.is_empty() {
                        return None;
                    }
                    return result;
                }
                _ => return None,
            }
        }
        None
    }
}

fn raw_reader(initializer: Option<usize>, lo: &Lowerer<'_>) -> Option<RawReaderSpec> {
    let (spec, saw_bindings) = with_attempt(|spec, attempt| (spec.clone(), attempt.saw_bindings))?;
    if !saw_bindings {
        return None;
    }
    // During cold boot the reader can create the exact canonical registry
    // identity before the package's present-symbol table adopts it. Do not
    // require that later adoption, and never accept a single-colon lookalike.
    let primitive = reader::find_symbol_index("EGCL-INTERNAL::%RAW-SLOT-VALUE")?;
    let package = egcl_stdlib::find_package("EGCL-INTERNAL")?;
    if egcl_stdlib::find_present_symbol(package, "%RAW-SLOT-VALUE")
        .is_some_and(|symbol| symbol.symbol_index() != Some(primitive))
    {
        return None;
    }
    let mut proof = RawReaderSpec {
        slot: 0,
        primitive,
        helpers: spec.helpers,
        initializer: initializer?,
        helper_values: lo.method_context_helper_values.as_slice().try_into().ok()?,
        bindings: [
            spec.context_key,
            closure_fn_binding_key("CALL-NEXT-METHOD"),
            closure_fn_binding_key("NEXT-METHOD-P"),
        ],
    };
    proof.slot = proof.read_slot(&lo.code, &lo.constants, &lo.names)?;
    Some(proof)
}

pub(super) fn finish(initializer: Option<usize>, lo: &mut Lowerer<'_>) {
    let raw_reader = raw_reader(initializer, lo);
    let helper_sites = initializer.and_then(|initializer| {
        Some(helper_values::Sites {
            initializer,
            creations: lo.method_context_helper_values.as_slice().try_into().ok()?,
        })
    });
    #[cfg(test)]
    if std::env::var_os("EGCL_RAW_READER_PROOF_DIAGNOSTICS").is_some() {
        let primitive = reader::find_symbol_index("EGCL-INTERNAL::%RAW-SLOT-VALUE");
        if lo.code.iter().any(|instruction| {
            matches!(instruction,
            Instr::CallNamed { sym, .. } if Some(*sym) == primitive)
        }) {
            let metadata = with_attempt(|spec, attempt| {
                (
                    spec.context_key.clone(),
                    spec.helpers,
                    attempt.saw_bindings,
                    attempt.used,
                    attempt.unknown,
                )
            });
            eprintln!(
                "[raw-reader-proof] owner {:?} primitive {:?} initializer {:?} helper-sites {:?} metadata {:?} candidate-slot {:?}",
                lo.self_symbol,
                primitive,
                initializer,
                lo.method_context_helper_values,
                metadata,
                raw_reader.as_ref().map(|proof| proof.slot)
            );
        }
    }
    SESSION.with(|slot| {
        if let Some(session) = slot.borrow_mut().as_mut() {
            session.raw_reader = raw_reader;
            session.helper_sites = helper_sites;
        }
    });
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

pub(super) fn elide_helper_values(body: &mut BytecodeFunction) {
    let candidate = SESSION.with(|slot| {
        let slot = slot.borrow();
        let session = slot.as_ref()?;
        let attempt = session.attempt.as_ref()?;
        #[cfg(test)]
        if std::env::var_os("EGCL_METHOD_HELPER_PROOF_DIAGNOSTICS").is_some() {
            eprintln!(
                "[method-helper-proof] {} unused={} raw_reader={} used={} opaque={} generated={} bindings={} sites={:?} variadic={} nested={} names={:?} prefix={:?}",
                body.name, session.unused, session.raw_reader.is_some(), attempt.used,
                attempt.lexical_opaque, attempt.generated, attempt.saw_bindings,
                session.helper_sites.as_ref().map(|sites| (sites.initializer, sites.creations)),
                body.variadic, body.nested_functions.len(), body.names,
                &body.code[..body.code.len().min(9)]
            );
        }
        (!session.unused
            && session.raw_reader.is_none()
            && !attempt.used
            && !attempt.lexical_opaque
            && attempt.saw_bindings)
            .then(|| (session.spec.clone(), session.helper_sites.clone()))
    });
    if let Some((spec, Some(sites))) = candidate {
        helper_values::elide(body, &spec, &sites);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{heap_test_lock, method_continuations, read_eval_all_env};

    fn compiled_case(source: &str, expected_unused: bool, setup: Option<&str>) {
        compiled_case_with(source, expected_unused, !expected_unused, setup);
    }

    /// `initializer_retained` is false both for a complete proof and for a body
    /// that never reads the generated context or helpers lexically: that body
    /// is compiled without them (its caller still establishes the dynamic
    /// context), so neither the initializer nor the helpers remain.
    fn compiled_case_with(
        source: &str,
        expected_unused: bool,
        initializer_retained: bool,
        setup: Option<&str>,
    ) {
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
        let (function, proof, _) = compile_and_reify_method_lambda(
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
        assert_eq!(bytecode.code.iter().any(|instruction| matches!(instruction, Instr::CallNamed { sym, nargs: 0 } if *sym == initializer)), initializer_retained,
            "only the exact generated initializer may be removed: {source}");
        if !initializer_retained {
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

    fn helper_case(source: &str, expected_elided: bool, setup: Option<&str>) {
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
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
        let (function, context_proof, _) = compile_and_reify_method_lambda(
            "(METHOD LEXICAL-HELPERS)",
            *params,
            *body,
            &env,
            metadata,
        )
        .unwrap();
        egcl_rt::rooted!(function = function);
        let (_, body) =
            bytecode_for_callable(egcl_rt::function::definition_index(*function), *function)
                .unwrap();
        assert!(
            context_proof.is_none(),
            "open/opaque calls must retain dynamic context: {source}"
        );
        // A body whose helper values are unreachable is compiled without the
        // generated bindings at all; otherwise the initializer stays.
        assert_eq!(
            body.code.iter().any(|instruction| matches!(instruction,
            Instr::CallNamed { sym, nargs: 0 } if *sym == initializer)),
            !expected_elided,
            "initializer retained exactly when helpers are: {source}"
        );
        let creations = body
            .code
            .iter()
            .filter(|i| matches!(i, Instr::MakeClosure { .. }))
            .count();
        if expected_elided {
            assert_eq!(creations, 0, "{source}: {:?}", body.code);
        } else {
            assert!(
                creations >= 2,
                "opaque/reference case retains helpers: {source}"
            );
        }
        crate::cli::set_compiled_method_cell(*function, false);
    }

    #[test]
    fn open_calls_drop_only_unused_lexical_values_not_dynamic_context() {
        helper_case(
            "(elide-open object)",
            true,
            Some("(defun elide-open (object) object)"),
        );
        helper_case(
            "(elide-foreign:call-next-method object)",
            true,
            Some(
                "(defpackage :elide-foreign (:use :cl) (:shadow :call-next-method) (:export :call-next-method)) (defun elide-foreign:call-next-method (x) x)",
            ),
        );
    }

    #[test]
    fn opaque_host_nested_and_first_class_continuations_keep_values() {
        helper_case("(eval '(function cl:call-next-method))", false, None);
        helper_case(
            "(compile nil '(lambda () #'cl:call-next-method))",
            false,
            None,
        );
        helper_case("(funcall #'cl:call-next-method)", false, None);
        helper_case("#'(lambda () (cl:call-next-method))", false, None);
        helper_case("(funcall #'(lambda (x) x) object)", false, None);
        helper_case(
            "(hidden-continuation)",
            false,
            Some("(defmacro hidden-continuation () '(function cl:call-next-method))"),
        );
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
        compiled_case_with(
            "(method-context-foreign:eval object)",
            false,
            false,
            Some(
                "(defpackage :method-context-foreign (:use :cl) (:shadow :eval) (:export :eval)) (defun method-context-foreign:eval (object) object)",
            ),
        );
        compiled_case_with(
            "(method-context-plain object)",
            false,
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
