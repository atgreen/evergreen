use std::io::{BufRead, Read, Write};
use std::process::{Command, Stdio};
use torcl_rt::value::{EOF, NIL};
use torcl_stdlib::streams::*;

#[test]
fn pipe_peer() {
    if std::env::var_os("TORCL_PIPE_PEER").is_none() {
        return;
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    if std::env::var("TORCL_PIPE_PEER").as_deref() == Ok("exit") {
        std::process::exit(0);
    }
    if std::env::var("TORCL_PIPE_PEER").as_deref() == Ok("binary") {
        std::io::stdout().write_all(&[0, 128, 255]).unwrap();
        std::io::stdout().flush().unwrap();
        let mut bytes = [0; 3];
        std::io::stdin().read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [255, 128, 0]);
        assert_eq!(std::io::stdin().read(&mut bytes).unwrap(), 0);
        std::process::exit(0);
    }
    if std::env::var("TORCL_PIPE_PEER").as_deref() == Ok("pressure") {
        println!("{}", "x".repeat(262144));
        std::io::stdout().flush().unwrap();
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        println!("reply:{line}");
        std::io::stdout().flush().unwrap();
    }
    eprint!("finished");
    std::io::stderr().flush().unwrap();
    std::process::exit(0);
}

#[test]
fn child_pipes_exchange_before_exit_and_close_stdin_delivers_eof() {
    torcl_rt::thread::current_thread_id();
    torcl_rt::gc::ensure_heap_initialized();
    install_gc_hooks();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pipe_peer", "--nocapture"])
        .env("TORCL_PIPE_PEER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    torcl_rt::rooted!(
        input = process_stdout_stream(child.stdout.take().unwrap(), StreamElementType::Character)
    );
    torcl_rt::rooted!(
        output = process_stdin_stream(child.stdin.take().unwrap(), StreamElementType::Character)
    );
    torcl_rt::rooted!(
        errors = process_stderr_stream(child.stderr.take().unwrap(), StreamElementType::Character)
    );
    loop {
        let (line, _) = stream_read_line(*input).unwrap();
        if string_text(line) == "READY" {
            break;
        }
        assert_ne!(line, EOF);
    }
    assert!(child.try_wait().unwrap().is_none());
    assert!(!stream_wait_for_input(*input, Some(0)).unwrap());
    assert!(!stream_listen(*input).unwrap());
    assert_eq!(stream_read_char_no_hang(*input).unwrap(), NIL);
    for stream in [*input, *output, *errors] {
        assert_eq!(file_position(stream).unwrap(), NIL);
        assert_eq!(
            set_file_position(stream, torcl_rt::TorclVal::from_fixnum(0)).unwrap(),
            NIL
        );
        assert_eq!(set_file_position_to_end(stream).unwrap(), NIL);
        assert_eq!(file_length_fn(stream).unwrap(), NIL);
    }
    for text in ["héllo\n", "second\n"] {
        let expected = format!("reply:{}", text.trim_end());
        let text = make_lisp_string(text);
        stream_write_string(*output, text, 0, None).unwrap();
        stream_finish_output(*output).unwrap();
        assert!(stream_wait_for_input(*input, Some(5000)).unwrap());
        let (line, _) = stream_read_line(*input).unwrap();
        assert_eq!(string_text(line), expected);
    }
    close(*output, false).unwrap();
    assert!(stream_wait_for_input(*input, Some(5000)).unwrap());
    assert_eq!(stream_read_char(*input).unwrap(), EOF);
    assert!(!stream_listen(*input).unwrap());
    let (line, missing_newline) = stream_read_line(*errors).unwrap();
    assert_eq!(string_text(line), "finished");
    assert!(missing_newline);
    assert!(child.wait().unwrap().success());
    close(*input, false).unwrap();
    close(*errors, false).unwrap();
}

