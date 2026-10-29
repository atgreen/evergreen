// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn aux_entry_prologue_retains_relocated_initforms_and_body() {
    const CHILD: &str = "EGCL_AUX_PROLOGUE_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::bytecode::aux_entry_prologue_tests::aux_entry_prologue_retains_relocated_initforms_and_body",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("AUX-PROLOGUE-INPUTS-RELOCATED"));
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
    let helper = resolve_sym("AUX-ROOT-HELPER").unwrap();
    let required = resolve_sym("AUX-ROOT-REQUIRED").unwrap();
    let let_star = resolve_sym("LET*").unwrap();
    for fresh_body in [false, true] {
        egcl_rt::rooted!(
            params = super::super::read_from_string_in_env(
                "(aux-root-required &aux (aux-root-first nil) (aux-root-second aux-root-first))",
                &mut env,
            )
            .unwrap()
            .0
        );
        egcl_rt::rooted!(
            body = super::super::read_from_string_in_env(
                "((declare (type integer aux-root-required)) aux-root-second)",
                &mut env,
            )
            .unwrap()
            .0
        );
        egcl_rt::rooted!(declaration = cp(*body).0);
        egcl_rt::rooted!(initializer = form_list(&[helper, required]));
        let first_spec = list_to_vec(*params)[2];
        let init_slot = cp(first_spec).1;
        // Install the last-allocated cons without another allocation: stressing
        // then moves it during the helper, rather than during fixture setup.
        unsafe {
            egcl_rt::gc::store_ref(init_slot.as_ptr() as *mut EgclVal, *initializer);
        }
        if fresh_body {
            *body = arena_cons(*declaration, cp(*body).1);
        }
        let before = if fresh_body { *body } else { *initializer };
        let (new_params, new_body) = aux_entry_prologue(*params, *body).expect("eligible AUX list");
        egcl_rt::rooted!(new_params = new_params);
        egcl_rt::rooted!(new_body = new_body);
        let after = if fresh_body { *body } else { *initializer };
        assert_ne!(
            before, after,
            "the selected source input must really move during construction"
        );
        assert_eq!(list_to_vec(*new_params), vec![required]);
        let outer = list_to_vec(*new_body);
        assert_eq!(outer.len(), 1);
        let prologue = list_to_vec(outer[0]);
        assert_eq!(prologue[0], let_star);
        let bindings = list_to_vec(prologue[1]);
        assert_eq!(bindings.len(), 2);
        let first = list_to_vec(bindings[0]);
        let second = list_to_vec(bindings[1]);
        assert_eq!(
            first[1], *initializer,
            "initializer is retained as executable source"
        );
        assert_eq!(
            second[1], first[0],
            "later AUX initializer refers to the earlier binding"
        );
        assert_eq!(
            prologue[2], *declaration,
            "leading declarations retain exact identity"
        );
        assert_eq!(prologue[3], second[0]);
    }
    println!("AUX-PROLOGUE-INPUTS-RELOCATED");
}

#[test]
fn aux_initialization_is_lowered_into_bytecode_instead_of_host_eval() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(
        params = super::super::read_from_string_in_env(
            "(&aux (aux-first 42) (aux-second aux-first))",
            &mut env,
        )
        .unwrap()
        .0
    );
    egcl_rt::rooted!(
        body = super::super::read_from_string_in_env("(aux-second)", &mut env,)
            .unwrap()
            .0
    );
    let function = compile_function("AUX-BYTECODE-PROOF", *params, *body, &env, false, false)
        .expect("AUX-only function must compile");
    assert!(
        !function.variadic,
        "the source AUX list must not reach the general binder"
    );
    assert_eq!(function.arity, 0);
    assert!(function.params_form.is_nil());
    assert!(function.constants.contains(&EgclVal::from_fixnum(42)));
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::Const(_)))
    );
    assert!(
        !function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::EvalHost(_)))
    );
}
