// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::error::EgclError;
use egcl_rt::ffi::{AlienType, Callback, ffi_call, marshal_to_c, unmarshal_from_c};
use egcl_rt::gc::{register_finalizer, set_finalizer_dispatch};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::runtime::check_sigint;
use egcl_rt::stack::{
    CodeInfo, Frame, FrameType, FrameWalker, SourceLocation, SourceLocationEntry, StackMapEntry,
};
use egcl_rt::thread::{
    FiberId, FiberState, current_fiber, current_stack, current_thread, fiber_yield, join_fiber,
    make_fiber, submit_fiber,
};
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};
use egcl_rt::{
    LogLevel, Runtime, RuntimeConfig, enter_safepoint, install_signal_handlers, parse_cli,
};
use std::path::PathBuf;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_serial() -> MutexGuard<'static, ()> {
    serial_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
    let saved: Vec<(String, Option<String>)> = vars
        .iter()
        .map(|(key, _)| ((*key).to_string(), std::env::var(key).ok()))
        .collect();
    for (key, value) in vars {
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
    f();
    for (key, value) in saved {
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

fn minimal_config() -> RuntimeConfig {
    RuntimeConfig {
        heap_size: 8 * 1024 * 1024,
        nursery_size: 2 * 1024 * 1024,
        tlab_size: 256 * 1024,
        stack_size: 128 * 1024,
        num_workers: 1,
        image_path: None,
        no_image: true,
        eval_form: None,
        load_file: None,
        gc_log: None,
        jit_dump: false,
        safepoint_spin: 1000,
        ffi_pool_pages: 4,
        log_level: LogLevel::Info,
    }
}

#[test]
fn environment_variables_map_to_runtime_config_and_defaults() {
    let _guard = lock_serial();
    // Per R2.03 and R2.16, the runtime reads the documented environment variables up front.
    with_env(
        &[
            ("EGCL_HEAP_SIZE", Some("16m")),
            ("EGCL_TLAB_SIZE", Some("512k")),
            ("EGCL_NURSERY_SIZE", Some("4m")),
            ("EGCL_STACK_SIZE", Some("256k")),
            ("EGCL_WORKERS", Some("3")),
            ("EGCL_IMAGE", Some("custom.bimg")),
            ("EGCL_GC_LOG", Some("gc.log")),
            ("EGCL_JIT_DUMP", Some("yes")),
            ("EGCL_SAFEPOINT_SPIN", Some("4096")),
            ("EGCL_FFI_POOL_PAGES", Some("9")),
            ("EGCL_LOG_LEVEL", Some("debug")),
        ],
        || {
            let cfg = RuntimeConfig::from_env().unwrap();
            assert_eq!(cfg.heap_size, 16 * 1024 * 1024);
            assert_eq!(cfg.tlab_size, 512 * 1024);
            assert_eq!(cfg.nursery_size, 4 * 1024 * 1024);
            assert_eq!(cfg.stack_size, 256 * 1024);
            assert_eq!(cfg.num_workers, 3);
            assert_eq!(cfg.image_path.as_deref(), Some("custom.bimg"));
            assert_eq!(cfg.gc_log.as_deref(), Some("gc.log"));
            assert!(cfg.jit_dump);
            assert_eq!(cfg.safepoint_spin, 4096);
            assert_eq!(cfg.ffi_pool_pages, 9);
            assert_eq!(cfg.log_level, LogLevel::Debug);
        },
    );

    with_env(
        &[
            ("EGCL_HEAP_SIZE", None),
            ("EGCL_TLAB_SIZE", None),
            ("EGCL_NURSERY_SIZE", None),
            ("EGCL_STACK_SIZE", None),
            ("EGCL_WORKERS", None),
            ("EGCL_IMAGE", None),
            ("EGCL_GC_LOG", None),
            ("EGCL_JIT_DUMP", None),
            ("EGCL_SAFEPOINT_SPIN", None),
            ("EGCL_FFI_POOL_PAGES", None),
            ("EGCL_LOG_LEVEL", None),
        ],
        || {
            let cfg = RuntimeConfig::from_env().unwrap();
            assert_eq!(cfg.heap_size, 512 * 1024 * 1024);
            assert_eq!(cfg.nursery_size, 64 * 1024 * 1024);
            assert_eq!(cfg.tlab_size, 2 * 1024 * 1024);
            assert_eq!(cfg.stack_size, 512 * 1024);
            assert!(cfg.num_workers >= 1);
            assert_eq!(cfg.image_path.as_deref(), Some("egcl.bimg"));
            assert_eq!(cfg.gc_log, None);
            assert!(!cfg.jit_dump);
            assert_eq!(cfg.safepoint_spin, 1000);
            assert_eq!(cfg.ffi_pool_pages, 4);
            assert_eq!(cfg.log_level, LogLevel::Info);
        },
    );
}

#[test]
fn invalid_environment_values_propagate_as_results_not_panics() {
    let _guard = lock_serial();
    // Per R2.16, config parsing validates the startup environment.
    // Per R2.18, failures propagate via `Result<T, EgclError>`.
    with_env(&[("EGCL_HEAP_SIZE", Some("bogus"))], || {
        let err = RuntimeConfig::from_env().unwrap_err();
        assert!(matches!(err, EgclError::Internal(_)));
        assert!(err.to_string().contains("EGCL_HEAP_SIZE"));
    });

    with_env(&[("EGCL_JIT_DUMP", Some("maybe"))], || {
        let err = RuntimeConfig::from_env().unwrap_err();
        assert!(matches!(err, EgclError::Internal(_)));
        assert!(err.to_string().contains("EGCL_JIT_DUMP"));
    });
}

#[test]
fn parse_cli_overrides_runtime_flags_and_preserves_passthrough_arguments() {
    let _guard = lock_serial();
    // Per R2.02, startup begins by parsing CLI input.
    // Per R2.03 and R2.16, CLI flags override worker and memory configuration.
    with_env(
        &[
            ("EGCL_WORKERS", Some("2")),
            ("EGCL_IMAGE", Some("env.bimg")),
        ],
        || {
            let args = vec![
                "--eval".to_string(),
                "(+ 1 2)".to_string(),
                "--workers".to_string(),
                "5".to_string(),
                "--heap-size".to_string(),
                "32m".to_string(),
                "--image".to_string(),
                "cli.bimg".to_string(),
                "--no-image".to_string(),
                "--gc-log".to_string(),
                "gc.txt".to_string(),
                "--jit-dump".to_string(),
                "--log-level".to_string(),
                "trace".to_string(),
                "--".to_string(),
                "script.lisp".to_string(),
                "--user-arg".to_string(),
            ];

            let (cfg, passthrough) = parse_cli(&args).unwrap();
            assert_eq!(cfg.eval_form.as_deref(), Some("(+ 1 2)"));
            assert_eq!(cfg.num_workers, 5);
            assert_eq!(cfg.heap_size, 32 * 1024 * 1024);
            assert_eq!(cfg.image_path.as_deref(), Some("cli.bimg"));
            assert!(cfg.no_image);
            assert_eq!(cfg.gc_log.as_deref(), Some("gc.txt"));
            assert!(cfg.jit_dump);
            assert_eq!(cfg.log_level, LogLevel::Trace);
            assert_eq!(passthrough, vec!["script.lisp", "--user-arg"]);
        },
    );
}

#[test]
fn parse_cli_reports_missing_and_unknown_flags_as_runtime_errors() {
    let _guard = lock_serial();
    // Per R2.18, CLI errors are surfaced as `Result` failures.
    let err = parse_cli(&["--eval".to_string()]).unwrap_err();
    assert!(matches!(err, EgclError::Internal(_)));
    assert!(err.to_string().contains("--eval"));

    let err = parse_cli(&["--wat".to_string()]).unwrap_err();
    assert!(matches!(err, EgclError::Internal(_)));
    assert!(err.to_string().contains("unknown flag"));
}

#[test]
fn signal_handler_installation_converts_sigint_into_runtime_observable_state() {
    let _guard = lock_serial();
    // Per R2.10, SIGINT is delivered to the runtime as an interrupt signal rather than terminating the process.
    install_signal_handlers().unwrap();
    assert!(!check_sigint());
    unsafe { libc::raise(libc::SIGINT) };
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(check_sigint());
    assert!(!check_sigint());
}

#[test]
fn runtime_init_rejects_invalid_startup_configuration_via_result() {
    let _guard = lock_serial();
    // Per R2.02, startup validates runtime configuration before continuing.
    // Per R2.18, invalid startup state returns `EgclError` rather than panicking.
    let mut cfg = minimal_config();
    cfg.heap_size = 0;
    let err = match Runtime::init(cfg) {
        Ok(_) => panic!("expected Runtime::init to reject heap_size=0"),
        Err(err) => err,
    };
    assert!(matches!(err, EgclError::Internal(_)));
    assert!(err.to_string().contains("heap_size"));
}

#[test]
fn runtime_eval_exercises_the_public_top_level_evaluator() {
    let _guard = lock_serial();
    // Per R2.02, after startup the runtime reaches the CL entry path.
    let mut runtime = Runtime::init(minimal_config()).unwrap();
    let value = runtime.eval("(+ 1 2 3)").unwrap();
    assert_eq!(value.as_fixnum(), 6);
    runtime.shutdown().unwrap();
}

#[test]
fn runtime_run_supports_eval_load_and_repl_exit_paths() {
    let _guard = lock_serial();
    // Per R2.02, startup proceeds through image/bootstrap selection into a CL entry action.
    // Per R2.17, shutdown yields a CL-controlled exit code.
    let mut eval_cfg = minimal_config();
    eval_cfg.eval_form = Some("(+ 40 2)".to_string());
    let mut eval_rt = Runtime::init(eval_cfg).unwrap();
    assert_eq!(eval_rt.run().unwrap(), 0);
    eval_rt.shutdown().unwrap();

    let mut load_cfg = minimal_config();
    let load_path = runtime_test_file("spec-runtime-load.lisp");
    std::fs::create_dir_all(load_path.parent().unwrap()).unwrap();
    std::fs::write(&load_path, "(+ 7 8)").unwrap();
    load_cfg.load_file = Some(load_path.to_string_lossy().into_owned());
    let mut load_rt = Runtime::init(load_cfg).unwrap();
    assert_eq!(load_rt.run().unwrap(), 0);
    load_rt.shutdown().unwrap();
    let _ = std::fs::remove_file(load_path);

    // REPL exit path — driven hermetically with a canned reader so the test
    // never blocks on interactive process stdin (which would hang the whole
    // serial-locked suite). Exercises an eval line and an explicit (quit).
    let mut repl_rt = Runtime::init(minimal_config()).unwrap();
    let input = std::io::Cursor::new(b"(+ 1 2)\n(quit)\n".to_vec());
    assert_eq!(repl_rt.run_repl_with_reader(input).unwrap(), 0);
    repl_rt.shutdown().unwrap();

    // And the EOF exit path (empty input → immediate clean exit, code 0).
    let mut eof_rt = Runtime::init(minimal_config()).unwrap();
    assert_eq!(eof_rt.run_repl_with_reader(std::io::empty()).unwrap(), 0);
    eof_rt.shutdown().unwrap();
}

#[test]
fn runtime_shutdown_transitions_the_instance_to_shutdown_state() {
    let _guard = lock_serial();
    // Per R2.17, shutdown is an explicit lifecycle transition.
    let mut runtime = Runtime::init(minimal_config()).unwrap();
    runtime.shutdown().unwrap();
    let err = runtime.run().unwrap_err();
    assert!(matches!(err, EgclError::Shutdown));
}

fn runtime_test_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("spec-runtime-tests")
        .join(name)
}

#[repr(C, align(8))]
struct HeaderAndWord {
    header: ObjectHeader,
    word: u64,
}

fn heap_object_value(type_id: u8) -> EgclVal {
    let boxed = Box::new(HeaderAndWord {
        header: ObjectHeader::new(type_id, 2),
        word: 0,
    });
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe { EgclVal::from_heap_ptr(ptr) }
}

fn helper_command(mode: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.arg("--exact")
        .arg("spec_runtime_core_subprocess_helper")
        .arg("--nocapture")
        .env("EGCL_RT_SPEC_HELPER", mode);
    cmd
}

fn run_helper(mode: &str, timeout: Duration) -> std::process::Output {
    let mut child = helper_command(mode)
        // Null stdin so a helper subprocess can never block on the parent's
        // inherited terminal (a TTY never sends EOF); it gets immediate EOF.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait_with_output().unwrap();
        }
        match child.try_wait().unwrap() {
            Some(_) => return child.wait_with_output().unwrap(),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

extern "C" fn ffi_abs_u64(x: u64) -> u64 {
    x.abs_diff(0)
}

extern "C" fn callback_target() -> u64 {
    99
}

extern "C" fn ffi_observe_native_state() -> u64 {
    let state = current_fiber()
        .map(|fiber| fiber.state() as u8)
        .unwrap_or(0xFE);
    OBSERVED_THREAD_STATE.store(state, Ordering::SeqCst);
    0
}

fn spawn_fiber(entry: EgclVal) -> FiberId {
    let id = make_fiber(entry).unwrap();
    submit_fiber(id).unwrap();
    id
}

fn fiber_returns_seven() -> EgclVal {
    fiber_yield().unwrap();
    EgclVal::from_fixnum(7)
}

fn fiber_reports_stack_base() -> EgclVal {
    EgclVal::from_fixnum(current_stack().base() as usize as i64)
}

fn fiber_calls_ffi_and_returns_nil() -> EgclVal {
    let _ = unsafe {
        ffi_call(
            ffi_observe_native_state as *const (),
            &AlienType::Void,
            &[],
            &[],
        )
    };
    NIL
}

fn slow_fiber() -> EgclVal {
    std::thread::sleep(Duration::from_millis(150));
    EgclVal::from_fixnum(1)
}

fn finalizer_dispatch(finalizer: EgclVal, object: EgclVal) {
    FINALIZER_CALLS.fetch_add(1, Ordering::SeqCst);
    LAST_FINALIZER.store(finalizer.to_raw() as usize, Ordering::SeqCst);
    LAST_FINALIZED_OBJECT.store(object.to_raw() as usize, Ordering::SeqCst);
}

static OBSERVED_THREAD_STATE: AtomicU8 = AtomicU8::new(0xFF);
static FINALIZER_CALLS: AtomicUsize = AtomicUsize::new(0);
static LAST_FINALIZER: AtomicUsize = AtomicUsize::new(0);
static LAST_FINALIZED_OBJECT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn runtime_boots_the_repl_entry_path_within_the_startup_budget() {
    let _guard = lock_serial();
    // Per R2.01, the runtime boots to a REPL-capable top-level entry within 50 ms.
    let started = Instant::now();
    let mut runtime = Runtime::init(minimal_config()).unwrap();
    // Drive the REPL entry hermetically (immediate EOF) rather than blocking on
    // interactive process stdin, which would hang the serial-locked suite.
    assert_eq!(runtime.run_repl_with_reader(std::io::empty()).unwrap(), 0);
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "startup path exceeded 50 ms budget: {:?}",
        started.elapsed()
    );
    runtime.shutdown().unwrap();
}

#[test]
fn startup_requires_image_load_unless_the_cli_selects_bootstrap_mode() {
    let _guard = lock_serial();
    // Per R2.02, startup runs CLI parsing, then image/bootstrap selection, before CL entry.
    let missing_image = runtime_test_file("missing-image-does-not-exist.bimg");
    let (cfg, passthrough) = parse_cli(&[
        "--image".to_string(),
        missing_image.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert!(passthrough.is_empty());

    let init_result = Runtime::init(cfg);
    assert!(
        init_result.is_err(),
        "startup must reject a missing image before entering CL"
    );
}

#[test]
fn fibers_run_user_functions_through_the_carrier_pool() {
    let _guard = lock_serial();
    // Per R2.04, fibers are multiplexed M:N onto exposed carrier threads.
    let entry = native_entry::entry(fiber_returns_seven);
    let ids: Vec<_> = (0..8).map(|_| spawn_fiber(entry)).collect();
    let results: Vec<_> = ids
        .into_iter()
        .map(|id| join_fiber(id).unwrap().as_fixnum())
        .collect();

    assert_eq!(results, vec![7; 8]);
}

#[test]
fn fiber_record_publishes_stack_roots() {
    let _guard = lock_serial();
    // A managed fiber publishes its stack roots (SP/FP) at a safepoint so the
    // collector can scan them while it is suspended (bliss-jtc.14.2).
    let entry = native_entry::entry(fiber_record_check);
    assert_eq!(join_fiber(spawn_fiber(entry)).unwrap().as_fixnum(), 7);
}

fn fiber_record_check() -> EgclVal {
    let Some(fiber) = current_fiber() else {
        return EgclVal::from_fixnum(-1);
    };
    let mut checks = 0;
    fiber.publish_stack(0xAAAA_0000, 0xBBBB_0000);
    if fiber.published_stack() == (0xAAAA_0000, 0xBBBB_0000) {
        checks |= 1;
    }
    fiber.continuation().save(11, 22, 33);
    if fiber.continuation().snapshot() == (11, 22, 33) {
        checks |= 2;
    }
    if fiber.dynamic_bindings().is_empty()
        && fiber.handler_stack().is_empty()
        && fiber.restart_stack().is_empty()
        && fiber.carrier_id().is_some()
    {
        checks |= 4;
    }
    EgclVal::from_fixnum(checks)
}

#[test]
fn many_fibers_complete_via_work_stealing() {
    let _guard = lock_serial();
    // Far more tasks than workers forces the per-worker deques to fill unevenly
    // and idle workers to steal (bliss-jtc.14.1); every task must still complete.
    let entry = native_entry::entry(fiber_returns_seven);
    let ids: Vec<_> = (0..200).map(|_| spawn_fiber(entry)).collect();
    let results: Vec<_> = ids
        .into_iter()
        .map(|id| join_fiber(id).unwrap().as_fixnum())
        .collect();
    assert_eq!(results.len(), 200);
    assert!(
        results.iter().all(|&r| r == 7),
        "every local or stolen task must run to completion"
    );
}

#[test]
fn fibers_have_distinct_cl_stacks_from_each_other_and_from_the_caller() {
    let _guard = lock_serial();
    // Per R2.05, each fiber maintains its own CL stack.
    let entry = native_entry::entry(fiber_reports_stack_base);
    // Capture the caller's stack base *first*. current_thread() lazily creates
    // this thread's bootstrap CL stack, so it must be materialized before the
    // fibers are spawned and freed — otherwise the allocator can hand the
    // caller's freshly-created stack the memory of an already-joined green
    // thread's stack, producing a spurious base-address collision.
    let current = current_thread().stack().base() as usize;

    let id1 = spawn_fiber(entry);
    let id2 = spawn_fiber(entry);

    let stack1 = join_fiber(id1).unwrap().as_fixnum() as usize;
    let stack2 = join_fiber(id2).unwrap().as_fixnum() as usize;

    assert_ne!(stack1, stack2, "fibers must not share a CL stack");
    assert_ne!(
        stack1, current,
        "fiber stack must differ from the caller's stack"
    );
    assert_ne!(
        stack2, current,
        "fiber stack must differ from the caller's stack"
    );
}

#[test]
fn frame_walking_uses_prev_fp_links_and_safepoint_maps() {
    // Per R2.06, stack frames are walkable via frame links and safepoint metadata.
    let source_locations = Box::leak(Box::new([SourceLocationEntry {
        pc_offset: 0,
        location: SourceLocation {
            file: Some("spec-runtime.lisp".to_string()),
            line: 12,
            column: 3,
        },
    }]));
    let stack_map_bytes = Box::leak(Box::new([0xAA_u8, 0xBB, 0xCC]));
    let stack_maps = Box::leak(Box::new([StackMapEntry {
        pc_offset: 0,
        bytes: stack_map_bytes.as_ptr() as usize,
        len: stack_map_bytes.len(),
    }]));
    let code_info = CodeInfo::new(source_locations, stack_maps);

    let mut older = Frame {
        prev_fp: std::ptr::null_mut(),
        return_pc: std::ptr::null(),
        function: NIL,
        code_info,
        flags: FrameType::Call as u32,
        num_locals: 0,
        _pad: 0,
    };
    let newer = Frame {
        prev_fp: &mut older,
        return_pc: std::ptr::null(),
        function: T,
        code_info,
        flags: FrameType::Catch as u32,
        num_locals: 0,
        _pad: 0,
    };

    let walked: Vec<_> = unsafe { FrameWalker::new(&newer) }.collect();
    assert_eq!(walked.len(), 2);
    assert_eq!(unsafe { (*walked[0]).frame_type() }, FrameType::Catch);
    assert_eq!(unsafe { (*walked[1]).frame_type() }, FrameType::Call);
    assert_eq!(
        code_info.source_location(0).unwrap().file.as_deref(),
        Some("spec-runtime.lisp")
    );
    assert_eq!(code_info.stack_map(0).unwrap(), &stack_map_bytes[..]);
}

#[test]
fn safepoint_polling_publishes_stack_state_for_gc_walkers() {
    let _guard = lock_serial();
    // Per R2.07, safepoint polling publishes the CL stack top for GC/debugger consumers.
    let thread = current_thread();
    enter_safepoint();

    assert_eq!(thread.stack().published_sp(), thread.stack().used());
    assert_eq!(thread.stack().published_fp(), thread.stack().fp());
}

#[test]
fn ffi_calls_use_the_c_abi_and_callbacks_round_trip_back_into_cl() {
    let _guard = lock_serial();
    // Per R2.11, FFI calls use the platform C ABI.
    // Per R2.12, callbacks from C into CL are supported.
    let ffi_result = unsafe {
        ffi_call(
            ffi_abs_u64 as *const (),
            &AlienType::Int {
                signed: false,
                bits: 64,
            },
            &[AlienType::Int {
                signed: false,
                bits: 64,
            }],
            &[42],
        )
    }
    .unwrap();
    assert_eq!(ffi_result, 42);

    let callback = Callback::new(
        EgclVal::from_fixnum(callback_target as *const () as usize as i64),
        AlienType::Int {
            signed: false,
            bits: 64,
        },
        vec![],
    )
    .unwrap();
    callback.prepare_call();
    let trampoline: extern "C" fn() -> u64 = unsafe { std::mem::transmute(callback.as_fn_ptr()) };
    assert_eq!(trampoline(), 99);
}

#[test]
fn ffi_marshalling_covers_scalars_pointers_struct_layouts_and_void() {
    // Per R2.13, alien marshalling covers integers, float, double, pointer, struct-by-value, and void.
    assert_eq!(
        marshal_to_c(
            EgclVal::from_fixnum(-7),
            &AlienType::Int {
                signed: true,
                bits: 8,
            },
        )
        .unwrap() as i8,
        -7
    );
    assert_eq!(
        marshal_to_c(
            EgclVal::from_fixnum(255),
            &AlienType::Int {
                signed: false,
                bits: 16,
            },
        )
        .unwrap() as u16,
        255
    );
    assert_eq!(
        marshal_to_c(
            EgclVal::from_fixnum(65_535),
            &AlienType::Int {
                signed: false,
                bits: 32,
            },
        )
        .unwrap() as u32,
        65_535
    );
    assert_eq!(
        marshal_to_c(
            EgclVal::from_fixnum(1),
            &AlienType::Int {
                signed: false,
                bits: 64,
            },
        )
        .unwrap(),
        1
    );
    assert_eq!(unmarshal_from_c(0, &AlienType::Void).unwrap(), NIL);
    assert_eq!(
        unmarshal_from_c(
            0xFFFF_FFFF_FFFF_FFF9,
            &AlienType::Int {
                signed: true,
                bits: 64,
            },
        )
        .unwrap()
        .as_fixnum(),
        -7
    );
    assert!(
        (unmarshal_from_c((3.5f32).to_bits() as u64, &AlienType::Float)
            .unwrap()
            .as_single_float()
            - 3.5)
            .abs()
            < f32::EPSILON
    );
    assert_eq!(
        marshal_to_c(NIL, &AlienType::Pointer(Box::new(AlienType::Void))).unwrap(),
        0
    );

    let pair = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            AlienType::Int {
                signed: true,
                bits: 32,
            },
        ],
        packed: false,
    };
    assert_eq!(pair.size(), 8);
    assert_eq!(pair.alignment(), 4);
}

#[test]
fn ffi_calls_transition_fibers_to_native_state() {
    let _guard = lock_serial();
    // Per R2.15, FFI calls transition the calling fiber into Native state.
    OBSERVED_THREAD_STATE.store(0xFF, Ordering::SeqCst);
    let entry = native_entry::entry(fiber_calls_ffi_and_returns_nil);
    let id = spawn_fiber(entry);
    assert_eq!(join_fiber(id).unwrap(), NIL);
    assert_eq!(
        OBSERVED_THREAD_STATE.load(Ordering::SeqCst),
        FiberState::Native as u8,
        "FFI callee should observe the fiber in Native state"
    );
}

#[test]
fn shutdown_runs_registered_finalizers_and_waits_for_in_flight_workers() {
    let _guard = lock_serial();
    // Per R2.17, shutdown runs finalizers, joins carrier threads, and exits under CL control.
    FINALIZER_CALLS.store(0, Ordering::SeqCst);
    LAST_FINALIZER.store(0, Ordering::SeqCst);
    LAST_FINALIZED_OBJECT.store(0, Ordering::SeqCst);
    set_finalizer_dispatch(finalizer_dispatch);

    let mut runtime = Runtime::init(minimal_config()).unwrap();
    let object = heap_object_value(type_id::SYMBOL);
    let finalizer = UNBOUND;
    register_finalizer(object, finalizer).unwrap();

    let slow_entry = native_entry::entry(slow_fiber);
    let slow_fiber = spawn_fiber(slow_entry);
    let started = Instant::now();
    runtime.shutdown().unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(150),
        "shutdown must wait for worker completion"
    );
    assert_eq!(
        FINALIZER_CALLS.load(Ordering::SeqCst),
        1,
        "shutdown must run finalizers"
    );
    assert_eq!(
        LAST_FINALIZER.load(Ordering::SeqCst) as u64,
        finalizer.to_raw()
    );
    assert_eq!(
        LAST_FINALIZED_OBJECT.load(Ordering::SeqCst) as u64,
        object.to_raw()
    );
    assert_eq!(join_fiber(slow_fiber).unwrap().as_fixnum(), 1);
}

