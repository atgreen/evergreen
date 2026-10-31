// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The AMOP slot-access boundary. Storage and the ANSI missing/unbound/update
//! protocols stay in the existing helpers; these functions select a slot
//! metaobject and invoke the extensible using-class generic functions.
use super::*;

/// Finalization publishes a complete wrapper before migration is allowed.
/// The caller roots its instance across arbitrary Lisp finalization hooks.
pub(super) fn prepare(instance: EgclVal, env: &mut Env) -> Result<(), EgclError> {
    if egcl_stdlib::is_instance(instance) {
        let class = egcl_stdlib::class_of(instance);
        if !finalized(class) && !egcl_stdlib::is_structure_class(class) {
            class_protocol::ensure_finalized(class, env)?;
        }
    }
    Ok(())
}

thread_local! {
    /// Classes seen with a finalized protocol, stamped with the dispatch
    /// generation. Only immediates and raw bits, never traced.
    static FINALIZED: [std::cell::Cell<Option<(u64, u64)>>; 64] =
        const { [const { std::cell::Cell::new(None) }; 64] };
}

/// `class_protocol_finalized`, which computes the class precedence list to
/// look for a forward-referenced superclass, memoized per dispatch generation:
/// every class definition or redefinition, and every change to a class's
/// protocol, moves that generation. Only a positive answer is remembered.
/// Allocation-free on a hit.
fn finalized(class: EgclVal) -> bool {
    let generation = GF_DISPATCH_GENERATION.load(AtomicOrdering::Acquire);
    let slot = ((class.0 >> 3) ^ (class.0 >> 11)) as usize % 64;
    if FINALIZED.with(|memo| memo[slot].get()) == Some((class.0, generation)) {
        return true;
    }
    let yes = egcl_stdlib::clos::class_protocol_finalized(class);
    if yes && GF_DISPATCH_GENERATION.load(AtomicOrdering::Acquire) == generation {
        FINALIZED.with(|memo| memo[slot].set(Some((class.0, generation))));
    }
    yes
}

#[derive(Clone, Copy)]
pub(super) enum Operation {
    Read,
    Write,
    Boundp,
    Makunbound,
}

impl Operation {
    fn symbol(self) -> &'static str {
        match self {
            Self::Read | Self::Write => "EGCL-MOP:SLOT-VALUE-USING-CLASS",
            Self::Boundp => "EGCL-MOP:SLOT-BOUNDP-USING-CLASS",
            Self::Makunbound => "EGCL-MOP:SLOT-MAKUNBOUND-USING-CLASS",
        }
    }

    fn delegate(self) -> &'static str {
        match self {
            Self::Read => "%STANDARD-SLOT-VALUE-USING-CLASS",
            Self::Write => "%STANDARD-SET-SLOT-VALUE-USING-CLASS",
            Self::Boundp => "%STANDARD-SLOT-BOUNDP-USING-CLASS",
            Self::Makunbound => "%STANDARD-SLOT-MAKUNBOUND-USING-CLASS",
        }
    }

    fn class_argument(self) -> usize {
        usize::from(matches!(self, Self::Write))
    }

    fn key(self) -> String {
        let symbol = resolve_sym(self.symbol()).unwrap_or(NIL);
        let name = sym_name(symbol);
        if matches!(self, Self::Write) {
            format!("(SETF {name})")
        } else {
            name
        }
    }
}

#[derive(Clone, Copy)]
struct Pristine {
    layout: usize,
    generation: u64,
    operation: u8,
    call_generation: u64,
    yes: bool,
}

thread_local! {
    // Wrapper addresses are permanent layout tokens, not movable Lisp values.
    // Method/class mutation already invalidates both this generation and the
    // compiled accessor cache. Nothing here needs GC tracing or image storage.
    static PRISTINE: [std::cell::Cell<Option<Pristine>>; 32] =
        const { [const { std::cell::Cell::new(None) }; 32] };
}

