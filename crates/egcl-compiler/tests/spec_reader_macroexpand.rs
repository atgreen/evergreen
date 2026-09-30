use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};

use egcl_compiler::macroexpand::{
    DeclInfo, Environment, FunctionInfo, VariableInfo, define_compiler_macro, define_global_macro,
    enclose, macroexpand, macroexpand_1, macroexpand_all, parse_macro, set_macroexpand_hook,
    set_macroexpand_limit, undefine_compiler_macro, undefine_global_macro,
};
use egcl_compiler::reader::{
    ReaderState, copy_readtable, get_dispatch_macro_character, get_macro_character, intern_symbol,
    make_dispatch_macro_character, make_readtable, read, read_from_string,
    read_from_string_with_base, register_package, set_dispatch_macro_character,
    set_macro_character, symbol_name,
};
use egcl_rt::error::EgclError;
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{NIL, T, EgclVal};

static MACRO_STATE_LOCK: Mutex<()> = Mutex::new(());
static HOOK_CALLED: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct ConsCell {
    car: EgclVal,
    cdr: EgclVal,
}

fn parse(src: &str) -> EgclVal {
    let (value, pos) = read_from_string(src).unwrap_or_else(|err| panic!("read {src:?}: {err:?}"));
    assert_eq!(pos, src.len(), "reader did not consume all of {src:?}");
    value
}

fn sym(name: &str) -> EgclVal {
    EgclVal::from_symbol_index(intern_symbol(name))
}

fn symbol_text(value: EgclVal) -> Option<String> {
    if value == NIL {
        return Some("NIL".to_string());
    }
    if value == T {
        return Some("T".to_string());
    }
    if value.is_symbol() {
        return symbol_name(value.as_symbol_index());
    }
    None
}

fn list_items(mut value: EgclVal) -> Vec<EgclVal> {
    let mut out = Vec::new();
    while value.is_cons() {
        let ptr = (value.0 & !egcl_rt::value::TAG_MASK) as *const ConsCell;
        unsafe {
            out.push((*ptr).car);
            value = (*ptr).cdr;
        }
    }
    assert_eq!(value, NIL, "expected a proper list");
    out
}

fn heap_type(value: EgclVal) -> u8 {
    assert!(
        value.is_heap_object(),
        "expected heap object, got {value:?}"
    );
    unsafe { (*(value.as_ptr() as *const ObjectHeader)).type_id() }
}

fn expect_stream_error_contains(result: Result<(EgclVal, usize), EgclError>, needle: &str) {
    match result {
        Err(EgclError::StreamError(message)) => assert!(
            message.contains(needle),
            "expected stream error containing {needle:?}, got {message:?}"
        ),
        other => panic!("expected stream error containing {needle:?}, got {other:?}"),
    }
}

fn expect_error_contains<T>(result: Result<T, EgclError>, needle: &str) {
    match result {
        Err(err) => assert!(
            err.to_string().contains(needle),
            "expected error containing {needle:?}, got {err:?}"
        ),
        Ok(_) => panic!("expected error containing {needle:?}"),
    }
}

fn macro_state_guard() -> MutexGuard<'static, ()> {
    MACRO_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

fn passthrough_hook(
    expander: EgclVal,
    _form: EgclVal,
    _env: &Environment,
) -> Result<EgclVal, EgclError> {
    Ok(expander)
}

fn recording_hook(
    expander: EgclVal,
    _form: EgclVal,
    _env: &Environment,
) -> Result<EgclVal, EgclError> {
    HOOK_CALLED.store(true, Ordering::SeqCst);
    Ok(expander)
}

#[test]
fn reader_acceptance_parses_standard_reader_macros_comments_and_escapes() {
    // Per R4.01, R4.02, and R4.03, the reader entrypoint MUST implement the
    // CL reader algorithm, standard syntax classes, and standard reader macros.
    let quoted = parse("'foo");
    let quoted_items = list_items(quoted);
    assert_eq!(symbol_text(quoted_items[0]).as_deref(), Some("QUOTE"));
    assert_eq!(symbol_text(quoted_items[1]).as_deref(), Some("FOO"));

    let quasiquoted = parse("`(,foo ,@bar)");
    let outer = list_items(quasiquoted);
    assert_eq!(symbol_text(outer[0]).as_deref(), Some("EGCL::QUASIQUOTE"));
    let inner = list_items(outer[1]);
    assert_eq!(
        symbol_text(list_items(inner[0])[0]).as_deref(),
        Some("EGCL::UNQUOTE")
    );
    assert_eq!(
        symbol_text(list_items(inner[1])[0]).as_deref(),
        Some("EGCL::UNQUOTE-SPLICING")
    );

    let commented = read_from_string("  ; ignored\n|MiXeD Case|").unwrap();
    assert_eq!(commented.1, "  ; ignored\n|MiXeD Case|".len());
    assert_eq!(symbol_text(commented.0).as_deref(), Some("MiXeD Case"));

    let single_escaped = parse("\\a");
    assert_eq!(symbol_text(single_escaped).as_deref(), Some("a"));
}