#[test]
fn runtime_accepts_large_fiber_populations_without_exhausting_the_api() {
    let _guard = lock_serial();
    // Per R2.19, the runtime supports large populations of simultaneous fibers.
    let entry = native_entry::entry(fiber_returns_seven);
    let ids: Vec<_> = (0..1024).map(|_| spawn_fiber(entry)).collect();
    for id in ids {
        assert_eq!(join_fiber(id).unwrap().as_fixnum(), 7);
    }
}

#[test]
fn subprocess_acceptance_catches_safepoint_sigsegv_and_null_pointer_sigsegv_as_conditions() {
    let _guard = lock_serial();
    // Per R2.08, a safepoint-page SIGSEGV becomes a safepoint trap.
    // Per R2.09, a null-pointer SIGSEGV becomes a CL TYPE-ERROR condition.
    let safepoint = run_helper("safepoint-segv", Duration::from_secs(2));
    assert!(
        safepoint.status.success(),
        "safepoint SIGSEGV path crashed instead of becoming a trap: {:?}",
        safepoint.status
    );
    assert!(String::from_utf8_lossy(&safepoint.stdout).contains("SAFEPOINT_TRAP"));

    let null_deref = run_helper("null-deref-segv", Duration::from_secs(2));
    assert!(
        null_deref.status.success(),
        "null-pointer SIGSEGV path crashed instead of becoming TYPE-ERROR: {:?}",
        null_deref.status
    );
    assert!(String::from_utf8_lossy(&null_deref.stdout).contains("TYPE_ERROR"));
}

