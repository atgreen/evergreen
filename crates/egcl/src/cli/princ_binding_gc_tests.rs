// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static MOVED: AtomicBool = AtomicBool::new(false);

fn relocate_and_decline(
    value: EgclVal,
    escape: bool,
    _: usize,
) -> Option<egcl_stdlib::format::LayoutDoc> {
    if ARMED.swap(false, Ordering::SeqCst) {
        assert!(escape, "WRITE readably remains an escaped operation");
        let readably = egcl_rt::symbols::find_index("*PRINT-READABLY*").unwrap();
        assert_eq!(egcl_rt::symbols::symbol_value(readably), Some(T));
        let before = value;
        egcl_rt::rooted!(value = value);
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(*value, before, "WRITE operand must actually move");
        MOVED.store(true, Ordering::SeqCst);
    }
    None
}

#[test]
fn write_restores_a_relocated_outer_readably_binding() {
    const CHILD: &str = "EGCL_WRITE_BINDING_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::princ_binding_gc_tests::write_restores_a_relocated_outer_readably_binding",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    let readably = resolve_sym("*PRINT-READABLY*").unwrap().as_symbol_index();
    egcl_rt::symbols::set_symbol_value(readably, NIL);
    for name in ["*PRINT-ESCAPE*", "*PRINT-PRETTY*", "*PRINT-ARRAY*"] {
        let index = resolve_sym(name).unwrap().as_symbol_index();
        egcl_rt::symbols::set_symbol_value(index, T);
    }
    let stream_key = resolve_sym(":STREAM").unwrap();
    let readably_key = resolve_sym(":READABLY").unwrap();
    let escape_key = resolve_sym(":ESCAPE").unwrap();
    let pretty_key = resolve_sym(":PRETTY").unwrap();
    egcl_rt::rooted!(stream = egcl_stdlib::make_string_output_stream(NIL).unwrap());
    egcl_stdlib::format::set_pprint_dispatch_hook(Some(relocate_and_decline));
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(outer = arena_cons(T, NIL));
    egcl_rt::rooted!(
        value = egcl_stdlib::sequences::build_simple_vector(&[EgclVal::from_fixnum(7)])
    );
    assert!(egcl_rt::symbols::bind_symbol_value(readably, *outer).is_none());
    let before = *outer;
    egcl_rt::rooted!(
        args = vec![
            *value,
            stream_key,
            *stream,
            readably_key,
            T,
            escape_key,
            NIL,
            pretty_key,
            T
        ]
    );
    ARMED.store(true, Ordering::SeqCst);
    egcl_rt::rooted!(
        result = evaluated_builtins::call("WRITE", &args, &mut env)
            .unwrap()
            .unwrap()
    );
    assert!(MOVED.load(Ordering::SeqCst));
    assert_ne!(
        *outer, before,
        "saved outer READABLY value must actually move"
    );
    assert_eq!(egcl_rt::symbols::symbol_value(readably), Some(*outer));
    assert_eq!(*result, *value);
    egcl_rt::rooted!(text = egcl_stdlib::get_output_stream_string(*stream).unwrap());
    assert_eq!(text.as_string(), "#(7)");
    egcl_rt::symbols::restore_symbol_binding(readably, None);
    egcl_stdlib::format::set_pprint_dispatch_hook(None);
}
