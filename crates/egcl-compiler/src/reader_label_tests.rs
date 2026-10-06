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
    egcl_rt::rooted!(
        labels = CircularLabels {
            labels: HashMap::new()
        }
    );
    egcl_rt::rooted!(placeholder = alloc_cons(NIL, NIL));
    labels.labels.insert(1, *placeholder);
    let before_placeholder = *placeholder;
    let (value, _) = read_token_with_base(&mut input, 0, &mut labels, 10, false, true, 0).unwrap();
    egcl_rt::rooted!(value = value);
    assert_ne!(
        *placeholder, before_placeholder,
        "reading the vector must move its placeholder"
    );
    assert_eq!(labels.labels[&1], *placeholder);
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
