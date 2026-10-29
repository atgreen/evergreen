// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Lexical next-method closures capture ordinary Lisp data, not stack addresses
//! or numeric registry tokens. Existing closure/frame/core tracing therefore
//! owns the entire continuation, including removed methods and argument values.
use super::*;

#[derive(Clone)]
pub(super) struct Invocation {
    gf: EgclVal,
    name: Arc<str>,
    methods: Arc<SharedCell<Vec<MethodDef>>>,
    // Method IDs are immediate meta handles, never moving heap pointers.
    expected: Arc<Vec<EgclVal>>,
    precedence: Arc<Vec<usize>>,
    lambda_list: EgclVal,
    encoded: Arc<SharedCell<EgclVal>>,
}
impl Invocation {
    pub(super) fn trace_with(
        &mut self,
        state: &mut EnvRootVisitState,
        visit: &mut dyn FnMut(*mut EgclVal),
    ) {
        visit(&mut self.gf);
        visit(&mut self.lambda_list);
        if state
            .method_invocations
            .insert(Arc::as_ptr(&self.methods) as usize)
        {
            for method in self.methods.borrow_mut().iter_mut() {
                visit_method_def_roots(method, state, visit);
            }
            visit(&mut *self.encoded.borrow_mut());
        }
    }
}
impl egcl_rt::gc::TraceHostRoots for Invocation {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        with_env_visit_state(|state| self.trace_with(state, visit));
    }
}

/// Scope restoration is independent of Lisp errors and Rust unwinding.
pub(super) struct InvocationScope {
    env: *mut Env,
    active: bool,
}
impl InvocationScope {
    pub(super) fn enter(env: &mut Env, invocation: Option<Invocation>) -> Self {
        let active = invocation.is_some();
        if let Some(invocation) = invocation {
            env.method_invocations.push(invocation);
        }
        Self { env, active }
    }
}
impl Drop for InvocationScope {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                (*self.env).method_invocations.pop();
            }
        }
    }
}

pub(super) struct MethodScope {
    env: *mut Env,
}
impl MethodScope {
    pub(super) fn enter(env: &mut Env, context: MethodContext) -> Self {
        env.method_context.push(context);
        Self { env }
    }
}
impl Drop for MethodScope {
    fn drop(&mut self) {
        unsafe {
            (*self.env).method_context.pop();
        }
    }
}

pub(super) fn invocation_for_method(
    env: &mut Env,
    method: &MethodDef,
    args: &[EgclVal],
) -> Result<Option<Invocation>, EgclError> {
    if env.method_invocations.last().is_some_and(|invocation| {
        invocation
            .methods
            .borrow()
            .iter()
            .any(|candidate| candidate.method_id == method.method_id)
    }) {
        return Ok(None);
    }
    let (name, _) = method_def_by_id(env, method.method_id).ok_or_else(bad)?;
    egcl_rt::rooted!(methods = env.methods.borrow().get(&name).cloned().unwrap_or_default());
    egcl_rt::rooted!(symbol = generic_name_form_for_key(env, &name));
    let precedence = egcl_stdlib::clos::argument_precedence_order(*symbol);
    egcl_rt::rooted!(invocation = snapshot(&name, &methods, &[], &precedence, env)?);
    let expected = ordered(&invocation, args, env);
    invocation.expected = Arc::new(expected);
    Ok(Some(invocation.clone()))
}

