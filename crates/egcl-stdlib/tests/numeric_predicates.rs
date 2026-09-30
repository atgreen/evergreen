use egcl_rt::bignum::alloc_ratio_cli;
use egcl_rt::error::EgclError;
use egcl_rt::value::{NIL, EgclVal};
use egcl_stdlib::numbers::{ash, minusp, plusp, zerop};

/// Serializes every test in this target that touches the heap (bliss-ifho).
/// `zero_sign_kernels_do_not_allocate_lisp_values` measures its own allocation
/// with `heap_stats().bytes_allocated`, which counts the whole PROCESS, so a
/// sibling test allocating a bignum on another thread lands inside its bracket
/// and it fails with somebody else's bytes. Cargo runs the tests in a target in
/// parallel threads, so without this the no-allocation assertion is a lottery
/// the whole workspace run pays for.
///
/// Poisoning is recovered rather than propagated so one panic does not cascade.
fn heap_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

macro_rules! serialize_heap_use {
    () => {
        let _global_guard = heap_lock().lock().unwrap_or_else(|e| e.into_inner());
    };
}

fn check(value: EgclVal, expected: (bool, bool, bool)) {
    assert_eq!(
        (
            zerop(value).unwrap(),
            plusp(value).unwrap(),
            minusp(value).unwrap()
        ),
        expected
    );
}

#[test]
fn zero_sign_kernels_do_not_allocate_lisp_values() {
    serialize_heap_use!();
    let before = egcl_rt::heap_stats().bytes_allocated;
    for n in -1000..1000 {
        check(EgclVal::from_fixnum(n), (n == 0, n > 0, n < 0));
    }
    for n in [
        0.0,
        -0.0,
        1.0,
        -1.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ] {
        check(EgclVal::from_single_float(n), (n == 0.0, n > 0.0, n < 0.0));
    }
    assert_eq!(egcl_rt::heap_stats().bytes_allocated, before);
}

#[test]
fn exact_ratio_signs_do_not_round_to_float_zero() {
    serialize_heap_use!();
    egcl_rt::rooted!(den = ash(EgclVal::from_fixnum(1), EgclVal::from_fixnum(2000)).unwrap());
    check(*den, (false, true, false));
    for num in [-1, 0, 1] {
        let ratio = alloc_ratio_cli(EgclVal::from_fixnum(num), *den);
        check(ratio, (num == 0, num > 0, num < 0));
    }
    let negative_denominator = alloc_ratio_cli(EgclVal::from_fixnum(1), EgclVal::from_fixnum(-2));
    check(negative_denominator, (false, false, true));
}

#[test]
fn numeric_predicate_type_errors_name_their_domains() {
    assert!(
        matches!(zerop(NIL), Err(EgclError::TypeError { expected, .. }) if expected == "number")
    );
    assert!(
        matches!(plusp(NIL), Err(EgclError::TypeError { expected, .. }) if expected == "real")
    );
    assert!(
        matches!(minusp(NIL), Err(EgclError::TypeError { expected, .. }) if expected == "real")
    );
}
