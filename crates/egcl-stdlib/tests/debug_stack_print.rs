// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::EgclVal;
use egcl_rt::debug_stack::{FrameOrigin, LogicalFrame};
use egcl_rt::value::NIL;
use egcl_stdlib::devtools::format_logical_frame;

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static INIT: std::sync::Once = std::sync::Once::new();
    let guard = LOCK.lock().unwrap();
    INIT.call_once(|| {
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
    });
    guard
}

#[test]
fn calls_distinguish_actual_nil_zero_arguments_and_missing_metadata() {
    let _serial = test_guard();
    let mut frame = LogicalFrame {
        function: Some("LEAF".into()),
        origin: FrameOrigin::Interpreted,
        arguments: Some(vec![EgclVal::from_fixnum(42), NIL]),
    };
    egcl_rt::rooted_ref!(_frame = &mut frame);
    assert_eq!(format_logical_frame(&frame).unwrap(), "(LEAF 42 NIL)");
    frame.arguments = Some(Vec::new());
    assert_eq!(format_logical_frame(&frame).unwrap(), "(LEAF)");
    frame.arguments = None;
    assert_eq!(
        format_logical_frame(&frame).unwrap(),
        "(LEAF <arguments unavailable>)"
    );
    frame.function = None;
    assert_eq!(
        format_logical_frame(&frame).unwrap(),
        "(<anonymous function> <arguments unavailable>)"
    );
}

#[test]
fn printing_one_argument_can_relocate_later_arguments() {
    let _serial = test_guard();
    egcl_stdlib::clos::bootstrap_clos().unwrap();
    let name = egcl_rt::symbols::make_uninterned("GC-PRINTER");
    let class = EgclVal::from_fixnum(400);
    egcl_stdlib::clos::set_find_class(name, class).unwrap();
    egcl_rt::rooted!(instance = egcl_stdlib::clos::allocate_instance(class).unwrap());
    egcl_rt::collect_t0_minor().unwrap();
    let original = egcl_rt::gc::alloc_double_float(42.0);
    egcl_rt::rooted!(
        frame = LogicalFrame {
            function: Some("LEAF".into()),
            origin: FrameOrigin::Interpreted,
            arguments: Some(vec![*instance, original]),
        }
    );
    fn collecting_printer(_value: EgclVal, _escape: bool) -> Option<String> {
        egcl_rt::collect_t0_minor().unwrap();
        Some("#<GC-PRINTER>".into())
    }
    struct ResetHook;
    impl Drop for ResetHook {
        fn drop(&mut self) {
            egcl_stdlib::format::set_print_object_hook(None);
        }
    }
    egcl_stdlib::format::set_print_object_hook(Some(collecting_printer));
    let _reset = ResetHook;
    let text = format_logical_frame(&frame).unwrap();
    assert_ne!(
        frame.arguments.as_ref().unwrap()[1],
        original,
        "printing must actually relocate the later argument"
    );
    assert_eq!(
        text,
        format!(
            "(LEAF #<GC-PRINTER> {})",
            egcl_stdlib::format::double_float_to_string(42.0)
        )
    );
}
