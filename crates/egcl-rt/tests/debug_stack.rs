// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::FrameType;
use egcl_rt::debug_stack::{CallFrame, FrameOrigin, capture_current};
use egcl_rt::value::NIL;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn init_heap() {
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
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
}

#[test]
fn arguments_survive_relocation_in_live_calls_and_owned_snapshots() {
    let _serial = TEST_LOCK.lock().unwrap();
    init_heap();
    egcl_rt::thread::current_stack();
    let original = egcl_rt::gc::alloc_double_float(42.0);
    let call = CallFrame::enter_with_args("LEAF", &[original, NIL]);
    // The call record is the only root: no activation slot or local root can
    // rescue its argument if the execution-root scanner omits the record.
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(snapshot = capture_current(1));
    let moved = snapshot[0].arguments.as_ref().unwrap()[0];
    assert_ne!(original, moved, "the argument must actually relocate");
    assert_eq!(unsafe { moved.as_ptr().add(8).cast::<f64>().read() }, 42.0);
    assert_eq!(snapshot[0].arguments.as_ref().unwrap()[1], NIL);
    drop(call);
    // A minor collection promotes the first object. Use a fresh nursery
    // argument to independently require movement with only a historical root.
    let historical = egcl_rt::gc::alloc_double_float(43.0);
    let call = CallFrame::enter_with_args("RETURNED", &[historical]);
    *snapshot = capture_current(1);
    drop(call);
    egcl_rt::collect_t0_minor().unwrap();
    let moved_again = snapshot[0].arguments.as_ref().unwrap()[0];
    assert_ne!(
        historical, moved_again,
        "the retained snapshot must be traced"
    );
    assert_eq!(
        unsafe { moved_again.as_ptr().add(8).cast::<f64>().read() },
        43.0
    );
    assert!(capture_current(1).is_empty());

    let unavailable = CallFrame::enter("UNKNOWN");
    let no_args = CallFrame::enter_with_args("ZERO", &[]);
    let frames = capture_current(2);
    assert_eq!(frames[0].arguments, Some(Vec::new()));
    assert_eq!(frames[1].arguments, None);
    drop(no_args);
    drop(unavailable);
}

#[test]
fn snapshots_resolve_function_objects_and_survive_collection_and_stack_pop() {
    let _serial = TEST_LOCK.lock().unwrap();
    init_heap();
    let name = egcl_rt::symbols::make_uninterned("SNAPSHOT-FUNCTION");
    egcl_rt::rooted!(function = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, name));
    let stack = egcl_rt::thread::current_stack();
    stack.push_frame(*function, std::ptr::null(), 0, 0).unwrap();
    // Interpreted function objects themselves are pinned. Exercise relocation
    // with a nursery value in an activation slot. Frame headers contain pinned
    // function identities; the collector traces movable activation slots.
    // An unknown function identity remains explicitly unavailable.
    let movable = egcl_rt::gc::alloc_double_float(42.0);
    let frame = stack.push_frame(NIL, std::ptr::null(), 1, 0).unwrap();
    unsafe { egcl_rt::EgclStack::frame_slots_mut(frame)[0] = movable };
    let snapshot = capture_current(100);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].function, None);
    assert_eq!(snapshot[1].function.as_deref(), Some("SNAPSHOT-FUNCTION"));
    egcl_rt::collect_t0_minor().unwrap();
    let relocated = unsafe { egcl_rt::EgclStack::frame_slots_mut(frame)[0] };
    assert_ne!(
        relocated, movable,
        "the managed stack cell must actually move"
    );
    assert_eq!(
        unsafe { relocated.as_ptr().add(8).cast::<f64>().read() },
        42.0
    );
    assert_eq!(capture_current(100), snapshot);
    stack.pop_frame();
    stack.pop_frame();
    egcl_rt::collect_t0_minor().unwrap();
    assert_eq!(snapshot[1].function.as_deref(), Some("SNAPSHOT-FUNCTION"));
}

