use bliss_rt::error::BlissError;
use bliss_rt::runtime::check_sigint;
use bliss_rt::{LogLevel, Runtime, RuntimeConfig, install_signal_handlers, parse_cli};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

fn serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
    let _guard = serial_lock().lock().unwrap();
    // Per R2.03 and R2.16, the runtime reads the documented environment variables up front.
    with_env(
        &[
            ("BLISS_HEAP_SIZE", Some("16m")),
            ("BLISS_TLAB_SIZE", Some("512k")),
            ("BLISS_NURSERY_SIZE", Some("4m")),
            ("BLISS_STACK_SIZE", Some("256k")),
            ("BLISS_WORKERS", Some("3")),
            ("BLISS_IMAGE", Some("custom.bimg")),
            ("BLISS_GC_LOG", Some("gc.log")),
            ("BLISS_JIT_DUMP", Some("yes")),
            ("BLISS_SAFEPOINT_SPIN", Some("4096")),
            ("BLISS_FFI_POOL_PAGES", Some("9")),
            ("BLISS_LOG_LEVEL", Some("debug")),
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
            ("BLISS_HEAP_SIZE", None),
            ("BLISS_TLAB_SIZE", None),
            ("BLISS_NURSERY_SIZE", None),
            ("BLISS_STACK_SIZE", None),
            ("BLISS_WORKERS", None),
            ("BLISS_IMAGE", None),
            ("BLISS_GC_LOG", None),
            ("BLISS_JIT_DUMP", None),
            ("BLISS_SAFEPOINT_SPIN", None),
            ("BLISS_FFI_POOL_PAGES", None),
            ("BLISS_LOG_LEVEL", None),
        ],
        || {
            let cfg = RuntimeConfig::from_env().unwrap();
            assert_eq!(cfg.heap_size, 512 * 1024 * 1024);
            assert_eq!(cfg.nursery_size, 64 * 1024 * 1024);
            assert_eq!(cfg.tlab_size, 2 * 1024 * 1024);
            assert_eq!(cfg.stack_size, 512 * 1024);
            assert!(cfg.num_workers >= 1);
            assert_eq!(cfg.image_path.as_deref(), Some("bliss.bimg"));
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
    let _guard = serial_lock().lock().unwrap();
    // Per R2.16, config parsing validates the startup environment.
    // Per R2.18, failures propagate via `Result<T, BlissError>`.
    with_env(&[("BLISS_HEAP_SIZE", Some("bogus"))], || {
        let err = RuntimeConfig::from_env().unwrap_err();
        assert!(matches!(err, BlissError::Internal(_)));
        assert!(err.to_string().contains("BLISS_HEAP_SIZE"));
    });

    with_env(&[("BLISS_JIT_DUMP", Some("maybe"))], || {
        let err = RuntimeConfig::from_env().unwrap_err();
        assert!(matches!(err, BlissError::Internal(_)));
        assert!(err.to_string().contains("BLISS_JIT_DUMP"));
    });
}

#[test]
fn parse_cli_overrides_runtime_flags_and_preserves_passthrough_arguments() {
    let _guard = serial_lock().lock().unwrap();
    // Per R2.02, startup begins by parsing CLI input.
    // Per R2.03 and R2.16, CLI flags override worker and memory configuration.
    with_env(&[("BLISS_WORKERS", Some("2")), ("BLISS_IMAGE", Some("env.bimg"))], || {
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
    });
}

#[test]
fn parse_cli_reports_missing_and_unknown_flags_as_runtime_errors() {
    let _guard = serial_lock().lock().unwrap();
    // Per R2.18, CLI errors are surfaced as `Result` failures.
    let err = parse_cli(&["--eval".to_string()]).unwrap_err();
    assert!(matches!(err, BlissError::Internal(_)));
    assert!(err.to_string().contains("--eval"));

    let err = parse_cli(&["--wat".to_string()]).unwrap_err();
    assert!(matches!(err, BlissError::Internal(_)));
    assert!(err.to_string().contains("unknown flag"));
}

#[test]
fn signal_handler_installation_converts_sigint_into_runtime_observable_state() {
    let _guard = serial_lock().lock().unwrap();
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
    let _guard = serial_lock().lock().unwrap();
    // Per R2.02, startup validates runtime configuration before continuing.
    // Per R2.18, invalid startup state returns `BlissError` rather than panicking.
    let mut cfg = minimal_config();
    cfg.heap_size = 0;
    let err = match Runtime::init(cfg) {
        Ok(_) => panic!("expected Runtime::init to reject heap_size=0"),
        Err(err) => err,
    };
    assert!(matches!(err, BlissError::Internal(_)));
    assert!(err.to_string().contains("heap_size"));
}

#[test]
fn runtime_eval_exercises_the_public_top_level_evaluator() {
    let _guard = serial_lock().lock().unwrap();
    // Per R2.02, after startup the runtime reaches the CL entry path.
    let mut runtime = Runtime::init(minimal_config()).unwrap();
    let value = runtime.eval("(+ 1 2 3)").unwrap();
    assert_eq!(value.as_fixnum(), 6);
    runtime.shutdown().unwrap();
}

#[test]
fn runtime_run_supports_eval_load_and_repl_exit_paths() {
    let _guard = serial_lock().lock().unwrap();
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

    let mut repl_rt = Runtime::init(minimal_config()).unwrap();
    assert_eq!(repl_rt.run().unwrap(), 0);
    repl_rt.shutdown().unwrap();
}

#[test]
fn runtime_shutdown_transitions_the_instance_to_shutdown_state() {
    let _guard = serial_lock().lock().unwrap();
    // Per R2.17, shutdown is an explicit lifecycle transition.
    let mut runtime = Runtime::init(minimal_config()).unwrap();
    runtime.shutdown().unwrap();
    let err = runtime.run().unwrap_err();
    assert!(matches!(err, BlissError::Shutdown));
}

fn runtime_test_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("spec-runtime-tests")
        .join(name)
}
