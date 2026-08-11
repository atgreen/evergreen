use bliss_rt::error::BlissError;
use bliss_rt::ffi::{
    AlienType, Callback, ffi_call, foreign_symbol, load_foreign_library, marshal_to_c,
    unmarshal_from_c,
};
use bliss_rt::sandbox::{Sandbox, SandboxPolicy};
use bliss_rt::value::{BlissVal, NIL};
use std::ffi::CString;

fn libc_handle() -> *mut () {
    load_foreign_library("libc.so.6")
        .or_else(|_| load_foreign_library("libSystem.B.dylib"))
        .or_else(|_| load_foreign_library("libc.so"))
        .expect("expected a libc-compatible shared library on supported platforms")
}

extern "C" fn callback_target() -> u64 {
    BlissVal::from_fixnum(77).to_raw()
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
    assert!(matches!(err, BlissError::FfiError(message) if message.contains("null function pointer")));
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
    let callback = Callback::new(closure, AlienType::Int { signed: false, bits: 64 }, vec![])
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
        (AlienType::Int { signed: true, bits: 8 }, -7_i64),
        (AlienType::Int { signed: true, bits: 16 }, -300_i64),
        (AlienType::Int { signed: true, bits: 32 }, -42_i64),
        (AlienType::Int { signed: true, bits: 64 }, -9_223_372_036_854_775_i64),
    ];
    for (alien_type, number) in signed_cases {
        let raw = marshal_to_c(BlissVal::from_fixnum(number), &alien_type).expect("marshal int");
        let round_trip = unmarshal_from_c(raw, &alien_type).expect("unmarshal int");
        assert_eq!(round_trip.as_fixnum(), number);
    }

    let float_value = BlissVal::from_single_float(3.5);
    let float_bits = marshal_to_c(float_value, &AlienType::Float).expect("marshal float");
    let float_round_trip = unmarshal_from_c(float_bits, &AlienType::Float).expect("unmarshal float");
    assert!((float_round_trip.as_single_float() - 3.5).abs() < f32::EPSILON);

    let double_bits = marshal_to_c(BlissVal::from_fixnum(12), &AlienType::Double)
        .expect("marshal double");
    let double_round_trip =
        unmarshal_from_c(double_bits, &AlienType::Double).expect("unmarshal double");
    assert_eq!(double_round_trip, BlissVal::from_fixnum(12));

    let null_pointer = marshal_to_c(NIL, &AlienType::Pointer(Box::new(AlienType::Void)))
        .expect("marshal pointer");
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