fn string_text(value: torcl_rt::TorclVal) -> String {
    assert!(value.is_heap_object());
    unsafe { torcl_rt::object::read_simple_string(value.as_ptr()) }
}

#[cfg(target_arch = "x86_64")]
mod scheduling {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;
    use torcl_rt::thread::{FiberState, fiber_state, make_fiber};
    use torcl_rt::{SchedulerConfig, SchedulerGroup, TorclVal};

    static PIN_CLOSE: AtomicBool = AtomicBool::new(false);
    static PRESSURE: AtomicBool = AtomicBool::new(false);
    static WAIT_ONLY: AtomicBool = AtomicBool::new(false);
    static INPUT: AtomicU64 = AtomicU64::new(0);
    static OUTPUT: AtomicU64 = AtomicU64::new(0);

    fn reader() -> TorclVal {
        torcl_rt::rooted!(input = TorclVal::from_raw(INPUT.load(Ordering::Acquire)));
        if WAIT_ONLY.load(Ordering::Acquire) {
            assert!(stream_wait_for_input(*input, None).is_err());
        } else {
            if PRESSURE.load(Ordering::Acquire) {
                let (line, _) = stream_read_line(*input).unwrap();
                assert_eq!(string_text(line), "x".repeat(262144));
            }
            let (line, _) = stream_read_line(*input).unwrap();
            assert_eq!(
                string_text(line),
                if PRESSURE.load(Ordering::Acquire) {
                    format!("reply:{}", "y".repeat(262144))
                } else {
                    "reply:cooperative".into()
                }
            );
        }
        NIL
    }

    fn writer() -> TorclVal {
        if WAIT_ONLY.load(Ordering::Acquire) {
            let input = TorclVal::from_raw(INPUT.load(Ordering::Acquire));
            close(input, true).unwrap();
            return NIL;
        }
        torcl_rt::rooted!(output = TorclVal::from_raw(OUTPUT.load(Ordering::Acquire)));
        let text = make_lisp_string(&if PRESSURE.load(Ordering::Acquire) {
            format!("{}\n", "y".repeat(262144))
        } else {
            "cooperative\n".into()
        });
        stream_write_string(*output, text, 0, None).unwrap();
        stream_finish_output(*output).unwrap();
        NIL
    }

    fn pinned_closer() -> TorclVal {
        use torcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
        torcl_rt::rooted!(output = TorclVal::from_raw(OUTPUT.load(Ordering::Acquire)));
        let text = make_lisp_string("cooperative\n");
        stream_write_string(*output, text, 0, None).unwrap();
        set_pinned_blocking_action(PinnedBlockingAction::Error);
        let fiber = torcl_rt::thread::current_fiber().unwrap();
        fiber.pin();
        let result = close(*output, false);
        fiber.unpin().unwrap();
        assert!(
            result.is_err(),
            "pinned policy rejection must reach CLOSE's caller"
        );
        assert!(open_stream_p(*output));
        close(*output, false).unwrap();
        NIL
    }

    #[test]
    fn pinned_close_keeps_output_for_retry_after_unpinning() {
        if std::env::var_os("TORCL_PIPE_SCHEDULER_CHILD").is_some() {
            PIN_CLOSE.store(true, Ordering::Release);
        }
        run_case(
            "scheduling::pinned_close_keeps_output_for_retry_after_unpinning",
            false,
            false,
        );
    }

    fn entry(f: fn() -> TorclVal) -> TorclVal {
        unsafe { TorclVal::from_function_ptr(f as *const () as *mut u8) }
    }

    #[test]
    fn blocked_pipe_read_releases_the_only_carrier() {
        run_case(
            "scheduling::blocked_pipe_read_releases_the_only_carrier",
            false,
            false,
        );
    }

    #[test]
    fn closing_pipe_cancels_indefinite_readiness() {
        run_case(
            "scheduling::closing_pipe_cancels_indefinite_readiness",
            true,
            false,
        );
    }

