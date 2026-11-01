// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Open-code the list mapping functions in compiled code (bliss-gg3i1.7.13).
//!
//! `(mapcar f l)` used to reach the MAPCAR builtin through a cold named call,
//! which snapshots every list into a Rust `Vec` and applies `f` through the
//! generic `apply_function` path for each element (~1.5 us per element). MAPC
//! and MAPCAN were `&rest` Lisp functions that consed their argument list and
//! went through `(apply #'mapcar ...)` as well. SBCL's compiler maps over
//! short lists constantly, so the per-call overhead (tens of microseconds)
//! dominated.
//!
//! The lowerer now rewrites a call with a fixed, small number of list
//! arguments into a loop over core forms, the way SBCL's own transforms do.
//! The caller's tier compiles that loop with no `&rest` list and no host
//! re-entry:
//!
//! * a literal `(lambda (x ...) body)` (or `#'(lambda ...)`) whose lambda list
//!   is just the required parameters is inlined as `(let ((x elt) ...) body)`
//!   -- no closure is created and nothing is called;
//! * a quoted symbol `'f` becomes `(funcall 'f elt ...)`, which resolves the
//!   global function at each call exactly as MAPCAR's own FUNCALL does;
//! * any other designator is evaluated once into a temporary and FUNCALLed.
//!
//! The behaviour that callers can observe is kept as it was:
//!
//! * the function argument is evaluated before the lists, and each list form
//!   once, left to right;
//! * MAPC/MAPL return their first list, MAPCAR/MAPLIST a fresh list, and
//!   MAPCAN/MAPCON join the results with NCONC (CLHS 14.2), signalling a
//!   TYPE-ERROR for a non-NIL atom that is not the last result, as NCONC does;
//! * mapping stops at the shortest list;
//! * MAPC/MAPCAR/MAPCAN and SOME/EVERY/NOTANY/NOTEVERY check that every list
//!   is a proper list before the function is called (the builtins snapshot the
//!   lists first). The open-coded loop runs only when each argument is a proper
//!   list; otherwise the original function is called, so a dotted list, a
//!   non-list atom, and (for SOME and friends) a vector behave exactly as
//!   before;
//! * MAPL/MAPLIST/MAPCON test each remaining list with LISTP before each call,
//!   as their Lisp definitions in `lib/boot.lisp` do.
//!
//! A NOTINLINE declaration or a compiler macro on the operator, a local
//! function of the same name (handled by the caller), more than
//! [`MAX_LISTS`] lists, or a malformed call keeps the ordinary call.

use super::*;

/// Calls with more lists than this keep the ordinary call.
const MAX_LISTS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mapper {
    Mapc,
    Mapcar,
    Mapcan,
    Mapl,
    Maplist,
    Mapcon,
    Some,
    Every,
    Notany,
    Notevery,
}

impl Mapper {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "MAPC" => Self::Mapc,
            "MAPCAR" => Self::Mapcar,
            "MAPCAN" => Self::Mapcan,
            "MAPL" => Self::Mapl,
            "MAPLIST" => Self::Maplist,
            "MAPCON" => Self::Mapcon,
            "SOME" => Self::Some,
            "EVERY" => Self::Every,
            "NOTANY" => Self::Notany,
            "NOTEVERY" => Self::Notevery,
            _ => return None,
        })
    }

    /// The function receives successive sublists rather than elements.
    fn on_sublists(self) -> bool {
        matches!(self, Self::Mapl | Self::Maplist | Self::Mapcon)
    }

    /// The function's builtin snapshots every argument before calling it, so
    /// the loop needs every argument to be a proper list up front.
    fn checks_lists_first(self) -> bool {
        !self.on_sublists()
    }

    fn is_predicate(self) -> bool {
        matches!(
            self,
            Self::Some | Self::Every | Self::Notany | Self::Notevery
        )
    }
}

/// True for the operators this module can open-code.
pub(super) fn is_mapping_operator(name: &str) -> bool {
    Mapper::from_name(name).is_some()
}

/// How the mapped function is applied to one set of arguments.
#[derive(Clone, Copy)]
enum Callee {
    /// `(let ((param arg) ...) . body)`: indices of the parameter symbols
    /// start at `params`; `body` is the lambda's body list.
    Inline { params: usize, body: usize },
    /// `(funcall designator arg ...)` with the original `(quote sym)` form.
    Quoted(usize),
    /// `(funcall temp arg ...)` with the designator evaluated once.
    Bound(usize),
    /// `(name arg ...)` for `#'name` naming a standard function.
    Direct(usize),
}