fn ordered(invocation: &Invocation, args: &[EgclVal], env: &Env) -> Vec<EgclVal> {
    egcl_rt::rooted!(invocation = invocation.clone());
    egcl_rt::rooted!(args = args.to_vec());
    egcl_rt::rooted!(methods = invocation.methods.borrow().clone());
    let mut found = Vec::new();
    for index in 0..methods.len() {
        egcl_rt::rooted!(method = methods[index].clone());
        if let Some(key) = method_specificity_vector(env, &method, &args) {
            found.push((index, precedence_ordered_key(key, &invocation.precedence)));
        }
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
        .into_iter()
        .map(|(index, _)| methods[index].method_id)
        .collect()
}

pub(super) fn retained_methods(env: &Env, name: &str, ids: &[EgclVal]) -> Option<Vec<MethodDef>> {
    let invocation = env.method_invocations.last()?;
    if invocation.name.as_ref() != name {
        return None;
    }
    let methods = invocation.methods.borrow();
    ids.iter()
        .map(|id| {
            methods
                .iter()
                .find(|method| method.method_id == *id)
                .cloned()
        })
        .collect()
}

/// Called only after the dispatcher has computed its authoritative full order.
/// Cached qualifier groups cannot reconstruct this order by concatenation.
pub(super) fn snapshot(
    name: &str,
    methods: &[MethodDef],
    expected: &[EgclVal],
    precedence: &[usize],
    env: &mut Env,
) -> Result<Invocation, EgclError> {
    egcl_rt::rooted!(methods = methods.to_vec());
    // Display keys cannot reconstruct an uninterned symbol or a SETF name.
    // Keep the original name identity when retaining the callable wrapper.
    egcl_rt::rooted!(symbol = generic_name_form_for_key(env, name));
    egcl_rt::rooted!(
        invocation = Invocation {
            gf: NIL,
            name: Arc::from(name),
            methods: Arc::new(SharedCell::new(methods.clone())),
            expected: Arc::new(expected.to_vec()),
            precedence: Arc::new(precedence.to_vec()),
            lambda_list: stored_generic_lambda_list(*symbol)
                .or_else(|| methods.first().map(|method| method.lambda_list))
                .unwrap_or(NIL),
            encoded: Arc::new(SharedCell::new(NIL)),
        }
    );
    invocation.gf = generic_fn_wrapper(env, *symbol, name);
    Ok(invocation.clone())
}

/// Every method body gets these local functions, including bodies whose macros
/// introduce their references. Raw source spelling cannot prove absence.
/// Only the method body gets these local functions. Lambda-list initforms
/// retain their original definition environment. FLET supplies real lexical
/// FUNCTION/FUNCALL/APPLY bindings; nested native MakeClosure captures the
/// ordinary boxed LET variable through the existing environment protocol.
pub(super) fn wrap_body(
    body: EgclVal,
    metadata: &mut Option<bytecode::MethodContextSpec>,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(body = body);
    egcl_rt::rooted!(context = gensym_symbol("METHOD-CONTEXT"));
    egcl_rt::rooted!(args = gensym_symbol("NEXT-ARGUMENTS"));
    egcl_rt::rooted!(marker = gensym_symbol("METHOD-HELPER-DECL"));
    egcl_rt::rooted!(declaration_name = resolve_sym("COMMON-LISP:DECLARE").ok_or_else(bad)?);
    egcl_rt::rooted!(marker_spec = arena_cons(*marker, NIL));
    egcl_rt::rooted!(declaration = vec_to_list(&[*declaration_name, *marker_spec]));
    egcl_rt::rooted!(symbols = Vec::<EgclVal>::new());
    for name in [
        "COMMON-LISP:LET",
        "COMMON-LISP:FLET",
        "COMMON-LISP:CALL-NEXT-METHOD",
        "COMMON-LISP:NEXT-METHOD-P",
        "COMMON-LISP:&REST",
        "EGCL-INTERNAL::%METHOD-CONTEXT",
        "EGCL-INTERNAL::%METHOD-CONTINUE",
        "EGCL-INTERNAL::%METHOD-NEXT-P",
    ] {
        let symbol = resolve_sym(name).ok_or_else(bad)?;
        symbols.push(symbol);
    }
    egcl_rt::rooted!(get = vec_to_list(&[symbols[5]]));
    egcl_rt::rooted!(binding = vec_to_list(&[*context, *get]));
    egcl_rt::rooted!(bindings = arena_cons(*binding, NIL));
    egcl_rt::rooted!(params = vec_to_list(&[symbols[4], *args]));
    egcl_rt::rooted!(call = vec_to_list(&[symbols[6], *context, *args]));
    egcl_rt::rooted!(cnm = vec_to_list(&[symbols[2], *params, *declaration, *call]));
    egcl_rt::rooted!(predicate = vec_to_list(&[symbols[7], *context]));
    egcl_rt::rooted!(nmp = vec_to_list(&[symbols[3], NIL, *declaration, *predicate]));
    egcl_rt::rooted!(functions = vec_to_list(&[*cnm, *nmp]));
    egcl_rt::rooted!(tail = arena_cons(*functions, *body));
    egcl_rt::rooted!(flet = arena_cons(symbols[1], *tail));
    egcl_rt::rooted!(let_form = vec_to_list(&[symbols[0], *bindings, *flet]));
    egcl_rt::rooted!(wrapped = arena_cons(*let_form, NIL));
    *metadata = Some(bytecode::MethodContextSpec {
        context_key: lexical_identity::key(*context),
        marker: marker.as_symbol_index(),
        initializer: symbols[5].as_symbol_index(),
        helpers: [
            symbols[5].as_symbol_index(),
            symbols[6].as_symbol_index(),
            symbols[7].as_symbol_index(),
        ],
    });
    Ok(*wrapped)
}

fn bad() -> EgclError {
    EgclError::ProgramError("invalid method continuation".into())
}
fn proper_values(mut value: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    while value.is_cons() {
        if !seen.insert(value.0) {
            return Err(bad());
        }
        let (car, cdr) = cp(value);
        values.push(car);
        value = cdr;
    }
    if !value.is_nil() {
        return Err(bad());
    }
    Ok(values)
}
fn fields(value: EgclVal, count: usize) -> Result<Vec<EgclVal>, EgclError> {
    let values = proper_values(value)?;
    if values.len() != count {
        return Err(bad());
    }
    Ok(values)
}
fn integer(value: EgclVal) -> Result<usize, EgclError> {
    if !value.is_fixnum() {
        return Err(bad());
    }
    usize::try_from(value.as_fixnum()).map_err(|_| bad())
}
fn numbers(values: &[usize]) -> EgclVal {
    vec_to_list(
        &values
            .iter()
            .map(|value| EgclVal::from_fixnum(*value as i64))
            .collect::<Vec<_>>(),
    )
}

/// A method's captured frame rides an ordinary function object. Its body and
/// lambda list remain exact Lisp objects, and the existing CLSR machinery
/// persists them even if the public method registry no longer contains it.
fn encode_method(method: &MethodDef, _env: &mut Env) -> EgclVal {
    egcl_rt::rooted!(method = method.clone());
    egcl_rt::rooted!(
        function = if method.method_id.is_nil() && method.body.is_symbol() {
            method.body
        } else {
            egcl_rt::rooted!(
                carrier = Closure {
                    params_form: method.lambda_list,
                    body: method.body,
                    captured_frame: method
                        .captured_frame
                        .clone()
                        .unwrap_or_else(|| Arc::new(SharedCell::new(EnvFrame::default()))),
                    captured_specials: Vec::new(),
                    captured_blocks: Vec::new(),
                    captured_tags: Vec::new(),
                    // This carrier is never invoked. It stores source/frame fields for
                    // decode_method; it must not capture the caller's FLET/MACROLET.
                    captured_funs: None,
                    captured_macros: None,
                }
            );
            bytecode::prepare_private_closure_allocation();
            egcl_rt::rooted!(name = egcl_rt::symbols::make_uninterned("METHOD-SOURCE"));
            egcl_rt::rooted!(
                function = egcl_rt::function::alloc_interpreted(
                    carrier.params_form,
                    carrier.body,
                    NIL,
                    *name
                )
            );
            // Captures are an edge from the actual heap function, not a
            // globally rooted tree-closure registry entry. Never self-bind.
            bytecode::register_private_closure_env(*function, Arc::clone(&carrier.captured_frame));
            *function
        }
    );
    egcl_rt::rooted!(specs = Vec::new());
    for index in 0..method.specializers.len() {
        egcl_rt::rooted!(spec = method.specializers[index].clone());
        let (tag, name, value) = match &*spec {
            MethodSpecializer::Any => (0, String::new(), NIL),
            MethodSpecializer::Class { source_name, class } => (1, source_name.clone(), *class),
            MethodSpecializer::Eql(value) => (2, String::new(), *value),
        };
        egcl_rt::rooted!(value = value);
        egcl_rt::rooted!(name = arena_str(&name));
        let entry = vec_to_list(&[EgclVal::from_fixnum(tag), *name, *value]);
        specs.push(entry);
    }
    egcl_rt::rooted!(specs = vec_to_list(&specs));
    vec_to_list(&[
        method.method_id,
        *function,
        *specs,
        EgclVal::from_fixnum(qualifier_to_u8(method.qualifier, method.combination) as i64),
    ])
}
fn decode_method(value: EgclVal, _env: &Env) -> Result<MethodDef, EgclError> {
    let entry = fields(value, 4)?;
    if entry[0].is_nil() && entry[1].is_symbol() {
        return Ok(builtin_next_method(entry[1]));
    }
    if !egcl_rt::function::is_interpreted_function(entry[1]) {
        return Err(bad());
    }
    let captured_frame = bytecode::closure_captured_env(entry[1]).ok_or_else(bad)?;
    let mut specializers = Vec::new();
    for spec in proper_values(entry[2])? {
        let spec = fields(spec, 3)?;
        specializers.push(match integer(spec[0])? {
            0 => MethodSpecializer::Any,
            1 if egcl_stdlib::clos::is_class_object(spec[2]) => MethodSpecializer::Class {
                source_name: val_as_str(spec[1]),
                class: spec[2],
            },
            2 => MethodSpecializer::Eql(spec[2]),
            _ => return Err(bad()),
        });
    }
    let qualifier = u8::try_from(integer(entry[3])?).map_err(|_| bad())?;
    Ok(MethodDef {
        method_id: entry[0],
        lambda_list: egcl_rt::function::lambda_list(entry[1]),
        body: egcl_rt::function::body(entry[1]),
        captured_frame: Some(captured_frame),
        specializers,
        qualifier: u8_to_qualifier(qualifier),
        combination: u8_to_method_combination(qualifier),
    })
}
fn encode_methods(methods: &[MethodDef], env: &mut Env) -> EgclVal {
    egcl_rt::rooted!(methods = methods.to_vec());
    egcl_rt::rooted!(values = Vec::new());
    for index in 0..methods.len() {
        let value = encode_method(&methods[index], env);
        values.push(value);
    }
    vec_to_list(&values)
}
fn decode_methods(value: EgclVal, env: &Env) -> Result<Vec<MethodDef>, EgclError> {
    proper_values(value)?
        .into_iter()
        .map(|value| decode_method(value, env))
        .collect()
}
fn encode_invocation(invocation: &Invocation, env: &mut Env) -> EgclVal {
    egcl_rt::rooted!(invocation = invocation.clone());
    let cached = *invocation.encoded.borrow();
    if !cached.is_nil() {
        return cached;
    }
    #[cfg(test)]
    ENCODINGS.with(|count| count.set(count.get() + 1));
    egcl_rt::rooted!(source_methods = invocation.methods.borrow().clone());
    egcl_rt::rooted!(methods = encode_methods(&source_methods, env));
    egcl_rt::rooted!(ids = vec_to_list(&invocation.expected));
    egcl_rt::rooted!(precedence = numbers(&invocation.precedence));
    egcl_rt::rooted!(name = arena_str(&invocation.name));
    egcl_rt::rooted!(
        encoded = vec_to_list(&[
            invocation.gf,
            *name,
            *methods,
            *ids,
            *precedence,
            invocation.lambda_list,
        ])
    );
    *invocation.encoded.borrow_mut() = *encoded;
    *encoded
}
fn decode_invocation(value: EgclVal, env: &Env) -> Result<Invocation, EgclError> {
    let entry = fields(value, 6)?;
    Ok(Invocation {
        gf: entry[0],
        name: Arc::from(val_as_str(entry[1])),
        methods: Arc::new(SharedCell::new(decode_methods(entry[2], env)?)),
        expected: Arc::new(proper_values(entry[3])?),
        precedence: Arc::new(
            proper_values(entry[4])?
                .into_iter()
                .map(integer)
                .collect::<Result<_, _>>()?,
        ),
        lambda_list: entry[5],
        encoded: Arc::new(SharedCell::new(value)),
    })
}
// Reuse the invocation's single carrier per method. Only the qualifier-list
// spines and the current method's argument list differ between contexts.
fn encode_next_group(methods: &[MethodDef], descriptors: EgclVal, env: &mut Env) -> EgclVal {
    egcl_rt::rooted!(methods = methods.to_vec());
    egcl_rt::rooted!(
        descriptors = proper_values(descriptors).expect("internal method descriptors")
    );
    egcl_rt::rooted!(values = Vec::new());
    for index in 0..methods.len() {
        let existing = descriptors.iter().copied().find(|descriptor| {
            !methods[index].method_id.is_nil() && cp(*descriptor).0 == methods[index].method_id
        });
        let value = existing.unwrap_or_else(|| encode_method(&methods[index], env));
        values.push(value);
    }
    vec_to_list(&values)
}
fn encode_next(next: &NextMethod, descriptors: EgclVal, env: &mut Env) -> EgclVal {
    egcl_rt::rooted!(descriptors = descriptors);
    egcl_rt::rooted!(next = next.clone());
    let (kind, combination, around, before, primary, after) = match &*next {
        NextMethod::Standard {
            around,
            before,
            primary,
            after,
        } => (0, 0, &around[..], &before[..], &primary[..], &after[..]),
        NextMethod::Primary { primary } => (1, 0, &[][..], &[][..], &primary[..], &[][..]),
        NextMethod::ShortForm {
            around,
            combination,
            primary,
        } => (
            2,
            combination_to_u8(*combination),
            &around[..],
            &[][..],
            &primary[..],
            &[][..],
        ),
    };
    // Copies must be rooted independently: encoding any group can collect.
    egcl_rt::rooted!(
        groups = [
            around.to_vec(),
            before.to_vec(),
            primary.to_vec(),
            after.to_vec()
        ]
    );
    egcl_rt::rooted!(encoded = Vec::new());
    for index in 0..groups.len() {
        let value = encode_next_group(&groups[index], *descriptors, env);
        encoded.push(value);
    }
    let tail = vec_to_list(&encoded);
    egcl_rt::rooted!(tail = tail);
    *tail = arena_cons(EgclVal::from_fixnum(combination as i64), *tail);
    #[cfg(test)]
    RELOCATE_NEXT_TAIL.with(|force| {
        if force.replace(false) {
            let before = tail.to_raw();
            egcl_rt::gc::collect_t0_minor().unwrap();
            assert_ne!(
                tail.to_raw(),
                before,
                "new combination cons must actually move"
            );
        }
    });
    arena_cons(EgclVal::from_fixnum(kind), *tail)
}
fn decode_next(value: EgclVal, env: &Env) -> Result<NextMethod, EgclError> {
    let entry = fields(value, 6)?;
    let mut groups = Vec::new();
    for value in &entry[2..] {
        groups.push(decode_methods(*value, env)?);
    }
    let mut groups = groups.into_iter();
    let around = groups.next().unwrap();
    let before = groups.next().unwrap();
    let primary = groups.next().unwrap();
    let after = groups.next().unwrap();
    match integer(entry[0])? {
        0 => Ok(NextMethod::Standard {
            around,
            before,
            primary,
            after,
        }),
        1 => Ok(NextMethod::Primary { primary }),
        2 => Ok(NextMethod::ShortForm {
            around,
            combination: u8_to_combination(u8::try_from(integer(entry[1])?).map_err(|_| bad())?),
            primary,
        }),
        _ => Err(bad()),
    }
}
fn encode_context(context: &MethodContext, env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(context = context.clone());
    let invocation = context.invocation.as_ref().ok_or_else(bad)?;
    egcl_rt::rooted!(invocation = encode_invocation(invocation, env));
    egcl_rt::rooted!(args = vec_to_list(&context.args));
    let descriptors = fields(*invocation, 6)?.get(2).copied().ok_or_else(bad)?;
    egcl_rt::rooted!(next = encode_next(&context.next, descriptors, env));
    Ok(vec_to_list(&[*invocation, context.method, *args, *next]))
}
fn decode_context(value: EgclVal, env: &Env) -> Result<MethodContext, EgclError> {
    let entry = fields(value, 4)?;
    Ok(MethodContext {
        invocation: Some(decode_invocation(entry[0], env)?),
        method: entry[1],
        args: proper_values(entry[2])?,
        next: decode_next(entry[3], env)?,
    })
}

fn check_arguments(invocation: &Invocation, args: &[EgclVal]) -> Result<(), EgclError> {
    let (required, optional, rest, key) = lambda_list_shape(invocation.lambda_list);
    if args.len() < required || (!rest && !key && args.len() > required + optional) {
        return Err(EgclError::ProgramError(
            "CALL-NEXT-METHOD: wrong number of arguments".into(),
        ));
    }
    let info = lambda_list_key_info(invocation.lambda_list);
    if !info.has_key || args.len() <= info.positional {
        return Ok(());
    }
    let tail = &args[info.positional..];
    if tail.len() % 2 != 0 {
        return Err(EgclError::ProgramError(
            "CALL-NEXT-METHOD: incomplete keyword pair".into(),
        ));
    }
    let mut allowed = info.allow_other_keys;
    for pair in tail.chunks_exact(2) {
        if pair[0].is_symbol() && sym_bare_name_rc(pair[0]).as_ref() == "ALLOW-OTHER-KEYS" {
            allowed |= !pair[1].is_nil();
            break;
        }
    }
    let mut keys = info.keys;
    for method in invocation.methods.borrow().iter() {
        if !invocation.expected.contains(&method.method_id) {
            continue;
        }
        let info = lambda_list_key_info(method.lambda_list);
        allowed |= info.allow_other_keys;
        for key in info.keys {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    for pair in tail.chunks_exact(2) {
        if !pair[0].is_symbol() {
            return Err(EgclError::TypeError {
                datum: pair[0],
                expected: "SYMBOL".into(),
            });
        }
        let key = sym_bare_name_rc(pair[0]);
        if !allowed
            && key.as_ref() != "ALLOW-OTHER-KEYS"
            && !keys.iter().any(|candidate| candidate == key.as_ref())
        {
            return Err(EgclError::ProgramError(
                "CALL-NEXT-METHOD: unknown keyword".into(),
            ));
        }
    }
    Ok(())
}
pub(super) fn validate_changed(
    context: &MethodContext,
    args: &[EgclVal],
    env: &Env,
) -> Result<(), EgclError> {
    let Some(invocation) = &context.invocation else {
        return Err(bad());
    };
    check_arguments(invocation, args)?;
    if ordered(invocation, args, env) != *invocation.expected {
        return Err(EgclError::ProgramError(
            "CALL-NEXT-METHOD: ordered applicable methods changed".into(),
        ));
    }
    Ok(())
}
pub(super) fn no_next(
    context: &MethodContext,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(context = context.clone());
    egcl_rt::rooted!(args = args.to_vec());
    let gf = context.invocation.as_ref().ok_or_else(bad)?.gf;
    egcl_rt::rooted!(call = vec![gf, context.method]);
    call.extend_from_slice(&args);
    invoke_generic_function("NO-NEXT-METHOD", &call, env)
}
pub(super) fn call(name: &str, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(args = args.to_vec());
    match name {
        "EGCL-INTERNAL::%METHOD-CONTEXT" | "EGCL-INTERNAL:%METHOD-CONTEXT" if args.is_empty() => {
            let context = env.method_context.last().cloned().ok_or_else(bad)?;
            encode_context(&context, env)
        }
        "EGCL-INTERNAL::%METHOD-CONTINUE" | "EGCL-INTERNAL:%METHOD-CONTINUE" if args.len() == 2 => {
            egcl_rt::rooted!(context = decode_context(args[0], env)?);
            egcl_rt::rooted!(values = proper_values(args[1])?);
            let _scope = InvocationScope::enter(env, context.invocation.clone());
            call_next_method_values(env, &context, &values)
        }
        "EGCL-INTERNAL::%METHOD-NEXT-P" | "EGCL-INTERNAL:%METHOD-NEXT-P" if args.len() == 1 => {
            let context = decode_context(args[0], env)?;
            env.clear_mv();
            Ok(if method_context_has_next(&context) {
                T
            } else {
                NIL
            })
        }
        _ => Err(EgclError::ProgramError(
            "invalid method continuation argument count".into(),
        )),
    }
}

#[cfg(test)]
static ENCODINGS: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { std::cell::Cell::new(0) }) };

#[cfg(test)]
static RELOCATE_NEXT_TAIL: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| const { std::cell::Cell::new(false) })
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninterned_generic_snapshot_retains_name_identity_across_calls() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let result = read_eval_all_env(
            r#"
(let ((name (gensym "PRIVATE-GENERIC")))
  (eval `(defgeneric ,name (object)))
  (eval `(defmethod ,name (object) (list object)))
  (eval `(defmethod ,name :before (object) (declare (ignore object)) nil))
  (let ((saved (fdefinition name)))
    (and (equal (funcall name 1) '(1))
         (equal (funcall name 2) '(2))
         (equal (funcall (fdefinition name) 3) '(3))
         (equal (funcall saved 4) '(4))
         (fboundp name)
         (eq (generic-function-name saved) name))))
"#,
            &mut env,
        )
        .unwrap();
        assert_eq!(result, T);
    }

    #[test]
    fn proved_constant_method_dispatch_has_no_invocation_encoding() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        read_eval_all_env(
            "(defgeneric no-context-constant (object)) (defmethod no-context-constant (object) 42)",
            &mut env,
        )
        .unwrap();
        let method = env.methods.borrow()["NO-CONTEXT-CONSTANT"][0].method_id;
        assert!(
            method_context_unused(method),
            "actual published compiled proof required"
        );
        ENCODINGS.with(|count| count.set(0));
        let result = read_eval_all_env("(no-context-constant 9)", &mut env).unwrap();
        assert_eq!(result, EgclVal::from_fixnum(42));
        assert_eq!(ENCODINGS.with(std::cell::Cell::get), 0);
        assert!(env.method_invocations.is_empty());
    }

    #[test]
    fn before_retirement_keeps_original_later_method_and_owned_context() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        read_eval_all_env(
            r#"
(defvar *retired-later* nil)
(defgeneric retire-later (object))
(defmethod retire-later (object) :old)
(defparameter *old-later* (find-method #'retire-later nil (list (find-class 't))))
(defun replace-later-once ()
  (unless *retired-later*
    (setf *retired-later* t)
    (remove-method #'retire-later *old-later*)
    (defmethod retire-later (object) :new)))
(defmethod retire-later :before (object) (replace-later-once))
"#,
            &mut env,
        )
        .unwrap();
        let ids: Vec<_> = env.methods.borrow()["RETIRE-LATER"]
            .iter()
            .map(|method| method.method_id)
            .collect();
        assert!(
            ids.iter().any(|id| method_context_unused(*id)),
            "constant primary has a published no-use proof"
        );
        assert!(
            ids.iter().any(|id| !method_context_unused(*id)),
            "mutable user call conservatively retains context"
        );
        ENCODINGS.with(|count| count.set(0));
        assert_eq!(
            read_eval_all_env("(retire-later 1)", &mut env).unwrap(),
            resolve_sym(":OLD").unwrap()
        );
        assert!(
            ENCODINGS.with(std::cell::Cell::get) > 0,
            "retired later body really used its original source context"
        );
        assert_eq!(
            read_eval_all_env("(retire-later 2)", &mut env).unwrap(),
            resolve_sym(":NEW").unwrap()
        );
    }

    #[test]
    fn proved_before_removal_still_retains_original_later_method() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        read_eval_all_env(
            r#"
(defgeneric remove-later (object))
(defmethod remove-later (object) :original)
(defparameter *remove-later-primary* (find-method #'remove-later nil (list (find-class 't))))
(defmethod remove-later :before (object)
  (remove-method #'remove-later *remove-later-primary*))
"#,
            &mut env,
        )
        .unwrap();
        let ids: Vec<_> = env.methods.borrow()["REMOVE-LATER"]
            .iter()
            .map(|method| method.method_id)
            .collect();
        assert!(
            ids.iter().all(|id| method_context_unused(*id)),
            "canonical removal and constant bodies have actual no-use proofs"
        );
        ENCODINGS.with(|count| count.set(0));
        assert_eq!(
            read_eval_all_env("(remove-later 1)", &mut env).unwrap(),
            resolve_sym(":ORIGINAL").unwrap()
        );
        assert!(
            ENCODINGS.with(std::cell::Cell::get) > 0,
            "multi-method dispatch retains metadata before the removing prefix"
        );
    }

    #[test]
    fn generated_accessor_without_compiled_proof_keeps_leaf_and_protocol_paths() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        read_eval_all_env("(defclass no-proof-class () ((cell :initform 17 :accessor no-proof-cell))) (defparameter *no-proof-object* (make-instance 'no-proof-class))", &mut env).unwrap();
        let id = env.methods.borrow()["NO-PROOF-CELL"][0].method_id;
        let function = METHOD_COMPILED.borrow_mut().remove(&id.0);
        METHOD_CONTEXT_UNUSED.borrow_mut().remove(&id.0);
        if let Some(function) = function {
            set_compiled_method_cell(function, false);
        }
        assert!(ACCESSOR_METHODS.borrow().contains_key(&id.0));
        assert!(!method_context_unused(id));
        ENCODINGS.with(|count| count.set(0));
        assert_eq!(
            read_eval_all_env("(no-proof-cell *no-proof-object*)", &mut env).unwrap(),
            EgclVal::from_fixnum(17)
        );
        assert_eq!(
            ENCODINGS.with(std::cell::Cell::get),
            0,
            "authoritative callback-free accessor does not need compiled proof"
        );
        read_eval_all_env("(defvar *slot-protocol-effects* 0) (defmethod slot-unbound ((class standard-class) object (slot (eql 'cell))) (incf *slot-protocol-effects*) :handled) (slot-makunbound *no-proof-object* 'cell)", &mut env).unwrap();
        ENCODINGS.with(|count| count.set(0));
        assert_eq!(
            read_eval_all_env("(no-proof-cell *no-proof-object*)", &mut env).unwrap(),
            resolve_sym(":HANDLED").unwrap()
        );
        assert_eq!(
            read_eval_all_env("*slot-protocol-effects*", &mut env).unwrap(),
            EgclVal::from_fixnum(1)
        );
        assert!(
            ENCODINGS.with(std::cell::Cell::get) > 0,
            "protocol fallback retains the original activation before callbacks"
        );
    }

    #[test]
    fn activation_clones_share_one_encoded_invocation_across_collection() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            invocation = Invocation {
                gf: T,
                name: Arc::from("shared-activation"),
                methods: Arc::new(SharedCell::new(Vec::new())),
                expected: Arc::new(Vec::new()),
                precedence: Arc::new(Vec::new()),
                lambda_list: NIL,
                encoded: Arc::new(SharedCell::new(NIL)),
            }
        );
        egcl_rt::rooted!(other_method = invocation.clone());
        assert!(Arc::ptr_eq(&invocation.methods, &other_method.methods));
        assert!(Arc::ptr_eq(&invocation.encoded, &other_method.encoded));
        egcl_rt::rooted!(first = encode_invocation(&invocation, &mut env));
        let _active = InvocationScope::enter(&mut env, Some(invocation.clone()));
        egcl_rt::gc::collect_t0_minor().unwrap();
        let second = encode_invocation(&other_method, &mut env);
        assert_eq!(*first, second, "one exact Lisp descriptor per invocation");
    }

    #[test]
    fn uncaptured_method_carrier_does_not_capture_invocation_locals() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        env.frame
            .borrow_mut()
            .vars
            .insert("CALLER-LOCAL".into(), EgclVal::from_fixnum(91));
        let method = MethodDef {
            method_id: EgclVal::from_fixnum(1),
            lambda_list: NIL,
            body: NIL,
            captured_frame: None,
            specializers: Vec::new(),
            qualifier: egcl_stdlib::MethodQualifier::Primary,
            combination: None,
        };
        egcl_rt::rooted!(descriptor = encode_method(&method, &mut env));
        let restored = decode_method(*descriptor, &env).unwrap();
        let frame = restored.captured_frame.unwrap();
        let frame = frame.borrow();
        assert!(frame.parent.is_none());
        assert!(!frame.vars.contains_key("CALLER-LOCAL"));
    }

    #[test]
    fn method_carrier_cycle_has_no_tree_registry_root() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let frame = Arc::new(SharedCell::new(EnvFrame::default()));
        let weak = Arc::downgrade(&frame);
        let method = MethodDef {
            method_id: EgclVal::from_fixnum(1),
            lambda_list: NIL,
            body: NIL,
            captured_frame: Some(Arc::clone(&frame)),
            specializers: Vec::new(),
            qualifier: egcl_stdlib::MethodQualifier::Primary,
            combination: None,
        };
        egcl_rt::rooted!(descriptor = encode_method(&method, &mut env));
        let function = fields(*descriptor, 4).unwrap()[1];
        assert!(
            reified_closure(function).is_none(),
            "no globally rooted tree identity"
        );
        assert!(
            egcl_rt::symbols::symbol_function(egcl_rt::function::definition_index(function))
                .is_none_or(|value| value == egcl_rt::value::UNBOUND)
        );
        frame.borrow_mut().vars.insert("SAVED".into(), *descriptor);
        drop(method);
        drop(frame);
        drop(descriptor);
        egcl_rt::gc::request_major_gc();
        egcl_rt::gc::collect_if_major_requested().unwrap();
        assert!(
            weak.upgrade().is_none(),
            "unreachable private function/frame cycle must retire"
        );
    }

    #[test]
    fn next_method_encoding_roots_the_new_combination_cons_before_final_allocation() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::gc::collect_t0_minor().unwrap();
        RELOCATE_NEXT_TAIL.with(|force| force.set(true));
        egcl_rt::rooted!(
            encoded = encode_next(
                &NextMethod::Primary {
                    primary: Vec::new()
                },
                NIL,
                &mut env
            )
        );
        assert!(!RELOCATE_NEXT_TAIL.with(std::cell::Cell::get));
        let fields = fields(*encoded, 6).unwrap();
        assert_eq!(fields[0], EgclVal::from_fixnum(1));
        assert_eq!(fields[1], EgclVal::from_fixnum(0));
        assert!(fields[2..].iter().all(|value| value.is_nil()));
        assert!(
            matches!(decode_next(*encoded, &env).unwrap(), NextMethod::Primary { primary } if primary.is_empty())
        );
    }

    #[test]
    fn serialized_continuation_payload_is_the_only_owner_of_moving_arguments() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(argument = arena_cons(EgclVal::from_fixnum(42), NIL));
        egcl_rt::rooted!(
            context = MethodContext {
                args: vec![*argument],
                next: NextMethod::Primary {
                    primary: Vec::new()
                },
                method: NIL,
                invocation: Some(Invocation {
                    gf: T,
                    name: Arc::from("payload-root-test"),
                    methods: Arc::new(SharedCell::new(Vec::new())),
                    expected: Arc::new(Vec::new()),
                    precedence: Arc::new(Vec::new()),
                    lambda_list: NIL,
                    encoded: Arc::new(SharedCell::new(NIL)),
                }),
            }
        );
        egcl_rt::rooted!(payload = encode_context(&context, &mut env).unwrap());
        let before = *argument;
        drop(argument);
        drop(context);
        egcl_rt::gc::collect_t0_minor().unwrap();
        let restored = decode_context(*payload, &env).unwrap();
        assert_ne!(
            restored.args[0], before,
            "the retained argument must actually relocate"
        );
        assert_eq!(cp(restored.args[0]).0, EgclVal::from_fixnum(42));
        assert!(!method_context_has_next(&restored));
    }
}