#[test]
fn reader_acceptance_parses_numbers_and_dispatch_forms() {
    // Per R4.04 and R4.06, the reader MUST parse standard numeric syntax and
    // # dispatch forms including radix prefixes, character literals, and #C.
    let (hex, _) = read_from_string("#x10").unwrap();
    assert_eq!(hex.as_fixnum(), 16);

    let (binary, _) = read_from_string("#b1010").unwrap();
    assert_eq!(binary.as_fixnum(), 10);

    let (base36, _) = read_from_string_with_base("z", 36, true).unwrap();
    assert_eq!(base36.as_fixnum(), 35);

    let ratio = parse("3/4");
    assert_eq!(heap_type(ratio), type_id::RATIO);

    let complex = parse("#C(1 2)");
    assert_eq!(heap_type(complex), type_id::COMPLEX);

    let (ch, _) = read_from_string("#\\Space").unwrap();
    assert!(ch.is_character());
    assert_eq!(ch.as_char(), ' ');
}

/// bliss-rr9q: libraries such as cl-str use the implementation character
/// names accepted by SBCL for vertical tab, next-line, and no-break space.
/// Character names are case-insensitive and may contain `-` or `_`; the
/// reader must consume the whole name rather than stopping at the first
/// non-alphabetic character.
#[test]
fn reader_accepts_common_implementation_character_names() {
    for (source, expected) in [
        ("#\\Vt", '\u{000b}'),
        ("#\\Next-Line", '\u{0085}'),
        ("#\\No-break_space", '\u{00a0}'),
        ("#\\Ideographic_space", '\u{3000}'),
    ] {
        let (value, end) = read_from_string(source)
            .unwrap_or_else(|error| panic!("reader rejected {source}: {error}"));
        assert_eq!(end, source.len(), "reader did not consume all of {source}");
        assert!(value.is_character(), "{source} did not read as a character");
        assert_eq!(value.as_char(), expected, "wrong character for {source}");
    }
}

#[test]
fn reader_acceptance_resolves_package_qualified_keyword_and_uninterned_symbols() {
    // Per R4.05, package-qualified, keyword, and uninterned symbols MUST be
    // resolved through the real reader entrypoint.
    register_package("APP");

    let (pkg, _) = read_from_string("app:thing").unwrap();
    // Canonical qualified registry spelling is "PKG::NAME" (bliss-nc3b);
    // single-colon keys are legacy.
    assert_eq!(symbol_text(pkg).as_deref(), Some("APP::THING"));

    let (keyword, _) = read_from_string(":flag").unwrap();
    assert_eq!(symbol_text(keyword).as_deref(), Some("KEYWORD:FLAG"));

    let (nil_sym, _) = read_from_string("CL:NIL").unwrap();
    assert_eq!(nil_sym, NIL);

    let (u1, _) = read_from_string("#:temp").unwrap();
    let (u2, _) = read_from_string("#:temp").unwrap();
    assert!(u1.is_symbol() && u2.is_symbol());
    assert_ne!(u1, u2, "uninterned symbols must be distinct");
}

#[test]
fn reader_readtable_entrypoints_store_and_copy_macro_dispatch_configuration() {
    // Per R4.02, R4.03, and R4.06, the readtable interfaces MUST represent
    // macro characters and dispatch sub-characters.
    let rt = make_readtable(None).unwrap();
    let bang_handler = EgclVal::from_fixnum(11);
    set_macro_character(rt, '!', bang_handler, true).unwrap();
    assert_eq!(
        get_macro_character(rt, '!').unwrap(),
        (Some(bang_handler), true)
    );

    make_dispatch_macro_character(rt, '%', false).unwrap();
    let sub_handler = EgclVal::from_fixnum(22);
    set_dispatch_macro_character(rt, '%', 'X', sub_handler).unwrap();
    assert_eq!(
        get_dispatch_macro_character(rt, '%', 'X').unwrap(),
        Some(sub_handler)
    );

    let copied = copy_readtable(rt, None).unwrap();
    assert_eq!(
        get_macro_character(copied, '!').unwrap(),
        (Some(bang_handler), true)
    );
    assert_eq!(
        get_dispatch_macro_character(copied, '%', 'X').unwrap(),
        Some(sub_handler),
        "dispatch sub-character configuration must survive copy-readtable"
    );
}

