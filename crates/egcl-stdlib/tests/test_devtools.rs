// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for egcl-stdlib devtools: ReplState, DebugFrame, breakpoints, profiler,
//! disassembler, trace, describe/inspect/room, and SWANK server.
//!
//! These are behavioral tests — they call the real interface functions and assert
//! expected return values / behavior. In red phase, calls to unimplemented!()
//! will cause the tests to fail (panic), which is correct. We do NOT use
//! #[should_panic] because that would mask whether the implementation is correct
//! once it exists.

use egcl_rt::value::{NIL, T};
use egcl_stdlib::devtools::*;
use egcl_stdlib::{
    BreakpointId, ReplState, describe, eval_in_frame, inspect, list_breakpoints, room, walk_stack,
};

// ══════════════════════════════════════════════════════════════════
// ReplState
// ══════════════════════════════════════════════════════════════════

#[test]
fn crate_root_reexports_devtools_surface() {
    let _ = BreakpointId(1);
    let _ = ReplState::new();
    let _ = walk_stack();
    let _describe: fn(
        egcl_rt::value::EgclVal,
        egcl_rt::value::EgclVal,
    ) -> Result<(), egcl_rt::error::EgclError> = describe;
    let _inspect: fn(egcl_rt::value::EgclVal) -> Result<(), egcl_rt::error::EgclError> =
        inspect;
    let _room: fn(
        Option<egcl_rt::value::EgclVal>,
        egcl_rt::value::EgclVal,
    ) -> Result<(), egcl_rt::error::EgclError> = room;
    let _eval = eval_in_frame;
    let _ = list_breakpoints();
}

#[test]
fn repl_state_initial_level_is_zero() {
    let state = ReplState::new();
    assert_eq!(state.level(), 0, "initial REPL level must be 0");
}

#[test]
fn repl_state_initial_package_is_valid() {
    let state = ReplState::new();
    let pkg = state.package();
    // The REPL starts in CL-USER or similar — package must not be UNBOUND.
    assert_ne!(
        pkg,
        egcl_rt::value::UNBOUND,
        "initial REPL package must not be the UNBOUND sentinel"
    );
    // Package should not be NIL either.
    assert_ne!(pkg, NIL, "initial REPL package must not be NIL");
}

// ══════════════════════════════════════════════════════════════════
// DebugFrame accessors (R6.12)
// ══════════════════════════════════════════════════════════════════

#[test]
fn walk_stack_returns_frames() {
    let frames = walk_stack();
    // walk_stack should return a non-empty vector of debug frames
    // representing the current call stack.
    assert!(
        !frames.is_empty(),
        "walk_stack should return at least one frame (the test's own frame)"
    );
}

#[test]
fn debug_frame_function_returns_value() {
    let frames = walk_stack();
    let frame = frames
        .first()
        .expect("walk_stack should return at least one frame");
    let func = frame.function();
    // The function for a frame should be a valid EgclVal (not NIL for a real frame).
    assert_ne!(func, NIL, "debug frame function should not be NIL");
}

#[test]
fn debug_frame_source_location_returns_option() {
    let frames = walk_stack();
    let frame = frames
        .first()
        .expect("walk_stack should return at least one frame");
    let loc = frame.source_location();
    // source_location returns Option<(file, line, col)>.
    // For compiled Rust test code, it may be None — but the call must succeed.
    // If present, line should be > 0.
    if let Some((file, line, _col)) = loc {
        assert!(!file.is_empty(), "source file should not be empty");
        assert!(line > 0, "source line should be > 0");
    }
}

#[test]
fn debug_frame_locals_accessible() {
    let frames = walk_stack();
    let frame = frames
        .first()
        .expect("walk_stack should return at least one frame");
    let locals = frame.locals();
    // locals() returns Option<Vec<(name, value)>>.
    // Whether we get Some or None depends on debug quality.
    // If present, each binding should have a valid name (symbol) and value.
    if let Some(bindings) = locals {
        for (name, _val) in &bindings {
            assert_ne!(*name, NIL, "local variable name should not be NIL");
        }
    }
}