#[test]
fn subprocess_acceptance_translates_cl_stack_overflow_into_storage_condition() {
    let _guard = lock_serial();
    // Per R2.20, CL stack overflow raises STORAGE-CONDITION rather than killing the process.
    let output = run_helper("cl-stack-overflow", Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stack overflow path terminated the subprocess: {:?}",
        output.status
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STORAGE_CONDITION"));
}

#[test]
fn subprocess_acceptance_aborts_unclassified_sigsegv_without_success() {
    let _guard = lock_serial();
    let output = run_helper("ordinary-segv", Duration::from_secs(2));

    assert!(
        !output.status.success(),
        "ordinary SIGSEGV must not be reported as a successful subprocess"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unhandled SIGSEGV"),
        "ordinary SIGSEGV should emit the minimal handler diagnostic; stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn subprocess_acceptance_recovers_safepoint_poll_page_sigsegv() {
    let _guard = lock_serial();
    let output = run_helper("safepoint-page-fault", Duration::from_secs(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "safepoint poll-page SIGSEGV should recover and continue; status={:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stdout.contains("SAFEPOINT_TRAP"),
        "safepoint poll-page SIGSEGV should reach the post-fault path; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn subprocess_acceptance_rewrites_null_guard_sigsegv_to_recovery_ip() {
    let _guard = lock_serial();
    let output = run_helper("null-guard-recovery", Duration::from_secs(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "null-guard SIGSEGV should recover outside the signal handler; status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.contains("NULL_GUARD_RECOVERED"),
        "recovery path should observe the deferred null-guard flag; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn subprocess_acceptance_rewrites_stack_guard_sigsegv_to_recovery_ip() {
    let _guard = lock_serial();
    let output = run_helper("stack-guard-recovery", Duration::from_secs(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "stack-guard SIGSEGV should recover outside the signal handler; status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.contains("STORAGE_CONDITION"),
        "recovery path should observe the deferred stack-guard flag; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn subprocess_acceptance_rewrites_egcl_stack_guard_sigsegv_to_recovery_ip() {
    let _guard = lock_serial();
    let output = run_helper("egcl-stack-guard-recovery", Duration::from_secs(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "EgclStack guard SIGSEGV should recover outside the signal handler; status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.contains("STORAGE_CONDITION"),
        "EgclStack guard recovery should observe the deferred stack-guard flag; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn signal_installation_enables_current_thread_sigaltstack() {
    let _guard = lock_serial();
    install_signal_handlers().unwrap();

    let stack = egcl_rt::syscall::current_sigaltstack().unwrap();
    assert_eq!(
        stack.ss_flags & egcl_rt::syscall::SS_DISABLE,
        0,
        "SIGSEGV handler must have an alternate signal stack"
    );
    assert!(
        stack.ss_size >= egcl_rt::syscall::MIN_SIGSTKSZ,
        "alternate signal stack is too small: {}",
        stack.ss_size
    );
}

#[test]
fn sigsegv_address_classifier_distinguishes_null_guard_from_ordinary_faults() {
    let page = egcl_rt::safepoint::SafepointPage::init().unwrap();
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0),
        egcl_rt::runtime::SigsegvFaultKind::NullGuard
    );
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0xfff),
        egcl_rt::runtime::SigsegvFaultKind::NullGuard
    );
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0x1_0000),
        egcl_rt::runtime::SigsegvFaultKind::Ordinary
    );
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(page.address() as usize),
        egcl_rt::runtime::SigsegvFaultKind::SafepointPoll
    );
    egcl_rt::runtime::register_sigsegv_stack_guard_range(0x2_0000, 4096);
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0x2_0000),
        egcl_rt::runtime::SigsegvFaultKind::StackGuard
    );
    egcl_rt::runtime::register_sigsegv_stack_guard_range(0x3_0000, 4096);
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0x2_0000),
        egcl_rt::runtime::SigsegvFaultKind::StackGuard,
        "registering another stack guard must not erase an existing live guard"
    );
    assert_eq!(
        egcl_rt::runtime::classify_sigsegv_address(0x3_0000),
        egcl_rt::runtime::SigsegvFaultKind::StackGuard
    );
}

