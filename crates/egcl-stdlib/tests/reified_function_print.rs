// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::value::NIL;

#[test]
fn reified_functions_print_as_functions_with_and_without_escape() {
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 16 * 1024 * 1024,
        heap_max: 64 * 1024 * 1024,
        nursery_size: 64 * 1024,
        tlab_size: 4096,
        region_size: 4096,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    egcl_rt::rooted!(function = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL));
    // Both directives must recognize the heap representation even without the
    // CLI's source-closure registry or PRINT-OBJECT hook.
    egcl_rt::rooted!(
        printed = egcl_stdlib::format::format(NIL, "~A ~S", &[*function, *function],).unwrap()
    );
    assert_eq!(printed.as_string(), "#<FUNCTION> #<FUNCTION>");
}
