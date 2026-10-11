// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{BigInt, BigRat, bigint_from_val, ratio_parts_val};
use egcl_rt::gc::{GcConfig, heap_stats, init_heap};

#[test]
fn ratio_numerator_survives_denominator_allocation() {
    const CHILD: &str = "EGCL_RATIO_NUMERATOR_GC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ratio_numerator_survives_denominator_allocation",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS_AT", "3")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_HEAP_SIZE")
            .env_remove("EGCL_HEAP_MAX")
            .env_remove("EGCL_NURSERY_SIZE")
            .env_remove("EGCL_TLAB_SIZE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ratio conversion failed: {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("forcing minor GC at allocation #3"),
            "{stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test result: ok. 1 passed; 0 failed"),
            "{stdout}"
        );
        assert!(
            stdout.contains("RATIO-NUMERATOR-RELOCATED"),
            "the child must demonstrate actual relocation"
        );
        return;
    }

    // No reader, symbols or CLI bootstrap: these are the only four Lisp heap
    // allocations: witness, numerator, denominator, ratio. The existing stress
    // hook collects immediately BEFORE allocation #3, inside BigRat::to_val.
    init_heap(&GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 1.0,
    })
    .unwrap();
    let numerator = BigInt::from_mag(1, vec![1, 1 << 16]);
    let denominator = BigInt::from_mag(1, vec![2, 1 << 16]);
    // Consecutive integers are coprime, so reduction keeps BOTH two-limb
    // components. All of this arithmetic uses Rust storage, not the Lisp heap.
    let ratio = BigRat::new(numerator.clone(), denominator.clone());
    assert_eq!(ratio.num.mag, numerator.mag);
    assert_eq!(ratio.den.mag, denominator.mag);
    let witness_number = BigInt::from_mag(1, vec![0xdead, 1 << 16]);
    egcl_rt::rooted!(witness = witness_number.to_val());
    let witness_before = witness.to_raw();
    assert_eq!(heap_stats().minor_gc_count, 0);
    // A two-limb bignum occupies 32 bytes including its header, already aligned
    // to the allocator's 16-byte alignment. Both allocations fit in this fresh
    // 256-byte TLAB, with no collection or intervening Lisp allocation. Keep
    // only a raw address: an extra numerator root would conceal the omission.
    // SAFETY: witness is a rooted bignum; only observe its address.
    let numerator_before = unsafe { witness.as_ptr() as usize } + 32;

    egcl_rt::rooted!(converted = ratio.to_val());
    assert_eq!(
        heap_stats().minor_gc_count,
        1,
        "only denominator allocation collects"
    );
    assert_ne!(
        witness.to_raw(),
        witness_before,
        "the nursery cohort must evacuate"
    );
    let (num, den) = ratio_parts_val(*converted).expect("both components remain a ratio");
    assert!(num.is_heap_object() && den.is_heap_object());
    // SAFETY: only strip the heap tag; do not dereference a possibly stale value.
    assert_ne!(
        unsafe { num.as_ptr() as usize },
        numerator_before,
        "the numerator must follow its relocation during denominator allocation"
    );
    let actual_num = bigint_from_val(num).expect("numerator remains a bignum");
    let actual_den = bigint_from_val(den).expect("denominator remains a bignum");
    assert_eq!(
        (actual_num.sign, actual_num.mag),
        (numerator.sign, numerator.mag)
    );
    assert_eq!(
        (actual_den.sign, actual_den.mag),
        (denominator.sign, denominator.mag)
    );
    let actual_witness = bigint_from_val(*witness).unwrap();
    assert_eq!(actual_witness.mag, witness_number.mag);
    println!("RATIO-NUMERATOR-RELOCATED");
}