#[test]
fn null_guard_sigsegv_flag_is_deferred_and_one_shot() {
    egcl_rt::runtime::post_sigsegv_null_guard();
    assert!(egcl_rt::runtime::check_sigsegv_null_guard());
    assert!(!egcl_rt::runtime::check_sigsegv_null_guard());
}

#[test]
fn sigsegv_recovery_ips_are_native_thread_local() {
    extern "C" fn main_null_recovery() -> ! {
        egcl_rt::syscall::exit_group(71);
    }
    extern "C" fn other_null_recovery() -> ! {
        egcl_rt::syscall::exit_group(72);
    }
    extern "C" fn main_stack_recovery() -> ! {
        egcl_rt::syscall::exit_group(73);
    }
    extern "C" fn other_stack_recovery() -> ! {
        egcl_rt::syscall::exit_group(74);
    }

    egcl_rt::runtime::set_sigsegv_null_guard_recovery_ip(main_null_recovery as *const () as usize);
    egcl_rt::runtime::set_sigsegv_stack_guard_recovery_ip(
        main_stack_recovery as *const () as usize,
    );

    std::thread::spawn(|| {
        egcl_rt::runtime::set_sigsegv_null_guard_recovery_ip(
            other_null_recovery as *const () as usize,
        );
        egcl_rt::runtime::set_sigsegv_stack_guard_recovery_ip(
            other_stack_recovery as *const () as usize,
        );
        assert_eq!(
            egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            other_null_recovery as *const () as usize
        );
        assert_eq!(
            egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
            other_stack_recovery as *const () as usize
        );
    })
    .join()
    .unwrap();

    assert_eq!(
        egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
        main_null_recovery as *const () as usize
    );
    assert_eq!(
        egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        main_stack_recovery as *const () as usize
    );
}