#[test]
fn reader_entrypoints_report_positions_and_malformed_input_errors() {
    // Per R4.08 and R4.09, successful reads MUST report positions and malformed
    // input MUST signal reader-style errors through the public entrypoints.
    let src = "(foo)";
    let (_, pos) = read_from_string(src).unwrap();
    assert_eq!(pos, src.len());

    expect_stream_error_contains(read_from_string("(foo"), "unterminated list");
    expect_stream_error_contains(read_from_string("\"abc"), "unterminated string");
    expect_stream_error_contains(read_from_string("\\"), "trailing single escape");
    expect_stream_error_contains(read_from_string("MISSING-PKG:SYM"), "package not found");
    expect_error_contains(read_from_string("1/0"), "division by zero");
}

#[test]
fn read_acceptance_uses_reader_state_string_input_and_read_suppress() {
    // Per R4.01, the public read entrypoint MUST consume a real input stream;
    // the bootstrap ReaderState interface is the observable entrypoint here.
    let raw_source = parse("\"(foo)\"");

    let mut state = ReaderState::new();
    state.set_input(raw_source);
    let form = read(&mut state).unwrap();
    assert_eq!(symbol_text(list_items(form)[0]).as_deref(), Some("FOO"));

    let mut suppressed = ReaderState::new();
    suppressed.set_input(raw_source);
    suppressed.set_read_suppress(true);
    assert_eq!(read(&mut suppressed).unwrap(), NIL);
}

#[test]
fn reader_hardening_disables_read_eval_by_default_and_allows_explicit_opt_in() {
    // Per R8.13 and R8.22, read-time evaluation MUST be disabled by default.
    // Per R4.06, # dispatch still needs an opt-in path for #. when enabled.
    expect_error_contains(read_from_string("#.(+ 1 2)"), "*READ-EVAL* is false");

    let raw_source = parse("\"#.(+ 1 2)\"");
    let mut state = ReaderState::new();
    state.set_input(raw_source);
    state.set_read_eval(true);
    let value = read(&mut state).unwrap();
    assert_eq!(value.as_fixnum(), 3);

    let quoted_source = parse("\"#.(quote hello)\"");
    let mut quoted = ReaderState::new();
    quoted.set_input(quoted_source);
    quoted.set_read_eval(true);
    let quoted_value = read(&mut quoted).unwrap();
    assert_eq!(symbol_text(quoted_value).as_deref(), Some("HELLO"));

    let car_source = parse("\"#.(car '(1 2))\"");
    let mut car_state = ReaderState::new();
    car_state.set_input(car_source);
    car_state.set_read_eval(true);
    let car_value = read(&mut car_state).unwrap();
    assert_eq!(car_value.as_fixnum(), 1);
}

#[test]
fn reader_hardening_rejects_circular_notation_by_default() {
    // Per R4.07, circular labels are part of the reader surface.
    // Per R8.12, the default user-facing entrypoint MUST reject them while
    // *READ-CIRCULAR* is NIL.
    expect_error_contains(read_from_string("#1=(a . #1#)"), "circular");
}

#[test]
fn reader_hardening_rejects_excessive_nesting() {
    // Per R8.11, the reader MUST defend against deeply nested input using the
    // default depth limit of 4096 levels.
    let current_exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(current_exe)
        .arg("--exact")
        .arg("reader_hardening_rejects_excessive_nesting_worker")
        .arg("--test-threads=1")
        .env("EGCL_NESTING_CHILD", "1")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "nested reader input should fail gracefully instead of aborting"
    );
}

#[test]
fn reader_hardening_rejects_excessive_nesting_worker() {
    if std::env::var_os("EGCL_NESTING_CHILD").is_none() {
        return;
    }

    let depth = 4097usize;
    let source = format!("{}0{}", "(".repeat(depth), ")".repeat(depth));
    expect_error_contains(read_from_string(&source), "nest");
}

