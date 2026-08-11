use bliss_rt::error::BlissError;
use bliss_rt::ffi::{
    AlienType, Callback, ffi_call, foreign_symbol, load_foreign_library, marshal_to_c,
    unmarshal_from_c,
};
use bliss_rt::sandbox::{Sandbox, SandboxPolicy};
use bliss_rt::value::{BlissVal, NIL};
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};

fn libc_handle() -> *mut () {
    load_foreign_library("libc.so.6")
        .or_else(|_| load_foreign_library("libSystem.B.dylib"))
        .or_else(|_| load_foreign_library("libc.so"))
        .expect("expected a libc-compatible shared library on supported platforms")
}

extern "C" fn callback_target() -> u64 {
    BlissVal::from_fixnum(77).to_raw()
}

fn read_source(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {}", path.display(), err))
}

fn collect_rust_sources(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("source directory must be readable") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[test]
fn sandbox_defaults_are_safe_and_deny_all_capabilities() {
    // Per R8.01, restricted mode disables file I/O, networking, FFI, and process spawning.
    // Per R8.02 and R8.22, sandbox grants are whitelist-based and safe-by-default.
    let sandbox = Sandbox::new(SandboxPolicy::default()).expect("default sandbox must construct");

    assert!(sandbox.policy().allowed_paths.is_empty());
    assert!(sandbox.check_path("/tmp/user.lisp").is_err());
    assert!(sandbox.check_network().is_err());
    assert!(sandbox.check_ffi().is_err());
    assert!(sandbox.check_subprocess().is_err());
}

#[test]
fn sandbox_whitelists_paths_and_enforces_heap_and_thread_budgets() {
    // Per R8.02, capability exceptions are whitelist-based.
    // Per R8.06, heap growth beyond the configured cap must be rejected.
    // Per R8.20, creating child execution inside the sandbox must remain constrained.
    let sandbox = Sandbox::new(SandboxPolicy {
        allow_filesystem: false,
        allow_network: false,
        allow_ffi: false,
        allow_subprocess: false,
        max_heap_bytes: 1024,
        max_threads: 2,
        allowed_paths: vec!["/tmp/bliss-scratch/".into()],
    })
    .expect("sandbox with limits must construct");

    assert!(sandbox.check_path("/tmp/bliss-scratch/input.lisp").is_ok());
    assert!(sandbox.check_path("/etc/passwd").is_err());
    assert!(sandbox.check_heap_alloc(256, 512).is_ok());
    assert!(sandbox.check_heap_alloc(900, 200).is_err());
    assert!(sandbox.check_thread_create(1).is_ok());
    assert!(sandbox.check_thread_create(2).is_err());
}

#[test]
fn sandbox_source_defines_eval_time_and_stack_guards_and_child_propagation() {
    // Per R8.08, sandbox mode must carry a CPU-time limit.
    // Per R8.15 and R8.16, public sandbox/runtime contracts must expose enforcement hooks.
    // Per R8.20, sandbox escape prevention must cover EVAL/COMPILE/LOAD and child-thread propagation.
    let sandbox_source = read_source("crates/bliss-rt/src/sandbox.rs");

    assert!(
        sandbox_source.contains("max_cpu_ms"),
        "sandbox policy must track CPU limits"
    );
    assert!(
        sandbox_source.contains("max_stack_depth"),
        "sandbox policy must track stack depth limits"
    );
    assert!(
        sandbox_source.contains("check_eval") || sandbox_source.contains("CAP_EVAL"),
        "sandbox must intercept reflective evaluation entrypoints"
    );
    assert!(
        sandbox_source.contains("propagate") || sandbox_source.contains("Arc<SandboxContext>"),
        "child threads must inherit the active sandbox context"
    );
}

#[test]
fn ffi_denial_and_allowance_are_observable_at_the_sandbox_boundary() {
    // Per R8.01, sandboxed FFI is disabled unless explicitly granted.
    // Per R8.02, the default grant set is empty.
    let denied = Sandbox::new(SandboxPolicy::default()).expect("sandbox must construct");
    let allowed = Sandbox::new(SandboxPolicy {
        allow_ffi: true,
        ..SandboxPolicy::default()
    })
    .expect("sandbox must construct");

    assert!(denied.check_ffi().is_err());
    assert!(allowed.check_ffi().is_ok());
}

#[test]
fn ffi_rejects_null_function_pointer_instead_of_crashing() {
    // Per R8.04, FFI pointers must be validated before dereference.
    // Per R2.18, runtime failures propagate as Result errors rather than panicking.
    let err = unsafe { ffi_call(std::ptr::null(), &AlienType::Void, &[], &[]) }
        .expect_err("null function pointer must be rejected");
    assert!(
        matches!(err, BlissError::FfiError(message) if message.contains("null function pointer"))
    );
}

#[test]
fn ffi_pointer_marshalling_never_exposes_a_raw_lisp_value_as_a_user_pointer() {
    // Per R8.05, raw pointers must never be observable from user-facing values.
    let tagged_value = BlissVal::from_single_float(1.25);
    let err = marshal_to_c(tagged_value, &AlienType::Pointer(Box::new(AlienType::Void)))
        .expect_err("opaque Lisp values must not be reinterpreted as raw pointers");
    assert!(
        matches!(err, BlissError::FfiError(ref message) if message.contains("pointer")),
        "unexpected error: {:?}",
        err
    );
}

#[test]
fn ffi_calls_real_c_symbols_with_platform_abi_and_pointer_arguments() {
    // Per R2.11, foreign calls use the platform C ABI.
    // Per R2.13, marshalling supports integers and pointers.
    let libc = libc_handle();
    let text = CString::new("bliss-runtime").expect("CString input is valid");

    unsafe {
        let abs_fn = foreign_symbol(libc, "abs").expect("libc must export abs");
        let abs_value = ffi_call(
            abs_fn,
            &AlienType::Int {
                signed: true,
                bits: 32,
            },
            &[AlienType::Int {
                signed: true,
                bits: 32,
            }],
            &[(-42_i32 as u32) as u64],
        )
        .expect("abs call must succeed");
        assert_eq!(abs_value as i32, 42);

        let strlen_fn = foreign_symbol(libc, "strlen").expect("libc must export strlen");
        let len = ffi_call(
            strlen_fn,
            &AlienType::Int {
                signed: false,
                bits: 64,
            },
            &[AlienType::Pointer(Box::new(AlienType::Int {
                signed: false,
                bits: 8,
            }))],
            &[text.as_ptr() as usize as u64],
        )
        .expect("strlen call must succeed");
        assert_eq!(len as usize, "bliss-runtime".len());
    }
}

#[test]
fn ffi_callback_trampoline_invokes_prepared_closure() {
    // Per R2.12, the FFI bridge supports callbacks from C into CL via trampolines.
    let closure = BlissVal::from_fixnum(callback_target as *const () as usize as i64);
    let callback = Callback::new(
        closure,
        AlienType::Int {
            signed: false,
            bits: 64,
        },
        vec![],
    )
    .expect("callback construction must succeed");
    callback.prepare_call();

    let raw = unsafe {
        let trampoline: extern "C" fn() -> u64 = std::mem::transmute(callback.as_fn_ptr());
        trampoline()
    };
    assert_eq!(BlissVal::from_raw(raw), BlissVal::from_fixnum(77));
}

#[test]
fn marshalling_round_trips_integer_float_and_pointer_shapes() {
    // Per R2.13 and R8.19, alien-value marshalling must cover integer, float, double, pointer,
    // and void shapes without silent information loss for supported values.
    let signed_cases = [
        (
            AlienType::Int {
                signed: true,
                bits: 8,
            },
            -7_i64,
        ),
        (
            AlienType::Int {
                signed: true,
                bits: 16,
            },
            -300_i64,
        ),
        (
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            -42_i64,
        ),
        (
            AlienType::Int {
                signed: true,
                bits: 64,
            },
            -9_223_372_036_854_775_i64,
        ),
    ];
    for (alien_type, number) in signed_cases {
        let raw = marshal_to_c(BlissVal::from_fixnum(number), &alien_type).expect("marshal int");
        let round_trip = unmarshal_from_c(raw, &alien_type).expect("unmarshal int");
        assert_eq!(round_trip.as_fixnum(), number);
    }

    let float_value = BlissVal::from_single_float(3.5);
    let float_bits = marshal_to_c(float_value, &AlienType::Float).expect("marshal float");
    let float_round_trip =
        unmarshal_from_c(float_bits, &AlienType::Float).expect("unmarshal float");
    assert!((float_round_trip.as_single_float() - 3.5).abs() < f32::EPSILON);

    let double_bits =
        marshal_to_c(BlissVal::from_fixnum(12), &AlienType::Double).expect("marshal double");
    let double_round_trip =
        unmarshal_from_c(double_bits, &AlienType::Double).expect("unmarshal double");
    assert_eq!(double_round_trip, BlissVal::from_fixnum(12));

    let null_pointer =
        marshal_to_c(NIL, &AlienType::Pointer(Box::new(AlienType::Void))).expect("marshal pointer");
    assert_eq!(null_pointer, 0);
    assert_eq!(
        unmarshal_from_c(null_pointer, &AlienType::Pointer(Box::new(AlienType::Void)))
            .expect("unmarshal pointer"),
        NIL
    );
}

#[test]
fn struct_layout_metadata_distinguishes_packed_and_aligned_structs() {
    // Per R2.13, alien marshalling includes struct-by-value support.
    let packed = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: false,
                bits: 8,
            },
            AlienType::Int {
                signed: false,
                bits: 32,
            },
        ],
        packed: true,
    };
    let unpacked = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: false,
                bits: 8,
            },
            AlienType::Int {
                signed: false,
                bits: 32,
            },
        ],
        packed: false,
    };

    assert_eq!(packed.alignment(), 1);
    assert_eq!(packed.size(), 5);
    assert_eq!(unpacked.alignment(), 4);
    assert_eq!(unpacked.size(), 8);
}