/// A pure delegate has exactly its required parameters, in the same order,
/// passed to the internal implementation. Looking only at an operator name
/// would incorrectly bypass a replacement method that changes the arguments.
fn standard_delegate(method: &MethodDef, operation: Operation, env: &Env) -> bool {
    if method.captured_frame.is_some() || method.qualifier != egcl_stdlib::MethodQualifier::Primary
    {
        return false;
    }
    let params = list_to_vec(method.lambda_list);
    let required = 3 + operation.class_argument();
    if params.len() != required || params.iter().any(|p| !p.is_symbol()) {
        return false;
    }
    // DEFMETHOD wraps every body in its next-method helpers; the delegate is
    // the source body inside them.
    let body = list_to_vec(method_continuations::unwrapped_body(method.body));
    if body.len() != 1 {
        return false;
    }
    let block = list_to_vec(body[0]);
    if block.len() != 3 || !block[0].is_symbol() || sym_bare_name_rc(block[0]).as_ref() != "BLOCK" {
        return false;
    }
    let call = list_to_vec(block[2]);
    call.len() == required + 1
        && call[0].is_symbol()
        && call[0]
            .symbol_index()
            .and_then(installed_function)
            .is_none()
        && named_call_target(env, &sym_name(call[0]), call[0].symbol_index(), true).is_none()
        && !env.generics.contains_key(sym_name_rc(call[0]).as_ref())
        && !env.methods.contains_key(sym_name_rc(call[0]).as_ref())
        && sym_name_rc(call[0]).starts_with("EGCL-INTERNAL:")
        && sym_bare_name_rc(call[0]).as_ref() == operation.delegate()
        && call[1..] == params
}

/// The layout may be accessed directly only if every potentially applicable
/// method is a pure standard delegate. Filter on the CLASS argument alone:
/// the remaining arguments could have EQL/custom slot-definition specializers.
/// Declining conservatively for those retains their full dispatch semantics.
/// Custom-metaclass instances always use the MOP, even without overrides.
///
/// May allocate on a generation miss: callers must root their arguments.
pub(super) fn pristine(instance: EgclVal, operation: Operation, env: &Env) -> bool {
    let class = egcl_stdlib::class_of(instance);
    // DEFSTRUCT's established storage protocol is independent of these AMOP
    // standard-class generic functions, including its native accessor cache.
    if egcl_stdlib::is_structure_class(class) {
        return true;
    }
    if BOOT_COMPLETE.with(|ready| ready.get()) && !finalized(class) {
        return false;
    }
    let Some(layout) = egcl_stdlib::clos::class_layout_key(class) else {
        return false;
    };
    let metaclass = egcl_stdlib::clos::class_metaclass(class);
    if !metaclass.is_nil() {
        let standard = reader::find_symbol_index("STANDARD-CLASS");
        if standard.is_none() || egcl_stdlib::class_name(metaclass).symbol_index() != standard {
            return false;
        }
    }
    let generation = GF_DISPATCH_GENERATION.load(AtomicOrdering::Acquire);
    let call_generation = bytecode::direct_call_gen();
    let slot = ((layout >> 4) ^ operation as usize) % 32;
    if let Some(entry) = PRISTINE.with(|cache| cache[slot].get())
        && entry.layout == layout
        && entry.generation == generation
        && entry.call_generation == call_generation
        && entry.operation == operation as u8
    {
        return entry.yes;
    }
    egcl_rt::rooted!(class = class);
    let name = operation.key();
    let symbol = EgclVal::from_symbol_index(reader::intern_symbol(&name));
    if !generic_binding::current(&name)
        || symbol.symbol_index().and_then(installed_function).is_some()
        || setf_writer_replacement(&name).is_some()
        || named_call_target(env, &name, symbol.symbol_index(), true).is_some()
        || !env
            .generics
            .borrow()
            .get(&name)
            .is_some_and(|gf| gf.combination == egcl_stdlib::MethodCombinationType::Standard)
    {
        return !BOOT_COMPLETE.with(|ready| ready.get())
            && !env.generics.contains_key(&name)
            && !env.methods.contains_key(&name);
    }
    egcl_rt::rooted!(methods = env.methods.get_cloned(&name).unwrap_or_default());
    let mut yes = !BOOT_COMPLETE.with(|ready| ready.get()) && methods.is_empty();
    let mut delegate = false;
    for index in 0..methods.len() {
        // Resolve applicability outside the definition-table borrow, with the
        // snapshot and class rooted across class-name resolution.
        let mut probe = methods[index].clone();
        probe.specializers = probe
            .specializers
            .get(operation.class_argument())
            .cloned()
            .into_iter()
            .collect();
        egcl_rt::rooted_ref!(_probe = &mut probe);
        if probe.specializers.is_empty()
            || method_specificity_vector(env, &probe, &[*class]).is_some()
        {
            if !standard_delegate(&methods[index], operation, env) {
                yes = false;
                break;
            }
            // Only boot's standard signature covers every object and slotd.
            // A delegate restricted in either remaining position cannot prove
            // that a default method will actually be applicable to this call.
            let position = operation.class_argument();
            delegate |= methods[index]
                .specializers
                .iter()
                .enumerate()
                .all(|(i, s)| i == position || matches!(s, MethodSpecializer::Any));
            yes = delegate;
        }
    }
    PRISTINE.with(|cache| {
        cache[slot].set(Some(Pristine {
            layout,
            generation,
            operation: operation as u8,
            call_generation,
            yes,
        }))
    });
    yes
}