/// Standard functions that are common mapping arguments. A COMMON-LISP
/// function cannot be redefined (CLHS 11.1.2.1.2), so `(mapcar #'car l)` may
/// call CAR directly -- which the native tiers open-code -- instead of
/// reifying `#'car` and FUNCALLing it. Every name here is a function, never a
/// macro or special operator, so `#'name` itself cannot signal.
const DIRECT_STANDARD_FUNCTIONS: &[&str] = &[
    "CAR",
    "CDR",
    "FIRST",
    "SECOND",
    "THIRD",
    "REST",
    "CAAR",
    "CADR",
    "CDAR",
    "CDDR",
    "CADDR",
    "LAST",
    "LIST",
    "LIST*",
    "CONS",
    "COPY-LIST",
    "REVERSE",
    "LENGTH",
    "IDENTITY",
    "NOT",
    "NULL",
    "ATOM",
    "CONSP",
    "LISTP",
    "SYMBOLP",
    "KEYWORDP",
    "STRINGP",
    "NUMBERP",
    "INTEGERP",
    "FUNCTIONP",
    "CHARACTERP",
    "VECTORP",
    "EQ",
    "EQL",
    "EQUAL",
    "EQUALP",
    "1+",
    "1-",
    "+",
    "-",
    "*",
    "MAX",
    "MIN",
    "SYMBOL-NAME",
    "SYMBOL-PACKAGE",
    "SYMBOL-VALUE",
    "SYMBOL-FUNCTION",
    "STRING",
    "STRING-UPCASE",
    "STRING-DOWNCASE",
    "PRINC-TO-STRING",
    "PRIN1-TO-STRING",
    "TYPE-OF",
    "CLASS-OF",
    "FIND-CLASS",
    "CLASS-NAME",
    "COPY-SEQ",
    "VALUES",
    "FUNCALL",
    "APPLY",
    "EVAL",
    "MACROEXPAND",
    "CONSTANTP",
    "PRINT",
    "PRINC",
    "PRIN1",
];

/// `#'name` for one of [`DIRECT_STANDARD_FUNCTIONS`], read as the
/// COMMON-LISP symbol (a package that shadows the name does not qualify).
fn direct_standard_function(form: EgclVal) -> Option<EgclVal> {
    if !form.is_cons() {
        return None;
    }
    let (head, rest) = cp(form);
    if !head.is_symbol() || sym_name(head) != "FUNCTION" || !rest.is_cons() {
        return None;
    }
    let (name, tail) = cp(rest);
    if !tail.is_nil() || !name.is_symbol() || name.is_nil() || name == T {
        return None;
    }
    let spelled = sym_name(name);
    if !DIRECT_STANDARD_FUNCTIONS.contains(&spelled.as_str()) {
        return None;
    }
    let cl = egcl_stdlib::find_package("COMMON-LISP")?;
    (egcl_stdlib::find_present_symbol(cl, &spelled) == Some(name)).then_some(name)
}

/// Forms under construction. Every value lives in `pool` (rooted by the
/// caller), so an allocation can move any of them; code refers to them by
/// index and reads the pool afresh after each allocation.
struct Forms {
    pool: Vec<EgclVal>,
}

impl Forms {
    fn val(&mut self, value: EgclVal) -> usize {
        self.pool.push(value);
        self.pool.len() - 1
    }

    fn sym(&mut self, name: &str) -> LowerResult<usize> {
        let symbol = resolve_sym(name).ok_or(Bail)?;
        Ok(self.val(symbol))
    }

    /// A private temporary in EGCL-INTERNAL, distinct per expansion within a
    /// function (like DOLIST's), so it can neither capture nor be captured by
    /// user names.
    fn temp(&mut self, role: &str, id: u32) -> LowerResult<usize> {
        self.sym(&format!("EGCL-INTERNAL::%MAP-{role}{id}"))
    }

    fn list(&mut self, items: &[usize]) -> usize {
        self.list_tail(items, None)
    }

    /// `(items... . tail)`. `vec_to_list_with_tail` roots its inputs; the
    /// values are copied out of the rooted pool with no allocation between.
    fn list_tail(&mut self, items: &[usize], tail: Option<usize>) -> usize {
        let values: Vec<EgclVal> = items.iter().map(|&index| self.pool[index]).collect();
        let tail = tail.map_or(NIL, |index| self.pool[index]);
        let form = super::super::vec_to_list_with_tail(&values, tail);
        self.val(form)
    }
}

/// The symbols every expansion uses, resolved once.
struct Ops {
    let_: usize,
    setq: usize,
    tagbody: usize,
    go: usize,
    when: usize,
    unless: usize,
    if_: usize,
    and: usize,
    or: usize,
    not: usize,
    car: usize,
    cdr: usize,
    cons: usize,
    consp: usize,
    listp: usize,
    null: usize,
    setf: usize,
    funcall: usize,
    function: usize,
    quote: usize,
    error: usize,
    t: usize,
    nil: usize,
}