#[test]
fn macroexpand_1_performs_exactly_one_step_while_macroexpand_iterates() {
    // Per R4.10 and R4.11, macroexpand-1 MUST perform one step, while
    // macroexpand MUST iterate to a fixed point without descending into subforms.
    let _guard = macro_state_guard();
    set_macroexpand_hook(passthrough_hook);

    let a = sym("CHAIN-A");
    let b = sym("CHAIN-B");
    define_global_macro(a, parse("(CHAIN-B)"));
    define_global_macro(b, EgclVal::from_fixnum(42));

    let form = parse("(CHAIN-A)");
    let (once, expanded_once) = macroexpand_1(form, &Environment::null()).unwrap();
    assert!(expanded_once);
    assert_eq!(symbol_text(list_items(once)[0]).as_deref(), Some("CHAIN-B"));

    let (fully, expanded_any) = macroexpand(form, &Environment::null()).unwrap();
    assert!(expanded_any);
    assert_eq!(fully.as_fixnum(), 42);

    undefine_global_macro(a);
    undefine_global_macro(b);
}

#[test]
fn macroexpand_does_not_descend_into_non_macro_subforms() {
    // Per R4.11, macroexpand MUST NOT recurse into subforms; only the
    // top-level form is considered.
    let _guard = macro_state_guard();
    set_macroexpand_hook(passthrough_hook);

    let inner = sym("INNER-MACRO");
    define_global_macro(inner, EgclVal::from_fixnum(99));

    let form = parse("(LIST (INNER-MACRO))");
    let (expanded, did_expand) = macroexpand(form, &Environment::null()).unwrap();
    assert!(!did_expand);
    assert_eq!(expanded, form);

    undefine_global_macro(inner);
}

#[test]
fn macroexpand_all_consults_compiler_macros_and_honors_decline() {
    // Per R4.12, the compiler MUST consult compiler macros during the code walk;
    // a declining compiler macro MUST fall through to the original function call.
    let _guard = macro_state_guard();
    set_macroexpand_hook(passthrough_hook);

    let ordinary = sym("TWOSTEP");
    let target = sym("TARGET-FN");
    let declined = sym("DECLINED-FN");
    let forty_two = EgclVal::from_fixnum(42);

    define_global_macro(ordinary, parse("(TARGET-FN 0)"));
    define_compiler_macro(target, Arc::new(move |_whole, _env| Ok(forty_two)));
    define_compiler_macro(declined, Arc::new(|whole, _env| Ok(whole)));

    let expanded = macroexpand_all(parse("(TWOSTEP 0)"), &Environment::null()).unwrap();
    assert_eq!(expanded.as_fixnum(), 42);

    let unchanged = macroexpand_all(parse("(DECLINED-FN 1)"), &Environment::null()).unwrap();
    let unchanged_items = list_items(unchanged);
    assert_eq!(
        symbol_text(unchanged_items[0]).as_deref(),
        Some("DECLINED-FN")
    );
    assert_eq!(unchanged_items[1].as_fixnum(), 1);

    undefine_global_macro(ordinary);
    undefine_compiler_macro(target);
    undefine_compiler_macro(declined);
}

#[test]
fn macroexpand_all_expands_symbol_macros_and_rewrites_setq_to_setf() {
    // Per R4.13, symbol macros MUST expand in variable position, and SETQ of
    // a symbol macro MUST be rewritten to SETF of its expansion.
    let place = parse("(CAR PLACE)");
    let env = Environment::null().augment_variable(sym("X"), VariableInfo::SymbolMacro(place));

    let (symbol_expansion, expanded) = macroexpand_1(sym("X"), &env).unwrap();
    assert!(expanded);
    assert_eq!(symbol_expansion, place);

    let rewritten = macroexpand_all(parse("(SETQ X (+ 1 2))"), &env).unwrap();
    let rewritten_items = list_items(rewritten);
    assert_eq!(symbol_text(rewritten_items[0]).as_deref(), Some("SETF"));
    assert_eq!(rewritten_items[1], place);
}

#[test]
fn macrolet_expander_can_intern_keyword_names() {
    // ASDF's ENSURE-PATHNAME defines ERR this way.  Eagerly expanding the
    // surrounding function body must be able to evaluate the pure INTERN*
    // call in the local macro expander instead of abandoning bytecode
    // compilation for every invocation of ENSURE-PATHNAME.
    let expanded = macroexpand_all(
        parse(
            "(macrolet ((err (constraint) \
               `(quote ,(intern* constraint :keyword)))) \
               (err want-file))",
        ),
        &Environment::null(),
    )
    .expect("expand ASDF-style ERR macrolet");

    let quoted = list_items(expanded);
    assert_eq!(symbol_text(quoted[0]).as_deref(), Some("QUOTE"));
    assert_eq!(symbol_text(quoted[1]).as_deref(), Some("KEYWORD:WANT-FILE"));
}