fn meta_call(name: &str, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
    let args = RootedVals::new(args.to_vec());
    let function = resolve_sym(name).unwrap_or(NIL);
    apply_function(function, &args, env)
}

/// Look up effective slot metaobjects through the public protocol, allowing
/// a custom slot-definition class to implement SLOT-DEFINITION-NAME.
fn slot_definition(class: EgclVal, slot: EgclVal, env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(slot = slot);
    egcl_rt::rooted!(remaining = meta_call("EGCL-MOP:CLASS-SLOTS", &[class], env)?);
    while remaining.is_cons() {
        egcl_rt::rooted!(definition = cp(*remaining).0);
        *remaining = cp(*remaining).1;
        let name = meta_call("EGCL-MOP:SLOT-DEFINITION-NAME", &[*definition], env)?;
        if name == *slot {
            return Ok(*definition);
        }
    }
    Ok(NIL)
}

/// None means that the ANSI raw path handles a missing/non-instance slot, or
/// the pristine standard protocol permits direct storage access.
pub(super) fn dispatch(
    operation: Operation,
    instance: EgclVal,
    slot: EgclVal,
    value: Option<EgclVal>,
    env: &mut Env,
) -> Result<Option<EgclVal>, EgclError> {
    egcl_rt::rooted!(instance = instance);
    egcl_rt::rooted!(slot = slot);
    egcl_rt::rooted!(value = value);
    if !egcl_stdlib::is_instance(*instance) || pristine(*instance, operation, env) {
        return Ok(None);
    }
    let args = RootedVals::new(vec![*instance, *slot, (*value).unwrap_or(NIL)]);
    egcl_rt::rooted!(class = egcl_stdlib::class_of(args[0]));
    egcl_rt::rooted!(definition = slot_definition(*class, args[1], env)?);
    if definition.is_nil() {
        return Ok(None);
    }
    let mut values = Vec::with_capacity(4);
    if matches!(operation, Operation::Write) {
        values.push(args[2]);
    }
    values.extend([*class, args[0], *definition]);
    let values = RootedVals::new(values);
    let function = EgclVal::from_symbol_index(reader::intern_symbol(&operation.key()));
    let answer = apply_function(function, &values, env)?;
    env.clear_mv();
    Ok(Some(answer))
}

pub(super) fn builtin(name: &str) -> bool {
    matches!(
        name.strip_prefix("EGCL-INTERNAL::")
            .or_else(|| name.strip_prefix("EGCL-INTERNAL:")),
        Some(
            "%RAW-SLOT-VALUE"
                | "%STANDARD-SLOT-VALUE-USING-CLASS"
                | "%STANDARD-SET-SLOT-VALUE-USING-CLASS"
                | "%STANDARD-SLOT-BOUNDP-USING-CLASS"
                | "%STANDARD-SLOT-MAKUNBOUND-USING-CLASS"
        )
    )
}

pub(super) fn raw(name: &str, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
    let args = RootedVals::new(args.to_vec());
    let bare = symbol_bare_name(name);
    if bare == "%RAW-SLOT-VALUE" {
        if args.len() != 2 {
            return Err(EgclError::ProgramError(
                "%RAW-SLOT-VALUE expects two arguments".into(),
            ));
        }
        return slot_value_or_signal_raw(args[0], args[1], env);
    }
    let write = bare == "%STANDARD-SET-SLOT-VALUE-USING-CLASS";
    let offset = usize::from(write);
    if args.len() != 3 + offset {
        return Err(EgclError::ProgramError(format!(
            "{name}: wrong argument count"
        )));
    }
    egcl_rt::rooted!(slot = meta_call("EGCL-MOP:SLOT-DEFINITION-NAME", &[args[2 + offset]], env)?);
    match bare.as_str() {
        "%STANDARD-SLOT-VALUE-USING-CLASS" => slot_value_or_signal_raw(args[1], *slot, env),
        "%STANDARD-SET-SLOT-VALUE-USING-CLASS" => {
            store_slot_value_raw(args[2], *slot, args[0], env)
        }
        "%STANDARD-SLOT-BOUNDP-USING-CLASS" => slot_boundp_protocol_raw(args[1], *slot, env),
        "%STANDARD-SLOT-MAKUNBOUND-USING-CLASS" => {
            slot_makunbound_protocol_raw(args[1], *slot, env)
        }
        _ => Err(EgclError::ProgramError(format!(
            "unknown slot protocol primitive: {name}"
        ))),
    }
}