impl Ops {
    fn new(forms: &mut Forms) -> LowerResult<Self> {
        Ok(Self {
            let_: forms.sym("LET")?,
            setq: forms.sym("SETQ")?,
            tagbody: forms.sym("TAGBODY")?,
            go: forms.sym("GO")?,
            when: forms.sym("WHEN")?,
            unless: forms.sym("UNLESS")?,
            if_: forms.sym("IF")?,
            and: forms.sym("AND")?,
            or: forms.sym("OR")?,
            not: forms.sym("NOT")?,
            car: forms.sym("CAR")?,
            cdr: forms.sym("CDR")?,
            cons: forms.sym("CONS")?,
            consp: forms.sym("CONSP")?,
            listp: forms.sym("LISTP")?,
            null: forms.sym("NULL")?,
            setf: forms.sym("SETF")?,
            funcall: forms.sym("FUNCALL")?,
            function: forms.sym("FUNCTION")?,
            quote: forms.sym("QUOTE")?,
            error: forms.sym("ERROR")?,
            t: forms.val(T),
            nil: forms.val(NIL),
        })
    }
}

/// A plain parameter the lambda can be inlined with: a symbol that LET can
/// bind (not NIL/T, a keyword, a constant, or a lambda-list keyword).
fn plain_parameter(param: EgclVal) -> bool {
    if !param.is_symbol() || param.is_nil() || param == T {
        return false;
    }
    let Some(index) = param.symbol_index() else {
        return false;
    };
    if egcl_rt::symbols::symbol_is_constant(index) {
        return false;
    }
    let name = sym_name(param);
    let bare = symbol_bare_name(&name);
    !(name.starts_with(':') || name.starts_with("KEYWORD:") || bare.starts_with('&'))
}

/// `(lambda (p ...) . body)` or `(function (lambda (p ...) . body))` with
/// exactly `arity` plain required parameters: returns the parameters and the
/// body. A leading documentation string followed by more forms is not valid
/// LET syntax, so such a lambda is not inlined.
fn inlinable_lambda(form: EgclVal, arity: usize) -> Option<(Vec<EgclVal>, EgclVal)> {
    if !form.is_cons() {
        return None;
    }
    let (head, tail) = cp(form);
    let lambda = if head.is_symbol() && sym_name(head) == "LAMBDA" {
        form
    } else if head.is_symbol() && sym_name(head) == "FUNCTION" && tail.is_cons() {
        let (inner, rest) = cp(tail);
        if !rest.is_nil() || !inner.is_cons() {
            return None;
        }
        let inner_head = cp(inner).0;
        if !inner_head.is_symbol() || sym_name(inner_head) != "LAMBDA" {
            return None;
        }
        inner
    } else {
        return None;
    };
    let (_, rest) = cp(lambda);
    if !rest.is_cons() {
        return None;
    }
    let (lambda_list, body) = cp(rest);
    let (params, params_tail) = super::super::list_to_vec_with_tail(lambda_list);
    if !params_tail.is_nil() || params.len() != arity || !params.iter().all(|&p| plain_parameter(p))
    {
        return None;
    }
    // Repeated parameters are an error in a lambda list, not a LET.
    for (index, &param) in params.iter().enumerate() {
        if params[..index].contains(&param) {
            return None;
        }
    }
    let (forms, body_tail) = super::super::list_to_vec_with_tail(body);
    if !body_tail.is_nil() {
        return None;
    }
    if forms.len() >= 2 && forms[0].is_string() {
        return None;
    }
    Some((params, body))
}

/// `(quote sym)` naming a global function by symbol.
fn quoted_designator(form: EgclVal) -> bool {
    if !form.is_cons() {
        return false;
    }
    let (head, rest) = cp(form);
    if !head.is_symbol() || sym_name(head) != "QUOTE" || !rest.is_cons() {
        return false;
    }
    let (value, tail) = cp(rest);
    tail.is_nil() && value.is_symbol() && !value.is_nil() && value != T
}

/// For the capture pre-scan (`compute_captured_names`): when `form` is a
/// mapping call whose literal lambda the lowerer will inline, the lambda's
/// body and the list forms, which then run in the enclosing function rather
/// than in a closure. A variable the lambda uses is then not captured, so it
/// can stay in a frame slot. Local NOTINLINE declarations and a same-named
/// local function are not visible here; when one keeps the call, the lambda
/// is compiled as a closure and asks for its captures to be boxed
/// (`request_boxing`), and the function is recompiled.
pub(super) fn inlined_lambda_parts(form: EgclVal) -> Option<(EgclVal, Vec<EgclVal>)> {
    let (op, rest) = cp(form);
    let symbol = op.symbol_index()?;
    Mapper::from_name(&sym_name(op))?;
    if mapping_transforms_disabled()
        || self_calls::notinline(symbol, &[])
        || compiler_macroexpand::has_compiler_macro(op)
    {
        return None;
    }
    let (args, tail) = super::super::list_to_vec_with_tail(rest);
    if !tail.is_nil() || args.len() < 2 || args.len() > MAX_LISTS + 1 {
        return None;
    }
    let (_, body) = inlinable_lambda(args[0], args.len() - 1)?;
    Some((body, args[1..].to_vec()))
}