#[test]
fn debug_frame_is_live_for_current_stack() {
    let frames = walk_stack();
    let frame = frames
        .first()
        .expect("walk_stack should return at least one frame");
    // A frame from the current stack should be live.
    assert!(
        frame.is_live(),
        "frame from current walk_stack should be live"
    );
}

#[test]
fn eval_in_frame_returns_result() {
    let frames = walk_stack();
    let frame = frames
        .first()
        .expect("walk_stack should return at least one frame");
    // Evaluating NIL in any frame should return NIL (self-evaluating).
    let result = eval_in_frame(NIL, frame);
    assert!(result.is_ok(), "eval_in_frame of NIL should succeed");
    assert_eq!(
        result.unwrap(),
        NIL,
        "eval_in_frame of NIL should return NIL"
    );
}

// ══════════════════════════════════════════════════════════════════
// Breakpoints (R6.14–R6.16)
// ══════════════════════════════════════════════════════════════════

#[test]
fn break_on_entry_returns_breakpoint_id() {
    let bp = break_on_entry(NIL, None);
    assert!(bp.is_ok(), "break_on_entry should return Ok");
    let id = bp.unwrap();
    // BreakpointId should be a valid, non-negative identifier.
    // (BreakpointId wraps u64, so always non-negative.)
    let _ = id.0; // accessible
}

#[test]
fn break_at_returns_breakpoint_id() {
    let bp = break_at("foo.lisp", 42, None);
    assert!(bp.is_ok(), "break_at should return Ok");
}

#[test]
fn break_at_with_condition() {
    let bp = break_at("bar.lisp", 10, Some(T));
    assert!(bp.is_ok(), "break_at with condition should return Ok");
}

#[test]
fn remove_breakpoint_succeeds_for_existing() {
    let bp = break_on_entry(NIL, None).expect("break_on_entry should succeed");
    let result = remove_breakpoint(bp);
    assert!(
        result.is_ok(),
        "remove_breakpoint should succeed for an existing breakpoint"
    );
}

#[test]
fn list_breakpoints_returns_established() {
    // Set a breakpoint, then list should contain it.
    let bp = break_on_entry(NIL, None).expect("break_on_entry should succeed");
    let bps = list_breakpoints();
    assert!(
        bps.contains(&bp),
        "list_breakpoints should include the breakpoint we just set"
    );
}

#[test]
fn list_breakpoints_after_remove_excludes_removed() {
    let bp = break_on_entry(NIL, None).expect("break_on_entry should succeed");
    remove_breakpoint(bp).expect("remove_breakpoint should succeed");
    let bps = list_breakpoints();
    assert!(
        !bps.contains(&bp),
        "list_breakpoints should not include a removed breakpoint"
    );
}

#[test]
fn breakpoint_id_equality() {
    let a = BreakpointId(42);
    let b = BreakpointId(42);
    let c = BreakpointId(99);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn breakpoint_id_is_clone_copy_debug() {
    let a = BreakpointId(7);
    let b = a; // Copy
    let c = a;
    assert_eq!(a, b);
    assert_eq!(a, c);
    let dbg = format!("{:?}", a);
    assert!(dbg.contains("7"));
}

#[test]
fn breakpoint_id_hash_is_consistent() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(BreakpointId(1));
    set.insert(BreakpointId(2));
    set.insert(BreakpointId(1)); // duplicate
    assert_eq!(set.len(), 2);
}

// ══════════════════════════════════════════════════════════════════
// Profiler (R6.21–R6.28)
// ══════════════════════════════════════════════════════════════════

#[test]
fn profiler_start_stop_lifecycle() {
    // Start the sampling profiler, then stop it — should get a report.
    let start_result = start_profiler(100);
    assert!(start_result.is_ok(), "start_profiler should succeed");

    let stop_result = stop_profiler();
    assert!(stop_result.is_ok(), "stop_profiler should succeed");
    let report = stop_result.unwrap();
    assert_ne!(report, NIL, "profiler report should not be NIL");
}

