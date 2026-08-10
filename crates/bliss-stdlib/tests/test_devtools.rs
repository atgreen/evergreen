//! Tests for bliss-stdlib devtools: ReplState, DebugFrame, breakpoints, profiler,
//! disassembler, trace, describe/inspect/room, and SWANK server.

use bliss_stdlib::devtools::*;
use bliss_rt::value::{BlissVal, NIL, T};

// ══════════════════════════════════════════════════════════════════
// ReplState
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn repl_state_new_panics_until_implemented() {
    let _state = ReplState::new();
}

/// Once implemented, `new()` should yield level 0 and a valid package.
/// We split the assertions to isolate the panic site.
#[test]
#[should_panic(expected = "not yet implemented")]
fn repl_state_initial_level_is_zero() {
    let state = ReplState::new();
    assert_eq!(state.level(), 0);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn repl_state_initial_package_is_valid() {
    let state = ReplState::new();
    // Package should not be NIL — the REPL starts in CL-USER or similar.
    let pkg = state.package();
    // We just check it's not the unbound/missing sentinel.
    assert_ne!(pkg, bliss_rt::value::UNBOUND);
}

// ══════════════════════════════════════════════════════════════════
// DebugFrame accessors
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn walk_stack_panics_until_implemented() {
    let _frames = walk_stack();
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn eval_in_frame_panics_until_implemented() {
    // We can't construct a DebugFrame directly (fields are private),
    // so we get one from walk_stack, which itself is unimplemented.
    let frames = walk_stack();
    if let Some(frame) = frames.first() {
        let _ = eval_in_frame(NIL, frame);
    }
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn debug_frame_function_panics_until_implemented() {
    let frames = walk_stack();
    if let Some(frame) = frames.first() {
        let _f = frame.function();
    }
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn debug_frame_source_location_panics_until_implemented() {
    let frames = walk_stack();
    if let Some(frame) = frames.first() {
        let _loc = frame.source_location();
    }
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn debug_frame_locals_panics_until_implemented() {
    let frames = walk_stack();
    if let Some(frame) = frames.first() {
        let _locals = frame.locals();
    }
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn debug_frame_is_live_panics_until_implemented() {
    let frames = walk_stack();
    if let Some(frame) = frames.first() {
        let _live = frame.is_live();
    }
}

// ══════════════════════════════════════════════════════════════════
// Breakpoints
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn break_on_entry_panics_until_implemented() {
    let _bp = break_on_entry(NIL, None);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn break_at_panics_until_implemented() {
    let _bp = break_at("foo.lisp", 42, None);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn remove_breakpoint_panics_until_implemented() {
    let _r = remove_breakpoint(BreakpointId(1));
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn list_breakpoints_panics_until_implemented() {
    let _bps = list_breakpoints();
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
    let c = a.clone();
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
// Profiler
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn start_profiler_panics_until_implemented() {
    let _ = start_profiler(100);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn stop_profiler_panics_until_implemented() {
    let _ = stop_profiler();
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn start_allocation_profiler_panics_until_implemented() {
    let _ = start_allocation_profiler();
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn stop_allocation_profiler_panics_until_implemented() {
    let _ = stop_allocation_profiler();
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn start_profiler_with_custom_rate() {
    let _ = start_profiler(1000);
}

// ══════════════════════════════════════════════════════════════════
// Disassembler
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn disassemble_no_tier_panics_until_implemented() {
    let _ = disassemble(NIL, None, NIL);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn disassemble_with_tier_panics_until_implemented() {
    let _ = disassemble(NIL, Some(T), NIL);
}

// ══════════════════════════════════════════════════════════════════
// Trace / Untrace
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn trace_function_panics_until_implemented() {
    let _ = trace_function(NIL, false, None);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn trace_function_with_break_and_condition() {
    let _ = trace_function(NIL, true, Some(T));
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn untrace_function_panics_until_implemented() {
    let _ = untrace_function(NIL);
}

// ══════════════════════════════════════════════════════════════════
// Describe / Inspect / Room
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn describe_panics_until_implemented() {
    let _ = describe(NIL, NIL);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn inspect_panics_until_implemented() {
    let _ = inspect(NIL);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn room_no_verbosity_panics_until_implemented() {
    let _ = room(None, NIL);
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn room_with_verbosity_panics_until_implemented() {
    let _ = room(Some(T), NIL);
}

// ══════════════════════════════════════════════════════════════════
// SWANK server
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn start_swank_server_default_panics_until_implemented() {
    let _ = start_swank_server(4005, "127.0.0.1");
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn stop_swank_server_panics_until_implemented() {
    let _ = stop_swank_server();
}

// ══════════════════════════════════════════════════════════════════
// repl_loop & invoke_debugger_ui
//
// NOTE: These tests currently panic inside ReplState::new() before
// reaching the target function. Once ReplState::new() is implemented,
// these tests will proceed to call repl_loop / invoke_debugger_ui
// and should then panic with their own "not yet implemented" message.
// At that point, update the expected panic message accordingly.
// ══════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "not yet implemented")]
fn repl_loop_panics_until_implemented() {
    // Phase 1: ReplState::new() panics here.
    // Phase 2 (after new() is implemented): repl_loop() should panic.
    let mut state = ReplState::new();
    // This line is only reached once ReplState::new() is implemented.
    let _ = repl_loop(&mut state);
    // If both are implemented, repl_loop should return Ok or Err, not silently pass.
    unreachable!("repl_loop should either panic or be tested for its return value");
}

#[test]
#[should_panic(expected = "not yet implemented")]
fn invoke_debugger_ui_panics_until_implemented() {
    // Phase 1: ReplState::new() panics here.
    // Phase 2 (after new() is implemented): invoke_debugger_ui() should panic.
    let mut state = ReplState::new();
    // This line is only reached once ReplState::new() is implemented.
    let _ = invoke_debugger_ui(NIL, &mut state);
    // If both are implemented, invoke_debugger_ui should return Ok or Err, not silently pass.
    unreachable!("invoke_debugger_ui should either panic or be tested for its return value");
}