/// The lowerer-private operator of `(%MAP-PRIMARY-VALUE form)`: FORM's
/// primary value, with any other values discarded (an explicit ClearMv).
const PRIMARY_VALUE: &str = "EGCL-INTERNAL::%MAP-PRIMARY-VALUE";

/// Whether `op` is the [`PRIMARY_VALUE`] marker. Only expansions built here
/// contain it; `lib/boot.lisp` defines it as a macro too, so the rare form
/// that reaches the evaluator still means the same thing.
pub(super) fn is_primary_value_marker(op: EgclVal) -> bool {
    op.symbol_index().is_some() && op.symbol_index() == reader::find_symbol_index(PRIMARY_VALUE)
}

impl Lowerer<'_> {
    /// `(%map-primary-value form)`. The result of a mapped call feeds a CONS
    /// that is open-coded only while the multiple-value state is known to be
    /// empty; a call's own values would otherwise stay live until the
    /// function returns.
    pub(super) fn lower_primary_value(&mut self, rest: EgclVal) -> LowerResult<()> {
        if !rest.is_cons() || !cp(rest).1.is_nil() {
            return Err(Bail);
        }
        self.lower_expr(cp(rest).0)?;
        self.emit(Instr::ClearMv);
        Ok(())
    }

    /// The open-coded replacement for a mapping call, or `None` to keep the
    /// ordinary call. `op` is the operator symbol and `rest` its arguments.
    pub(super) fn mapping_expansion(
        &mut self,
        name: &str,
        op: EgclVal,
        rest: EgclVal,
    ) -> LowerResult<Option<EgclVal>> {
        let Some(mapper) = Mapper::from_name(name) else {
            return Ok(None);
        };
        let Some(symbol) = op.symbol_index() else {
            return Ok(None);
        };
        if mapping_transforms_disabled()
            || self_calls::notinline(symbol, &self.inline_policies)
            || compiler_macroexpand::has_compiler_macro(op)
        {
            return Ok(None);
        }
        let (args, tail) = super::super::list_to_vec_with_tail(rest);
        if !tail.is_nil() || args.len() < 2 || args.len() > MAX_LISTS + 1 {
            return Ok(None);
        }
        let nlists = args.len() - 1;

        let mut forms = Forms {
            pool: Vec::with_capacity(96),
        };
        // Everything below goes through the rooted pool. The arguments are
        // copied in before anything allocates.
        egcl_rt::rooted_ref!(_pool_root = &mut forms.pool);
        let function_form = forms.val(args[0]);
        let list_forms: Vec<usize> = args[1..].iter().map(|&form| forms.val(form)).collect();
        let lambda = inlinable_lambda(args[0], nlists);
        drop(args);
        let callee_shape = match lambda {
            Some((params, body)) => {
                let first = forms.pool.len();
                for param in params {
                    forms.val(param);
                }
                let body = forms.val(body);
                Some((first, body))
            }
            None => None,
        };

        let id = self.fresh_id();
        let ops = Ops::new(&mut forms)?;
        let callee = match callee_shape {
            Some((params, body)) => Callee::Inline { params, body },
            None if quoted_designator(forms.pool[function_form]) => Callee::Quoted(function_form),
            None => match direct_standard_function(forms.pool[function_form]) {
                // A lexical function of that name would make `#'name` the
                // local one; such a binding of a COMMON-LISP name is undefined
                // behaviour, but keep the FUNCALL then.
                Some(name)
                    if !self
                        .local_fns
                        .contains_key(&super::super::lexical_function_name_key(name))
                        && !self
                            .closure_fns
                            .contains(&super::super::lexical_function_name_key(name))
                        && !self
                            .inline_local_fns
                            .contains_key(&super::super::lexical_function_name_key(name)) =>
                {
                    Callee::Direct(forms.val(name))
                }
                _ => Callee::Bound(forms.temp("F", id)?),
            },
        };
        let lists: Vec<usize> = (0..nlists)
            .map(|i| forms.temp(&format!("L{i}-"), id))
            .collect::<LowerResult<_>>()?;
        let cursors: Vec<usize> = (0..nlists)
            .map(|i| forms.temp(&format!("C{i}-"), id))
            .collect::<LowerResult<_>>()?;

        // (let ((F fn) (L0 list0) ...) ...): the function, then each list,
        // evaluated once in order.
        let mut bindings = Vec::with_capacity(nlists + 1);
        if let Callee::Bound(temp) = callee {
            bindings.push(forms.list(&[temp, function_form]));
        }
        for i in 0..nlists {
            bindings.push(forms.list(&[lists[i], list_forms[i]]));
        }
        let bindings = forms.list(&bindings);

        let body = expand_loop(&mut forms, &ops, mapper, callee, &lists, &cursors, id)?;
        let body = if mapper.checks_lists_first() {
            // (if (and <proper L0> ...) <loop> (funcall #'op fn L0 ...))
            let mut checks = Vec::with_capacity(nlists + 1);
            checks.push(ops.and);
            for (i, &list) in lists.iter().enumerate() {
                let proper = proper_list_test(&mut forms, &ops, list, id, i)?;
                checks.push(if mapper.is_predicate() {
                    // SOME and friends take any sequence: a vector's elements
                    // are mapped as a fresh list of them.
                    let as_list = vector_as_list(&mut forms, &ops, list)?;
                    forms.list(&[ops.or, proper, as_list])
                } else {
                    proper
                });
            }
            let checks = forms.list(&checks);
            let operator = forms.val(op);
            let fallback_function = match callee {
                Callee::Bound(temp) => temp,
                Callee::Quoted(form) => form,
                Callee::Direct(_) => function_form,
                // The fallback runs only when an argument is neither a proper
                // list nor (for the predicates) a vector. The builtin then
                // checks every argument before it calls anything, and
                // signals, so the inlined lambda is never needed there.
                Callee::Inline { .. } => {
                    let list = forms.sym("LIST")?;
                    forms.list(&[ops.function, list])
                }
            };
            let original = forms.list(&[ops.function, operator]);
            let mut call = vec![ops.funcall, original, fallback_function];
            call.extend_from_slice(&lists);
            let fallback = forms.list(&call);
            forms.list(&[ops.if_, checks, body, fallback])
        } else {
            body
        };
        let form = forms.list(&[ops.let_, bindings, body]);
        Ok(Some(forms.pool[form]))
    }
}

