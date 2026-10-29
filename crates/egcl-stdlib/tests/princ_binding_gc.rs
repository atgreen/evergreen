// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::EgclVal;
use egcl_rt::value::{NIL, T};
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static MOVED: AtomicBool = AtomicBool::new(false);

fn relocate_and_decline(
    value: EgclVal,
    escape: bool,
    _: usize,
) -> Option<egcl_stdlib::format::LayoutDoc> {
    assert!(!escape);
    for name in ["*PRINT-READABLY*", "*PRINT-ESCAPE*"] {
        let index = egcl_rt::symbols::find_index(name).unwrap();
        assert_eq!(egcl_rt::symbols::symbol_value(index), Some(NIL));
    }
    if value.is_heap_object() && ARMED.swap(false, Ordering::SeqCst) {
        let before = value;
        egcl_rt::rooted!(value = value);
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(
            *value, before,
            "declining pretty operand must actually move"
        );
        MOVED.store(true, Ordering::SeqCst);
    }
    None
}

#[test]
fn aesthetic_bindings_and_declining_pretty_operand_survive_relocation() {
    const CHILD: &str = "EGCL_PRINC_BINDING_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "aesthetic_bindings_and_declining_pretty_operand_survive_relocation",
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
    let readably = egcl_rt::symbols::intern("*PRINT-READABLY*");
    let escape = egcl_rt::symbols::intern("*PRINT-ESCAPE*");
    let pretty = egcl_rt::symbols::intern("*PRINT-PRETTY*");
    let array = egcl_rt::symbols::intern("*PRINT-ARRAY*");
    for index in [readably, escape, pretty, array] {
        egcl_rt::symbols::set_symbol_value(index, T);
    }
    egcl_stdlib::format::set_pprint_dispatch_hook(Some(relocate_and_decline));
    // Drain setup allocations before creating the two fresh movable probes.
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        outer = egcl_stdlib::streams::make_lisp_string_fresh("outer generalized boolean")
    );
    egcl_rt::rooted!(
        value = egcl_stdlib::sequences::build_simple_vector(&[EgclVal::from_fixnum(7)])
    );
    assert!(egcl_rt::symbols::bind_symbol_value(readably, *outer).is_none());
    let outer_before = *outer;
    let value_before = *value;
    ARMED.store(true, Ordering::SeqCst);
    egcl_rt::rooted!(text = egcl_stdlib::format(NIL, "~A", &[*value]).unwrap());
    assert!(MOVED.load(Ordering::SeqCst));
    assert_ne!(*value, value_before);
    assert_ne!(
        *outer, outer_before,
        "saved generalized boolean must move too"
    );
    assert_eq!(text.as_string(), "#(7)");
    assert_eq!(egcl_rt::symbols::symbol_value(readably), Some(*outer));
    assert_eq!(egcl_rt::symbols::symbol_value(escape), Some(T));
    egcl_rt::symbols::restore_symbol_binding(readably, None);
    egcl_stdlib::format::set_pprint_dispatch_hook(None);
}
