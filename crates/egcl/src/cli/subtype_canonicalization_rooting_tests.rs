use super::*;

const CHILD: &str = "EGCL_SUBTYPE_CANONICALIZATION_CHILD";
const FILTER: &str =
    "cli::subtype_canonicalization_rooting_tests::canonicalization_moves_the_other_operand";

#[test]
fn canonicalization_preserves_both_operands_under_real_relocation() {
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            FILTER,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_POISON",
            "EGCL_FORCE_TIER",
            "EGCL_GC_DISABLE",
        ] {
            command.env_remove(key);
        }
        command
            .env(CHILD, if stress { "moving" } else { "normal" })
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
        if stress {
            command.env("EGCL_GC_STRESS", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}: {}\n{stdout}\n{stderr}",
            output.status
        );
        results.push(
            stdout
                .lines()
                .find(|line| line.starts_with("SUBTYPE-RESULTS "))
                .expect("child emits semantic answers")
                .to_owned(),
        );
        if stress {
            assert!(stdout.contains("SUBTYPE-RELOCATED 4"), "{stdout}\n{stderr}");
        }
    }
    assert_eq!(
        results[0], results[1],
        "moving and non-stress answers must agree"
    );
}

#[test]
#[ignore = "spawned in a fresh process because allocation stress configuration is cached"]
fn canonicalization_moves_the_other_operand() {
    let mode = std::env::var(CHILD).expect("run through the parent test");
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let unsigned = resolve_sym("UNSIGNED-BYTE").unwrap();
    let integer = resolve_sym("INTEGER").unwrap();
    // Intern everything the canonicalizer consults before arming stress.
    resolve_sym("*").unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::gc::stress_arm_after_init();
    let mut answers = Vec::new();
    let mut moved = 0;
    for row in 0..4 {
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::rooted!(left = NIL);
        egcl_rt::rooted!(right = NIL);
        let fresh_right = row < 2;
        if fresh_right {
            *left = vec_to_list(&[
                unsigned,
                EgclVal::from_fixnum(if row == 0 { 8 } else { 16 }),
            ]);
            *right = vec_to_list(&[
                unsigned,
                EgclVal::from_fixnum(if row == 0 { 16 } else { 8 }),
            ]);
        } else {
            *right = vec_to_list(&[unsigned, EgclVal::from_fixnum(8)]);
            *left = vec_to_list(&[
                integer,
                EgclVal::from_fixnum(if row == 2 { 0 } else { -1 }),
                EgclVal::from_fixnum(7),
            ]);
        }
        // This outer cons was allocated last. The first allocating
        // canonicalization is of its OTHER operand in both branch shapes.
        let before = if fresh_right {
            right.to_raw()
        } else {
            left.to_raw()
        };
        let answer = subtypep_relation(*left, *right);
        let after = if fresh_right {
            right.to_raw()
        } else {
            left.to_raw()
        };
        if before != after {
            moved += 1;
        }
        if mode == "moving" {
            assert_ne!(
                before, after,
                "row {row}: opposite operand must actually move inside canonicalization"
            );
        }
        let expected = (row % 2 == 0, true);
        assert_eq!(
            answer, expected,
            "row {row}: both certainty and containment survive"
        );
        answers.push(answer);
    }
    println!("\nSUBTYPE-RESULTS {answers:?}");
    println!("SUBTYPE-RELOCATED {moved}");
}
