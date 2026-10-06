// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn constructor_check_forms_follow_relocated_inputs() {
    const CHILD: &str = "EGCL_STRUCT_SLOT_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::struct_slot_type_tests::constructor_check_forms_follow_relocated_inputs",
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-INPUTS-RELOCATED"));
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
    ensure_package_registry();
    let integer = resolve_sym("INTEGER").unwrap();
    for fresh_type in [true, false] {
        egcl_rt::rooted!(form = arena_cons(EgclVal::from_fixnum(42), NIL));
        egcl_rt::rooted!(
            slot_type = vec_to_list(&[integer, EgclVal::from_fixnum(0), EgclVal::from_fixnum(9),])
        );
        if !fresh_type {
            *form = arena_cons(EgclVal::from_fixnum(43), NIL);
        }
        let before = if fresh_type { *slot_type } else { *form };
        egcl_rt::rooted!(checked = checked_struct_slot_form(*form, *slot_type));
        let after = if fresh_type { *slot_type } else { *form };
        assert_ne!(before, after, "the input under test must actually move");
        let parts = list_to_vec(*checked);
        let binding = list_to_vec(cp(parts[1]).0);
        let check = list_to_vec(parts[2]);
        assert_eq!(
            binding[1], *form,
            "LET must contain the relocated value form"
        );
        assert_eq!(
            check[2], *slot_type,
            "CHECK-TYPE must contain the relocated type"
        );
        assert_eq!(binding[0], check[1]);
        assert_eq!(binding[0], parts[3]);
    }
    egcl_rt::rooted!(env = Env::new(false));
    let class = resolve_sym("WRITER-ROOT-CLASS").unwrap();
    let slot = resolve_sym("WRITER-ROOT-SLOT").unwrap();
    let accessor = resolve_sym("WRITER-ROOT-ACCESSOR").unwrap();
    let property = resolve_sym(STRUCT_SLOT_TYPES_PROPERTY).unwrap();
    egcl_rt::rooted!(types = vec_to_list(&[slot, integer]));
    symbol_plist_put(class, property, *types);
    let setf = resolve_sym("SETF").unwrap();
    let check = resolve_sym("EGCL::%CHECKED-STRUCTURE-SLOT-VALUE").unwrap();
    egcl_rt::rooted!(method_name = vec_to_list(&[setf, accessor]));
    let before = *method_name;
    install_slot_accessor_method(&mut env, *method_name, class, slot, true).unwrap();
    assert_ne!(*method_name, before, "the checked writer name must move");
    let key = function_name_key(*method_name);
    let methods = env.methods.borrow();
    let method = methods.get(&key).unwrap().last().unwrap();
    fn contains(form: EgclVal, symbol: EgclVal) -> bool {
        if form.is_cons() {
            let (car, cdr) = cp(form);
            contains(car, symbol) || contains(cdr, symbol)
        } else {
            form == symbol
        }
    }
    assert!(contains(method.body, check), "the checked writer body must survive GC");
    println!("STRUCT-INPUTS-RELOCATED");
}
