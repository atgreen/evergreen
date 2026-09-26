//! Generated inbound scalar ABI adapters, independently called by C fixtures.
//! R2.12/R2.14: this tests the machine-code boundary, not Lisp callback dispatch.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::cell::RefCell;
use torcl_rt::ffi::{AlienType, callback::CallbackAdapter};

struct Context {
    count: usize,
    seen: RefCell<Vec<u64>>,
    result: u64,
}

unsafe extern "C" fn dispatch(context: *mut (), arguments: *const u64) -> u64 {
    let context = unsafe { &*context.cast::<Context>() };
    let arguments = unsafe { std::slice::from_raw_parts(arguments, context.count) };
    *context.seen.borrow_mut() = arguments.to_vec();
    context.result
}

fn integer(bits: u8, signed: bool) -> AlienType {
    AlienType::Int { bits, signed }
}

fn symbol(name: &str) -> *const () {
    use std::{process::Command, sync::OnceLock};
    static LIBRARY: OnceLock<usize> = OnceLock::new();
    let library = *LIBRARY.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("torcl-ffi-callback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("callbacks.so");
        let result = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ffi_callbacks.c"
            ))
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let library = torcl_rt::ffi::load_foreign_library(output.to_str().unwrap()).unwrap();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(dir).unwrap();
        library as usize
    });
    unsafe { torcl_rt::ffi::foreign_symbol(library as *mut (), name).unwrap() }
}

#[test]
fn inbound_register_banks_and_stack_spills_preserve_raw_scalar_bits() {
    let mut types = vec![
        integer(8, true),
        integer(16, false),
        integer(64, false),
        AlienType::Float,
        AlienType::Double,
        AlienType::Pointer(Box::new(AlienType::Void)),
        integer(64, true),
        integer(32, true),
        integer(32, true),
    ];
    types.extend(std::iter::repeat_n(AlienType::Double, 8));
    let mut context = Context {
        count: types.len(),
        seen: RefCell::new(vec![]),
        result: 7.25f64.to_bits(),
    };
    let adapter = CallbackAdapter::new(
        &AlienType::Double,
        &types,
        &mut context as *mut _ as *mut (),
        dispatch,
    )
    .unwrap();
    let invoke: unsafe extern "C" fn(*const ()) -> f64 =
        unsafe { std::mem::transmute(symbol("torcl_callback_mixed")) };
    assert_eq!(unsafe { invoke(adapter.as_fn_ptr()) }, 7.25);
    let mut expected = vec![
        0xf9,
        65530,
        u64::MAX,
        1.25f32.to_bits() as u64,
        (-0.0f64).to_bits(),
        0x12340,
        (-9i64) as u64,
        17,
        23,
    ];
    expected.extend((2..=9).map(|n| (n as f64).to_bits()));
    assert_eq!(*context.seen.borrow(), expected);
}

#[test]
fn callbacks_have_independent_entries_without_thread_local_preparation() {
    let mut first = Context {
        count: 0,
        seen: RefCell::new(vec![]),
        result: 11,
    };
    let mut second = Context {
        count: 0,
        seen: RefCell::new(vec![]),
        result: 29,
    };
    let ty = integer(64, false);
    let a = CallbackAdapter::new(&ty, &[], &mut first as *mut _ as *mut (), dispatch).unwrap();
    let b = CallbackAdapter::new(&ty, &[], &mut second as *mut _ as *mut (), dispatch).unwrap();
    assert_ne!(a.as_fn_ptr(), b.as_fn_ptr());
    let invoke: unsafe extern "C" fn(*const (), *const ()) -> u64 =
        unsafe { std::mem::transmute(symbol("torcl_callback_alternate")) };
    assert_eq!(unsafe { invoke(a.as_fn_ptr(), b.as_fn_ptr()) }, 112911);
}