/// `(and (vectorp L) (not (eq (array-element-type L) nil)) (setq L (coerce L 'list)) t)`:
/// replace a vector argument of SOME and friends by the list of its active
/// elements. A vector of element type NIL is left to the builtin.
fn vector_as_list(forms: &mut Forms, ops: &Ops, list: usize) -> LowerResult<usize> {
    let vectorp = forms.sym("VECTORP")?;
    let element_type = forms.sym("ARRAY-ELEMENT-TYPE")?;
    let eq = forms.sym("EQ")?;
    let coerce = forms.sym("COERCE")?;
    let list_sym = forms.sym("LIST")?;
    let is_vector = forms.list(&[vectorp, list]);
    let type_of = forms.list(&[element_type, list]);
    let quoted_nil = forms.list(&[ops.quote, ops.nil]);
    let nil_type = forms.list(&[eq, type_of, quoted_nil]);
    let not_nil_type = forms.list(&[ops.not, nil_type]);
    let quoted_list = forms.list(&[ops.quote, list_sym]);
    let coerced = forms.list(&[coerce, list, quoted_list]);
    let store = forms.list(&[ops.setq, list, coerced]);
    Ok(forms.list(&[ops.and, is_vector, not_nil_type, store, ops.t]))
}

/// `(let ((V list)) (tagbody VT (when (consp V) (setq V (cdr V)) (go VT))) (null V))`
fn proper_list_test(
    forms: &mut Forms,
    ops: &Ops,
    list: usize,
    id: u32,
    index: usize,
) -> LowerResult<usize> {
    let walker = forms.temp(&format!("V{index}-"), id)?;
    let tag = forms.temp(&format!("VT{index}-"), id)?;
    let binding = forms.list(&[walker, list]);
    let bindings = forms.list(&[binding]);
    let consp = forms.list(&[ops.consp, walker]);
    let cdr = forms.list(&[ops.cdr, walker]);
    let step = forms.list(&[ops.setq, walker, cdr]);
    let go = forms.list(&[ops.go, tag]);
    let when = forms.list(&[ops.when, consp, step, go]);
    let walk = forms.list(&[ops.tagbody, tag, when]);
    let null = forms.list(&[ops.null, walker]);
    Ok(forms.list(&[ops.let_, bindings, walk, null]))
}

/// `(setf (cdr cons) value)`, which compiles to the EGCL::SET-CDR store.
fn set_cdr(forms: &mut Forms, ops: &Ops, cons: usize, value: usize) -> usize {
    let place = forms.list(&[ops.cdr, cons]);
    forms.list(&[ops.setf, place, value])
}