    #[test]
    fn backpressured_pipe_write_releases_the_only_carrier() {
        run_case(
            "scheduling::backpressured_pipe_write_releases_the_only_carrier",
            false,
            true,
        );
    }

    fn run_case(name: &str, wait_only: bool, pressure: bool) {
        const CHILD: &str = "TORCL_PIPE_SCHEDULER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
            eprintln!("pipe read blocked its carrier");
            std::process::exit(124);
        });
        torcl_rt::thread::current_thread_id();
        torcl_rt::gc::ensure_heap_initialized();
        install_gc_hooks();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pipe_peer", "--nocapture"])
            .env("TORCL_PIPE_PEER", if pressure { "pressure" } else { "1" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        torcl_rt::rooted!(
            input =
                process_stdout_stream(child.stdout.take().unwrap(), StreamElementType::Character)
        );
        torcl_rt::rooted!(
            output =
                process_stdin_stream(child.stdin.take().unwrap(), StreamElementType::Character)
        );
        loop {
            let (line, _) = stream_read_line(*input).unwrap();
            if string_text(line) == "READY" {
                break;
            }
        }
        PRESSURE.store(pressure, Ordering::Release);
        WAIT_ONLY.store(wait_only, Ordering::Release);
        INPUT.store(input.to_raw(), Ordering::Release);
        OUTPUT.store(output.to_raw(), Ordering::Release);
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
        if PIN_CLOSE.load(Ordering::Acquire) {
            group
                .submit(make_fiber(entry(pinned_closer)).unwrap())
                .unwrap();
            group.finish().unwrap();
            let (line, _) = stream_read_line(*input).unwrap();
            assert_eq!(string_text(line), "reply:cooperative");
            assert_eq!(stream_read_char(*input).unwrap(), EOF);
            assert!(child.wait().unwrap().success());
            close(*input, false).unwrap();
            return;
        }

        let waiting = make_fiber(entry(if pressure { writer } else { reader })).unwrap();
        group.submit(waiting).unwrap();
        while fiber_state(waiting) != Some(FiberState::Blocked) {
            torcl_rt::poll_safepoint();
            std::thread::yield_now();
        }
        // Moving GC must be able to run while the pipe operation is parked.
        torcl_rt::collect_t0_minor().unwrap();
        INPUT.store(input.to_raw(), Ordering::Release);
        OUTPUT.store(output.to_raw(), Ordering::Release);
        group
            .submit(make_fiber(entry(if pressure { reader } else { writer })).unwrap())
            .unwrap();
        group.finish().unwrap();
        close(*output, false).unwrap();
        assert!(child.wait().unwrap().success());
        close(*input, false).unwrap();
    }
}

#[test]
fn native_pipe_read_allows_collection_before_peer_reply() {
    const CHILD: &str = "TORCL_NATIVE_PIPE_GC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_pipe_read_allows_collection_before_peer_reply",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        eprintln!("native pipe read prevented collection");
        std::process::exit(124);
    });
    torcl_rt::thread::current_thread_id();
    torcl_rt::gc::ensure_heap_initialized();
    install_gc_hooks();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pipe_peer", "--nocapture"])
        .env("TORCL_PIPE_PEER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    torcl_rt::rooted!(
        input = process_stdout_stream(child.stdout.take().unwrap(), StreamElementType::Character)
    );
    loop {
        let (line, _) = stream_read_line(*input).unwrap();
        if string_text(line) == "READY" {
            break;
        }
    }
    torcl_rt::rooted!(kept = torcl_rt::gc::alloc_double_float(41.0));
    let original = kept.to_raw();
    let mut stdin = child.stdin.take().unwrap();
    let collector = std::thread::spawn(move || {
        torcl_rt::collect_t0_minor().unwrap();
        stdin.write_all(b"collected\n").unwrap();
        // Drop stdin: the child exits after sending its reply.
    });
    let (line, _) = stream_read_line(*input).unwrap();
    assert_eq!(string_text(line), "reply:collected");
    assert_ne!(
        kept.to_raw(),
        original,
        "collection must move the caller's root"
    );
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    collector.join().unwrap();
    assert!(child.wait().unwrap().success());
    close(*input, false).unwrap();
}