#[test]
fn float_narrow_integer_pointer_and_void_results_follow_c_abi() {
    for (name, result_ty, argument_ty, input, output) in [
        (
            "torcl_callback_float",
            AlienType::Float,
            AlienType::Float,
            3.5f32.to_bits() as u64,
            (-0.0f32).to_bits() as u64,
        ),
        (
            "torcl_callback_signed",
            integer(8, true),
            integer(8, true),
            0xf9,
            0xf3,
        ),
        (
            "torcl_callback_pointer",
            AlienType::Pointer(Box::new(AlienType::Void)),
            AlienType::Pointer(Box::new(AlienType::Void)),
            0x12340,
            0x45670,
        ),
        (
            "torcl_callback_void",
            AlienType::Void,
            integer(32, false),
            37,
            0,
        ),
    ] {
        let mut context = Context {
            count: 1,
            seen: RefCell::new(vec![]),
            result: output,
        };
        let adapter = CallbackAdapter::new(
            &result_ty,
            &[argument_ty],
            &mut context as *mut _ as *mut (),
            dispatch,
        )
        .unwrap();
        let invoke: unsafe extern "C" fn(*const ()) -> u64 =
            unsafe { std::mem::transmute(symbol(name)) };
        let expected = match name {
            "torcl_callback_signed" => (-13i64) as u64,
            "torcl_callback_void" => 42,
            _ => output,
        };
        assert_eq!(unsafe { invoke(adapter.as_fn_ptr()) }, expected, "{name}");
        assert_eq!(*context.seen.borrow(), vec![input], "{name}");
    }
}

#[test]
fn unsupported_callback_signatures_are_rejected_before_publication() {
    assert!(
        CallbackAdapter::new(
            &AlienType::Void,
            &[AlienType::Void],
            std::ptr::null_mut(),
            dispatch
        )
        .is_err()
    );
    assert!(
        CallbackAdapter::new(&integer(24, false), &[], std::ptr::null_mut(), dispatch).is_err()
    );
    assert!(
        CallbackAdapter::new(
            &AlienType::Struct {
                fields: vec![integer(32, true)],
                packed: false
            },
            &[],
            std::ptr::null_mut(),
            dispatch
        )
        .is_err()
    );
}

#[test]
fn nested_generated_entries_keep_their_own_context() {
    struct Nested {
        inner: *const (),
        bias: u64,
    }
    unsafe extern "C" fn nested(context: *mut (), _: *const u64) -> u64 {
        let context = unsafe { &*context.cast::<Nested>() };
        let inner: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(context.inner) };
        unsafe { inner() + context.bias }
    }
    let mut inner_context = Context {
        count: 0,
        seen: RefCell::new(vec![]),
        result: 29,
    };
    let ty = integer(64, false);
    let inner =
        CallbackAdapter::new(&ty, &[], &mut inner_context as *mut _ as *mut (), dispatch).unwrap();
    let mut outer_context = Nested {
        inner: inner.as_fn_ptr(),
        bias: 11,
    };
    let outer =
        CallbackAdapter::new(&ty, &[], &mut outer_context as *mut _ as *mut (), nested).unwrap();
    let invoke: unsafe extern "C" fn(*const (), *const ()) -> u64 =
        unsafe { std::mem::transmute(symbol("torcl_callback_alternate")) };
    assert_eq!(
        unsafe { invoke(outer.as_fn_ptr(), inner.as_fn_ptr()) },
        402940
    );
}

#[test]
fn generated_entry_is_callable_on_a_fresh_thread_without_preparation() {
    unsafe extern "C" fn constant(context: *mut (), _: *const u64) -> u64 {
        unsafe { *context.cast::<u64>() }
    }
    let context = std::sync::Arc::new(u64::MAX);
    let adapter = CallbackAdapter::new(
        &integer(64, false),
        &[],
        std::sync::Arc::as_ptr(&context) as *mut (),
        constant,
    )
    .unwrap();
    let answer = std::thread::spawn(move || {
        let entry: unsafe extern "C" fn() -> u64 =
            unsafe { std::mem::transmute(adapter.as_fn_ptr()) };
        let answer = unsafe { entry() };
        drop(adapter);
        drop(context);
        answer
    })
    .join()
    .unwrap();
    assert_eq!(answer, u64::MAX);
}