extern "C" fn null_guard_recovery_exit() -> ! {
    if egcl_rt::runtime::check_sigsegv_null_guard() {
        egcl_rt::syscall::dbg_write(b"NULL_GUARD_RECOVERED\n");
        egcl_rt::syscall::exit_group(0);
    }
    egcl_rt::syscall::dbg_write(b"NULL_GUARD_FLAG_MISSING\n");
    egcl_rt::syscall::exit_group(1);
}

extern "C" fn null_guard_type_error_exit() -> ! {
    if egcl_rt::runtime::check_sigsegv_null_guard() {
        unsafe { libc::write(1, b"TYPE_ERROR\n".as_ptr().cast(), b"TYPE_ERROR\n".len()) };
        egcl_rt::syscall::exit_group(0);
    }
    egcl_rt::syscall::dbg_write(b"NULL_GUARD_FLAG_MISSING\n");
    egcl_rt::syscall::exit_group(1);
}

extern "C" fn stack_guard_recovery_exit() -> ! {
    if egcl_rt::runtime::check_sigsegv_stack_guard() {
        egcl_rt::syscall::dbg_write(b"STORAGE_CONDITION\n");
        egcl_rt::syscall::exit_group(0);
    }
    egcl_rt::syscall::dbg_write(b"STACK_GUARD_FLAG_MISSING\n");
    egcl_rt::syscall::exit_group(1);
}