/// One application of the mapped function to `args`.
fn call_form(forms: &mut Forms, ops: &Ops, callee: Callee, args: &[usize]) -> usize {
    match callee {
        Callee::Inline { params, body } => {
            let bindings: Vec<usize> = args
                .iter()
                .enumerate()
                .map(|(i, &arg)| forms.list(&[params + i, arg]))
                .collect();
            let bindings = forms.list(&bindings);
            forms.list_tail(&[ops.let_, bindings], Some(body))
        }
        Callee::Quoted(designator) | Callee::Bound(designator) => {
            let mut call = vec![ops.funcall, designator];
            call.extend_from_slice(args);
            forms.list(&call)
        }
        Callee::Direct(name) => {
            let mut call = vec![name];
            call.extend_from_slice(args);
            forms.list(&call)
        }
    }
}

/// `(error 'type-error :datum DATUM :expected-type 'list)`, the error
/// `lib/boot.lisp` signals for a non-list argument.
fn list_type_error(forms: &mut Forms, ops: &Ops, datum: usize) -> LowerResult<usize> {
    let type_error = forms.sym("TYPE-ERROR")?;
    let quoted_type_error = forms.list(&[ops.quote, type_error]);
    let datum_key = forms.sym(":DATUM")?;
    let expected_key = forms.sym(":EXPECTED-TYPE")?;
    let list = forms.sym("LIST")?;
    let quoted_list = forms.list(&[ops.quote, list]);
    Ok(forms.list(&[
        ops.error,
        quoted_type_error,
        datum_key,
        datum,
        expected_key,
        quoted_list,
    ]))
}

/// The loop, run with the list temporaries bound:
///
/// ```text
/// (let ((C0 L0) ... extra...)
///   init...
///   (tagbody TOP (when MORE checks... body...))
///   after...
///   RESULT)
/// ```
///
/// where `body` ends by stepping the cursors and going back to TOP.
fn expand_loop(
    forms: &mut Forms,
    ops: &Ops,
    mapper: Mapper,
    callee: Callee,
    lists: &[usize],
    cursors: &[usize],
    id: u32,
) -> LowerResult<usize> {
    let top = forms.temp("TOP", id)?;
    // The arguments of one call: the elements, or the sublists themselves.
    let args: Vec<usize> = if mapper.on_sublists() {
        cursors.to_vec()
    } else {
        cursors
            .iter()
            .map(|&cursor| forms.list(&[ops.car, cursor]))
            .collect()
    };
    let call = call_form(forms, ops, callee, &args);
    // (setq C0 (cdr C0) C1 (cdr C1) ...) (go TOP)
    let mut step = vec![ops.setq];
    for &cursor in cursors {
        let cdr = forms.list(&[ops.cdr, cursor]);
        step.push(cursor);
        step.push(cdr);
    }
    let step = forms.list(&step);
    let go_top = forms.list(&[ops.go, top]);
    // Continue while every cursor is a cons (the lists were checked to be
    // proper), or for the sublist mappers while none is NIL.
    let mut more = vec![ops.and];
    for &cursor in cursors {
        more.push(if mapper.on_sublists() {
            cursor
        } else {
            forms.list(&[ops.consp, cursor])
        });
    }
    let more = forms.list(&more);
    // MAPL and friends check each remaining list before every call, exactly
    // as `%require-proper-lists` in lib/boot.lisp does.
    let mut checks = Vec::new();
    if mapper.on_sublists() {
        for &cursor in cursors {
            let listp = forms.list(&[ops.listp, cursor]);
            let signal = list_type_error(forms, ops, cursor)?;
            checks.push(forms.list(&[ops.unless, listp, signal]));
        }
    }

    let mut bindings: Vec<usize> = cursors
        .iter()
        .zip(lists)
        .map(|(&cursor, &list)| forms.list(&[cursor, list]))
        .collect();
    let mut init = Vec::new();
    let mut body = Vec::new();
    let mut after = Vec::new();
    let result = match mapper {
        Mapper::Mapc | Mapper::Mapl => {
            body.extend([call, step, go_top]);
            lists[0]
        }
        Mapper::Mapcar | Mapper::Maplist => {
            // HEAD is a dummy cons whose cdr collects the results; TAIL is
            // its last cons.
            let head = forms.temp("HEAD", id)?;
            let tail = forms.temp("TAIL", id)?;
            let cell = forms.temp("NEW", id)?;
            let first = forms.list(&[ops.cons, ops.nil, ops.nil]);
            bindings.push(forms.list(&[head, first]));
            bindings.push(forms.list(&[tail, ops.nil]));
            init.push(forms.list(&[ops.setq, tail, head]));
            // (let ((NEW (cons CALL nil))) (setf (cdr TAIL) NEW) (setq TAIL NEW))
            let primary = forms.sym(PRIMARY_VALUE)?;
            let value = forms.list(&[primary, call]);
            let fresh = forms.list(&[ops.cons, value, ops.nil]);
            let cell_binding = forms.list(&[cell, fresh]);
            let cell_bindings = forms.list(&[cell_binding]);
            let link = set_cdr(forms, ops, tail, cell);
            let advance = forms.list(&[ops.setq, tail, cell]);
            let collect = forms.list(&[ops.let_, cell_bindings, link, advance]);
            body.extend([collect, step, go_top]);
            forms.list(&[ops.cdr, head])
        }
        Mapper::Mapcan | Mapper::Mapcon => {
            nconc_parts(
                forms,
                ops,
                call,
                id,
                &mut bindings,
                &mut init,
                &mut body,
                &mut after,
            )?;
            body.extend([step, go_top]);
            let head = forms.temp("HEAD", id)?;
            forms.list(&[ops.cdr, head])
        }
        Mapper::Some | Mapper::Every | Mapper::Notany | Mapper::Notevery => {
            // Exit at the first deciding value through the loop's own test
            // rather than a BLOCK, so a RETURN in an inlined lambda body still
            // reaches the caller's block. SOME/NOTANY look for a true value:
            // R starts NIL and the loop continues while it stays NIL.
            // EVERY/NOTEVERY look for NIL: R starts T.
            let value = forms.temp("R", id)?;
            let seeking_true = matches!(mapper, Mapper::Some | Mapper::Notany);
            let initial = if seeking_true { ops.nil } else { ops.t };
            bindings.push(forms.list(&[value, initial]));
            let store = forms.list(&[ops.setq, value, call]);
            let undecided = if seeking_true {
                forms.list(&[ops.null, value])
            } else {
                value
            };
            let continue_ = forms.list(&[ops.when, undecided, step, go_top]);
            body.extend([store, continue_]);
            match mapper {
                // The first true value itself, or NIL.
                Mapper::Some => value,
                // T when no call returned NIL.
                Mapper::Every => forms.list(&[ops.if_, value, ops.t, ops.nil]),
                _ => forms.list(&[ops.not, value]),
            }
        }
    };

    let mut when = vec![ops.when, more];
    when.extend(checks);
    when.extend(body);
    let when = forms.list(&when);
    let tagbody = forms.list(&[ops.tagbody, top, when]);
    let bindings = forms.list(&bindings);
    let mut form = vec![ops.let_, bindings];
    form.extend(init);
    form.push(tagbody);
    form.extend(after);
    form.push(result);
    Ok(forms.list(&form))
}

