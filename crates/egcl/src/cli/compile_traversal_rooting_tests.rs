// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn top_level_processing_follows_a_relocated_tail() {
    check_relocated_tail(false);
}

#[test]
fn definition_seeding_follows_a_relocated_tail() {
    check_relocated_tail(true);
}

fn check_relocated_tail(seed: bool) {
    const CHILD: &str = "EGCL_COMPILE_TRAVERSAL_ROOT_CHILD";
    let name = if seed {
        "definition_seeding_follows_a_relocated_tail"
    } else {
        "top_level_processing_follows_a_relocated_tail"
    };
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("cli::compile_traversal_rooting_tests::{name}"),
                "--nocapture",
            ])
            .env(CHILD, "1")
            // Build fresh input, then collect explicitly during recursion.
            .env("EGCL_GC_STRESS", "0")
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("COMPILE-TAIL-RELOCATED"));
        return;
    }
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 4096,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.0,
    })
    .unwrap();
    egcl_rt::rooted!(env = Env::new(false));
    let (first, second) = if seed {
        (
            "(defconstant +traversal-first+ (progn (%force-minor-gc-for-test) 41))",
            "(defconstant +traversal-next+ 73)",
        )
    } else {
        (
            "(eval-when (:compile-toplevel) (%force-minor-gc-for-test))",
            "(eval-when (:compile-toplevel) (setq +traversal-next+ 73))",
        )
    };
    egcl_rt::rooted!(first = read_from_string_in_env(first, &mut env).unwrap().0);
    egcl_rt::rooted!(second = read_from_string_in_env(second, &mut env).unwrap().0);
    let marker = resolve_sym("+TRAVERSAL-NEXT+").unwrap();
    let progn = resolve_sym("PROGN").unwrap();
    egcl_rt::rooted!(tail = arena_cons(*second, NIL));
    egcl_rt::rooted!(body = arena_cons(*first, *tail));
    egcl_rt::rooted!(form = arena_cons(progn, *body));
    let before = *tail;
    with_eval_context(&mut env, EvalContext::CompileFile, |env| {
        if seed {
            seed_compile_time_definitions(*form, env);
        } else {
            process_compile_toplevel_form(*form, env)?;
        }
        Ok(())
    })
    .unwrap();
    assert_ne!(
        *tail, before,
        "the tail must actually move during recursion"
    );
    assert_eq!(
        env.lookup_var_symbol(marker),
        Some(EgclVal::from_fixnum(73))
    );
    println!("COMPILE-TAIL-RELOCATED");
}
