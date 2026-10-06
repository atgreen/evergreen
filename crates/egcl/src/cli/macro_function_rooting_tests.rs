// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn snapshots_and_installed_functions_follow_relocated_inputs() {
    const CHILD: &str = "EGCL_MACRO_FUNCTION_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::macro_function_rooting_tests::snapshots_and_installed_functions_follow_relocated_inputs",
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
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("MACRO-INPUTS-RELOCATED"));
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
    })
    .unwrap();
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(whole = arena_cons(resolve_sym("SNAPSHOT-PROBE").unwrap(), NIL));
    egcl_rt::rooted!(
        definition = MacroDef {
            params_form: NIL,
            body: arena_cons(EgclVal::from_fixnum(42), NIL),
            captured_frame: Arc::clone(&env.frame),
            bytecode: None,
            function: None,
        }
    );
    let before_body = definition.body;
    egcl_rt::rooted!(snapshot = snapshot_macro_expander(&env, &definition));
    assert_ne!(
        definition.body, before_body,
        "the macro body must actually move"
    );
    assert_eq!(
        apply_function(*snapshot, &[*whole, NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(42),
    );

    egcl_rt::rooted!(
        params = vec_to_list(&[
            resolve_sym("WHOLE").unwrap(),
            resolve_sym("ENVIRONMENT").unwrap(),
        ])
    );
    let name = gensym_symbol("INSTALLED-EXPANDER");
    egcl_rt::rooted!(body = arena_cons(EgclVal::from_fixnum(73), NIL));
    egcl_rt::rooted!(function = egcl_rt::function::alloc_interpreted(*params, *body, NIL, name));
    let before_function_body = *body;
    egcl_rt::rooted!(installed = macro_definition_for_function(&env, *function));
    assert_ne!(
        *body, before_function_body,
        "the pinned function's body must actually move"
    );
    assert_eq!(installed.function, Some(*function));
    assert_eq!(egcl_rt::function::body(*function), *body);
    let call = list_to_vec(cp(installed.body).0);
    assert_eq!(cp(cp(call[1]).1).0, *function);
    // Exercise the source representation used after image restoration.
    installed.function = None;
    egcl_rt::rooted!(restored = snapshot_macro_expander(&env, &installed));
    assert_eq!(
        apply_function(*restored, &[*whole, NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(73),
    );
    println!("MACRO-INPUTS-RELOCATED");
}