/// MAPCAN / MAPCON: join the results as `(apply #'nconc results)` would.
///
/// A result is linked in once the next one arrives (only then is it known not
/// to be the last). A non-NIL atom that is not the last result is an error
/// in NCONC; the first such atom is remembered and signalled after the
/// mapping finishes, which is when the NCONC of the collected results would
/// have signalled it. HEAD is a dummy cons whose cdr is the result and TAIL
/// the last cons linked so far.
#[allow(clippy::too_many_arguments)]
fn nconc_parts(
    forms: &mut Forms,
    ops: &Ops,
    call: usize,
    id: u32,
    bindings: &mut Vec<usize>,
    init: &mut Vec<usize>,
    body: &mut Vec<usize>,
    after: &mut Vec<usize>,
) -> LowerResult<()> {
    let head = forms.temp("HEAD", id)?;
    let tail = forms.temp("TAIL", id)?;
    let value = forms.temp("R", id)?;
    let atom = forms.temp("ATOM", id)?;
    let bad = forms.temp("BAD", id)?;
    let last_tag = forms.temp("LAST", id)?;
    let progn = forms.sym("PROGN")?;

    let first = forms.list(&[ops.cons, ops.nil, ops.nil]);
    bindings.push(forms.list(&[head, first]));
    bindings.push(forms.list(&[tail, ops.nil]));
    bindings.push(forms.list(&[atom, ops.nil]));
    bindings.push(forms.list(&[bad, ops.nil]));
    init.push(forms.list(&[ops.setq, tail, head]));

    // A pending atom followed by any result is not last: remember the first.
    //   (when ATOM (unless BAD (setq BAD ATOM)) (setq ATOM nil))
    let set_bad = forms.list(&[ops.setq, bad, atom]);
    let unless_bad = forms.list(&[ops.unless, bad, set_bad]);
    let clear_atom = forms.list(&[ops.setq, atom, ops.nil]);
    let retire_atom = forms.list(&[ops.when, atom, unless_bad, clear_atom]);
    //   (if (consp R)
    //       (progn (setf (cdr TAIL) R) (setq TAIL R)
    //              (tagbody LAST (when (consp (cdr TAIL)) (setq TAIL (cdr TAIL)) (go LAST))))
    //       (when R (setq ATOM R)))
    let consp_value = forms.list(&[ops.consp, value]);
    let link = set_cdr(forms, ops, tail, value);
    let to_value = forms.list(&[ops.setq, tail, value]);
    let tail_cdr = forms.list(&[ops.cdr, tail]);
    let more_tail = forms.list(&[ops.consp, tail_cdr]);
    let next_cdr = forms.list(&[ops.cdr, tail]);
    let advance = forms.list(&[ops.setq, tail, next_cdr]);
    let go_last = forms.list(&[ops.go, last_tag]);
    let walk_step = forms.list(&[ops.when, more_tail, advance, go_last]);
    let walk = forms.list(&[ops.tagbody, last_tag, walk_step]);
    let linked = forms.list(&[progn, link, to_value, walk]);
    let remember_atom = forms.list(&[ops.setq, atom, value]);
    let atom_case = forms.list(&[ops.when, value, remember_atom]);
    let dispatch = forms.list(&[ops.if_, consp_value, linked, atom_case]);
    //   (let ((R CALL)) retire dispatch)
    let value_binding = forms.list(&[value, call]);
    let value_bindings = forms.list(&[value_binding]);
    body.push(forms.list(&[ops.let_, value_bindings, retire_atom, dispatch]));

    //   (when BAD (error 'type-error :datum BAD :expected-type 'list))
    //   (when ATOM (setf (cdr TAIL) ATOM))
    let signal = list_type_error(forms, ops, bad)?;
    after.push(forms.list(&[ops.when, bad, signal]));
    let final_tail = set_cdr(forms, ops, tail, atom);
    after.push(forms.list(&[ops.when, atom, final_tail]));
    Ok(())
}