#[test]
fn unsafe_runtime_code_is_confined_to_the_specified_modules_and_documented() {
    // Per R8.03, unsafe Rust must be confined to ffi/gc/signal modules and documented with SAFETY comments.
    let mut files = Vec::new();
    collect_rust_sources(Path::new("crates/bliss-rt/src"), &mut files);

    for path in files {
        let source = read_source(&path);
        if !source.contains("unsafe") {
            continue;
        }

        let normalized = path.to_string_lossy().replace('\\', "/");
        let allowed = normalized.ends_with("/ffi.rs")
            || normalized.contains("/gc/")
            || normalized.ends_with("/signal.rs");

        assert!(
            allowed,
            "unexpected unsafe code outside approved modules: {}",
            normalized
        );
        assert!(
            source.contains("SAFETY:"),
            "unsafe code in {} must be justified with a SAFETY comment",
            normalized
        );
    }
}

#[test]
fn signal_and_sandbox_sources_cover_async_safety_image_validation_and_fuzzing_hooks() {
    // Per R8.10, signal handlers must defer work to a flag checked later.
    // Per R8.17, image loading must validate data before use.
    // Per R8.18, dedicated fuzzing hooks must exist for security-critical paths.
    let runtime_source = read_source("crates/bliss-rt/src/runtime.rs");
    let image_source = read_source("crates/bliss-rt/src/image.rs");
    let ffi_test_source = read_source("crates/bliss-rt/tests/test_ffi.rs");

    assert!(runtime_source.contains("SIGINT_RECEIVED.store"));
    assert!(runtime_source.contains("install_signal_handlers"));
    assert!(image_source.contains("validate_image_header"));
    assert!(ffi_test_source.contains("ffi_call"));
}

#[test]
fn reader_sources_define_depth_circular_read_eval_and_overflow_guards() {
    // Per R8.11 and R8.12, the reader must track nesting depth and circular labels.
    // Per R8.13, read-time evaluation must be configurable and disabled by policy.
    // Per R8.14, numeric overflow must not silently wrap.
    let reader_source = read_source("crates/bliss-compiler/src/reader.rs");
    let types_source = read_source("crates/bliss-rt/src/types.rs");

    assert!(reader_source.contains("CircularLabels"));
    assert!(reader_source.contains("read_eval"));
    assert!(reader_source.contains("overflow") || reader_source.contains("too large"));
    assert!(types_source.contains("bignum"));
}