#[test]
fn macrolet_expander_can_select_generated_syntax_with_if() {
    // ASDF's ENSURE-PATHNAME TRANSFORM macro uses IF while constructing its
    // quasiquoted expansion.  The choice happens while the local macro runs,
    // not in the generated run-time form.
    let expanded = macroexpand_all(
        parse(
            "(macrolet ((choose (condition) \
               `(quote ,(if condition :yes :no)))) \
               (choose t))",
        ),
        &Environment::null(),
    )
    .expect("expand local macro containing IF");

    let quoted = list_items(expanded);
    assert_eq!(symbol_text(quoted[0]).as_deref(), Some("QUOTE"));
    assert_eq!(symbol_text(quoted[1]).as_deref(), Some("KEYWORD:YES"));
}

#[test]
fn macroexpand_environment_shadowing_and_declarations_are_visible() {
    // Per R4.14, the environment protocol MUST return accurate information for
    // bindings and declarations visible at macro-expansion time.
    let base = Environment::null().augment_variable(
        sym("X"),
        VariableInfo::SymbolMacro(EgclVal::from_fixnum(7)),
    );
    let child = base.augment_environment(
        vec![(sym("X"), VariableInfo::Lexical)],
        vec![(sym("BLOCK"), FunctionInfo::SpecialOperator)],
        vec![DeclInfo::Declaration(sym("MY-DECL").0)],
    );

    let (expanded, did_expand) = macroexpand_1(sym("X"), &child).unwrap();
    assert!(!did_expand);
    assert_eq!(expanded, sym("X"));
    assert_eq!(child.declaration_information(sym("MY-DECL")), Some(T));
    assert!(matches!(
        child.function_information(sym("BLOCK")),
        Some(FunctionInfo::SpecialOperator)
    ));
}

#[test]
fn macroexpand_uses_the_macroexpand_hook() {
    // Per R4.15, macroexpand-1 MUST call *macroexpand-hook* to perform the
    // actual expansion.
    let _guard = macro_state_guard();
    HOOK_CALLED.store(false, Ordering::SeqCst);
    set_macroexpand_hook(recording_hook);

    let name = sym("HOOKED-MACRO");
    define_global_macro(name, EgclVal::from_fixnum(123));

    let (expanded, did_expand) =
        macroexpand_1(parse("(HOOKED-MACRO)"), &Environment::null()).unwrap();
    assert!(did_expand);
    assert_eq!(expanded.as_fixnum(), 123);
    assert!(HOOK_CALLED.load(Ordering::SeqCst));

    undefine_global_macro(name);
    set_macroexpand_hook(passthrough_hook);
}

#[test]
fn parse_macro_and_enclose_capture_the_defining_environment() {
    // Per R4.14, parse-macro/enclose must produce a local macro expander that
    // closes over the lexical macroexpansion environment visible at definition time.
    let defining_env = Environment::null().augment_variable(
        sym("X"),
        VariableInfo::SymbolMacro(EgclVal::from_fixnum(41)),
    );
    let parsed =
        parse_macro(sym("M"), NIL, parse("(X)"), Some(&defining_env)).expect("parse local macro");
    let expander = enclose(parsed, &defining_env).expect("close local macro");
    let call_env = Environment::null().augment_function(sym("M"), FunctionInfo::Macro(expander));

    let (expanded, did_expand) = macroexpand_1(parse("(M)"), &call_env).unwrap();
    assert!(did_expand);
    assert_eq!(expanded.as_fixnum(), 41);
}

#[test]
fn macroexpand_detects_circular_expansion() {
    // Per R4.16, circular expansion in a single expansion chain MUST signal
    // program-error style failure rather than looping forever.
    let _guard = macro_state_guard();
    set_macroexpand_hook(passthrough_hook);
    set_macroexpand_limit(32);

    let cyc = sym("CYCLE");
    define_global_macro(cyc, parse("(CYCLE)"));

    expect_error_contains(
        macroexpand(parse("(CYCLE)"), &Environment::null()),
        "circular",
    );

    undefine_global_macro(cyc);
    set_macroexpand_limit(65536);
}
