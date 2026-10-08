// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn hook_bridge_preserves_relocated_forms_and_expansions() {
    const CHILD: &str = "EGCL_MACROEXPAND_HOOK_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::macroexpand_hook_rooting_tests::hook_bridge_preserves_relocated_forms_and_expansions",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("HOOK-INPUTS-RELOCATED"));
        return;
    }
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.0,
    }).unwrap();
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(hook = read_eval_all_env(
        "(lambda (expander form environment) (funcall expander form environment))",
        &mut env,
    ).unwrap());
    let hook_symbol = resolve_sym("*MACROEXPAND-HOOK*").unwrap();
    egcl_rt::rooted!(_binding = DynBind::establish(hook_symbol, *hook));
    let symbol = gensym_symbol("HOOK-SYMBOL");
    egcl_rt::rooted!(expansion = arena_cons(EgclVal::from_fixnum(73), NIL));
    egcl_rt::rooted!(environment = MacroexpandEnv::null()
        .augment_variable(symbol, VariableInfo::SymbolMacro(*expansion)));
    let before = expansion.to_raw();
    egcl_rt::rooted!(result = invoke_lisp_macroexpand_hook(*expansion, symbol, &environment).unwrap());
    assert_ne!(before, expansion.to_raw(), "the symbol expansion must actually move");
    assert_eq!(*result, *expansion);
    assert_eq!(cp(*result).0, EgclVal::from_fixnum(73));

    let key = compiler_macroexpand::next_registered_macro_key();
    compiler_macroexpand::register_macro_function(key, Arc::new(|form, _| Ok(form)));
    egcl_rt::rooted!(whole = arena_cons(EgclVal::from_fixnum(91), NIL));
    let before = whole.to_raw();
    egcl_rt::rooted!(result = invoke_lisp_macroexpand_hook(key, *whole, &environment).unwrap());
    assert_ne!(before, whole.to_raw(), "the macro form must actually move");
    assert_eq!(*result, *whole);
    assert_eq!(cp(*result).0, EgclVal::from_fixnum(91));
    compiler_macroexpand::unregister_macro_function(key);
    println!("HOOK-INPUTS-RELOCATED");
}
