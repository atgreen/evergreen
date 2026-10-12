// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::ir::ValueRepresentation;
use egcl_compiler::t2::x64_calls::NativeCallValues;

fn assert_raw_homes(code: &NativeCode) {
    assert!(code.is_t2);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("public loop must install mapped T2 code")
    };
    let maps = mapped.value_maps_for_test();
    assert!(
        maps.iter()
            .any(|site| match maps.value_map(site.return_offset) {
                Some(NativeCallValues::Frame(frame)) => frame
                    .values()
                    .iter()
                    .any(|value| value.repr == ValueRepresentation::UnboxedF64),
                Some(NativeCallValues::Deoptimizing { f64_slots, .. }) => !f64_slots.is_empty(),
                _ => false,
            }),
        "installed public loop must have actual raw F64 native homes"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn public_packed_f64_loop_keeps_raw_values_and_reloads_moved_arrays() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun packed-f64-collector ()
           (%ordinary-t2-entry-for-test)
           (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = ordinary_t2::install(
        "PUBLIC-PACKED-F64-LOOP",
        "(src dst n scale)",
        "((dotimes (i n)
            (let ((x (aref src i)))
              (packed-f64-collector)
              (setf (aref dst i) (+ (* x scale) x))))
          dst)",
        &mut env,
    );
    assert_raw_homes(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("public loop must install mapped T2 code")
    };
    assert!(
        code.osr_entries.is_empty(),
        "raw OSR import is a separate contract"
    );
    egcl_rt::collect_t0_minor().unwrap();
    // This minimal compiler environment does not load MAKE-ARRAY's boot Lisp.
    // Supply its runtime representation directly; the installed kernel still
    // uses public AREF, SETF and arithmetic throughout.
    egcl_rt::rooted!(src = egcl_rt::packed_f64::make(4, 1.25).unwrap());
    egcl_rt::rooted!(dst = egcl_rt::packed_f64::make(4, 0.0).unwrap());
    egcl_rt::rooted!(scale = egcl_rt::gc::alloc_double_float(2.0));
    let before = (src.to_raw(), dst.to_raw());
    egcl_rt::rooted!(args = vec![*src, *dst, EgclVal::from_fixnum(4), *scale]);
    ordinary_t2::expect_entry(&code);
    egcl_rt::rooted!(result = run_native(&code, symbol, &args, &mut env).unwrap());
    assert!(
        ordinary_t2::observed_entry(),
        "collecting helper must see the installed caller"
    );
    assert_eq!(*result, *dst);
    assert_ne!(src.to_raw(), before.0, "source must actually relocate");
    assert_ne!(dst.to_raw(), before.1, "destination must actually relocate");
    assert_eq!(
        mapped.deopt_count(),
        0,
        "valid packed input must stay native"
    );
    for index in 0..4 {
        assert_eq!(egcl_rt::packed_f64::get(*src, index), Some(1.25));
        assert_eq!(egcl_rt::packed_f64::get(*dst, index), Some(3.75));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn public_packed_f64_loop_reboxes_loaded_value_on_scalar_guard_failure() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = ordinary_t2::install(
        "PUBLIC-PACKED-F64-GUARD",
        "(src dst n scale)",
        "((dotimes (i n)
            (let ((x (aref src i)))
              (setf (aref dst i) (+ (* x scale) x))))
          dst)",
        &mut env,
    );
    assert_raw_homes(&code);
    egcl_rt::rooted!(src = egcl_rt::packed_f64::make(4, 1.25).unwrap());
    egcl_rt::rooted!(dst = egcl_rt::packed_f64::make(4, 0.0).unwrap());
    // Mixed double/fixnum arithmetic is valid Lisp. The native double-only
    // scalar guard must reconstruct X after the successful packed load.
    egcl_rt::rooted!(args = vec![*src, *dst, EgclVal::from_fixnum(4), EgclVal::from_fixnum(2)]);
    egcl_rt::rooted!(result = run_native(&code, symbol, &args, &mut env).unwrap());
    assert_eq!(*result, *dst);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert_eq!(
        mapped.deopt_count(),
        1,
        "scalar guard must enter precise deopt once"
    );
    for index in 0..4 {
        assert_eq!(egcl_rt::packed_f64::get(*dst, index), Some(3.75));
    }
}
