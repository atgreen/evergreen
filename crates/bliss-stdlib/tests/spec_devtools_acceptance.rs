use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::devtools::*;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

fn swank_secret_for_first_server() -> &'static str {
    "bliss-swank-0000000000000001"
}

fn connect_and_auth(port: u16, secret: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect swank");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    stream.write_all(secret.as_bytes()).expect("write secret");
    stream
}

fn read_ascii_response(stream: &mut TcpStream) -> String {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).expect("read swank response");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn swank_rex(op: &str, id: u64) -> String {
    let message = format!("(:emacs-rex {op} \"CL-USER\" :repl-thread {id})\n");
    format!("{:06x}{}", message.len(), message)
}

#[test]
fn debugger_public_hooks_support_stack_walk_eval_and_breakpoints() {
    // Per R6.12 and R6.18, public debugger APIs expose frames and eval-in-frame.
    // Per R6.14-R6.16, breakpoints are user-visible debugger hooks.
    let frames = walk_stack();
    assert!(!frames.is_empty(), "walk_stack should expose the current execution stack");

    let frame = frames.first().expect("first frame");
    assert!(frame.is_live(), "current stack frame should be live");
    assert_eq!(eval_in_frame(BlissVal::from_fixnum(7), frame).expect("eval in frame"), BlissVal::from_fixnum(7));

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
    assert!(instrumented.entries.iter().any(|entry| entry.call_count == Some(1)));

    start_allocation_profiler().expect("start allocation profiler");
    record_allocation(0x0E, 64, 0x2222);
    assert_eq!(stop_allocation_profiler().expect("stop allocation profiler"), T);
    let allocation = get_last_profiler_report().expect("allocation report");
    assert_eq!(allocation.kind, ProfilerKind::Allocation);
    assert!(allocation.entries.iter().any(|entry| entry.alloc_bytes.unwrap_or(0) >= 64));

    let (result, timing) = time_execution(|| Ok(BlissVal::from_fixnum(42)));
    assert_eq!(result.expect("timed result"), BlissVal::from_fixnum(42));
    assert!(timing.completed);
    assert!(timing.wall_clock_ns > 0);
}

#[test]
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
fn swank_server_enforces_authentication_and_serves_eval_completion_and_thread_queries() {
    // Per R6.33-R6.38, Bliss MUST expose a SWANK-compatible authenticated IDE endpoint.
    let port = free_port();
    start_swank_server(port, "127.0.0.1").expect("start swank server");
    thread::sleep(Duration::from_millis(150));

    let mut bad = connect_and_auth(port, "wrong-secret\n");
    let bad_reply = read_ascii_response(&mut bad);
    assert!(bad_reply.contains("authentication failed"), "reply was: {bad_reply}");

    let mut conn1 = connect_and_auth(port, swank_secret_for_first_server());
    let ok_reply = read_ascii_response(&mut conn1);
    assert!(ok_reply.contains("(:ok t)"), "reply was: {ok_reply}");

    let mut conn2 = connect_and_auth(port, swank_secret_for_first_server());
    let second_ok = read_ascii_response(&mut conn2);
    assert!(second_ok.contains("(:ok t)"), "reply was: {second_ok}");

    let eval = swank_rex("(swank:listener-eval \"(+ 1 2)\")", 7);
    conn1.write_all(eval.as_bytes()).expect("send swank eval");
    let eval_reply = read_ascii_response(&mut conn1);
    assert!(eval_reply.contains('3'), "eval reply was: {eval_reply}");

    let completions = swank_rex("(swank:simple-completions \"for\")", 8);
    conn1
        .write_all(completions.as_bytes())
        .expect("send completions");
    let completions_reply = read_ascii_response(&mut conn1);
    assert!(completions_reply.to_lowercase().contains("format"), "reply was: {completions_reply}");

    let threads = swank_rex("(swank:list-threads)", 9);
    conn2.write_all(threads.as_bytes()).expect("send thread query");
    let thread_reply = read_ascii_response(&mut conn2);
    assert!(thread_reply.to_lowercase().contains("running"), "reply was: {thread_reply}");

    stop_swank_server().expect("stop swank server");
}

#[test]
fn bundled_asdf_artifact_is_present_for_require_and_output_translation_integration() {
    // Per R6.45-R6.48, Bliss ships ASDF and an implementation-owned output cache integration.
    let asdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/asdf.lisp");
    let source = std::fs::read_to_string(&asdf).expect("read bundled asdf");
    assert!(source.contains("(provide \"asdf\")"), "bundled ASDF should provide the asdf module");
    assert!(source.to_lowercase().contains("asdf-output-translations"), "bundled ASDF should expose output translation support");
}
