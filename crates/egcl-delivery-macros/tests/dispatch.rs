use egcl_delivery_macros::builtin_dispatch;

#[cfg(not(egcl_specialized_builtins))]
fn omitted_implementation() -> i32 {
    99
}

#[builtin_dispatch(name)]
fn dispatch(name: &str, argument: i32) -> Option<i32> {
    Some(match name {
        "CAR" | "FIRST" if argument > 0 => argument,
        name @ ("CDR" | "REST") => name.len() as i32,
        "SIN" => omitted_implementation(),
        _ => return None,
    })
}

#[test]
fn aliases_guards_and_fallback_survive_specialization() {
    assert_eq!(dispatch("CAR", 42), Some(42));
    assert_eq!(dispatch("FIRST", 42), Some(42));
    assert_eq!(dispatch("CAR", 0), None);
    assert_eq!(dispatch("UNKNOWN", 42), None);
    #[cfg(egcl_specialized_builtins)]
    {
        assert_eq!(dispatch("CDR", 42), None);
        assert_eq!(dispatch("SIN", 42), None);
    }
    #[cfg(not(egcl_specialized_builtins))]
    {
        assert_eq!(dispatch("CDR", 42), Some(3));
        assert_eq!(dispatch("SIN", 42), Some(99));
    }
}
