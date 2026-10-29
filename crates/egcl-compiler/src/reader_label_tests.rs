// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn circular_vector_patch_survives_relocated_placeholder_and_result() {
    const CHILD: &str = "EGCL_READER_LABEL_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reader::reader_label_tests::circular_vector_patch_survives_relocated_placeholder_and_result",
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("READER-LABEL-RELOCATED"));
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
    let chars = "#(#1#)".chars().collect::<Vec<_>>();
    let mut input = ReaderInput::slice(&chars);
    egcl_rt::rooted!(labels = CircularLabels::default());
    egcl_rt::rooted!(placeholder = alloc_cons(NIL, NIL));
    labels.labels.borrow_mut().insert(1, *placeholder);
    let before_placeholder = *placeholder;
    let (value, _) = read_token_with_base(&mut input, 0, &labels, 10, false, true, 0).unwrap();
    egcl_rt::rooted!(value = value);
    assert_ne!(
        *placeholder, before_placeholder,
        "reading the vector must move its placeholder"
    );
    assert_eq!(labels.labels.borrow()[&1], *placeholder);
    patch_circular_label(*value, *placeholder, std::iter::empty());
    // SAFETY: the parsed object is a one-element simple vector.
    assert_eq!(
        unsafe { *(value.as_ptr().add(16) as *const EgclVal) },
        *value
    );
    let before_value = *value;
    egcl_rt::collect_t0_minor().unwrap();
    assert_ne!(
        *value, before_value,
        "the completed vector must actually move"
    );
    assert_eq!(
        unsafe { *(value.as_ptr().add(16) as *const EgclVal) },
        *value
    );
    // Give a one-slot instance poisoned alignment padding. Only its logical
    // slot may be traversed or rewritten, never the extra allocation word.
    let instance_body = egcl_rt::gc::alloc_typed(16, type_id::STANDARD_OBJECT).unwrap();
    let instance = unsafe {
        *(instance_body as *mut u64) = 0;
        *(instance_body.add(8) as *mut EgclVal) = *placeholder;
        *(instance_body.add(16) as *mut u64) = 0xfafafafafafafafa;
        EgclVal::from_heap_ptr(instance_body.sub(8))
    };
    egcl_rt::rooted!(instance = instance);
    patch_circular_label(*instance, *placeholder, std::iter::empty());
    unsafe {
        assert_eq!(*(instance.as_ptr().add(16) as *const EgclVal), *instance);
        assert_eq!(
            *(instance.as_ptr().add(24) as *const u64),
            0xfafafafafafafafa
        );
    }
    println!("READER-LABEL-RELOCATED");
}

#[test]
fn recursive_label_scope_keeps_relocated_values_and_restores_after_errors() {
    const CHILD: &str = "EGCL_RECURSIVE_LABEL_ROOT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=5", "45"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "reader::reader_label_tests::recursive_label_scope_keeps_relocated_values_and_restores_after_errors", "--nocapture"])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("RECURSIVE-LABEL-RELOCATED"));
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
    {
        let scope = CircularLabelScope::enter(false);
        egcl_rt::rooted!(labels = scope.labels.clone());
        // The map is the only root for this value while the nested parser runs.
        let before = alloc_cons(EgclVal::from_fixnum(73), NIL);
        labels.labels.borrow_mut().insert(1, before);
        let chars = "((42) #1#)".chars().collect::<Vec<_>>();
        egcl_rt::rooted!(
            parsed = read_form_at_recursive(&chars, 0, 10, false, true)
                .unwrap()
                .0
        );
        egcl_rt::rooted!(after = labels.labels.borrow()[&1]);
        assert_ne!(
            before, *after,
            "nested parsing must relocate the label-only value"
        );
        let second = unsafe { (*(parsed.as_ptr() as *const ConsCell)).cdr };
        assert_eq!(
            unsafe { (*(second.as_ptr() as *const ConsCell)).car },
            *after
        );
        assert_eq!(
            unsafe { (*(after.as_ptr() as *const ConsCell)).car },
            EgclVal::from_fixnum(73)
        );
        // A nonrecursive operation has its own table, including on failure.
        assert!(read_from_string("#1#").is_err());
        let chars = "#1#".chars().collect::<Vec<_>>();
        assert_eq!(
            read_form_at_recursive(&chars, 0, 10, false, true)
                .unwrap()
                .0,
            *after
        );
        assert!(
            read_form_at_recursive(&"#2=(".chars().collect::<Vec<_>>(), 0, 10, false, false)
                .is_err()
        );
        assert_eq!(
            read_form_at_recursive(&chars, 0, 10, false, true)
                .unwrap()
                .0,
            *after
        );
    }
    assert!(
        read_from_string("#1#").is_err(),
        "labels must not outlive their enclosing read"
    );
    assert!(ACTIVE_CIRCULAR_LABELS.with(|active| active.borrow().is_none()));
    println!("RECURSIVE-LABEL-RELOCATED");
}