#[test]
fn logical_frames_merge_with_runtime_frames_and_outlive_unwinding() {
    let _serial = TEST_LOCK.lock().unwrap();
    assert!(capture_current(100).is_empty());
    let stack = egcl_rt::thread::current_stack();
    let outer = CallFrame::enter("OUTER");
    stack
        .push_frame(NIL, std::ptr::null(), 0, FrameType::Call as u32)
        .unwrap();
    let middle = CallFrame::enter("MIDDLE");
    // Control records are anchors, but are not Lisp function calls.
    stack
        .push_frame(NIL, std::ptr::null(), 0, FrameType::Catch as u32)
        .unwrap();
    let leaf = CallFrame::enter("LEAF");
    let snapshot = capture_current(100);
    assert_eq!(snapshot.len(), 4);
    assert_eq!(snapshot[0].function.as_deref(), Some("LEAF"));
    assert_eq!(snapshot[1].function.as_deref(), Some("MIDDLE"));
    assert_eq!(snapshot[2].function, None);
    assert_eq!(snapshot[2].origin, FrameOrigin::Managed);
    assert_eq!(snapshot[3].function.as_deref(), Some("OUTER"));
    assert_eq!(snapshot[3].origin, FrameOrigin::Interpreted);
    assert_eq!(capture_current(2), snapshot[..2]);
    assert!(capture_current(0).is_empty());
    drop(leaf);
    stack.pop_frame();
    drop(middle);
    stack.pop_frame();
    drop(outer);
    assert!(capture_current(100).is_empty());
    assert_eq!(snapshot[0].function.as_deref(), Some("LEAF"));
}

#[test]
fn managed_argument_records_merge_without_duplicate_calls() {
    let _serial = TEST_LOCK.lock().unwrap();
    let outer = CallFrame::enter_with_args("TREE-OUTER", &[egcl_rt::EgclVal::from_fixnum(41)]);
    let stack = egcl_rt::thread::current_stack();
    stack.push_frame(NIL, std::ptr::null(), 0, 0).unwrap();
    let managed = CallFrame::enter_managed("T0-MIDDLE", &[egcl_rt::EgclVal::from_fixnum(42)]);
    let inner = CallFrame::enter_with_args("TREE-INNER", &[]);
    let frames = capture_current(100);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].function.as_deref(), Some("TREE-INNER"));
    assert_eq!(frames[1].function.as_deref(), Some("T0-MIDDLE"));
    assert_eq!(frames[1].origin, FrameOrigin::Managed);
    assert_eq!(
        frames[1].arguments,
        Some(vec![egcl_rt::EgclVal::from_fixnum(42)])
    );
    assert_eq!(frames[2].function.as_deref(), Some("TREE-OUTER"));
    assert_eq!(capture_current(2), frames[..2]);
    drop(inner);
    drop(managed);
    stack.pop_frame();
    drop(outer);
    assert!(capture_current(100).is_empty());
}

#[test]
fn control_records_do_not_consume_the_call_limit() {
    let _serial = TEST_LOCK.lock().unwrap();
    let stack = egcl_rt::thread::current_stack();
    stack.push_frame(NIL, std::ptr::null(), 0, 0).unwrap();
    stack
        .push_frame(NIL, std::ptr::null(), 0, FrameType::Catch as u32)
        .unwrap();
    stack
        .push_frame(NIL, std::ptr::null(), 0, FrameType::Unwind as u32)
        .unwrap();
    let snapshot = capture_current(1);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].origin, FrameOrigin::Managed);
    stack.pop_frame();
    stack.pop_frame();
    stack.pop_frame();
}

