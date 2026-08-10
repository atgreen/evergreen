//! Tests for bliss-stdlib conditions module (spec §5.4).
use bliss_stdlib::conditions::*;
use bliss_rt::value::BlissVal;

fn sym(i: u32) -> BlissVal { BlissVal::from_symbol_index(i) }

#[test]
fn make_simple_error_variants() {
    let _ = make_simple_error("err: ~A", &[BlissVal::from_fixnum(42)]);
    let _ = make_simple_error("plain", &[]);
}

#[test]
fn make_type_error_returns_val() {
    let _ = make_type_error(BlissVal::from_fixnum(5), sym(1));
}

#[test]
fn signal_no_handler_ok() {
    assert!(signal_condition(make_simple_error("t", &[])).is_ok());
}

#[test]
fn error_condition_with_debugger_hook() {
    set_debugger_hook(Some(BlissVal::from_fixnum(1)));
    let _ = error_condition(make_simple_error("unhandled", &[]));
    set_debugger_hook(None);
}

#[test]
fn cerror_callable() {
    let _ = cerror("Continue", make_simple_error("cont", &[]));
}

#[test]
fn warn_returns_ok() {
    assert!(warn_condition(make_simple_error("warning", &[])).is_ok());
}

#[test]
fn handler_bind_no_signal() {
    let body = BlissVal::from_fixnum(99);
    assert_eq!(handler_bind(&[(sym(10), BlissVal::from_fixnum(0))], body).unwrap(), body);
    assert_eq!(handler_bind(&[], BlissVal::from_fixnum(77)).unwrap(), BlissVal::from_fixnum(77));
}

#[test]
fn handler_case_no_signal() {
    let form = BlissVal::from_fixnum(55);
    assert_eq!(handler_case(form, &[(sym(20), BlissVal::from_fixnum(0))]).unwrap(), form);
    assert_eq!(handler_case(BlissVal::from_fixnum(33), &[]).unwrap(), BlissVal::from_fixnum(33));
}

#[test]
fn handler_case_nested() {
    let inner = handler_case(BlissVal::from_fixnum(0), &[(sym(31), BlissVal::from_fixnum(200))]);
    let outer = handler_case(inner.unwrap_or(BlissVal::from_fixnum(0)),
                             &[(sym(30), BlissVal::from_fixnum(100))]);
    assert!(outer.is_ok());
}

#[test]
fn restart_spec_fields() {
    let full = RestartSpec {
        name: sym(40), function: BlissVal::from_fixnum(1),
        report_function: Some(BlissVal::from_fixnum(2)),
        interactive_function: Some(BlissVal::from_fixnum(3)),
        test_function: Some(BlissVal::from_fixnum(4)),
    };
    assert_eq!(full.name, sym(40));
    assert!(full.report_function.is_some());
    let minimal = RestartSpec {
        name: sym(41), function: BlissVal::from_fixnum(1),
        report_function: None, interactive_function: None, test_function: None,
    };
    assert!(minimal.interactive_function.is_none());
}

#[test]
fn restart_bind_returns_body() {
    let spec = RestartSpec {
        name: sym(50), function: BlissVal::from_fixnum(1),
        report_function: None, interactive_function: None, test_function: None,
    };
    assert_eq!(restart_bind(&[spec], BlissVal::from_fixnum(42)).unwrap(), BlissVal::from_fixnum(42));
    assert_eq!(restart_bind(&[], BlissVal::from_fixnum(88)).unwrap(), BlissVal::from_fixnum(88));
}

#[test]
fn restart_bind_multiple_specs() {
    let s1 = RestartSpec {
        name: sym(80), function: BlissVal::from_fixnum(1),
        report_function: None, interactive_function: None, test_function: None,
    };
    let s2 = RestartSpec {
        name: sym(81), function: BlissVal::from_fixnum(2),
        report_function: Some(BlissVal::from_fixnum(3)),
        interactive_function: Some(BlissVal::from_fixnum(4)),
        test_function: Some(BlissVal::from_fixnum(5)),
    };
    assert!(restart_bind(&[s1, s2], BlissVal::from_fixnum(0)).is_ok());
}

#[test]
fn compute_restarts_and_find() {
    let _ = compute_restarts(None);
    let _ = compute_restarts(Some(make_simple_error("t", &[])));
    assert!(find_restart(sym(60), None).is_none());
}

#[test]
fn invoke_restart_interactively_callable() {
    let _ = invoke_restart_interactively(BlissVal::from_fixnum(1));
}

#[test]
fn debugger_hook_lifecycle() {
    set_debugger_hook(Some(BlissVal::from_fixnum(1)));
    let _ = invoke_debugger(make_simple_error("debug", &[]));
    set_debugger_hook(None);
}