/// `EGCL_NO_MAPPING_TRANSFORMS=1` keeps the ordinary calls, for A/B checks.
fn mapping_transforms_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED
        .get_or_init(|| std::env::var("EGCL_NO_MAPPING_TRANSFORMS").is_ok_and(|value| value == "1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(env: &Env, name: &str, body: &str) -> Arc<BytecodeFunction> {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(body = reader::read_from_string(body).unwrap().0);
        Arc::new(
            compile_function(name, *params, *body, env, true, false).expect("mapping call lowers"),
        )
    }

    fn calls(body: &BytecodeFunction, name: &str) -> bool {
        let symbol = resolve_sym(name).unwrap().as_symbol_index();
        body.code
            .iter()
            .any(|op| matches!(op, Instr::CallNamed { sym, .. } if *sym == symbol))
    }

    #[test]
    fn mapping_calls_compile_to_loops() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for (operator, source) in [
            ("MAPCAR", "((mapcar 'mapping-test-target x))"),
            ("MAPC", "((mapc (lambda (y) (mapping-test-target y)) x))"),
            ("MAPCAN", "((mapcan 'mapping-test-target x x))"),
            ("MAPLIST", "((maplist 'mapping-test-target x))"),
            ("MAPL", "((mapl 'mapping-test-target x))"),
            ("MAPCON", "((mapcon 'mapping-test-target x))"),
            ("SOME", "((some 'mapping-test-target x x))"),
            ("EVERY", "((every 'mapping-test-target x))"),
            ("NOTANY", "((notany 'mapping-test-target x))"),
            ("NOTEVERY", "((notevery 'mapping-test-target x))"),
        ] {
            let body = compile(&env, "MAPPING-OPEN-CODED-TEST", source);
            egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
            assert!(!calls(&body, operator), "{source} still calls {operator}");
            assert!(calls(&body, "MAPPING-TEST-TARGET"), "{source}");
        }
    }

    #[test]
    fn notinline_and_many_lists_keep_the_call() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for source in [
            "((declare (notinline mapcar)) (mapcar 'mapping-test-target x))",
            "((mapcar 'mapping-test-target x x x x x))",
            "((mapcar 'mapping-test-target))",
        ] {
            let body = compile(&env, "MAPPING-KEPT-TEST", source);
            egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
            assert!(calls(&body, "MAPCAR"), "{source} no longer calls MAPCAR");
        }
    }

    #[test]
    fn standard_function_designators_are_called_directly() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let body = compile(&env, "MAPPING-DIRECT-TEST", "((mapcar #'symbol-name x))");
        egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
        assert!(calls(&body, "SYMBOL-NAME"));
        // The only FUNCALL left is the improper-list fallback's.
        let funcall = egcl_rt::symbols::find_index("FUNCALL");
        let funcalls = body
            .code
            .iter()
            .filter(|op| matches!(op, Instr::CallNamed { sym, .. } if Some(*sym) == funcall))
            .count();
        assert_eq!(funcalls, 1);
    }
}