#[test]
fn binary_child_pipes_preserve_all_octets() {
    install_gc_hooks();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pipe_peer", "--nocapture"])
        .env("TORCL_PIPE_PEER", "binary")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    torcl_rt::rooted!(
        input = process_stdout_stream(
            child.stdout.take().unwrap(),
            StreamElementType::UnsignedByte8
        )
    );
    torcl_rt::rooted!(
        output = process_stdin_stream(
            child.stdin.take().unwrap(),
            StreamElementType::UnsignedByte8
        )
    );
    let mut prefix = Vec::new();
    while !prefix.ends_with(b"READY\n") {
        let byte = stream_read_byte(*input).unwrap();
        assert_ne!(byte, EOF);
        prefix.push(byte.as_fixnum() as u8);
    }
    for expected in [0, 128, 255] {
        assert_eq!(stream_read_byte(*input).unwrap().as_fixnum(), expected);
    }
    for byte in [255, 128, 0] {
        stream_write_byte(*output, torcl_rt::TorclVal::from_fixnum(byte)).unwrap();
    }
    // CLOSE must flush these pending bytes before sending EOF.
    close(*output, false).unwrap();
    assert_eq!(stream_read_byte(*input).unwrap(), EOF);
    assert!(child.wait().unwrap().success());
    close(*input, false).unwrap();
}

#[test]
fn abandoned_pipe_is_closed_without_flushing_during_gc() {
    const CHILD: &str = "TORCL_PIPE_FINALIZER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "abandoned_pipe_is_closed_without_flushing_during_gc",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        eprintln!("pipe finalizer blocked or failed to close stdin");
        std::process::exit(124);
    });
    torcl_rt::thread::current_thread_id();
    torcl_rt::gc::ensure_heap_initialized();
    install_gc_hooks();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pipe_peer", "--nocapture"])
        .env("TORCL_PIPE_PEER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    torcl_rt::rooted!(
        input = process_stdout_stream(child.stdout.take().unwrap(), StreamElementType::Character)
    );
    loop {
        let (line, _) = stream_read_line(*input).unwrap();
        if string_text(line) == "READY" {
            break;
        }
    }
    {
        torcl_rt::rooted!(
            output =
                process_stdin_stream(child.stdin.take().unwrap(), StreamElementType::Character)
        );
        let text = make_lisp_string("discard this buffer");
        stream_write_string(*output, text, 0, None).unwrap();
    }
    torcl_rt::full_gc().unwrap();
    torcl_rt::full_gc().unwrap();
    assert!(stream_wait_for_input(*input, Some(5000)).unwrap());
    assert_eq!(stream_read_char(*input).unwrap(), EOF);
    assert!(child.wait().unwrap().success());
    close(*input, false).unwrap();
}

#[test]
fn close_reports_broken_pipe_and_abort_releases_it() {
    install_gc_hooks();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pipe_peer", "--nocapture"])
        .env("TORCL_PIPE_PEER", "exit")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    assert!(child.wait().unwrap().success());
    torcl_rt::rooted!(output = process_stdin_stream(stdin, StreamElementType::Character));
    let text = make_lisp_string("pending output");
    stream_write_string(*output, text, 0, None).unwrap();
    assert!(matches!(
        close(*output, false),
        Err(torcl_rt::TorclError::StreamError(_))
    ));
    assert!(
        open_stream_p(*output),
        "failed flush must leave explicit abort possible"
    );
    assert!(
        stream_finish_output(*output).is_err(),
        "failed bytes must remain pending"
    );
    close(*output, true).unwrap();
    assert!(!open_stream_p(*output));
    close(*output, false).unwrap();
}