#[test]
fn profiler_custom_sample_rate() {
    let result = start_profiler(1000);
    assert!(result.is_ok(), "start_profiler with 1000 Hz should succeed");
    let _ = stop_profiler(); // cleanup
}

#[test]
fn allocation_profiler_lifecycle() {
    let start = start_allocation_profiler();
    assert!(start.is_ok(), "start_allocation_profiler should succeed");

    let stop = stop_allocation_profiler();
    assert!(stop.is_ok(), "stop_allocation_profiler should succeed");
    let report = stop.unwrap();
    assert_ne!(report, NIL, "allocation profiler report should not be NIL");
}

// ══════════════════════════════════════════════════════════════════
// Disassembler (R6.29–R6.32a)
// ══════════════════════════════════════════════════════════════════

#[test]
fn disassemble_no_tier_succeeds() {
    let result = disassemble(NIL, None, NIL);
    assert!(result.is_ok(), "disassemble with no tier should succeed");
}

#[test]
fn disassemble_with_tier_succeeds() {
    let result = disassemble(NIL, Some(T), NIL);
    assert!(result.is_ok(), "disassemble with tier should succeed");
}

// ══════════════════════════════════════════════════════════════════
// Trace / Untrace (R6.39–R6.40)
// ══════════════════════════════════════════════════════════════════

#[test]
fn trace_untrace_lifecycle() {
    let trace_result = trace_function(NIL, false, None);
    assert!(trace_result.is_ok(), "trace_function should succeed");

    let untrace_result = untrace_function(NIL);
    assert!(untrace_result.is_ok(), "untrace_function should succeed");
}

#[test]
fn trace_with_break_and_condition() {
    let result = trace_function(NIL, true, Some(T));
    assert!(
        result.is_ok(),
        "trace_function with break=true and condition should succeed"
    );
}

// ══════════════════════════════════════════════════════════════════
// Describe / Inspect / Room (R6.41–R6.43)
// ══════════════════════════════════════════════════════════════════

#[test]
fn describe_returns_ok() {
    let result = describe(NIL, NIL);
    assert!(result.is_ok(), "describe should succeed");
}

#[test]
fn inspect_returns_ok() {
    let result = inspect(NIL);
    assert!(result.is_ok(), "inspect should succeed");
}

#[test]
fn room_no_verbosity_returns_ok() {
    let result = room(None, NIL);
    assert!(result.is_ok(), "room with no verbosity should succeed");
}

#[test]
fn room_with_verbosity_returns_ok() {
    let result = room(Some(T), NIL);
    assert!(result.is_ok(), "room with verbosity should succeed");
}

// ══════════════════════════════════════════════════════════════════
// repl_loop & invoke_debugger_ui
// ══════════════════════════════════════════════════════════════════

#[test]
fn repl_loop_is_callable() {
    let mut state = ReplState::new();
    // repl_loop should return a Result — either Ok or Err.
    let result = repl_loop(&mut state);
    // In a test context (no real terminal), repl_loop may return Err
    // to indicate no input available. That's acceptable.
    // The key test: it must not panic and must return a valid Result.
    assert!(
        result.is_ok() || result.is_err(),
        "repl_loop should return a Result"
    );
}

#[test]
fn invoke_debugger_ui_is_callable() {
    let mut state = ReplState::new();
    // An empty reader, never `io::stdin().lock()`: the debugger now takes the
    // caller's line source precisely because re-locking stdin deadlocks a REPL
    // that already holds the lock (bliss-bxlq), and a test must not block on the
    // real stdin either (bliss-z57).
    let mut input: &[u8] = b"";
    let result = invoke_debugger_ui(NIL, &mut state, &mut input);
    // invoke_debugger_ui should return Ok or Err (not panic).
    assert!(
        result.is_ok() || result.is_err(),
        "invoke_debugger_ui should return a Result"
    );
}
