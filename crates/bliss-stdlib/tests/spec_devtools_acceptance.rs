use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::devtools::*;

use std::path::Path;
use std::thread;
use std::time::Duration;

#[test]
fn debugger_public_hooks_support_stack_walk_eval_and_breakpoints() {
    // Per R6.12 and R6.18, public debugger APIs expose frames and eval-in-frame.
    // Per R6.14-R6.16, breakpoints are user-visible debugger hooks.
    let frames = walk_stack();
    assert!(
        !frames.is_empty(),
        "walk_stack should expose the current execution stack"
    );

    let frame = frames.first().expect("first frame");
    assert!(frame.is_live(), "current stack frame should be live");
    assert_eq!(
        eval_in_frame(BlissVal::from_fixnum(7), frame).expect("eval in frame"),
        BlissVal::from_fixnum(7)
    );

    let entry_bp = break_on_entry(BlissVal::from_raw(0x6), Some(T)).expect("break-on-entry");
    let line_bp = break_at("spec/devtools.lisp", 12, Some(T)).expect("break-at");
    let ids = list_breakpoints();
    assert!(ids.contains(&entry_bp));
    assert!(ids.contains(&line_bp));
    remove_breakpoint(entry_bp).expect("remove entry breakpoint");
    remove_breakpoint(line_bp).expect("remove source breakpoint");
}

#[test]
fn profiler_and_timing_apis_return_structured_observable_results() {
    // Per R6.23-R6.26, profiler APIs must produce structured reports.
    // Per R6.44, TIME must expose timing and allocation metrics.
    start_profiler(1).expect("start profiler with out-of-range rate should still clamp");
    record_safepoint_pc(0x1234);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(stop_profiler().expect("stop profiler"), T);
    let sampling = get_last_profiler_report().expect("sampling report");
    assert_eq!(sampling.kind, ProfilerKind::Sampling);
    assert!(sampling.total_samples >= 1);

    start_instrumentation_profiler().expect("start instrumentation profiler");
    instrument_function_entry(0xAA);
    thread::sleep(Duration::from_millis(2));
    instrument_function_exit(0xAA);
    let instrumented = stop_instrumentation_profiler().expect("stop instrumentation profiler");
    assert_eq!(instrumented.kind, ProfilerKind::Instrumented);
    assert!(
        instrumented
            .entries
            .iter()
            .any(|entry| entry.call_count == Some(1))
    );

    start_allocation_profiler().expect("start allocation profiler");
    record_allocation(0x0E, 64, 0x2222);
    assert_eq!(
        stop_allocation_profiler().expect("stop allocation profiler"),
        T
    );
    let allocation = get_last_profiler_report().expect("allocation report");
    assert_eq!(allocation.kind, ProfilerKind::Allocation);
    assert!(
        allocation
            .entries
            .iter()
            .any(|entry| entry.alloc_bytes.unwrap_or(0) >= 64)
    );

    let (result, timing) = time_execution(|| Ok(BlissVal::from_fixnum(42)));
    assert_eq!(result.expect("timed result"), BlissVal::from_fixnum(42));
    assert!(timing.completed);
    assert!(timing.wall_clock_ns > 0);
}

#[test]
#[ignore = "stage 6: devtools/telemetry"]
fn trace_disassemble_describe_inspect_and_room_hooks_are_callable_end_to_end() {
    // Per R6.29-R6.32a, R6.39-R6.43, the public devtools hooks must be callable from user code.
    let traced = BlissVal::from_symbol_index(77);
    trace_function(traced, true, Some(T)).expect("trace function");
    assert!(has_trace_hook(traced));
    assert!(is_traced(traced));
    assert!(trace_entry(traced, &[BlissVal::from_fixnum(1)]));
    trace_exit(traced, BlissVal::from_fixnum(2));
    untrace_function(traced).expect("untrace function");
    assert!(!is_traced(traced));

    let fake_fn = BlissVal::from_raw(0x6);
    disassemble(fake_fn, Some(NIL), NIL).expect("disassemble interpreted function");
    describe(BlissVal::from_fixnum(9), NIL).expect("describe");
    inspect(BlissVal::from_fixnum(9)).expect("inspect");
    room(Some(T), NIL).expect("room");
}

#[test]
fn ide_protocol_uses_vendored_slynk_library_not_rust_swank_server() {
    // Per R6.33, Bliss must load upstream Slynk/SWANK as Lisp code and must not
    // implement the wire protocol in the Rust runtime/stdlib.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let slynk = repo_root.join("lib/slynk");
    assert!(slynk.join("slynk.lisp").is_file());
    assert!(slynk.join("slynk-rpc.lisp").is_file());
    assert!(slynk.join("backend/bliss.lisp").is_file());
    assert!(slynk.join("start-slynk.lisp").is_file());

    let start_slynk = std::fs::read_to_string(slynk.join("start-slynk.lisp"))
        .expect("read start-slynk.lisp");
    assert!(start_slynk.contains("slynk:create-server"));

    let devtools = std::fs::read_to_string(repo_root.join("crates/bliss-stdlib/src/devtools.rs"))
        .expect("read devtools.rs");
    let lib_rs = std::fs::read_to_string(repo_root.join("crates/bliss-stdlib/src/lib.rs"))
        .expect("read lib.rs");
    let builtin_server = ["start", "_swank", "_server"].concat();
    let dispatcher = ["dispatch", "_swank", "_message"].concat();
    let rex_marker = ["(:emacs", "-rex"].concat();
    assert!(!devtools.contains(&builtin_server));
    assert!(!devtools.contains(&dispatcher));
    assert!(!devtools.contains(&rex_marker));
    assert!(!lib_rs.contains(&builtin_server));
}

#[test]
#[ignore = "stage 6: ASDF self-host"]
fn bundled_asdf_require_path_is_observable_through_the_real_cli() {
    // Per R6.45-R6.48, the acceptance gate must drive REQUIRE/ASDF behavior
    // through the real CLI rather than grepping the bundled source file.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("cargo")
        .args([
            "run",
            "-q",
            "-p",
            "bliss",
            "--",
            "--eval",
            "(require :asdf)\n(print bliss-ext:*asdf-output-translations*)\n(print asdf:*last-operation-tier*)",
        ])
        .current_dir(&repo_root)
        .output()
        .expect("run real bliss require path");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains(".CACHE/BLISS/ASDF"), "stdout: {stdout}");
    assert!(stdout.contains("T1"), "stdout: {stdout}");
}
