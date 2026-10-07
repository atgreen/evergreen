// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn search_cursor_survives_relocating_callbacks() {
    const CHILD: &str = "EGCL_SEARCH_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "90"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::search_rooting_tests::search_cursor_survives_relocating_callbacks",
                "--nocapture",
            ])
            .env(CHILD, "1")
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("SEARCH-CURSOR-RELOCATED"));
        return;
    }
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    read_eval_all_env(
        "(defun search-collect-key (value)
             (eval '(%force-minor-gc-for-test))
             (car (cons value nil)))
         (defun search-collect-test (left right)
             (eval '(%force-minor-gc-for-test))
             (eql left right))",
        &mut env,
    )
    .unwrap();
    read_eval_all_env(EMBEDDED_BOOT_LISP, &mut env).unwrap();
    BOOT_COMPLETE.with(|flag| flag.set(true));
    for vector in [false, true] {
        for (expression, expected) in [
            ("(search '(2 3) search-source :key #'search-collect-key)", 1),
            (
                "(search '(2 3) search-source :key #'search-collect-key :from-end t)",
                4,
            ),
            (
                "(search '(2 3) search-source :test #'search-collect-test)",
                1,
            ),
            (
                "(search '(2 3) search-source :test #'search-collect-test :from-end t)",
                4,
            ),
        ] {
            egcl_rt::rooted!(form = read_from_string_in_env(expression, &mut env).unwrap().0);
            env.set_var("SEARCH-SOURCE", NIL);
            egcl_rt::collect_t0_minor().unwrap();
            egcl_rt::rooted!(source = NIL);
            if vector {
                *source = egcl_stdlib::build_simple_vector(
                    &[0, 2, 3, 4, 2, 3, 5].map(EgclVal::from_fixnum),
                );
            } else {
                for item in [5, 3, 2, 4, 3, 2, 0] {
                    *source = arena_cons(EgclVal::from_fixnum(item), *source);
                }
            }
            env.set_var("SEARCH-SOURCE", *source);
            let before = source.to_raw();
            assert_eq!(
                eval_form(*form, &mut env).unwrap(),
                EgclVal::from_fixnum(expected)
            );
            assert_ne!(
                source.to_raw(),
                before,
                "callback must relocate the candidate sequence"
            );
        }
    }
    println!("SEARCH-CURSOR-RELOCATED");
}