#[cfg(egcl_fibers)]
mod fibers {
    use super::*;
    use egcl_rt::debug_stack::capture_fiber;
    use egcl_rt::thread::{self, FiberState};
    use egcl_rt::{EgclVal, SchedulerConfig, SchedulerGroup};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);

    fn parked_argument() -> EgclVal {
        let value = egcl_rt::gc::alloc_double_float(73.0);
        let call = CallFrame::enter_with_args("PARKED", &[value]);
        thread::park_current_fiber().unwrap();
        let frame = capture_current(1);
        assert_eq!(
            frame[0].arguments.as_ref().unwrap()[0].as_double_float(),
            73.0
        );
        drop(call);
        EgclVal::from_fixnum(73)
    }

    #[test]
    fn parked_fiber_arguments_are_relocated_before_resuming() {
        let _serial = TEST_LOCK.lock().unwrap();
        init_heap();
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
        let id = thread::make_fiber(super::native_entry::entry(parked_argument)).unwrap();
        group.submit(id).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let before = loop {
            egcl_rt::poll_safepoint();
            assert!(std::time::Instant::now() < deadline, "fiber did not park");
            if thread::fiber_state(id) == Some(FiberState::Blocked) {
                if let Some(frames) = capture_fiber(id, 1).unwrap() {
                    break frames[0].arguments.as_ref().unwrap()[0];
                }
            }
            std::thread::yield_now();
        };
        // No rooted snapshot exists yet. Only the parked fiber's call record
        // can preserve and relocate this argument.
        egcl_rt::collect_t0_minor().unwrap();
        egcl_rt::rooted!(frames = capture_fiber(id, 1).unwrap().unwrap());
        let after = frames[0].arguments.as_ref().unwrap()[0];
        assert_ne!(before, after, "the parked argument must actually move");
        assert_eq!(after.as_double_float(), 73.0);
        group.unpark(id).unwrap();
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(73)]);
        egcl_rt::collect_t0_minor().unwrap();
        assert_eq!(
            frames[0].arguments.as_ref().unwrap()[0].as_double_float(),
            73.0
        );
    }

    fn traced_fiber() -> EgclVal {
        let id = thread::current_fiber_id().unwrap();
        let name = format!("FIBER-{}", id.0);
        let frame = CallFrame::enter(&name);
        let stack = thread::current_stack();
        stack.push_frame(NIL, std::ptr::null(), 0, 0).unwrap();
        stack
            .push_frame(NIL, std::ptr::null(), 0, FrameType::Catch as u32)
            .unwrap();
        let leaf_name = format!("LEAF-{}", id.0);
        let leaf =
            CallFrame::enter_with_args(&leaf_name, &[EgclVal::from_fixnum(id.0 as i64), NIL]);
        let snapshot = capture_current(100);
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].function.as_deref(), Some(leaf_name.as_str()));
        assert_eq!(snapshot[1].origin, FrameOrigin::Managed);
        assert_eq!(snapshot[2].function.as_deref(), Some(name.as_str()));
        assert_eq!(
            capture_fiber(id, 1).unwrap(),
            None,
            "mounted stack cannot be inspected remotely"
        );
        let mut carrier = thread::current_thread_id();
        for _ in 0..200 {
            thread::fiber_yield().unwrap();
            let next = thread::current_thread_id();
            if next != carrier {
                MIGRATIONS.fetch_add(1, Ordering::Relaxed);
                carrier = next;
            }
            assert_eq!(capture_current(100), snapshot);
        }
        thread::park_current_fiber().unwrap();
        assert_eq!(capture_current(100), snapshot);
        drop(leaf);
        stack.pop_frame();
        stack.pop_frame();
        drop(frame);
        assert!(capture_current(100).is_empty());
        EgclVal::from_fixnum(42)
    }

    #[test]
    fn current_and_suspended_fibers_keep_their_own_frames_across_migration() {
        let _serial = TEST_LOCK.lock().unwrap();
        let caller = CallFrame::enter("NATIVE-CALLER");
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 4 }).unwrap();
        let mut ids = Vec::new();
        for _ in 0..32 {
            let entry = super::native_entry::entry(traced_fiber);
            let id = thread::make_fiber(entry).unwrap();
            assert_eq!(
                capture_fiber(id, 1).unwrap().unwrap()[0].origin,
                FrameOrigin::Entry
            );
            group.submit(id).unwrap();
            ids.push(id);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        for &id in &ids {
            let snapshot = loop {
                egcl_rt::poll_safepoint();
                assert!(std::time::Instant::now() < deadline, "fiber did not park");
                if thread::fiber_state(id) == Some(FiberState::Blocked) {
                    if let Some(snapshot) = capture_fiber(id, 100).unwrap() {
                        break snapshot;
                    }
                }
                std::thread::yield_now();
            };
            let name = format!("FIBER-{}", id.0);
            let leaf_name = format!("LEAF-{}", id.0);
            assert_eq!(snapshot.len(), 3);
            assert_eq!(snapshot[0].function.as_deref(), Some(leaf_name.as_str()));
            assert_eq!(
                snapshot[0].arguments,
                Some(vec![EgclVal::from_fixnum(id.0 as i64), NIL])
            );
            assert_eq!(snapshot[1].origin, FrameOrigin::Managed);
            assert_eq!(snapshot[2].function.as_deref(), Some(name.as_str()));
            assert_eq!(
                thread::fiber_backtrace(id, 100).unwrap().unwrap(),
                vec![leaf_name, "<anonymous function>".into(), name]
            );
        }
        assert!(
            MIGRATIONS.load(Ordering::Relaxed) > 0,
            "must exercise migration"
        );
        for id in ids {
            group.unpark(id).unwrap();
        }
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(42); 32]);
        assert_eq!(
            capture_current(100)[0].function.as_deref(),
            Some("NATIVE-CALLER")
        );
        drop(caller);
    }
}

#[cfg(egcl_fibers)]
#[path = "support/native_entry.rs"]
mod native_entry;