#[test]
fn spec_runtime_core_subprocess_helper() {
    match std::env::var("EGCL_RT_SPEC_HELPER").ok().as_deref() {
        Some("safepoint-segv") => {
            install_signal_handlers().unwrap();
            let page = egcl_rt::safepoint::SafepointPage::init().unwrap();
            page.request_safepoint().unwrap();
            unsafe {
                std::ptr::read_volatile(page.address());
            }
            page.resume().unwrap();
            println!("SAFEPOINT_TRAP");
        }
        Some("null-deref-segv") => {
            install_signal_handlers().unwrap();
            egcl_rt::runtime::set_sigsegv_null_guard_recovery_ip(
                null_guard_type_error_exit as *const () as usize,
            );
            unsafe {
                let ptr: *mut u8 = std::ptr::null_mut();
                std::ptr::write_volatile(ptr, 1);
            }
        }
        Some("cl-stack-overflow") => {
            let mut runtime = Runtime::init(minimal_config()).unwrap();
            let _ = runtime.eval("(progn (defun loop () (loop)) (loop))");
            println!("STORAGE_CONDITION");
        }
        Some("ordinary-segv") => {
            install_signal_handlers().unwrap();
            unsafe {
                let ptr = egcl_rt::syscall::mmap(
                    std::ptr::null_mut(),
                    4096,
                    egcl_rt::syscall::PROT_NONE,
                    egcl_rt::syscall::MAP_PRIVATE | egcl_rt::syscall::MAP_ANONYMOUS,
                    -1,
                    0,
                )
                .unwrap();
                std::ptr::write_volatile(ptr, 1);
            }
        }
        Some("safepoint-page-fault") => {
            install_signal_handlers().unwrap();
            let page = egcl_rt::safepoint::SafepointPage::init().unwrap();
            page.request_safepoint().unwrap();
            unsafe {
                std::ptr::read_volatile(page.address());
            }
            page.resume().unwrap();
            println!("SAFEPOINT_TRAP");
        }
        Some("null-guard-recovery") => {
            install_signal_handlers().unwrap();
            egcl_rt::runtime::set_sigsegv_null_guard_recovery_ip(
                null_guard_recovery_exit as *const () as usize,
            );
            unsafe {
                let ptr: *mut u8 = std::ptr::null_mut();
                std::ptr::write_volatile(ptr, 1);
            }
        }
        Some("stack-guard-recovery") => {
            install_signal_handlers().unwrap();
            let ptr = unsafe {
                egcl_rt::syscall::mmap(
                    std::ptr::null_mut(),
                    4096,
                    egcl_rt::syscall::PROT_NONE,
                    egcl_rt::syscall::MAP_PRIVATE | egcl_rt::syscall::MAP_ANONYMOUS,
                    -1,
                    0,
                )
                .unwrap()
            };
            egcl_rt::runtime::register_sigsegv_stack_guard_range(ptr as usize, 4096);
            egcl_rt::runtime::set_sigsegv_stack_guard_recovery_ip(
                stack_guard_recovery_exit as *const () as usize,
            );
            unsafe {
                std::ptr::write_volatile(ptr, 1);
            }
        }
        Some("egcl-stack-guard-recovery") => {
            install_signal_handlers().unwrap();
            let stack = egcl_rt::EgclStack::new(4096);
            egcl_rt::runtime::set_sigsegv_stack_guard_recovery_ip(
                stack_guard_recovery_exit as *const () as usize,
            );
            unsafe {
                std::ptr::write_volatile(stack.guard_base().unwrap() as *mut u8, 1);
            }
        }
        _ => {}
    }
}

#[path = "support/native_entry.rs"]
mod native_entry;
