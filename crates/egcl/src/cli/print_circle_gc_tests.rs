// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static MOVED: AtomicBool = AtomicBool::new(false);

fn relocate_and_decline(
    value: EgclVal,
    _: bool,
    _: usize,
) -> Option<egcl_stdlib::format::LayoutDoc> {
    if ARMED.swap(false, Ordering::SeqCst) {
        let before = value;
        egcl_rt::rooted!(value = value);
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(
            *value, before,
            "the pretty-hook operand must actually relocate"
        );
        MOVED.store(true, Ordering::SeqCst);
    }
    None
}

#[test]
fn declining_pretty_hook_preserves_relocated_array_and_circle_label() {
    const CHILD: &str = "EGCL_PRINT_CIRCLE_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::print_circle_gc_tests::declining_pretty_hook_preserves_relocated_array_and_circle_label", "--nocapture"])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_FORCE_TIER", "interp")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .output().unwrap();
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
    let circle = egcl_rt::symbols::intern("*PRINT-CIRCLE*");
    let pretty = egcl_rt::symbols::intern("*PRINT-PRETTY*");
    egcl_rt::symbols::set_symbol_value(circle, T);
    egcl_rt::symbols::set_symbol_value(pretty, T);
    egcl_stdlib::format::set_pprint_dispatch_hook(Some(relocate_and_decline));
    egcl_rt::rooted!(object = egcl_stdlib::sequences::build_simple_vector(&[NIL]));
    egcl_stdlib::sequences::set_svref(*object, 0, *object).unwrap();
    ARMED.store(true, Ordering::SeqCst);
    let mut text = String::new();
    print_val(*object, &mut text);
    assert!(MOVED.load(Ordering::SeqCst));
    assert_eq!(text, "#1=#(#1#)");
}
